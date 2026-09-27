//! Which incoming messages deserve a desktop notification, independent of
//! how the platform shows it.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// Longest notification body in characters; the log keeps the full text.
const MAX_BODY_CHARS: usize = 200;
/// A bouncer replaying history can deliver dozens of highlights at once.
const BURST_LIMIT: usize = 5;
const BURST_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotificationRules {
    pub enabled: bool,
    /// Channel messages that mention our nickname or a highlight word.
    pub highlights: bool,
    pub private_messages: bool,
    pub highlight_words: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Highlight,
    PrivateMessage,
}

impl NotificationRules {
    /// Decides whether a channel message (`channel: true`) or a private
    /// PRIVMSG should notify. NOTICEs are usually automated (bots, services),
    /// so only channel NOTICEs that highlight us count.
    pub fn trigger(
        &self,
        own_nickname: Option<&str>,
        sender: &str,
        text: &str,
        channel: bool,
        notice: bool,
    ) -> Option<Trigger> {
        if !self.enabled || own_nickname.is_some_and(|own| irc_eq(own, sender)) {
            return None;
        }
        if !channel {
            return (self.private_messages && !notice).then_some(Trigger::PrivateMessage);
        }
        let text = plain_text(text);
        (self.highlights
            && (own_nickname.is_some_and(|own| mentions_nickname(&text, own))
                || contains_highlight_word(&text, &self.highlight_words)))
        .then_some(Trigger::Highlight)
    }
}

/// Whether `nickname` appears in `text` as a whole word. Neighbouring nickname
/// characters (letters, digits and `-_[]\`^{}|`) make it part of another word,
/// so `bob` does not match `bobby`.
pub fn mentions_nickname(text: &str, nickname: &str) -> bool {
    if nickname.is_empty() {
        return false;
    }
    let text = irc_lowercase(text);
    let nickname = irc_lowercase(nickname);
    text.match_indices(&nickname).any(|(start, found)| {
        let before = text[..start].chars().next_back();
        let after = text[start + found.len()..].chars().next();
        !before.is_some_and(is_nick_char) && !after.is_some_and(is_nick_char)
    })
}

/// Case-insensitive substring match: highlight words are often Japanese,
/// which has no word boundaries.
pub fn contains_highlight_word(text: &str, words: &[String]) -> bool {
    let text = text.to_lowercase();
    words
        .iter()
        .map(|word| word.trim())
        .filter(|word| !word.is_empty())
        .any(|word| text.contains(&word.to_lowercase()))
}

/// Parses the comma-separated highlight word field.
pub fn parse_highlight_words(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Notification body text: IRC formatting removed, CTCP ACTION shown as
/// `* sender text`, and long messages shortened.
pub fn body_text(sender: &str, text: &str) -> String {
    let text = match text
        .strip_prefix("\u{1}ACTION ")
        .map(|action| action.strip_suffix('\u{1}').unwrap_or(action))
    {
        Some(action) => format!("* {sender} {}", plain_text(action)),
        None => plain_text(text),
    };
    if text.chars().count() <= MAX_BODY_CHARS {
        return text;
    }
    let mut short: String = text.chars().take(MAX_BODY_CHARS - 1).collect();
    short.push('…');
    short
}

/// Removes mIRC formatting: bold, italics, underline, strikethrough,
/// monospace, reverse, reset and color codes with their `fg[,bg]` digits.
fn plain_text(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{2}' | '\u{f}' | '\u{11}' | '\u{16}' | '\u{1d}' | '\u{1e}' | '\u{1f}' => {}
            '\u{3}' => {
                fn digits(chars: &mut std::iter::Peekable<std::str::Chars>) -> usize {
                    let mut count = 0;
                    while count < 2 && chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                        count += 1;
                    }
                    count
                }
                if digits(&mut chars) > 0 && chars.peek() == Some(&',') {
                    let mut ahead = chars.clone();
                    ahead.next();
                    if ahead.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                        digits(&mut chars);
                    }
                }
            }
            '\u{1}' => {}
            ch => plain.push(ch),
        }
    }
    plain
}

fn is_nick_char(ch: char) -> bool {
    ch.is_alphanumeric() || "-_[]\\`^{}|".contains(ch)
}

/// RFC 1459 case mapping: `[]\~` are the uppercase forms of `{}|^`.
fn irc_lowercase(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '[' => '{',
            ']' => '}',
            '\\' => '|',
            '~' => '^',
            ch => ch.to_ascii_lowercase(),
        })
        .collect()
}

fn irc_eq(left: &str, right: &str) -> bool {
    irc_lowercase(left) == irc_lowercase(right)
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
            highlights: true,
            private_messages: true,
            highlight_words: vec!["ビルド".into(), "Deploy".into()],
        }
    }

    #[test]
    fn nickname_mentions_need_word_boundaries() {
        assert!(mentions_nickname("bob: hi", "Bob"));
        assert!(mentions_nickname("hi, bob!", "bob"));
        assert!(mentions_nickname("おはよう bob さん", "bob"));
        assert!(mentions_nickname("{away}", "[away]"));
        assert!(!mentions_nickname("bobby: hi", "bob"));
        assert!(!mentions_nickname("bob_: hi", "bob"));
        assert!(!mentions_nickname("anything", ""));
    }

    #[test]
    fn highlight_words_match_substrings_case_insensitively() {
        let words = rules().highlight_words;
        assert!(contains_highlight_word("ビルドが壊れた", &words));
        assert!(contains_highlight_word("deployed now", &words));
        assert!(!contains_highlight_word("nothing here", &words));
        assert!(!contains_highlight_word("x", &[" ".into()]));
        assert_eq!(parse_highlight_words(" a, ,b ,"), vec!["a", "b"]);
    }

    #[test]
    fn triggers_follow_preferences() {
        let rules = rules();
        let own = Some("me");
        assert_eq!(
            rules.trigger(own, "alice", "me: ping", true, false),
            Some(Trigger::Highlight)
        );
        assert_eq!(
            rules.trigger(own, "alice", "\u{2}me\u{2}: ping", true, true),
            Some(Trigger::Highlight)
        );
        assert_eq!(rules.trigger(own, "alice", "hello all", true, false), None);
        assert_eq!(
            rules.trigger(own, "Me", "me: note to self", true, false),
            None
        );
        assert_eq!(
            rules.trigger(own, "alice", "hello", false, false),
            Some(Trigger::PrivateMessage)
        );
        assert_eq!(rules.trigger(own, "NickServ", "hello", false, true), None);

        let off = NotificationRules {
            enabled: false,
            ..rules.clone()
        };
        assert_eq!(off.trigger(own, "alice", "me: ping", true, false), None);
        let no_private = NotificationRules {
            private_messages: false,
            ..rules.clone()
        };
        assert_eq!(no_private.trigger(own, "alice", "hi", false, false), None);
        let no_highlight = NotificationRules {
            highlights: false,
            ..rules
        };
        assert_eq!(
            no_highlight.trigger(own, "alice", "me: hi", true, false),
            None
        );
    }

    #[test]
    fn body_text_strips_formatting_and_shortens() {
        assert_eq!(
            body_text("a", "\u{3}04,01red\u{3} \u{2}bold\u{f}"),
            "red bold"
        );
        assert_eq!(body_text("a", "\u{3}12,x"), ",x");
        assert_eq!(
            body_text("alice", "\u{1}ACTION waves\u{1}"),
            "* alice waves"
        );
        let long = "あ".repeat(500);
        let body = body_text("a", &long);
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
