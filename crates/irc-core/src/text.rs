//! IRC message text: formatting codes, CTCP ACTION and nickname mentions.

/// Removes mIRC formatting: bold, italics, underline, strikethrough,
/// monospace, reverse, reset and color codes with their `fg[,bg]` digits.
/// CTCP delimiters are dropped as well.
pub fn strip_formatting(text: &str) -> String {
    fn digits(chars: &mut std::iter::Peekable<std::str::Chars>) -> usize {
        let mut count = 0;
        while count < 2 && chars.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            count += 1;
        }
        count
    }
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1}' | '\u{2}' | '\u{f}' | '\u{11}' | '\u{16}' | '\u{1d}' | '\u{1e}' | '\u{1f}' => {}
            '\u{3}' => {
                if digits(&mut chars) > 0 && chars.peek() == Some(&',') {
                    let mut ahead = chars.clone();
                    ahead.next();
                    if ahead.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                        digits(&mut chars);
                    }
                }
            }
            ch => plain.push(ch),
        }
    }
    plain
}

/// The action text of a CTCP ACTION (`/me`) message.
pub fn action_text(text: &str) -> Option<&str> {
    text.strip_prefix("\u{1}ACTION ")
        .map(|action| action.strip_suffix('\u{1}').unwrap_or(action))
}

/// Whether `nickname` appears in `text` as a whole word. Neighbouring nickname
/// characters (letters, digits and `-_[]\`^{}|`) make it part of another word,
/// so `bob` does not match `bobby`, while `@bob` and `bob:` match.
pub fn mentions_nickname(text: &str, nickname: &str) -> bool {
    !mention_ranges(text, nickname).is_empty()
}

/// Byte ranges of the whole-word occurrences of `nickname` in `text`.
pub fn mention_ranges(text: &str, nickname: &str) -> Vec<std::ops::Range<usize>> {
    if nickname.is_empty() {
        return Vec::new();
    }
    let lower = irc_lowercase(text);
    let nickname = irc_lowercase(nickname);
    lower
        .match_indices(&nickname)
        .map(|(start, found)| start..start + found.len())
        .filter(|range| {
            let before = lower[..range.start].chars().next_back();
            let after = lower[range.end..].chars().next();
            !before.is_some_and(is_nick_char) && !after.is_some_and(is_nick_char)
        })
        .collect()
}

/// Nickname equality under RFC 1459 case mapping.
pub fn same_nickname(left: &str, right: &str) -> bool {
    irc_lowercase(left) == irc_lowercase(right)
}

fn is_nick_char(ch: char) -> bool {
    ch.is_alphanumeric() || "-_[]\\`^{}|".contains(ch)
}

/// RFC 1459 case mapping: `[]\~` are the uppercase forms of `{}|^`. Every
/// mapping keeps the byte length, so indexes stay valid in the original.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nickname_mentions_need_word_boundaries() {
        assert!(mentions_nickname("bob: hi", "Bob"));
        assert!(mentions_nickname("hi, @bob!", "bob"));
        assert!(mentions_nickname("おはよう bob さん", "bob"));
        assert!(mentions_nickname("{away}", "[away]"));
        assert!(!mentions_nickname("bobby: hi", "bob"));
        assert!(!mentions_nickname("bob_: hi", "bob"));
        assert!(!mentions_nickname("anything", ""));
        assert!(same_nickname("Nick[a]", "nick{A}"));
        assert_eq!(mention_ranges("Bob, bobby and @bob", "bob"), [0..3, 16..19]);
    }

    #[test]
    fn formatting_and_actions_are_removed() {
        assert_eq!(
            strip_formatting("\u{3}04,01red\u{3} \u{2}bold\u{f}"),
            "red bold"
        );
        assert_eq!(strip_formatting("\u{3}12,x"), ",x");
        assert_eq!(action_text("\u{1}ACTION waves\u{1}"), Some("waves"));
        assert_eq!(action_text("waves"), None);
    }
}
