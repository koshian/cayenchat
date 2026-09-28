//! IRCv3 message tags, read from the `irc` library's parsed messages.
//!
//! irc-proto 1.1.0 splits tags and unescapes values (`\:`, `\s`, `\\`, `\r`,
//! `\n`; an invalid escape keeps the character and a trailing lone backslash
//! is dropped, as the specification asks). It keeps every occurrence of a key
//! and distinguishes `key` from `key=`, so lookups here apply the remaining
//! rules: the last occurrence wins and an empty value equals a missing one.
//! Only the values this client uses are read; messages keep no tag maps.

use std::time::{Duration, SystemTime};

use irc::proto::{Message as IrcMessage, message::Tag};

/// Server-to-client tag data limit, including the leading `@` and the space
/// after the tags. The body keeps its own 512-byte limit.
pub const MAX_TAG_BYTES: usize = 8191;

/// Tags longer than this are shortened in the diagnostic transcript, which
/// keeps 1,000 lines per server.
const TRANSCRIPT_TAG_BYTES: usize = 512;

/// The value of `key`, or `None` when absent, empty, over the size limit or
/// not valid UTF-8. The line codec replaces undecodable bytes with U+FFFD
/// before tags are parsed; such values are dropped rather than used.
pub fn tag_value<'a>(message: &'a IrcMessage, key: &str) -> Option<&'a str> {
    let tags = message.tags.as_deref()?;
    if tag_bytes(tags) > MAX_TAG_BYTES {
        return None;
    }
    let Tag(_, value) = tags.iter().rev().find(|Tag(name, _)| name == key)?;
    value
        .as_deref()
        .filter(|value| !value.is_empty() && !value.contains('\u{FFFD}'))
}

/// Size of the tag section as it would appear on the wire, re-escaped from
/// the parsed values. The original bytes are not available after parsing, so
/// an invalid escape (`\b`) or an explicit empty value (`key=`) counts one
/// byte less than it was sent with.
pub fn tag_bytes(tags: &[Tag]) -> usize {
    if tags.is_empty() {
        return 0;
    }
    let separators = tags.len() - 1;
    let body: usize = tags
        .iter()
        .map(|Tag(name, value)| {
            name.len()
                + value
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .map_or(0, |value| 1 + escaped_len(value))
        })
        .sum();
    // Leading '@' and the trailing space.
    body + separators + 2
}

fn escaped_len(value: &str) -> usize {
    value
        .chars()
        .map(|ch| match ch {
            ';' | ' ' | '\\' | '\r' | '\n' => 2,
            other => other.len_utf8(),
        })
        .sum()
}

/// The message without its tags, as shown in the server log.
pub fn untagged_line(message: &IrcMessage) -> String {
    match &message.prefix {
        Some(prefix) => format!(":{prefix} {}", String::from(&message.command)),
        None => String::from(&message.command),
    }
}

/// A transcript line with the tag section shortened to a bounded size.
pub fn transcript_line(message: &IrcMessage) -> String {
    let line = message.to_string();
    let line = line.trim_end_matches(['\r', '\n']);
    let tag_end = match message.tags.as_deref() {
        Some(tags) if !tags.is_empty() => line.find(' ').unwrap_or(line.len()),
        _ => return line.to_owned(),
    };
    if tag_end <= TRANSCRIPT_TAG_BYTES {
        return line.to_owned();
    }
    let mut cut = TRANSCRIPT_TAG_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}…[{} tag bytes omitted]{}",
        &line[..cut],
        tag_end - cut,
        &line[tag_end..]
    )
}

/// The `msgid` tag: the server's identifier for this message, opaque and
/// compared exactly. Read whenever present, since only servers that
/// implement message IDs send it; the application decides whether the value
/// is usable as an identity (`model::NativeMessageId`).
pub fn msgid(message: &IrcMessage) -> Option<&str> {
    tag_value(message, "msgid")
}

/// The `time` tag as an instant, when server-time is enabled on this
/// connection. Absent or invalid values yield `None` so callers fall back to
/// the receipt time.
pub fn server_time(message: &IrcMessage, enabled: bool) -> Option<SystemTime> {
    if !enabled {
        return None;
    }
    parse_server_time(tag_value(message, "time")?)
}

/// Parses `YYYY-MM-DDThh:mm:ss.sssZ` (UTC). Any number of fraction digits,
/// or none, is accepted; other offsets, dates before 1970 and out-of-range
/// fields are rejected. A leap second (`:60`) counts as `:59`.
pub fn parse_server_time(value: &str) -> Option<SystemTime> {
    let bytes = value.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let slice = bytes.get(range)?;
        if slice.is_empty() || !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        slice
            .iter()
            .try_fold(0u32, |acc, byte| Some(acc * 10 + u32::from(byte - b'0')))
    };
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || *bytes.last()? != b'Z'
    {
        return None;
    }
    let year = digits(0..4)?;
    let month = digits(5..7)?;
    let day = digits(8..10)?;
    let hour = digits(11..13)?;
    let minute = digits(14..16)?;
    let second = digits(17..19)?.min(59);
    let millis = match &bytes[19..bytes.len() - 1] {
        [] => 0,
        [b'.', fraction @ ..]
            if !fraction.is_empty() && fraction.iter().all(u8::is_ascii_digit) =>
        {
            fraction
                .iter()
                .chain(std::iter::repeat(&b'0'))
                .take(3)
                .fold(0u32, |acc, byte| acc * 10 + u32::from(byte - b'0'))
        }
        _ => return None,
    };
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + u64::from(hour * 3600 + minute * 60 + second);
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(seconds * 1000 + u64::from(millis)))
}

fn is_leap(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date from 1970 on
/// (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: u32, month: u32, day: u32) -> u64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    u64::from(era) * 146_097 + u64::from(day_of_era) - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> IrcMessage {
        line.parse().unwrap()
    }

    fn unix(value: &str) -> Option<u128> {
        parse_server_time(value).map(|time| {
            time.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        })
    }

    #[test]
    fn values_are_unescaped_last_wins_and_empty_means_missing() {
        let message = parse(
            "@a=first;+client=x;a=last\\sone\\:\\\\;empty=;bare;odd=\\b\\ :n!u@h PRIVMSG #c :hi",
        );
        assert_eq!(tag_value(&message, "a"), Some("last one;\\"));
        assert_eq!(tag_value(&message, "+client"), Some("x"));
        assert_eq!(tag_value(&message, "empty"), None);
        assert_eq!(tag_value(&message, "bare"), None);
        // Invalid escape keeps the character; a trailing lone backslash is dropped.
        assert_eq!(tag_value(&message, "odd"), Some("b"));
        assert_eq!(tag_value(&message, "unknown"), None);
        assert_eq!(untagged_line(&message), ":n!u@h PRIVMSG #c hi");
    }

    #[test]
    fn replaced_bytes_and_oversized_tag_sections_are_ignored() {
        let message = parse("@time=2026-01-01T00:00:00.000Z;x=bad\u{FFFD} :s NOTICE * :hi");
        assert_eq!(tag_value(&message, "x"), None);
        assert!(tag_value(&message, "time").is_some());

        // Exactly at the limit: '@' + "k=" + value + ' ' = MAX_TAG_BYTES.
        let fits = "v".repeat(MAX_TAG_BYTES - 4);
        let message = parse(&format!("@k={fits} :s PRIVMSG #c :body"));
        assert_eq!(tag_bytes(message.tags.as_deref().unwrap()), MAX_TAG_BYTES);
        assert_eq!(tag_value(&message, "k").map(str::len), Some(fits.len()));
        let message = parse(&format!("@k={fits}v :s PRIVMSG #c :body"));
        assert_eq!(tag_value(&message, "k"), None);
        // The body is still parsed independently of the tag limit.
        assert!(
            matches!(message.command, irc::proto::Command::PRIVMSG(_, ref text) if text == "body")
        );
    }

    #[test]
    fn transcript_shortens_only_long_tag_sections() {
        let short = parse("@time=2026-01-01T00:00:00.000Z :s PRIVMSG #c :hi");
        assert_eq!(
            transcript_line(&short),
            "@time=2026-01-01T00:00:00.000Z :s PRIVMSG #c hi"
        );
        let long = parse(&format!("@k={} :s PRIVMSG #c :hi", "é".repeat(600)));
        let line = transcript_line(&long);
        assert!(line.len() < 600);
        assert!(line.contains("tag bytes omitted]"));
        assert!(line.ends_with(" :s PRIVMSG #c hi"));
    }

    #[test]
    fn msgid_uses_the_normalized_reader() {
        let tagged = parse("@time=2026-09-27T23:58:31.123Z;msgid=abc :n!u@h PRIVMSG #c :hi");
        assert_eq!(msgid(&tagged), Some("abc"));
        assert_eq!(msgid(&parse(":n!u@h PRIVMSG #c :hi")), None);
        assert_eq!(msgid(&parse("@msgid= :n!u@h PRIVMSG #c :hi")), None);
        assert_eq!(
            msgid(&parse("@msgid=a;msgid=b :n!u@h PRIVMSG #c :hi")),
            Some("b")
        );
        assert_eq!(
            msgid(&parse("@msgid=x\u{FFFD} :n!u@h PRIVMSG #c :hi")),
            None
        );
        // Escapes are undone before the value is used.
        assert_eq!(
            msgid(&parse("@msgid=a\\sb :n!u@h PRIVMSG #c :hi")),
            Some("a b")
        );
    }

    #[test]
    fn server_time_parses_utc_and_rejects_invalid_values() {
        assert_eq!(unix("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(unix("2011-10-19T16:40:51.620Z"), Some(1_319_042_451_620));
        assert_eq!(unix("2024-02-29T23:59:59Z"), Some(1_709_251_199_000));
        assert_eq!(unix("2016-12-31T23:59:60.5Z"), Some(1_483_228_799_500));
        assert_eq!(
            unix("2026-09-27T12:34:56.1234Z"),
            unix("2026-09-27T12:34:56.123Z")
        );
        for invalid in [
            "",
            "2026-09-27",
            "2026-09-27T12:34:56.000",
            "2026-09-27T12:34:56.000+09:00",
            "2026-13-01T00:00:00.000Z",
            "2023-02-29T00:00:00.000Z",
            "2026-09-27T24:00:00.000Z",
            "2026-09-27T12:60:00.000Z",
            "1969-12-31T23:59:59.000Z",
            "2026-09-27T12:34:56.Z",
            "2026-09-27T12:34:5x.000Z",
            "+026-09-27T12:34:56.000Z",
        ] {
            assert_eq!(unix(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn server_time_needs_the_capability_and_a_valid_tag() {
        let message = parse("@time=2011-10-19T16:40:51.620Z :n!u@h PRIVMSG #c :hi");
        assert!(server_time(&message, false).is_none());
        assert!(server_time(&message, true).is_some());
        let invalid = parse("@time=yesterday :n!u@h PRIVMSG #c :hi");
        assert!(server_time(&invalid, true).is_none());
        let untagged = parse(":n!u@h PRIVMSG #c :hi");
        assert!(server_time(&untagged, true).is_none());
    }
}
