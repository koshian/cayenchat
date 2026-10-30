// SPDX-License-Identifier: MPL-2.0
//! Which incoming messages deserve a desktop notification, independent of
//! the chat protocol and of how the platform shows it.
//!
//! Protocol adapters decide what counts as a mention (IRC: our nickname as a
//! word; Matrix: the event's intentional mentions) and look for keywords
//! with [`keyword_ranges`] in the text without formatting, once per message:
//! the same ranges are drawn in the logs.

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
    /// Matches overlapping an occurrence of one of these are dropped.
    pub exclude_keywords: Vec<String>,
    pub private_messages: bool,
    /// Also draw attention with a sound and the taskbar button.
    pub sound: bool,
}

/// An incoming message as the notification rules see it.
#[derive(Clone, Copy, Debug)]
pub struct IncomingMessage {
    /// Sent to a channel or room rather than directly to us.
    pub channel: bool,
    pub notice: bool,
    /// Our own message echoed back (for example by a bouncer).
    pub from_self: bool,
    pub mentioned: bool,
    /// The text contains one of the rules' keywords.
    pub keyword: bool,
    /// History replayed by a bouncer or server, or a line the server or
    /// bouncer sent itself; it was live, if ever, some time ago. It still
    /// counts for [`NotificationRules::attention`] but never notifies.
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Mention,
    Keyword,
    PrivateMessage,
}

impl NotificationRules {
    /// NOTICEs never raise attention: neither channel nor private NOTICEs
    /// notify, regardless of their nickname or keyword content.
    /// Replayed history never notifies either.
    pub fn trigger(&self, message: IncomingMessage) -> Option<Trigger> {
        if self.enabled && !message.replayed {
            self.attention(message)
        } else {
            None
        }
    }

    /// [`trigger`](Self::trigger) without the master switch and without
    /// regard to replay: what a message is worth the user's attention for,
    /// wherever it came from. The jump to notified lines uses it, so
    /// highlighted lines and private messages, replayed ones included, can
    /// be visited with notifications off.
    pub fn attention(&self, message: IncomingMessage) -> Option<Trigger> {
        if message.from_self || message.notice {
            None
        } else if !message.channel {
            self.private_messages.then_some(Trigger::PrivateMessage)
        } else if self.mentions && message.mentioned {
            Some(Trigger::Mention)
        } else if self.keyword_alerts && message.keyword {
            Some(Trigger::Keyword)
        } else {
            None
        }
    }
}

/// Byte ranges in `text` of every keyword occurrence, sorted and merged.
/// A case-insensitive substring match: keywords are often Japanese, which
/// has no word boundaries. Characters compare by their lowercase forms, so
/// ranges stay on the original text's character boundaries.
pub fn keyword_ranges(text: &str, keywords: &[String]) -> Vec<Range<usize>> {
    keyword_ranges_outside(text, keywords, &[])
}

/// Like [`keyword_ranges`], but an occurrence overlapping one of `excluded`
/// (disjoint, ascending, see [`keyword_ranges`]) is dropped before merging, so
/// a neighbour of a dropped occurrence stays. Occurrences are judged and merged
/// as they are found; nothing proportional to their number is kept.
pub fn keyword_ranges_outside(
    text: &str,
    keywords: &[String],
    excluded: &[Range<usize>],
) -> Vec<Range<usize>> {
    let keywords: Vec<Vec<char>> = keywords
        .iter()
        .map(|word| word.trim())
        .filter(|word| !word.is_empty())
        .map(|word| word.chars().flat_map(char::to_lowercase).collect())
        .collect();
    let mut merged: Vec<Range<usize>> = Vec::new();
    if keywords.is_empty() {
        return merged;
    }
    for (start, _) in text.char_indices() {
        for word in &keywords {
            let Some(len) = match_at(&text[start..], word) else {
                continue;
            };
            let found = start..start + len;
            if overlaps_any(excluded, &found) {
                continue;
            }
            // Starts ascend, so only the last range can touch this one.
            match merged.last_mut() {
                Some(last) if last.end >= found.start => last.end = last.end.max(found.end),
                _ => merged.push(found),
            }
        }
    }
    merged
}

/// The `ranges` that overlap none of `excluded` (disjoint, ascending).
pub fn ranges_outside(
    mut ranges: Vec<Range<usize>>,
    excluded: &[Range<usize>],
) -> Vec<Range<usize>> {
    ranges.retain(|range| !overlaps_any(excluded, range));
    ranges
}

fn overlaps_any(excluded: &[Range<usize>], range: &Range<usize>) -> bool {
    let next = excluded.partition_point(|cut| cut.end <= range.start);
    excluded.get(next).is_some_and(|cut| cut.start < range.end)
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
            exclude_keywords: Vec::new(),
            private_messages: true,
            sound: false,
        }
    }

    /// A channel message whose keyword matches the adapter found with
    /// [`keyword_ranges`] and `rules()`' keywords.
    fn channel(text: &str, mentioned: bool) -> IncomingMessage {
        IncomingMessage {
            channel: true,
            notice: false,
            from_self: false,
            mentioned,
            keyword: !keyword_ranges(text, &rules().keywords).is_empty(),
            replayed: false,
        }
    }

    #[test]
    fn keywords_match_substrings_case_insensitively() {
        let words = rules().keywords;
        let contains_keyword = |text, words: &[String]| !keyword_ranges(text, words).is_empty();
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
    fn excluded_text_drops_only_overlapping_matches() {
        let words = vec!["Dalk".to_owned()];
        let excluded = vec!["<dalk>".to_owned()];
        let kept = |text: &str, excluded: &[String]| {
            let cuts = keyword_ranges(text, excluded);
            keyword_ranges_outside(text, &words, &cuts).len()
        };
        assert_eq!(kept("Dalk、ちょっと聞いて", &excluded), 1);
        assert_eq!(kept("<dalk> こんにちは", &excluded), 0);
        assert_eq!(kept("<dalk> Dalk、返事して", &excluded), 1);
        assert_eq!(kept("<DALK> Dalk!", &excluded), 1);
        assert_eq!(kept("<dalk><dalk>", &excluded), 0);
        assert_eq!(kept("<dalk> dalk <dalk> dalk", &excluded), 2);
        assert_eq!(kept("<dalk>", &[]), 1);
        assert_eq!(kept("<dalk>", &[" ".into()]), 1);
        // A match only partly inside the excluded text is cancelled too.
        let cuts = keyword_ranges("xDalkx", &["lkx".into()]);
        assert!(keyword_ranges_outside("xDalkx", &words, &cuts).is_empty());
    }

    #[test]
    fn adjacent_and_overlapping_matches_are_excluded_one_by_one() {
        let remaining = |text: &str, words: &[&str], excluded: &[&str]| {
            let words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
            let excluded: Vec<String> = excluded.iter().map(|w| (*w).to_owned()).collect();
            keyword_ranges_outside(text, &words, &keyword_ranges(text, &excluded))
        };
        // The second `Dalk` lies outside the excluded `xDalk`.
        assert_eq!(remaining("xDalkDalk", &["Dalk"], &["xDalk"]), vec![5..9]);
        // `cde` survives although `abc` is cancelled.
        assert_eq!(remaining("abcde", &["abc", "cde"], &["ab"]), vec![2..5]);
        // A short and a long match from the same start are judged separately.
        assert_eq!(remaining("Dalkx", &["Dalk", "Dalkx"], &["x"]), vec![0..4]);
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
            channel: false,
            notice,
            from_self: false,
            mentioned: false,
            keyword: false,
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
        // Replayed lines are still worth a visit, only not a notification.
        assert_eq!(
            rules.attention(replayed(channel("me: ping", true))),
            Some(Trigger::Mention)
        );
        assert_eq!(
            rules.attention(replayed(channel("Deploy done", false))),
            Some(Trigger::Keyword)
        );
        assert_eq!(
            rules.attention(replayed(private(false))),
            Some(Trigger::PrivateMessage)
        );
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
    fn notices_never_raise_attention_even_when_text_matches() {
        let notice = |message| IncomingMessage {
            notice: true,
            ..message
        };
        let rules = rules();
        for message in [
            notice(channel("me: ping", true)),
            notice(channel("Deploy done", false)),
            notice(IncomingMessage {
                channel: false,
                notice: false,
                from_self: false,
                mentioned: true,
                keyword: true,
                replayed: false,
            }),
        ] {
            assert_eq!(rules.attention(message), None);
            assert_eq!(rules.trigger(message), None);
        }
        assert_eq!(
            rules.trigger(channel("me: ping", true)),
            Some(Trigger::Mention)
        );
        assert_eq!(
            rules.trigger(channel("Deploy done", false)),
            Some(Trigger::Keyword)
        );
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
