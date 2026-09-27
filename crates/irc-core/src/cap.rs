//! IRCv3 capability negotiation, shared by SASL and opt-in extensions.
//!
//! One [`CapNegotiation`] exists per connection attempt, so a reconnect
//! always starts from nothing. It is pure: it returns the commands to send
//! and the worker sends and records them.
//!
//! Negotiation starts (`CAP LS 302`) only when SASL or an opt-in extension
//! is configured; otherwise registration keeps the plain `CAP END` the `irc`
//! library's own `identify()` sends. Each capability is requested with its
//! own `CAP REQ`, because a REQ is accepted or rejected as a whole: a server
//! declining an optional extension must not take SASL down with it. `CAP END`
//! is sent once every request is answered and SASL has finished. A server
//! without CAP registers directly (001), which ends negotiation.

use std::collections::{HashMap, HashSet};

use base64::Engine;
use irc::proto::{CapSubCommand, Command as IrcCommand, Message as IrcMessage, Response};

pub const SASL: &str = "sasl";
pub const MESSAGE_TAGS: &str = "message-tags";
pub const SERVER_TIME: &str = "server-time";

/// Advertised capabilities kept per connection; a hostile server cannot grow
/// the table beyond this.
const MAX_OFFERED: usize = 256;
/// Longest advertised value kept (for example `sasl=PLAIN,EXTERNAL`).
const MAX_VALUE_BYTES: usize = 512;

/// Opt-in IRCv3 extensions requested for one connection. Everything is off
/// by default; turning an option on only asks the server for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ircv3Options {
    /// Accept any well-formed tag, client-only tags and TAGMSG.
    pub message_tags: bool,
    /// Receive the server's `time` tag. Independent of `message_tags`.
    pub server_time: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No CAP LS was sent; CAP replies are ignored.
    Inactive,
    Listing,
    Requesting,
    /// CAP END was sent or registration completed.
    Ended,
}

/// Commands to send and diagnostics to show after one incoming message.
#[derive(Debug, Default, PartialEq)]
pub struct CapStep {
    pub send: Vec<IrcCommand>,
    pub notes: Vec<String>,
}

pub struct CapNegotiation {
    phase: Phase,
    /// Opt-in extensions still wanted, in request order.
    optional: Vec<&'static str>,
    sasl: Option<SaslHandshake>,
    offered: HashMap<String, Option<String>>,
    enabled: HashSet<&'static str>,
    /// Requests without an ACK or NAK yet.
    pending: Vec<&'static str>,
    /// Capabilities of a multiline ACK until its last line.
    ack_lines: Vec<String>,
}

impl CapNegotiation {
    /// `utf8` tells whether the connection's wire encoding is UTF-8. Tag
    /// values are UTF-8, but the `irc` codec decodes a whole line with the
    /// connection's encoding, so arbitrary client tags could corrupt a legacy
    /// encoding's decoder state; `message-tags` is then not requested.
    /// `server-time` values are ASCII and stay available.
    pub fn new(options: Ircv3Options, sasl: Option<crate::SaslCredentials>, utf8: bool) -> Self {
        let mut optional = Vec::new();
        if options.message_tags && utf8 {
            optional.push(MESSAGE_TAGS);
        }
        if options.server_time {
            optional.push(SERVER_TIME);
        }
        Self {
            phase: Phase::Inactive,
            optional,
            sasl: sasl.map(SaslHandshake::new),
            offered: HashMap::new(),
            enabled: HashSet::new(),
            pending: Vec::new(),
            ack_lines: Vec::new(),
        }
    }

    /// The command that opens registration: `CAP LS 302` when anything is to
    /// be negotiated, otherwise the library's customary `CAP END`.
    pub fn start(&mut self) -> IrcCommand {
        if self.sasl.is_some() || !self.optional.is_empty() {
            self.phase = Phase::Listing;
            IrcCommand::CAP(None, CapSubCommand::LS, Some("302".into()), None)
        } else {
            IrcCommand::CAP(None, CapSubCommand::END, None, None)
        }
    }

    pub fn negotiating(&self) -> bool {
        matches!(self.phase, Phase::Listing | Phase::Requesting)
    }

    pub fn uses_sasl(&self) -> bool {
        self.sasl.is_some()
    }

    pub fn enabled(&self, capability: &str) -> bool {
        self.enabled.contains(capability)
    }

    /// Registration completed (001). Pending SASL is abandoned, as before
    /// this negotiation existed; NEW and DEL are still followed.
    pub fn registered(&mut self) {
        self.sasl = None;
        if self.phase != Phase::Inactive {
            self.phase = Phase::Ended;
        }
    }

    /// Advances negotiation. An error is a refusal the caller must not retry
    /// automatically (a missing or rejected SASL mechanism, failed SASL).
    pub fn observe(&mut self, message: &IrcMessage) -> Result<CapStep, String> {
        let mut step = CapStep::default();
        if self.phase == Phase::Inactive {
            return Ok(step);
        }
        if let Some((subcommand, continued, list)) = cap_reply(&message.command) {
            match subcommand.as_str() {
                "LS" if self.phase == Phase::Listing => {
                    self.remember_offers(&list);
                    if !continued {
                        self.request_after_listing(&mut step)?;
                    }
                }
                "ACK" => {
                    self.ack_lines
                        .extend(list.split_whitespace().map(str::to_owned));
                    if !continued {
                        let acknowledged = std::mem::take(&mut self.ack_lines);
                        self.acknowledge(&acknowledged, &mut step);
                    }
                }
                "NAK" => {
                    for name in list.split_whitespace() {
                        let Some(index) = self.pending.iter().position(|cap| *cap == name) else {
                            continue;
                        };
                        let name = self.pending.remove(index);
                        if name == SASL {
                            return Err("Server rejected SASL capability.".into());
                        }
                        step.notes.push(format!(
                            "Server declined optional capability {name}; continuing without it."
                        ));
                    }
                }
                "NEW" => {
                    self.remember_offers(&list);
                    if self.phase != Phase::Listing {
                        self.request_optional(&mut step);
                    }
                }
                "DEL" => {
                    for name in list.split_whitespace() {
                        self.offered.remove(name);
                        self.pending.retain(|cap| *cap != name);
                        if self.enabled.iter().any(|cap| *cap == name) {
                            self.enabled.retain(|cap| *cap != name);
                            step.notes
                                .push(format!("Server withdrew capability {name}."));
                        }
                    }
                }
                _ => {}
            }
        } else if let Some(sasl) = self.sasl.as_mut() {
            step.send.extend(sasl.observe(message)?);
        }
        self.end_if_settled(&mut step);
        Ok(step)
    }

    fn remember_offers(&mut self, list: &str) {
        for token in list.split_whitespace() {
            let (name, value) = match token.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (token, None),
            };
            // Capabilities this connection may request are always kept, so
            // filling the table cannot hide them.
            let wanted = name == SASL || self.optional.contains(&name);
            if name.is_empty()
                || (!wanted
                    && !self.offered.contains_key(name)
                    && self.offered.len() >= MAX_OFFERED)
            {
                continue;
            }
            let value = value
                .filter(|value| value.len() <= MAX_VALUE_BYTES)
                .map(str::to_owned);
            self.offered.insert(name.to_owned(), value);
        }
    }

    fn request_after_listing(&mut self, step: &mut CapStep) -> Result<(), String> {
        self.phase = Phase::Requesting;
        if self.sasl.is_some() {
            let plain = match self.offered.get(SASL) {
                // CAP 3.1 servers list `sasl` without mechanisms.
                Some(None) => true,
                Some(Some(mechanisms)) => mechanisms
                    .split(',')
                    .any(|name| name.eq_ignore_ascii_case("PLAIN")),
                None => false,
            };
            if !plain {
                return Err("Server does not offer SASL PLAIN.".into());
            }
            step.send.push(request(SASL));
            self.pending.push(SASL);
        }
        self.request_optional(step);
        Ok(())
    }

    fn request_optional(&mut self, step: &mut CapStep) {
        for name in self.optional.clone() {
            if self.offered.contains_key(name)
                && !self.enabled.contains(name)
                && !self.pending.contains(&name)
            {
                step.send.push(request(name));
                self.pending.push(name);
            }
        }
    }

    fn acknowledge(&mut self, acknowledged: &[String], step: &mut CapStep) {
        for token in acknowledged {
            if let Some(name) = token.strip_prefix('-') {
                self.enabled.retain(|cap| *cap != name);
                continue;
            }
            let Some(index) = self.pending.iter().position(|cap| cap == token) else {
                continue;
            };
            let name = self.pending.remove(index);
            self.enabled.insert(name);
            if name == SASL {
                if let Some(sasl) = self.sasl.as_mut() {
                    step.send.extend(sasl.begin());
                }
            } else {
                step.notes.push(format!("Capability {name} enabled."));
            }
        }
    }

    fn end_if_settled(&mut self, step: &mut CapStep) {
        let sasl_done = self.sasl.as_ref().is_none_or(SaslHandshake::complete);
        if self.phase == Phase::Requesting && self.pending.is_empty() && sasl_done {
            step.send
                .push(IrcCommand::CAP(None, CapSubCommand::END, None, None));
            self.phase = Phase::Ended;
        }
    }
}

fn request(name: &str) -> IrcCommand {
    IrcCommand::CAP(None, CapSubCommand::REQ, None, Some(name.into()))
}

/// A server CAP reply as (subcommand, continued, capability list).
///
/// Server replies always carry a target first (`CAP * LS ...`,
/// `CAP nick NEW ...`). irc-proto guesses which argument is the subcommand,
/// which goes wrong when the target itself is a subcommand name (a user
/// called `new`). Its fields keep the original argument order, so the
/// arguments are read back by position instead.
fn cap_reply(command: &IrcCommand) -> Option<(String, bool, String)> {
    let IrcCommand::CAP(first, subcommand, third, fourth) = command else {
        return None;
    };
    let args: Vec<&str> = first
        .as_deref()
        .into_iter()
        .chain(Some(subcommand.to_str()))
        .chain(third.as_deref())
        .chain(fourth.as_deref())
        .collect();
    let subcommand = args.get(1)?.to_ascii_uppercase();
    match args.as_slice() {
        [_, _, "*", list] => Some((subcommand, true, (*list).to_owned())),
        [_, _, list] => Some((subcommand, false, (*list).to_owned())),
        [_, _] => Some((subcommand, false, String::new())),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaslPhase {
    Idle,
    Challenging,
    WaitingForResult,
    Complete,
}

/// SASL PLAIN once the `sasl` capability is acknowledged.
struct SaslHandshake {
    credentials: crate::SaslCredentials,
    phase: SaslPhase,
}

impl SaslHandshake {
    fn new(credentials: crate::SaslCredentials) -> Self {
        Self {
            credentials,
            phase: SaslPhase::Idle,
        }
    }

    fn complete(&self) -> bool {
        self.phase == SaslPhase::Complete
    }

    fn begin(&mut self) -> Vec<IrcCommand> {
        self.phase = SaslPhase::Challenging;
        vec![IrcCommand::AUTHENTICATE("PLAIN".into())]
    }

    fn observe(&mut self, message: &IrcMessage) -> Result<Vec<IrcCommand>, String> {
        let mut send = Vec::new();
        match (self.phase, &message.command) {
            (SaslPhase::Challenging, IrcCommand::AUTHENTICATE(challenge)) if challenge == "+" => {
                let payload = format!(
                    "\0{}\0{}",
                    self.credentials.username, self.credentials.password
                );
                let encoded = base64::engine::general_purpose::STANDARD.encode(payload.as_bytes());
                for chunk in encoded.as_bytes().chunks(400) {
                    send.push(IrcCommand::AUTHENTICATE(
                        std::str::from_utf8(chunk).expect("base64 is ASCII").into(),
                    ));
                }
                if encoded.len() % 400 == 0 {
                    send.push(IrcCommand::AUTHENTICATE("+".into()));
                }
                self.credentials.password.clear();
                self.phase = SaslPhase::WaitingForResult;
            }
            (SaslPhase::WaitingForResult, IrcCommand::Response(Response::RPL_SASLSUCCESS, _)) => {
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
            ) => return Err("SASL authentication failed.".into()),
            _ => {}
        }
        Ok(send)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SaslCredentials;

    fn line(text: &str) -> IrcMessage {
        text.parse().unwrap()
    }

    fn sent(step: &CapStep) -> Vec<String> {
        step.send.iter().map(String::from).collect()
    }

    fn options(message_tags: bool, server_time: bool) -> Ircv3Options {
        Ircv3Options {
            message_tags,
            server_time,
        }
    }

    fn credentials() -> SaslCredentials {
        SaslCredentials {
            username: "account".into(),
            password: "secret".into(),
        }
    }

    #[test]
    fn nothing_is_negotiated_when_everything_is_off() {
        let mut cap = CapNegotiation::new(Ircv3Options::default(), None, true);
        assert_eq!(String::from(&cap.start()), "CAP END");
        assert!(!cap.negotiating());
        let step = cap
            .observe(&line(":s CAP * LS :sasl server-time message-tags"))
            .unwrap();
        assert_eq!(step, CapStep::default());
        let step = cap.observe(&line(":s CAP me NEW :server-time")).unwrap();
        assert_eq!(step, CapStep::default());
        assert!(!cap.enabled(SERVER_TIME));
    }

    #[test]
    fn sasl_alone_requests_only_sasl_and_ends_after_success() {
        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        assert_eq!(String::from(&cap.start()), "CAP LS 302");
        let step = cap
            .observe(&line(
                ":s CAP * LS :sasl=PLAIN,EXTERNAL server-time message-tags",
            ))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ sasl"]);
        let step = cap.observe(&line(":s CAP * ACK :sasl")).unwrap();
        assert_eq!(sent(&step), ["AUTHENTICATE PLAIN"]);
        let step = cap.observe(&line("AUTHENTICATE +")).unwrap();
        assert_eq!(sent(&step), ["AUTHENTICATE AGFjY291bnQAc2VjcmV0"]);
        assert!(cap.negotiating());
        let step = cap
            .observe(&line(":s 903 * :SASL authentication successful"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(!cap.negotiating());
        assert!(!cap.enabled(SERVER_TIME));
    }

    #[test]
    fn continuation_lines_are_collected_before_requesting() {
        let mut cap = CapNegotiation::new(options(true, true), None, true);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS * :multi-prefix server-time"))
            .unwrap();
        assert!(step.send.is_empty());
        let step = cap
            .observe(&line(":s CAP * LS :message-tags draft/x=1"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ message-tags", "CAP REQ server-time"]);
        let step = cap.observe(&line(":s CAP * ACK :server-time")).unwrap();
        assert!(step.send.is_empty(), "message-tags is still pending");
        let step = cap.observe(&line(":s CAP * ACK :message-tags")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(cap.enabled(SERVER_TIME) && cap.enabled(MESSAGE_TAGS));
    }

    #[test]
    fn multiline_ack_applies_on_its_last_line() {
        let mut cap = CapNegotiation::new(options(true, true), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :server-time message-tags"))
            .unwrap();
        let step = cap.observe(&line(":s CAP * ACK * :server-time")).unwrap();
        assert!(step.send.is_empty());
        assert!(!cap.enabled(SERVER_TIME));
        let step = cap.observe(&line(":s CAP * ACK :message-tags")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(cap.enabled(SERVER_TIME) && cap.enabled(MESSAGE_TAGS));
    }

    #[test]
    fn declined_optional_capability_does_not_break_sasl() {
        let mut cap = CapNegotiation::new(options(false, true), Some(credentials()), true);
        cap.start();
        let step = cap.observe(&line(":s CAP * LS :sasl server-time")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ sasl", "CAP REQ server-time"]);
        let step = cap.observe(&line(":s CAP * NAK :server-time")).unwrap();
        assert!(step.send.is_empty());
        assert_eq!(step.notes.len(), 1);
        cap.observe(&line(":s CAP * ACK :sasl")).unwrap();
        cap.observe(&line("AUTHENTICATE +")).unwrap();
        let step = cap.observe(&line(":s 903 * :ok")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(!cap.enabled(SERVER_TIME));
    }

    #[test]
    fn sasl_failures_remain_refusals() {
        let mut cap = CapNegotiation::new(options(true, true), Some(credentials()), true);
        cap.start();
        assert_eq!(
            cap.observe(&line(":s CAP * LS :server-time")),
            Err("Server does not offer SASL PLAIN.".into())
        );

        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        assert!(cap.observe(&line(":s CAP * LS :sasl=EXTERNAL")).is_err());

        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        cap.observe(&line(":s CAP * LS :sasl")).unwrap();
        assert_eq!(
            cap.observe(&line(":s CAP * NAK :sasl")),
            Err("Server rejected SASL capability.".into())
        );

        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        cap.observe(&line(":s CAP * LS :sasl")).unwrap();
        cap.observe(&line(":s CAP * ACK :sasl")).unwrap();
        cap.observe(&line("AUTHENTICATE +")).unwrap();
        assert!(cap.observe(&line(":s 904 * :failed")).is_err());
    }

    #[test]
    fn unoffered_extensions_end_negotiation_immediately() {
        let mut cap = CapNegotiation::new(options(true, true), None, true);
        cap.start();
        let step = cap.observe(&line(":s CAP * LS :multi-prefix")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        let step = cap.observe(&line(":s CAP * LS :")).unwrap();
        assert!(step.send.is_empty(), "a late LS after END is ignored");
    }

    #[test]
    fn new_and_del_follow_the_server_after_registration() {
        let mut cap = CapNegotiation::new(options(false, true), None, true);
        cap.start();
        let step = cap.observe(&line(":s CAP * LS :multi-prefix")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        cap.registered();
        // A nickname that parses as a subcommand name does not confuse replies.
        let step = cap.observe(&line(":s CAP new NEW :server-time")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time"]);
        let step = cap.observe(&line(":s CAP new ACK :server-time")).unwrap();
        assert!(step.send.is_empty(), "no CAP END after registration");
        assert!(cap.enabled(SERVER_TIME));
        let step = cap.observe(&line(":s CAP new DEL :server-time")).unwrap();
        assert_eq!(step.notes, ["Server withdrew capability server-time."]);
        assert!(!cap.enabled(SERVER_TIME));
        // Unwanted capabilities are never requested.
        let step = cap
            .observe(&line(":s CAP new NEW :message-tags sasl"))
            .unwrap();
        assert!(step.send.is_empty());
    }

    #[test]
    fn legacy_encodings_do_not_request_message_tags() {
        let mut cap = CapNegotiation::new(options(true, true), None, false);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS :message-tags server-time"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time"]);

        let mut cap = CapNegotiation::new(options(true, false), None, false);
        assert_eq!(String::from(&cap.start()), "CAP END");
    }

    #[test]
    fn registration_without_cap_support_ends_negotiation() {
        let mut cap = CapNegotiation::new(options(true, true), None, true);
        cap.start();
        cap.observe(&line(":s 421 * CAP :Unknown command")).unwrap();
        assert!(cap.negotiating());
        cap.registered();
        assert!(!cap.negotiating());
        let step = cap.observe(&line(":s CAP * ACK :server-time")).unwrap();
        assert!(step.send.is_empty());
        assert!(!cap.enabled(SERVER_TIME), "unrequested ACK is ignored");
    }

    #[test]
    fn advertised_capabilities_are_bounded() {
        let mut cap = CapNegotiation::new(options(false, true), None, true);
        cap.start();
        let many: Vec<String> = (0..MAX_OFFERED + 50).map(|n| format!("x{n}")).collect();
        cap.observe(&line(&format!(":s CAP * LS * :{}", many.join(" "))))
            .unwrap();
        let step = cap.observe(&line(":s CAP * LS :server-time")).unwrap();
        assert_eq!(cap.offered.len(), MAX_OFFERED + 1);
        assert_eq!(
            sent(&step),
            ["CAP REQ server-time"],
            "wanted names survive a full table"
        );
    }
}
