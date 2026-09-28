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
//! version (no OS or host), TIME the local time, and CLIENTINFO the tags
//! answered here plus ACTION (and AVATAR while peer avatars are on).
//! USERINFO, FINGER, SOURCE, DCC and unknown tags get no answer, and there
//! is no ERRMSG. Channel requests, replayed history, our own echoes and
//! requests from servers are never answered.
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
    text::{nickname_key, same_nickname, strip_formatting},
    valid_channel,
};

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

#[derive(Debug)]
pub(crate) struct CtcpReplies {
    /// CTCP AVATAR is answered (by `peer_avatar`) on this connection.
    avatar: bool,
    requests: Budget,
    replies: Budget,
    local_time: fn() -> String,
}

impl CtcpReplies {
    pub(crate) fn new(avatar: bool) -> Self {
        Self {
            avatar,
            requests: Budget::default(),
            replies: Budget::default(),
            local_time,
        }
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
        if replayed || (user && same_nickname(sender, current_nick)) {
            return Some(handled);
        }
        // STATUSMSG (`@#chan`) reaches part of a channel; still a channel.
        let to_channel = valid_channel(target.trim_start_matches(['~', '@', '%', '+']));
        if !to_channel && !same_nickname(target, current_nick) {
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
                handled.events.push(Event::ServerLine(if request {
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
            handled.events.push(Event::ServerLine(format!(
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
                handled.events.push(Event::ServerLine(line));
            }
            None => handled
                .events
                .push(Event::ServerLine(format!("{line} (not answered)"))),
        }
        Some(handled)
    }

    /// The answer's CTCP body (without 0x01) for a private request.
    fn answer(&self, tag: &str, params: &str) -> Option<String> {
        match tag.to_ascii_uppercase().as_str() {
            "PING" if params.is_empty() => Some("PING".into()),
            "PING" if params.len() <= MAX_PING_BYTES && !params.contains('\u{1}') => {
                Some(format!("PING {params}"))
            }
            "VERSION" => Some(format!("VERSION {VERSION}")),
            "TIME" => Some(format!("TIME {}", (self.local_time)())),
            "CLIENTINFO" => Some(format!("CLIENTINFO {}", self.client_info())),
            _ => None,
        }
    }

    fn client_info(&self) -> &'static str {
        if self.avatar {
            "ACTION AVATAR CLIENTINFO PING TIME VERSION"
        } else {
            "ACTION CLIENTINFO PING TIME VERSION"
        }
    }
}

/// The tag and parameters of a CTCP message, or `None` for other text. The
/// closing 0x01 is optional, as in other clients.
fn split(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_prefix('\u{1}')?;
    let body = body.strip_suffix('\u{1}').unwrap_or(body);
    Some(body.split_once(' ').unwrap_or((body, "")))
}

fn shown_tag(tag: &str) -> String {
    if !tag.is_empty()
        && tag.len() <= MAX_TAG_CHARS
        && tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'))
    {
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
            ..CtcpReplies::new(false)
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
                Event::ServerLine(line) => line.clone(),
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
                "NOTICE bob :\u{1}CLIENTINFO ACTION CLIENTINFO PING TIME VERSION\u{1}",
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
        let mut ctcp = CtcpReplies::new(true);
        let handled = observe(
            &mut ctcp,
            ":bob!u@h PRIVMSG alice :\u{1}CLIENTINFO\u{1}",
            now,
        )
        .unwrap();
        assert_eq!(
            wire(&handled),
            ["NOTICE bob :\u{1}CLIENTINFO ACTION AVATAR CLIENTINFO PING TIME VERSION\u{1}"]
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
                ":bob!u@h PRIVMSG alice :\u{1}USERINFO\u{1}",
                "CTCP USERINFO request from bob (not answered)",
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
