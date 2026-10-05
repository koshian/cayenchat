//! Which incoming messages deserve a desktop notification, independent of
//! the chat protocol and of how the platform shows it.
//!
//! Protocol adapters decide what counts as a mention (IRC: our nickname as a
//! word; Matrix: the event's intentional mentions) and pass plain text with
//! formatting already removed.

use std::{
    collections::VecDeque,
    ops::Range,
    time::{Duration, Instant},
};

/// Longest notification body in characters; the log keeps the full text.
const MAX_BODY_CHARS: usize = 200;
/// A bouncer replaying history can deliver dozens of notifications at once.
const BURST_LIMIT: usize = 5;
const BURST_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotificationRules {
    pub enabled: bool,
    /// Channel messages that mention us.
    pub mentions: bool,
    /// Channel messages containing one of `keywords`.
    pub keyword_alerts: bool,
    pub keywords: Vec<String>,
    pub private_messages: bool,
    /// Also draw attention with a sound and the taskbar button.
    pub sound: bool,
}

/// An incoming message as the notification rules see it.
#[derive(Clone, Copy, Debug)]
pub struct IncomingMessage<'a> {
    /// Plain text without formatting codes.
    pub text: &'a str,
    /// Sent to a channel or room rather than directly to us.
    pub channel: bool,
    pub notice: bool,
    /// Our own message echoed back (for example by a bouncer).
    pub from_self: bool,
    pub mentioned: bool,
    /// History replayed by a bouncer or server, or a line the server or
    /// bouncer sent itself; it was live, if ever, some time ago.
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Mention,
    Keyword,
    PrivateMessage,
}

impl NotificationRules {
    /// NOTICEs are usually automated (bots, services), so private NOTICEs
    /// never notify; channel NOTICEs still notify on mentions and keywords.
    /// Replayed history never notifies.
    pub fn trigger(&self, message: IncomingMessage) -> Option<Trigger> {
        if !self.enabled || message.from_self || message.replayed {
            None
        } else if !message.channel {
            (self.private_messages && !message.notice).then_some(Trigger::PrivateMessage)
        } else if self.mentions && message.mentioned {
            Some(Trigger::Mention)
        } else if self.keyword_alerts && contains_keyword(message.text, &self.keywords) {
            Some(Trigger::Keyword)
        } else {
            None
        }
    }
}

/// Case-insensitive substring match: keywords are often Japanese, which has
/// no word boundaries.
pub fn contains_keyword(text: &str, keywords: &[String]) -> bool {
    !keyword_ranges(text, keywords).is_empty()
}

/// Byte ranges in `text` of every keyword occurrence, sorted and merged.
/// Characters compare by their lowercase forms, so ranges stay on the
/// original text's character boundaries.
pub fn keyword_ranges(text: &str, keywords: &[String]) -> Vec<Range<usize>> {
    let keywords: Vec<Vec<char>> = keywords
        .iter()
        .map(|word| word.trim())
        .filter(|word| !word.is_empty())
        .map(|word| word.chars().flat_map(char::to_lowercase).collect())
        .collect();
    if keywords.is_empty() {
        return Vec::new();
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (start, _) in text.char_indices() {
        let end = keywords
            .iter()
            .filter_map(|word| match_at(&text[start..], word).map(|len| start + len))
            .max();
        if let Some(end) = end {
            match ranges.last_mut() {
                Some(last) if last.end >= start => last.end = last.end.max(end),
                _ => ranges.push(start..end),
            }
        }
    }
    ranges
}

/// Length in bytes of the prefix of `text` equal to `word` ignoring case.
fn match_at(text: &str, word: &[char]) -> Option<usize> {
    let mut wanted = word.iter();
    let mut pending = wanted.next()?;
    for (index, ch) in text.char_indices() {
        for lower in ch.to_lowercase() {
            if lower != *pending {
                return None;
            }
            match wanted.next() {
                Some(next) => pending = next,
                None => return Some(index + ch.len_utf8()),
            }
        }
    }
    None
}

/// Parses the comma-separated keyword field.
pub fn parse_keywords(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Shortens long messages for the notification body.
pub fn body_text(text: &str) -> String {
    if text.chars().count() <= MAX_BODY_CHARS {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(MAX_BODY_CHARS - 1).collect();
    short.push('…');
    short
}

/// Drops notifications beyond a short burst so history playback cannot flood
/// the desktop. The chat log and unread marks are unaffected.
#[derive(Debug, Default)]
pub struct BurstLimiter {
    sent: VecDeque<Instant>,
}

impl BurstLimiter {
    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .sent
            .front()
            .is_some_and(|sent| now.duration_since(*sent) >= BURST_WINDOW)
        {
            self.sent.pop_front();
        }
        if self.sent.len() >= BURST_LIMIT {
            return false;
        }
        self.sent.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> NotificationRules {
        NotificationRules {
            enabled: true,
            mentions: true,
            keyword_alerts: true,
            keywords: vec!["ビルド".into(), "Deploy".into()],
            private_messages: true,
            sound: false,
        }
    }

    fn channel(text: &str, mentioned: bool) -> IncomingMessage<'_> {
        IncomingMessage {
            text,
            channel: true,
            notice: false,
            from_self: false,
            mentioned,
            replayed: false,
        }
    }

    #[test]
    fn keywords_match_substrings_case_insensitively() {
        let words = rules().keywords;
        assert!(contains_keyword("ビルドが壊れた", &words));
        assert!(contains_keyword("deployed now", &words));
        assert!(!contains_keyword("nothing here", &words));
        assert!(!contains_keyword("x", &[" ".into()]));
        assert_eq!(parse_keywords(" a, ,b ,"), vec!["a", "b"]);
        assert_eq!(
            keyword_ranges("Deploy ビルド deploy", &words),
            [0..6, 7..16, 17..23]
        );
        let overlapping = ["abc".into(), "bcd".into()];
        assert_eq!(keyword_ranges("xabcdx", &overlapping), vec![(1..5)]);
    }

    #[test]
    fn mentions_and_keywords_are_separate_choices() {
        let rules = rules();
        assert_eq!(
            rules.trigger(channel("me: ping", true)),
            Some(Trigger::Mention)
        );
        assert_eq!(
            rules.trigger(channel("Deploy done", false)),
            Some(Trigger::Keyword)
        );
        assert_eq!(rules.trigger(channel("hello all", false)), None);

        let keywords_only = NotificationRules {
            mentions: false,
            ..rules.clone()
        };
        assert_eq!(keywords_only.trigger(channel("me: ping", true)), None);
        assert_eq!(
            keywords_only.trigger(channel("me: deploy?", true)),
            Some(Trigger::Keyword)
        );
        let mentions_only = NotificationRules {
            keyword_alerts: false,
            ..rules
        };
        assert_eq!(mentions_only.trigger(channel("Deploy done", false)), None);
        assert_eq!(
            mentions_only.trigger(channel("me: ping", true)),
            Some(Trigger::Mention)
        );
    }

    #[test]
    fn private_messages_self_echoes_and_master_switch() {
        let rules = rules();
        let private = |notice| IncomingMessage {
            text: "hello",
            channel: false,
            notice,
            from_self: false,
            mentioned: false,
            replayed: false,
        };
        assert_eq!(rules.trigger(private(false)), Some(Trigger::PrivateMessage));
        assert_eq!(rules.trigger(private(true)), None);
        let echo = IncomingMessage {
            from_self: true,
            ..channel("Deploy me", true)
        };
        assert_eq!(rules.trigger(echo), None);
        let replayed = |message| IncomingMessage {
            replayed: true,
            ..message
        };
        assert_eq!(rules.trigger(replayed(channel("me: ping", true))), None);
        assert_eq!(rules.trigger(replayed(channel("Deploy done", false))), None);
        assert_eq!(rules.trigger(replayed(private(false))), None);
        let off = NotificationRules {
            enabled: false,
            ..rules.clone()
        };
        assert_eq!(off.trigger(channel("me: ping", true)), None);
        let no_private = NotificationRules {
            private_messages: false,
            ..rules
        };
        assert_eq!(no_private.trigger(private(false)), None);
    }

    #[test]
    fn long_bodies_are_shortened() {
        assert_eq!(body_text("short"), "short");
        let body = body_text(&"あ".repeat(500));
        assert_eq!(body.chars().count(), MAX_BODY_CHARS);
        assert!(body.ends_with('…'));
    }

    #[test]
    fn burst_limiter_recovers_after_the_window() {
        let mut limiter = BurstLimiter::default();
        let start = Instant::now();
        for _ in 0..BURST_LIMIT {
            assert!(limiter.allow(start));
        }
        assert!(!limiter.allow(start + Duration::from_secs(1)));
        assert!(limiter.allow(start + BURST_WINDOW));
    }
}
