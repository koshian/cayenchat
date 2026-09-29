//! IRC transport adapter. Third-party IRC types stay inside this crate.

mod accounts;
mod cap;
mod ctcp;
mod history;
mod metadata;
mod peer_avatar;
mod replay;
mod tags;
pub mod text;

pub use cap::Ircv3Options;
pub use history::{
    HISTORY_LIMIT, HistoryMessage, HistoryResume, MAX_HISTORY_LINES, MessageReference,
    OlderHistoryStatus,
};
pub use metadata::{AvatarRequestFailure, MAX_PUBLISHED_AVATAR_BYTES, publishable_avatar};
pub use peer_avatar::shareable as shareable_avatar;

use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime},
};

use encoding::{EncoderTrap, label::encoding_from_whatwg_label};
use futures_util::StreamExt;
use irc::{
    client::{
        data::user::AccessLevel,
        prelude::{Client, Config},
    },
    proto::{Command as IrcCommand, Message as IrcMessage, Prefix, Response, mode::Mode},
};
use tokio::sync::{Notify, mpsc};

const COMMAND_CAPACITY: usize = 128;
const EVENT_CAPACITY: usize = 512;
const USER_DISCONNECT: &str = "Disconnected by user.";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Servers may hold registration until their ident (RFC 1413) and DNS lookups
/// finish. IRCnet waits about 30 seconds when the client's port 113 silently
/// drops packets, so the limit must comfortably exceed that.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(90);
/// Limits on WHOIS replies that have not reached end-of-WHOIS (318), so a
/// hostile server cannot grow memory by never finishing them.
const MAX_PENDING_WHOIS: usize = 32;
const MAX_WHOIS_ITEMS: usize = 512;
/// The realname sent in `USER` when none is configured; with a shared peer
/// avatar it starts with KVIrc's avatar mark.
const REALNAME: &str = "CayenChat";
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
    pub channels: Vec<String>,
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
}

impl fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("nickname", &self.nickname)
            .field("username", &self.username)
            .field("realname", &self.realname)
            .field("channels", &self.channels)
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
            nickname,
            channels,
            use_tls: true,
            verify_tls_certificates: true,
            encoding: "UTF-8".into(),
            server_password: None,
            allow_plaintext_pass: false,
            sasl: None,
            ircv3: Ircv3Options::default(),
            shared_avatar: None,
            resume_history: Vec::new(),
        }
    }

    /// Whether registration marks the realname as having an avatar: only
    /// with peer exchange on and a URL explicitly shared.
    pub fn advertises_avatar(&self) -> bool {
        self.ircv3.peer_avatars && self.shared_avatar.is_some()
    }

    /// The realname as sent in `USER` and `SETNAME`: the configured value
    /// (or the default), with KVIrc's avatar mark when an avatar is shared.
    pub(crate) fn wire_realname(&self) -> String {
        peer_avatar::realname(base_realname(&self.realname), self.advertises_avatar())
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
        for channel in &self.channels {
            if !valid_channel(channel) {
                return Err(format!("Invalid channel name: {channel}"));
            }
            validate_wire(&format!("JOIN {channel}\r\n"), &self.encoding)?;
        }
        validate_wire(&format!("NICK {}\r\n", self.nickname), &self.encoding)?;
        validate_wire(
            &format!("USER {} 0 * :{}\r\n", self.username, self.wire_realname()),
            &self.encoding,
        )?;
        if let Some(url) = &self.shared_avatar {
            peer_avatar::shareable(url, self.encoding.eq_ignore_ascii_case("UTF-8"))?;
        }
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

fn validate_wire(wire: &str, label: &str) -> Result<(), String> {
    let codec = encoding_from_whatwg_label(label)
        .ok_or_else(|| format!("Unsupported character encoding: {label}"))?;
    let bytes = codec
        .encode(wire, EncoderTrap::Strict)
        .map_err(|_| format!("Text contains a character that cannot be encoded as {label}."))?;
    if bytes.len() > 512 {
        return Err("IRC command exceeds the 512-byte wire limit.".into());
    }
    Ok(())
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
    Joined {
        channel: String,
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
        /// Someone else named our nickname as a word (for example `nick:` or
        /// `@nick`), ignoring formatting codes.
        mentioned: bool,
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
    OutgoingAccepted {
        channel: String,
        text: String,
        notice: bool,
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
    Disconnected(String),
    /// The server rejected credentials or this configuration. Terminal like
    /// `Disconnected`, but reconnecting with the same settings would only be
    /// rejected again, so callers must not retry automatically.
    Refused(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelActivityKind {
    Joined { mask: Option<String> },
    Left { reason: Option<String> },
    Quit { reason: Option<String> },
    ModeChanged { modes: String },
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

async fn diagnostic(events: &mpsc::Sender<Event>, started: Instant, message: impl Into<String>) {
    let _ = events
        .send(Event::Diagnostic {
            elapsed: started.elapsed(),
            message: message.into(),
        })
        .await;
}

fn redacted_wire_line(message: &IrcMessage) -> String {
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
        IrcCommand::Raw(verb, args)
            if matches!(
                verb.to_ascii_uppercase().as_str(),
                "NS" | "NICKSERV" | "CS" | "CHANSERV"
            ) && args
                .first()
                .is_some_and(|arg| is_secret_service_command(arg)) =>
        {
            format!("{verb} [redacted]")
        }
        _ => tags::transcript_line(message),
    }
}

fn is_secret_service_command(command: &str) -> bool {
    matches!(
        command.to_ascii_uppercase().as_str(),
        "IDENTIFY" | "ID" | "LOGIN" | "REGISTER" | "GHOST" | "RECOVER" | "RELEASE" | "SET"
    )
}

fn is_service_secret(target: &str, body: &str) -> bool {
    let name = target.split('@').next().unwrap_or(target);
    matches!(name.to_ascii_uppercase().as_str(), "NICKSERV" | "CHANSERV")
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

async fn wire(
    events: &mpsc::Sender<Event>,
    started: Instant,
    direction: WireDirection,
    line: String,
) {
    let _ = events
        .send(Event::Wire {
            elapsed: started.elapsed(),
            direction,
            line,
        })
        .await;
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
    },
    Raw(IrcMessage),
    /// Publish (`Some`) or remove (`None`) our own avatar; `request`
    /// identifies the outcome events.
    OwnAvatar {
        request: u64,
        url: Option<String>,
    },
    /// The URL answered to CTCP AVATAR queries (`None`: stop answering).
    ShareAvatar(Option<String>),
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
    Quit,
}

fn validate_outgoing(outgoing: &Outgoing, encoding: &str) -> Result<(), String> {
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
        Outgoing::OwnAvatar { url, .. } => {
            let mut args = vec!["*".to_owned(), "SET".into(), metadata::AVATAR_KEY.into()];
            args.extend(url.clone());
            IrcMessage::from(IrcCommand::Raw("METADATA".into(), args))
        }
        Outgoing::ShareAvatar(url) => IrcMessage::from(IrcCommand::NOTICE(
            "*".into(),
            format!("\u{1}AVATAR {}\u{1}", url.as_deref().unwrap_or_default()),
        )),
        // Built and checked by the worker, whose reference types decide it.
        Outgoing::OlderHistory { channel, .. } => IrcMessage::from(IrcCommand::Raw(
            "CHATHISTORY".into(),
            vec!["BEFORE".into(), channel.clone(), "*".into(), "1".into()],
        )),
        Outgoing::SetName(realname) => IrcMessage::from(IrcCommand::Raw(
            "SETNAME".into(),
            vec![peer_avatar::realname(base_realname(realname), true)],
        )),
        Outgoing::Quit => return Ok(()),
    };
    validate_wire(&message.to_string(), encoding)
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
    /// `None` after [`Connection::take_events`].
    events: Option<Events>,
    encoding: String,
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
        let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, events) = mpsc::channel(EVENT_CAPACITY);
        let cancel = Arc::new(Notify::new());
        let worker_cancel = cancel.clone();
        thread::Builder::new()
            .name("cayenchat-irc".into())
            .spawn(move || {
                let failure_events = event_tx.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();
                    match runtime {
                        Ok(runtime) => runtime.block_on(run_cancellable(
                            config,
                            command_rx,
                            event_tx,
                            worker_cancel,
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
            events: Some(Events(events)),
            encoding,
        })
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
        };
        validate_outgoing(&outgoing, &self.encoding)?;
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
        };
        validate_outgoing(&outgoing, &self.encoding)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue private message: {error}"))
    }

    pub fn send_member_command(&self, nickname: &str, action: MemberCommand) -> Result<(), String> {
        let outgoing = member_outgoing(nickname, action)?;
        validate_outgoing(&outgoing, &self.encoding)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue member command: {error}"))
    }

    pub fn send_command(&self, line: &str, selected_channel: Option<&str>) -> Result<(), String> {
        let outgoing = parse_slash_command(line, selected_channel)?;
        validate_outgoing(&outgoing, &self.encoding)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue IRC command: {error}"))
    }

    pub fn change_nickname(&self, nickname: &str) -> Result<(), String> {
        if !valid_nickname(nickname) {
            return Err("Invalid nickname.".into());
        }
        let outgoing = checked_command(IrcCommand::NICK(nickname.to_owned()))?;
        validate_outgoing(&outgoing, &self.encoding)?;
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
        validate_outgoing(&outgoing, &self.encoding)?;
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
        validate_outgoing(&outgoing, &self.encoding)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue avatar sharing: {error}"))
    }

    /// Changes the realname on this connection with IRCv3 `SETNAME`. The
    /// outcome arrives as [`Event::RealNameChanged`] or
    /// [`Event::RealNameFailed`]; without `setname` the latter is immediate
    /// and the value applies at the next connection. `Err` means the value
    /// cannot be sent and nothing was queued.
    pub fn set_real_name(&self, realname: &str) -> Result<(), String> {
        check_realname(realname)?;
        let outgoing = Outgoing::SetName(realname.to_owned());
        validate_outgoing(&outgoing, &self.encoding)?;
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
        validate_outgoing(&outgoing, &self.encoding)?;
        self.commands
            .try_send(outgoing)
            .map_err(|error| format!("Could not queue history request: {error}"))
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
    run_cancellable(config, commands, events, Arc::new(Notify::new())).await
}

async fn run_cancellable(
    config: ConnectionConfig,
    mut commands: mpsc::Receiver<Outgoing>,
    events: mpsc::Sender<Event>,
    cancel: Arc<Notify>,
) {
    let started = Instant::now();
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
    let registration_user = config.username.clone();
    let auto_join_channels = config.channels.clone();
    let irc_config = library_config(&config);
    let advertise_avatar = config.advertises_avatar();
    let registration_realname = config.wire_realname();
    // The avatar mark is fixed for the connection, also for SETNAME.
    let mut pending_setname = 0usize;
    let mut peers = config.ircv3.peer_avatars.then(|| {
        peer_avatar::PeerAvatars::new(
            wire_encoding.eq_ignore_ascii_case("UTF-8"),
            config.shared_avatar.clone(),
        )
    });
    let mut ctcp = ctcp::CtcpReplies::new(config.ircv3.peer_avatars);
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
                "Message tags are not requested with the {wire_encoding} encoding: tag values are UTF-8."
            ),
        )
        .await;
    }
    if config.ircv3.metadata && !wire_encoding.eq_ignore_ascii_case("UTF-8") {
        diagnostic(
            &events,
            started,
            format!(
                "Avatar metadata with the {wire_encoding} encoding: only ASCII avatar URLs are used, because metadata values are UTF-8."
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
                let _ = events.send(Event::Disconnected(USER_DISCONNECT.into())).await;
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
        let line = redacted_wire_line(&message);
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
        wire(&events, started, WireDirection::Sent, line).await;
    }
    let mut registration_deadline = tokio::time::Instant::now() + REGISTRATION_TIMEOUT;
    let mut registered = false;
    // Registration is paused while the UI asks for another nickname.
    let mut awaiting_nick = false;
    let mut refusal = None;
    let mut current_nick = registration_nick;
    let mut roster = RosterTracker::default();
    let mut whois = WhoisCollector::default();
    let mut replay = replay::ReplayTracker::default();
    let mut batch_negotiated = false;
    let mut metadata = metadata::MetadataState::new(wire_encoding.eq_ignore_ascii_case("UTF-8"));
    let mut metadata_enabled = false;
    let mut history = history::HistoryRequests::with_resume(config.resume_history);
    let mut history_enabled = false;
    let mut accounts = accounts::Accounts::default();
    let track_accounts = config.ircv3.accounts;
    // Last reported Event::HistoryAvailable.
    let mut history_available = false;
    let mut registration_progress = tokio::time::interval(Duration::from_secs(10));
    registration_progress.tick().await;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break; };
                match command {
                    Outgoing::Message {target, text, display_text, notice} => {
                        let message = if notice {
                            IrcMessage::from(IrcCommand::NOTICE(target.clone(), text))
                        } else {
                            IrcMessage::from(IrcCommand::PRIVMSG(target.clone(), text))
                        };
                        let line = redacted_wire_line(&message);
                        let result = validate_wire(&message.to_string(), &wire_encoding)
                            .and_then(|_| client.send(message).map_err(|error| error.to_string()));
                        let event = match result {
                            Ok(()) => {
                                wire(&events, started, WireDirection::Sent, line).await;
                                let text = echo_text(&target, display_text);
                                Event::OutgoingAccepted {channel: target, text, notice}
                            }
                            Err(error) => Event::ServerLine(format!("Send failed: {error}")),
                        };
                        if events.send(event).await.is_err() { break; }
                    }
                    Outgoing::Raw(message) => {
                        let line = redacted_wire_line(&message);
                        let retry_nick = match &message.command {
                            IrcCommand::NICK(nickname) if !registered => Some(nickname.clone()),
                            _ => None,
                        };
                        let result = validate_wire(&message.to_string(), &wire_encoding)
                            .and_then(|_| client.send(message).map_err(|error| error.to_string()));
                        match result {
                            Ok(()) => {
                                wire(&events, started, WireDirection::Sent, line).await;
                                if let Some(nickname) = retry_nick {
                                    current_nick = nickname;
                                    awaiting_nick = false;
                                    refusal = None;
                                    registration_deadline = tokio::time::Instant::now() + REGISTRATION_TIMEOUT;
                                }
                            }
                            Err(error) => {
                                if events.send(Event::ServerLine(format!("Command failed: {error}"))).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Outgoing::OwnAvatar { request, url } => {
                        let queued = metadata.request_own(
                            request,
                            url.as_deref(),
                            registered && metadata_enabled,
                            tokio::time::Instant::now(),
                        );
                        match queued {
                            Ok(command) => {
                                let message = IrcMessage::from(command);
                                let line = redacted_wire_line(&message);
                                if let Err(error) = client.send(message) {
                                    let _ = events.send(Event::Disconnected(error_chain(&error))).await;
                                    return;
                                }
                                wire(&events, started, WireDirection::Sent, line).await;
                            }
                            Err(failure) => {
                                if events.send(Event::OwnAvatarFailed { request, failure }).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Outgoing::ShareAvatar(url) => {
                        if let Some(peers) = peers.as_mut() {
                            peers.set_share(url);
                        }
                    }
                    Outgoing::SetName(realname) => {
                        let failure = if !registered || !negotiation.enabled(cap::SETNAME) {
                            Some(RealNameFailure::Unsupported)
                        } else if pending_setname >= MAX_PENDING_SETNAME {
                            Some(RealNameFailure::Busy)
                        } else {
                            let message = IrcMessage::from(IrcCommand::Raw(
                                "SETNAME".into(),
                                vec![peer_avatar::realname(base_realname(&realname), advertise_avatar)],
                            ));
                            let line = redacted_wire_line(&message);
                            match validate_wire(&message.to_string(), &wire_encoding)
                                .and_then(|_| client.send(message).map_err(|error| error.to_string()))
                            {
                                Ok(()) => {
                                    pending_setname += 1;
                                    wire(&events, started, WireDirection::Sent, line).await;
                                    None
                                }
                                Err(error) => Some(RealNameFailure::Rejected(error)),
                            }
                        };
                        if let Some(failure) = failure
                            && events.send(Event::RealNameFailed(failure)).await.is_err()
                        {
                            break;
                        }
                    }
                    Outgoing::OlderHistory { channel, request, reference, limit } => {
                        let queued = if registered && history_enabled {
                            history.enqueue_older(&channel, request, &reference, limit)
                        } else {
                            Err(history::Finished::failed_older(channel, request))
                        };
                        let sent = match queued {
                            Ok(()) => request_history(&client, &events, started, &mut history, registered).await,
                            Err(finished) => history_finished(&events, started, finished).await,
                        };
                        if !sent {
                            return;
                        }
                    }
                    Outgoing::Quit => {
                        let quit = IrcMessage::from(IrcCommand::QUIT(Some("Leaving CayenChat".into())));
                        if client.send(quit.clone()).is_ok() {
                            wire(&events, started, WireDirection::Sent, redacted_wire_line(&quit)).await;
                        }
                        // ClientStream drives the library's outgoing queue. Poll it once more
                        // so QUIT is flushed before the runtime and socket are dropped.
                        let _ = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
                        let _ = events.send(Event::Disconnected(USER_DISCONNECT.into())).await;
                        break;
                    }
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(message)) => {
                        wire(&events, started, WireDirection::Received, redacted_wire_line(&message)).await;
                        // irc's transport enqueues PONG before exposing PING here.
                        if let IrcCommand::PING(data, _) = &message.command {
                            let pong = IrcMessage::from(IrcCommand::PONG(data.clone(), None));
                            wire(&events, started, WireDirection::Sent, redacted_wire_line(&pong)).await;
                        }
                        // irc's client state enqueues configured JOINs on end-of-MOTD.
                        if matches!(message.command, IrcCommand::Response(Response::RPL_ENDOFMOTD | Response::ERR_NOMOTD, _)) {
                            for channel in &auto_join_channels {
                                let join = IrcMessage::from(IrcCommand::JOIN(channel.clone(), None, None));
                                wire(&events, started, WireDirection::Sent, redacted_wire_line(&join)).await;
                            }
                        }
                        if !registered {
                            match &message.command {
                                IrcCommand::Response(response, _) => diagnostic(&events, started,
                                    format!("Server replied {response:?}.")).await,
                                IrcCommand::CAP(_, subcommand, _, _) => diagnostic(&events, started,
                                    format!("Server CAP response: {subcommand:?}.")).await,
                                _ => {}
                            }
                        }
                        if !wire_encoding.eq_ignore_ascii_case("UTF-8") && requires_utf8(&message) {
                            let _ = events.send(Event::Refused(
                                "Server requires UTF-8 (UTF8ONLY); change this server's character encoding to UTF-8.".into()
                            )).await;
                            return;
                        }
                        match negotiation.observe(&message) {
                            Ok(step) => {
                                for note in step.notes {
                                    diagnostic(&events, started, note).await;
                                }
                                for command in step.send {
                                    let message = IrcMessage::from(command);
                                    let line = redacted_wire_line(&message);
                                    if let Err(error) = client.send(message) {
                                        let _ = events.send(Event::Disconnected(error_chain(&error))).await;
                                        return;
                                    }
                                    wire(&events, started, WireDirection::Sent, line).await;
                                }
                            }
                            Err(error) => {
                                let _ = events.send(Event::Refused(error)).await;
                                return;
                            }
                        }
                        // CAP DEL (or ACK -batch) withdrew batch: forget open batches.
                        if batch_negotiated && !negotiation.enabled(cap::BATCH) {
                            replay.reset();
                        }
                        batch_negotiated = negotiation.enabled(cap::BATCH);
                        // Metadata stopped (its own DEL, or batch went away):
                        // every avatar of this connection becomes unknown.
                        if metadata_enabled && !negotiation.enabled(cap::METADATA) {
                            let reset = metadata.reset().into_iter().chain([Event::AvatarsReset]).collect();
                            for event in merged(&mut peers, reset) {
                                if events.send(event).await.is_err() {
                                    return;
                                }
                            }
                        }
                        metadata_enabled = negotiation.enabled(cap::METADATA);
                        // chathistory went away (its own DEL, or batch): the
                        // request being answered ends without lines.
                        if history_enabled && !negotiation.enabled(cap::CHATHISTORY) {
                            for finished in history.reset() {
                                if !history_finished(&events, started, finished).await {
                                    return;
                                }
                            }
                        }
                        let history_started = !history_enabled && negotiation.enabled(cap::CHATHISTORY);
                        history_enabled = negotiation.enabled(cap::CHATHISTORY);
                        history.isupport(&message);
                        accounts.isupport(&message);
                        if !registered && let Some(reason) = registration_refusal(&message) {
                            refusal = Some(reason);
                        }
                        if matches!(message.command, IrcCommand::Response(Response::RPL_WELCOME, _)) {
                            registered = true;
                            negotiation.registered();
                            diagnostic(&events, started, "Registration completed (001 received).").await;
                        }
                        // Reported ahead of the 001's own Registered event;
                        // the application forgets it only on disconnect.
                        if history_available != (registered && history_enabled) {
                            history_available = registered && history_enabled;
                            if events.send(Event::HistoryAvailable(history_available)).await.is_err() {
                                return;
                            }
                        }
                        // Subscribe once registered (the draft allows it earlier
                        // only with `before-connect`), also after a later NEW.
                        let start = if registered && metadata_enabled {
                            metadata.start(tokio::time::Instant::now())
                        } else {
                            Vec::new()
                        };
                        if !start.is_empty() {
                            for command in start {
                                let message = IrcMessage::from(command);
                                let line = redacted_wire_line(&message);
                                if let Err(error) = client.send(message) {
                                    let _ = events.send(Event::Disconnected(error_chain(&error))).await;
                                    return;
                                }
                                wire(&events, started, WireDirection::Sent, line).await;
                            }
                            if events.send(Event::MetadataReady).await.is_err() {
                                return;
                            }
                        }
                        // Enabled after registration (CAP NEW): the channels
                        // already joined get their history too.
                        if history_started && registered {
                            for channel in client.list_channels().unwrap_or_default() {
                                if let Some(note) = history.enqueue(&channel) {
                                    diagnostic(&events, started, note).await;
                                }
                            }
                        }
                        // Our history replies are consumed whole; nothing in
                        // them is live traffic.
                        if history_enabled {
                            match history.observe(&message) {
                                history::Observed::Unrelated => {}
                                history::Observed::Consumed => continue,
                                history::Observed::Finished(finished) => {
                                    if !history_finished(&events, started, finished).await
                                        || !request_history(&client, &events, started, &mut history, registered).await
                                    {
                                        return;
                                    }
                                    continue;
                                }
                            }
                        }
                        // The server withdrew setname: waiting requests end.
                        if pending_setname > 0 && !negotiation.enabled(cap::SETNAME) {
                            pending_setname = 0;
                            if events.send(Event::RealNameFailed(RealNameFailure::Unsupported)).await.is_err() {
                                return;
                            }
                        }
                        // SETNAME is never chat: ours is reported, others' are
                        // not tracked.
                        match &message.command {
                            IrcCommand::Raw(verb, args) if verb == "SETNAME" => {
                                if message.source_nickname() == Some(current_nick.as_str()) {
                                    pending_setname = pending_setname.saturating_sub(1);
                                    let realname = args.last().map(String::as_str).unwrap_or_default();
                                    let event = Event::RealNameChanged {
                                        realname: peer_avatar::without_mark(realname).to_owned(),
                                    };
                                    if events.send(event).await.is_err() { return; }
                                }
                                continue;
                            }
                            IrcCommand::Raw(verb, args) if verb == "FAIL" && args.first().is_some_and(|c| c == "SETNAME") => {
                                pending_setname = pending_setname.saturating_sub(1);
                                let detail = args.last().filter(|_| args.len() > 1).cloned().unwrap_or_default();
                                let event = Event::RealNameFailed(RealNameFailure::Rejected(detail));
                                if events.send(event).await.is_err() { return; }
                                continue;
                            }
                            _ => {}
                        }
                        if metadata_enabled {
                            let joined = client.list_channels().unwrap_or_default();
                            let handled = metadata.observe(&message, tokio::time::Instant::now(), &current_nick, |channel| {
                                joined.iter().any(|name| name.eq_ignore_ascii_case(channel))
                            });
                            if let Some(handled) = handled {
                                for note in handled.notes {
                                    diagnostic(&events, started, note).await;
                                }
                                for event in merged(&mut peers, handled.events) {
                                    if events.send(event).await.is_err() { return; }
                                }
                                continue;
                            }
                        }
                        // CTCP AVATAR and replies to our own lookups are
                        // not chat and not server lines.
                        let replayed = replay.replayed(&message);
                        if let Some(peers) = peers.as_mut()
                            && let Some(handled) = peers.observe(&message, &current_nick, &roster.last, replayed, tokio::time::Instant::now())
                        {
                            if let Err(detail) = send_all(&client, &events, started, handled.send).await {
                                let _ = events.send(Event::Disconnected(detail)).await;
                                return;
                            }
                            for note in handled.notes {
                                diagnostic(&events, started, note).await;
                            }
                            for event in handled.events {
                                if events.send(event).await.is_err() { return; }
                            }
                            continue;
                        }
                        // Other CTCP requests and replies become one
                        // readable server line, answered when asked.
                        if let Some(handled) = ctcp.observe(&message, &current_nick, replayed, tokio::time::Instant::now()) {
                            if let Err(detail) = send_all(&client, &events, started, handled.send).await {
                                let _ = events.send(Event::Disconnected(detail)).await;
                                return;
                            }
                            for event in handled.events {
                                if events.send(event).await.is_err() { return; }
                            }
                            continue;
                        }
                        // Peer avatars end before metadata's events for the
                        // same message are merged, so a departed user's
                        // peer avatar cannot stand in for their metadata.
                        let mut lifecycle = peers
                            .as_mut()
                            .map(|peers| peers.lifecycle(&message, &roster.last, &current_nick))
                            .unwrap_or_default();
                        lifecycle.extend(merged(&mut peers, if metadata_enabled {
                            if let IrcCommand::PART(channel, _) | IrcCommand::KICK(channel, _, _) = &message.command {
                                metadata.forget_channel(channel);
                            }
                            let handled = metadata.lifecycle(&message, &roster.last, &current_nick, replayed, tokio::time::Instant::now());
                            for note in handled.notes {
                                diagnostic(&events, started, note).await;
                            }
                            handled.events
                        } else {
                            Vec::new()
                        }));
                        let whois_reply = whois.observe(&message);
                        if history_enabled && message.source_nickname() == Some(current_nick.as_str()) {
                            match &message.command {
                                IrcCommand::JOIN(channel, _, _) => {
                                    if let Some(note) = history.enqueue(channel) {
                                        diagnostic(&events, started, note).await;
                                    }
                                }
                                IrcCommand::PART(channel, _) => {
                                    for finished in history.forget(channel) {
                                        if !history_finished(&events, started, finished).await {
                                            return;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if let IrcCommand::KICK(channel, nickname, _) = &message.command
                            && nickname == &current_nick
                        {
                            for finished in history.forget(channel) {
                                if !history_finished(&events, started, finished).await {
                                    return;
                                }
                            }
                        }
                        // Our WHOX reply is not a server line.
                        if track_accounts && let Some(reply) = accounts.observe_who(&message) {
                            for event in reply {
                                if events.send(event).await.is_err() { return; }
                            }
                            if let Some(command) = accounts.next_who(tokio::time::Instant::now())
                                && let Err(detail) = send_all(&client, &events, started, vec![command]).await
                            {
                                let _ = events.send(Event::Disconnected(detail)).await;
                                return;
                            }
                            continue;
                        }
                        let mut account_events = if track_accounts {
                            accounts.observe(&message, &current_nick, &roster.last)
                        } else {
                            Vec::new()
                        };
                        let server_time = tags::server_time(&message, negotiation.enabled(cap::SERVER_TIME));
                        for event in translate_message(&client, &mut roster, &mut replay, &current_nick, message, server_time, batch_negotiated).into_iter().chain(whois_reply.map(|info| Event::Whois(Box::new(info)))) {
                            match &event {
                                Event::Registered { nickname } | Event::NickChanged { nickname } => current_nick = nickname.clone(),
                                // Someone speaking live may be asked about
                                // their avatar; nobody else is.
                                Event::ChannelMessage { sender, replayed: false, .. }
                                | Event::PrivateMessage { sender, replayed: false, .. } => {
                                    if let Some(note) = peers.as_mut().and_then(|peers| peers.speaker(sender, &current_nick, &roster.last)) {
                                        diagnostic(&events, started, note).await;
                                    }
                                }
                                _ => {}
                            }
                            if track_accounts {
                                match &event {
                                    Event::Names { channel, users } => account_events.extend(accounts.names(channel, users)),
                                    Event::Joined { channel } => accounts.joined(channel),
                                    _ => {}
                                }
                            }
                            if events.send(event).await.is_err() { return; }
                        }
                        for event in account_events {
                            if events.send(event).await.is_err() { return; }
                        }
                        if track_accounts
                            && let Some(command) = accounts.next_who(tokio::time::Instant::now())
                            && let Err(detail) = send_all(&client, &events, started, vec![command]).await
                        {
                            let _ = events.send(Event::Disconnected(detail)).await;
                            return;
                        }
                        for event in lifecycle {
                            if events.send(event).await.is_err() { return; }
                        }
                        // After the JOIN's own events, so the channel exists
                        // in the application before its history is asked for.
                        if history_enabled && !request_history(&client, &events, started, &mut history, registered).await {
                            return;
                        }
                    }
                    // irc consumes 432/433 and reports NoUsableNick because no
                    // alternate nicknames are configured; the link stays usable.
                    Some(Err(irc::error::Error::NoUsableNick)) if !registered => {
                        diagnostic(&events, started,
                            format!("Server rejected nickname {current_nick} (432/433); waiting for another nickname.")).await;
                        awaiting_nick = true;
                        refusal = Some(format!("Nickname {current_nick} is unavailable (432/433)."));
                        if events.send(Event::NicknameRejected { nickname: current_nick.clone() }).await.is_err() {
                            return;
                        }
                    }
                    Some(Err(irc::error::Error::NoUsableNick)) => {
                        if events.send(Event::ServerLine(
                            "Nickname change rejected: the nickname is in use or invalid (432/433).".into()
                        )).await.is_err() {
                            return;
                        }
                    }
                    Some(Err(error)) => {
                        let detail = stream_error_detail(&error);
                        diagnostic(&events, started, format!("IRC stream failed: {detail}")).await;
                        let _ = events.send(refusal.take().map_or(Event::Disconnected(detail), Event::Refused)).await;
                        break;
                    }
                    None => {
                        diagnostic(&events, started, "Server closed the IRC stream.").await;
                        let _ = events.send(refusal.take().map_or_else(
                            || Event::Disconnected("Server closed the connection.".into()),
                            Event::Refused,
                        )).await;
                        break;
                    }
                }
            }
            // Deferred avatar synchronizations (774), request timeouts and
            // spaced-out lookups; no timer runs while none is pending.
            _ = tokio::time::sleep_until(metadata.next_deadline().unwrap_or_else(tokio::time::Instant::now)), if metadata_enabled && metadata.next_deadline().is_some() => {
                let joined = client.list_channels().unwrap_or_default();
                let handled = metadata.tick(tokio::time::Instant::now(), |channel| {
                    joined.iter().any(|name| name.eq_ignore_ascii_case(channel))
                });
                for command in handled.send {
                    let message = IrcMessage::from(command);
                    let line = redacted_wire_line(&message);
                    if client.send(message).is_ok() {
                        wire(&events, started, WireDirection::Sent, line).await;
                    }
                }
                for note in handled.notes {
                    diagnostic(&events, started, note).await;
                }
                for event in merged(&mut peers, handled.events) {
                    if events.send(event).await.is_err() { return; }
                }
            }
            // An unanswered history request; no timer runs otherwise.
            _ = tokio::time::sleep_until(history.next_deadline().unwrap_or_else(tokio::time::Instant::now)), if history_enabled && history.next_deadline().is_some() => {
                if let Some(history::Observed::Finished(finished)) = history.tick(tokio::time::Instant::now())
                    && (!history_finished(&events, started, finished).await
                        || !request_history(&client, &events, started, &mut history, registered).await)
                {
                    return;
                }
            }
            // Peer avatar lookups and queries, spaced out; no timer runs
            // while none is queued or outstanding.
            _ = tokio::time::sleep_until(peers.as_ref().and_then(peer_avatar::PeerAvatars::next_deadline).unwrap_or_else(tokio::time::Instant::now)), if registered && peers.as_ref().and_then(peer_avatar::PeerAvatars::next_deadline).is_some() => {
                let handled = peers.as_mut().map(|peers| peers.tick(tokio::time::Instant::now(), &current_nick, &roster.last)).unwrap_or_default();
                if let Err(detail) = send_all(&client, &events, started, handled.send).await {
                    let _ = events.send(Event::Disconnected(detail)).await;
                    return;
                }
            }
            _ = registration_progress.tick(), if !registered && !awaiting_nick => {
                diagnostic(&events, started, "Still waiting for IRC registration (001 welcome).").await;
            }
            _ = tokio::time::sleep_until(registration_deadline), if !registered && !awaiting_nick => {
                diagnostic(&events, started, "IRC registration timed out.").await;
                let _ = events.send(refusal.take().map_or_else(
                    || Event::Disconnected("Registration timed out.".into()),
                    Event::Refused,
                )).await;
                break;
            }
        }
    }
}

/// Sends the next queued history request, if any may go out now, and
/// reports it. `false` means the connection or the event channel failed.
async fn request_history(
    client: &Client,
    events: &mpsc::Sender<Event>,
    started: Instant,
    history: &mut history::HistoryRequests,
    registered: bool,
) -> bool {
    if !registered {
        return true;
    }
    let Some((channel, page, command)) = history.next_request(tokio::time::Instant::now()) else {
        return true;
    };
    if let Err(detail) = send_all(client, events, started, vec![command]).await {
        let _ = events.send(Event::Disconnected(detail)).await;
        return false;
    }
    // Only recent history needs its place reserved; an older page goes
    // before everything the conversation holds.
    let resumed = match page {
        history::Page::Latest => false,
        history::Page::Resume => true,
        history::Page::Before { .. } => return true,
    };
    events
        .send(Event::HistoryRequested { channel, resumed })
        .await
        .is_ok()
}

/// Reports a finished history request. `false` means the event channel
/// closed.
async fn history_finished(
    events: &mpsc::Sender<Event>,
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
    events: &mpsc::Sender<Event>,
    started: Instant,
    commands: Vec<IrcCommand>,
) -> Result<(), String> {
    for command in commands {
        let message = IrcMessage::from(command);
        let line = redacted_wire_line(&message);
        client.send(message).map_err(|error| error_chain(&error))?;
        wire(events, started, WireDirection::Sent, line).await;
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
    last: HashMap<String, Vec<String>>,
    renamed_roles: HashMap<(String, String), char>,
}

impl RosterTracker {
    fn rename(&mut self, channels: &[String], old_nick: &str, new_nick: &str) {
        for channel in channels {
            let old_key = (channel.clone(), old_nick.to_lowercase());
            let role = self.renamed_roles.remove(&old_key).or_else(|| {
                self.last.get(channel).and_then(|members| {
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
        self.last.get(channel).is_some_and(|members| {
            members
                .iter()
                .any(|member| display_nickname(member).eq_ignore_ascii_case(nickname))
        })
    }

    fn forget_channel(&mut self, channel: &str) {
        self.last.remove(channel);
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
                info.realname = if args.len() > 5 {
                    args.last().cloned()
                } else {
                    None
                };
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
fn topic_event(message: &IrcMessage) -> Option<Event> {
    let (channel, topic) = match &message.command {
        IrcCommand::Response(Response::RPL_TOPIC, args) => {
            (args.get(1)?, args.get(2).cloned().unwrap_or_default())
        }
        IrcCommand::Response(Response::RPL_NOTOPIC, args) => (args.get(1)?, String::new()),
        IrcCommand::TOPIC(channel, topic) => (channel, topic.clone().unwrap_or_default()),
        _ => return None,
    };
    valid_channel(channel).then(|| Event::Topic {
        channel: channel.clone(),
        topic,
    })
}

fn names_snapshot(client: &Client, roster: &mut RosterTracker, channel: &str) -> Event {
    let tracked = client.list_users(channel);
    let known = tracked.is_some();
    let mut users: Vec<String> = tracked
        .unwrap_or_default()
        .iter()
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
        roster.last.insert(channel.to_owned(), users.clone());
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
            text: text.clone(),
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
        IrcCommand::ChannelMODE(channel, modes) => Some(Event::ChannelActivity {
            channel: channel.clone(),
            actor: actor.clone().unwrap_or_else(|| "server".into()),
            kind: ChannelActivityKind::ModeChanged {
                modes: modes
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
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
            roster.rename(&channels, message.source_nickname().unwrap_or(""), new_nick);
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
                mentioned: !crate::text::same_nickname(sender, current_nick)
                    && crate::text::mentions_nickname(
                        &crate::text::strip_formatting(text),
                        current_nick,
                    ),
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
            let mut events = topic_event(&message).into_iter().collect::<Vec<_>>();
            events.push(Event::ServerLine(tags::untagged_line(&message)));
            events
        }
    };
    if let Some(activity) = activity {
        translated.push(activity);
    }
    if let IrcCommand::QUIT(reason) = &message.command
        && let Some(actor) = actor
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
    translated.extend(
        changed_channels
            .iter()
            .map(|channel| names_snapshot(client, roster, channel)),
    );
    translated
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        time::Instant,
    };

    #[test]
    fn topic_lines_become_topic_events() {
        let topic = |line: &str| topic_event(&line.parse::<IrcMessage>().unwrap());
        let event = |channel: &str, text: &str| {
            Some(Event::Topic {
                channel: channel.into(),
                topic: text.into(),
            })
        };
        assert_eq!(
            topic(":irc.example 332 me #test :Welcome, all"),
            event("#test", "Welcome, all")
        );
        assert_eq!(
            topic(":irc.example 331 me #test :No topic is set"),
            event("#test", "")
        );
        assert_eq!(
            topic(":alice!u@h TOPIC #test :New topic"),
            event("#test", "New topic")
        );
        assert_eq!(topic(":alice!u@h TOPIC #test :"), event("#test", ""));
        assert_eq!(topic(":alice!u@h PRIVMSG #test :hi"), None);
    }

    #[test]
    fn member_commands_validate_targets_and_build_expected_irc_lines() {
        let wire = |action| match member_outgoing("Alice", action).unwrap() {
            Outgoing::Raw(message) => message.to_string().trim_end().to_owned(),
            other => panic!("expected raw command: {other:?}"),
        };
        assert_eq!(wire(MemberCommand::Whois), "WHOIS Alice");
        assert_eq!(
            wire(MemberCommand::Invite {
                channel: "#test".into()
            }),
            "INVITE Alice #test"
        );
        assert_eq!(
            wire(MemberCommand::GiveOp {
                channel: "#test".into()
            }),
            "MODE #test +o Alice"
        );
        assert_eq!(
            wire(MemberCommand::Deop {
                channel: "#test".into()
            }),
            "MODE #test -o Alice"
        );
        assert!(member_outgoing("bad nick", MemberCommand::Whois).is_err());
        assert!(member_outgoing("Alice,Bob", MemberCommand::Whois).is_err());
    }

    #[test]
    fn private_conversations_are_targets_for_me_and_msg_but_not_channel_commands() {
        let target = |line: &str| match parse_slash_command(line, Some("bob")) {
            Ok(Outgoing::Message { target, .. }) => Ok(target),
            Ok(other) => Err(format!("{other:?}")),
            Err(error) => Err(error),
        };
        assert_eq!(target("/me waves").as_deref(), Ok("bob"));
        assert_eq!(target("/msg :hello").as_deref(), Ok("bob"));
        assert_eq!(target("/msg carol hi").as_deref(), Ok("carol"));
        assert!(parse_slash_command("/part", Some("bob")).is_err());
        assert!(parse_slash_command("/topic new", Some("bob")).is_err());
    }

    #[test]
    fn service_credentials_are_not_echoed_into_conversations() {
        assert_eq!(
            echo_text("NickServ", "IDENTIFY hunter2".into()),
            "[redacted]"
        );
        assert_eq!(
            echo_text("nickserv@services.example", "identify a b".into()),
            "[redacted]"
        );
        assert_eq!(echo_text("NickServ", "INFO bob".into()), "INFO bob");
        assert_eq!(
            echo_text("bob", "IDENTIFY hunter2".into()),
            "IDENTIFY hunter2"
        );
    }

    #[test]
    fn account_tags_ride_along_with_channel_and_private_messages() {
        let private =
            |line: &str| private_message(&line.parse::<IrcMessage>().unwrap(), "me", false, None);
        let account = |event: Option<Event>| match event {
            Some(Event::PrivateMessage { account, .. }) => account,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            account(private("@account=alice :alice!u@h PRIVMSG me :hi")),
            Some("alice".into())
        );
        // Not logged in: the tag is absent; `*` and empty values are not accounts.
        assert_eq!(account(private(":alice!u@h PRIVMSG me :hi")), None);
        assert_eq!(
            account(private("@account=* :alice!u@h PRIVMSG me :hi")),
            None
        );
        assert_eq!(
            account(private("@account= :alice!u@h PRIVMSG me :hi")),
            None
        );
        // The account can differ between messages of one nickname.
        assert_eq!(
            account(private("@account=alice2 :alice!u@h PRIVMSG me :again")),
            Some("alice2".into())
        );
    }

    #[test]
    fn private_messages_come_only_from_users_to_our_nickname() {
        let translate =
            |line: &str| private_message(&line.parse::<IrcMessage>().unwrap(), "Me", false, None);
        assert_eq!(
            translate(":alice!u@h PRIVMSG me :hello"),
            Some(Event::PrivateMessage {
                sender: "alice".into(),
                text: "hello".into(),
                notice: false,
                server_time: None,
                msgid: None,
                account: None,
                replayed: false,
            })
        );
        assert_eq!(
            translate(":alice!u@h NOTICE Me :psst"),
            Some(Event::PrivateMessage {
                sender: "alice".into(),
                text: "psst".into(),
                notice: true,
                server_time: None,
                msgid: None,
                account: None,
                replayed: false,
            })
        );
        assert!(matches!(
            translate(":alice!u@h PRIVMSG Me :\u{1}ACTION waves\u{1}"),
            Some(Event::PrivateMessage { .. })
        ));
        assert_eq!(translate(":alice!u@h PRIVMSG Me :\u{1}VERSION\u{1}"), None);
        assert_eq!(translate(":irc.example NOTICE Me :*** Looking up"), None);
        assert_eq!(translate(":alice!u@h PRIVMSG other :hello"), None);
        // RFC 1459 case mapping: `[` and `{` are the same letter.
        let message = ":alice!u@h PRIVMSG m{e} :x".parse::<IrcMessage>().unwrap();
        assert!(private_message(&message, "M[E]", false, None).is_some());
        // Our own line to someone, relayed by a bouncer (another client of
        // ours, or its playback).
        let own =
            |line: &str| private_message(&line.parse::<IrcMessage>().unwrap(), "Me", true, None);
        assert_eq!(
            own("@msgid=x1 :me!u@h PRIVMSG bob :sent elsewhere"),
            Some(Event::OwnPrivateMessage {
                target: "bob".into(),
                text: "sent elsewhere".into(),
                notice: false,
                server_time: None,
                msgid: Some("x1".into()),
                replayed: true,
            })
        );
        assert_eq!(own(":me!u@h PRIVMSG #chan :channel"), None);
        assert_eq!(own(":me!u@h PRIVMSG bob :\u{1}VERSION\u{1}"), None);
        assert_eq!(translate(":alice!u@h PRIVMSG #chan :hello"), None);
    }

    #[test]
    fn whois_collector_bounds_unfinished_replies() {
        let mut collector = WhoisCollector::default();
        for index in 0..MAX_PENDING_WHOIS + 10 {
            let line = format!(":srv 311 me nick{index} u h * :real");
            assert_eq!(collector.observe(&line.parse().unwrap()), None);
        }
        assert_eq!(collector.pending.len(), MAX_PENDING_WHOIS);
        for _ in 0..MAX_WHOIS_ITEMS {
            let line = ":srv 319 me nick0 :#a #b";
            collector.observe(&line.parse().unwrap());
            let line = ":srv 671 me nick0 :is using a secure connection";
            collector.observe(&line.parse().unwrap());
        }
        let info = collector
            .observe(&":srv 318 me nick0 :End".parse().unwrap())
            .unwrap();
        assert_eq!(info.channels.len(), MAX_WHOIS_ITEMS);
        assert_eq!(info.extra.len(), MAX_WHOIS_ITEMS);
    }

    #[test]
    fn whois_collector_merges_replies_until_end_of_whois() {
        let mut collector = WhoisCollector::default();
        let mut feed = |line: &str| collector.observe(&line.parse::<IrcMessage>().unwrap());
        assert_eq!(feed(":srv 301 me Alice :before whois"), None);
        assert_eq!(
            feed(":srv 311 me Alice ~alice example.org * :Alice Liddell"),
            None
        );
        assert_eq!(feed(":srv 319 me Alice :@#one +#two"), None);
        assert_eq!(feed(":srv 319 me Alice :#three"), None);
        assert_eq!(
            feed(":srv 312 me Alice irc.example.org :Example server"),
            None
        );
        assert_eq!(feed(":srv 301 me Alice :gone fishing"), None);
        assert_eq!(feed(":srv 330 me Alice alice :is logged in as"), None);
        assert_eq!(
            feed(":srv 671 me Alice :is using a secure connection"),
            None
        );
        assert_eq!(
            feed(":srv 317 me Alice 665 1788066240 :seconds idle, signon time"),
            None
        );
        let info = feed(":srv 318 me Alice :End of WHOIS list.").unwrap();
        assert!(info.found());
        assert_eq!(info.nickname, "Alice");
        assert_eq!(info.username.as_deref(), Some("~alice"));
        assert_eq!(info.host.as_deref(), Some("example.org"));
        assert_eq!(info.realname.as_deref(), Some("Alice Liddell"));
        assert_eq!(info.channels, ["@#one", "+#two", "#three"]);
        assert_eq!(info.server.as_deref(), Some("irc.example.org"));
        assert_eq!(info.server_info.as_deref(), Some("Example server"));
        assert_eq!(info.away.as_deref(), Some("gone fishing"));
        assert_eq!(info.account.as_deref(), Some("alice"));
        assert_eq!(info.idle_seconds, Some(665));
        assert_eq!(info.signon, Some(1788066240));
        assert_eq!(info.extra, ["is using a secure connection"]);
        assert_eq!(feed(":srv 301 me Alice :after whois"), None);

        assert_eq!(feed(":srv 401 me Nobody :No such nick/channel"), None);
        let missing = feed(":srv 318 me Nobody :End of WHOIS list.").unwrap();
        assert_eq!(missing.nickname, "Nobody");
        assert!(!missing.found());
    }

    #[test]
    fn tls_provider_is_explicit_when_both_backends_are_enabled() {
        ensure_tls_crypto_provider().unwrap();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
        let _ = rustls::ClientConfig::builder();
    }

    #[test]
    fn closed_worker_keeps_buffered_diagnostics_readable() {
        let (commands, _) = mpsc::channel(1);
        let (event_tx, events) = mpsc::channel(1);
        event_tx
            .try_send(Event::Diagnostic {
                elapsed: Duration::ZERO,
                message: "TLS failed".into(),
            })
            .unwrap();
        drop(event_tx);
        let mut connection = Connection {
            commands,
            cancel: Arc::new(Notify::new()),
            events: Some(Events(events)),
            encoding: "UTF-8".into(),
        };
        assert!(!connection.is_closed());
        assert!(matches!(
            connection.try_recv(),
            Some(Event::Diagnostic { .. })
        ));
        assert!(connection.is_closed());
    }

    #[test]
    fn taken_events_can_be_awaited_until_the_worker_ends() {
        let (commands, _) = mpsc::channel(1);
        let (event_tx, events) = mpsc::channel(1);
        let mut connection = Connection {
            commands,
            cancel: Arc::new(Notify::new()),
            events: Some(Events(events)),
            encoding: "UTF-8".into(),
        };
        let mut events = connection.take_events().unwrap();
        assert!(connection.take_events().is_none());
        assert!(connection.try_recv().is_none());
        event_tx.try_send(Event::TransportConnected).unwrap();
        drop(event_tx);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(matches!(
                events.recv().await,
                Some(Event::TransportConnected)
            ));
            assert!(events.recv().await.is_none());
        });
        assert!(events.is_closed());
    }

    #[test]
    fn diagnostic_wire_preserves_messages_and_redacts_credentials() {
        let line = |command| redacted_wire_line(&IrcMessage::from(command));
        assert_eq!(
            line(IrcCommand::PASS("server-secret".into())),
            "PASS [redacted]"
        );
        assert_eq!(
            line(IrcCommand::AUTHENTICATE("base64-secret".into())),
            "AUTHENTICATE [redacted]"
        );
        assert_eq!(
            line(IrcCommand::AUTHENTICATE("PLAIN".into())),
            "AUTHENTICATE PLAIN"
        );
        assert_eq!(
            line(IrcCommand::OPER("alice".into(), "oper-secret".into())),
            "OPER alice [redacted]"
        );
        assert_eq!(
            line(IrcCommand::JOIN(
                "#test".into(),
                Some("channel-key".into()),
                None
            )),
            "JOIN #test [redacted]"
        );
        assert_eq!(
            line(IrcCommand::PRIVMSG(
                "NickServ".into(),
                "IDENTIFY account-secret".into()
            )),
            "PRIVMSG NickServ :[redacted]"
        );
        assert_eq!(
            line(IrcCommand::CHANSERV("IDENTIFY channel-secret".into())),
            "CHANSERV [redacted]"
        );
        assert_eq!(
            line(IrcCommand::PRIVMSG(
                "#test".into(),
                "hello from the channel".into()
            )),
            "PRIVMSG #test :hello from the channel"
        );
        let error = irc::error::Error::CodecFailed {
            codec: "ISO-2022-JP",
            data: "PASS server-secret".into(),
        };
        assert!(!stream_error_detail(&error).contains("server-secret"));
    }

    #[test]
    fn certificate_verification_is_opt_out_for_tls_only() {
        let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
        assert!(!library_config(&config).dangerously_accept_invalid_certs());
        config.verify_tls_certificates = false;
        assert!(library_config(&config).dangerously_accept_invalid_certs());
        config.use_tls = false;
        assert!(!library_config(&config).dangerously_accept_invalid_certs());
    }

    #[test]
    fn debug_output_redacts_credentials_and_username_is_validated() {
        let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
        config.server_password = Some("server-secret".into());
        config.sasl = Some(SaslCredentials {
            username: "account".into(),
            password: "sasl-secret".into(),
        });
        let debug = format!("{config:?}");
        assert!(!debug.contains("server-secret"), "{debug}");
        assert!(!debug.contains("sasl-secret"), "{debug}");
        assert!(debug.contains("account"));

        assert_eq!(config.username, "alice");
        config.username = "ident".into();
        config.validate().unwrap();
        assert_eq!(config.nickname, "alice");
        assert_eq!(library_config(&config).username.as_deref(), Some("ident"));
        assert_eq!(library_config(&config).nickname.as_deref(), Some("alice"));
        for bad in ["", "two words", "a@b", "bad\r\nQUIT"] {
            config.username = bad.into();
            assert!(config.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn config_and_outgoing_reject_protocol_control_characters() {
        let mut config = ConnectionConfig::tls(
            "irc.example.org".into(),
            "alice".into(),
            vec!["#test".into()],
        );
        assert!(config.validate().is_ok());
        config.nickname = "bad\r\nNICK".into();
        assert!(config.validate().is_err());
        assert!(!valid_channel("#bad\nJOIN"));
        config.nickname = "alice".into();
        config.sasl = Some(SaslCredentials {
            username: "account".into(),
            password: "secret".into(),
        });
        config.use_tls = false;
        assert!(config.validate().is_err());
        assert!(validate_wire("PRIVMSG #test :🙂\r\n", "ISO-2022-JP").is_err());
        let japanese = "あ".repeat(200);
        let outgoing = parse_slash_command(&format!("/msg #test {japanese}"), None).unwrap();
        assert!(validate_outgoing(&outgoing, "ISO-2022-JP").is_ok());
        assert!(validate_outgoing(&outgoing, "UTF-8").is_err());
    }

    #[test]
    fn utf8_only_isupport_is_detected() {
        let message: IrcMessage = ":server 005 alice UTF8ONLY CHANTYPES=# :are supported"
            .parse()
            .unwrap();
        assert!(requires_utf8(&message));
        let message: IrcMessage = ":server 005 alice CHANTYPES=# :are supported"
            .parse()
            .unwrap();
        assert!(!requires_utf8(&message));
    }

    #[test]
    fn iso_2022_jp_channel_name_and_message_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            loop {
                let mut line = Vec::new();
                reader.read_until(b'\n', &mut line).unwrap();
                if line.starts_with(b"USER ") {
                    break;
                }
            }
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
                .unwrap();
            let mut join = Vec::new();
            reader.read_until(b'\n', &mut join).unwrap();
            // The encoded comma is part of が, not a channel-list separator.
            assert_eq!(join, b"JOIN #\x1b$B$,$,\x1b(B\r\n");
            socket.write_all(b":alice!u@h JOIN #\x1b$B$,$,\x1b(B\r\n:alice!u@h PRIVMSG #\x1b$B$,$,\x1b(B :\x1b$B$3$s$K$A$O\x1b(B\r\n").unwrap();
            let reply = loop {
                let mut line = Vec::new();
                reader.read_until(b'\n', &mut line).unwrap();
                if line.starts_with(b"PRIVMSG ") {
                    break line;
                }
            };
            assert_eq!(reply, b"PRIVMSG #\x1b$B$,$,\x1b(B \x1b$BJV;v\x1b(B\r\n");
        });
        let mut config =
            ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#がが".into()]);
        config.port = port;
        config.use_tls = false;
        config.encoding = "ISO-2022-JP".into();
        let mut connection = Connection::connect(config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut joined = false;
        let mut received = false;
        while Instant::now() < deadline && !(joined && received) {
            match connection.try_recv() {
                Some(Event::Joined { channel }) => joined = channel == "#がが",
                Some(Event::ChannelMessage { channel, text, .. }) => {
                    received = channel == "#がが" && text == "こんにちは";
                }
                Some(Event::Disconnected(reason)) => panic!("unexpected disconnect: {reason}"),
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(joined && received, "Japanese channel event was not decoded");
        assert!(connection.send_message("#がが", "🙂", false).is_err());
        connection.send_message("#がが", "返事", false).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn slash_commands_infer_selected_channel_and_reject_injection() {
        let wire = |line: &str, selected: Option<&str>| match parse_slash_command(line, selected)
            .unwrap()
        {
            Outgoing::Raw(message) => message.to_string().trim_end().to_owned(),
            other => panic!("expected raw command: {other:?}"),
        };
        assert_eq!(wire("/part", Some("#test")), "PART #test");
        assert_eq!(
            wire("/part Leaving now", Some("#test")),
            "PART #test :Leaving now"
        );
        assert_eq!(
            wire("/topic New topic", Some("#test")),
            "TOPIC #test :New topic"
        );
        assert_eq!(wire("/mode +o bob", Some("#test")), "MODE #test +o bob");
        assert_eq!(
            wire("/kick bob goodbye", Some("#test")),
            "KICK #test bob goodbye"
        );
        assert_eq!(wire("/invite bob", Some("#test")), "INVITE bob #test");
        assert_eq!(wire("/names", Some("#test")), "NAMES #test");
        assert_eq!(wire("/join #other", None), "JOIN #other");
        assert_eq!(wire("/raw WHOIS bob", None), "WHOIS bob");
        assert!(parse_slash_command("/part", None).is_err());
        assert!(parse_slash_command("/raw PRIVMSG #test :hi\r\nQUIT", Some("#test")).is_err());
        match parse_slash_command("/me waves", Some("#test")).unwrap() {
            Outgoing::Message {
                target,
                text,
                display_text,
                ..
            } => {
                assert_eq!(target, "#test");
                assert_eq!(text, "\u{1}ACTION waves\u{1}");
                assert_eq!(display_text, "* waves");
            }
            other => panic!("expected action: {other:?}"),
        }
        match parse_slash_command("/msg :hello everyone", Some("#test")).unwrap() {
            Outgoing::Message { target, text, .. } => {
                assert_eq!(target, "#test");
                assert_eq!(text, "hello everyone");
            }
            other => panic!("expected message: {other:?}"),
        }
    }

    #[test]
    fn safe_channel_targets_and_commands_keep_the_full_name() {
        for channel in ["!test", "!ABCDEtest", "!ABCDE日本語"] {
            let config = ConnectionConfig::tls(
                "irc.example.org".into(),
                "alice".into(),
                vec![channel.into()],
            );
            assert!(config.validate().is_ok());
            assert!(!valid_nickname(channel));
            for (line, expected) in [
                (format!("/join {channel}"), format!("JOIN {channel}")),
                ("/part".into(), format!("PART {channel}")),
                (
                    format!("/part {channel} bye"),
                    format!("PART {channel} bye"),
                ),
                (
                    "/topic new topic".into(),
                    format!("TOPIC {channel} :new topic"),
                ),
                (
                    format!("/topic {channel} topic"),
                    format!("TOPIC {channel} topic"),
                ),
                ("/mode +o bob".into(), format!("MODE {channel} +o bob")),
                (
                    format!("/mode {channel} +o bob"),
                    format!("MODE {channel} +o bob"),
                ),
                ("/kick bob bye".into(), format!("KICK {channel} bob bye")),
                (
                    format!("/kick {channel} bob bye"),
                    format!("KICK {channel} bob bye"),
                ),
                ("/invite bob".into(), format!("INVITE bob {channel}")),
                (
                    format!("/invite bob {channel}"),
                    format!("INVITE bob {channel}"),
                ),
                ("/names".into(), format!("NAMES {channel}")),
            ] {
                let Outgoing::Raw(message) = parse_slash_command(&line, Some(channel)).unwrap()
                else {
                    panic!("expected a raw command for {line}");
                };
                assert_eq!(message.to_string().trim_end(), expected);
            }
            let Outgoing::Message { target, .. } =
                parse_slash_command("/me waves", Some(channel)).unwrap()
            else {
                panic!("expected an action");
            };
            assert_eq!(target, channel);
            for action in [
                MemberCommand::Invite {
                    channel: channel.into(),
                },
                MemberCommand::GiveOp {
                    channel: channel.into(),
                },
                MemberCommand::Deop {
                    channel: channel.into(),
                },
            ] {
                assert!(member_outgoing("bob", action).is_ok());
            }
        }
        for invalid in ["!", "!bad name", "!bad,other", "!bad\r\nJOIN", "!bad\0"] {
            assert!(!valid_channel(invalid));
        }
        // Accepting safe channels must not mistake mode flags for channel names.
        assert!(!valid_channel("+o"));
    }

    #[test]
    fn safe_channel_join_names_messages_and_part_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
            let read = |lines: &mut std::io::Lines<BufReader<std::net::TcpStream>>| {
                lines.next().expect("client closed the connection").unwrap()
            };
            while !read(&mut lines).starts_with("USER ") {}
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
                .unwrap();
            assert_eq!(read(&mut lines), "JOIN !test");
            // IRCnet resolves the short name to the server-assigned full name.
            socket.write_all(b":alice!u@h JOIN !ABCDEtest\r\n:server 353 alice = !ABCDEtest :@alice bob\r\n:server 366 alice !ABCDEtest :End of NAMES\r\n:bob!u@h PRIVMSG !ABCDEtest :hello\r\n:bob!u@h NOTICE !ABCDEtest :notice\r\n").unwrap();
            for expected in [
                "PRIVMSG !ABCDEtest reply",
                "NOTICE !ABCDEtest notice",
                "INVITE bob !ABCDEtest",
                "MODE !ABCDEtest +o bob",
                "PART !ABCDEtest",
            ] {
                assert_eq!(read(&mut lines), expected);
            }
            socket.write_all(b":alice!u@h PART !ABCDEtest\r\n").unwrap();
        });
        let mut config =
            ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["!test".into()]);
        config.port = port;
        config.use_tls = false;
        let mut connection = Connection::connect(config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut joined, mut names, mut message, mut notice, mut parted) =
            (false, false, false, false, false);
        let mut sent = false;
        while Instant::now() < deadline && !parted {
            match connection.try_recv() {
                Some(Event::Joined { channel }) => {
                    assert_eq!(channel, "!ABCDEtest");
                    joined = true;
                }
                Some(Event::Names { channel, users }) if users.len() == 2 => {
                    assert_eq!(channel, "!ABCDEtest");
                    assert!(users.contains(&"@alice".into()));
                    assert!(users.contains(&"bob".into()));
                    names = true;
                }
                Some(Event::ChannelMessage {
                    channel,
                    sender,
                    text,
                    notice: is_notice,
                    ..
                }) => {
                    assert_eq!(channel, "!ABCDEtest");
                    assert_eq!(sender, "bob");
                    assert_eq!(text, if is_notice { "notice" } else { "hello" });
                    if is_notice {
                        notice = true;
                    } else {
                        message = true;
                    }
                }
                Some(Event::Parted { channel }) => {
                    assert_eq!(channel, "!ABCDEtest");
                    parted = true;
                }
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(5)),
            }
            if joined && names && message && notice && !sent {
                connection
                    .send_message("!ABCDEtest", "reply", false)
                    .unwrap();
                connection
                    .send_message("!ABCDEtest", "notice", true)
                    .unwrap();
                connection
                    .send_member_command(
                        "bob",
                        MemberCommand::Invite {
                            channel: "!ABCDEtest".into(),
                        },
                    )
                    .unwrap();
                connection
                    .send_member_command(
                        "bob",
                        MemberCommand::GiveOp {
                            channel: "!ABCDEtest".into(),
                        },
                    )
                    .unwrap();
                connection
                    .send_command("/part", Some("!ABCDEtest"))
                    .unwrap();
                sent = true;
            }
        }
        assert!(
            joined && names && message && notice && parted,
            "safe channel lifecycle incomplete"
        );
        server.join().unwrap();
    }

    #[test]
    fn local_server_registration_join_receive_and_send() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            let mut registration = Vec::new();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                registration.push(line.trim_end().to_owned());
                if line.starts_with("USER ") {
                    break;
                }
            }
            // Without credentials there is no PASS, and USER carries the
            // configured username rather than the nickname.
            assert!(!registration.iter().any(|line| line.starts_with("PASS")));
            assert!(registration.contains(&"NICK alice".to_owned()));
            assert!(
                registration.last().unwrap().starts_with("USER ident1 0 * "),
                "{registration:?}"
            );
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
                .unwrap();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                if line.starts_with("JOIN #test") {
                    break;
                }
            }
            socket.write_all(b"PING :ping-token\r\n").unwrap();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                if line.starts_with("PONG ") {
                    assert_eq!(line.trim_end(), "PONG ping-token");
                    break;
                }
            }
            socket.write_all(
                b":alice!u@h JOIN #test\r\n:alice!u@h JOIN #other\r\n:server 353 alice = #other :alice\r\n:server 366 alice #other :End of NAMES\r\n:alice!u@h PRIVMSG #test :hello\r\n:server 353 alice = #test :@alice bob\r\n:server 366 alice #test :End of NAMES\r\n:charlie!u@h JOIN #test\r\n:alice!u@h MODE #test +o charlie\r\n:bob!u@h PART #test\r\n:charlie!u@h NICK dave\r\n:dave!u@h QUIT :bye\r\n"
            ).unwrap();
            let mut outgoing = Vec::new();
            while outgoing.len() < 7 {
                line.clear();
                lines.read_line(&mut line).unwrap();
                if line.starts_with("PRIVMSG ")
                    || line.starts_with("NOTICE ")
                    || line.starts_with("WHOIS ")
                    || line.starts_with("INVITE ")
                    || line.starts_with("MODE ")
                {
                    outgoing.push(line.trim_end().to_owned());
                }
            }
            outgoing
        });

        let mut config = ConnectionConfig::tls(
            "127.0.0.1".into(),
            "alice".into(),
            vec!["#test".into(), "#other".into()],
        );
        config.username = "ident1".into();
        config.port = port;
        config.use_tls = false;
        let mut connection = Connection::connect(config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen_registered = false;
        let mut seen_joined = false;
        let mut seen_message = false;
        let mut seen_names = false;
        let mut seen_roster_updated = false;
        let mut seen_nick_update = false;
        let mut seen_quit_update = false;
        let mut activities = Vec::new();
        let mut people = Vec::new();
        let mut rosters = Vec::new();
        let mut transcript = Vec::new();
        while Instant::now() < deadline
            && !(seen_registered
                && seen_joined
                && seen_message
                && seen_names
                && seen_roster_updated
                && seen_nick_update
                && seen_quit_update)
        {
            if let Some(event) = connection.try_recv() {
                match event {
                    Event::Registered { nickname } => seen_registered = nickname == "alice",
                    Event::Joined { channel } => seen_joined |= channel == "#test",
                    Event::ChannelMessage {
                        channel,
                        sender,
                        text,
                        ..
                    } => {
                        seen_message = channel == "#test" && sender == "alice" && text == "hello";
                    }
                    Event::Names { channel, users } => {
                        // charlie only ever joins #test, so its NICK and QUIT
                        // must not republish #other.
                        assert!(
                            channel != "#other"
                                || !activities.iter().any(|(actor, _)| actor == "charlie"),
                            "#other roster republished for an unrelated user"
                        );
                        rosters.push(users.clone());
                        seen_names |= channel == "#test"
                            && users.contains(&"@alice".to_owned())
                            && users.contains(&"bob".to_owned());
                        seen_roster_updated |= channel == "#test"
                            && users.contains(&"@alice".to_owned())
                            && users.contains(&"@charlie".to_owned())
                            && !users.contains(&"bob".to_owned());
                        seen_nick_update |= channel == "#test"
                            && users.contains(&"@dave".to_owned())
                            && !users.contains(&"@charlie".to_owned());
                        seen_quit_update |= channel == "#test" && users == ["@alice"];
                    }
                    Event::ChannelActivity {
                        channel,
                        actor,
                        kind,
                        ..
                    } if channel == "#test" => activities.push((actor, kind)),
                    Event::ChannelActivity {
                        channel,
                        actor,
                        kind: ChannelActivityKind::Quit { .. },
                        ..
                    } => panic!("{actor} quit shown in {channel} without being a member"),
                    Event::Wire {
                        direction, line, ..
                    } => transcript.push((direction, line)),
                    Event::Disconnected(reason) => panic!("unexpected disconnect: {reason}"),
                    event @ (Event::UserNickChanged { .. } | Event::UserQuit { .. }) => {
                        people.push(event)
                    }
                    _ => {}
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        // Other users' renames and quits, once each, for private
        // conversations.
        assert_eq!(
            people,
            [
                Event::UserNickChanged {
                    from: "charlie".into(),
                    to: "dave".into()
                },
                Event::UserQuit {
                    nickname: "dave".into(),
                    reason: Some("bye".into())
                },
            ]
        );
        assert!(
            seen_registered
                && seen_joined
                && seen_message
                && seen_names
                && seen_roster_updated
                && seen_nick_update
                && seen_quit_update,
            "rosters: {rosters:?}"
        );
        for expected in [
            (
                "alice",
                ChannelActivityKind::Joined {
                    mask: Some("u@h".into()),
                },
            ),
            (
                "charlie",
                ChannelActivityKind::Joined {
                    mask: Some("u@h".into()),
                },
            ),
            (
                "alice",
                ChannelActivityKind::ModeChanged {
                    modes: "+o charlie".into(),
                },
            ),
            ("bob", ChannelActivityKind::Left { reason: None }),
            (
                "dave",
                ChannelActivityKind::Quit {
                    reason: Some("bye".into()),
                },
            ),
        ] {
            assert!(
                activities
                    .iter()
                    .any(|(actor, kind)| actor == expected.0 && kind == &expected.1),
                "missing {expected:?}: {activities:?}"
            );
        }
        connection.send_message("#test", "outgoing", false).unwrap();
        connection.send_message("#test", "notice", true).unwrap();
        connection
            .send_private_message("charlie", "hello", false)
            .unwrap();
        connection
            .send_member_command("charlie", MemberCommand::Whois)
            .unwrap();
        connection
            .send_member_command(
                "charlie",
                MemberCommand::Invite {
                    channel: "#other".into(),
                },
            )
            .unwrap();
        connection
            .send_member_command(
                "charlie",
                MemberCommand::GiveOp {
                    channel: "#test".into(),
                },
            )
            .unwrap();
        connection
            .send_member_command(
                "charlie",
                MemberCommand::Deop {
                    channel: "#test".into(),
                },
            )
            .unwrap();
        let outgoing = server.join().unwrap();
        for expected in [
            "PRIVMSG charlie hello",
            "WHOIS charlie",
            "INVITE charlie #other",
            "MODE #test +o charlie",
            "MODE #test -o charlie",
        ] {
            assert!(
                outgoing.iter().any(|line| line == expected),
                "missing {expected}: {outgoing:?}"
            );
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && !transcript.iter().any(|(direction, line)| {
                *direction == WireDirection::Sent && line == "NOTICE #test notice"
            })
        {
            if let Some(Event::Wire {
                direction, line, ..
            }) = connection.try_recv()
            {
                transcript.push((direction, line));
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        for (direction, expected) in [
            (WireDirection::Sent, "CAP END"),
            (WireDirection::Sent, "JOIN #test"),
            (WireDirection::Received, "PING ping-token"),
            (WireDirection::Sent, "PONG ping-token"),
            (WireDirection::Sent, "PRIVMSG #test outgoing"),
            (WireDirection::Sent, "NOTICE #test notice"),
        ] {
            assert!(
                transcript
                    .iter()
                    .any(|(actual_direction, line)| *actual_direction == direction
                        && line == expected),
                "missing {direction:?} {expected}: {transcript:?}"
            );
        }
        assert!(
            transcript.iter().any(|(direction, line)| {
                *direction == WireDirection::Received
                    && line.starts_with(":alice!u@h PRIVMSG #test")
                    && line.ends_with("hello")
            }),
            "{transcript:?}"
        );
        assert!(
            outgoing.contains(&"PRIVMSG #test outgoing".to_owned()),
            "{outgoing:?}"
        );
        assert!(
            outgoing.contains(&"NOTICE #test notice".to_owned()),
            "{outgoing:?}"
        );
    }

    #[test]
    fn disconnect_during_tls_handshake_ends_immediately() {
        // The listener accepts but never answers the TLS ClientHello, so the
        // worker would otherwise wait for the full transport timeout.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || listener.accept().map(|(socket, _)| socket));

        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
        config.port = port;
        let mut connection = Connection::connect(config).unwrap();
        let _socket = server.join().unwrap().unwrap();
        connection.disconnect().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut reason = None;
        while Instant::now() < deadline && reason.is_none() {
            match connection.try_recv() {
                Some(Event::Disconnected(detail) | Event::Refused(detail)) => reason = Some(detail),
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(reason.as_deref(), Some(USER_DISCONNECT));
    }

    #[test]
    fn plaintext_server_password_requires_opt_in_and_sasl_still_requires_tls() {
        let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
        config.use_tls = false;
        config.server_password = Some("user/network:secret".into());
        assert!(config.validate().is_err());
        config.allow_plaintext_pass = true;
        config.validate().unwrap();
        config.sasl = Some(SaslCredentials {
            username: "account".into(),
            password: "secret".into(),
        });
        assert!(config.validate().is_err());
    }

    #[test]
    fn opted_in_plaintext_connection_sends_server_password() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut received = Vec::new();
            let mut line = String::new();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                received.push(line.trim_end().to_owned());
                if line.starts_with("USER ") {
                    break;
                }
            }
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
                .unwrap();
            received
        });

        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
        config.port = port;
        config.use_tls = false;
        config.server_password = Some("alice/net:secret".into());
        config.allow_plaintext_pass = true;
        let mut connection = Connection::connect(config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut registered = false;
        while Instant::now() < deadline && !registered {
            match connection.try_recv() {
                Some(Event::Registered { .. }) => registered = true,
                Some(Event::Disconnected(reason) | Event::Refused(reason)) => {
                    panic!("unexpected disconnect: {reason}")
                }
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(registered, "client did not register");
        let received = server.join().unwrap();
        assert!(
            received.contains(&"PASS alice/net:secret".to_owned()),
            "{received:?}"
        );
    }

    #[test]
    fn disconnect_flushes_quit_to_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                if line.starts_with("USER ") {
                    break;
                }
            }
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
                .unwrap();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                if line.starts_with("QUIT ") {
                    return line.trim_end().to_owned();
                }
            }
        });

        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
        config.port = port;
        config.use_tls = false;
        let mut connection = Connection::connect(config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut registered = false;
        while Instant::now() < deadline {
            if let Some(Event::Registered { .. }) = connection.try_recv() {
                registered = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(registered, "client did not register");
        connection.disconnect().unwrap();
        assert_eq!(server.join().unwrap(), "QUIT :Leaving CayenChat");
    }

    #[test]
    fn sasl_plain_waits_for_success_before_ending_cap_negotiation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let read = |reader: &mut BufReader<std::net::TcpStream>| {
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if !line.starts_with("PING ") {
                        break line.trim_end().to_owned();
                    }
                }
            };
            let initial = (0..4).map(|_| read(&mut lines)).collect::<Vec<_>>();
            assert!(initial.contains(&"CAP LS 302".to_owned()), "{initial:?}");
            assert!(
                initial.contains(&"PASS server-secret".to_owned()),
                "{initial:?}"
            );
            assert!(initial.contains(&"NICK alice".to_owned()), "{initial:?}");
            assert!(
                initial.iter().any(|line| line.starts_with("USER alice ")),
                "{initial:?}"
            );
            socket
                .write_all(b":server CAP alice LS :sasl=PLAIN,EXTERNAL\r\n")
                .unwrap();
            assert_eq!(read(&mut lines), "CAP REQ sasl");
            socket
                .write_all(b":server CAP alice ACK :sasl\r\n")
                .unwrap();
            assert_eq!(read(&mut lines), "AUTHENTICATE PLAIN");
            socket.write_all(b"AUTHENTICATE +\r\n").unwrap();
            let expected = base64::engine::general_purpose::STANDARD.encode(b"\0account\0secret");
            assert_eq!(read(&mut lines), format!("AUTHENTICATE {expected}"));
            socket
                .write_all(b":server 903 alice :SASL authentication successful\r\n")
                .unwrap();
            assert_eq!(read(&mut lines), "CAP END");
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
                .unwrap();
            assert_eq!(read(&mut lines), "JOIN #test");
            socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
            assert_eq!(read(&mut lines), "QUIT :Leaving CayenChat");
        });

        let mut config =
            ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#test".into()]);
        config.port = port;
        // This local fixture exercises the SASL protocol without a certificate. The public
        // constructor rejects this combination; production credentials require TLS.
        config.use_tls = false;
        config.server_password = Some("server-secret".into());
        config.sasl = Some(SaslCredentials {
            username: "account".into(),
            password: "secret".into(),
        });
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run(config, command_rx, event_tx));
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut registered = false;
        let mut joined = false;
        let mut transcript = Vec::new();
        while Instant::now() < deadline && !(registered && joined) {
            match event_rx.try_recv() {
                Ok(Event::Registered { nickname }) => registered = nickname == "alice",
                Ok(Event::Joined { channel }) => joined = channel == "#test",
                Ok(Event::Wire {
                    direction, line, ..
                }) => transcript.push((direction, line)),
                Ok(Event::Disconnected(reason)) => panic!("unexpected disconnect: {reason}"),
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(registered && joined);
        for (direction, expected) in [
            (WireDirection::Sent, "CAP LS 302"),
            (WireDirection::Sent, "PASS [redacted]"),
            (
                WireDirection::Received,
                ":server CAP alice LS sasl=PLAIN,EXTERNAL",
            ),
            (WireDirection::Sent, "CAP REQ sasl"),
            (WireDirection::Sent, "AUTHENTICATE PLAIN"),
            (WireDirection::Received, "AUTHENTICATE +"),
            (WireDirection::Sent, "AUTHENTICATE [redacted]"),
            (WireDirection::Sent, "CAP END"),
            (WireDirection::Sent, "JOIN #test"),
        ] {
            assert!(
                transcript
                    .iter()
                    .any(|(actual_direction, line)| *actual_direction == direction
                        && line == expected),
                "missing {direction:?} {expected}: {transcript:?}"
            );
        }
        assert!(!format!("{transcript:?}").contains("server-secret"));
        assert!(!format!("{transcript:?}").contains("account"));
        assert!(!format!("{transcript:?}").contains("secret"));
        command_tx.try_send(Outgoing::Quit).unwrap();
        server.join().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn sasl_failure_stops_registration() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                if line.starts_with("USER ") {
                    break;
                }
            }
            socket
                .write_all(b":server CAP alice LS :sasl=PLAIN\r\n")
                .unwrap();
            line.clear();
            lines.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), "CAP REQ sasl");
            socket
                .write_all(b":server CAP alice ACK :sasl\r\n")
                .unwrap();
            line.clear();
            lines.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), "AUTHENTICATE PLAIN");
            socket.write_all(b"AUTHENTICATE +\r\n").unwrap();
            line.clear();
            lines.read_line(&mut line).unwrap();
            assert!(line.starts_with("AUTHENTICATE "));
            socket
                .write_all(b":server 904 alice :Authentication failed\r\n")
                .unwrap();
        });

        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
        config.port = port;
        config.use_tls = false; // The private worker is tested without a certificate.
        config.sasl = Some(SaslCredentials {
            username: "account".into(),
            password: "wrong".into(),
        });
        let (_commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run(config, command_rx, event_tx));
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut failure = None;
        while Instant::now() < deadline {
            match event_rx.try_recv() {
                Ok(Event::Refused(reason)) => {
                    failure = Some(reason);
                    break;
                }
                Ok(Event::Disconnected(reason)) => panic!("retryable SASL failure: {reason}"),
                Ok(Event::Registered { .. }) => panic!("registered after SASL failure"),
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(failure.as_deref(), Some("SASL authentication failed."));
        server.join().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn password_mismatch_is_refused_instead_of_retryable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                if line.starts_with("USER ") {
                    break;
                }
            }
            socket
                .write_all(b":server 464 alice :Password incorrect\r\nERROR :Closing link\r\n")
                .unwrap();
        });

        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
        config.port = port;
        config.use_tls = false; // The private worker is tested without a certificate.
        config.server_password = Some("wrong".into());
        let (_commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run(config, command_rx, event_tx));
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut failure = None;
        while Instant::now() < deadline {
            match event_rx.try_recv() {
                Ok(Event::Refused(reason)) => {
                    failure = Some(reason);
                    break;
                }
                Ok(Event::Disconnected(reason)) => panic!("retryable 464: {reason}"),
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(
            failure.as_deref(),
            Some("Server rejected the password (464).")
        );
        server.join().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn nickname_in_use_waits_for_another_nickname() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                lines.read_line(&mut line).unwrap();
                if line.starts_with("USER ") {
                    break;
                }
            }
            socket
                .write_all(b":server 433 * alice :Nickname is already in use\r\n")
                .unwrap();
            line.clear();
            lines.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), "NICK alice_");
            socket
                .write_all(b":server 001 alice_ :Welcome\r\n")
                .unwrap();
            line.clear();
            let _ = lines.read_line(&mut line);
        });

        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
        config.port = port;
        config.use_tls = false; // The private worker is tested without a certificate.
        let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run(config, command_rx, event_tx));
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut rejected = None;
        let mut registered = None;
        while Instant::now() < deadline && registered.is_none() {
            match event_rx.try_recv() {
                Ok(Event::NicknameRejected { nickname }) => {
                    rejected = Some(nickname);
                    commands
                        .try_send(checked_command(IrcCommand::NICK("alice_".into())).unwrap())
                        .unwrap();
                }
                Ok(Event::Registered { nickname }) => registered = Some(nickname),
                Ok(Event::Disconnected(reason) | Event::Refused(reason)) => {
                    panic!("disconnected after 433: {reason}")
                }
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(rejected.as_deref(), Some("alice"));
        assert_eq!(registered.as_deref(), Some("alice_"));
        commands.try_send(Outgoing::Quit).unwrap();
        server.join().unwrap();
        worker.join().unwrap();
    }

    /// Reads client lines from a fixture socket, skipping PINGs.
    fn read_client_line(reader: &mut BufReader<std::net::TcpStream>) -> String {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if !line.starts_with("PING ") {
                break line.trim_end().to_owned();
            }
        }
    }

    /// Runs one connection against a fixture until `done` sees its events,
    /// then quits. Returns every event received.
    fn run_fixture(config: ConnectionConfig, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run(config, command_rx, event_tx));
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut received = Vec::new();
        while Instant::now() < deadline && !done(&received) {
            match event_rx.try_recv() {
                Ok(event) => {
                    if let Event::Disconnected(reason) | Event::Refused(reason) = &event {
                        panic!("unexpected end: {reason}");
                    }
                    received.push(event);
                }
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(done(&received), "{received:?}");
        command_tx.try_send(Outgoing::Quit).unwrap();
        worker.join().unwrap();
        received
    }

    fn plain_config(port: u16, ircv3: Ircv3Options) -> ConnectionConfig {
        let mut config =
            ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#test".into()]);
        config.port = port;
        config.use_tls = false;
        config.ircv3 = ircv3;
        config
    }

    fn channel_messages(events: &[Event]) -> Vec<(String, Option<SystemTime>)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::ChannelMessage {
                    text, server_time, ..
                } => Some((text.clone(), *server_time)),
                _ => None,
            })
            .collect()
    }

    const TAGGED_TRAFFIC: &[u8] =
        b"@time=2011-10-19T16:40:51.620Z;msgid=Ab1 :bob!u@h PRIVMSG #test :stamped\r\n\
@time=not-a-time;msgid= :bob!u@h PRIVMSG #test :invalid\r\n\
@+typing=active;time=2011-10-19T16:40:52.000Z :bob!u@h TAGMSG #test\r\n\
@+typing=active :bob!u@h TAGMSG alice\r\n\
@time=2011-10-19T16:40:53.000Z :bob!u@h PART #test :bye\r\n\
@time=2011-10-19T16:40:54.000Z :irc.example NOTICE alice :tagged server line\r\n\
:bob!u@h PRIVMSG #test :plain\r\n";

    fn history_options() -> Ircv3Options {
        Ircv3Options {
            chathistory: true,
            ..Ircv3Options::default()
        }
    }

    /// Answers registration for a server offering `offer`, acknowledging
    /// every request, then accepts the JOIN of #test.
    fn register_with(
        socket: &mut std::net::TcpStream,
        lines: &mut BufReader<std::net::TcpStream>,
        offer: &str,
        isupport: &str,
    ) -> Vec<String> {
        assert_eq!(read_client_line(lines), "CAP LS 302");
        let _nick = read_client_line(lines);
        let _user = read_client_line(lines);
        socket
            .write_all(format!(":srv CAP * LS :{offer}\r\n").as_bytes())
            .unwrap();
        let mut requested = Vec::new();
        loop {
            let line = read_client_line(lines);
            if line == "CAP END" {
                break;
            }
            let name = line.strip_prefix("CAP REQ ").unwrap().to_owned();
            socket
                .write_all(format!(":srv CAP * ACK :{name}\r\n").as_bytes())
                .unwrap();
            requested.push(name);
        }
        socket
            .write_all(
                format!(":srv 001 alice :Welcome\r\n:srv 005 alice {isupport} :are supported\r\n:srv 376 alice :End\r\n")
                    .as_bytes(),
            )
            .unwrap();
        assert_eq!(read_client_line(lines), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
        requested
    }

    #[test]
    fn chathistory_requests_the_latest_lines_on_join_and_reports_them_once() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let requested = register_with(
                &mut socket,
                &mut lines,
                "batch server-time message-tags draft/chathistory draft/event-playback",
                "CHATHISTORY=20 MSGREFTYPES=msgid,timestamp",
            );
            assert_eq!(
                requested,
                ["message-tags", "server-time", "batch", "draft/chathistory"]
            );
            assert_eq!(
                read_client_line(&mut lines),
                "CHATHISTORY LATEST #test * 20"
            );
            // A live line arrives before the reply and is also in it.
            socket
                .write_all(
                    b"@time=2026-09-27T23:58:31.000Z;msgid=live1 :bob!u@h PRIVMSG #test :alice: live during request\r\n\
@draft/chathistory-end :srv BATCH +r1 chathistory #test\r\n\
@batch=r1;time=2026-09-27T23:50:00.000Z;msgid=old1 :bob!u@h PRIVMSG #test :alice: old\r\n\
@batch=r1;time=2026-09-27T23:58:31.000Z;msgid=live1 :bob!u@h PRIVMSG #test :alice: live during request\r\n\
:srv BATCH -r1\r\n\
@time=2026-09-27T23:59:00.000Z;msgid=after1 :bob!u@h PRIVMSG #test :after\r\n",
                )
                .unwrap();
            let _ = read_client_line(&mut lines);
        });
        let events = run_fixture(plain_config(port, history_options()), |events| {
            channel_messages(events).len() == 2
                && events
                    .iter()
                    .any(|event| matches!(event, Event::ChannelHistory { .. }))
        });
        server.join().unwrap();
        let requested = events
            .iter()
            .position(|e| matches!(e, Event::HistoryRequested { channel, resumed: false } if channel == "#test"))
            .expect("request reported");
        let joined = events
            .iter()
            .position(|e| matches!(e, Event::Joined { .. }))
            .unwrap();
        assert!(joined < requested, "the channel exists before its request");
        let history: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::ChannelHistory {
                    channel,
                    messages,
                    incomplete: false,
                } => Some((channel, messages)),
                _ => None,
            })
            .collect();
        assert_eq!(history.len(), 1);
        let (channel, messages) = history[0];
        assert_eq!(channel, "#test");
        let lines: Vec<_> = messages
            .iter()
            .map(|m| (m.text.as_str(), m.msgid.as_deref()))
            .collect();
        assert_eq!(
            lines,
            [
                ("alice: old", Some("old1")),
                ("alice: live during request", Some("live1"))
            ]
        );
        assert!(messages.iter().all(|m| m.server_time.is_some()));
        // Reply lines never become live messages or server lines.
        let live: Vec<_> = channel_messages(&events)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        assert_eq!(live, ["alice: live during request", "after"]);
        assert!(!events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("BATCH") || line.contains("old"))));
    }

    #[test]
    fn user_accounts_follow_joins_account_changes_and_one_whox_query() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let requested = register_with(
                &mut socket,
                &mut lines,
                "account-notify extended-join",
                "WHOX",
            );
            assert_eq!(requested, ["account-notify", "extended-join"]);
            socket
                .write_all(
                    b":srv 353 alice = #test :alice bob carol\r\n:srv 366 alice #test :End\r\n",
                )
                .unwrap();
            // Members present before us: one WHOX query, its reply consumed.
            assert_eq!(read_client_line(&mut lines), "WHO #test %tnar,1");
            socket
                .write_all(
                    b":srv 354 alice 1 bob bob-acct :Bob Builder\r\n\
:srv 354 alice 1 carol 0 :Carol\r\n\
:srv 315 alice #test :End of /WHO list\r\n\
:dave!u@h JOIN #test dave-acct :Dave D\r\n\
:bob!u@h ACCOUNT *\r\n\
:carol!u@h ACCOUNT carol-acct\r\n\
:carol!u@h NICK carla\r\n\
:dave!u@h QUIT :bye\r\n",
                )
                .unwrap();
            let _ = read_client_line(&mut lines);
        });
        let options = Ircv3Options {
            accounts: true,
            ..Ircv3Options::default()
        };
        let events = run_fixture(plain_config(port, options), |events| {
            events.iter().any(
                |e| matches!(e, Event::UserAccountForgotten { nickname } if nickname == "dave"),
            )
        });
        server.join().unwrap();
        let tracked: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                Event::UserAccount {
                    nickname,
                    account,
                    realname,
                } => Some(format!(
                    "{nickname} {} {}",
                    account.as_deref().unwrap_or("-"),
                    realname.as_deref().unwrap_or("-")
                )),
                Event::UserAccountForgotten { nickname } => Some(format!("{nickname} gone")),
                _ => None,
            })
            .collect();
        assert_eq!(
            tracked,
            [
                "bob bob-acct Bob Builder",
                "carol - Carol",
                "dave dave-acct Dave D",
                "bob - Bob Builder",
                "carol carol-acct Carol",
                "carol gone",
                "carla carol-acct Carol",
                "dave gone",
            ]
        );
        // The WHOX reply and account lines are not server lines.
        assert!(!events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("354") || line.contains("Bob Builder"))));
    }

    #[test]
    fn user_accounts_are_not_tracked_unless_asked_for() {
        let (port, server) = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut lines = BufReader::new(socket.try_clone().unwrap());
                // Nothing to negotiate: registration goes without CAP.
                assert_eq!(read_client_line(&mut lines), "CAP END");
                let _ = (read_client_line(&mut lines), read_client_line(&mut lines));
                socket
                    .write_all(b":srv 001 alice :Welcome\r\n:srv 005 alice WHOX :ok\r\n:srv 376 alice :End\r\n")
                    .unwrap();
                assert_eq!(read_client_line(&mut lines), "JOIN #test");
                socket
                    .write_all(b":alice!u@h JOIN #test\r\n:bob!u@h JOIN #test b :B\r\n:bob!u@h PRIVMSG #test :hi\r\n")
                    .unwrap();
                // No WHO follows: the next line is the QUIT.
                assert!(read_client_line(&mut lines).starts_with("QUIT"));
            });
            (port, server)
        };
        let events = run_fixture(plain_config(port, Ircv3Options::default()), |events| {
            channel_messages(events).len() == 1
        });
        server.join().unwrap();
        assert!(!events.iter().any(|e| matches!(
            e,
            Event::UserAccount { .. } | Event::UserAccountForgotten { .. }
        )));
    }

    /// Like [`run_fixture`], but `step` may queue commands as events arrive;
    /// it returns `true` once everything expected has been received.
    fn run_fixture_driven(
        config: ConnectionConfig,
        mut step: impl FnMut(&[Event], &mpsc::Sender<Outgoing>) -> bool,
    ) -> Vec<Event> {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run(config, command_rx, event_tx));
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut received = Vec::new();
        let mut done = false;
        while Instant::now() < deadline && !done {
            match event_rx.try_recv() {
                Ok(event) => {
                    if let Event::Disconnected(reason) | Event::Refused(reason) = &event {
                        panic!("unexpected end: {reason}");
                    }
                    received.push(event);
                    done = step(&received, &command_tx);
                }
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(done, "{received:?}");
        command_tx.try_send(Outgoing::Quit).unwrap();
        worker.join().unwrap();
        received
    }

    fn older_request(request: u64, msgid: &str) -> Outgoing {
        Outgoing::OlderHistory {
            channel: "#test".into(),
            request,
            reference: MessageReference {
                msgid: Some(msgid.into()),
                time: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_550_000)),
            },
            limit: 100,
        }
    }

    #[test]
    fn older_history_asks_before_the_oldest_message_and_reports_the_page_once() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            register_with(
                &mut socket,
                &mut lines,
                "batch server-time message-tags draft/chathistory",
                "CHATHISTORY=20 MSGREFTYPES=msgid,timestamp",
            );
            assert_eq!(
                read_client_line(&mut lines),
                "CHATHISTORY LATEST #test * 20"
            );
            socket
                .write_all(
                    b":srv BATCH +r chathistory #test
@batch=r;time=2026-09-27T23:50:00.000Z;msgid=m5 :bob!u@h PRIVMSG #test :five
:srv BATCH -r
",
                )
                .unwrap();
            assert_eq!(
                read_client_line(&mut lines),
                "CHATHISTORY BEFORE #test msgid=m5 20"
            );
            // A live line arrives while the page is on its way.
            socket
                .write_all(
                    b"@time=2026-09-27T23:59:00.000Z;msgid=live :carol!u@h PRIVMSG #test :live meanwhile
@draft/chathistory-end :srv BATCH +o chathistory #test
@batch=o;time=2026-09-27T23:40:00.000Z;msgid=m3 :bob!u@h PRIVMSG #test :three
@batch=o;time=2026-09-27T23:45:00.000Z;msgid=m4 :bob!u@h NOTICE #test :four
:srv BATCH -o
",
                )
                .unwrap();
            assert!(read_client_line(&mut lines).starts_with("QUIT"));
        });
        let mut asked = false;
        let events =
            run_fixture_driven(plain_config(port, history_options()), |events, commands| {
                if !asked
                    && events
                        .iter()
                        .any(|e| matches!(e, Event::ChannelHistory { .. }))
                {
                    asked = true;
                    commands.try_send(older_request(42, "m5")).unwrap();
                }
                channel_messages(events).len() == 1
                    && events
                        .iter()
                        .any(|e| matches!(e, Event::OlderChannelHistory { .. }))
            });
        server.join().unwrap();
        let available = events
            .iter()
            .position(|e| matches!(e, Event::HistoryAvailable(true)))
            .expect("availability reported");
        let joined = events
            .iter()
            .position(|e| matches!(e, Event::Joined { .. }))
            .unwrap();
        assert!(available < joined);
        let pages: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::OlderChannelHistory {
                    channel,
                    request,
                    messages,
                    status,
                } => Some((channel, *request, messages, *status)),
                _ => None,
            })
            .collect();
        assert_eq!(pages.len(), 1);
        let (channel, request, messages, status) = &pages[0];
        assert_eq!((channel.as_str(), *request), ("#test", 42));
        assert_eq!(*status, OlderHistoryStatus::Beginning);
        let lines: Vec<_> = messages
            .iter()
            .map(|m| (m.text.as_str(), m.notice, m.msgid.as_deref()))
            .collect();
        assert_eq!(
            lines,
            [("three", false, Some("m3")), ("four", true, Some("m4"))]
        );
        // The live line stayed live; the page never did; no second reservation.
        assert_eq!(
            channel_messages(&events)
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>(),
            ["live meanwhile"]
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::HistoryRequested { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn older_history_fails_at_once_without_chathistory() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            register_with(&mut socket, &mut lines, "batch server-time", "CHANTYPES=#");
            // Nothing is asked of the server: the next line is QUIT.
            assert!(read_client_line(&mut lines).starts_with("QUIT"));
        });
        let mut asked = false;
        let events =
            run_fixture_driven(plain_config(port, history_options()), |events, commands| {
                if !asked && events.iter().any(|e| matches!(e, Event::Joined { .. })) {
                    asked = true;
                    commands.try_send(older_request(7, "m1")).unwrap();
                }
                events
                    .iter()
                    .any(|e| matches!(e, Event::OlderChannelHistory { .. }))
            });
        server.join().unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::HistoryAvailable(_)))
        );
        assert!(events.iter().any(|e| matches!(e,
            Event::OlderChannelHistory { request: 7, status: OlderHistoryStatus::Failed, messages, .. }
            if messages.is_empty())));
    }

    #[test]
    fn a_reconnect_asks_only_for_the_lines_after_the_cut() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            register_with(
                &mut socket,
                &mut lines,
                "batch server-time message-tags draft/chathistory",
                "CHATHISTORY=2 MSGREFTYPES=msgid,timestamp",
            );
            assert_eq!(
                read_client_line(&mut lines),
                "CHATHISTORY LATEST #test msgid=m5 2"
            );
            // Live traffic after our JOIN, then the missed lines (the
            // server repeats the live one), and exactly as many as asked.
            socket
                .write_all(
                    b"@time=2026-09-28T00:10:00.000Z;msgid=m8 :bob!u@h PRIVMSG #test :live
:srv BATCH +r chathistory #test
@batch=r;time=2026-09-28T00:05:00.000Z;msgid=m7 :bob!u@h PRIVMSG #test :missed
@batch=r;time=2026-09-28T00:10:00.000Z;msgid=m8 :bob!u@h PRIVMSG #test :live
:srv BATCH -r
",
                )
                .unwrap();
            assert!(read_client_line(&mut lines).starts_with("QUIT"));
        });
        let mut config = plain_config(port, history_options());
        config.resume_history = vec![HistoryResume {
            channel: "#TEST".into(),
            after: MessageReference {
                msgid: Some("m5".into()),
                time: None,
            },
        }];
        let events = run_fixture(config, |events| {
            events
                .iter()
                .any(|event| matches!(event, Event::ChannelHistory { .. }))
        });
        server.join().unwrap();
        assert!(events.iter().any(|event| matches!(event,
            Event::HistoryRequested { channel, resumed: true } if channel == "#test")));
        let Some(Event::ChannelHistory {
            messages,
            incomplete,
            ..
        }) = events
            .iter()
            .find(|event| matches!(event, Event::ChannelHistory { .. }))
        else {
            unreachable!()
        };
        let texts: Vec<_> = messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["missed", "live"]);
        assert!(
            *incomplete,
            "as many lines as asked for: more may be missing"
        );
        assert_eq!(
            channel_messages(&events)
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>(),
            ["live"]
        );
    }

    #[test]
    fn chathistory_is_not_requested_from_servers_without_it() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let requested = register_with(
                &mut socket,
                &mut lines,
                "batch server-time message-tags",
                "CHANTYPES=#",
            );
            assert!(requested.is_empty(), "{requested:?}");
            socket
                .write_all(b":bob!u@h PRIVMSG #test :hello\r\n")
                .unwrap();
            // The next thing the client sends is its QUIT, not a request.
            assert!(read_client_line(&mut lines).starts_with("QUIT"));
        });
        let events = run_fixture(plain_config(port, history_options()), |events| {
            channel_messages(events).len() == 1
        });
        server.join().unwrap();
        assert!(!events.iter().any(|event| matches!(
            event,
            Event::HistoryRequested { .. } | Event::ChannelHistory { .. }
        )));
    }

    #[test]
    fn a_reply_cut_off_by_a_disconnect_reports_nothing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            register_with(&mut socket, &mut lines, "batch draft/chathistory", "");
            assert_eq!(
                read_client_line(&mut lines),
                "CHATHISTORY LATEST #test * 50"
            );
            socket
                .write_all(
                    b":srv BATCH +r chathistory #test\r\n@batch=r :bob!u@h PRIVMSG #test :partial\r\n",
                )
                .unwrap();
            drop(socket);
        });
        let (_command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
        let config = plain_config(port, history_options());
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run(config, command_rx, event_tx));
        });
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match event_rx.try_recv() {
                Ok(event) => {
                    let end = matches!(event, Event::Disconnected(_));
                    events.push(event);
                    if end {
                        break;
                    }
                }
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
        worker.join().unwrap();
        server.join().unwrap();
        assert!(
            matches!(events.last(), Some(Event::Disconnected(_))),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::HistoryRequested { .. }))
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            Event::ChannelHistory { .. } | Event::ChannelMessage { .. }
        )));
    }

    #[test]
    fn disabled_ircv3_sends_plain_cap_end_and_ignores_tags() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let initial: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
            assert_eq!(initial[0], "CAP END", "no CAP LS when nothing is enabled");
            assert!(!initial.iter().any(|line| line.contains("CAP REQ")));
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
            socket.write_all(TAGGED_TRAFFIC).unwrap();
            let _ = read_client_line(&mut lines);
        });
        let events = run_fixture(plain_config(port, Ircv3Options::default()), |events| {
            channel_messages(events).len() == 3
        });
        server.join().unwrap();
        let messages = channel_messages(&events);
        assert!(
            messages.iter().all(|(_, time)| time.is_none()),
            "{messages:?}"
        );
        assert_tagmsg_invisible(&events);
    }

    fn assert_tagmsg_invisible(events: &[Event]) {
        for event in events {
            match event {
                Event::Wire { .. } | Event::Diagnostic { .. } => {}
                other => assert!(
                    !format!("{other:?}").contains("TAGMSG")
                        && !format!("{other:?}").contains("typing"),
                    "TAGMSG leaked into {other:?}"
                ),
            }
        }
        assert!(events.iter().any(|event| matches!(event,
            Event::Wire { direction: WireDirection::Received, line, .. } if line.contains("TAGMSG"))));
    }

    #[test]
    fn server_time_is_negotiated_per_connection_and_reset_on_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            // First connection: server-time offered across continuation lines.
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let initial: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
            assert_eq!(initial[0], "CAP LS 302");
            socket
                .write_all(b":server CAP * LS * :multi-prefix sasl=PLAIN\r\n:server CAP * LS :server-time message-tags\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP REQ server-time");
            socket
                .write_all(b":server CAP * ACK :server-time\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP END");
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
            socket.write_all(TAGGED_TRAFFIC).unwrap();
            let _ = read_client_line(&mut lines);
            drop(socket);

            // Reconnect: a server without CAP that still sends time tags.
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            assert_eq!(read_client_line(&mut lines), "CAP LS 302");
            socket
                .write_all(b":server 421 * CAP :Unknown command\r\n")
                .unwrap();
            let _nick = read_client_line(&mut lines);
            let _user = read_client_line(&mut lines);
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
            socket.write_all(TAGGED_TRAFFIC).unwrap();
            let _ = read_client_line(&mut lines);
        });
        let options = Ircv3Options {
            message_tags: false,
            server_time: true,
            batch: false,
            metadata: false,
            peer_avatars: false,
            chathistory: false,
            accounts: false,
        };
        let events = run_fixture(plain_config(port, options), |events| {
            channel_messages(events).len() == 3
        });
        let stamp = |millis: u64| Some(SystemTime::UNIX_EPOCH + Duration::from_millis(millis));
        assert_eq!(
            channel_messages(&events),
            [
                ("stamped".to_owned(), stamp(1_319_042_451_620)),
                ("invalid".to_owned(), None),
                ("plain".to_owned(), None),
            ]
        );
        // An old timestamp alone does not make a live line replayed history.
        assert!(events.iter().any(|event| matches!(event,
            Event::ChannelMessage { text, replayed: false, server_time: Some(_), .. }
                if text == "stamped")));
        // The message ID travels with the event; an empty one is absent.
        assert!(events.iter().any(|event| matches!(event,
            Event::ChannelMessage { text, msgid: Some(id), .. } if text == "stamped" && id == "Ab1")));
        assert!(events.iter().any(|event| matches!(event,
            Event::ChannelMessage { text, msgid: None, .. } if text == "invalid")));
        assert!(events.iter().any(|event| matches!(event,
            Event::ChannelActivity { kind: ChannelActivityKind::Left { .. }, server_time, .. }
                if *server_time == stamp(1_319_042_453_000))));
        assert!(events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line == ":irc.example NOTICE alice :tagged server line")));
        assert_tagmsg_invisible(&events);

        let events = run_fixture(plain_config(port, options), |events| {
            channel_messages(events).len() == 3
        });
        server.join().unwrap();
        assert!(
            channel_messages(&events)
                .iter()
                .all(|(_, time)| time.is_none()),
            "a new connection does not inherit the previous negotiation"
        );
    }

    #[test]
    fn legacy_encoding_keeps_server_time_and_skips_message_tags() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let codec = encoding_from_whatwg_label("iso-2022-jp").unwrap();
        let body = codec
            // Encoded with the line ending so the encoder returns to ASCII.
            .encode("日本語の本文\r\n", EncoderTrap::Strict)
            .unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            assert_eq!(read_client_line(&mut lines), "CAP LS 302");
            socket
                .write_all(b":server CAP * LS :message-tags server-time\r\n")
                .unwrap();
            let requests: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
            assert!(
                requests.contains(&"CAP REQ server-time".to_owned()),
                "{requests:?}"
            );
            assert!(
                !requests.iter().any(|line| line.contains("message-tags")),
                "{requests:?}"
            );
            socket
                .write_all(b":server CAP * ACK :server-time\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP END");
            socket
                .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
            let mut line =
                b"@time=2011-10-19T16:40:51.620Z;msgid=abc :bob!u@h PRIVMSG #test :".to_vec();
            line.extend_from_slice(&body);
            socket.write_all(&line).unwrap();
            let _ = read_client_line(&mut lines);
        });
        let mut config = plain_config(
            port,
            Ircv3Options {
                message_tags: true,
                server_time: true,
                batch: false,
                metadata: false,
                peer_avatars: false,
                chathistory: false,
                accounts: false,
            },
        );
        config.encoding = "ISO-2022-JP".into();
        let events = run_fixture(config, |events| channel_messages(events).len() == 1);
        server.join().unwrap();
        assert_eq!(
            channel_messages(&events),
            [(
                "日本語の本文".to_owned(),
                Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_319_042_451_620))
            )]
        );
        assert!(events.iter().any(|event| matches!(event,
            Event::Diagnostic { message, .. } if message.contains("Message tags are not requested"))));
    }

    /// Channel message texts with their replayed flags, in arrival order.
    fn replay_flags(events: &[Event]) -> Vec<(String, bool)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::ChannelMessage { text, replayed, .. } => Some((text.clone(), *replayed)),
                _ => None,
            })
            .collect()
    }

    fn batch_server_lines(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::ServerLine(line) if line.contains("BATCH") => Some(line.clone()),
                _ => None,
            })
            .collect()
    }

    const BATCH_TRAFFIC: &[u8] = b":srv BATCH +H1 chathistory #test\r\n\
@batch=H1;time=2011-10-19T16:40:51.620Z :bob!u@h PRIVMSG #test :alice: old\r\n\
:bob!u@h PRIVMSG #test :alice: live between\r\n\
@batch=H1 :srv BATCH +n1 example.com/nested\r\n\
@batch=n1 :bob!u@h PRIVMSG #test :alice: nested old\r\n\
:srv BATCH +u1 example.com/unknown\r\n\
@batch=u1 :bob!u@h PRIVMSG #test :alice: unknown type\r\n\
@batch=h1 :bob!u@h PRIVMSG #test :alice: other case\r\n\
@batch=H1 :srv BATCH -n1\r\n\
:srv BATCH -u1\r\n\
:srv BATCH -H1\r\n\
@batch=H1 :bob!u@h PRIVMSG #test :alice: after end\r\n";

    #[test]
    fn negotiated_batch_marks_history_until_it_is_withdrawn() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            assert_eq!(read_client_line(&mut lines), "CAP LS 302");
            socket
                .write_all(b":srv CAP * LS :batch server-time message-tags draft/chathistory\r\n")
                .unwrap();
            let requests: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
            assert!(
                requests.contains(&"CAP REQ batch".to_owned()),
                "{requests:?}"
            );
            assert!(
                requests
                    .iter()
                    .all(|line| !line.starts_with("CAP REQ") || line == "CAP REQ batch"),
                "only the opted-in capability is requested: {requests:?}"
            );
            socket.write_all(b":srv CAP * ACK :batch\r\n").unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP END");
            socket
                .write_all(b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
            socket.write_all(BATCH_TRAFFIC).unwrap();
            // Withdrawing batch forgets a history batch left open.
            socket
                .write_all(
                    b":srv BATCH +h2 chathistory #test\r\n\
@batch=h2 :bob!u@h PRIVMSG #test :alice: before del\r\n\
:srv CAP alice DEL :batch\r\n\
@batch=h2 :bob!u@h PRIVMSG #test :alice: after del\r\n",
                )
                .unwrap();
            let _ = read_client_line(&mut lines);
            drop(socket);

            // Reconnect: batch is no longer offered; the old reference is
            // meaningless on the new connection.
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            assert_eq!(read_client_line(&mut lines), "CAP LS 302");
            socket.write_all(b":srv CAP * LS :server-time\r\n").unwrap();
            let requests: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
            assert!(requests.contains(&"CAP END".to_owned()), "{requests:?}");
            assert!(!requests.iter().any(|line| line.starts_with("CAP REQ")));
            socket
                .write_all(b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket
                .write_all(b":alice!u@h JOIN #test\r\n@batch=h2 :bob!u@h PRIVMSG #test :alice: stale reference\r\n")
                .unwrap();
            let _ = read_client_line(&mut lines);
        });
        let options = Ircv3Options {
            batch: true,
            ..Ircv3Options::default()
        };
        let events = run_fixture(plain_config(port, options), |events| {
            replay_flags(events).len() == 8
        });
        let owned = |pairs: &[(&str, bool)]| -> Vec<(String, bool)> {
            pairs
                .iter()
                .map(|(text, replayed)| ((*text).to_owned(), *replayed))
                .collect()
        };
        assert_eq!(
            replay_flags(&events),
            owned(&[
                ("alice: old", true),
                ("alice: live between", false),
                ("alice: nested old", true),
                ("alice: unknown type", false),
                ("alice: other case", false),
                ("alice: after end", false),
                ("alice: before del", true),
                ("alice: after del", false),
            ])
        );
        // History is still a mention; the app suppresses only its alerts.
        assert!(events.iter().any(|event| matches!(event,
            Event::ChannelMessage { text, mentioned: true, replayed: true, .. } if text == "alice: old")));
        // server-time was not requested, so its tag is ignored.
        assert!(
            channel_messages(&events)
                .iter()
                .all(|(_, time)| time.is_none())
        );
        // Negotiated framing stays out of the server log but in the transcript.
        let before_del = events
            .iter()
            .position(|event| matches!(event, Event::ServerLine(line) if line.contains("DEL")))
            .unwrap_or(events.len());
        assert!(batch_server_lines(&events[..before_del]).is_empty());
        assert!(events.iter().any(|event| matches!(event,
            Event::Wire { direction: WireDirection::Received, line, .. } if line.contains("BATCH +H1"))));

        let events = run_fixture(plain_config(port, options), |events| {
            replay_flags(events).len() == 1
        });
        server.join().unwrap();
        assert_eq!(
            replay_flags(&events),
            owned(&[("alice: stale reference", false)])
        );
    }

    #[test]
    fn unsolicited_history_batches_keep_their_compatibility_handling() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let initial: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
            assert_eq!(initial[0], "CAP END", "batch off: nothing is negotiated");
            socket
                .write_all(b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
            socket.write_all(BATCH_TRAFFIC).unwrap();
            let _ = read_client_line(&mut lines);
        });
        let events = run_fixture(plain_config(port, Ircv3Options::default()), |events| {
            replay_flags(events).len() == 6
        });
        server.join().unwrap();
        // Same classification as before the option existed.
        assert_eq!(
            replay_flags(&events),
            [
                ("alice: old", true),
                ("alice: live between", false),
                ("alice: nested old", true),
                ("alice: unknown type", false),
                ("alice: other case", false),
                ("alice: after end", false),
            ]
            .map(|(text, replayed)| (text.to_owned(), replayed))
        );
        // Unsolicited framing lines still reach the server log as before.
        assert_eq!(
            batch_server_lines(&events),
            [
                ":srv BATCH +H1 CHATHISTORY #test",
                ":srv BATCH +n1 EXAMPLE.COM/NESTED",
                ":srv BATCH +u1 EXAMPLE.COM/UNKNOWN",
                ":srv BATCH -n1",
                ":srv BATCH -u1",
                ":srv BATCH -H1",
            ]
        );
    }

    fn avatar_events(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::UserAvatar { nickname, url } => {
                    Some(format!("{nickname}={}", url.as_deref().unwrap_or("-")))
                }
                Event::AvatarMoved { from, to } => Some(format!("{from}->{to}")),
                Event::AvatarsReset => Some("reset".into()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn metadata_avatars_follow_subscription_updates_and_withdrawal() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            assert_eq!(read_client_line(&mut lines), "CAP LS 302");
            socket
                .write_all(b":srv CAP * LS :batch draft/metadata-2=max-subs=10 metadata-notify\r\n")
                .unwrap();
            let requests: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
            assert!(
                requests.contains(&"CAP REQ batch".to_owned()),
                "{requests:?}"
            );
            assert!(
                !requests.iter().any(|line| line.contains("metadata")),
                "metadata waits for batch: {requests:?}"
            );
            socket.write_all(b":srv CAP * ACK :batch\r\n").unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP REQ draft/metadata-2");
            socket
                .write_all(b":srv CAP * ACK :draft/metadata-2\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP END");
            // Our own metadata arrives in the registration burst.
            socket
                .write_all(
                    b":srv 001 alice :Welcome\r\n\
:srv BATCH +r metadata alice\r\n\
@batch=r :srv METADATA alice avatar * :https://example.com/me.png\r\n\
:srv BATCH -r\r\n\
:srv 376 alice :End\r\n",
                )
                .unwrap();
            // The subscription goes out at 001, before the configured JOIN.
            assert_eq!(read_client_line(&mut lines), "METADATA * SUB avatar");
            assert_eq!(read_client_line(&mut lines), "METADATA * GET avatar");
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket
                .write_all(
                    b":srv 770 alice avatar\r\n\
:alice!u@h JOIN #test\r\n\
:srv 353 alice = #test :alice bob @carol\r\n\
:srv 366 alice #test :End\r\n\
:srv BATCH +m metadata #test\r\n\
@batch=m :srv METADATA bob avatar * :https://example.com/bob/{size}\r\n\
@batch=m :srv METADATA carol display-name * :Carol\r\n\
@batch=m :srv METADATA #test avatar * :https://example.com/room.png\r\n\
:srv BATCH -m\r\n\
:srv 774 alice #test 1\r\n",
                )
                .unwrap();
            // After the server's delay the client asks once.
            assert_eq!(read_client_line(&mut lines), "METADATA #test SYNC");
            socket
                .write_all(
                    b":srv BATCH +s metadata #test\r\n\
@batch=s :srv METADATA carol avatar * :https://example.com/c.png\r\n\
:srv BATCH -s\r\n\
:bob!u@h METADATA bob avatar * :https://example.com/bob2.png\r\n\
:bob!u@h NICK bobby\r\n\
:carol!u@h PART #test :bye\r\n\
:bobby!u@h QUIT :gone\r\n\
:srv CAP alice DEL :batch\r\n",
                )
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP REQ -draft/metadata-2");
            socket
                .write_all(
                    b":srv CAP alice ACK :-draft/metadata-2\r\n\
:srv METADATA dave avatar * :https://example.com/late.png\r\n\
:dave!u@h PRIVMSG #test :after\r\n",
                )
                .unwrap();
            let _ = read_client_line(&mut lines);
        });
        let options = Ircv3Options {
            batch: true,
            metadata: true,
            ..Ircv3Options::default()
        };
        let events = run_fixture(plain_config(port, options), |events| {
            channel_messages(events).len() == 1
        });
        server.join().unwrap();
        assert_eq!(
            avatar_events(&events),
            [
                "alice=https://example.com/me.png",
                "bob=https://example.com/bob/{size}",
                "carol=https://example.com/c.png",
                "bob=https://example.com/bob2.png",
                "bob->bobby",
                "carol=-",
                "bobby=-",
                "reset",
            ]
        );
        // Metadata is neither chat nor server log; only the transcript has it.
        assert_eq!(channel_messages(&events).len(), 1);
        let reset = events
            .iter()
            .position(|event| matches!(event, Event::AvatarsReset))
            .unwrap();
        for event in &events[..reset] {
            if let Event::ServerLine(line) = event {
                assert!(
                    !line.contains("METADATA")
                        && !line.contains(" 770 ")
                        && !line.contains(" 774 "),
                    "{line}"
                );
            }
        }
        // After withdrawal a METADATA line is ordinary server traffic again.
        assert!(events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("late.png"))));
    }

    #[test]
    fn kvirc_peer_avatars_are_exchanged_by_url_only() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // The peer's lines are what KVIrc 5.2 sends (see peer_avatar.rs).
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            // No capability is needed.
            assert_eq!(read_client_line(&mut lines), "CAP END");
            assert_eq!(read_client_line(&mut lines), "NICK alice");
            let user = read_client_line(&mut lines);
            assert!(user.starts_with("USER alice 0 * "), "{user:?}");
            assert!(user.ends_with("\u{3}4\u{f}CayenChat"), "{user:?}");
            socket
                .write_all(b":srv.example 001 alice :Welcome\r\n:srv.example 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            // NAMES has no realname: nobody is asked about until they speak.
            socket
                .write_all(
                    b":alice!u@h JOIN #test\r\n\
:srv.example 353 alice = #test :alice kv @bob carol\r\n\
:srv.example 366 alice #test :End\r\n\
:kv!u@h PRIVMSG #test :hello\r\n\
:kv!u@h PRIVMSG alice :\x01AVATAR\x01\r\n",
                )
                .unwrap();
            let mut sent = [read_client_line(&mut lines), read_client_line(&mut lines)];
            sent.sort();
            assert_eq!(
                sent,
                [
                    "NOTICE kv :\u{1}AVATAR https://example.com/alice.png\u{1}".to_owned(),
                    "WHO kv".to_owned(),
                ]
            );
            socket
                .write_all(
                    b":srv.example 352 alice #test ~kv host srv.example kv H :0 \x034\x0fKVIrc 5.2\r\n\
:srv.example 315 alice kv :End of /WHO list.\r\n",
                )
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "PRIVMSG kv \u{1}AVATAR\u{1}");
            socket
                .write_all(
                    b":kv!u@h NOTICE alice :\x01AVATAR https://example.com/kv.png M\x01\r\n\
:bob!u@h NOTICE #test :\x01AVATAR bob.png 20480\x01\r\n\
:carol!u@h NOTICE alice :\x01AVATAR\x01\r\n\
:bob!u@h PRIVMSG #test :after\r\n",
                )
                .unwrap();
            // A file offer is never fetched by DCC.
            let next = read_client_line(&mut lines);
            assert!(
                !next.contains("DCC") && !next.contains("bob.png"),
                "{next:?}"
            );
        });
        let mut config = plain_config(
            port,
            Ircv3Options {
                peer_avatars: true,
                ..Ircv3Options::default()
            },
        );
        config.shared_avatar = Some("https://example.com/alice.png".into());
        let events = run_fixture(config, |events| {
            channel_messages(events).len() == 2 && !avatar_events(events).is_empty()
        });
        server.join().unwrap();
        assert_eq!(avatar_events(&events), ["kv=https://example.com/kv.png"]);
        assert_eq!(
            channel_messages(&events)
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>(),
            ["hello", "after"]
        );
        // Protocol traffic is neither chat nor server log.
        for event in &events {
            match event {
                Event::PrivateMessage { text, .. } => panic!("private row: {text:?}"),
                Event::ServerLine(line) => assert!(
                    !line.contains("AVATAR") && !line.contains(" 352 ") && !line.contains(" 315 "),
                    "{line}"
                ),
                _ => {}
            }
        }
    }

    #[test]
    fn ctcp_requests_are_answered_privately_and_shown_readably() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            assert_eq!(read_client_line(&mut lines), "CAP END");
            assert_eq!(read_client_line(&mut lines), "NICK alice");
            read_client_line(&mut lines);
            socket
                .write_all(b":srv.example 001 alice :Welcome\r\n:srv.example 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket
                .write_all(
                    b":alice!u@h JOIN #test\r\n\
:srv.example 353 alice = #test :alice bob carol\r\n\
:srv.example 366 alice #test :End\r\n\
:bob!u@h PRIVMSG alice :\x01VERSION\x01\r\n\
:carol!u@h PRIVMSG #test :\x01TIME\x01\r\n\
:bob!u@h PRIVMSG alice :\x01PING 1727490000\x01\r\n\
:bob!u@h PRIVMSG #test :\x01ACTION waves\x01\r\n\
:bob!u@h PRIVMSG #test :hello\r\n",
                )
                .unwrap();
            assert_eq!(
                read_client_line(&mut lines),
                concat!(
                    "NOTICE bob :\u{1}VERSION CayenChat ",
                    env!("CARGO_PKG_VERSION"),
                    "\u{1}"
                )
            );
            assert_eq!(
                read_client_line(&mut lines),
                "NOTICE bob :\u{1}PING 1727490000\u{1}"
            );
            // The channel request is not answered.
            let next = read_client_line(&mut lines);
            assert!(next.is_empty() || next.starts_with("QUIT"), "{next:?}");
        });
        let config = plain_config(port, Ircv3Options::default());
        let events = run_fixture(config, |events| channel_messages(events).len() == 2);
        server.join().unwrap();
        assert_eq!(
            channel_messages(&events)
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>(),
            ["\u{1}ACTION waves\u{1}", "hello"]
        );
        let shown: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::ServerLine(line) if line.contains("CTCP") => Some(line.as_str()),
                Event::PrivateMessage { text, .. } => panic!("private row: {text:?}"),
                _ => None,
            })
            .collect();
        assert_eq!(
            shown,
            [
                "CTCP VERSION request from bob",
                "CTCP TIME request from carol to #test (not answered)",
                "CTCP PING request from bob",
            ]
        );
    }

    #[test]
    fn peer_avatars_off_leaves_ctcp_avatar_unanswered() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            assert_eq!(read_client_line(&mut lines), "CAP END");
            assert_eq!(read_client_line(&mut lines), "NICK alice");
            // No mark, even with a URL configured.
            let user = read_client_line(&mut lines);
            assert!(user.ends_with(" CayenChat"), "{user:?}");
            socket
                .write_all(b":srv.example 001 alice :Welcome\r\n:srv.example 376 alice :End\r\n")
                .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket
                .write_all(
                    b":alice!u@h JOIN #test\r\n\
:srv.example 353 alice = #test :alice kv\r\n\
:srv.example 366 alice #test :End\r\n\
:kv!u@h PRIVMSG alice :\x01AVATAR\x01\r\n\
:kv!u@h PRIVMSG #test :hello\r\n",
                )
                .unwrap();
            // Nothing is answered or looked up.
            let next = read_client_line(&mut lines);
            assert!(next.is_empty() || next.starts_with("QUIT"), "{next:?}");
        });
        let mut config = plain_config(port, Ircv3Options::default());
        config.shared_avatar = Some("https://example.com/alice.png".into());
        let events = run_fixture(config, |events| channel_messages(events).len() == 1);
        server.join().unwrap();
        assert!(avatar_events(&events).is_empty());
        assert!(events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line == "CTCP AVATAR request from kv (not answered)")));
    }

    #[test]
    fn the_configured_realname_is_registered_with_the_avatar_mark_only_on_the_wire() {
        let mut config = plain_config(6667, Ircv3Options::default());
        assert_eq!(
            config.wire_realname(),
            "CayenChat",
            "unset keeps the default"
        );
        config.realname = "   ".into();
        assert_eq!(config.wire_realname(), "CayenChat", "blank is unset");
        config.realname = " Alice Liddell ".into();
        assert_eq!(config.wire_realname(), "Alice Liddell");
        config.ircv3.peer_avatars = true;
        config.shared_avatar = Some("https://example.com/a.png".into());
        assert_eq!(config.wire_realname(), "\u{3}4\u{f}Alice Liddell");
        assert_eq!(
            config.realname, " Alice Liddell ",
            "the setting stays clean"
        );
        assert_eq!(
            peer_avatar::without_mark(&config.wire_realname()),
            "Alice Liddell"
        );
        assert!(config.validate().is_ok());
        config.realname = "a\r\nQUIT".into();
        assert!(config.validate().is_err());
        assert!(
            Connection::connect(config)
                .err()
                .is_some_and(|error| error.contains("line breaks"))
        );
    }

    /// Serves registration with the capabilities in `offer`, then runs
    /// `script` and quits. Returns the client's USER line.
    fn setname_server(
        offer: &'static str,
        script: impl FnOnce(&mut dyn FnMut(&str), &mut BufReader<std::net::TcpStream>) + Send + 'static,
    ) -> (u16, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut send = |text: &str| socket.write_all(text.as_bytes()).unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP LS 302");
            send(&format!(":srv CAP * LS :{offer}\r\n"));
            let registration: Vec<String> = (0..2).map(|_| read_client_line(&mut lines)).collect();
            let mut user = registration
                .iter()
                .find(|line| line.starts_with("USER "))
                .cloned();
            loop {
                let line = read_client_line(&mut lines);
                if let Some(cap) = line.strip_prefix("CAP REQ ") {
                    send(&format!(":srv CAP * ACK :{cap}\r\n"));
                } else if line == "CAP END" {
                    break;
                } else if line.starts_with("USER ") {
                    user = Some(line);
                }
            }
            send(":srv 001 alice :Welcome\r\n");
            script(&mut send, &mut lines);
            user.expect("USER was sent")
        });
        (port, server)
    }

    fn wait_for(connection: &mut Connection, want: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            match connection.try_recv() {
                Some(event) if want(&event) => return event,
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("event did not arrive");
    }

    fn setname_options() -> Ircv3Options {
        Ircv3Options {
            server_time: true,
            ..Ircv3Options::default()
        }
    }

    #[test]
    fn setname_changes_the_realname_and_the_server_confirms_it() {
        let (port, server) = setname_server("server-time setname", |send, lines| {
            assert_eq!(
                read_client_line(lines),
                "SETNAME :\u{3}4\u{f}Bob Builder",
                "the avatar mark of this connection is kept"
            );
            send(":alice!u@h SETNAME :\u{3}4\u{f}Bob Builder\r\n");
            // Another user's change is not ours and not shown.
            send(":bob!u@h SETNAME :Other\r\n");
            assert_eq!(read_client_line(lines), "SETNAME \u{3}4\u{f}Again");
            send(":alice!u@h SETNAME :\u{3}4\u{f}Again\r\n");
            let _ = read_client_line(lines);
        });
        let mut config = plain_config(port, setname_options());
        config.realname = "Alice".into();
        config.ircv3.peer_avatars = true;
        config.shared_avatar = Some("https://example.com/a.png".into());
        let mut connection = Connection::connect(config).unwrap();
        wait_for(&mut connection, |event| {
            matches!(event, Event::Registered { .. })
        });
        connection.set_real_name("Bob Builder").unwrap();
        assert_eq!(
            wait_for(&mut connection, |event| matches!(
                event,
                Event::RealNameChanged { .. }
            )),
            Event::RealNameChanged {
                realname: "Bob Builder".into()
            }
        );
        connection.set_real_name("Again").unwrap();
        assert_eq!(
            wait_for(&mut connection, |event| matches!(
                event,
                Event::RealNameChanged { .. }
            )),
            Event::RealNameChanged {
                realname: "Again".into()
            }
        );
        connection.disconnect().unwrap();
        assert_eq!(server.join().unwrap(), "USER alice 0 * \u{3}4\u{f}Alice");
    }

    #[test]
    fn setname_without_the_capability_is_reported_at_once() {
        let (port, server) = setname_server("server-time", |_, lines| {
            // Nothing may reach the server; the next line is the QUIT.
            assert!(read_client_line(lines).starts_with("QUIT"));
        });
        let mut config = plain_config(port, setname_options());
        config.realname = "Alice".into();
        let mut connection = Connection::connect(config).unwrap();
        wait_for(&mut connection, |event| {
            matches!(event, Event::Registered { .. })
        });
        connection.set_real_name("Bob").unwrap();
        assert_eq!(
            wait_for(&mut connection, |event| matches!(
                event,
                Event::RealNameFailed(_)
            )),
            Event::RealNameFailed(RealNameFailure::Unsupported)
        );
        connection.disconnect().unwrap();
        assert_eq!(server.join().unwrap(), "USER alice 0 * Alice");
    }

    #[test]
    fn a_rejected_setname_carries_the_servers_reason() {
        let (port, server) = setname_server("setname", |send, lines| {
            assert_eq!(read_client_line(lines), "SETNAME Bob");
            send("FAIL SETNAME INVALID_REALNAME :Real name is not valid\r\n");
            assert!(read_client_line(lines).starts_with("QUIT"));
        });
        let mut connection = Connection::connect(plain_config(port, setname_options())).unwrap();
        wait_for(&mut connection, |event| {
            matches!(event, Event::Registered { .. })
        });
        assert!(connection.set_real_name("a\nb").is_err());
        connection.set_real_name("Bob").unwrap();
        assert_eq!(
            wait_for(&mut connection, |event| matches!(
                event,
                Event::RealNameFailed(_)
            )),
            Event::RealNameFailed(RealNameFailure::Rejected("Real name is not valid".into()))
        );
        connection.disconnect().unwrap();
        assert_eq!(server.join().unwrap(), "USER alice 0 * CayenChat");
    }

    #[test]
    fn own_avatar_requests_and_later_joiners_follow_the_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // The server answers like Ergo 2.19 does (see spec/development.md).
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut send = |text: &str| socket.write_all(text.as_bytes()).unwrap();
            assert_eq!(read_client_line(&mut lines), "CAP LS 302");
            send(":srv CAP * LS :batch draft/metadata-2=before-connect,max-subs=10\r\n");
            let _: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
            send(":srv CAP * ACK :batch\r\n");
            assert_eq!(read_client_line(&mut lines), "CAP REQ draft/metadata-2");
            send(":srv CAP * ACK :draft/metadata-2\r\n");
            assert_eq!(read_client_line(&mut lines), "CAP END");
            send(
                ":srv 001 alice :Welcome\r\n:srv BATCH +r metadata alice\r\n:srv BATCH -r\r\n:srv 376 alice :End\r\n",
            );
            assert_eq!(read_client_line(&mut lines), "METADATA * SUB avatar");
            assert_eq!(read_client_line(&mut lines), "METADATA * GET avatar");
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            send(
                ":srv 770 alice avatar\r\n:srv 774 * *ALL 0 :Try again later\r\n\
:srv BATCH +g metadata alice\r\n@batch=g :srv 766 alice alice avatar :Key is not set\r\n:srv BATCH -g\r\n\
:alice!u@h JOIN #test\r\n:srv 353 alice = #test :alice bob\r\n:srv 366 alice #test :End\r\n",
            );
            // Publishing, confirmed with the value the server kept.
            assert_eq!(
                read_client_line(&mut lines),
                "METADATA * SET avatar https://example.com/me.png"
            );
            send(":srv 761 alice alice avatar * :https://example.com/me.png\r\n");
            // A user who joins later is looked up after a pause; members
            // from NAMES are not.
            send(":dave!u@h JOIN #test\r\n");
            assert_eq!(read_client_line(&mut lines), "METADATA dave GET avatar");
            send(
                ":srv BATCH +d metadata dave\r\n@batch=d :srv 761 alice dave avatar * :https://example.com/d.png\r\n:srv BATCH -d\r\n",
            );
            // Removal touches only the avatar key.
            assert_eq!(read_client_line(&mut lines), "METADATA * SET avatar");
            send(":srv 766 alice alice avatar :Key deleted\r\n");
            assert_eq!(
                read_client_line(&mut lines),
                "METADATA * SET avatar https://example.com/second.png"
            );
            send("FAIL METADATA INVALID_VALUE avatar :Value is too long\r\n");
            send(":bob!u@h PRIVMSG #test :done\r\n");
            let _ = read_client_line(&mut lines);
        });
        let mut connection = Connection::connect(plain_config(
            port,
            Ircv3Options {
                batch: true,
                metadata: true,
                ..Ircv3Options::default()
            },
        ))
        .unwrap();
        // Refused on this connection until registration and the capability.
        connection
            .set_own_avatar(1, Some("https://example.com/early.png"))
            .unwrap();
        assert!(connection.set_own_avatar(9, Some("bad\r\nQUIT")).is_err());
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut seen = Vec::new();
        let mut names = false;
        let mut ready = false;
        while Instant::now() < deadline {
            let Some(event) = connection.try_recv() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            match &event {
                Event::MetadataReady => ready = true,
                Event::Names { .. } if ready && !names => {
                    names = true;
                    connection
                        .set_own_avatar(2, Some("https://example.com/me.png"))
                        .unwrap();
                }
                Event::UserAvatar { nickname, .. } if nickname == "dave" => {
                    connection.set_own_avatar(3, None).unwrap();
                }
                Event::OwnAvatar {
                    request: Some(3), ..
                } => connection
                    .set_own_avatar(4, Some("https://example.com/second.png"))
                    .unwrap(),
                Event::ChannelMessage { .. } => {
                    seen.push(event);
                    break;
                }
                _ => {}
            }
            seen.push(event);
        }
        connection.disconnect().unwrap();
        server.join().unwrap();
        let own: Vec<_> = seen
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::OwnAvatar { .. } | Event::OwnAvatarFailed { .. }
                )
            })
            .cloned()
            .collect();
        assert_eq!(
            own,
            [
                Event::OwnAvatarFailed {
                    request: 1,
                    failure: AvatarRequestFailure::Unavailable
                },
                Event::OwnAvatar {
                    url: None,
                    request: None
                },
                Event::OwnAvatar {
                    url: Some("https://example.com/me.png".into()),
                    request: Some(2)
                },
                Event::OwnAvatar {
                    url: None,
                    request: Some(3)
                },
                Event::OwnAvatarFailed {
                    request: 4,
                    failure: AvatarRequestFailure::Rejected {
                        code: "INVALID_VALUE".into(),
                        description: "Value is too long".into()
                    }
                },
            ]
        );
        assert_eq!(
            avatar_events(&seen),
            [
                "alice=https://example.com/me.png",
                "dave=https://example.com/d.png",
                "alice=-",
            ]
        );
        // Metadata never becomes a chat row or a server-log line.
        assert!(!seen.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("METADATA") || line.contains(" 76"))));
    }

    #[test]
    fn metadata_without_batch_brings_batch_along_or_stays_off() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for metadata in [false, true] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut lines = BufReader::new(socket.try_clone().unwrap());
                let first = read_client_line(&mut lines);
                if !metadata {
                    // Everything off: the plain registration of old.
                    assert_eq!(first, "CAP END");
                } else {
                    assert_eq!(first, "CAP LS 302");
                    socket
                        .write_all(b":srv CAP * LS :batch draft/metadata-2 server-time\r\n")
                        .unwrap();
                    let requests: Vec<String> =
                        (0..4).map(|_| read_client_line(&mut lines)).collect();
                    assert!(
                        requests.contains(&"CAP REQ server-time".to_owned()),
                        "{requests:?}"
                    );
                    assert!(
                        requests.contains(&"CAP REQ batch".to_owned()),
                        "{requests:?}"
                    );
                    // Batch declined: metadata is never requested alone.
                    socket
                        .write_all(b":srv CAP * ACK :server-time\r\n:srv CAP * NAK :batch\r\n")
                        .unwrap();
                    assert_eq!(read_client_line(&mut lines), "CAP END");
                }
                socket
                    .write_all(
                        b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n\
:srv METADATA bob avatar * :https://example.com/b.png\r\n\
:bob!u@h PRIVMSG #test :hi\r\n",
                    )
                    .unwrap();
                loop {
                    let next = read_client_line(&mut lines);
                    assert!(!next.starts_with("METADATA"), "{next}");
                    if next == "JOIN #test" {
                        break;
                    }
                }
                let _ = read_client_line(&mut lines);
            }
        });
        for options in [
            Ircv3Options::default(),
            // Metadata with batch off: batch is asked for as its
            // prerequisite; declined, so no metadata either.
            Ircv3Options {
                server_time: true,
                metadata: true,
                ..Ircv3Options::default()
            },
        ] {
            let events = run_fixture(plain_config(port, options), |events| {
                channel_messages(events).len() == 1
            });
            assert!(avatar_events(&events).is_empty());
        }
        server.join().unwrap();
    }
}
