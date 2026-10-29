// SPDX-License-Identifier: MPL-2.0
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

use irc::proto::{CapSubCommand, Command as IrcCommand, Message as IrcMessage};

use crate::sasl::{Out, SaslHandshake};
use crate::scram;

pub const SASL: &str = "sasl";
pub const MESSAGE_TAGS: &str = "message-tags";
pub const SERVER_TIME: &str = "server-time";
pub const BATCH: &str = "batch";
/// The server sends our own messages back with their final form.
pub const ECHO_MESSAGE: &str = "echo-message";
/// Replies to a command carry its `label`; needs `batch` and `message-tags`.
pub const LABELED_RESPONSE: &str = "labeled-response";
/// `ACCOUNT` messages for users sharing a channel with us.
pub const ACCOUNT_NOTIFY: &str = "account-notify";
/// The account and real name in other users' `JOIN`.
pub const EXTENDED_JOIN: &str = "extended-join";
/// The sender's services account on each message. Requested only when a
/// negotiation happens anyway, and not on legacy encodings (values are UTF-8).
pub const ACCOUNT_TAG: &str = "account-tag";
/// Change the realname on a live connection. Requested only when a
/// negotiation happens anyway, so plain registration is unchanged.
pub const SETNAME: &str = "setname";
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
/// Maximum capabilities retained while a multiline ACK is in progress.
const MAX_ACK_CAPABILITIES: usize = MAX_OFFERED;
/// Maximum bytes retained across a multiline ACK.
const MAX_ACK_BYTES: usize = crate::tags::MAX_TAG_BYTES;

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
    /// Sent messages are confirmed by the server: `echo-message`, and with
    /// `labeled-response` (and its `batch` and `message-tags`, on UTF-8
    /// connections) exact matching of each echo to its local line. Both
    /// capabilities are standard; the behavior change makes this opt-in.
    pub confirmed_sending: bool,
    /// Follow the services accounts and real names of the users we share a
    /// channel with: `account-notify` and `extended-join`, plus a WHOX query
    /// per joined channel where the server has WHOX. Both capabilities are
    /// standard; the query makes this opt-in.
    pub accounts: bool,
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
#[derive(Debug, Default)]
pub struct CapStep {
    pub send: Vec<IrcCommand>,
    pub notes: Vec<String>,
    /// SCRAM key derivation the caller runs off the async runtime and
    /// answers with [`CapNegotiation::derived`].
    pub(crate) derive: Option<scram::Job>,
}

impl CapStep {
    fn take(&mut self, out: Out) {
        self.send.extend(out.send);
        self.notes.extend(out.notes);
        self.derive = out.derive.or(self.derive.take());
    }
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
    ack_bytes: usize,
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
        if (options.message_tags || options.chathistory || options.confirmed_sending) && utf8 {
            optional.push(MESSAGE_TAGS);
        }
        if options.message_tags && utf8 {
            explicit.push(MESSAGE_TAGS);
        }
        if options.server_time || options.chathistory || options.confirmed_sending {
            optional.push(SERVER_TIME);
        }
        if options.server_time {
            explicit.push(SERVER_TIME);
        }
        // Batch references and the history types are ASCII, like server-time.
        if options.batch
            || options.metadata
            || options.chathistory
            || (options.confirmed_sending && utf8)
        {
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
        if options.confirmed_sending {
            optional.push(ECHO_MESSAGE);
            explicit.push(ECHO_MESSAGE);
            // Labels ride on message tags and their multi-message replies
            // on batches; without both only the echo is used.
            if utf8 {
                optional.push(LABELED_RESPONSE);
                explicit.push(LABELED_RESPONSE);
            }
        }
        if options.accounts {
            optional.push(ACCOUNT_NOTIFY);
            explicit.push(ACCOUNT_NOTIFY);
            optional.push(EXTENDED_JOIN);
            explicit.push(EXTENDED_JOIN);
        }
        // Likewise chathistory: without batch its replies would look live.
        // Its references are ASCII, so legacy encodings may use it.
        if options.chathistory {
            optional.push(CHATHISTORY);
            explicit.push(CHATHISTORY);
        }
        // Extensions that are asked for only when a negotiation happens
        // anyway, so plain registration stays as it was.
        if sasl.is_some() || !optional.is_empty() {
            if utf8 {
                optional.push(ACCOUNT_TAG);
                explicit.push(ACCOUNT_TAG);
            }
            optional.push(SETNAME);
            explicit.push(SETNAME);
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
            ack_bytes: 0,
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

    /// Registration completed (001). SASL must have completed first when it
    /// was configured; NEW and DEL are still followed afterward.
    pub fn registered(&mut self) -> Result<(), &'static str> {
        if self.sasl.as_ref().is_some_and(|sasl| !sasl.complete()) {
            return Err("SASL authentication did not complete before registration.");
        }
        self.sasl = None;
        if self.phase != Phase::Inactive {
            self.phase = Phase::Ended;
        }
        Ok(())
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
                    self.append_ack(&list)?;
                    if !continued {
                        let acknowledged = std::mem::take(&mut self.ack_lines);
                        self.ack_bytes = 0;
                        self.acknowledge(&acknowledged, &mut step)?;
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
            step.take(sasl.observe(message)?);
        }
        self.enforce_dependencies(&mut step);
        self.end_if_settled(&mut step);
        Ok(step)
    }

    fn append_ack(&mut self, list: &str) -> Result<(), String> {
        let tokens = list.split_whitespace();
        let mut token_count = 0usize;
        for token in tokens.clone() {
            token_count += 1;
            if token.len() > MAX_VALUE_BYTES {
                self.ack_lines.clear();
                self.ack_bytes = 0;
                return Err("Server sent an oversized CAP ACK token.".into());
            }
        }
        let Some(total_tokens) = self.ack_lines.len().checked_add(token_count) else {
            self.ack_lines.clear();
            self.ack_bytes = 0;
            return Err("Server sent too many CAP ACK tokens.".into());
        };
        let Some(total_bytes) = self.ack_bytes.checked_add(list.len()) else {
            self.ack_lines.clear();
            self.ack_bytes = 0;
            return Err("Server sent an oversized CAP ACK.".into());
        };
        if total_tokens > MAX_ACK_CAPABILITIES {
            self.ack_lines.clear();
            self.ack_bytes = 0;
            return Err("Server sent too many CAP ACK tokens.".into());
        }
        if total_bytes > MAX_ACK_BYTES {
            self.ack_lines.clear();
            self.ack_bytes = 0;
            return Err("Server sent an oversized CAP ACK.".into());
        }

        self.ack_lines.extend(tokens.map(str::to_owned));
        self.ack_bytes = total_bytes;
        Ok(())
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
            let value = match value {
                Some(value) if value.len() <= MAX_VALUE_BYTES => Some(value.to_owned()),
                // An explicit `sasl` list that is too long to keep must not
                // read as a bare `sasl` (any mechanism): keep it as an empty
                // list, which offers none.
                Some(_) if name == SASL => Some(String::new()),
                _ => None,
            };
            self.offered.insert(name.to_owned(), value);
        }
    }

    fn request_after_listing(&mut self, step: &mut CapStep) -> Result<(), String> {
        self.phase = Phase::Requesting;
        if let Some(sasl) = &self.sasl {
            let wanted = sasl.mechanism().name();
            let offered = match self.offered.get(SASL) {
                // CAP 3.1 servers list `sasl` without mechanisms.
                Some(None) => true,
                Some(Some(mechanisms)) => mechanisms
                    .split(',')
                    .any(|name| name.eq_ignore_ascii_case(wanted)),
                None => false,
            };
            if !offered {
                return Err(format!("Server does not offer SASL {wanted}."));
            }
            step.notes
                .push(format!("SASL {wanted} is offered; requesting sasl."));
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
                && (name != LABELED_RESPONSE || self.enabled.contains(MESSAGE_TAGS))
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

    fn acknowledge(&mut self, acknowledged: &[String], step: &mut CapStep) -> Result<(), String> {
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
                step.notes.push("Capability sasl enabled.".into());
                if let Some(sasl) = self.sasl.as_mut() {
                    step.take(sasl.begin()?);
                }
            } else {
                step.notes.push(format!("Capability {name} enabled."));
                if name == BATCH || name == MESSAGE_TAGS {
                    // Now that batch is on, metadata and chathistory may follow.
                    self.request_optional(step);
                }
            }
        }
        Ok(())
    }

    /// The finished SCRAM derivation (see [`CapStep::derive`]).
    pub(crate) fn derived(&mut self, derived: scram::Derived) -> Result<CapStep, String> {
        let mut step = CapStep::default();
        let sasl = self.sasl.as_mut().ok_or("Unexpected SCRAM result.")?;
        step.take(sasl.derived(derived)?);
        Ok(step)
    }

    /// What a failed SASL exchange recorded; an `Err` cannot carry notes.
    pub(crate) fn take_sasl_failure_notes(&mut self) -> Vec<String> {
        self.sasl
            .as_mut()
            .map(SaslHandshake::take_failure_notes)
            .unwrap_or_default()
    }

    /// Where SASL stands, for the diagnostics of a registration timeout.
    pub fn sasl_stage(&self) -> Option<String> {
        self.sasl
            .as_ref()
            .map(|sasl| format!("{}, {}", sasl.mechanism().name(), sasl.stage()))
    }

    /// draft/metadata-2 and draft/chathistory must not outlive batch (a DEL
    /// or `ACK -batch`): they are dropped at once, so nothing depends on
    /// them, and the server is asked to disable them too. A request still
    /// waiting is forgotten; its late ACK is then ignored as unrequested.
    fn enforce_dependencies(&mut self, step: &mut CapStep) {
        if self.enabled.contains(BATCH) {
            return;
        }
        for name in [METADATA, CHATHISTORY, LABELED_RESPONSE] {
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
    name == METADATA || name == CHATHISTORY || name == LABELED_RESPONSE
}

/// Extensions that make this client want `name` without its own opt-in.
fn dependents(name: &str) -> &'static [&'static str] {
    match name {
        BATCH => &[METADATA, CHATHISTORY, LABELED_RESPONSE],
        MESSAGE_TAGS => &[CHATHISTORY, ECHO_MESSAGE, LABELED_RESPONSE],
        SERVER_TIME => &[CHATHISTORY, ECHO_MESSAGE],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SaslCredentials, SaslMechanism};

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
            confirmed_sending: false,
            accounts: false,
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
            mechanism: Default::default(),
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
        assert!(step.send.is_empty() && step.notes.is_empty());
        let step = cap.observe(&line(":s CAP me NEW :server-time")).unwrap();
        assert!(step.send.is_empty() && step.notes.is_empty());
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
        assert!(cap.registered().is_ok());
    }

    #[test]
    fn registration_requires_configured_sasl_to_complete() {
        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        cap.observe(&line(":s CAP * LS :sasl")).unwrap();
        cap.observe(&line(":s CAP * ACK :sasl")).unwrap();
        cap.observe(&line("AUTHENTICATE +")).unwrap();

        assert_eq!(
            cap.registered(),
            Err("SASL authentication did not complete before registration.")
        );

        let mut cap = CapNegotiation::new(Ircv3Options::default(), None, true);
        assert!(cap.registered().is_ok());
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
    fn multiline_ack_limits_total_bytes_and_token_length() {
        let mut cap = CapNegotiation::new(options(true, true), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :server-time message-tags"))
            .unwrap();

        let longest = "x".repeat(MAX_VALUE_BYTES);
        cap.observe(&line(&format!(":s CAP * ACK * :{longest}")))
            .unwrap();
        for _ in 0..14 {
            let token = "x".repeat(MAX_VALUE_BYTES);
            cap.observe(&line(&format!(":s CAP * ACK * :{token}")))
                .unwrap();
        }
        let last = "x".repeat(MAX_ACK_BYTES - 15 * MAX_VALUE_BYTES);
        cap.observe(&line(&format!(":s CAP * ACK * :{last}")))
            .unwrap();
        assert_eq!(cap.ack_bytes, MAX_ACK_BYTES);
        assert_eq!(cap.ack_lines.len(), 16);

        let error = cap.observe(&line(":s CAP * ACK * :x")).unwrap_err();
        assert_eq!(error, "Server sent an oversized CAP ACK.");
        assert!(cap.ack_lines.is_empty());
        assert_eq!(cap.ack_bytes, 0);

        let mut cap = CapNegotiation::new(options(true, true), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :server-time message-tags"))
            .unwrap();
        let too_long = "x".repeat(MAX_VALUE_BYTES + 1);
        let error = cap
            .observe(&line(&format!(":s CAP * ACK * :{too_long}")))
            .unwrap_err();
        assert_eq!(error, "Server sent an oversized CAP ACK token.");
        assert!(cap.ack_lines.is_empty());
    }

    #[test]
    fn multiline_ack_limits_capability_count() {
        let mut cap = CapNegotiation::new(options(true, true), None, true);
        cap.start();
        cap.observe(&line(":s CAP * LS :server-time message-tags"))
            .unwrap();
        let tokens = (0..MAX_ACK_CAPABILITIES)
            .map(|n| format!("x{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        cap.observe(&line(&format!(":s CAP * ACK * :{tokens}")))
            .unwrap();
        assert_eq!(cap.ack_lines.len(), MAX_ACK_CAPABILITIES);

        let error = cap.observe(&line(":s CAP * ACK * :overflow")).unwrap_err();
        assert_eq!(error, "Server sent too many CAP ACK tokens.");
        assert!(cap.ack_lines.is_empty());
        assert_eq!(cap.ack_bytes, 0);
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
    fn the_chosen_mechanism_must_be_offered_and_is_never_replaced() {
        let with = |mechanism| SaslCredentials {
            mechanism,
            ..credentials()
        };
        for (mechanism, offered, name) in [
            (
                SaslMechanism::ScramSha1,
                "sasl=PLAIN,SCRAM-SHA-256",
                "SCRAM-SHA-1",
            ),
            (SaslMechanism::ScramSha256, "sasl=PLAIN", "SCRAM-SHA-256"),
            (
                SaslMechanism::ScramSha512,
                "sasl=SCRAM-SHA-256",
                "SCRAM-SHA-512",
            ),
            (SaslMechanism::Plain, "sasl=SCRAM-SHA-256", "PLAIN"),
        ] {
            let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(with(mechanism)), true);
            cap.start();
            cap.observe(&line(":s CAP * LS * :server-time")).unwrap();
            let error = cap
                .observe(&line(&format!(":s CAP * LS :{offered}")))
                .unwrap_err();
            assert_eq!(error, format!("Server does not offer SASL {name}."));
        }
        // The list may span lines, and names compare case-insensitively.
        let mut cap = CapNegotiation::new(
            Ircv3Options::default(),
            Some(with(SaslMechanism::ScramSha256)),
            true,
        );
        cap.start();
        cap.observe(&line(":s CAP * LS * :server-time")).unwrap();
        let step = cap
            .observe(&line(":s CAP * LS :sasl=plain,scram-sha-256"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ sasl"]);
    }

    #[test]
    fn an_oversized_sasl_list_does_not_read_as_a_bare_sasl() {
        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        let padding = "x".repeat(MAX_VALUE_BYTES + 8);
        let error = cap
            .observe(&line(&format!(":s CAP * LS :sasl=EXTERNAL,{padding}")))
            .unwrap_err();
        assert_eq!(error, "Server does not offer SASL PLAIN.");

        // Bare `sasl` and a plain list are still accepted.
        for offered in ["sasl", "sasl=PLAIN"] {
            let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
            cap.start();
            let step = cap
                .observe(&line(&format!(":s CAP * LS :{offered}")))
                .unwrap();
            assert_eq!(sent(&step), ["CAP REQ sasl"]);
        }
    }

    #[test]
    fn sasl_failures_remain_refusals() {
        let mut cap = CapNegotiation::new(options(true, true), Some(credentials()), true);
        cap.start();
        assert_eq!(
            cap.observe(&line(":s CAP * LS :server-time")).unwrap_err(),
            "Server does not offer SASL PLAIN."
        );

        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        assert!(cap.observe(&line(":s CAP * LS :sasl=EXTERNAL")).is_err());

        let mut cap = CapNegotiation::new(Ircv3Options::default(), Some(credentials()), true);
        cap.start();
        cap.observe(&line(":s CAP * LS :sasl")).unwrap();
        assert_eq!(
            cap.observe(&line(":s CAP * NAK :sasl")).unwrap_err(),
            "Server rejected SASL capability."
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
        cap.registered().unwrap();
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
        cap.registered().unwrap();
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

    // setname: requested only when a negotiation happens anyway and the
    // server offers it, so plain registration is unchanged.
    #[test]
    fn setname_is_requested_only_beside_other_negotiation() {
        let mut plain = CapNegotiation::new(Ircv3Options::default(), None, true);
        assert_eq!(String::from(&plain.start()), "CAP END");

        let mut cap = CapNegotiation::new(options(false, true), None, true);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS :server-time setname"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time", "CAP REQ setname"]);
        cap.observe(&line(":s CAP * ACK :setname")).unwrap();
        assert!(cap.enabled(SETNAME));

        let mut without = CapNegotiation::new(options(false, true), None, true);
        without.start();
        let step = without.observe(&line(":s CAP * LS :server-time")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time"]);
        without.observe(&line(":s CAP * ACK :server-time")).unwrap();
        assert!(!without.enabled(SETNAME));
    }

    #[test]
    fn confirmed_sending_asks_for_echo_and_labels_with_their_dependencies() {
        let sending = Ircv3Options {
            confirmed_sending: true,
            ..Ircv3Options::default()
        };
        let mut cap = CapNegotiation::new(sending, None, true);
        assert_eq!(String::from(&cap.start()), "CAP LS 302");
        let step = cap
            .observe(&line(
                ":s CAP * LS :echo-message labeled-response batch message-tags server-time",
            ))
            .unwrap();
        // labeled-response waits for batch and message-tags.
        assert_eq!(
            sent(&step),
            [
                "CAP REQ message-tags",
                "CAP REQ server-time",
                "CAP REQ batch",
                "CAP REQ echo-message"
            ]
        );
        cap.observe(&line(":s CAP * ACK :batch")).unwrap();
        let step = cap.observe(&line(":s CAP * ACK :message-tags")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ labeled-response"]);
        // Losing batch drops labeled-response, not echo-message.
        cap.observe(&line(":s CAP * ACK :labeled-response echo-message"))
            .unwrap();
        assert!(cap.enabled(LABELED_RESPONSE) && cap.enabled(ECHO_MESSAGE));
        let step = cap.observe(&line(":s CAP * DEL :batch")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ -labeled-response"]);
        assert!(cap.enabled(ECHO_MESSAGE) && !cap.enabled(LABELED_RESPONSE));

        // A server with echo-message only: no labels, no helpers.
        let mut echo_only = CapNegotiation::new(sending, None, true);
        echo_only.start();
        let step = echo_only
            .observe(&line(":s CAP * LS :echo-message"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ echo-message"]);

        // Legacy encodings: echo-message alone (tags carry UTF-8).
        let mut legacy = CapNegotiation::new(sending, None, false);
        legacy.start();
        let step = legacy
            .observe(&line(
                ":s CAP * LS :echo-message labeled-response batch message-tags",
            ))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ echo-message"]);

        // Off: plain registration as before.
        let mut off = CapNegotiation::new(Ircv3Options::default(), None, true);
        assert_eq!(String::from(&off.start()), "CAP END");
    }

    #[test]
    fn user_accounts_request_account_notify_and_extended_join_when_offered() {
        let accounts = Ircv3Options {
            accounts: true,
            ..Ircv3Options::default()
        };
        let mut cap = CapNegotiation::new(accounts, None, true);
        assert_eq!(String::from(&cap.start()), "CAP LS 302");
        let step = cap
            .observe(&line(
                ":s CAP * LS :account-notify extended-join away-notify",
            ))
            .unwrap();
        assert_eq!(
            sent(&step),
            ["CAP REQ account-notify", "CAP REQ extended-join"]
        );
        // Either one alone is fine; a legacy encoding needs no exception
        // (no tag values are involved).
        let mut legacy = CapNegotiation::new(accounts, None, false);
        legacy.start();
        let step = legacy.observe(&line(":s CAP * LS :extended-join")).unwrap();
        assert_eq!(sent(&step), ["CAP REQ extended-join"]);
        // Off: plain registration, as before.
        let mut off = CapNegotiation::new(Ircv3Options::default(), None, true);
        assert_eq!(String::from(&off.start()), "CAP END");
    }

    #[test]
    fn account_tag_rides_along_with_negotiation_on_utf8_connections_only() {
        let mut plain = CapNegotiation::new(Ircv3Options::default(), None, true);
        assert_eq!(String::from(&plain.start()), "CAP END");

        let mut cap = CapNegotiation::new(options(false, true), None, true);
        cap.start();
        let step = cap
            .observe(&line(":s CAP * LS :server-time account-tag"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time", "CAP REQ account-tag"]);

        let mut legacy = CapNegotiation::new(options(false, true), None, false);
        legacy.start();
        let step = legacy
            .observe(&line(":s CAP * LS :server-time account-tag"))
            .unwrap();
        assert_eq!(sent(&step), ["CAP REQ server-time"]);
    }

    #[test]
    fn registration_without_cap_support_ends_negotiation() {
        let mut cap = CapNegotiation::new(options(true, true), None, true);
        cap.start();
        cap.observe(&line(":s 421 * CAP :Unknown command")).unwrap();
        assert!(cap.negotiating());
        cap.registered().unwrap();
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
        cap.registered().unwrap();
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
                mechanism: Default::default(),
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
        cap.registered().unwrap();
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
            cap.registered().unwrap();
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
