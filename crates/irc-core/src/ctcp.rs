// SPDX-License-Identifier: MPL-2.0
//! Answers to common CTCP queries (D029).
//!
//! A PRIVMSG or NOTICE whose text starts with 0x01 is a CTCP message. ACTION
//! stays chat, and CTCP AVATAR belongs to `peer_avatar` while peer avatars
//! are on; every other CTCP message is consumed here, so it never makes a
//! chat row, private message, mention or notification. It becomes one
//! readable server line instead, such as `CTCP VERSION request from bob`.
//!
//! Only live private requests from a user are answered, by NOTICE to the
//! sender: PING echoes its argument, VERSION gives the client name and
//! version (no OS or host), TIME the local time, USERINFO, FINGER and SOURCE
//! fixed public text (no login name, host or idle time), ERRMSG an
//! "no error" answer echoing the query, and CLIENTINFO the tags answered here
//! plus ACTION (and AVATAR while peer avatars are on). An unknown but
//! plainly spelled tag gets `ERRMSG <tag> :Unknown query`. DCC (never
//! interpreted) and AVATAR while peer avatars are off get no answer.
//! Replies are NOTICEs and are never answered, so ERRMSG cannot loop.
//! Channel requests, replayed history, our own echoes and requests from
//! servers are never answered.
//!
//! [`CtcpReplies::request`] sends our own requests (`/ctcp`, `/ping`). A
//! CTCP PING with no arguments carries a token and is timed with the
//! monotonic clock; only a NOTICE from the same nick echoing the token
//! within [`PING_TIMEOUT`] is shown as a round trip time. Pending pings are
//! bounded and our requests are paced like incoming ones.
//!
//! Requests are rate-limited like CTCP AVATAR answers, so a flood cannot
//! make us exceed the server's flood limit: at most five per ten seconds in
//! total and two per user, whether answered or only shown. Past the limit
//! requests are dropped without an answer or a line, apart from one line
//! per ten seconds saying so. CTCP replies (NOTICEs) are shown under their own limit of the
//! same size.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Prefix};
use tokio::time::Instant;

use crate::{
    Event,
    metadata::Handled,
    text::{nickname_key, strip_formatting},
    valid_channel,
};
use cayenchat_model::names::CaseMapping;

const WINDOW: Duration = Duration::from_secs(10);
/// Requests handled (answered or shown) per [`WINDOW`] in total and per user.
const MAX_PER_WINDOW: usize = 5;
const MAX_PER_USER: usize = 2;
/// A PING argument longer than this is not echoed.
const MAX_PING_BYTES: usize = 128;
/// Longest tag shown; a longer or unusual one is shown as `CTCP request`.
const MAX_TAG_CHARS: usize = 32;
/// Reply parameters shown in the server log are cut to this many characters.
const MAX_SHOWN_CHARS: usize = 200;
const VERSION: &str = concat!("CayenChat ", env!("CARGO_PKG_VERSION"));
const SOURCE: &str = "https://github.com/koshian/cayenchat";
/// A ping with no matching reply after this long is reported and dropped.
const PING_TIMEOUT: Duration = Duration::from_secs(30);
/// Pings waiting for a reply at once.
const MAX_PENDING: usize = 8;
/// Longest argument list of a request we send, in bytes.
const MAX_REQUEST_BYTES: usize = 200;

/// A bounded count of recent events, in total and per user. Only admitted
/// events are remembered, so the per-user table stays within the total.
#[derive(Debug, Default)]
struct Budget {
    recent: VecDeque<Instant>,
    per_user: HashMap<String, VecDeque<Instant>>,
    /// When a refusal was last reported; one report per [`WINDOW`].
    reported: Option<Instant>,
}

enum Admission {
    Admitted,
    /// A refusal to report: the first one in a window.
    Refused,
    Muted,
}

impl Budget {
    fn admit(&mut self, sender: &str, now: Instant) -> Admission {
        let fresh = |sent: &Instant| now.duration_since(*sent) < WINDOW;
        while self.recent.front().is_some_and(|sent| !fresh(sent)) {
            self.recent.pop_front();
        }
        self.per_user.retain(|_, times| {
            times.retain(fresh);
            !times.is_empty()
        });
        let key = nickname_key(sender);
        let used = self.per_user.get(&key).map_or(0, VecDeque::len);
        if self.recent.len() >= MAX_PER_WINDOW || used >= MAX_PER_USER {
            if self.reported.is_some_and(|reported| fresh(&reported)) {
                return Admission::Muted;
            }
            self.reported = Some(now);
            return Admission::Refused;
        }
        self.recent.push_back(now);
        self.per_user.entry(key).or_default().push_back(now);
        Admission::Admitted
    }
}

/// A CTCP PING we sent and still wait for.
#[derive(Debug)]
struct Pending {
    nick: String,
    token: String,
    sent: Instant,
}

#[derive(Debug)]
pub(crate) struct CtcpReplies {
    /// CTCP AVATAR is answered (by `peer_avatar`) on this connection.
    avatar: bool,
    /// The server named in ping results.
    network: String,
    requests: Budget,
    replies: Budget,
    /// Requests we send.
    sent: Budget,
    /// Oldest first.
    pending: VecDeque<Pending>,
    nonce: u64,
    counter: u64,
    /// How the server compares nicknames; set from ISUPPORT.
    casemapping: CaseMapping,
    local_time: fn() -> String,
}

impl CtcpReplies {
    pub(crate) fn new(avatar: bool, network: &str) -> Self {
        use std::hash::{BuildHasher, Hasher};
        Self {
            avatar,
            network: plain(network),
            requests: Budget::default(),
            replies: Budget::default(),
            sent: Budget::default(),
            pending: VecDeque::new(),
            nonce: std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish(),
            counter: 0,
            casemapping: CaseMapping::default(),
            local_time,
        }
    }

    pub(crate) fn set_casemapping(&mut self, casemapping: CaseMapping) {
        self.casemapping = casemapping;
    }

    /// Sends our own CTCP request `tag` with `args` to `nick`. `PING` with no
    /// arguments is timed. A refusal is one server line and nothing is sent.
    pub(crate) fn request(&mut self, nick: &str, tag: &str, args: &str, now: Instant) -> Handled {
        let mut handled = Handled::default();
        let tag = tag.to_ascii_uppercase();
        let refuse = |handled: &mut Handled, why: &str| {
            handled.events.push(Event::CtcpLine(format!(
                "CTCP {tag} request to {} not sent: {why}",
                plain(nick)
            )));
        };
        self.expire(now, &mut handled);
        let timed = tag == "PING" && args.is_empty();
        if timed
            && self
                .pending
                .iter()
                .any(|pending| self.casemapping.same(&pending.nick, nick))
        {
            refuse(&mut handled, "still waiting for the previous ping");
            return handled;
        }
        if timed && self.pending.len() >= MAX_PENDING {
            refuse(&mut handled, "too many pings are waiting for a reply");
            return handled;
        }
        if !matches!(self.sent.admit(nick, now), Admission::Admitted) {
            refuse(&mut handled, "too many requests; wait a few seconds");
            return handled;
        }
        let body = if timed {
            self.counter += 1;
            let token = format!("{:08x}{:x}", self.nonce as u32, self.counter);
            self.pending.push_back(Pending {
                nick: nick.to_owned(),
                token: token.clone(),
                sent: now,
            });
            format!("PING {token}")
        } else if args.is_empty() {
            tag.clone()
        } else {
            format!("{tag} {args}")
        };
        handled.send.push(IrcCommand::PRIVMSG(
            nick.to_owned(),
            format!("\u{1}{body}\u{1}"),
        ));
        handled.events.push(Event::CtcpLine(format!(
            "CTCP {tag} request sent to {}",
            plain(nick)
        )));
        handled
    }

    /// When the oldest ping times out.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .front()
            .map(|pending| pending.sent + PING_TIMEOUT)
    }

    /// Reports and drops the pings that timed out.
    pub(crate) fn tick(&mut self, now: Instant) -> Handled {
        let mut handled = Handled::default();
        self.expire(now, &mut handled);
        handled
    }

    fn expire(&mut self, now: Instant, handled: &mut Handled) {
        while self
            .pending
            .front()
            .is_some_and(|pending| now.duration_since(pending.sent) >= PING_TIMEOUT)
        {
            let pending = self.pending.pop_front().expect("checked");
            handled.events.push(Event::CtcpLine(format!(
                "CTCP PING to {} on {} timed out (no reply in {} seconds)",
                plain(&pending.nick),
                self.network,
                PING_TIMEOUT.as_secs()
            )));
        }
    }

    /// The round trip line for a reply that answers one of our pings.
    fn round_trip(&mut self, sender: &str, params: &str, now: Instant) -> Option<String> {
        let casemapping = self.casemapping;
        let index = self.pending.iter().position(|pending| {
            pending.token == params
                && casemapping.same(&pending.nick, sender)
                // A reply past the deadline is never a measurement, whether
                // or not the timer has run yet.
                && now.duration_since(pending.sent) < PING_TIMEOUT
        })?;
        let pending = self.pending.remove(index)?;
        let millis = now.duration_since(pending.sent).as_secs_f64() * 1000.0;
        Some(format!(
            "CTCP PING reply from {} on {}: {millis:.1} ms",
            plain(&pending.nick),
            self.network
        ))
    }

    /// Handles a CTCP message other than ACTION. `Some` means the message
    /// is consumed: it must not become a chat row or a raw server line.
    pub(crate) fn observe(
        &mut self,
        message: &IrcMessage,
        current_nick: &str,
        replayed: bool,
        now: Instant,
    ) -> Option<Handled> {
        let (target, text, request) = match &message.command {
            IrcCommand::PRIVMSG(target, text) => (target, text, true),
            IrcCommand::NOTICE(target, text) => (target, text, false),
            _ => return None,
        };
        let (tag, params) = split(text)?;
        if tag.eq_ignore_ascii_case("ACTION") {
            return None;
        }
        let mut handled = Handled::default();
        let (sender, user) = match &message.prefix {
            Some(Prefix::Nickname(nick, _, _)) => (nick.as_str(), true),
            Some(Prefix::ServerName(name)) => (name.as_str(), false),
            None => ("server", false),
        };
        if replayed || (user && self.casemapping.same(sender, current_nick)) {
            return Some(handled);
        }
        // STATUSMSG (`@#chan`) reaches part of a channel; still a channel.
        let to_channel = valid_channel(target.trim_start_matches(['~', '@', '%', '+']));
        if !to_channel && !self.casemapping.same(target, current_nick) {
            return Some(handled);
        }
        if !request
            && user
            && !to_channel
            && tag.eq_ignore_ascii_case("PING")
            && let Some(line) = self.round_trip(sender, params, now)
        {
            handled.events.push(Event::CtcpLine(line));
            return Some(handled);
        }
        let shown_tag = shown_tag(tag);
        let shown_sender = plain(sender);
        let to = if to_channel {
            format!(" to {}", plain(target))
        } else {
            String::new()
        };
        let budget = if request {
            &mut self.requests
        } else {
            &mut self.replies
        };
        match budget.admit(sender, now) {
            Admission::Admitted => {}
            Admission::Refused => {
                handled.events.push(Event::CtcpLine(if request {
                    format!("Too many CTCP requests; ignoring them for now (latest {shown_tag} from {shown_sender}).")
                } else {
                    format!("Too many CTCP replies; hiding them for now (latest {shown_tag} from {shown_sender}).")
                }));
                return Some(handled);
            }
            Admission::Muted => return Some(handled),
        }
        if !request {
            let params = shown_params(params);
            let params = if params.is_empty() {
                String::new()
            } else {
                format!(": {params}")
            };
            handled.events.push(Event::CtcpLine(format!(
                "{shown_tag} reply from {shown_sender}{to}{params}"
            )));
            return Some(handled);
        }
        let answer = (user && !to_channel && crate::valid_nickname(sender))
            .then(|| self.answer(tag, params))
            .flatten();
        let line = format!("{shown_tag} request from {shown_sender}{to}");
        match answer {
            Some(answer) => {
                handled.send.push(IrcCommand::NOTICE(
                    sender.to_owned(),
                    format!("\u{1}{answer}\u{1}"),
                ));
                handled.events.push(Event::CtcpLine(line));
            }
            None => handled
                .events
                .push(Event::CtcpLine(format!("{line} (not answered)"))),
        }
        Some(handled)
    }

    /// The answer's CTCP body (without 0x01) for a private request.
    fn answer(&self, tag: &str, params: &str) -> Option<String> {
        match tag.to_ascii_uppercase().as_str() {
            "PING" if params.is_empty() => Some("PING".into()),
            "PING" if echoable(params) => Some(format!("PING {params}")),
            "PING" => None,
            "VERSION" => Some(format!("VERSION {VERSION}")),
            "TIME" => Some(format!("TIME {}", (self.local_time)())),
            "CLIENTINFO" => Some(format!("CLIENTINFO {}", self.client_info())),
            "USERINFO" => Some("USERINFO CayenChat user".into()),
            "FINGER" => Some("FINGER CayenChat user".into()),
            "SOURCE" => Some(format!("SOURCE {SOURCE}")),
            "ERRMSG" if !echoable(params) => None,
            "ERRMSG" if params.is_empty() => Some("ERRMSG ERRMSG :No error".into()),
            "ERRMSG" => Some(format!("ERRMSG {params} :No error")),
            // Never interpreted, and AVATAR follows the peer avatar setting.
            "DCC" | "AVATAR" => None,
            known => (is_plain_tag(tag)).then(|| format!("ERRMSG {known} :Unknown query")),
        }
    }

    fn client_info(&self) -> &'static str {
        if self.avatar {
            "ACTION AVATAR CLIENTINFO ERRMSG FINGER PING SOURCE TIME USERINFO VERSION"
        } else {
            "ACTION CLIENTINFO ERRMSG FINGER PING SOURCE TIME USERINFO VERSION"
        }
    }
}

/// Whether a PING or ERRMSG argument may be echoed back: short and free of
/// control characters, as `/ctcp` requires of what we send ourselves.
fn echoable(params: &str) -> bool {
    params.len() <= MAX_PING_BYTES && !params.chars().any(char::is_control)
}

/// The tag and parameters of a CTCP message, or `None` for other text. The
/// closing 0x01 is optional, as in other clients.
fn split(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_prefix('\u{1}')?;
    let body = body.strip_suffix('\u{1}').unwrap_or(body);
    Some(body.split_once(' ').unwrap_or((body, "")))
}

/// Whether `/ctcp` may send this request: a nickname target, a plain tag
/// that is not ACTION (`/me`), DCC (unsupported) or AVATAR (follows the
/// peer avatar setting), and short arguments without control characters, so
/// nothing can end the CTCP message or start another command.
pub(crate) fn check_request(nick: &str, tag: &str, args: &str) -> Result<(), String> {
    if !crate::valid_nickname(nick) {
        return Err("Invalid CTCP target; give a nickname.".into());
    }
    if !is_plain_tag(tag) {
        return Err("Invalid CTCP command.".into());
    }
    match tag.to_ascii_uppercase().as_str() {
        "ACTION" => return Err("Use /me to send an action.".into()),
        "DCC" => return Err("DCC is not supported.".into()),
        "AVATAR" => return Err("CTCP AVATAR follows the peer avatar setting.".into()),
        _ => {}
    }
    if args.len() > MAX_REQUEST_BYTES {
        return Err(format!(
            "CTCP arguments must be at most {MAX_REQUEST_BYTES} bytes."
        ));
    }
    if args.chars().any(char::is_control) {
        return Err("CTCP arguments must not contain control characters.".into());
    }
    Ok(())
}

/// A tag spelled plainly enough to show and to echo back.
pub(crate) fn is_plain_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_TAG_CHARS
        && tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'))
}

fn shown_tag(tag: &str) -> String {
    if is_plain_tag(tag) {
        format!("CTCP {}", tag.to_ascii_uppercase())
    } else {
        "CTCP".into()
    }
}

/// Untrusted text as plain text for the server log.
fn plain(text: &str) -> String {
    strip_formatting(text)
        .chars()
        .filter(|ch| !ch.is_control())
        .collect()
}

/// Reply parameters as plain text for the server log, cut when long.
fn shown_params(params: &str) -> String {
    let plain = plain(params);
    let plain = plain.trim();
    match plain.char_indices().nth(MAX_SHOWN_CHARS) {
        Some((cut, _)) => format!("{}…", &plain[..cut]),
        None => plain.to_owned(),
    }
}

fn local_time() -> String {
    chrono::Local::now()
        .format("%a %b %e %H:%M:%S %Y %z")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "alice";

    fn fixed_time() -> String {
        "Mon Sep 28 12:34:56 2026 +0900".into()
    }

    fn replies() -> CtcpReplies {
        CtcpReplies {
            local_time: fixed_time,
            ..CtcpReplies::new(false, "irc.test")
        }
    }

    fn observe(ctcp: &mut CtcpReplies, text: &str, now: Instant) -> Option<Handled> {
        ctcp.observe(&text.parse().unwrap(), ME, false, now)
    }

    fn wire(handled: &Handled) -> Vec<String> {
        handled
            .send
            .iter()
            .map(|command| {
                IrcMessage::from(command.clone())
                    .to_string()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn lines(handled: &Handled) -> Vec<String> {
        handled
            .events
            .iter()
            .map(|event| match event {
                Event::CtcpLine(line) => line.clone(),
                other => panic!("unexpected event {other:?}"),
            })
            .collect()
    }

    #[test]
    fn private_requests_are_answered_by_notice() {
        let now = Instant::now();
        let cases = [
            (
                "\u{1}PING 1727490000 123\u{1}",
                "NOTICE bob :\u{1}PING 1727490000 123\u{1}",
            ),
            ("\u{1}PING\u{1}", "NOTICE bob \u{1}PING\u{1}"),
            ("\u{1}ping 42", "NOTICE bob :\u{1}PING 42\u{1}"),
            (
                "\u{1}VERSION\u{1}",
                concat!(
                    "NOTICE bob :\u{1}VERSION CayenChat ",
                    env!("CARGO_PKG_VERSION"),
                    "\u{1}"
                ),
            ),
            (
                "\u{1}TIME\u{1}",
                "NOTICE bob :\u{1}TIME Mon Sep 28 12:34:56 2026 +0900\u{1}",
            ),
            (
                "\u{1}CLIENTINFO\u{1}",
                "NOTICE bob :\u{1}CLIENTINFO ACTION CLIENTINFO ERRMSG FINGER PING SOURCE TIME USERINFO VERSION\u{1}",
            ),
            (
                "\u{1}USERINFO\u{1}",
                "NOTICE bob :\u{1}USERINFO CayenChat user\u{1}",
            ),
            (
                "\u{1}FINGER\u{1}",
                "NOTICE bob :\u{1}FINGER CayenChat user\u{1}",
            ),
            (
                "\u{1}SOURCE\u{1}",
                "NOTICE bob :\u{1}SOURCE https://github.com/koshian/cayenchat\u{1}",
            ),
            (
                "\u{1}ERRMSG PING :x\u{1}",
                "NOTICE bob :\u{1}ERRMSG PING :x :No error\u{1}",
            ),
            (
                "\u{1}ERRMSG\u{1}",
                "NOTICE bob :\u{1}ERRMSG ERRMSG :No error\u{1}",
            ),
            (
                "\u{1}Frobnicate now\u{1}",
                "NOTICE bob :\u{1}ERRMSG FROBNICATE :Unknown query\u{1}",
            ),
        ];
        for (index, (request, answer)) in cases.into_iter().enumerate() {
            // A fresh window per case keeps the rate limit out of the way.
            let now = now + WINDOW * index as u32;
            let mut ctcp = replies();
            let handled = observe(
                &mut ctcp,
                &format!(":bob!u@h PRIVMSG alice :{request}"),
                now,
            )
            .unwrap();
            assert_eq!(wire(&handled), [answer], "{request:?}");
            assert_eq!(lines(&handled).len(), 1);
        }
        let mut ctcp = replies();
        let handled = observe(&mut ctcp, ":bob!u@h PRIVMSG ALICE :\u{1}VERSION\u{1}", now).unwrap();
        assert_eq!(lines(&handled), ["CTCP VERSION request from bob"]);
        // CLIENTINFO names AVATAR only where it is answered.
        let mut ctcp = CtcpReplies::new(true, "irc.test");
        let handled = observe(
            &mut ctcp,
            ":bob!u@h PRIVMSG alice :\u{1}CLIENTINFO\u{1}",
            now,
        )
        .unwrap();
        assert_eq!(
            wire(&handled),
            [
                "NOTICE bob :\u{1}CLIENTINFO ACTION AVATAR CLIENTINFO ERRMSG FINGER PING SOURCE TIME USERINFO VERSION\u{1}"
            ]
        );
        // The real clock gives a non-empty time.
        assert!(!local_time().is_empty());
    }

    #[test]
    fn other_requests_are_shown_but_not_answered() {
        let now = Instant::now();
        let long_ping = format!(":bob!u@h PRIVMSG alice :\u{1}PING {}\u{1}", "9".repeat(129));
        let cases = [
            (
                ":bob!u@h PRIVMSG alice :\u{1}WEIRD\u{2}\u{1}",
                "CTCP request from bob (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG alice :\u{1}DCC SEND x.txt 1 2 3\u{1}",
                "CTCP DCC request from bob (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG alice :\u{1}AVATAR\u{1}",
                "CTCP AVATAR request from bob (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG #a :\u{1}VERSION\u{1}",
                "CTCP VERSION request from bob to #a (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG @#a :\u{1}VERSION\u{1}",
                "CTCP VERSION request from bob to @#a (not answered)",
            ),
            (
                ":irc.example.net PRIVMSG alice :\u{1}VERSION\u{1}",
                "CTCP VERSION request from irc.example.net (not answered)",
            ),
            (&long_ping, "CTCP PING request from bob (not answered)"),
            (
                ":bob!u@h PRIVMSG alice :\u{1}PING a\rb\u{1}",
                "CTCP PING request from bob (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG alice :\u{1}PING a\0b\u{1}",
                "CTCP PING request from bob (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG alice :\u{1}PING a\u{1b}[2Jb\u{1}",
                "CTCP PING request from bob (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG alice :\u{1}ERRMSG a\rb\u{1}",
                "CTCP ERRMSG request from bob (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG alice :\u{1}ERRMSG a\u{7}b\u{1}",
                "CTCP ERRMSG request from bob (not answered)",
            ),
            (
                ":bob!u@h PRIVMSG alice :\u{1}\u{2}X\u{1}",
                "CTCP request from bob (not answered)",
            ),
            (
                ":bob!u@h NOTICE alice :\u{1}VERSION irssi \u{2}1.4\u{2}\u{1}",
                "CTCP VERSION reply from bob: irssi 1.4",
            ),
            (
                ":bob!u@h NOTICE #a :\u{1}PING\u{1}",
                "CTCP PING reply from bob to #a",
            ),
            (
                ":b\u{2}ob!u@h NOTICE alice :\u{1}PING\u{1}",
                "CTCP PING reply from bob",
            ),
        ];
        for (index, (text, line)) in cases.into_iter().enumerate() {
            let mut ctcp = replies();
            let handled = observe(&mut ctcp, text, now + WINDOW * index as u32).unwrap();
            assert!(handled.send.is_empty(), "{text:?}");
            assert_eq!(lines(&handled), [line], "{text:?}");
        }
        // Long reply text is cut.
        let mut ctcp = replies();
        let text = format!(
            ":bob!u@h NOTICE alice :\u{1}VERSION {}\u{1}",
            "x".repeat(300)
        );
        let line = &lines(&observe(&mut ctcp, &text, now).unwrap())[0];
        assert!(line.ends_with('…') && line.chars().count() < 240, "{line}");
    }

    #[test]
    fn replayed_own_and_foreign_messages_are_swallowed_silently() {
        let now = Instant::now();
        let mut ctcp = replies();
        let replayed = ctcp
            .observe(
                &":bob!u@h PRIVMSG alice :\u{1}VERSION\u{1}".parse().unwrap(),
                ME,
                true,
                now,
            )
            .unwrap();
        assert!(replayed.send.is_empty() && replayed.events.is_empty());
        for text in [
            ":alice!u@h PRIVMSG alice :\u{1}VERSION\u{1}",
            ":alice!u@h PRIVMSG bob :\u{1}VERSION\u{1}",
            ":bob!u@h PRIVMSG carol :\u{1}VERSION\u{1}",
        ] {
            let handled = observe(&mut ctcp, text, now).unwrap();
            assert!(
                handled.send.is_empty() && handled.events.is_empty(),
                "{text}"
            );
        }
        // ACTION and ordinary text are not ours.
        assert!(
            observe(
                &mut ctcp,
                ":bob!u@h PRIVMSG alice :\u{1}ACTION waves\u{1}",
                now
            )
            .is_none()
        );
        assert!(
            observe(
                &mut ctcp,
                ":bob!u@h PRIVMSG #a :\u{1}action waves\u{1}",
                now
            )
            .is_none()
        );
        assert!(observe(&mut ctcp, ":bob!u@h PRIVMSG alice :VERSION", now).is_none());
        assert!(observe(&mut ctcp, ":bob!u@h JOIN #a", now).is_none());
    }

    /// The token of the PING that `handled` sends.
    fn sent_token(handled: &Handled) -> String {
        wire(handled)[0]
            .trim_start_matches("PRIVMSG bob :\u{1}PING ")
            .trim_end_matches('\u{1}')
            .to_owned()
    }

    fn reply(sender: &str, target: &str, body: &str) -> String {
        format!(":{sender}!u@h NOTICE {target} :\u{1}{body}\u{1}")
    }

    #[test]
    fn requests_are_sent_and_ping_is_timed() {
        let now = Instant::now();
        let mut ctcp = replies();
        let handled = ctcp.request("bob", "version", "", now);
        assert_eq!(wire(&handled), ["PRIVMSG bob \u{1}VERSION\u{1}"]);
        assert_eq!(lines(&handled), ["CTCP VERSION request sent to bob"]);
        let handled = ctcp.request("bob", "FOO", "a b", now);
        assert_eq!(wire(&handled), ["PRIVMSG bob :\u{1}FOO a b\u{1}"]);
        assert!(ctcp.pending.is_empty());

        let mut ctcp = replies();
        let handled = ctcp.request("bob", "PING", "", now);
        let token = sent_token(&handled);
        assert!(!token.is_empty() && ctcp.next_deadline() == Some(now + PING_TIMEOUT));
        // Unrelated, wrong-token, other-nick and channel replies are not it.
        for (index, text) in [
            reply("bob", "alice", "PING 12345"),
            reply("carol", "alice", &format!("PING {token}")),
            reply("bob", "#a", &format!("PING {token}")),
            reply("bob", "alice", &format!("PING {token}x")),
            reply("bob", "alice", "VERSION x"),
        ]
        .into_iter()
        .enumerate()
        {
            // Spaced out so the reply limit stays out of the way.
            let at = now + Duration::from_secs(6 * index as u64);
            let line = &lines(&observe(&mut ctcp, &text, at).unwrap())[0];
            assert!(!line.contains(" ms"), "{line}");
        }
        assert_eq!(ctcp.pending.len(), 1);
        // The matching reply gives the round trip once, with the nick and
        // the network; a repeat is not a measurement.
        let at = now + Duration::from_millis(250);
        let text = reply("BOB", "alice", &format!("PING {token}"));
        let handled = observe(&mut ctcp, &text, at).unwrap();
        assert_eq!(
            lines(&handled),
            ["CTCP PING reply from bob on irc.test: 250.0 ms"]
        );
        assert!(ctcp.pending.is_empty() && ctcp.next_deadline().is_none());
        let again = &lines(&observe(&mut ctcp, &text, now + WINDOW * 10).unwrap())[0];
        assert!(!again.contains(" ms"), "{again}");
    }

    #[test]
    fn pings_are_bounded_expire_and_do_not_pile_up() {
        let now = Instant::now();
        let mut ctcp = replies();
        let first = ctcp.request("bob", "PING", "", now);
        let token = sent_token(&first);
        // One ping per nick at a time.
        let second = ctcp.request("Bob", "PING", "", now);
        assert!(second.send.is_empty());
        assert!(lines(&second)[0].contains("not sent"));
        // A late reply after the timeout is reported as timed out and the
        // reply itself is not a measurement.
        let late = now + PING_TIMEOUT;
        let tick = ctcp.tick(late);
        assert_eq!(
            lines(&tick),
            ["CTCP PING to bob on irc.test timed out (no reply in 30 seconds)"]
        );
        let line = &lines(
            &observe(
                &mut ctcp,
                &reply("bob", "alice", &format!("PING {token}")),
                late,
            )
            .unwrap(),
        )[0];
        assert!(!line.contains(" ms"), "{line}");
        // Pending pings and sent requests are limited.
        let mut ctcp = replies();
        let mut sent = 0;
        for index in 0..20 {
            let handled = ctcp.request(&format!("user{index}"), "PING", "", now);
            sent += handled.send.len();
            assert!(ctcp.pending.len() <= MAX_PENDING);
        }
        assert_eq!(sent, MAX_PER_WINDOW);
        // Room again in the next window.
        assert_eq!(
            ctcp.request("user30", "PING", "", now + WINDOW).send.len(),
            1
        );
    }

    #[test]
    fn late_reply_is_not_measured_even_before_the_timer_runs() {
        let now = Instant::now();
        let mut ctcp = replies();
        let token = sent_token(&ctcp.request("bob", "PING", "", now));
        let text = reply("bob", "alice", &format!("PING {token}"));
        // Just past the deadline, with no tick first.
        let late = now + PING_TIMEOUT;
        let line = &lines(&observe(&mut ctcp, &text, late).unwrap())[0];
        assert!(!line.contains(" ms"), "{line}");
        // Just inside it is a measurement.
        let mut ctcp = replies();
        let token = sent_token(&ctcp.request("bob", "PING", "", now));
        let text = reply("bob", "alice", &format!("PING {token}"));
        let early = now + PING_TIMEOUT - Duration::from_millis(1);
        let line = &lines(&observe(&mut ctcp, &text, early).unwrap())[0];
        assert!(line.contains("29999.0 ms"), "{line}");
    }

    #[test]
    fn ping_sender_follows_the_connection_casemapping() {
        let now = Instant::now();
        for (mapping, same) in [(CaseMapping::Ascii, false), (CaseMapping::Rfc1459, true)] {
            let mut ctcp = replies();
            ctcp.set_casemapping(mapping);
            let handled = ctcp.request("lat[", "PING", "", now);
            let token = wire(&handled)[0]
                .trim_start_matches("PRIVMSG lat[ :\u{1}PING ")
                .trim_end_matches('\u{1}')
                .to_owned();
            let again = ctcp.request("lat{", "PING", "", now);
            assert_eq!(again.send.is_empty(), same);
            let text = reply("lat{", "alice", &format!("PING {token}"));
            let line = &lines(&observe(&mut ctcp, &text, now).unwrap())[0];
            assert_eq!(line.contains(" ms"), same, "{line}");
        }
    }

    #[test]
    fn own_nick_exclusion_follows_the_connection_casemapping() {
        let now = Instant::now();
        for (mapping, own_echo) in [(CaseMapping::Ascii, false), (CaseMapping::Rfc1459, true)] {
            let mut ctcp = replies();
            ctcp.set_casemapping(mapping);
            let handled = ctcp.request("lat[", "PING", "", now);
            let token = wire(&handled)[0]
                .trim_start_matches("PRIVMSG lat[ :\u{1}PING ")
                .trim_end_matches('\u{1}')
                .to_owned();
            // We are `lat{`; the peer `lat[` answers.
            let text = reply("lat[", "lat{", &format!("PING {token}"));
            let handled = ctcp.observe(&text.parse().unwrap(), "lat{", false, now);
            let line = &lines(&handled.unwrap());
            assert_eq!(line.is_empty(), own_echo, "{line:?}");
            if !own_echo {
                assert!(line[0].contains(" ms"), "{line:?}");
            }
        }
    }

    #[test]
    fn requests_are_checked() {
        assert!(check_request("bob", "VERSION", "").is_ok());
        assert!(check_request("bob", "foo", "a b").is_ok());
        for (nick, tag, args) in [
            ("#chan", "VERSION", ""),
            ("", "VERSION", ""),
            ("bob smith", "VERSION", ""),
            ("bob", "", ""),
            ("bob", "VER\u{1}SION", ""),
            ("bob", "VER SION", ""),
            ("bob", "ACTION", "waves"),
            ("bob", "dcc", "SEND x 1 2 3"),
            ("bob", "AVATAR", ""),
            ("bob", "PING", "a\u{1}b"),
            ("bob", "PING", "a\r\nQUIT"),
            ("bob", "PING", "a\0b"),
        ] {
            assert!(
                check_request(nick, tag, args).is_err(),
                "{nick} {tag} {args:?}"
            );
        }
        assert!(check_request("bob", "PING", &"x".repeat(MAX_REQUEST_BYTES + 1)).is_err());
    }

    #[test]
    fn floods_are_bounded_per_user_and_in_total() {
        let now = Instant::now();
        let mut ctcp = replies();
        let ping = ":bob!u@h PRIVMSG alice :\u{1}PING 1\u{1}";
        // Two per user per window, then one line saying so, then silence.
        assert_eq!(observe(&mut ctcp, ping, now).unwrap().send.len(), 1);
        assert_eq!(observe(&mut ctcp, ping, now).unwrap().send.len(), 1);
        let refused = observe(&mut ctcp, ping, now).unwrap();
        assert!(refused.send.is_empty());
        assert_eq!(
            lines(&refused),
            ["Too many CTCP requests; ignoring them for now (latest CTCP PING from bob)."]
        );
        let muted = observe(&mut ctcp, ping, now).unwrap();
        assert!(muted.send.is_empty() && muted.events.is_empty());
        // Another user is still answered, and does not bring the line back
        // for bob's next refused request.
        let carol = ":carol!u@h PRIVMSG alice :\u{1}PING 1\u{1}";
        assert_eq!(observe(&mut ctcp, carol, now).unwrap().send.len(), 1);
        let muted = observe(&mut ctcp, ping, now).unwrap();
        assert!(muted.send.is_empty() && muted.events.is_empty());
        assert_eq!(
            observe(&mut ctcp, ping, now + WINDOW).unwrap().send.len(),
            1
        );

        // Five per window in total, from any number of users, channel
        // requests included.
        let later = now + WINDOW * 10;
        let mut ctcp = replies();
        let mut sent = 0;
        let mut shown = 0;
        for index in 0..40 {
            let target = if index % 2 == 0 { "alice" } else { "#a" };
            let text = format!(":user{index}!u@h PRIVMSG {target} :\u{1}VERSION\u{1}");
            let handled = observe(&mut ctcp, &text, later).unwrap();
            sent += handled.send.len();
            shown += handled.events.len();
        }
        assert_eq!(shown, MAX_PER_WINDOW + 1);
        assert_eq!(sent, 3);
        assert!(ctcp.requests.per_user.len() <= MAX_PER_WINDOW);
        // A reply flood does not use the request budget.
        let mut ctcp = replies();
        for index in 0..10 {
            let text = format!(":user{index}!u@h NOTICE alice :\u{1}VERSION x\u{1}");
            observe(&mut ctcp, &text, later).unwrap();
        }
        assert_eq!(observe(&mut ctcp, ping, later).unwrap().send.len(), 1);
    }
}
