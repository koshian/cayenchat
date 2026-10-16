// SPDX-License-Identifier: MPL-2.0
//! IRC transport adapter. Third-party IRC types stay inside this crate.

mod accounts;
mod cap;
mod chanmodes;
mod charsets;
mod ctcp;
mod echo;
mod history;
mod metadata;
mod modes;
mod peer_avatar;
mod presence;
mod relay;
mod replay;
mod tags;
pub mod text;

pub use cap::Ircv3Options;
pub use charsets::ChannelEncoding;
pub use history::{
    HISTORY_LIMIT, HistoryMessage, HistoryResume, MAX_HISTORY_LINES, MessageReference,
    OlderHistoryStatus,
};
pub use metadata::{AvatarRequestFailure, MAX_PUBLISHED_AVATAR_BYTES, publishable_avatar};
pub use modes::MemberMode;
pub use peer_avatar::shareable as shareable_avatar;

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fmt,
    ops::ControlFlow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc as std_mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use cayenchat_model::names::CaseMapping;
use charsets::ChannelCharsets;
use futures_util::StreamExt;
use irc::{
    client::{
        ClientStream,
        data::user::AccessLevel,
        prelude::{Client, Config},
    },
    proto::{
        Command as IrcCommand, Message as IrcMessage, Prefix, Response,
        charsets::{BodyCharsets, Fixed, encode_line},
        mode::{ChannelMode, Mode},
    },
};
use tokio::sync::{Notify, mpsc};

const COMMAND_CAPACITY: usize = 128;
const EVENT_CAPACITY: usize = 512;
/// How far back direct-message discovery looks without a known disconnect,
/// and at most.
const TARGETS_DEFAULT_WINDOW: Duration = Duration::from_secs(24 * 3600);
const TARGETS_MAX_WINDOW: Duration = Duration::from_secs(7 * 24 * 3600);
const USER_DISCONNECT: &str = "Disconnected by user.";
/// How long the application waits for workers to finish after
/// [`Connection::shutdown`], shared by all connections.
pub const QUIT_WAIT: Duration = Duration::from_secs(2);
/// The longest the worker writes QUIT and waits for the server to close
/// after it. Kept below [`QUIT_WAIT`] so the worker ends on its own before
/// the wait does.
const QUIT_FLUSH_LIMIT: Duration = Duration::from_millis(1500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Servers may hold registration until their ident (RFC 1413) and DNS lookups
/// finish. IRCnet waits about 30 seconds when the client's port 113 silently
/// drops packets, so the limit must comfortably exceed that.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(90);
/// How long the `irc` crate waits for the PONG to its periodic PING (every
/// 180 s). Its default is 20 s, which a full overload run exceeded (issue
/// #272): the measuring server stalled about 30 s, and the worker stops
/// reading while the event queue is full, so the PONG may also wait in the
/// socket buffer; the crate checks the deadline before it looks at buffered
/// data. This is a margin, not a bound; a dead connection still ends after it.
const PING_TIMEOUT_SECS: u32 = 120;
/// How long [`Connection::check_alive`] waits for any line from the server.
/// Longer than a round trip over a network that has just come back, and
/// short enough that a lost connection is replaced soon after waking.
const ALIVE_CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// Commands held back during an alive check; further ones are refused so a
/// check cannot grow the backlog.
const MAX_HELD_COMMANDS: usize = COMMAND_CAPACITY;
const ALIVE_CHECK_FAILED: &str = "No response from the server after waking from sleep.";
/// Limits on WHOIS replies that have not reached end-of-WHOIS (318), so a
/// hostile server cannot grow memory by never finishing them.
const MAX_PENDING_WHOIS: usize = 32;
const MAX_WHOIS_ITEMS: usize = 512;
/// The realname sent in `USER` when none is configured; with a shared peer
/// avatar it starts with KVIrc's avatar mark.
const REALNAME: &str = "CayenChat";
/// The `QUIT` reason sent when none is configured.
const QUIT_MESSAGE: &str = "Leaving CayenChat";
/// `SETNAME` requests waiting for an answer; a server answers each in order.
const MAX_PENDING_SETNAME: usize = 4;

fn ensure_tls_crypto_provider() -> Result<(), String> {
    use rustls::crypto::CryptoProvider;

    if CryptoProvider::get_default().is_none() {
        // GPUI's HTTP client enables ring while `irc` enables aws-lc-rs.
        // rustls cannot choose a default when both features are present.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    if CryptoProvider::get_default().is_none() {
        return Err("Could not initialize the TLS cryptography provider.".into());
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub struct SaslCredentials {
    pub username: String,
    pub password: String,
}

// Credentials never appear in debug output.
impl fmt::Debug for SaslCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SaslCredentials")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub nickname: String,
    /// The `USER` command's username (ident), independent of the nickname.
    pub username: String,
    /// The configured real name (GECOS); empty means the built-in default.
    /// The avatar mark is added on the wire only, never stored here.
    pub realname: String,
    /// The `QUIT` reason sent on a normal disconnect; empty means the
    /// built-in default.
    pub quit_message: String,
    /// The `PART` reason for a `PART` without an explicit one; empty means
    /// no reason. Independent of `quit_message`.
    pub part_message: String,
    pub channels: Vec<String>,
    /// Keys (`+k`) sent with the JOIN of the auto-join `channels` they are
    /// listed for. Secret: never shown in `Debug` output or the transcript.
    pub channel_keys: BTreeMap<String, String>,
    pub use_tls: bool,
    pub verify_tls_certificates: bool,
    pub encoding: String,
    pub server_password: Option<String>,
    /// Lets `PASS` go out over a plaintext connection; the user opted in.
    pub allow_plaintext_pass: bool,
    pub sasl: Option<SaslCredentials>,
    /// Opt-in IRCv3 extensions; all off unless the user enabled them.
    pub ircv3: Ircv3Options,
    /// The avatar URL shared with other clients through CTCP AVATAR, used
    /// only with [`Ircv3Options::peer_avatars`]. When set, the realname
    /// sent at registration carries KVIrc's avatar mark; the URL itself can
    /// change later with [`Connection::share_avatar`].
    pub shared_avatar: Option<String>,
    /// Channels whose log was cut off when the previous connection ended,
    /// with the newest message received before it. With chathistory
    /// negotiated, the first JOIN of each asks only for what came after
    /// (reconnect gap recovery); otherwise they are unused.
    pub resume_history: Vec<HistoryResume>,
    /// With chathistory negotiated, direct-message conversations with
    /// messages since this time are asked for once after registration
    /// (`CHATHISTORY TARGETS`): normally when the previous connection ended.
    /// `None` looks a day back; nothing goes further back than a week.
    pub history_targets_since: Option<SystemTime>,
    /// Channels whose text uses another encoding than `encoding`; the
    /// channel name and everything else on a line keep `encoding`.
    pub channel_encodings: Vec<ChannelEncoding>,
}

impl fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("nickname", &self.nickname)
            .field("username", &self.username)
            .field("realname", &self.realname)
            .field("quit_message", &self.quit_message)
            .field("part_message", &self.part_message)
            .field("channels", &self.channels)
            .field("channel_keys", &self.channel_keys.len())
            .field("use_tls", &self.use_tls)
            .field("verify_tls_certificates", &self.verify_tls_certificates)
            .field("encoding", &self.encoding)
            .field(
                "server_password",
                &self.server_password.as_ref().map(|_| "[redacted]"),
            )
            .field("allow_plaintext_pass", &self.allow_plaintext_pass)
            .field("sasl", &self.sasl)
            .field("ircv3", &self.ircv3)
            .field("shared_avatar", &self.shared_avatar)
            .field("resume_history", &self.resume_history.len())
            .field("history_targets_since", &self.history_targets_since)
            .field("channel_encodings", &self.channel_encodings)
            .finish()
    }
}

impl ConnectionConfig {
    /// A TLS configuration whose `USER` username starts equal to `nickname`;
    /// callers set [`ConnectionConfig::username`] from their own settings.
    pub fn tls(host: String, nickname: String, channels: Vec<String>) -> Self {
        Self {
            host,
            port: 6697,
            username: nickname.clone(),
            realname: String::new(),
            quit_message: String::new(),
            part_message: String::new(),
            nickname,
            channels,
            channel_keys: BTreeMap::new(),
            use_tls: true,
            verify_tls_certificates: true,
            encoding: "UTF-8".into(),
            server_password: None,
            allow_plaintext_pass: false,
            sasl: None,
            ircv3: Ircv3Options::default(),
            shared_avatar: None,
            resume_history: Vec::new(),
            history_targets_since: None,
            channel_encodings: Vec::new(),
        }
    }

    /// Whether registration marks the realname as having an avatar: only
    /// with peer exchange on, a URL explicitly shared and an encoding that
    /// can carry KVIrc's mark (not ISO-2022-JP).
    pub fn advertises_avatar(&self) -> bool {
        self.ircv3.peer_avatars
            && self.shared_avatar.is_some()
            && peer_avatar::carries_mark(&self.encoding)
    }

    /// The realname as sent in `USER` and `SETNAME`: the configured value
    /// (or the default), with KVIrc's avatar mark when an avatar is shared.
    pub(crate) fn wire_realname(&self) -> String {
        peer_avatar::realname(base_realname(&self.realname), self.advertises_avatar())
    }

    /// The `QUIT` reason: the configured message, or the default when blank.
    pub fn wire_quit_message(&self) -> String {
        let configured = self.quit_message.trim();
        if configured.is_empty() {
            QUIT_MESSAGE.to_owned()
        } else {
            configured.to_owned()
        }
    }

    /// The default `PART` reason: the trimmed configured message, if any.
    fn wire_part_message(&self) -> Option<String> {
        let configured = self.part_message.trim();
        (!configured.is_empty()).then(|| configured.to_owned())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.host.is_empty()
            || self.host.chars().any(char::is_whitespace)
            || self.host.chars().any(char::is_control)
        {
            return Err("Server hostname must not be empty or contain whitespace.".into());
        }
        if self.port == 0 {
            return Err("Server port must be nonzero.".into());
        }
        if self.nickname.is_empty()
            || self
                .nickname
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control() || ch == ',')
        {
            return Err("Nickname must not be empty or contain spaces or controls.".into());
        }
        if self.username.is_empty()
            || self
                .username
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '@')
        {
            return Err("Username must not be empty or contain spaces, controls or @.".into());
        }
        check_realname(&self.realname)?;
        if self
            .quit_message
            .chars()
            .any(|ch| matches!(ch, '\r' | '\n' | '\0'))
        {
            return Err("Quit message must not contain line breaks.".into());
        }
        validate_wire(
            &format!("QUIT :{}\r\n", self.wire_quit_message()),
            &self.encoding,
        )?;
        if self
            .part_message
            .chars()
            .any(|ch| matches!(ch, '\r' | '\n' | '\0'))
        {
            return Err("Part message must not contain line breaks.".into());
        }
        if let Some(reason) = self.wire_part_message() {
            // The channel is not known yet; the final length is checked when a
            // PART is sent.
            validate_wire(&format!("PART #x :{reason}\r\n"), &self.encoding)?;
        }
        for channel in &self.channels {
            if !valid_channel(channel) {
                return Err(format!("Invalid channel name: {channel}"));
            }
            validate_wire(&format!("JOIN {channel}\r\n"), &self.encoding)?;
        }
        for (channel, key) in &self.channel_keys {
            if !valid_channel(channel) || !valid_channel_key(key) {
                return Err("Channel key must not be empty or contain spaces or controls.".into());
            }
            validate_wire(&format!("JOIN {channel} {key}\r\n"), &self.encoding)
                .map_err(|_| "Channel key is too long or cannot be encoded.".to_owned())?;
        }
        validate_wire(&format!("NICK {}\r\n", self.nickname), &self.encoding)?;
        validate_wire(
            &format!("USER {} 0 * :{}\r\n", self.username, self.wire_realname()),
            &self.encoding,
        )?;
        if let Some(url) = &self.shared_avatar {
            peer_avatar::shareable(url, self.encoding.eq_ignore_ascii_case("UTF-8"))?;
        }
        ChannelCharsets::new(&self.channel_encodings)?;
        if let Some(password) = &self.server_password {
            if password.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
                return Err("Server password contains a protocol control character.".into());
            }
            if !password.is_empty() && !self.use_tls && !self.allow_plaintext_pass {
                return Err("Enable TLS, or allow sending the server password without TLS.".into());
            }
        }
        if let Some(sasl) = &self.sasl {
            if !self.use_tls {
                return Err("SASL PLAIN requires TLS.".into());
            }
            if sasl.username.is_empty()
                || sasl.password.is_empty()
                || sasl
                    .username
                    .chars()
                    .any(|ch| matches!(ch, '\r' | '\n' | '\0'))
                || sasl
                    .password
                    .chars()
                    .any(|ch| matches!(ch, '\r' | '\n' | '\0'))
            {
                return Err(
                    "SASL username and password must be nonempty and contain no protocol controls."
                        .into(),
                );
            }
        }
        Ok(())
    }
}

/// The configured realname, or the default when it is blank.
fn base_realname(configured: &str) -> &str {
    let configured = configured.trim();
    if configured.is_empty() {
        REALNAME
    } else {
        configured
    }
}

/// A realname must fit one line; the encoding and length are checked with
/// the whole command.
fn check_realname(value: &str) -> Result<(), String> {
    if value.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
        return Err("Real name must not contain line breaks.".into());
    }
    Ok(())
}

/// How lines are written: the connection's encoding (a WHATWG label, as the
/// line codec takes it) and the encodings some channels use for their text.
/// A plain label stands for a connection without channel overrides.
#[derive(Clone, Copy)]
struct Wire<'a> {
    label: &'a str,
    charsets: Option<&'a dyn BodyCharsets>,
}

impl<'a> From<&'a str> for Wire<'a> {
    fn from(label: &'a str) -> Self {
        Self {
            label,
            charsets: None,
        }
    }
}

impl<'a> From<&'a String> for Wire<'a> {
    fn from(label: &'a String) -> Self {
        label.as_str().into()
    }
}

/// `text` as it goes on the wire, or why it cannot be sent: an unknown label,
/// or a character the encoding cannot hold.
fn encode_strict<'a, 'w>(
    text: &'a str,
    wire: impl Into<Wire<'w>>,
) -> Result<std::borrow::Cow<'a, [u8]>, String> {
    let wire = wire.into();
    let encoding = charsets::resolve(wire.label)?;
    let (bytes, unmappable) = encode_line(text, encoding, wire.charsets);
    if let Some(failed) = unmappable {
        let name = if failed == encoding {
            wire.label
        } else {
            failed.name()
        };
        return Err(format!(
            "Text contains a character that cannot be encoded as {name}."
        ));
    }
    Ok(bytes)
}

fn validate_wire<'w>(wire: &str, encoding: impl Into<Wire<'w>>) -> Result<(), String> {
    let bytes = encode_strict(wire, encoding)?;
    if bytes.len() > 512 {
        return Err("IRC command exceeds the 512-byte wire limit.".into());
    }
    Ok(())
}

/// [`validate_wire`] for a `PRIVMSG` or `NOTICE`: the line must also fit in
/// 512 bytes once the server has put our `prefix_bytes` (`:nick!user@host `)
/// in front of it, or the recipients get the end of it cut off.
fn validate_relayed_wire<'w>(
    wire: &str,
    encoding: impl Into<Wire<'w>>,
    prefix_bytes: usize,
) -> Result<(), String> {
    let encoding = encoding.into();
    validate_wire(wire, encoding)?;
    let bytes = encode_strict(wire, encoding)?;
    // A text of one word goes out without the `:` that marks the last
    // parameter, but the server writes it when relaying.
    let relayed_colon = wire
        .trim_end_matches(['\r', '\n'])
        .splitn(3, ' ')
        .nth(2)
        .is_some_and(|text| !text.starts_with(':'));
    let needed = bytes.len() + usize::from(relayed_colon);
    if needed + prefix_bytes > 512 {
        return Err(format!(
            "Message is too long: {} bytes of the 512-byte line remain for the command and text once the server adds your nick!user@host, but it needs {needed}.",
            512usize.saturating_sub(prefix_bytes),
        ));
    }
    Ok(())
}

/// The tag section of a message a client sends may take 4094 bytes (IRCv3
/// message-tags) without the leading `@` and the space after it, so 4096 as
/// `tags::tag_bytes` counts them. It is separate from the 512 bytes of the
/// message itself.
const MAX_CLIENT_TAG_BYTES: usize = 4096;

/// Checks a message that carries tags: the message without them against the
/// usual 512-byte limit, its tags against their own limit, and the whole
/// line for characters the encoding cannot hold. Tag values are UTF-8 (the
/// worker requests tags only on UTF-8 connections).
fn validate_tagged_wire<'w>(
    message: &IrcMessage,
    encoding: impl Into<Wire<'w>>,
) -> Result<(), String> {
    let encoding = encoding.into();
    let mut untagged = message.clone();
    untagged.tags = None;
    validate_wire(&untagged.to_string(), encoding)?;
    if let Some(tags) = &message.tags {
        if tags::tag_bytes(tags) > MAX_CLIENT_TAG_BYTES {
            return Err("Message tags exceed the 4094-byte limit.".into());
        }
        // The line as a whole, for encodability only: tags are not part of
        // the 512 bytes.
        encode_strict(&message.to_string(), encoding)?;
    }
    Ok(())
}

/// The channel-name comparison a RPL_ISUPPORT line sets: `CASEMAPPING=<name>`,
/// or the default again when `-CASEMAPPING` withdraws it.
fn advertised_casemapping(message: &IrcMessage) -> Option<CaseMapping> {
    let IrcCommand::Response(Response::RPL_ISUPPORT, args) = &message.command else {
        return None;
    };
    // The first argument is our nickname, the last the trailing text.
    args.iter()
        .skip(1)
        .flat_map(|arg| arg.split_whitespace())
        .filter_map(|token| {
            if token.eq_ignore_ascii_case("-CASEMAPPING") {
                Some(CaseMapping::default())
            } else {
                let (name, value) = token.split_once('=')?;
                name.eq_ignore_ascii_case("CASEMAPPING")
                    .then(|| CaseMapping::parse(value))
            }
        })
        .next_back()
}

fn requires_utf8(message: &IrcMessage) -> bool {
    matches!(&message.command, IrcCommand::Response(Response::RPL_ISUPPORT, args)
        if args.iter().any(|arg| arg.split_whitespace().any(|token| token == "UTF8ONLY")))
}

/// Whether a name is a supported channel target, including IRCnet safe channels.
/// Keep the server-assigned identifier in `!` names intact.
pub fn valid_channel(value: &str) -> bool {
    value.starts_with(['#', '&', '!'])
        && value.len() > 1
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == ',')
}

/// Whether `value` can be sent as a channel key: one IRC parameter, so no
/// spaces, controls or commas (a comma separates the keys of several
/// channels), and not starting with `:`.
pub fn valid_channel_key(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(':')
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == ',')
}

/// Whether a name can be a nickname target (no channel prefix, spaces or
/// separators).
pub fn valid_nickname(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(['#', '&', '!', '~', '@', '%', '+'])
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, ',' | ':'))
}

fn validate_message_text(text: &str) -> Result<(), String> {
    if text.is_empty() || text.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
        Err("Message must not be empty or contain line breaks.".into())
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireDirection {
    Sent,
    Received,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Diagnostic {
        elapsed: Duration,
        message: String,
    },
    Wire {
        elapsed: Duration,
        direction: WireDirection,
        line: String,
    },
    TransportConnected,
    Registered {
        nickname: String,
    },
    /// The server advertised how it compares channel names. Until then (and
    /// on each new connection) RFC 1459 applies.
    CaseMapping(CaseMapping),
    Joined {
        channel: String,
    },
    /// The mode letters set on a joined channel, in the order learned: from
    /// `RPL_CHANNELMODEIS` after our join, then kept current by `MODE`.
    /// Arguments (key, limit) and member or list modes are not included.
    ChannelModes {
        channel: String,
        flags: String,
    },
    Parted {
        channel: String,
    },
    NickChanged {
        nickname: String,
    },
    ChannelMessage {
        channel: String,
        sender: String,
        text: String,
        notice: bool,
        /// The server's `time` tag when server-time is negotiated and the
        /// tag is valid; `None` means use the receipt time.
        server_time: Option<SystemTime>,
        /// The server's `msgid` tag, when present and non-empty.
        msgid: Option<String>,
        /// The sender's services account when the message was sent
        /// (`account-tag`), when the server reports one.
        account: Option<String>,
        /// History or a server/bouncer line rather than a live message from
        /// a user; it must not notify again.
        replayed: bool,
    },
    ChannelActivity {
        channel: String,
        actor: String,
        kind: ChannelActivityKind,
        server_time: Option<SystemTime>,
    },
    /// The topic of a channel: on joining (`RPL_TOPIC`, `RPL_NOTOPIC`) or
    /// when someone changes it. Empty when there is none. The line itself
    /// also stays a [`Event::ServerLine`].
    Topic {
        channel: String,
        topic: String,
        /// Who changed it, for a `TOPIC` command; `None` for the topic a
        /// server states on joining.
        changed_by: Option<String>,
        server_time: Option<SystemTime>,
    },
    /// A PRIVMSG or NOTICE a user sent to our nickname. Server notices stay
    /// [`Event::ServerLine`]; CTCP other than ACTION is handled by `ctcp`.
    PrivateMessage {
        sender: String,
        text: String,
        notice: bool,
        server_time: Option<SystemTime>,
        /// The server's `msgid` tag, when present and non-empty.
        msgid: Option<String>,
        /// The sender's services account (`account-tag`), when reported.
        account: Option<String>,
        /// Replayed history (IRCv3 history batch).
        replayed: bool,
    },
    /// A PRIVMSG or NOTICE we sent to a user, seen on the wire: a bouncer
    /// relaying what another of our clients sent, or its playback. (Our own
    /// messages from this client arrive as [`Event::OutgoingAccepted`].)
    OwnPrivateMessage {
        target: String,
        text: String,
        notice: bool,
        server_time: Option<SystemTime>,
        msgid: Option<String>,
        replayed: bool,
    },
    /// Another user changed nickname (ours is [`Event::NickChanged`]).
    UserNickChanged {
        from: String,
        to: String,
    },
    /// Another user quit. Only users sharing a channel with us are seen.
    UserQuit {
        nickname: String,
        reason: Option<String>,
    },
    Names {
        channel: String,
        users: Vec<String>,
    },
    /// What is known about a user we share a channel with (opt-in "user
    /// accounts"): their services account (`None`: not logged in or not
    /// known) and real name, from `extended-join`, `account-notify` or a
    /// WHOX reply. Sent again when either changes, also under a new
    /// nickname; the old one is then [`Event::UserAccountForgotten`].
    UserAccount {
        nickname: String,
        account: Option<String>,
        realname: Option<String>,
    },
    /// The user no longer shares a channel with us: forget what was known.
    UserAccountForgotten {
        nickname: String,
    },
    ServerLine(String),
    /// A completed WHOIS reply, emitted at end-of-WHOIS (318).
    Whois(Box<WhoisInfo>),
    /// `local_id`: set when the connection tracks this message for its
    /// server echo (`echo-message`): [`Event::OutgoingConfirmed`] or
    /// [`Event::OutgoingFailed`] with the same id follows, or the connection
    /// ends first.
    OutgoingAccepted {
        local_id: Option<u64>,
        channel: String,
        /// What the conversation shows (`* waves` for an ACTION, a redaction
        /// for credentials sent to services).
        text: String,
        notice: bool,
        /// The action's own text when this is a CTCP ACTION (`/me`), for
        /// the conversation log; `text` is not to be parsed for it. `None`
        /// for a message and for credentials sent to services.
        action: Option<String>,
    },
    /// The server rejected the registration nickname (432/433). The link
    /// stays open until the UI supplies another one with
    /// [`Connection::change_nickname`].
    NicknameRejected {
        nickname: String,
    },
    /// A user's avatar from IRCv3 metadata (`draft/metadata-2`, opt-in),
    /// or `None` when it was removed or is no longer known (the user quit
    /// or no longer shares a channel with us). The URL is untrusted text;
    /// it may contain the registry's `{size}` placeholder.
    UserAvatar {
        nickname: String,
        url: Option<String>,
    },
    /// A user whose avatar may be known changed nickname; the avatar moves
    /// with them. Sent only while metadata is enabled.
    AvatarMoved {
        from: String,
        to: String,
    },
    /// Metadata stopped (the capability or `batch` was withdrawn): every
    /// avatar of this connection is unknown from now on, and our own avatar
    /// can no longer be published on it.
    AvatarsReset,
    /// Avatar metadata is usable on this connection: registered, with the
    /// capability negotiated and the subscription sent. Our own avatar can
    /// be published from now until `AvatarsReset` or the connection ends.
    MetadataReady,
    /// Our own avatar as the server reports it: the answer to our query
    /// after subscribing, the confirmation of our request `request`, or a
    /// change made elsewhere (another client of the same user, services).
    /// The value may differ from what we asked for.
    OwnAvatar {
        url: Option<String>,
        request: Option<u64>,
    },
    /// Our realname is now `realname` (without the avatar mark): the server
    /// confirmed our `SETNAME`, or another client of ours changed it.
    RealNameChanged {
        realname: String,
    },
    /// Our `SETNAME` did not succeed.
    RealNameFailed(RealNameFailure),
    /// Our request `request` to publish or remove our avatar did not
    /// succeed.
    OwnAvatarFailed {
        request: u64,
        failure: AvatarRequestFailure,
    },
    /// We asked the server for `channel`'s latest history (opt-in
    /// `draft/chathistory`). Its reply is older than every line of the
    /// channel that arrives after this event. Exactly one
    /// [`Event::ChannelHistory`] follows on this connection. `resumed`: only
    /// the lines after the channel's [`HistoryResume`] reference were asked
    /// for (reconnect gap recovery).
    HistoryRequested {
        channel: String,
        resumed: bool,
    },
    /// The complete reply to our history request for `channel`, in the
    /// server's order (oldest first). Empty when the server had nothing, the
    /// request failed or timed out, or the capability went away; a reply
    /// that never ended reports nothing partial.
    ///
    /// `incomplete`: a resumed request whose reply reached its limit, so
    /// lines between the reference and the oldest line returned may be
    /// missing.
    ChannelHistory {
        channel: String,
        messages: Vec<HistoryMessage>,
        incomplete: bool,
    },
    /// Whether older channel history can be asked for on this connection
    /// ([`Connection::request_older_history`]): registered with
    /// `draft/chathistory` negotiated. Sent when that changes; a new
    /// connection starts without it.
    HistoryAvailable(bool),
    /// The end of older-history request `request` for `channel`: its lines
    /// in the server's order (oldest first), none when it ended otherwise.
    /// Exactly one follows every accepted request on this connection.
    OlderChannelHistory {
        channel: String,
        request: u64,
        messages: Vec<HistoryMessage>,
        status: OlderHistoryStatus,
    },
    /// The server echoed our message `local_id` back. `text` is the final
    /// text when the server changed it (and the local line is the text
    /// itself); `msgid` and `server_time` are what the server recorded.
    OutgoingConfirmed {
        local_id: u64,
        text: Option<String>,
        /// The final text of a CTCP ACTION, which `text` never replaces.
        action: Option<String>,
        msgid: Option<String>,
        server_time: Option<SystemTime>,
    },
    /// Our message `local_id` was rejected, or not confirmed in time.
    OutgoingFailed {
        local_id: u64,
        reason: String,
    },
    Disconnected(String),
    /// The user ended the connection (Disconnect, `/quit`, quitting the
    /// app). Terminal like `Disconnected`; nothing should reconnect.
    Closed(String),
    /// The server rejected credentials or this configuration. Terminal like
    /// `Disconnected`, but reconnecting with the same settings would only be
    /// rejected again, so callers must not retry automatically.
    Refused(String),
}

/// One operator or voice grant (`+o nick`) or removal (`-v nick`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivilegeChange {
    pub granted: bool,
    /// `o` or `v`.
    pub mode: char,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelActivityKind {
    Joined {
        mask: Option<String>,
    },
    Left {
        reason: Option<String>,
    },
    Quit {
        reason: Option<String>,
    },
    /// The actor's old nickname; `to` is the new one.
    NickChanged {
        to: String,
    },
    ModeChanged {
        /// The display text; arguments may be hidden or wrongly paired.
        modes: String,
        /// The `+o`/`-o`/`+v`/`-v` changes whose targets are certain; empty
        /// when the mode string cannot be paired with its arguments.
        privileges: Vec<PrivilegeChange>,
        /// Inside an IRCv3 history batch.
        replayed: bool,
    },
    /// `target` was removed from the channel by the actor. `own` is set
    /// when that is us.
    Kicked {
        target: String,
        reason: Option<String>,
        own: bool,
    },
    /// The server refused our JOIN (the actor is `server`). `banned` is set
    /// for `ERR_BANNEDFROMCHAN`; `bad_key` for `ERR_BADCHANNELKEY` (475),
    /// the only refusal a channel key can cure; `reason` is the server's text.
    JoinFailed {
        banned: bool,
        bad_key: bool,
        reason: String,
    },
}

impl ChannelActivityKind {
    /// Whether the user should not miss this line.
    pub fn is_alert(&self) -> bool {
        matches!(self, Self::Kicked { .. } | Self::JoinFailed { .. })
    }
}

/// Why a `SETNAME` did not change the realname.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealNameFailure {
    /// `setname` is not enabled on this connection, or it is not registered.
    Unsupported,
    /// Too many requests are still waiting for the server.
    Busy,
    /// `FAIL SETNAME`; carries the server's description.
    Rejected(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WhoisInfo {
    pub nickname: String,
    pub username: Option<String>,
    pub host: Option<String>,
    pub realname: Option<String>,
    /// The real name carried a mark for a CTCP AVATAR avatar (removed from
    /// `realname`).
    pub avatar_mark: bool,
    pub channels: Vec<String>,
    pub server: Option<String>,
    pub server_info: Option<String>,
    pub away: Option<String>,
    pub idle_seconds: Option<u64>,
    /// Sign-on time as Unix seconds.
    pub signon: Option<i64>,
    pub account: Option<String>,
    pub operator: Option<String>,
    /// Other WHOIS numerics (certificate, secure connection, real host...).
    pub extra: Vec<String>,
}

impl WhoisInfo {
    /// False when the server ended WHOIS without a 311 user reply.
    pub fn found(&self) -> bool {
        self.username.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberCommand {
    Whois,
    Invite { channel: String },
    GiveOp { channel: String },
    Deop { channel: String },
}

/// Set by [`Connection::shutdown`]. It reaches the worker without going
/// through the command or event queues, which a flooded or undrained
/// connection can leave full.
#[derive(Default)]
struct QuitSignal {
    requested: AtomicBool,
    wake: Notify,
}

impl QuitSignal {
    fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    async fn requested(&self) {
        if !self.is_requested() {
            self.wake.notified().await;
        }
    }
}

/// The worker's event queue. Once a shutdown was requested nobody needs the
/// events, so sending no longer waits for the UI to drain them; otherwise a
/// full queue would keep the worker from writing QUIT.
#[derive(Clone)]
struct EventSender {
    events: mpsc::Sender<Event>,
    quit: Arc<QuitSignal>,
}

impl EventSender {
    #[cfg(test)]
    fn new(events: mpsc::Sender<Event>) -> Self {
        Self {
            events,
            quit: Arc::default(),
        }
    }

    async fn send(&self, event: Event) -> Result<(), ()> {
        if self.quit.is_requested() {
            // Dropping the event is fine; a closed receiver is not an error here.
            let _ = self.events.try_send(event);
            return Ok(());
        }
        tokio::select! {
            sent = self.events.send(event) => sent.map_err(|_| ()),
            _ = self.quit.requested() => Ok(()),
        }
    }
}

/// The next command, with a shutdown request counting as a plain quit.
async fn next_command(
    commands: &mut mpsc::Receiver<Outgoing>,
    quit: &QuitSignal,
) -> Option<Outgoing> {
    tokio::select! {
        biased;
        _ = quit.requested() => Some(Outgoing::Quit),
        command = commands.recv() => command,
    }
}

async fn diagnostic(events: &EventSender, started: Instant, message: impl Into<String>) {
    let _ = events
        .send(Event::Diagnostic {
            elapsed: started.elapsed(),
            message: message.into(),
        })
        .await;
}

const REDACTED_KEY: &str = "[redacted]";

/// `modes` with every argument hidden when they set or clear a channel key.
/// The parser pairs arguments with modes by its own table (a `-l` takes one),
/// so in `-l+k key` the key lands on the limit; hiding only the key's own
/// argument cannot be trusted.
fn redact_key_modes(modes: &[Mode<ChannelMode>]) -> Vec<Mode<ChannelMode>> {
    if !modes.iter().any(|mode| {
        matches!(
            mode,
            Mode::Plus(ChannelMode::Key, _) | Mode::Minus(ChannelMode::Key, _)
        )
    }) {
        return modes.to_vec();
    }
    modes
        .iter()
        .map(|mode| match mode {
            Mode::Plus(m, Some(_)) => Mode::Plus(m.clone(), Some(REDACTED_KEY.into())),
            Mode::Minus(m, Some(_)) => Mode::Minus(m.clone(), Some(REDACTED_KEY.into())),
            other => other.clone(),
        })
        .collect()
}

/// The operator and voice changes of a `MODE`, for the conversation log.
///
/// The parser pairs arguments with modes by its own table, which is wrong
/// for a `-l` (no argument) and unknown for other letters, so the arguments
/// it consumed (in order) are paired again here. Only modes whose argument
/// count is the same on every server are understood; any other letter, a
/// missing argument or an argument left over makes the whole line
/// untrusted and yields nothing. `h`, `a` and `q` mean different things per
/// server (`PREFIX`), so they are not privileges here.
fn privilege_changes(modes: &[Mode<ChannelMode>]) -> Vec<PrivilegeChange> {
    let mut flags = Vec::new();
    let mut arguments = Vec::new();
    for mode in modes {
        let (granted, mode, argument) = match mode {
            Mode::Plus(mode, argument) => (true, mode, argument),
            Mode::Minus(mode, argument) => (false, mode, argument),
            Mode::NoPrefix(_) => return Vec::new(),
        };
        flags.push((granted, mode));
        arguments.extend(argument);
    }
    let mut arguments = arguments.into_iter();
    let mut changes = Vec::new();
    for (granted, mode) in flags {
        let takes_argument = match mode {
            ChannelMode::Oper
            | ChannelMode::Voice
            | ChannelMode::Ban
            | ChannelMode::Exception
            | ChannelMode::InviteException
            | ChannelMode::Key => true,
            ChannelMode::Limit => granted,
            ChannelMode::InviteOnly
            | ChannelMode::Moderated
            | ChannelMode::RegisteredOnly
            | ChannelMode::Secret
            | ChannelMode::ProtectedTopic
            | ChannelMode::NoExternalMessages => false,
            _ => return Vec::new(),
        };
        let target = if takes_argument {
            match arguments.next() {
                Some(target) => Some(target),
                None => return Vec::new(),
            }
        } else {
            None
        };
        if let (ChannelMode::Oper | ChannelMode::Voice, Some(target)) = (mode, target)
            && !target.is_empty()
        {
            changes.push(PrivilegeChange {
                granted,
                mode: if *mode == ChannelMode::Oper { 'o' } else { 'v' },
                target: target.clone(),
            });
        }
    }
    if arguments.next().is_some() {
        return Vec::new();
    }
    changes
}

/// Mode arguments as separate words (`MODE` after the target, or
/// `RPL_CHANNELMODEIS` after the nickname and channel): when the mode string
/// sets or clears a key, which argument is the key depends on the other
/// modes, so every argument is hidden.
fn redact_mode_words(words: &[String]) -> Option<Vec<String>> {
    let modes = words.first()?;
    (modes.starts_with(['+', '-']) && modes.contains('k') && words.len() > 1).then(|| {
        let mut redacted = vec![modes.clone()];
        redacted.extend(words[1..].iter().map(|_| REDACTED_KEY.to_owned()));
        redacted
    })
}

/// `message` with a channel key in a `MODE` or `324` hidden, if it has one.
fn without_channel_key(message: &IrcMessage) -> Option<IrcMessage> {
    let command = match &message.command {
        IrcCommand::ChannelMODE(channel, modes) => {
            IrcCommand::ChannelMODE(channel.clone(), redact_key_modes(modes))
        }
        IrcCommand::Raw(verb, args)
            if verb.eq_ignore_ascii_case("MODE")
                && args.first().is_some_and(|t| valid_channel(t)) =>
        {
            let mut redacted = vec![args[0].clone()];
            redacted.extend(redact_mode_words(&args[1..])?);
            IrcCommand::Raw(verb.clone(), redacted)
        }
        IrcCommand::Response(Response::RPL_CHANNELMODEIS, args) if args.len() > 2 => {
            let mut redacted = args[..2].to_vec();
            redacted.extend(redact_mode_words(&args[2..])?);
            IrcCommand::Response(Response::RPL_CHANNELMODEIS, redacted)
        }
        _ => return None,
    };
    let mut redacted = message.clone();
    redacted.command = command;
    Some(redacted)
}

fn redacted_wire_line(message: &IrcMessage) -> String {
    if let Some(redacted) = without_channel_key(message) {
        return tags::transcript_line(&redacted);
    }
    match &message.command {
        IrcCommand::PASS(_) => "PASS [redacted]".into(),
        IrcCommand::AUTHENTICATE(data)
            if data != "+" && data != "*" && !data.eq_ignore_ascii_case("PLAIN") =>
        {
            "AUTHENTICATE [redacted]".into()
        }
        IrcCommand::OPER(name, _) => format!("OPER {name} [redacted]"),
        IrcCommand::JOIN(channel, Some(_), _) => format!("JOIN {channel} [redacted]"),
        IrcCommand::NICKSERV(_) => "NICKSERV [redacted]".into(),
        IrcCommand::CHANSERV(_) => "CHANSERV [redacted]".into(),
        IrcCommand::PRIVMSG(target, body) | IrcCommand::NOTICE(target, body)
            if is_service_secret(target, body) =>
        {
            let verb = if matches!(message.command, IrcCommand::PRIVMSG(_, _)) {
                "PRIVMSG"
            } else {
                "NOTICE"
            };
            format!("{verb} {target} :[redacted]")
        }
        IrcCommand::Raw(verb, _) if verb.eq_ignore_ascii_case("PASS") => "PASS [redacted]".into(),
        IrcCommand::Raw(verb, args) if verb.eq_ignore_ascii_case("AUTHENTICATE") => {
            if args.first().is_some_and(|arg| arg == "+" || arg == "*") {
                message
                    .to_string()
                    .trim_end_matches(['\r', '\n'])
                    .to_owned()
            } else {
                "AUTHENTICATE [redacted]".into()
            }
        }
        IrcCommand::Raw(verb, args) if verb.eq_ignore_ascii_case("OPER") => {
            format!(
                "OPER {} [redacted]",
                args.first().map(String::as_str).unwrap_or("")
            )
        }
        // `/quote AUTH user password` on networks with a server-side AUTH.
        IrcCommand::Raw(verb, _) if verb.eq_ignore_ascii_case("AUTH") => "AUTH [redacted]".into(),
        IrcCommand::Raw(verb, args)
            if matches!(
                verb.to_ascii_uppercase().as_str(),
                "NS" | "NICKSERV" | "CS" | "CHANSERV" | "AUTHSERV"
            ) && args
                .first()
                .and_then(|arg| arg.split_whitespace().next())
                .is_some_and(is_secret_service_command) =>
        {
            format!("{verb} [redacted]")
        }
        _ => tags::transcript_line(message),
    }
}

/// Service commands that carry a password: NickServ/ChanServ (Atheme,
/// Anope), QuakeNet's Q (`AUTH`, `NEWPASS`), Undernet's X (`LOGIN`,
/// `NEWPASS`) and GameSurge's AuthServ (`AUTH`, `PASS`).
fn is_secret_service_command(command: &str) -> bool {
    matches!(
        command.to_ascii_uppercase().as_str(),
        "IDENTIFY"
            | "ID"
            | "LOGIN"
            | "AUTH"
            | "REGISTER"
            | "GHOST"
            | "RECOVER"
            | "RELEASE"
            | "SET"
            | "NEWPASS"
            | "PASS"
    )
}

/// Whether `target` is a service that takes passwords, by nickname, also as
/// `nick@server` (`Q@CServe.quakenet.org`, `X@channels.undernet.org`).
pub fn is_credential_service(target: &str) -> bool {
    let name = target.split('@').next().unwrap_or(target);
    matches!(
        name.to_ascii_uppercase().as_str(),
        "NICKSERV" | "CHANSERV" | "USERSERV" | "Q" | "X" | "AUTHSERV"
    )
}

/// Whether `body` sent to `target` gives a service a password.
fn is_service_secret(target: &str, body: &str) -> bool {
    is_credential_service(target)
        && body
            .split_whitespace()
            .next()
            .is_some_and(is_secret_service_command)
}

/// What our own message shows in its conversation: credentials sent to
/// services (`IDENTIFY`, `REGISTER`, ...) are not shown, as in the
/// transcript.
fn echo_text(target: &str, text: String) -> String {
    if is_service_secret(target, &text) {
        "[redacted]".into()
    } else {
        text
    }
}

/// The worker's side of the diagnostic transcript. IRC lines are formatted
/// and sent to the UI only while someone may read them: until registration
/// completes (the transcript is shown while a connection does not come up)
/// and while the UI asks for them ([`Connection::set_transcript`]).
struct Transcript {
    events: EventSender,
    started: Instant,
    requested: Arc<AtomicBool>,
    registered: bool,
}

impl Transcript {
    /// The line to record for `message`, if lines are recorded now.
    fn line(&self, message: &IrcMessage) -> Option<String> {
        (!self.registered || self.requested.load(Ordering::Relaxed))
            .then(|| redacted_wire_line(message))
    }

    async fn record(&self, direction: WireDirection, line: Option<String>) {
        if let Some(line) = line {
            let _ = self
                .events
                .send(Event::Wire {
                    elapsed: self.started.elapsed(),
                    direction,
                    line,
                })
                .await;
        }
    }
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut result = error.to_string();
    let mut source = error.source();
    for _ in 0..4 {
        let Some(next) = source else { break };
        let detail = next.to_string();
        if !result.contains(&detail) {
            result.push_str(": ");
            result.push_str(&detail);
        }
        source = next.source();
    }
    result
}

/// Whether we are in `channel`, under IRC case mapping. The library's channel
/// list is copied only when this is asked, not for every message.
fn is_joined(client: &Client, casemapping: CaseMapping, channel: &str) -> bool {
    client
        .list_channels()
        .is_some_and(|joined| joined.iter().any(|name| casemapping.same(name, channel)))
}

fn stream_error_detail(error: &irc::error::Error) -> String {
    match error {
        irc::error::Error::CodecFailed { codec, .. } => {
            format!("IRC line codec ({codec}) failed; undecodable line omitted.")
        }
        irc::error::Error::InvalidMessage { .. } => {
            "Invalid IRC line; unparsed line omitted.".into()
        }
        _ => error_chain(error),
    }
}

#[derive(Debug)]
enum Outgoing {
    Message {
        target: String,
        text: String,
        display_text: String,
        notice: bool,
        /// The encoding of `text` fixed when the connection handle accepted
        /// the message (`None`: the server's), so the checks of the worker
        /// and the bytes the codec writes agree with the one that accepted
        /// it even if the channel's setting changes meanwhile.
        encoder: Option<&'static encoding_rs::Encoding>,
    },
    Raw(IrcMessage),
    /// Our own CTCP request to a user (`/ctcp`, `/ping`); the worker adds the
    /// token of a timed PING and paces the requests.
    Ctcp {
        nick: String,
        tag: String,
        args: String,
    },
    /// Publish (`Some`) or remove (`None`) our own avatar; `request`
    /// identifies the outcome events.
    OwnAvatar {
        request: u64,
        url: Option<String>,
    },
    /// The URL answered to CTCP AVATAR queries (`None`: stop answering).
    ShareAvatar(Option<String>),
    /// Ask this user for their avatar with CTCP AVATAR now.
    QueryAvatar(String),
    /// Change the realname with `SETNAME`; the text is the configured value
    /// without the avatar mark.
    SetName(String),
    /// One page of `channel`'s history before `reference`.
    OlderHistory {
        channel: String,
        request: u64,
        reference: MessageReference,
        limit: usize,
    },
    /// Verify the link now (see [`Connection::check_alive`]); the duration is
    /// how long the server may stay silent.
    CheckAlive(Duration),
    Quit,
    /// `Quit` with the reason the user typed instead of the configured one.
    QuitWith(String),
}

#[cfg(test)]
fn validate_outgoing(outgoing: &Outgoing, encoding: &str) -> Result<(), String> {
    validate_outgoing_relayed(outgoing, encoding, 0)
}

/// [`validate_outgoing`] where a message to others must also fit behind the
/// `prefix_bytes` the server adds (see [`validate_relayed_wire`]).
fn validate_outgoing_relayed<'w>(
    outgoing: &Outgoing,
    encoding: impl Into<Wire<'w>>,
    prefix_bytes: usize,
) -> Result<(), String> {
    let encoding = encoding.into();
    let message = match outgoing {
        Outgoing::Message {
            target,
            text,
            notice,
            ..
        } => {
            if *notice {
                IrcMessage::from(IrcCommand::NOTICE(target.clone(), text.clone()))
            } else {
                IrcMessage::from(IrcCommand::PRIVMSG(target.clone(), text.clone()))
            }
        }
        Outgoing::Raw(message) => message.clone(),
        // The longest form: PING carries a token.
        Outgoing::Ctcp { nick, tag, args } => IrcMessage::from(IrcCommand::PRIVMSG(
            nick.clone(),
            format!("\u{1}{tag} {args} 0123456789abcdef0123\u{1}"),
        )),
        Outgoing::OwnAvatar { url, .. } => {
            let mut args = vec!["*".to_owned(), "SET".into(), metadata::AVATAR_KEY.into()];
            args.extend(url.clone());
            IrcMessage::from(IrcCommand::Raw("METADATA".into(), args))
        }
        Outgoing::ShareAvatar(url) => IrcMessage::from(IrcCommand::NOTICE(
            "*".into(),
            format!("\u{1}AVATAR {}\u{1}", url.as_deref().unwrap_or_default()),
        )),
        Outgoing::QueryAvatar(nickname) => IrcMessage::from(IrcCommand::PRIVMSG(
            nickname.clone(),
            "\u{1}AVATAR\u{1}".into(),
        )),
        // Built and checked by the worker, whose reference types decide it.
        Outgoing::OlderHistory { channel, .. } => IrcMessage::from(IrcCommand::Raw(
            "CHATHISTORY".into(),
            vec!["BEFORE".into(), channel.clone(), "*".into(), "1".into()],
        )),
        Outgoing::SetName(realname) => IrcMessage::from(IrcCommand::Raw(
            "SETNAME".into(),
            vec![peer_avatar::realname(
                base_realname(realname),
                peer_avatar::carries_mark(encoding.label),
            )],
        )),
        Outgoing::Quit | Outgoing::CheckAlive(_) => return Ok(()),
        Outgoing::QuitWith(reason) => IrcMessage::from(IrcCommand::QUIT(Some(reason.clone()))),
    };
    if let Outgoing::Message { encoder, .. } = outgoing {
        let fixed = Fixed(*encoder);
        let encoding = Wire {
            label: encoding.label,
            charsets: Some(&fixed),
        };
        validate_relayed_wire(&message.to_string(), encoding, prefix_bytes)
    } else {
        validate_wire(&message.to_string(), encoding)
    }
}

/// The selected conversation's target for /me and /msg without a target: a
/// channel or, in a private conversation, the peer's nickname.
fn selected_target(target: Option<&str>) -> Result<&str, String> {
    target
        .filter(|value| valid_channel(value) || valid_nickname(value))
        .ok_or_else(|| "Select a channel or provide an explicit target.".into())
}

fn selected_channel(channel: Option<&str>) -> Result<&str, String> {
    channel
        .filter(|value| valid_channel(value))
        .ok_or_else(|| "Select a channel or provide an explicit target.".into())
}

fn split_word(value: &str) -> (&str, &str) {
    match value.find(char::is_whitespace) {
        Some(index) => (&value[..index], value[index..].trim_start()),
        None => (value, ""),
    }
}

fn ctcp_request(nick: &str, tag: &str, args: &str) -> Result<Outgoing, String> {
    ctcp::check_request(nick, tag, args)?;
    Ok(Outgoing::Ctcp {
        nick: nick.into(),
        tag: tag.to_ascii_uppercase(),
        args: args.into(),
    })
}

fn checked_raw(wire: &str) -> Result<Outgoing, String> {
    if wire.is_empty()
        || wire.starts_with([':', '@'])
        || wire.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0'))
    {
        return Err("Invalid IRC command.".into());
    }
    let (command, _) = split_word(wire);
    if !command.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err("IRC command name must contain only letters.".into());
    }
    let message: IrcMessage = wire
        .parse()
        .map_err(|error| format!("Invalid IRC command: {error}"))?;
    Ok(Outgoing::Raw(message))
}

/// Gives a `PART` without a reason the connection's default one; an
/// explicit reason is kept.
fn with_default_part_reason(mut outgoing: Outgoing, default: Option<&str>) -> Outgoing {
    if let (Outgoing::Raw(message), Some(default)) = (&mut outgoing, default)
        && let IrcCommand::PART(_, reason @ None) = &mut message.command
    {
        *reason = Some(default.to_owned());
    }
    outgoing
}

fn checked_command(command: IrcCommand) -> Result<Outgoing, String> {
    let message = IrcMessage::from(command);
    let wire = message.to_string();
    if wire
        .trim_end_matches(['\r', '\n'])
        .chars()
        .any(|ch| matches!(ch, '\r' | '\n' | '\0'))
    {
        return Err("IRC command contains a protocol control.".into());
    }
    Ok(Outgoing::Raw(message))
}

fn member_outgoing(nickname: &str, action: MemberCommand) -> Result<Outgoing, String> {
    if !valid_nickname(nickname) {
        return Err("Invalid nickname.".into());
    }
    match action {
        MemberCommand::Whois => checked_command(IrcCommand::WHOIS(None, nickname.into())),
        MemberCommand::Invite { channel } => {
            if !valid_channel(&channel) {
                return Err("Invalid invite channel.".into());
            }
            checked_command(IrcCommand::INVITE(nickname.into(), channel))
        }
        MemberCommand::GiveOp { channel } => {
            if !valid_channel(&channel) {
                return Err("Invalid mode channel.".into());
            }
            checked_raw(&format!("MODE {channel} +o {nickname}"))
        }
        MemberCommand::Deop { channel } => {
            if !valid_channel(&channel) {
                return Err("Invalid mode channel.".into());
            }
            checked_raw(&format!("MODE {channel} -o {nickname}"))
        }
    }
}

fn parse_slash_command(line: &str, selected: Option<&str>) -> Result<Outgoing, String> {
    let body = line
        .strip_prefix('/')
        .ok_or("IRC commands must start with /.")?
        .trim();
    let (verb, rest) = split_word(body);
    if verb.is_empty() {
        return Err("Enter an IRC command after /.".into());
    }
    let verb = verb.to_ascii_uppercase();
    match verb.as_str() {
        "ME" => {
            let target = selected_target(selected)?;
            let action = rest.trim_start_matches(':');
            if action.is_empty() {
                return Err("/me requires action text.".into());
            }
            let text = format!("\u{1}ACTION {action}\u{1}");
            validate_message_text(&text)?;
            Ok(Outgoing::Message {
                target: target.into(),
                text,
                display_text: format!("* {action}"),
                notice: false,
                encoder: None,
            })
        }
        "MSG" | "PRIVMSG" | "NOTICE" => {
            let (target, text) = if let Some(text) = rest.strip_prefix(':') {
                (selected_target(selected)?, text)
            } else {
                let (first, remainder) = split_word(rest);
                if remainder.is_empty() {
                    (selected_target(selected)?, first)
                } else {
                    (first, remainder.trim_start_matches(':'))
                }
            };
            if target.is_empty()
                || target
                    .chars()
                    .any(|ch| ch.is_whitespace() || ch.is_control() || ch == ':')
            {
                return Err("Invalid message target.".into());
            }
            validate_message_text(text)?;
            Ok(Outgoing::Message {
                target: target.into(),
                text: text.into(),
                display_text: text.into(),
                notice: verb == "NOTICE",
                encoder: None,
            })
        }
        "PART" | "TOPIC" => {
            let (first, remainder) = split_word(rest);
            let (channel, body) = if valid_channel(first) {
                (first, remainder)
            } else {
                (selected_channel(selected)?, rest)
            };
            let body = body.trim_start_matches(':');
            let command = if verb == "PART" {
                IrcCommand::PART(channel.into(), (!body.is_empty()).then(|| body.into()))
            } else {
                IrcCommand::TOPIC(channel.into(), (!body.is_empty()).then(|| body.into()))
            };
            checked_command(command)
        }
        "KICK" => {
            let (first, remainder) = split_word(rest);
            let (channel, nick_and_reason) = if valid_channel(first) {
                (first, remainder)
            } else {
                (selected_channel(selected)?, rest)
            };
            let (nick, reason) = split_word(nick_and_reason);
            if nick.is_empty() {
                return Err("/kick requires a nickname.".into());
            }
            checked_command(IrcCommand::KICK(
                channel.into(),
                nick.into(),
                (!reason.is_empty()).then(|| reason.trim_start_matches(':').into()),
            ))
        }
        "MODE" => {
            let (first, _) = split_word(rest);
            let wire = if valid_channel(first) {
                format!("MODE {rest}")
            } else {
                format!("MODE {} {rest}", selected_channel(selected)?)
            };
            checked_raw(wire.trim_end())
        }
        "NAMES" => {
            let wire = if rest.is_empty() {
                format!("NAMES {}", selected_channel(selected)?)
            } else {
                format!("NAMES {rest}")
            };
            checked_raw(&wire)
        }
        "INVITE" => {
            let (nickname, channel) = split_word(rest);
            if nickname.is_empty() {
                return Err("/invite requires a nickname.".into());
            }
            let channel = if channel.is_empty() {
                selected_channel(selected)?
            } else {
                channel
            };
            if !valid_channel(channel) {
                return Err("Invalid invite channel.".into());
            }
            checked_command(IrcCommand::INVITE(nickname.into(), channel.into()))
        }
        // Goes through the worker so the reason defaults to the configured
        // QUIT message and the connection ends as a user disconnect.
        "QUIT" => {
            let reason = rest.trim_start_matches(':').trim();
            if reason.is_empty() {
                return Ok(Outgoing::Quit);
            }
            checked_command(IrcCommand::QUIT(Some(reason.into())))?;
            Ok(Outgoing::QuitWith(reason.into()))
        }
        "CTCP" => {
            let (nick, rest) = split_word(rest);
            let (tag, args) = split_word(rest);
            if nick.is_empty() || tag.is_empty() {
                return Err("Usage: /ctcp <nick> <command> [arguments].".into());
            }
            ctcp_request(nick, tag, args)
        }
        // CTCP PING to a user (the round trip), not the server's PING.
        "PING" => {
            let (nick, extra) = split_word(rest);
            if nick.is_empty() || !extra.is_empty() {
                return Err("Usage: /ping <nick>.".into());
            }
            ctcp_request(nick, "PING", "")
        }
        "RAW" | "QUOTE" => checked_raw(rest),
        _ => checked_raw(body),
    }
}

/// Events from the IRC worker. A UI can move this out of its [`Connection`]
/// and await it, instead of polling the connection on a timer.
pub struct Events(mpsc::Receiver<Event>);

impl Events {
    /// Waits for the next event. `None` means the worker has ended and every
    /// event it sent has been received. Works on any async executor.
    pub async fn recv(&mut self) -> Option<Event> {
        self.0.recv().await
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        self.0.try_recv().ok()
    }

    pub fn is_closed(&self) -> bool {
        self.0.is_closed() && self.0.is_empty()
    }
}

pub struct Connection {
    commands: mpsc::Sender<Outgoing>,
    /// Stops a worker that is still resolving or opening the transport,
    /// before it reads queued commands.
    cancel: Arc<Notify>,
    /// Asks the worker to quit even when both queues are full.
    quit: Arc<QuitSignal>,
    /// `None` after [`Connection::take_events`].
    events: Option<Events>,
    encoding: String,
    /// Channels with an encoding of their own for their text.
    charsets: Arc<ChannelCharsets>,
    /// Reason for a `PART` that names none.
    part_message: Option<String>,
    /// The server's announced `MODES` limit; 0 until one is seen.
    modes_per_line: Arc<AtomicUsize>,
    /// Bytes a server adds in front of our relayed lines (see [`relay`]).
    relay_prefix: Arc<AtomicUsize>,
    /// Disconnects once the worker thread has ended.
    closed: Option<Mutex<std_mpsc::Receiver<()>>>,
    /// Whether IRC lines are recorded after registration.
    transcript: Arc<AtomicBool>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

impl Connection {
    pub fn connect(config: ConnectionConfig) -> Result<Self, String> {
        config.validate()?;
        if config.use_tls {
            ensure_tls_crypto_provider()?;
        }
        let encoding = config.encoding.clone();
        let charsets = Arc::new(ChannelCharsets::new(&config.channel_encodings)?);
        let worker_charsets = charsets.clone();
        let part_message = config.wire_part_message();
        let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, events) = mpsc::channel(EVENT_CAPACITY);
        let cancel = Arc::new(Notify::new());
        let worker_cancel = cancel.clone();
        let quit = Arc::new(QuitSignal::default());
        let worker_events = EventSender {
            events: event_tx.clone(),
            quit: quit.clone(),
        };
        let modes_per_line = Arc::new(AtomicUsize::new(0));
        let worker_modes = modes_per_line.clone();
        let relay_prefix = Arc::new(AtomicUsize::new(
            relay::OwnMask::default().prefix_bytes(&config.nickname, &encoding),
        ));
        let worker_relay_prefix = relay_prefix.clone();
        let (alive, closed) = std_mpsc::channel::<()>();
        let transcript = Arc::new(AtomicBool::new(false));
        let worker_transcript = transcript.clone();
        thread::Builder::new()
            .name("cayenchat-irc".into())
            .spawn(move || {
                // Dropped when the worker ends, which `wait_closed` observes.
                let _alive = alive;
                let failure_events = event_tx.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();
                    match runtime {
                        Ok(runtime) => runtime.block_on(run_cancellable(
                            config,
                            command_rx,
                            worker_events,
                            worker_cancel,
                            worker_modes,
                            worker_relay_prefix,
                            worker_transcript,
                            worker_charsets,
                        )),
                        Err(error) => {
                            let _ = event_tx.blocking_send(Event::Disconnected(error.to_string()));
                        }
                    }
                }));
                if result.is_err() {
                    let _ = failure_events.blocking_send(Event::Disconnected(
                        "IRC worker stopped unexpectedly.".into(),
                    ));
                }
            })
            .map_err(|error| format!("Could not start IRC worker: {error}"))?;
        Ok(Self {
            commands,
            cancel,
            quit,
            events: Some(Events(events)),
            encoding,
            charsets,
            part_message,
            modes_per_line,
            relay_prefix,
            closed: Some(Mutex::new(closed)),
            transcript,
        })
    }

    /// Blocks up to `timeout` until the worker has ended, which after
    /// [`Connection::disconnect`] means QUIT was flushed. For quitting the
    /// application, where the process would otherwise end before the worker
    /// writes it. Returns whether the worker ended in time.
    pub fn wait_closed(&self, timeout: Duration) -> bool {
        let Some(closed) = &self.closed else {
            return true;
        };
        let Ok(closed) = closed.lock() else {
            return true;
        };
        matches!(
            closed.recv_timeout(timeout),
            Err(std_mpsc::RecvTimeoutError::Disconnected)
        )
    }

    /// Whether `Event::Wire` lines are sent after registration; they always
    /// are before it. Off by default, since formatting and sending every line
    /// costs work on each message that only the debug transcript reads.
    pub fn set_transcript(&self, on: bool) {
        self.transcript.store(on, Ordering::Relaxed);
    }

    /// Moves the event stream out; afterwards `try_recv` returns `None` and
    /// `is_closed` reports `false` here.
    pub fn take_events(&mut self) -> Option<Events> {
        self.events.take()
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        self.events.as_mut()?.try_recv()
    }

    pub fn is_closed(&self) -> bool {
        self.events.as_ref().is_some_and(Events::is_closed)
    }

    /// A message with the encoding its target uses now fixed in it; whatever
    /// the setting becomes while it waits, it is checked and written with
    /// this one.
    fn pinned(&self, mut outgoing: Outgoing) -> Outgoing {
        if let Outgoing::Message {
            target, encoder, ..
        } = &mut outgoing
        {
            *encoder = self.charsets.encoder(target);
        }
        outgoing
    }

    fn validate(&self, outgoing: &Outgoing) -> Result<(), String> {
        let prefix = relay::published(&self.relay_prefix);
        let wire = Wire {
            label: &self.encoding,
            charsets: Some(&*self.charsets),
        };
        validate_outgoing_relayed(outgoing, wire, prefix)
    }

    /// Sets the encodings `channel` uses for the text of the messages it
    /// receives (`decode`) and sends (`encode`), as WHATWG labels; a side
    /// that is `None` follows the server's encoding, and both `None` removes
    /// the override. Applies to the next line in either direction. Only the
    /// text changes: the channel name keeps the server's encoding. Refused
    /// when the server requires UTF-8 (`UTF8ONLY`) and a label is not UTF-8.
    pub fn set_channel_encoding(
        &self,
        channel: &str,
        decode: Option<&str>,
        encode: Option<&str>,
    ) -> Result<(), String> {
        self.charsets.set(channel, decode, encode)
    }

    /// Whether the server announced `UTF8ONLY`, so channels can only use
    /// UTF-8.
    pub fn utf8_only(&self) -> bool {
        self.charsets.utf8_only()
    }

    pub fn send_message(&self, channel: &str, text: &str, notice: bool) -> Result<(), String> {
        if !valid_channel(channel) {
            return Err("Select a channel before sending.".into());
        }
        validate_message_text(text)?;
        let outgoing = Outgoing::Message {
            target: channel.to_owned(),
            text: text.to_owned(),
            display_text: text.to_owned(),
            notice,
            encoder: None,
        };
        let outgoing = self.pinned(outgoing);
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue IRC message: {error}"))
    }

    pub fn send_private_message(
        &self,
        nickname: &str,
        text: &str,
        notice: bool,
    ) -> Result<(), String> {
        if !valid_nickname(nickname) {
            return Err("Invalid message target.".into());
        }
        validate_message_text(text)?;
        let outgoing = Outgoing::Message {
            target: nickname.to_owned(),
            text: text.to_owned(),
            display_text: text.to_owned(),
            notice,
            encoder: None,
        };
        let outgoing = self.pinned(outgoing);
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue private message: {error}"))
    }

    pub fn send_member_command(&self, nickname: &str, action: MemberCommand) -> Result<(), String> {
        let outgoing = member_outgoing(nickname, action)?;
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue member command: {error}"))
    }

    /// Gives or takes op or voice for several members of `channel` at once.
    /// Lines are split to the server's announced `MODES` limit (3 when it
    /// announced none). Everything is validated before anything is queued.
    pub fn send_member_modes(
        &self,
        channel: &str,
        mode: MemberMode,
        nicknames: &[String],
    ) -> Result<(), String> {
        if !valid_channel(channel) {
            return Err("Invalid mode channel.".into());
        }
        if nicknames.is_empty() {
            return Err("Select at least one member.".into());
        }
        if let Some(bad) = nicknames.iter().find(|nick| !valid_nickname(nick)) {
            return Err(format!("Invalid nickname: {bad}"));
        }
        let announced = self.modes_per_line.load(Ordering::Relaxed);
        let lines = modes::mode_lines(channel, mode, nicknames, modes::effective_limit(announced));
        let outgoing = lines
            .into_iter()
            .map(|args| {
                // Sent as given: parsing "+vv" into modes and back would
                // write it as "+v+v".
                let outgoing = checked_command(IrcCommand::Raw("MODE".into(), args))?;
                self.validate(&outgoing)?;
                Ok(outgoing)
            })
            .collect::<Result<Vec<_>, String>>()?;
        // Everything or nothing: this thread is the only sender, so room that
        // exists now stays until the lines are queued.
        if self.commands.capacity() < outgoing.len() {
            return Err("The connection is busy; try again in a moment.".into());
        }
        for outgoing in outgoing {
            self.commands
                .try_send(outgoing)
                .map_err(|error| format!("Could not queue member command: {error}"))?;
        }
        Ok(())
    }

    pub fn send_command(&self, line: &str, selected_channel: Option<&str>) -> Result<(), String> {
        let outgoing = parse_slash_command(line, selected_channel)?;
        let outgoing = with_default_part_reason(outgoing, self.part_message.as_deref());
        let outgoing = self.pinned(outgoing);
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue IRC command: {error}"))
    }

    /// Sends `JOIN channel [key]`. The key is a secret: it is validated here,
    /// and the transcript shows `[redacted]` for it.
    pub fn join_channel(&self, channel: &str, key: Option<&str>) -> Result<(), String> {
        if !valid_channel(channel) {
            return Err("Invalid channel name.".into());
        }
        if key.is_some_and(|key| !valid_channel_key(key)) {
            return Err("Channel key must not be empty or contain spaces or controls.".into());
        }
        let outgoing = checked_command(IrcCommand::JOIN(
            channel.to_owned(),
            key.map(str::to_owned),
            None,
        ))?;
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue IRC command: {error}"))
    }

    pub fn change_nickname(&self, nickname: &str) -> Result<(), String> {
        if !valid_nickname(nickname) {
            return Err("Invalid nickname.".into());
        }
        let outgoing = checked_command(IrcCommand::NICK(nickname.to_owned()))?;
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue nickname change: {error}"))
    }

    /// Publishes our avatar URL (`Some`) or removes our avatar (`None`) on
    /// this connection, through experimental IRCv3 metadata. Only the
    /// `avatar` key is touched. The outcome arrives as
    /// [`Event::OwnAvatar`] with this `request`, or
    /// [`Event::OwnAvatarFailed`]; queuing it says nothing about success.
    /// The URL must already satisfy the caller's URL policy.
    pub fn set_own_avatar(&self, request: u64, url: Option<&str>) -> Result<(), String> {
        if let Some(url) = url {
            publishable_avatar(url, self.encoding.eq_ignore_ascii_case("UTF-8"))?;
        }
        let outgoing = Outgoing::OwnAvatar {
            request,
            url: url.map(str::to_owned),
        };
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue avatar request: {error}"))
    }

    /// Shares `url` with other clients through CTCP AVATAR from now on, or
    /// stops (`None`); it has an effect only when the connection was made
    /// with [`Ircv3Options::peer_avatars`]. The realname mark sent at
    /// registration stays until the next connection. The URL must already
    /// satisfy the caller's URL policy.
    pub fn share_avatar(&self, url: Option<&str>) -> Result<(), String> {
        if let Some(url) = url {
            peer_avatar::shareable(url, self.encoding.eq_ignore_ascii_case("UTF-8"))?;
        }
        let outgoing = Outgoing::ShareAvatar(url.map(str::to_owned));
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue avatar sharing: {error}"))
    }

    /// Asks `nickname` for their avatar with CTCP AVATAR, even if they were
    /// asked before. It has an effect only when the connection was made with
    /// [`Ircv3Options::peer_avatars`] and we share a channel with the user;
    /// an answer arrives as [`Event::UserAvatar`].
    pub fn request_peer_avatar(&self, nickname: &str) -> Result<(), String> {
        if !valid_nickname(nickname) {
            return Err("Invalid nickname.".into());
        }
        let outgoing = Outgoing::QueryAvatar(nickname.to_owned());
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue avatar request: {error}"))
    }

    /// Changes the realname on this connection with IRCv3 `SETNAME`. The
    /// outcome arrives as [`Event::RealNameChanged`] or
    /// [`Event::RealNameFailed`]; without `setname` the latter is immediate
    /// and the value applies at the next connection. `Err` means the value
    /// cannot be sent and nothing was queued.
    pub fn set_real_name(&self, realname: &str) -> Result<(), String> {
        check_realname(realname)?;
        let outgoing = Outgoing::SetName(realname.to_owned());
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue realname change: {error}"))
    }

    /// Asks for up to `limit` lines of `channel`'s history before
    /// `reference` (the oldest message the caller holds), as request
    /// `request`. The lines, or the reason there are none, arrive as one
    /// [`Event::OlderChannelHistory`] with the same `request`; when history
    /// is not available ([`Event::HistoryAvailable`]) it arrives at once as
    /// a failure. `Err` means nothing was queued and no event follows.
    pub fn request_older_history(
        &self,
        channel: &str,
        request: u64,
        reference: MessageReference,
        limit: usize,
    ) -> Result<(), String> {
        if !valid_channel(channel) {
            return Err("Invalid history target.".into());
        }
        let outgoing = Outgoing::OlderHistory {
            channel: channel.to_owned(),
            request,
            reference,
            limit,
        };
        self.validate(&outgoing)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue history request: {error}"))
    }

    /// Verifies that the server still answers, for after the machine has
    /// slept: the worker sends a `PING` to the server and, when no line at
    /// all arrives within [`ALIVE_CHECK_TIMEOUT`], ends with
    /// `Event::Disconnected`. A connection that answers is left alone. Only a
    /// registered connection is checked, and a check already running is not
    /// restarted or extended. Messages and commands sent meanwhile are held
    /// and written once the server has answered; if it has not, they are
    /// reported as not sent.
    pub fn check_alive(&self) -> Result<(), String> {
        self.queue_alive_check(ALIVE_CHECK_TIMEOUT)
    }

    fn queue_alive_check(&self, timeout: Duration) -> Result<(), String> {
        self.commands
            .try_send(Outgoing::CheckAlive(timeout))
            .map_err(|error| format!("Could not queue connection check: {error}"))
    }

    /// Like [`Connection::disconnect`] for an application that is exiting:
    /// QUIT goes out even when the command or event queue is full, and the
    /// remaining events are dropped. Follow with [`Connection::wait_closed`].
    pub fn shutdown(&self) {
        self.cancel.notify_one();
        self.quit.request();
    }

    pub fn disconnect(&self) -> Result<(), String> {
        // A worker still setting up the transport ends at once; a connected
        // one takes QUIT from the queue and flushes it first.
        self.cancel.notify_one();
        self.commands
            .try_send(Outgoing::Quit)
            .map_err(|error| format!("Could not queue disconnect: {error}"))
    }
}

fn library_config(config: &ConnectionConfig) -> Config {
    Config {
        server: Some(config.host.clone()),
        port: Some(config.port),
        nickname: Some(config.nickname.clone()),
        username: Some(config.username.clone()),
        realname: Some(config.wire_realname()),
        password: config.server_password.clone(),
        channels: config.channels.clone(),
        channel_keys: config
            .channel_keys
            .iter()
            .map(|(channel, key)| (channel.clone(), key.clone()))
            .collect(),
        ping_timeout: Some(PING_TIMEOUT_SECS),
        use_tls: Some(config.use_tls),
        encoding: Some(config.encoding.clone()),
        dangerously_accept_invalid_certs: Some(config.use_tls && !config.verify_tls_certificates),
        ..Config::default()
    }
}

#[cfg(test)]
async fn run(
    config: ConnectionConfig,
    commands: mpsc::Receiver<Outgoing>,
    events: mpsc::Sender<Event>,
) {
    let config_encodings = config.channel_encodings.clone();
    run_cancellable(
        config,
        commands,
        EventSender::new(events),
        Arc::new(Notify::new()),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(true)),
        Arc::new(ChannelCharsets::new(&config_encodings).expect("valid channel encodings")),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_cancellable(
    config: ConnectionConfig,
    mut commands: mpsc::Receiver<Outgoing>,
    events: EventSender,
    cancel: Arc<Notify>,
    modes_per_line: Arc<AtomicUsize>,
    relay_prefix: Arc<AtomicUsize>,
    transcript: Arc<AtomicBool>,
    charsets: Arc<ChannelCharsets>,
) {
    let started = Instant::now();
    let transcript = Transcript {
        events: events.clone(),
        started,
        requested: transcript,
        registered: false,
    };
    let host = config.host.clone();
    let port = config.port;
    let use_tls = config.use_tls;
    let verify_tls_certificates = config.verify_tls_certificates;
    diagnostic(
        &events,
        started,
        format!(
            "Resolving {host}:{port} (TLS: {}, encoding: {}).",
            if use_tls { "on" } else { "off" },
            config.encoding
        ),
    )
    .await;
    let lookup = tokio::select! {
        lookup = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((host.as_str(), port)),
        ) => lookup,
        _ = cancel.notified() => {
            let _ = events.send(Event::Disconnected(USER_DISCONNECT.into())).await;
            return;
        }
    };
    match lookup {
        Ok(Ok(addresses)) => {
            diagnostic(
                &events,
                started,
                format!("DNS lookup returned {} address(es).", addresses.count()),
            )
            .await
        }
        Ok(Err(error)) => {
            diagnostic(
                &events,
                started,
                format!("DNS lookup failed: {error}. Transport will still be attempted."),
            )
            .await
        }
        Err(_) => {
            diagnostic(
                &events,
                started,
                "DNS lookup did not finish within 3 seconds. Transport will still be attempted.",
            )
            .await
        }
    }
    let wire_encoding = config.encoding.clone();
    let server_password = config.server_password.clone();
    let registration_nick = config.nickname.clone();
    let targets_since = config.history_targets_since;
    let registration_user = config.username.clone();
    let auto_join_channels = config.channels.clone();
    let mut irc_config = library_config(&config);
    irc_config.body_charsets = Some(irc::proto::charsets::BodyCharsetsHandle(charsets.clone()));
    let advertise_avatar = config.advertises_avatar();
    let registration_realname = config.wire_realname();
    let quit_message = config.wire_quit_message();
    let peers = config.ircv3.peer_avatars.then(|| {
        peer_avatar::PeerAvatars::new(
            wire_encoding.eq_ignore_ascii_case("UTF-8"),
            config.shared_avatar.clone(),
        )
    });
    let ctcp = ctcp::CtcpReplies::new(config.ircv3.peer_avatars, &config.host);
    let mut negotiation = cap::CapNegotiation::new(
        config.ircv3,
        config.sasl,
        wire_encoding.eq_ignore_ascii_case("UTF-8"),
    );
    if config.ircv3.message_tags && !wire_encoding.eq_ignore_ascii_case("UTF-8") {
        diagnostic(
            &events,
            started,
            format!(
                "Message tags are not requested with the {wire_encoding} encoding: \
                 tag values are UTF-8."
            ),
        )
        .await;
    }
    if config.ircv3.metadata && !wire_encoding.eq_ignore_ascii_case("UTF-8") {
        diagnostic(
            &events,
            started,
            format!(
                "Avatar metadata with the {wire_encoding} encoding: only ASCII avatar URLs \
                 are used, because metadata values are UTF-8."
            ),
        )
        .await;
    }
    if peers.is_some() {
        diagnostic(
            &events,
            started,
            if advertise_avatar {
                "Peer avatars (CTCP AVATAR) on; the realname marks a shared avatar."
            } else {
                "Peer avatars (CTCP AVATAR) on; nothing is shared."
            },
        )
        .await;
    }
    diagnostic(
        &events,
        started,
        if use_tls && verify_tls_certificates {
            "Opening TCP connection and performing TLS handshake with certificate verification."
        } else if use_tls {
            "Opening TCP connection and performing TLS handshake without certificate verification."
        } else {
            "Opening TCP connection."
        },
    )
    .await;
    let mut connecting = Box::pin(Client::from_config(irc_config));
    let mut connect_timeout = Box::pin(tokio::time::sleep(CONNECT_TIMEOUT));
    let mut progress = tokio::time::interval(Duration::from_secs(5));
    progress.tick().await;
    let result = loop {
        tokio::select! {
            result = &mut connecting => break Some(result),
            _ = &mut connect_timeout => break None,
            _ = cancel.notified() => {
                diagnostic(&events, started, "Transport setup cancelled by the user.").await;
                let _ = events.send(Event::Closed(USER_DISCONNECT.into())).await;
                return;
            }
            _ = progress.tick() => diagnostic(&events, started,
                "Still waiting for TCP connection or TLS handshake.").await,
        }
    };
    let mut client = match result {
        Some(Ok(client)) => client,
        Some(Err(error)) => {
            let detail = error_chain(&error);
            diagnostic(&events, started, format!("Transport failed: {detail}")).await;
            let _ = events.send(Event::Disconnected(detail)).await;
            return;
        }
        None => {
            let detail = format!(
                "TCP/TLS setup timed out after {} seconds.",
                CONNECT_TIMEOUT.as_secs()
            );
            diagnostic(&events, started, &detail).await;
            let _ = events.send(Event::Disconnected(detail)).await;
            return;
        }
    };
    diagnostic(
        &events,
        started,
        if use_tls {
            "TLS transport established."
        } else {
            "TCP transport established."
        },
    )
    .await;
    if events.send(Event::TransportConnected).await.is_err() {
        return;
    }
    let mut stream = match client.stream() {
        Ok(stream) => stream,
        Err(error) => {
            let detail = error_chain(&error);
            diagnostic(
                &events,
                started,
                format!("Could not open IRC stream: {detail}"),
            )
            .await;
            let _ = events.send(Event::Disconnected(detail)).await;
            return;
        }
    };
    let opening = negotiation.start();
    diagnostic(
        &events,
        started,
        if negotiation.uses_sasl() {
            "Sending CAP LS, optional PASS, NICK, and USER; waiting for SASL and welcome."
        } else if negotiation.negotiating() {
            "Sending CAP LS, optional PASS, NICK, and USER; negotiating opt-in IRCv3 capabilities."
        } else {
            "Sending optional PASS, NICK, and USER; waiting for server welcome (001)."
        },
    )
    .await;
    // Mirror the library's identify() sequence so every registration command
    // can be included in the diagnostic transcript after it is queued.
    let mut registration = vec![opening];
    if let Some(password) = server_password.filter(|value| !value.is_empty()) {
        registration.push(IrcCommand::PASS(password));
    }
    registration.push(IrcCommand::NICK(registration_nick.clone()));
    registration.push(IrcCommand::USER(
        registration_user,
        "0".into(),
        registration_realname,
    ));
    for command in registration {
        let message = IrcMessage::from(command);
        let line = transcript.line(&message);
        if let Err(error) = client.send(message) {
            let detail = error_chain(&error);
            diagnostic(
                &events,
                started,
                format!("Registration send failed: {detail}"),
            )
            .await;
            let _ = events.send(Event::Disconnected(detail)).await;
            return;
        }
        transcript.record(WireDirection::Sent, line).await;
    }
    let mut registration_progress = tokio::time::interval(Duration::from_secs(10));
    registration_progress.tick().await;
    let wire_utf8 = wire_encoding.eq_ignore_ascii_case("UTF-8");
    let mut worker = Worker {
        client,
        events,
        started,
        transcript,
        wire_encoding,
        charsets,
        quit_message,
        advertise_avatar,
        auto_join_channels,
        targets_since,
        track_accounts: config.ircv3.accounts,
        modes_per_line,
        relay_prefix,
        own_mask: relay::OwnMask::default(),
        pending_setname: 0,
        peers,
        ctcp,
        negotiation,
        registration_deadline: tokio::time::Instant::now() + REGISTRATION_TIMEOUT,
        registered: false,
        // Registration is paused while the UI asks for another nickname.
        awaiting_nick: false,
        refusal: None,
        current_nick: registration_nick,
        roster: RosterTracker::default(),
        whois: WhoisCollector::default(),
        replay: replay::ReplayTracker::default(),
        batch_negotiated: false,
        metadata: metadata::MetadataState::new(wire_utf8),
        metadata_enabled: false,
        casemapping: CaseMapping::default(),
        history: history::HistoryRequests::with_resume(config.resume_history),
        history_enabled: false,
        echoes: echo::Echoes::default(),
        next_local_id: 0,
        reported_unsent: 0,
        accounts: accounts::Accounts::default(),
        channel_modes: chanmodes::ChannelModes::default(),
        targets_asked: false,
        // Last reported Event::HistoryAvailable.
        history_available: false,
        alive_check: None,
        alive_checks: 0,
        held: VecDeque::new(),
    };

    loop {
        tokio::select! {
            command = next_command(&mut commands, &worker.events.quit) => {
                if worker.on_command(command, &mut stream).await.is_break() {
                    break;
                }
            }
            incoming = stream.next() => {
                if worker.on_incoming(incoming).await.is_break() {
                    break;
                }
            }
            // Deferred avatar synchronizations (774), request timeouts and
            // spaced-out lookups; no timer runs while none is pending.
            _ = tokio::time::sleep_until(until(worker.metadata_deadline())),
                if worker.metadata_deadline().is_some() =>
            {
                if worker.on_metadata_deadline().await.is_break() {
                    break;
                }
            }
            // Report an unconfirmed outgoing message even when the server
            // sends no further lines. No timer runs while none is pending.
            _ = tokio::time::sleep_until(until(worker.echo_deadline())),
                if worker.echo_deadline().is_some() =>
            {
                if worker.on_echo_deadline().await.is_break() {
                    break;
                }
            }
            // An unanswered history request; no timer runs otherwise.
            _ = tokio::time::sleep_until(until(worker.history_deadline())),
                if worker.history_deadline().is_some() =>
            {
                if worker.on_history_deadline().await.is_break() {
                    break;
                }
            }
            // A channel mode query that was never answered frees its place
            // for the next; no timer runs while none is waiting.
            _ = tokio::time::sleep_until(until(worker.mode_query_deadline())),
                if worker.mode_query_deadline().is_some() =>
            {
                if worker.on_mode_query_deadline().await.is_break() {
                    break;
                }
            }
            // Peer avatar lookups and queries, spaced out; no timer runs
            // while none is queued or outstanding.
            _ = tokio::time::sleep_until(until(worker.peer_deadline())),
                if worker.peer_deadline().is_some() =>
            {
                if worker.on_peer_deadline().await.is_break() {
                    break;
                }
            }
            // A CTCP PING without a reply is reported once; no timer runs
            // while none is pending.
            _ = tokio::time::sleep_until(until(worker.ctcp.next_deadline())),
                if worker.ctcp.next_deadline().is_some() =>
            {
                if worker.on_ctcp_deadline().await.is_break() {
                    break;
                }
            }
            // The server did not answer an alive check; no timer runs
            // otherwise.
            _ = tokio::time::sleep_until(until(worker.alive_check)),
                if worker.alive_check.is_some() =>
            {
                worker.on_alive_deadline().await;
                break;
            }
            _ = registration_progress.tick(), if worker.waiting_for_registration() => {
                worker.note("Still waiting for IRC registration (001 welcome).").await;
            }
            _ = tokio::time::sleep_until(worker.registration_deadline),
                if worker.waiting_for_registration() =>
            {
                worker.on_registration_timeout().await;
                break;
            }
        }
        // The server answered: write what was held back meanwhile.
        if worker.alive_check.is_none()
            && !worker.held.is_empty()
            && worker.flush_held(&mut stream).await.is_break()
        {
            break;
        }
    }
}

/// `Break` ends the connection loop.
type Flow = ControlFlow<()>;

/// The connection loop's state. `run_cancellable` keeps the command
/// receiver and the IRC stream, which the `select!` polls, and calls one
/// method here per branch.
struct Worker {
    channel_modes: chanmodes::ChannelModes,
    client: Client,
    events: EventSender,
    started: Instant,
    transcript: Transcript,
    wire_encoding: String,
    /// The channels with an encoding of their own for their text.
    charsets: Arc<ChannelCharsets>,
    quit_message: String,
    // The avatar mark is fixed for the connection, also for SETNAME.
    advertise_avatar: bool,
    auto_join_channels: Vec<String>,
    targets_since: Option<SystemTime>,
    track_accounts: bool,
    modes_per_line: Arc<AtomicUsize>,
    relay_prefix: Arc<AtomicUsize>,
    own_mask: relay::OwnMask,
    pending_setname: usize,
    peers: Option<peer_avatar::PeerAvatars>,
    ctcp: ctcp::CtcpReplies,
    negotiation: cap::CapNegotiation,
    registration_deadline: tokio::time::Instant,
    registered: bool,
    awaiting_nick: bool,
    refusal: Option<String>,
    current_nick: String,
    roster: RosterTracker,
    whois: WhoisCollector,
    replay: replay::ReplayTracker,
    batch_negotiated: bool,
    metadata: metadata::MetadataState,
    metadata_enabled: bool,
    casemapping: CaseMapping,
    history: history::HistoryRequests,
    history_enabled: bool,
    echoes: echo::Echoes,
    next_local_id: u64,
    /// Queued lines `UTF8ONLY` kept from being written that were already reported.
    reported_unsent: usize,
    accounts: accounts::Accounts,
    targets_asked: bool,
    history_available: bool,
    /// When the running alive check gives up; any line from the server ends it.
    alive_check: Option<tokio::time::Instant>,
    /// Alive checks started, for their `PING` tokens.
    alive_checks: u64,
    /// Commands received during the alive check, in order.
    held: VecDeque<Outgoing>,
}

/// The instant a `sleep_until` waits for; it is unused while the branch
/// has no deadline and its precondition is false.
fn until(deadline: Option<tokio::time::Instant>) -> tokio::time::Instant {
    deadline.unwrap_or_else(tokio::time::Instant::now)
}

impl Worker {
    async fn note(&self, message: impl Into<String>) {
        diagnostic(&self.events, self.started, message).await;
    }

    /// Sends `event` to the application; `Break` if it is gone.
    async fn emit(&self, event: Event) -> Flow {
        match self.events.send(event).await {
            Ok(()) => ControlFlow::Continue(()),
            Err(_) => ControlFlow::Break(()),
        }
    }

    async fn emit_all(&self, events: impl IntoIterator<Item = Event>) -> Flow {
        for event in events {
            self.emit(event).await?;
        }
        ControlFlow::Continue(())
    }

    /// Sends `events` after merging them with the peer avatars.
    async fn emit_merged(&mut self, events: Vec<Event>) -> Flow {
        let events = merged(&mut self.peers, events);
        self.emit_all(events).await
    }

    /// Reports a failed link and ends the loop.
    async fn disconnect(&self, detail: String) -> Flow {
        let _ = self.events.send(Event::Disconnected(detail)).await;
        ControlFlow::Break(())
    }

    /// Sends commands the worker made itself; a failed link ends the loop.
    async fn send_commands(&self, commands: Vec<IrcCommand>) -> Flow {
        match send_all(&self.client, &self.transcript, commands).await {
            Ok(()) => ControlFlow::Continue(()),
            Err(detail) => self.disconnect(detail).await,
        }
    }

    async fn finish_history(&self, finished: history::Finished) -> Flow {
        if history_finished(&self.events, self.started, finished).await {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }

    async fn send_next_history(&mut self) -> Flow {
        let sent = request_history(
            &self.client,
            &self.events,
            &self.transcript,
            &mut self.history,
            self.registered,
        )
        .await;
        if sent {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }

    fn waiting_for_registration(&self) -> bool {
        !self.registered && !self.awaiting_nick
    }

    fn metadata_deadline(&self) -> Option<tokio::time::Instant> {
        self.metadata
            .next_deadline()
            .filter(|_| self.metadata_enabled)
    }

    fn echo_deadline(&self) -> Option<tokio::time::Instant> {
        self.echoes.next_deadline()
    }

    fn history_deadline(&self) -> Option<tokio::time::Instant> {
        self.history
            .next_deadline()
            .filter(|_| self.history_enabled)
    }

    fn mode_query_deadline(&self) -> Option<tokio::time::Instant> {
        self.channel_modes
            .next_deadline()
            .filter(|_| self.registered)
    }

    fn peer_deadline(&self) -> Option<tokio::time::Instant> {
        self.peers
            .as_ref()
            .and_then(peer_avatar::PeerAvatars::next_deadline)
            .filter(|_| self.registered)
    }

    async fn on_command(&mut self, command: Option<Outgoing>, stream: &mut ClientStream) -> Flow {
        let Some(command) = command else {
            return ControlFlow::Break(());
        };
        if let Outgoing::CheckAlive(timeout) = command {
            return self.start_alive_check(timeout).await;
        }
        // Whether the link is alive is not known yet: a write could vanish
        // into a dead connection, so it waits for the answer. QUIT does not.
        if self.alive_check.is_some() && !matches!(command, Outgoing::Quit | Outgoing::QuitWith(_))
        {
            if self.held.len() >= MAX_HELD_COMMANDS {
                return self.refuse_held(command).await;
            }
            self.held.push_back(command);
            return ControlFlow::Continue(());
        }
        match command {
            Outgoing::Message {
                target,
                text,
                display_text,
                notice,
                encoder,
            } => {
                self.send_message(target, text, display_text, notice, encoder)
                    .await
            }
            Outgoing::Raw(message) => self.send_raw(message).await,
            Outgoing::Ctcp { nick, tag, args } => self.send_ctcp(&nick, &tag, &args).await,
            Outgoing::OwnAvatar { request, url } => self.set_own_avatar(request, url).await,
            Outgoing::ShareAvatar(url) => {
                if let Some(peers) = self.peers.as_mut() {
                    peers.set_share(url);
                }
                ControlFlow::Continue(())
            }
            Outgoing::QueryAvatar(nickname) => {
                let note = self.peers.as_mut().and_then(|peers| {
                    peers.request(&nickname, &self.current_nick, &self.roster.presence)
                });
                if let Some(note) = note {
                    self.note(note).await;
                }
                ControlFlow::Continue(())
            }
            Outgoing::SetName(realname) => self.set_name(realname).await,
            Outgoing::OlderHistory {
                channel,
                request,
                reference,
                limit,
            } => self.older_history(channel, request, reference, limit).await,
            Outgoing::CheckAlive(_) => ControlFlow::Continue(()),
            outgoing @ (Outgoing::Quit | Outgoing::QuitWith(_)) => {
                let reason = match outgoing {
                    Outgoing::QuitWith(reason) => reason,
                    _ => self.quit_message.clone(),
                };
                self.quit(reason, stream).await
            }
        }
    }

    /// Tells the connection handle how much the server adds in front of the
    /// lines we send, for its length check.
    fn observe_own_mask(&mut self, message: &IrcMessage) {
        if self.own_mask.observe(message, &self.current_nick) {
            self.publish_relay_prefix();
        }
    }

    /// How lines are written on this connection.
    fn wire(&self) -> Wire<'_> {
        Wire {
            label: &self.wire_encoding,
            charsets: Some(&*self.charsets),
        }
    }

    fn publish_relay_prefix(&self) {
        let bytes = self
            .own_mask
            .prefix_bytes(&self.current_nick, &self.wire_encoding);
        relay::publish(&self.relay_prefix, bytes);
    }

    async fn send_message(
        &mut self,
        target: String,
        text: String,
        display_text: String,
        notice: bool,
        encoder: Option<&'static encoding_rs::Encoding>,
    ) -> Flow {
        // With echo-message the local line is confirmed by
        // the server's echo; a label (labeled-response)
        // names it exactly.
        let shown = echo_text(&target, display_text.clone());
        let action = (shown == display_text)
            .then(|| text::action_text(&text).map(str::to_owned))
            .flatten();
        self.next_local_id += 1;
        let local_id = self.next_local_id;
        let label = if self.registered && self.negotiation.enabled(cap::ECHO_MESSAGE) {
            self.echoes.track(
                local_id,
                &target,
                notice,
                &text,
                shown != text,
                self.negotiation.enabled(cap::LABELED_RESPONSE),
            )
        } else {
            None
        };
        let tracked = label.is_some();
        let message = if notice {
            IrcMessage::from(IrcCommand::NOTICE(target.clone(), text))
        } else {
            IrcMessage::from(IrcCommand::PRIVMSG(target.clone(), text))
        };
        let message = echo::with_label(message, label.flatten());
        let line = self.transcript.line(&message);
        // Only the 512 bytes of our own line: the length once the server
        // relays it is checked by the connection handle before queuing.
        let fixed = Fixed(encoder);
        let wire = Wire {
            label: &self.wire_encoding,
            charsets: Some(&fixed),
        };
        // The codec writes this line later: it keeps the encoding that was
        // checked, whatever the channel's setting becomes by then.
        let written = irc::proto::IrcCodec::sanitize(message.to_string());
        // The handle accepted the message before `UTF8ONLY` was known.
        let refused = (self.charsets.utf8_only()
            && encoder.is_some_and(|encoding| encoding != encoding_rs::UTF_8))
        .then(|| {
            "the server requires UTF-8 (UTF8ONLY); the message was accepted in another character encoding"
                .to_owned()
        });
        let result = refused
            .map_or(Ok(()), Err)
            .and_then(|()| validate_tagged_wire(&message, wire))
            .and_then(|_| {
                self.charsets.pin(&written, encoder);
                self.client.send(message).map_err(|error| {
                    self.charsets.unpin(&written);
                    error.to_string()
                })
            });
        let event = match result {
            Ok(()) => {
                self.transcript.record(WireDirection::Sent, line).await;
                Event::OutgoingAccepted {
                    local_id: tracked.then_some(local_id),
                    channel: target,
                    text: shown,
                    notice,
                    action,
                }
            }
            Err(error) => {
                self.echoes.untrack(local_id);
                Event::ServerLine(format!("Send failed: {error}"))
            }
        };
        self.emit(event).await
    }

    async fn send_raw(&mut self, message: IrcMessage) -> Flow {
        let line = self.transcript.line(&message);
        let retry_nick = match &message.command {
            IrcCommand::NICK(nickname) if !self.registered => Some(nickname.clone()),
            _ => None,
        };
        let result = validate_wire(&message.to_string(), self.wire())
            .and_then(|_| self.client.send(message).map_err(|error| error.to_string()));
        match result {
            Ok(()) => {
                self.transcript.record(WireDirection::Sent, line).await;
                if let Some(nickname) = retry_nick {
                    self.current_nick = nickname;
                    self.publish_relay_prefix();
                    self.awaiting_nick = false;
                    self.refusal = None;
                    self.registration_deadline = tokio::time::Instant::now() + REGISTRATION_TIMEOUT;
                }
                ControlFlow::Continue(())
            }
            Err(error) => {
                self.emit(Event::ServerLine(format!("Command failed: {error}")))
                    .await
            }
        }
    }

    async fn send_ctcp(&mut self, nick: &str, tag: &str, args: &str) -> Flow {
        if !self.registered {
            return self
                .emit(Event::ServerLine(format!(
                    "CTCP {tag} request to {nick} not sent: not connected yet"
                )))
                .await;
        }
        let handled = self
            .ctcp
            .request(nick, tag, args, tokio::time::Instant::now());
        self.send_commands(handled.send).await?;
        self.emit_all(handled.events).await
    }

    async fn on_ctcp_deadline(&mut self) -> Flow {
        let handled = self.ctcp.tick(tokio::time::Instant::now());
        self.emit_all(handled.events).await
    }

    /// Asks the server for a `PONG` (any line from it counts as the answer).
    /// Ignored before registration and while a check is already running.
    async fn start_alive_check(&mut self, timeout: Duration) -> Flow {
        if !self.registered || self.alive_check.is_some() {
            return ControlFlow::Continue(());
        }
        self.alive_checks += 1;
        let token = format!("cayenchat-wake-{}", self.alive_checks);
        self.send_commands(vec![IrcCommand::PING(token, None)])
            .await?;
        self.alive_check = Some(tokio::time::Instant::now() + timeout);
        self.note("Checking that the server still answers.").await;
        ControlFlow::Continue(())
    }

    /// Ends a running alive check because the server sent a line.
    async fn alive_check_answered(&mut self) {
        if self.alive_check.take().is_some() {
            self.note("The server answered the connection check.").await;
        }
    }

    async fn on_alive_deadline(&mut self) {
        self.alive_check = None;
        self.report_held().await;
        self.note("The server did not answer the connection check.")
            .await;
        let _ = self
            .events
            .send(Event::Disconnected(ALIVE_CHECK_FAILED.into()))
            .await;
    }

    /// Refuses a command that cannot be held. A request the application
    /// already accepted still gets the outcome event it waits for.
    async fn refuse_held(&mut self, command: Outgoing) -> Flow {
        match command {
            Outgoing::OwnAvatar { request, .. } => {
                self.emit(Event::OwnAvatarFailed {
                    request,
                    failure: AvatarRequestFailure::Busy,
                })
                .await
            }
            Outgoing::OlderHistory {
                channel, request, ..
            } => {
                self.finish_history(history::Finished::failed_older(channel, request))
                    .await
            }
            _ => {
                self.emit(Event::ServerLine(
                    "Command not sent: the connection is being checked.".into(),
                ))
                .await
            }
        }
    }

    /// Tells the application that held commands were dropped with the link.
    async fn report_held(&mut self) {
        let count = std::mem::take(&mut self.held).len();
        if count > 0 {
            let _ = self
                .events
                .send(Event::ServerLine(format!(
                    "{count} command(s) not sent: the connection was lost."
                )))
                .await;
        }
    }

    /// Handles the commands held during a check that the server answered.
    async fn flush_held(&mut self, stream: &mut ClientStream) -> Flow {
        while let Some(command) = self.held.pop_front() {
            self.on_command(Some(command), stream).await?;
        }
        ControlFlow::Continue(())
    }

    async fn set_own_avatar(&mut self, request: u64, url: Option<String>) -> Flow {
        let queued = self.metadata.request_own(
            request,
            url.as_deref(),
            self.registered && self.metadata_enabled,
            tokio::time::Instant::now(),
        );
        match queued {
            Ok(command) => {
                let message = IrcMessage::from(command);
                let line = self.transcript.line(&message);
                if let Err(error) = self.client.send(message) {
                    return self.disconnect(error_chain(&error)).await;
                }
                self.transcript.record(WireDirection::Sent, line).await;
                ControlFlow::Continue(())
            }
            Err(failure) => self.emit(Event::OwnAvatarFailed { request, failure }).await,
        }
    }

    async fn set_name(&mut self, realname: String) -> Flow {
        let failure = if !self.registered || !self.negotiation.enabled(cap::SETNAME) {
            Some(RealNameFailure::Unsupported)
        } else if self.pending_setname >= MAX_PENDING_SETNAME {
            Some(RealNameFailure::Busy)
        } else {
            let message = IrcMessage::from(IrcCommand::Raw(
                "SETNAME".into(),
                vec![peer_avatar::realname(
                    base_realname(&realname),
                    self.advertise_avatar,
                )],
            ));
            let line = self.transcript.line(&message);
            match validate_wire(&message.to_string(), &self.wire_encoding)
                .and_then(|_| self.client.send(message).map_err(|error| error.to_string()))
            {
                Ok(()) => {
                    self.pending_setname += 1;
                    self.transcript.record(WireDirection::Sent, line).await;
                    None
                }
                Err(error) => Some(RealNameFailure::Rejected(error)),
            }
        };
        match failure {
            Some(failure) => self.emit(Event::RealNameFailed(failure)).await,
            None => ControlFlow::Continue(()),
        }
    }

    async fn older_history(
        &mut self,
        channel: String,
        request: u64,
        reference: MessageReference,
        limit: usize,
    ) -> Flow {
        let queued = if self.registered && self.history_enabled {
            self.history
                .enqueue_older(&channel, request, &reference, limit)
        } else {
            Err(history::Finished::failed_older(channel, request))
        };
        match queued {
            Ok(()) => self.send_next_history().await,
            Err(finished) => self.finish_history(finished).await,
        }
    }

    async fn quit(&mut self, reason: String, stream: &mut ClientStream) -> Flow {
        let quit = IrcMessage::from(IrcCommand::QUIT(Some(reason)));
        let line = self.transcript.line(&quit);
        if self.client.send(quit).is_ok() {
            self.transcript.record(WireDirection::Sent, line).await;
        }
        // Polling the stream writes the queue, QUIT included. The server
        // closes after QUIT, which ends the stream; reading (and discarding)
        // up to that point leaves nothing unread, since dropping a socket
        // with unread data sends RST and can discard a QUIT not yet sent.
        // EOF is taken as send completion: the server is assumed to close
        // only after receiving QUIT. A close before that is the server's own
        // doing, and a write after it would be reset, so waiting is futile.
        let _ = tokio::time::timeout(QUIT_FLUSH_LIMIT, async {
            while let Some(Ok(_)) = stream.next().await {}
        })
        .await;
        let _ = self
            .events
            .send(Event::Closed(USER_DISCONNECT.into()))
            .await;
        ControlFlow::Break(())
    }

    async fn on_incoming(
        &mut self,
        incoming: Option<Result<IrcMessage, irc::error::Error>>,
    ) -> Flow {
        match incoming {
            Some(Ok(message)) => {
                self.alive_check_answered().await;
                self.on_irc_message(message).await
            }
            // irc consumes 432/433 and reports NoUsableNick because no
            // alternate nicknames are configured; the link stays usable.
            Some(Err(irc::error::Error::NoUsableNick)) if !self.registered => {
                self.note(format!(
                    "Server rejected nickname {} (432/433); waiting for another nickname.",
                    self.current_nick
                ))
                .await;
                self.awaiting_nick = true;
                self.refusal = Some(format!(
                    "Nickname {} is unavailable (432/433).",
                    self.current_nick
                ));
                let nickname = self.current_nick.clone();
                self.emit(Event::NicknameRejected { nickname }).await
            }
            Some(Err(irc::error::Error::NoUsableNick)) => {
                self.emit(Event::ServerLine(
                    "Nickname change rejected: the nickname is in use or invalid (432/433).".into(),
                ))
                .await
            }
            Some(Err(error)) => {
                self.report_held().await;
                let detail = stream_error_detail(&error);
                self.note(format!("IRC stream failed: {detail}")).await;
                let event = self
                    .refusal
                    .take()
                    .map_or(Event::Disconnected(detail), Event::Refused);
                let _ = self.events.send(event).await;
                ControlFlow::Break(())
            }
            None => {
                self.report_held().await;
                self.note("Server closed the IRC stream.").await;
                let event = self.refusal.take().map_or_else(
                    || Event::Disconnected("Server closed the connection.".into()),
                    Event::Refused,
                );
                let _ = self.events.send(event).await;
                ControlFlow::Break(())
            }
        }
    }

    async fn on_irc_message(&mut self, message: IrcMessage) -> Flow {
        self.record_received(&message).await;
        if requires_utf8(&message) && self.charsets.set_utf8_only() {
            self.note(
                "Server requires UTF-8 (UTF8ONLY): the channels' other character encodings are not used.",
            )
            .await;
        }
        if requires_utf8(&message) {
            let unsent = self.charsets.unsent_for_utf8_only();
            if unsent > self.reported_unsent {
                self.reported_unsent = unsent;
                self.note(format!(
                    "Server requires UTF-8 (UTF8ONLY): {unsent} queued message(s) accepted in another character encoding were not sent."
                ))
                .await;
            }
        }
        if !self.wire_encoding.eq_ignore_ascii_case("UTF-8") && requires_utf8(&message) {
            let _ = self
                .events
                .send(Event::Refused(
                    "Server requires UTF-8 (UTF8ONLY); change this server's character \
                     encoding to UTF-8."
                        .into(),
                ))
                .await;
            return ControlFlow::Break(());
        }
        let history_started = self.negotiate(&message).await?;
        self.track_registration(&message, history_started).await?;
        let replayed = self.replay.replayed(&message);
        let history = self.consume_own_replies(&message).await?;
        // A line replayed from history shows our mask of the past.
        if !history && !self.replay.in_history_batch(&message) {
            self.observe_own_mask(&message);
        }
        if history || self.consume_avatar_and_ctcp(&message, replayed).await? {
            return ControlFlow::Continue(());
        }
        self.deliver(message, replayed).await
    }

    /// Records the line in the transcript, with the replies the library
    /// queued for it, and explains registration progress.
    async fn record_received(&self, message: &IrcMessage) {
        self.transcript
            .record(WireDirection::Received, self.transcript.line(message))
            .await;
        // irc's transport enqueues PONG before exposing PING here.
        if let IrcCommand::PING(data, _) = &message.command {
            let pong = IrcMessage::from(IrcCommand::PONG(data.clone(), None));
            self.transcript
                .record(WireDirection::Sent, self.transcript.line(&pong))
                .await;
        }
        // irc's client state enqueues configured JOINs on end-of-MOTD.
        if matches!(
            message.command,
            IrcCommand::Response(Response::RPL_ENDOFMOTD | Response::ERR_NOMOTD, _)
        ) {
            for channel in &self.auto_join_channels {
                let join = IrcMessage::from(IrcCommand::JOIN(channel.clone(), None, None));
                self.transcript
                    .record(WireDirection::Sent, self.transcript.line(&join))
                    .await;
            }
        }
        if !self.registered {
            match &message.command {
                IrcCommand::Response(response, _) => {
                    self.note(format!("Server replied {response:?}.")).await
                }
                IrcCommand::CAP(_, subcommand, _, _) => {
                    self.note(format!("Server CAP response: {subcommand:?}."))
                        .await
                }
                _ => {}
            }
        }
    }

    /// Feeds the capability negotiation and what depends on the enabled
    /// capabilities. `Continue` says whether chathistory was just enabled.
    async fn negotiate(&mut self, message: &IrcMessage) -> ControlFlow<(), bool> {
        match self.negotiation.observe(message) {
            Ok(step) => {
                for note in step.notes {
                    self.note(note).await;
                }
                self.send_commands(step.send).await?;
            }
            Err(error) => {
                let _ = self.events.send(Event::Refused(error)).await;
                return ControlFlow::Break(());
            }
        }
        // CAP DEL (or ACK -batch) withdrew batch: forget open batches.
        if self.batch_negotiated && !self.negotiation.enabled(cap::BATCH) {
            self.replay.reset();
        }
        self.batch_negotiated = self.negotiation.enabled(cap::BATCH);
        // Metadata stopped (its own DEL, or batch went away):
        // every avatar of this connection becomes unknown.
        if self.metadata_enabled && !self.negotiation.enabled(cap::METADATA) {
            let reset = self
                .metadata
                .reset()
                .into_iter()
                .chain([Event::AvatarsReset])
                .collect();
            self.emit_merged(reset).await?;
        }
        self.metadata_enabled = self.negotiation.enabled(cap::METADATA);
        // chathistory went away (its own DEL, or batch): the
        // request being answered ends without lines.
        if self.history_enabled && !self.negotiation.enabled(cap::CHATHISTORY) {
            for finished in self.history.reset() {
                self.finish_history(finished).await?;
            }
        }
        let history_started = !self.history_enabled && self.negotiation.enabled(cap::CHATHISTORY);
        self.history_enabled = self.negotiation.enabled(cap::CHATHISTORY);
        self.history.isupport(message);
        modes::observe(message, &self.modes_per_line);
        self.channel_modes.isupport(message);
        self.accounts.isupport(message);
        if let Some(announced) = advertised_casemapping(message)
            && announced != self.casemapping
        {
            self.casemapping = announced;
            self.charsets.set_casemapping(announced);
            self.metadata.set_casemapping(announced);
            self.roster.presence.set_casemapping(announced);
            self.history.set_casemapping(announced);
            self.accounts.set_casemapping(announced);
            self.ctcp.set_casemapping(announced);
            self.channel_modes.set_casemapping(announced);
            self.emit(Event::CaseMapping(announced)).await?;
        }
        ControlFlow::Continue(history_started)
    }

    /// Registration, and the work that follows from it: history, metadata
    /// subscription.
    async fn track_registration(&mut self, message: &IrcMessage, history_started: bool) -> Flow {
        if !self.registered
            && let Some(reason) = registration_refusal(message)
        {
            self.refusal = Some(reason);
        }
        if matches!(
            message.command,
            IrcCommand::Response(Response::RPL_WELCOME, _)
        ) {
            self.registered = true;
            self.transcript.registered = true;
            self.negotiation.registered();
            self.note("Registration completed (001 received).").await;
        }
        // Reported ahead of the 001's own Registered event;
        // the application forgets it only on disconnect.
        if self.history_available != (self.registered && self.history_enabled) {
            self.history_available = self.registered && self.history_enabled;
            self.emit(Event::HistoryAvailable(self.history_available))
                .await?;
            // Once per connection: which conversations had
            // messages while we were away. Their history
            // follows through the same queue.
            if self.history_available && !self.targets_asked {
                self.targets_asked = true;
                let now = SystemTime::now();
                let since = self.targets_since.unwrap_or(now - TARGETS_DEFAULT_WINDOW);
                self.history
                    .enqueue_targets(since.max(now - TARGETS_MAX_WINDOW), now);
                self.send_next_history().await?;
            }
        }
        // Subscribe once registered (the draft allows it earlier
        // only with `before-connect`), also after a later NEW.
        let start = if self.registered && self.metadata_enabled {
            self.metadata.start(tokio::time::Instant::now())
        } else {
            Vec::new()
        };
        if !start.is_empty() {
            self.send_commands(start).await?;
            self.emit(Event::MetadataReady).await?;
        }
        // Enabled after registration (CAP NEW): the channels
        // already joined get their history too.
        if history_started && self.registered {
            for channel in self.client.list_channels().unwrap_or_default() {
                if let Some(note) = self.history.enqueue(&channel) {
                    self.note(note).await;
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// Replies to our own requests (history, SETNAME). `Continue(true)`
    /// means the message is not live traffic and goes no further.
    async fn consume_own_replies(&mut self, message: &IrcMessage) -> ControlFlow<(), bool> {
        // Our history replies are consumed whole; nothing in
        // them is live traffic.
        if self.history_enabled {
            match self.history.observe(message) {
                history::Observed::Unrelated => {}
                history::Observed::Consumed => return ControlFlow::Continue(true),
                history::Observed::Finished(finished) => {
                    self.finish_history(finished).await?;
                    self.send_next_history().await?;
                    return ControlFlow::Continue(true);
                }
            }
        }
        // The server withdrew setname: waiting requests end.
        if self.pending_setname > 0 && !self.negotiation.enabled(cap::SETNAME) {
            self.pending_setname = 0;
            self.emit(Event::RealNameFailed(RealNameFailure::Unsupported))
                .await?;
        }
        // SETNAME is never chat: ours is reported, others' are
        // not tracked.
        match &message.command {
            IrcCommand::Raw(verb, args) if verb == "SETNAME" => {
                if message.source_nickname() == Some(self.current_nick.as_str()) {
                    self.pending_setname = self.pending_setname.saturating_sub(1);
                    let realname = args.last().map(String::as_str).unwrap_or_default();
                    let event = Event::RealNameChanged {
                        realname: peer_avatar::without_mark(realname).to_owned(),
                    };
                    self.emit(event).await?;
                }
                ControlFlow::Continue(true)
            }
            IrcCommand::Raw(verb, args)
                if verb == "FAIL" && args.first().is_some_and(|c| c == "SETNAME") =>
            {
                self.pending_setname = self.pending_setname.saturating_sub(1);
                let detail = args
                    .last()
                    .filter(|_| args.len() > 1)
                    .cloned()
                    .unwrap_or_default();
                self.emit(Event::RealNameFailed(RealNameFailure::Rejected(detail)))
                    .await?;
                ControlFlow::Continue(true)
            }
            _ => ControlFlow::Continue(false),
        }
    }

    /// Avatar metadata, CTCP AVATAR and other CTCP, which are not chat.
    /// `Continue(true)` means the message goes no further.
    async fn consume_avatar_and_ctcp(
        &mut self,
        message: &IrcMessage,
        replayed: bool,
    ) -> ControlFlow<(), bool> {
        if self.metadata_enabled {
            let handled = self.metadata.observe(
                message,
                tokio::time::Instant::now(),
                &self.current_nick,
                |channel| is_joined(&self.client, self.casemapping, channel),
            );
            if let Some(handled) = handled {
                for note in handled.notes {
                    self.note(note).await;
                }
                self.emit_merged(handled.events).await?;
                return ControlFlow::Continue(true);
            }
        }
        // CTCP AVATAR and replies to our own lookups are
        // not chat and not server lines.
        if let Some(peers) = self.peers.as_mut()
            && let Some(handled) = peers.observe(
                message,
                &self.current_nick,
                &self.roster.presence,
                replayed,
                tokio::time::Instant::now(),
            )
        {
            self.send_commands(handled.send).await?;
            for note in handled.notes {
                self.note(note).await;
            }
            self.emit_all(handled.events).await?;
            return ControlFlow::Continue(true);
        }
        // Other CTCP requests and replies become one
        // readable server line, answered when asked.
        if let Some(handled) = self.ctcp.observe(
            message,
            &self.current_nick,
            replayed,
            tokio::time::Instant::now(),
        ) {
            self.send_commands(handled.send).await?;
            self.emit_all(handled.events).await?;
            return ControlFlow::Continue(true);
        }
        ControlFlow::Continue(false)
    }

    /// Turns the message into events for the application.
    async fn deliver(&mut self, message: IrcMessage, replayed: bool) -> Flow {
        // Peer avatars end before metadata's events for the
        // same message are merged, so a departed user's
        // peer avatar cannot stand in for their metadata.
        let mut lifecycle = self
            .peers
            .as_mut()
            .map(|peers| peers.lifecycle(&message, &self.roster.presence, &self.current_nick))
            .unwrap_or_default();
        let metadata_events = if self.metadata_enabled {
            if let IrcCommand::PART(channel, _) | IrcCommand::KICK(channel, _, _) = &message.command
            {
                self.metadata.forget_channel(channel);
            }
            let handled = self.metadata.lifecycle(
                &message,
                &self.roster.presence,
                &self.current_nick,
                replayed,
                tokio::time::Instant::now(),
            );
            for note in handled.notes {
                self.note(note).await;
            }
            handled.events
        } else {
            Vec::new()
        };
        lifecycle.extend(merged(&mut self.peers, metadata_events));
        let whois_reply = self.whois.observe(&message);
        self.track_own_membership(&message).await?;
        let mode_events = self.track_channel_modes(&message).await;
        // Echoes, ACKs and errors of messages we sent.
        if self.observe_echoes(&message).await? {
            return ControlFlow::Continue(());
        }
        // Our WHOX reply is not a server line.
        if self.track_accounts
            && let Some(reply) = self.accounts.observe_who(&message)
        {
            self.emit_all(reply).await?;
            if let Some(command) = self.accounts.next_who(tokio::time::Instant::now()) {
                self.send_commands(vec![command]).await?;
            }
            return ControlFlow::Continue(());
        }
        let mut account_events = if self.track_accounts {
            self.accounts
                .observe(&message, &self.current_nick, &self.roster.presence)
        } else {
            Vec::new()
        };
        let server_time = tags::server_time(&message, self.negotiation.enabled(cap::SERVER_TIME));
        let translated = translate_message(
            &self.client,
            &mut self.roster,
            &mut self.replay,
            &self.current_nick,
            message,
            server_time,
            self.batch_negotiated,
        );
        for event in translated
            .into_iter()
            .chain(whois_reply.map(|info| Event::Whois(Box::new(info))))
        {
            match &event {
                Event::Registered { nickname } | Event::NickChanged { nickname } => {
                    self.current_nick = nickname.clone();
                    self.publish_relay_prefix();
                }
                // Someone speaking live may be asked about
                // their avatar; nobody else is.
                Event::ChannelMessage {
                    sender,
                    replayed: false,
                    ..
                }
                | Event::PrivateMessage {
                    sender,
                    replayed: false,
                    ..
                } => {
                    let note = self.peers.as_mut().and_then(|peers| {
                        peers.speaker(sender, &self.current_nick, &self.roster.presence)
                    });
                    if let Some(note) = note {
                        self.note(note).await;
                    }
                }
                _ => {}
            }
            if self.track_accounts {
                match &event {
                    Event::Names { channel, users } => {
                        account_events.extend(self.accounts.names(channel, users))
                    }
                    Event::Joined { channel } => self.accounts.joined(channel),
                    _ => {}
                }
            }
            self.emit(event).await?;
        }
        self.emit_all(account_events).await?;
        self.emit_all(mode_events).await?;
        self.send_mode_queries().await?;
        if self.track_accounts
            && let Some(command) = self.accounts.next_who(tokio::time::Instant::now())
        {
            self.send_commands(vec![command]).await?;
        }
        self.emit_all(lifecycle).await?;
        // After the JOIN's own events, so the channel exists
        // in the application before its history is asked for.
        if self.history_enabled {
            self.send_next_history().await?;
        }
        ControlFlow::Continue(())
    }

    /// Asks for the modes of own joins (also after a reconnect), a few
    /// unanswered at a time so a join burst is paced by the replies and
    /// timeouts, not by incoming traffic; until the reply arrives the modes
    /// are unknown. Raw: the typed command would leave a trailing space.
    async fn send_mode_queries(&mut self) -> Flow {
        let queries = self
            .channel_modes
            .take_queries(tokio::time::Instant::now())
            .into_iter()
            .map(|channel| IrcCommand::Raw("MODE".into(), vec![channel]))
            .collect();
        self.send_commands(queries).await
    }

    async fn on_mode_query_deadline(&mut self) -> Flow {
        self.send_mode_queries().await
    }

    /// The flags of a joined channel after `message`. Leaving or joining
    /// ourselves discards what was known.
    async fn track_channel_modes(&mut self, message: &IrcMessage) -> Vec<Event> {
        let own = message.source_nickname() == Some(self.current_nick.as_str());
        match &message.command {
            IrcCommand::JOIN(channel, _, _) if own => {
                if let Some(note) = self.channel_modes.joined(channel, self.casemapping) {
                    self.note(note).await;
                }
            }
            IrcCommand::PART(channel, _) if own => {
                self.channel_modes.left(channel, self.casemapping);
            }
            IrcCommand::KICK(channel, nickname, _) if nickname == &self.current_nick => {
                self.channel_modes.left(channel, self.casemapping);
            }
            _ => {}
        }
        self.channel_modes
            .observe(
                message,
                self.casemapping,
                self.replay.in_history_batch(message),
            )
            .map(|(channel, flags)| Event::ChannelModes { channel, flags })
            .into_iter()
            .collect()
    }

    /// Our own joins, parts and kicks decide which channels have history.
    async fn track_own_membership(&mut self, message: &IrcMessage) -> Flow {
        if self.history_enabled && message.source_nickname() == Some(self.current_nick.as_str()) {
            match &message.command {
                IrcCommand::JOIN(channel, _, _) => {
                    if let Some(note) = self.history.enqueue(channel) {
                        self.note(note).await;
                    }
                }
                IrcCommand::PART(channel, _) => {
                    for finished in self.history.forget(channel) {
                        self.finish_history(finished).await?;
                    }
                }
                _ => {}
            }
        }
        if let IrcCommand::KICK(channel, nickname, _) = &message.command
            && nickname == &self.current_nick
        {
            for finished in self.history.forget(channel) {
                self.finish_history(finished).await?;
            }
        }
        ControlFlow::Continue(())
    }

    /// `Continue(true)` means the server's echo or ACK consumed the message.
    async fn observe_echoes(&mut self, message: &IrcMessage) -> ControlFlow<(), bool> {
        let mut echo_events = self.echoes.expire(tokio::time::Instant::now());
        if !self.negotiation.enabled(cap::ECHO_MESSAGE) && !self.echoes.outstanding().is_empty() {
            echo_events.extend(self.echoes.reset());
        }
        let echoed = self.echoes.observe(
            message,
            &self.current_nick,
            self.negotiation.enabled(cap::LABELED_RESPONSE),
        );
        let consumed = echoed.is_some();
        echo_events.extend(echoed.unwrap_or_default());
        self.emit_all(echo_events).await?;
        ControlFlow::Continue(consumed)
    }

    async fn on_metadata_deadline(&mut self) -> Flow {
        let handled = self.metadata.tick(tokio::time::Instant::now(), |channel| {
            is_joined(&self.client, self.casemapping, channel)
        });
        for command in handled.send {
            let message = IrcMessage::from(command);
            let line = self.transcript.line(&message);
            if self.client.send(message).is_ok() {
                self.transcript.record(WireDirection::Sent, line).await;
            }
        }
        for note in handled.notes {
            self.note(note).await;
        }
        self.emit_merged(handled.events).await
    }

    async fn on_echo_deadline(&mut self) -> Flow {
        let expired = self.echoes.expire(tokio::time::Instant::now());
        self.emit_all(expired).await
    }

    async fn on_history_deadline(&mut self) -> Flow {
        if let Some(history::Observed::Finished(finished)) =
            self.history.tick(tokio::time::Instant::now())
        {
            self.finish_history(finished).await?;
            self.send_next_history().await?;
        }
        ControlFlow::Continue(())
    }

    async fn on_peer_deadline(&mut self) -> Flow {
        let handled = self
            .peers
            .as_mut()
            .map(|peers| {
                peers.tick(
                    tokio::time::Instant::now(),
                    &self.current_nick,
                    &self.roster.presence,
                )
            })
            .unwrap_or_default();
        self.send_commands(handled.send).await
    }

    async fn on_registration_timeout(&mut self) {
        self.note("IRC registration timed out.").await;
        let event = self.refusal.take().map_or_else(
            || Event::Disconnected("Registration timed out.".into()),
            Event::Refused,
        );
        let _ = self.events.send(event).await;
    }
}

/// Sends the next queued history request, if any may go out now, and
/// reports it. `false` means the connection or the event channel failed.
async fn request_history(
    client: &Client,
    events: &EventSender,
    transcript: &Transcript,
    history: &mut history::HistoryRequests,
    registered: bool,
) -> bool {
    if !registered {
        return true;
    }
    let Some((channel, page, command)) = history.next_request(tokio::time::Instant::now()) else {
        return true;
    };
    if let Err(detail) = send_all(client, transcript, vec![command]).await {
        let _ = events.send(Event::Disconnected(detail)).await;
        return false;
    }
    // Only recent history needs its place reserved; an older page goes
    // before everything the conversation holds.
    let resumed = match page {
        history::Page::Latest => false,
        history::Page::Resume => true,
        history::Page::Before { .. } | history::Page::Targets { .. } => return true,
    };
    events
        .send(Event::HistoryRequested { channel, resumed })
        .await
        .is_ok()
}

/// Reports a finished history request. `false` means the event channel
/// closed.
async fn history_finished(
    events: &EventSender,
    started: Instant,
    finished: history::Finished,
) -> bool {
    if let Some(note) = &finished.note {
        diagnostic(events, started, note.clone()).await;
    }
    let status = finished.older_status();
    let event = match finished.page {
        history::Page::Latest | history::Page::Resume => Event::ChannelHistory {
            channel: finished.channel,
            messages: finished.messages,
            incomplete: finished.incomplete,
        },
        history::Page::Before { request } => Event::OlderChannelHistory {
            channel: finished.channel,
            request,
            messages: finished.messages,
            status,
        },
        // Its peers were queued when the reply ended; nothing to report.
        history::Page::Targets { .. } => return true,
    };
    events.send(event).await.is_ok()
}

/// Passes metadata's avatar events through the peer avatar merge when
/// CTCP AVATAR exchange is on.
fn merged(peers: &mut Option<peer_avatar::PeerAvatars>, events: Vec<Event>) -> Vec<Event> {
    match peers {
        Some(peers) => peers.merge_metadata(events),
        None => events,
    }
}

/// Sends commands the worker made itself and records them in the
/// transcript; `Err` means the link failed.
async fn send_all(
    client: &Client,
    transcript: &Transcript,
    commands: Vec<IrcCommand>,
) -> Result<(), String> {
    for command in commands {
        let message = IrcMessage::from(command);
        let line = transcript.line(&message);
        client.send(message).map_err(|error| error_chain(&error))?;
        transcript.record(WireDirection::Sent, line).await;
    }
    Ok(())
}

/// Registration replies after which the server closes the link and an
/// identical reconnect would be refused again.
fn registration_refusal(message: &IrcMessage) -> Option<String> {
    match &message.command {
        IrcCommand::Response(Response::ERR_PASSWDMISMATCH, _) => {
            Some("Server rejected the password (464).".into())
        }
        IrcCommand::Response(Response::ERR_YOUREBANNEDCREEP, _) => {
            Some("Server refused the connection because this client is banned (465).".into())
        }
        _ => None,
    }
}

fn display_nickname(member: &str) -> &str {
    member.trim_start_matches(['~', '&', '@', '%', '+'])
}

#[derive(Default)]
struct RosterTracker {
    /// The members with a role prefix (`@nick`) in each channel's last
    /// published roster, so a NICK change can carry the role over. Members
    /// without a role are not kept: large channels republish on every
    /// change, and most of their members have none.
    ranked: HashMap<String, Vec<String>>,
    presence: presence::PresenceIndex,
    renamed_roles: HashMap<(String, String), char>,
}

impl RosterTracker {
    fn rename(&mut self, channels: &[String], old_nick: &str, new_nick: &str) {
        for channel in channels {
            let old_key = (channel.clone(), old_nick.to_lowercase());
            let role = self.renamed_roles.remove(&old_key).or_else(|| {
                self.ranked.get(channel).and_then(|members| {
                    members
                        .iter()
                        .find(|member| display_nickname(member).eq_ignore_ascii_case(old_nick))
                        .and_then(|member| member.chars().next())
                        .filter(|prefix| "~&@%+".contains(*prefix))
                })
            });
            if let Some(role) = role {
                self.renamed_roles
                    .insert((channel.clone(), new_nick.to_lowercase()), role);
            }
        }
    }

    fn clear_mode_targets(&mut self, channel: &str, modes: &[Mode<irc::proto::mode::ChannelMode>]) {
        for mode in modes {
            if let Mode::Plus(_, Some(nickname)) | Mode::Minus(_, Some(nickname)) = mode {
                self.renamed_roles
                    .remove(&(channel.to_owned(), nickname.to_lowercase()));
            }
        }
    }

    /// Uses the snapshot published before the current message, because the
    /// library has already removed a quitting user from its own roster.
    fn had_member(&self, channel: &str, nickname: &str) -> bool {
        self.presence.in_channel(channel, nickname)
    }

    fn forget_channel(&mut self, channel: &str) {
        self.ranked.remove(channel);
        self.presence.remove_channel(channel);
        self.renamed_roles
            .retain(|(known_channel, _), _| known_channel != channel);
    }

    fn accept_names(&mut self, channel: &str) {
        self.renamed_roles
            .retain(|(known_channel, _), _| known_channel != channel);
    }
}

/// Collects WHOIS numerics per nickname until end-of-WHOIS.
#[derive(Default)]
struct WhoisCollector {
    pending: HashMap<String, WhoisInfo>,
}

impl WhoisCollector {
    fn observe(&mut self, message: &IrcMessage) -> Option<WhoisInfo> {
        let (code, args) = match &message.command {
            IrcCommand::Response(response, args) => (*response as u16, args),
            IrcCommand::Raw(command, args) => (command.parse::<u16>().ok()?, args),
            _ => return None,
        };
        let nickname = args.get(1)?;
        let key = nickname.to_lowercase();
        let arg = |index: usize| args.get(index).filter(|value| !value.is_empty()).cloned();
        if code == 318 {
            return Some(self.pending.remove(&key).unwrap_or_else(|| WhoisInfo {
                nickname: nickname.clone(),
                ..Default::default()
            }));
        }
        // RPL_AWAY also answers PRIVMSG, so it only joins a WHOIS in progress.
        if code == 301 {
            if let Some(info) = self.pending.get_mut(&key) {
                info.away = args.last().cloned();
            }
            return None;
        }
        if !matches!(
            code,
            276 | 307 | 311..=313 | 317 | 319 | 320 | 330 | 335 | 338 | 378 | 379 | 671
        ) {
            return None;
        }
        if !self.pending.contains_key(&key) && self.pending.len() >= MAX_PENDING_WHOIS {
            return None;
        }
        let info = self.pending.entry(key).or_insert_with(|| WhoisInfo {
            nickname: nickname.clone(),
            ..Default::default()
        });
        match code {
            311 => {
                info.nickname = nickname.clone();
                info.username = arg(2);
                info.host = arg(3);
                let real = if args.len() > 5 { args.last() } else { None };
                info.avatar_mark = real.is_some_and(|real| peer_avatar::advertises_avatar(real));
                info.realname = real.map(|real| peer_avatar::without_mark(real).to_owned());
            }
            312 => {
                info.server = arg(2);
                info.server_info = arg(3);
            }
            313 => info.operator = args.last().cloned(),
            317 => {
                info.idle_seconds = args.get(2).and_then(|value| value.parse().ok());
                info.signon = args.get(3).and_then(|value| value.parse().ok());
            }
            319 => {
                if let Some(list) = args.last() {
                    let room = MAX_WHOIS_ITEMS.saturating_sub(info.channels.len());
                    info.channels
                        .extend(list.split_whitespace().take(room).map(str::to_owned));
                }
            }
            330 => info.account = arg(2),
            _ if info.extra.len() < MAX_WHOIS_ITEMS => info.extra.push(args[2..].join(" ")),
            _ => {}
        }
        None
    }
}

/// The channel topic a server line states or changes, if it is one.
fn topic_event(message: &IrcMessage, server_time: Option<SystemTime>) -> Option<Event> {
    let (channel, topic) = match &message.command {
        IrcCommand::Response(Response::RPL_TOPIC, args) => {
            (args.get(1)?, args.get(2).cloned().unwrap_or_default())
        }
        IrcCommand::Response(Response::RPL_NOTOPIC, args) => (args.get(1)?, String::new()),
        IrcCommand::TOPIC(channel, topic) => (channel, topic.clone().unwrap_or_default()),
        _ => return None,
    };
    let changed_by = match &message.command {
        IrcCommand::TOPIC(..) => message.source_nickname().map(str::to_owned),
        _ => None,
    };
    valid_channel(channel).then(|| Event::Topic {
        channel: channel.clone(),
        topic,
        changed_by,
        server_time,
    })
}

fn names_snapshot(client: &Client, roster: &mut RosterTracker, channel: &str) -> Event {
    let tracked = client.list_users(channel);
    let known = tracked.is_some();
    let mut users: Vec<String> = tracked
        .unwrap_or_default()
        .iter()
        // Servers may pad NAMES with extra spaces, which the client library
        // splits into users without a nickname.
        .filter(|user| !user.get_nickname().is_empty())
        .map(|user| {
            let prefix = match user.highest_access_level() {
                AccessLevel::Owner => "~",
                AccessLevel::Admin => "&",
                AccessLevel::Oper => "@",
                AccessLevel::HalfOp => "%",
                AccessLevel::Voice => "+",
                AccessLevel::Member => "",
            };
            format!("{prefix}{}", user.get_nickname())
        })
        .collect();
    // The library appends every NAMES reply to the channel's list, so a
    // repeated reply (e.g. /names) lists everyone twice. Keep the last entry
    // per nickname: it comes from the newest reply. (Our own KICK no longer
    // leaves a stale list behind: the vendored `irc` drops it, #342.)
    let mut seen = HashSet::new();
    users.reverse();
    users.retain(|user| seen.insert(display_nickname(user).to_lowercase()));
    users.reverse();
    let has_renamed_roles = roster
        .renamed_roles
        .keys()
        .any(|(known_channel, _)| known_channel == channel);
    if has_renamed_roles {
        for user in &mut users {
            let nickname = display_nickname(user).to_owned();
            if let Some(prefix) = roster
                .renamed_roles
                .get(&(channel.to_owned(), nickname.to_lowercase()))
            {
                *user = format!("{prefix}{nickname}");
            }
        }
    }
    roster.renamed_roles.retain(|(known_channel, nickname), _| {
        known_channel != channel
            || users
                .iter()
                .any(|user| display_nickname(user).eq_ignore_ascii_case(nickname))
    });
    // Only channels the library tracks as joined are remembered, so arbitrary
    // end-of-NAMES replies cannot grow the roster cache.
    if known {
        roster.presence.replace_channel(channel, &users);
        let ranked = users
            .iter()
            .filter(|user| display_nickname(user).len() < user.len());
        roster
            .ranked
            .insert(channel.to_owned(), ranked.cloned().collect());
    }
    Event::Names {
        channel: channel.to_owned(),
        users,
    }
}

/// Translates a user's PRIVMSG or NOTICE addressed to `current_nick`.
fn private_message(
    message: &IrcMessage,
    current_nick: &str,
    replayed: bool,
    server_time: Option<SystemTime>,
) -> Option<Event> {
    let (target, text, notice) = match &message.command {
        IrcCommand::PRIVMSG(target, text) => (target, text, false),
        IrcCommand::NOTICE(target, text) => (target, text, true),
        _ => return None,
    };
    // Servers and services without a user mask (`:irc.example NOTICE me`)
    // are not conversations.
    let Some(Prefix::Nickname(sender, _, _)) = &message.prefix else {
        return None;
    };
    let ctcp = text.starts_with('\u{1}');
    if ctcp && !text.starts_with("\u{1}ACTION ") {
        return None;
    }
    // Our own line to someone else, relayed by a bouncer.
    if crate::text::same_nickname(sender, current_nick)
        && !crate::text::same_nickname(target, current_nick)
        && valid_nickname(target)
    {
        return Some(Event::OwnPrivateMessage {
            target: target.clone(),
            // Another client's IDENTIFY, or its playback, is hidden too.
            text: echo_text(target, text.clone()),
            notice,
            server_time,
            msgid: tags::msgid(message).map(str::to_owned),
            replayed,
        });
    }
    if !crate::text::same_nickname(target, current_nick) {
        return None;
    }
    Some(Event::PrivateMessage {
        sender: sender.clone(),
        text: text.clone(),
        notice,
        server_time,
        msgid: tags::msgid(message).map(str::to_owned),
        account: tags::account(message).map(str::to_owned),
        replayed,
    })
}

fn translate_message(
    client: &Client,
    roster: &mut RosterTracker,
    replay: &mut replay::ReplayTracker,
    current_nick: &str,
    message: IrcMessage,
    server_time: Option<SystemTime>,
    batch_negotiated: bool,
) -> Vec<Event> {
    // TAGMSG carries only tags. None is shown yet: no chat row, unread
    // mark, notification or preview; the transcript still records it.
    replay.observe(&message);
    if matches!(&message.command, IrcCommand::Raw(verb, _) if verb.eq_ignore_ascii_case("TAGMSG")) {
        return Vec::new();
    }
    // Negotiated batch framing is not chat either; it stays in the
    // transcript. Unsolicited BATCH lines keep appearing in the server log
    // as they always did.
    if batch_negotiated && matches!(message.command, IrcCommand::BATCH(..)) {
        return Vec::new();
    }
    let actor = message.source_nickname().map(str::to_owned);
    let activity = match &message.command {
        IrcCommand::JOIN(channel, _, _) => actor.as_ref().map(|actor| {
            let mask = message.prefix.as_ref().and_then(|prefix| {
                let prefix = prefix.to_string();
                prefix
                    .strip_prefix(actor)
                    .and_then(|suffix| suffix.strip_prefix('!'))
                    .filter(|mask| !mask.is_empty())
                    .map(str::to_owned)
            });
            Event::ChannelActivity {
                channel: channel.clone(),
                actor: actor.clone(),
                kind: ChannelActivityKind::Joined { mask },
                server_time,
            }
        }),
        IrcCommand::PART(channel, reason) => actor.as_ref().map(|actor| Event::ChannelActivity {
            channel: channel.clone(),
            actor: actor.clone(),
            kind: ChannelActivityKind::Left {
                reason: reason.clone(),
            },
            server_time,
        }),
        IrcCommand::KICK(channel, target, reason) => Some(Event::ChannelActivity {
            channel: channel.clone(),
            actor: actor.clone().unwrap_or_else(|| "server".into()),
            kind: ChannelActivityKind::Kicked {
                target: target.clone(),
                reason: reason.clone(),
                own: target == current_nick,
            },
            server_time,
        }),
        IrcCommand::Response(
            response @ (Response::ERR_CHANNELISFULL
            | Response::ERR_INVITEONLYCHAN
            | Response::ERR_BANNEDFROMCHAN
            | Response::ERR_BADCHANNELKEY),
            args,
        ) => match args.as_slice() {
            [_, channel, reason, ..] if valid_channel(channel) => Some(Event::ChannelActivity {
                channel: channel.clone(),
                actor: "server".into(),
                kind: ChannelActivityKind::JoinFailed {
                    banned: *response == Response::ERR_BANNEDFROMCHAN,
                    bad_key: *response == Response::ERR_BADCHANNELKEY,
                    reason: reason.clone(),
                },
                server_time,
            }),
            _ => None,
        },
        IrcCommand::ChannelMODE(channel, modes) => Some(Event::ChannelActivity {
            channel: channel.clone(),
            actor: actor.clone().unwrap_or_else(|| "server".into()),
            kind: ChannelActivityKind::ModeChanged {
                modes: redact_key_modes(modes)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                privileges: privilege_changes(modes),
                // Only a history batch: a server's own live line has no
                // user mask but is not a replay.
                replayed: replay.in_history_batch(&message),
            },
            server_time,
        }),
        _ => None,
    };
    let changed_channels = match &message.command {
        IrcCommand::JOIN(channel, _, _) | IrcCommand::ChannelMODE(channel, _) => {
            vec![channel.clone()]
        }
        IrcCommand::PART(channel, _) if message.source_nickname() != Some(current_nick) => {
            vec![channel.clone()]
        }
        IrcCommand::KICK(channel, nickname, _) if nickname != current_nick => {
            vec![channel.clone()]
        }
        // Only rosters that contained the user change; republishing every
        // joined channel on each QUIT or NICK made netsplits costly.
        IrcCommand::QUIT(_) | IrcCommand::NICK(_) => {
            let nickname = message.source_nickname().unwrap_or("");
            client
                .list_channels()
                .unwrap_or_default()
                .into_iter()
                .filter(|channel| roster.had_member(channel, nickname))
                .collect()
        }
        _ => Vec::new(),
    };
    match &message.command {
        IrcCommand::NICK(new_nick) => {
            let channels = client.list_channels().unwrap_or_default();
            let old_nick = message.source_nickname().unwrap_or("");
            roster.rename(&channels, old_nick, new_nick);
            roster.presence.rename(old_nick, new_nick);
        }
        IrcCommand::QUIT(_) => {
            roster
                .presence
                .quit(message.source_nickname().unwrap_or(""));
        }
        IrcCommand::ChannelMODE(channel, modes) => roster.clear_mode_targets(channel, modes),
        IrcCommand::PART(channel, _) if message.source_nickname() == Some(current_nick) => {
            roster.forget_channel(channel);
        }
        IrcCommand::KICK(channel, nickname, _) if nickname == current_nick => {
            roster.forget_channel(channel);
        }
        IrcCommand::Response(Response::RPL_ENDOFNAMES, args) => {
            if let Some(channel) = args.iter().find(|arg| valid_channel(arg)) {
                roster.accept_names(channel);
            }
        }
        _ => {}
    }
    let replayed = replay.replayed(&message);
    if let Some(private) = private_message(&message, current_nick, replayed, server_time) {
        return vec![private];
    }
    let mut translated = match &message.command {
        IrcCommand::Response(Response::RPL_WELCOME, args) => vec![Event::Registered {
            nickname: args
                .first()
                .cloned()
                .unwrap_or_else(|| current_nick.to_owned()),
        }],
        IrcCommand::JOIN(channel, _, _) if message.source_nickname() == Some(current_nick) => {
            vec![Event::Joined {
                channel: channel.clone(),
            }]
        }
        IrcCommand::PART(channel, _) if message.source_nickname() == Some(current_nick) => {
            vec![Event::Parted {
                channel: channel.clone(),
            }]
        }
        IrcCommand::KICK(channel, nickname, _) if nickname == current_nick => {
            vec![Event::Parted {
                channel: channel.clone(),
            }]
        }
        IrcCommand::NICK(nickname) if message.source_nickname() == Some(current_nick) => {
            vec![Event::NickChanged {
                nickname: nickname.clone(),
            }]
        }
        IrcCommand::NICK(nickname) => match message.source_nickname() {
            Some(from) => vec![Event::UserNickChanged {
                from: from.to_owned(),
                to: nickname.clone(),
            }],
            None => Vec::new(),
        },
        IrcCommand::PRIVMSG(target, text) | IrcCommand::NOTICE(target, text)
            if valid_channel(target) =>
        {
            let sender = message.source_nickname().unwrap_or("server");
            vec![Event::ChannelMessage {
                channel: target.clone(),
                sender: sender.to_owned(),
                text: text.clone(),
                notice: matches!(message.command, IrcCommand::NOTICE(_, _)),
                server_time,
                msgid: tags::msgid(&message).map(str::to_owned),
                account: tags::account(&message).map(str::to_owned),
                replayed,
            }]
        }
        IrcCommand::Response(Response::RPL_ENDOFNAMES, args) => {
            let Some(channel) = args.iter().find(|arg| valid_channel(arg)) else {
                return Vec::new();
            };
            vec![names_snapshot(client, roster, channel)]
        }
        _ => {
            let mut events = topic_event(&message, server_time)
                .into_iter()
                .collect::<Vec<_>>();
            let shown = without_channel_key(&message);
            events.push(Event::ServerLine(tags::untagged_line(
                shown.as_ref().unwrap_or(&message),
            )));
            events
        }
    };
    if let Some(activity) = activity {
        translated.push(activity);
    }
    if let IrcCommand::QUIT(reason) = &message.command
        && let Some(actor) = &actor
    {
        translated.push(Event::UserQuit {
            nickname: actor.clone(),
            reason: reason.clone(),
        });
        translated.extend(
            changed_channels
                .iter()
                .map(|channel| Event::ChannelActivity {
                    channel: channel.clone(),
                    actor: actor.clone(),
                    kind: ChannelActivityKind::Quit {
                        reason: reason.clone(),
                    },
                    server_time,
                }),
        );
    }
    if let IrcCommand::NICK(to) = &message.command
        && let Some(actor) = &actor
    {
        translated.extend(
            changed_channels
                .iter()
                .map(|channel| Event::ChannelActivity {
                    channel: channel.clone(),
                    actor: actor.clone(),
                    kind: ChannelActivityKind::NickChanged { to: to.clone() },
                    server_time,
                }),
        );
    }
    translated.extend(
        changed_channels
            .iter()
            .map(|channel| names_snapshot(client, roster, channel)),
    );
    translated
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
