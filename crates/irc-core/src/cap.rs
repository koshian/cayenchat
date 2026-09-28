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
pub const BATCH: &str = "batch";
/// The experimental metadata draft, used only for user avatars. The legacy
/// `metadata-notify` is never requested: the draft forbids asking for both.
pub const METADATA: &str = "draft/metadata-2";
/// The work-in-progress chathistory extension; the unprefixed name is
/// reserved for the final specification.
pub const CHATHISTORY: &str = "draft/chathistory";

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
    /// Receive `BATCH` and the `batch` tag, so history batches are told
    /// apart from live traffic. Independent of the other options.
    pub batch: bool,
    /// Receive user avatars (and publish ours) through the experimental
    /// `draft/metadata-2`, when the server offers it. The draft requires
    /// `batch`, which is then requested too even if `batch` is off, but only
    /// from servers that offer metadata.
    pub metadata: bool,
    /// Exchange avatars with other clients through KVIrc's CTCP AVATAR
    /// (experimental). Needs no capability; see `peer_avatar`.
    pub peer_avatars: bool,
    /// Request recent channel history with `draft/chathistory`
    /// (experimental), when the server offers it. Replies are recognized by
    /// their batch, so the capability is requested only after `batch` is
    /// acknowledged; `server-time` and, on UTF-8 connections,
    /// `message-tags` (for message IDs) are requested with it, as the
    /// specification's full support lists them.
    pub chathistory: bool,
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
    /// Extensions the user opted into directly. The others in `optional`
    /// are wanted only for an extension that needs them, and are requested
    /// only from a server that offers that extension.
    explicit: Vec<&'static str>,
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
        let mut explicit = Vec::new();
        if (options.message_tags || options.chathistory) && utf8 {
            optional.push(MESSAGE_TAGS);
        }
        if options.message_tags && utf8 {
            explicit.push(MESSAGE_TAGS);
        }
        if options.server_time || options.chathistory {
            optional.push(SERVER_TIME);
        }
        if options.server_time {
            explicit.push(SERVER_TIME);
        }
        // Batch references and the history types are ASCII, like server-time.
        if options.batch || options.metadata || options.chathistory {
            optional.push(BATCH);
        }
        if options.batch {
            explicit.push(BATCH);
        }
        // draft/metadata-2 MUST be used with batch. It is requested only once
        // batch is enabled, so a server declining batch never ends up with
        // metadata alone. Values are UTF-8 even on legacy encodings; the
        // worker then accepts only ASCII avatar URLs (see `metadata`).
        if options.metadata {
            optional.push(METADATA);
            explicit.push(METADATA);
        }
        // Likewise chathistory: without batch its replies would look live.
        // Its references are ASCII, so legacy encodings may use it.
        if options.chathistory {
            optional.push(CHATHISTORY);
            explicit.push(CHATHISTORY);
        }
        Self {
            phase: Phase::Inactive,
            optional,
            explicit,
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
        self.enforce_dependencies(&mut step);
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
                && (!needs_batch(name) || self.enabled.contains(BATCH))
                && (self.explicit.contains(&name)
                    || dependents(name).iter().any(|dependent| {
                        self.optional.contains(dependent) && self.offered.contains_key(*dependent)
                    }))
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
                if name == BATCH {
                    // Now that batch is on, metadata and chathistory may follow.
                    self.request_optional(step);
                }
            }
        }
    }

    /// draft/metadata-2 and draft/chathistory must not outlive batch (a DEL
    /// or `ACK -batch`): they are dropped at once, so nothing depends on
    /// them, and the server is asked to disable them too. A request still
    /// waiting is forgotten; its late ACK is then ignored as unrequested.
    fn enforce_dependencies(&mut self, step: &mut CapStep) {
        if self.enabled.contains(BATCH) {
            return;
        }
        for name in [METADATA, CHATHISTORY] {
            let waiting = self.pending.contains(&name);
            if self.enabled.remove(name) || waiting {
                self.pending.retain(|cap| *cap != name);
                step.send.push(request(&format!("-{name}")));
                step.notes.push(format!(
                    "Capability {BATCH} is gone; disabling {name}, which requires it."
                ));
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

/// Extensions whose replies only make sense inside batches.
fn needs_batch(name: &str) -> bool {
    name == METADATA || name == CHATHISTORY
}

/// Extensions that make this client want `name` without its own opt-in.
fn dependents(name: &str) -> &'static [&'static str] {
    match name {
        BATCH => &[METADATA, CHATHISTORY],
        SERVER_TIME | MESSAGE_TAGS => &[CHATHISTORY],
        _ => &[],
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
                    Response::ERR_NICKLOCKED
                    | Response::ERR_SASLFAIL
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
            batch: false,
            metadata: false,
            peer_avatars: false,
            chathistory: false,
        }
    }

    fn history(utf8_tags: bool) -> Ircv3Options {
        Ircv3Options {
            message_tags: utf8_tags,
            chathistory: true,
            ..Ircv3Options::default()
        }
    }

    fn avatars(batch: bool) -> Ircv3Options {
        Ircv3Options {
            batch,
            metadata: true,
            ..Ircv3Options::default()
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
    fn batch_is_requested_only_when_opted_in_and_offered() {
        let batch_only = Ircv3Options {
            batch: true,
            ..Ircv3Options::default()
        };
        // Offered alongside everything else: only batch is asked for.
        let mut cap = CapNegotiation::new(batch_only, None, true);
        assert_eq!(String::from(&cap.start()), "CAP LS 302");
        let step = cap
            .observe(&line(
                ":s CAP * LS :batch server-time message-tags draft/chathistory",
            ))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ batch"]);
        let step = cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(cap.enabled(BATCH));
        assert!(!cap.enabled(SERVER_TIME) && !cap.enabled(MESSAGE_TAGS));
        cap.registered();
        cap.observe(&line(":s CAP me DEL :batch")).unwrap();
        assert!(!cap.enabled(BATCH));

        // Not offered: nothing is requested.
        let mut cap = CapNegotiation::new(batch_only, None, true);
        cap.start();
        let step = cap.observe(&line(":s CAP * LS :server-time")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);

        // Off: never requested, even when offered.
        let mut cap = CapNegotiation::new(options(false, true), None, true);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS :batch server-time"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time"]);

        // Batch data is ASCII, so legacy encodings still ask for it.
        let mut cap = CapNegotiation::new(batch_only, None, false);
        cap.start();
        let step = cap.observe(&line(":s CAP * LS :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ batch"]);
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

    // capability-negotiation: "The list of capabilities MUST be parsed and
    // processed from left to right ... the last one received takes
    // priority. Clients MUST ignore any trailing whitespace."
    #[test]
    fn capability_lists_are_read_left_to_right_and_the_last_value_wins() {
        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        let step = cap
            .observe(&line(
                ":s CAP * LS :sasl=EXTERNAL server-time sasl=PLAIN   ",
            ))
            .unwrap();
        assert_eq!(
            sent(&step),
            ["CAP REQ sasl"],
            "PLAIN, the later value, counts"
        );
        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        assert!(
            cap.observe(&line(":s CAP * LS :sasl=PLAIN sasl=EXTERNAL"))
                .is_err(),
            "EXTERNAL replaced PLAIN"
        );
    }

    // capability-negotiation: "If no capabilities are available, an empty
    // parameter MUST be sent." Older servers leave the parameter out.
    #[test]
    fn an_empty_capability_list_ends_negotiation() {
        for reply in [":s CAP * LS :", ":s CAP * LS"] {
            let mut cap = CapNegotiation::new(options(true, true), None, true);
            cap.start();
            let step = cap.observe(&line(reply)).unwrap();
            assert_eq!(sent(&step), ["CAP END"], "{reply}");
            assert!(!cap.negotiating());
        }
    }

    // capability-negotiation: "Capability names are case-sensitive" and the
    // full name is an opaque identifier.
    #[test]
    fn capability_names_are_case_sensitive() {
        let mut cap = CapNegotiation::new(options(false, true), None, true);
        cap.start();
        let step = cap
            .observe(&line(
                ":s CAP * LS :Server-Time SERVER-TIME example.org/server-time",
            ))
            .unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        cap.registered();
        let step = cap.observe(&line(":s CAP me NEW :server-time")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time"]);
        cap.observe(&line(":s CAP me ACK :SERVER-TIME")).unwrap();
        assert!(
            !cap.enabled(SERVER_TIME),
            "a differently cased ACK is another name"
        );
        cap.observe(&line(":s CAP me ACK :server-time")).unwrap();
        assert!(cap.enabled(SERVER_TIME));
        cap.observe(&line(":s CAP me DEL :Server-Time")).unwrap();
        assert!(cap.enabled(SERVER_TIME));
    }

    // capability-negotiation: a client changes only what it asked for;
    // DEL of something never enabled is harmless.
    #[test]
    fn unrequested_acks_and_unknown_dels_change_nothing() {
        let mut cap = CapNegotiation::new(options(false, true), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :server-time message-tags batch"))
            .unwrap();
        let step = cap
            .observe(&line(":s CAP * ACK :server-time message-tags batch"))
            .unwrap();
        assert!(cap.enabled(SERVER_TIME));
        assert!(!cap.enabled(MESSAGE_TAGS) && !cap.enabled(BATCH));
        assert_eq!(step.notes, ["Capability server-time enabled."]);
        let step = cap.observe(&line(":s CAP * DEL :batch unknown")).unwrap();
        assert!(step.notes.is_empty());
        assert!(cap.enabled(SERVER_TIME));
    }

    fn sasl_lines(password_len: usize) -> Vec<String> {
        let mut cap = CapNegotiation::new(
            Ircv3Options::default(),
            Some(SaslCredentials {
                username: "a".into(),
                password: "p".repeat(password_len),
            }),
            true,
        );
        cap.start();
        cap.observe(&line(":s CAP * LS :sasl")).unwrap();
        cap.observe(&line(":s CAP * ACK :sasl")).unwrap();
        sent(&cap.observe(&line("AUTHENTICATE +")).unwrap())
    }

    // SASL 3.1: the response "is encoded with Base64 then split to 400-byte
    // chunks ... If the last chunk was exactly 400 bytes long, it must also
    // be followed by `AUTHENTICATE +`".
    #[test]
    fn sasl_responses_are_split_into_400_byte_chunks() {
        // "\0a\0" plus the password: 300 bytes encode to exactly 400.
        let exact = sasl_lines(297);
        assert_eq!(exact.len(), 2);
        assert_eq!(exact[0].len(), "AUTHENTICATE ".len() + 400);
        assert_eq!(exact[1], "AUTHENTICATE +");
        let longer = sasl_lines(298);
        assert_eq!(longer.len(), 2);
        assert_eq!(longer[0].len(), "AUTHENTICATE ".len() + 400);
        assert_ne!(longer[1], "AUTHENTICATE +");
        let two_full = sasl_lines(597);
        assert_eq!(two_full.len(), 3);
        assert_eq!(two_full[2], "AUTHENTICATE +");
        let short = sasl_lines(10);
        assert_eq!(short.len(), 1);
        assert_ne!(short[0], "AUTHENTICATE +");
    }

    // SASL 3.1: 902 (account locked or held), 904 (failed), 905 (too long),
    // 906 (aborted) and 907 (already authenticated) end the attempt; none of
    // them may be retried blindly.
    #[test]
    fn every_sasl_failure_numeric_is_a_refusal() {
        for numeric in ["902", "904", "905", "906", "907"] {
            let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
            cap.start();
            cap.observe(&line(":s CAP * LS :sasl")).unwrap();
            cap.observe(&line(":s CAP * ACK :sasl")).unwrap();
            cap.observe(&line("AUTHENTICATE +")).unwrap();
            assert!(
                cap.observe(&line(&format!(":s {numeric} * :no"))).is_err(),
                "{numeric}"
            );
        }
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

    #[test]
    fn metadata_waits_for_batch_and_brings_it_along() {
        // Batch off: it is still requested as metadata's prerequisite, but
        // only from a server that offers metadata.
        let mut cap = CapNegotiation::new(avatars(false), None, true);
        assert_eq!(String::from(&cap.start()), "CAP LS 302");
        let step = cap
            .observe(&line(":s CAP * LS :batch draft/metadata-2"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ batch"]);
        let step = cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ draft/metadata-2"]);
        let mut cap = CapNegotiation::new(avatars(false), None, true);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS :batch server-time"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP END"], "no metadata: batch stays off");

        // Both on and offered: batch first, metadata after its ACK.
        let mut cap = CapNegotiation::new(avatars(true), None, true);
        cap.start();
        let step = cap
            .observe(&line(
                ":s CAP * LS :draft/metadata-2=max-subs=50 metadata-notify batch",
            ))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ batch"]);
        let step = cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ draft/metadata-2"]);
        let step = cap
            .observe(&line(":s CAP * ACK :draft/metadata-2"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(cap.enabled(METADATA) && cap.enabled(BATCH));

        // Batch declined: metadata is not requested and registration ends.
        let mut cap = CapNegotiation::new(avatars(true), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :draft/metadata-2 batch"))
            .unwrap();
        let step = cap.observe(&line(":s CAP * NAK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(!cap.enabled(METADATA));

        // Metadata declined: batch keeps working.
        let mut cap = CapNegotiation::new(avatars(true), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :draft/metadata-2 batch"))
            .unwrap();
        cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        let step = cap
            .observe(&line(":s CAP * NAK :draft/metadata-2"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(cap.enabled(BATCH) && !cap.enabled(METADATA));

        // Not offered: nothing extra is asked, legacy metadata-notify never.
        let mut cap = CapNegotiation::new(avatars(true), None, true);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS :metadata-notify batch"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ batch"]);
        let step = cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
    }

    #[test]
    fn chathistory_brings_batch_server_time_and_message_tags_and_waits_for_batch() {
        let mut cap = CapNegotiation::new(history(false), None, true);
        assert_eq!(String::from(&cap.start()), "CAP LS 302");
        let step = cap
            .observe(&line(
                ":s CAP * LS :batch server-time message-tags draft/chathistory draft/event-playback",
            ))
            .unwrap();
        assert_eq!(
            sent(&step),
            [
                "CAP REQ message-tags",
                "CAP REQ server-time",
                "CAP REQ batch"
            ],
            "event-playback is never asked for"
        );
        cap.observe(&line(":s CAP * ACK :message-tags")).unwrap();
        cap.observe(&line(":s CAP * ACK :server-time")).unwrap();
        let step = cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ draft/chathistory"]);
        let step = cap
            .observe(&line(":s CAP * ACK :draft/chathistory"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(cap.enabled(CHATHISTORY));

        // A server without chathistory is asked for none of its helpers.
        let mut cap = CapNegotiation::new(history(false), None, true);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS :batch server-time message-tags"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP END"]);

        // Batch declined: chathistory is not requested.
        let mut cap = CapNegotiation::new(history(false), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :batch draft/chathistory"))
            .unwrap();
        let step = cap.observe(&line(":s CAP * NAK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(!cap.enabled(CHATHISTORY));

        // chathistory declined (NAK): batch stays, nothing else happens.
        let mut cap = CapNegotiation::new(history(false), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :batch draft/chathistory"))
            .unwrap();
        cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        let step = cap
            .observe(&line(":s CAP * NAK :draft/chathistory"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(cap.enabled(BATCH) && !cap.enabled(CHATHISTORY));
        assert!(step.notes.iter().any(|note| note.contains("declined")));

        // Legacy encodings: no message-tags, the rest as usual.
        let mut cap = CapNegotiation::new(history(true), None, false);
        cap.start();
        let step = cap
            .observe(&line(
                ":s CAP * LS :batch server-time message-tags draft/chathistory",
            ))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time", "CAP REQ batch"]);

        // Losing batch after registration drops chathistory too.
        let mut cap = CapNegotiation::new(history(false), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :batch draft/chathistory"))
            .unwrap();
        cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        cap.observe(&line(":s CAP * ACK :draft/chathistory"))
            .unwrap();
        cap.registered();
        let step = cap.observe(&line(":s CAP me DEL :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ -draft/chathistory"]);
        assert!(!cap.enabled(CHATHISTORY));
        // Offered again later: batch first, then chathistory.
        let step = cap.observe(&line(":s CAP me NEW :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ batch"]);
        let step = cap.observe(&line(":s CAP me ACK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ draft/chathistory"]);
    }

    #[test]
    fn metadata_with_sasl_and_legacy_encodings() {
        let mut cap = CapNegotiation::new(avatars(true), Some(credentials()), false);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS :sasl batch draft/metadata-2"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ sasl", "CAP REQ batch"]);
        let step = cap
            .observe(&line(":s CAP * ACK :draft/metadata-2"))
            .unwrap();
        assert!(step.send.is_empty(), "an unrequested ACK changes nothing");
        assert!(!cap.enabled(METADATA));
        let step = cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ draft/metadata-2"]);
        cap.observe(&line(":s CAP * ACK :draft/metadata-2"))
            .unwrap();
        let step = cap.observe(&line(":s CAP * ACK :sasl")).unwrap();
        assert_eq!(sent(&step), ["AUTHENTICATE PLAIN"]);
        cap.observe(&line("AUTHENTICATE +")).unwrap();
        let step = cap.observe(&line(":s 903 * :ok")).unwrap();
        assert_eq!(sent(&step), ["CAP END"]);
        assert!(cap.enabled(METADATA));
    }

    #[test]
    fn losing_batch_or_metadata_after_registration_stops_metadata() {
        let registered = || {
            let mut cap = CapNegotiation::new(avatars(true), None, true);
            cap.start();
            cap.observe(&line(":s CAP * LS :batch draft/metadata-2"))
                .unwrap();
            cap.observe(&line(":s CAP * ACK :batch")).unwrap();
            cap.observe(&line(":s CAP * ACK :draft/metadata-2"))
                .unwrap();
            cap.registered();
            cap
        };
        let mut cap = registered();
        let step = cap.observe(&line(":s CAP me DEL :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ -draft/metadata-2"]);
        assert!(!cap.enabled(METADATA) && !cap.enabled(BATCH));
        let step = cap
            .observe(&line(":s CAP me ACK :-draft/metadata-2"))
            .unwrap();
        assert!(step.send.is_empty());
        // Batch returns: both are requested again, in order.
        let step = cap.observe(&line(":s CAP me NEW :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ batch"]);
        let step = cap.observe(&line(":s CAP me ACK :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ draft/metadata-2"]);

        let mut cap = registered();
        let step = cap
            .observe(&line(":s CAP me DEL :draft/metadata-2"))
            .unwrap();
        assert!(step.send.is_empty());
        assert!(!cap.enabled(METADATA) && cap.enabled(BATCH));
        let step = cap
            .observe(&line(":s CAP me NEW :draft/metadata-2"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ draft/metadata-2"]);
        cap.observe(&line(":s CAP me ACK :draft/metadata-2"))
            .unwrap();
        assert!(cap.enabled(METADATA));

        // An ACK disabling batch counts like DEL.
        let mut cap = registered();
        let step = cap.observe(&line(":s CAP me ACK :-batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ -draft/metadata-2"]);
        assert!(!cap.enabled(METADATA));
    }
}
