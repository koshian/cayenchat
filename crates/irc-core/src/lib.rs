//! IRC transport adapter. Third-party IRC types stay inside this crate.

use std::{
    collections::HashMap,
    fmt, thread,
    time::{Duration, Instant},
};

use base64::Engine;
use encoding::{EncoderTrap, label::encoding_from_whatwg_label};
use futures_util::StreamExt;
use irc::{
    client::{
        data::user::AccessLevel,
        prelude::{Client, Config},
    },
    proto::{
        CapSubCommand, Capability, Command as IrcCommand, Message as IrcMessage, Response,
        mode::Mode,
    },
};
use tokio::sync::mpsc;

const COMMAND_CAPACITY: usize = 128;
const EVENT_CAPACITY: usize = 512;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Servers may hold registration until their ident (RFC 1413) and DNS lookups
/// finish. IRCnet waits about 30 seconds when the client's port 113 silently
/// drops packets, so the limit must comfortably exceed that.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(90);
/// Limits on WHOIS replies that have not reached end-of-WHOIS (318), so a
/// hostile server cannot grow memory by never finishing them.
const MAX_PENDING_WHOIS: usize = 32;
const MAX_WHOIS_ITEMS: usize = 512;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaslCredentials {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub nickname: String,
    pub channels: Vec<String>,
    pub use_tls: bool,
    pub verify_tls_certificates: bool,
    pub encoding: String,
    pub server_password: Option<String>,
    pub sasl: Option<SaslCredentials>,
}

impl ConnectionConfig {
    pub fn tls(host: String, nickname: String, channels: Vec<String>) -> Self {
        Self {
            host,
            port: 6697,
            nickname,
            channels,
            use_tls: true,
            verify_tls_certificates: true,
            encoding: "UTF-8".into(),
            server_password: None,
            sasl: None,
        }
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
        for channel in &self.channels {
            if !valid_channel(channel) {
                return Err(format!("Invalid channel name: {channel}"));
            }
            validate_wire(&format!("JOIN {channel}\r\n"), &self.encoding)?;
        }
        validate_wire(&format!("NICK {}\r\n", self.nickname), &self.encoding)?;
        if let Some(password) = &self.server_password {
            if password.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
                return Err("Server password contains a protocol control character.".into());
            }
            if !password.is_empty() && !self.use_tls {
                return Err("Enable TLS before using a server password.".into());
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

fn valid_channel(value: &str) -> bool {
    (value.starts_with('#') || value.starts_with('&'))
        && value.len() > 1
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == ',')
}

fn valid_nickname(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(['#', '&', '~', '@', '%', '+'])
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
enum SaslPhase {
    Listing,
    Requesting,
    Challenging,
    WaitingForResult,
    Complete,
}

struct SaslHandshake {
    credentials: SaslCredentials,
    phase: SaslPhase,
    offered: bool,
    sent_lines: Vec<String>,
}

impl SaslHandshake {
    fn new(credentials: SaslCredentials) -> Self {
        Self {
            credentials,
            phase: SaslPhase::Listing,
            offered: false,
            sent_lines: Vec::new(),
        }
    }

    fn take_sent_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.sent_lines)
    }

    fn observe(&mut self, client: &Client, message: &IrcMessage) -> Result<(), String> {
        match (&self.phase, &message.command) {
            (
                SaslPhase::Listing,
                IrcCommand::CAP(_, CapSubCommand::LS, continuation, capabilities),
            ) => {
                // irc-proto represents a three-argument server CAP LS as the
                // continuation field; a four-argument multiline LS has both.
                let capabilities = capabilities
                    .as_deref()
                    .or(continuation.as_deref())
                    .unwrap_or("");
                self.offered |= capabilities.split_whitespace().any(|capability| {
                    let mut parts = capability.splitn(2, '=');
                    if parts.next() != Some("sasl") {
                        return false;
                    }
                    parts.next().is_none_or(|mechanisms| {
                        mechanisms
                            .split(',')
                            .any(|name| name.eq_ignore_ascii_case("PLAIN"))
                    })
                });
                if continuation.as_deref() != Some("*") {
                    if !self.offered {
                        return Err("Server does not offer SASL PLAIN.".into());
                    }
                    client
                        .send_cap_req(&[Capability::Sasl])
                        .map_err(|error| error.to_string())?;
                    self.sent_lines.push("CAP REQ sasl".into());
                    self.phase = SaslPhase::Requesting;
                }
            }
            (
                SaslPhase::Requesting,
                IrcCommand::CAP(_, CapSubCommand::ACK, continuation, capabilities),
            ) => {
                let capabilities = capabilities
                    .as_deref()
                    .or(continuation.as_deref())
                    .unwrap_or("");
                if !capabilities
                    .split_whitespace()
                    .any(|capability| capability == "sasl" || capability.starts_with("sasl="))
                {
                    return Err("Server did not acknowledge SASL.".into());
                }
                client
                    .send_sasl_plain()
                    .map_err(|error| error.to_string())?;
                self.sent_lines.push("AUTHENTICATE PLAIN".into());
                self.phase = SaslPhase::Challenging;
            }
            (SaslPhase::Requesting, IrcCommand::CAP(_, CapSubCommand::NAK, _, _)) => {
                return Err("Server rejected SASL capability.".into());
            }
            (SaslPhase::Challenging, IrcCommand::AUTHENTICATE(challenge)) if challenge == "+" => {
                let payload = format!(
                    "\0{}\0{}",
                    self.credentials.username, self.credentials.password
                );
                let encoded = base64::engine::general_purpose::STANDARD.encode(payload.as_bytes());
                for chunk in encoded.as_bytes().chunks(400) {
                    client
                        .send_sasl(std::str::from_utf8(chunk).expect("base64 is ASCII"))
                        .map_err(|error| error.to_string())?;
                    self.sent_lines.push("AUTHENTICATE [redacted]".into());
                }
                if encoded.len() % 400 == 0 {
                    client.send_sasl("+").map_err(|error| error.to_string())?;
                    self.sent_lines.push("AUTHENTICATE +".into());
                }
                self.credentials.password.clear();
                self.phase = SaslPhase::WaitingForResult;
            }
            (SaslPhase::WaitingForResult, IrcCommand::Response(Response::RPL_SASLSUCCESS, _)) => {
                client
                    .send(IrcCommand::CAP(None, CapSubCommand::END, None, None))
                    .map_err(|error| error.to_string())?;
                self.sent_lines.push("CAP END".into());
                self.phase = SaslPhase::Complete;
            }
            (
                _,
                IrcCommand::Response(
                    Response::ERR_SASLFAIL
                    | Response::ERR_SASLTOOLONG
                    | Response::ERR_SASLABORT
                    | Response::ERR_SASLALREADY,
                    _,
                ),
            ) => {
                return Err("SASL authentication failed.".into());
            }
            _ => {}
        }
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
    },
    ChannelActivity {
        channel: String,
        actor: String,
        kind: ChannelActivityKind,
    },
    Names {
        channel: String,
        users: Vec<String>,
    },
    ServerLine(String),
    /// A completed WHOIS reply, emitted at end-of-WHOIS (318).
    Whois(Box<WhoisInfo>),
    OutgoingAccepted {
        channel: String,
        text: String,
        notice: bool,
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
        _ => message
            .to_string()
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
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
        Outgoing::Quit => return Ok(()),
    };
    validate_wire(&message.to_string(), encoding)
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
            let target = selected_channel(selected)?;
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
                (selected_channel(selected)?, text)
            } else {
                let (first, remainder) = split_word(rest);
                if remainder.is_empty() {
                    (selected_channel(selected)?, first)
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

pub struct Connection {
    commands: mpsc::Sender<Outgoing>,
    events: mpsc::Receiver<Event>,
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
        thread::Builder::new()
            .name("cayenchat-irc".into())
            .spawn(move || {
                let failure_events = event_tx.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();
                    match runtime {
                        Ok(runtime) => runtime.block_on(run(config, command_rx, event_tx)),
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
            events,
            encoding,
        })
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn is_closed(&self) -> bool {
        self.events.is_closed() && self.events.is_empty()
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

    pub fn send_private_message(&self, nickname: &str, text: &str) -> Result<(), String> {
        if !valid_nickname(nickname) {
            return Err("Invalid message target.".into());
        }
        validate_message_text(text)?;
        let outgoing = Outgoing::Message {
            target: nickname.to_owned(),
            text: text.to_owned(),
            display_text: text.to_owned(),
            notice: false,
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

    pub fn disconnect(&self) -> Result<(), String> {
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
        username: Some(config.nickname.clone()),
        realname: Some("CayenChat".into()),
        password: config.server_password.clone(),
        channels: config.channels.clone(),
        use_tls: Some(config.use_tls),
        encoding: Some(config.encoding.clone()),
        dangerously_accept_invalid_certs: Some(config.use_tls && !config.verify_tls_certificates),
        ..Config::default()
    }
}

async fn run(
    config: ConnectionConfig,
    mut commands: mpsc::Receiver<Outgoing>,
    events: mpsc::Sender<Event>,
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
    match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    {
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
    let auto_join_channels = config.channels.clone();
    let irc_config = library_config(&config);
    let mut sasl = config.sasl.map(SaslHandshake::new);
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
    diagnostic(
        &events,
        started,
        if sasl.is_some() {
            "Sending CAP LS, optional PASS, NICK, and USER; waiting for SASL and welcome."
        } else {
            "Sending optional PASS, NICK, and USER; waiting for server welcome (001)."
        },
    )
    .await;
    // Mirror the library's identify() sequence so every registration command
    // can be included in the diagnostic transcript after it is queued.
    let mut registration = if sasl.is_some() {
        vec![IrcCommand::CAP(
            None,
            CapSubCommand::LS,
            Some("302".into()),
            None,
        )]
    } else {
        vec![IrcCommand::CAP(None, CapSubCommand::END, None, None)]
    };
    if let Some(password) = server_password.filter(|value| !value.is_empty()) {
        registration.push(IrcCommand::PASS(password));
    }
    registration.push(IrcCommand::NICK(registration_nick.clone()));
    registration.push(IrcCommand::USER(
        registration_nick.clone(),
        "0".into(),
        "CayenChat".into(),
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
    let registration_deadline = tokio::time::Instant::now() + REGISTRATION_TIMEOUT;
    let mut registered = false;
    let mut refusal = None;
    let mut current_nick = registration_nick;
    let mut roster = RosterTracker::default();
    let mut whois = WhoisCollector::default();
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
                                Event::OutgoingAccepted {channel: target, text: display_text, notice}
                            }
                            Err(error) => Event::ServerLine(format!("Send failed: {error}")),
                        };
                        if events.send(event).await.is_err() { break; }
                    }
                    Outgoing::Raw(message) => {
                        let line = redacted_wire_line(&message);
                        let result = validate_wire(&message.to_string(), &wire_encoding)
                            .and_then(|_| client.send(message).map_err(|error| error.to_string()));
                        match result {
                            Ok(()) => wire(&events, started, WireDirection::Sent, line).await,
                            Err(error) => {
                                if events.send(Event::ServerLine(format!("Command failed: {error}"))).await.is_err() {
                                    break;
                                }
                            }
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
                        let _ = events.send(Event::Disconnected("Disconnected by user.".into())).await;
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
                        if let Some(handshake) = sasl.as_mut() {
                            let result = handshake.observe(&client, &message);
                            let sent_lines = handshake.take_sent_lines();
                            for line in sent_lines {
                                wire(&events, started, WireDirection::Sent, line).await;
                            }
                            if let Err(error) = result {
                                let _ = events.send(Event::Refused(error)).await;
                                return;
                            }
                        }
                        if !registered && let Some(reason) = registration_refusal(&message) {
                            refusal = Some(reason);
                        }
                        if matches!(message.command, IrcCommand::Response(Response::RPL_WELCOME, _)) {
                            registered = true;
                            sasl = None;
                            diagnostic(&events, started, "Registration completed (001 received).").await;
                        }
                        let whois_reply = whois.observe(&message);
                        for event in translate_message(&client, &mut roster, &current_nick, message).into_iter().chain(whois_reply.map(|info| Event::Whois(Box::new(info)))) {
                            match &event {
                                Event::Registered { nickname } | Event::NickChanged { nickname } => current_nick = nickname.clone(),
                                _ => {}
                            }
                            if events.send(event).await.is_err() { return; }
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
            _ = registration_progress.tick(), if !registered => {
                diagnostic(&events, started, "Still waiting for IRC registration (001 welcome).").await;
            }
            _ = tokio::time::sleep_until(registration_deadline), if !registered => {
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
    for user in &mut users {
        let nickname = display_nickname(user).to_owned();
        if let Some(prefix) = roster
            .renamed_roles
            .get(&(channel.to_owned(), nickname.to_lowercase()))
        {
            *user = format!("{prefix}{nickname}");
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

fn translate_message(
    client: &Client,
    roster: &mut RosterTracker,
    current_nick: &str,
    message: IrcMessage,
) -> Vec<Event> {
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
            }
        }),
        IrcCommand::PART(channel, reason) => actor.as_ref().map(|actor| Event::ChannelActivity {
            channel: channel.clone(),
            actor: actor.clone(),
            kind: ChannelActivityKind::Left {
                reason: reason.clone(),
            },
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
        IrcCommand::QUIT(_) | IrcCommand::NICK(_) => client.list_channels().unwrap_or_default(),
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
        IrcCommand::PRIVMSG(target, text) | IrcCommand::NOTICE(target, text)
            if valid_channel(target) =>
        {
            vec![Event::ChannelMessage {
                channel: target.clone(),
                sender: message.source_nickname().unwrap_or("server").to_owned(),
                text: text.clone(),
                notice: matches!(message.command, IrcCommand::NOTICE(_, _)),
            }]
        }
        IrcCommand::Response(Response::RPL_ENDOFNAMES, args) => {
            let Some(channel) = args.iter().find(|arg| valid_channel(arg)) else {
                return Vec::new();
            };
            vec![names_snapshot(client, roster, channel)]
        }
        _ => vec![Event::ServerLine(message.to_string().trim_end().to_owned())],
    };
    if let Some(activity) = activity {
        translated.push(activity);
    }
    if let IrcCommand::QUIT(reason) = &message.command
        && let Some(actor) = actor
    {
        translated.extend(
            changed_channels
                .iter()
                .filter(|channel| roster.had_member(channel, &actor))
                .map(|channel| Event::ChannelActivity {
                    channel: channel.clone(),
                    actor: actor.clone(),
                    kind: ChannelActivityKind::Quit {
                        reason: reason.clone(),
                    },
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
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        time::Instant,
    };

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
            events,
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
                    } if channel == "#test" => activities.push((actor, kind)),
                    Event::ChannelActivity {
                        channel,
                        actor,
                        kind: ChannelActivityKind::Quit { .. },
                    } => panic!("{actor} quit shown in {channel} without being a member"),
                    Event::Wire {
                        direction, line, ..
                    } => transcript.push((direction, line)),
                    Event::Disconnected(reason) => panic!("unexpected disconnect: {reason}"),
                    _ => {}
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
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
        connection.send_private_message("charlie", "hello").unwrap();
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
}
