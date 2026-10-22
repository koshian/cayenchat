// SPDX-License-Identifier: MPL-2.0
//! Character encodings of single channels.
//!
//! A connection reads and writes lines in the server's encoding. A channel
//! may override that for the text of its messages, in either direction; the
//! line codec applies it ([`irc::proto::charsets`]), while everything else on
//! the line, the channel name above all, keeps the server's encoding.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use cayenchat_model::names::CaseMapping;
use encoding_rs::Encoding;
use irc::proto::charsets::{BodyCharsets, Pin};

use crate::valid_channel;

/// The encoding overrides of one channel, as WHATWG labels. A side that is
/// `None` follows the connection's encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelEncoding {
    pub channel: String,
    /// For the text of messages received in the channel.
    pub decode: Option<String>,
    /// For the text of messages sent to the channel.
    pub encode: Option<String>,
}

/// An encoding `encoding_rs` can both read and write, by its label.
pub(crate) fn resolve(label: &str) -> Result<&'static Encoding, String> {
    Encoding::for_label(label.as_bytes())
        .filter(|encoding| encoding.output_encoding() == *encoding)
        .ok_or_else(|| format!("Unsupported character encoding: {label}"))
}

/// `text` with each character ISO-2022-JP cannot hold written as a decimal
/// numeric character reference (`&#128512;`), one scalar value at a time and
/// in order. Characters it can hold, and references already in the text, are
/// left as they are.
fn with_char_refs(text: &str) -> std::borrow::Cow<'_, str> {
    let encoding = encoding_rs::ISO_2022_JP;
    if !encoding.encode(text).2 {
        return text.into();
    }
    let mut converted = String::with_capacity(text.len() + 16);
    let mut buffer = [0; 4];
    for ch in text.chars() {
        if encoding.encode(ch.encode_utf8(&mut buffer)).2 {
            converted.push_str(&format!("&#{};", u32::from(ch)));
        } else {
            converted.push(ch);
        }
    }
    converted.into()
}

/// Converts the body of the message `text` for a send in ISO-2022-JP: all of
/// a plain text, only the user's description of a CTCP ACTION. Other CTCP
/// requests keep their text, and with it the check that refuses them. The
/// `display_text` follows, so the log shows what was sent.
pub(crate) fn apply_char_refs(text: &mut String, display_text: &mut String) {
    if let Some(action) = crate::text::action_text(text) {
        let closed = text.ends_with('\u{1}');
        let converted = with_char_refs(action);
        if converted != action {
            let shown = display_text.strip_prefix("* ") == Some(action);
            let new_text = format!(
                "\u{1}ACTION {converted}{}",
                if closed { "\u{1}" } else { "" }
            );
            if shown {
                *display_text = format!("* {converted}");
            }
            *text = new_text;
        }
    } else if !text.starts_with('\u{1}')
        && let std::borrow::Cow::Owned(converted) = with_char_refs(text)
    {
        if display_text == text {
            display_text.clone_from(&converted);
        }
        *text = converted;
    }
}

struct Entry {
    channel: String,
    decode: Option<&'static Encoding>,
    encode: Option<&'static Encoding>,
}

struct State {
    mapping: CaseMapping,
    entries: Vec<Entry>,
    /// The server announced `UTF8ONLY`: only UTF-8 overrides apply.
    utf8_only: bool,
}

impl State {
    fn find(&self, channel: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| self.mapping.same(&entry.channel, channel))
    }

    /// `encoding` unless the server allows UTF-8 only.
    fn allowed(&self, encoding: Option<&'static Encoding>) -> Option<&'static Encoding> {
        encoding.filter(|encoding| !self.utf8_only || *encoding == encoding_rs::UTF_8)
    }
}

/// The overrides of one connection. The line codec reads them on its own
/// thread for every message, while the connection handle changes them.
pub(crate) struct ChannelCharsets {
    state: RwLock<State>,
    /// Whether any override exists, so a connection without one does not
    /// take the lock for each line.
    any: AtomicBool,
    /// The encoding fixed for each queued line until the codec writes it,
    /// oldest first, so a change made while a message waits cannot alter
    /// what was checked when it was accepted.
    pins: Mutex<HashMap<String, VecDeque<Option<&'static Encoding>>>>,
    /// How many pins `pins` holds, so the codec does not take its lock for
    /// a line when there are none.
    pinned: AtomicUsize,
    /// Lines the codec skipped because of `UTF8ONLY`.
    skipped: AtomicUsize,
    /// Lines the codec dropped for holding CR, LF or NUL (#554), and the
    /// wake-up that tells the worker to report them.
    refused: AtomicUsize,
    refusal: tokio::sync::Notify,
}

impl ChannelCharsets {
    /// The configured overrides are all kept as they are: until the network
    /// announces its `CASEMAPPING` the names cannot be told apart reliably,
    /// and merging two that only the default mapping equates (`#a[` and
    /// `#a{`) would lose one for good.
    pub(crate) fn new(entries: &[ChannelEncoding]) -> Result<Self, String> {
        let mut kept: Vec<Entry> = Vec::new();
        for entry in entries {
            if !valid_channel(&entry.channel) {
                return Err(format!("Invalid channel name: {}", entry.channel));
            }
            let decode = entry.decode.as_deref().map(resolve).transpose()?;
            let encode = entry.encode.as_deref().map(resolve).transpose()?;
            kept.retain(|other| other.channel != entry.channel);
            if decode.is_some() || encode.is_some() {
                kept.push(Entry {
                    channel: entry.channel.clone(),
                    decode,
                    encode,
                });
            }
        }
        Ok(Self {
            any: AtomicBool::new(!kept.is_empty()),
            state: RwLock::new(State {
                mapping: CaseMapping::default(),
                entries: kept,
                utf8_only: false,
            }),
            pins: Mutex::new(HashMap::new()),
            pinned: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            refusal: tokio::sync::Notify::new(),
        })
    }

    /// Resolves when the codec has dropped lines; [`Self::take_refused`]
    /// then says how many.
    pub(crate) async fn refusal(&self) {
        self.refusal.notified().await;
    }

    pub(crate) fn take_refused(&self) -> usize {
        self.refused.swap(0, Ordering::Relaxed)
    }

    /// Fixes the encoding of the text of `line`, which is about to be queued
    /// for the codec, until the codec writes it.
    pub(crate) fn pin(&self, line: &str, encoding: Option<&'static Encoding>) {
        if let Ok(mut pins) = self.pins.lock() {
            pins.entry(line.to_owned()).or_default().push_back(encoding);
            self.pinned.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Takes the pin back when `line` could not be queued.
    pub(crate) fn unpin(&self, line: &str) {
        self.take(line, true);
    }

    /// The oldest pin of `line`, or the newest with `newest`.
    fn take(&self, line: &str, newest: bool) -> Option<Option<&'static Encoding>> {
        let mut pins = self.pins.lock().ok()?;
        let queue = pins.get_mut(line)?;
        let pin = if newest {
            queue.pop_back()
        } else {
            queue.pop_front()
        };
        if queue.is_empty() {
            pins.remove(line);
        }
        self.pinned.fetch_sub(1, Ordering::Relaxed);
        pin
    }

    /// Replaces the overrides of `channel`; labels that are `None` follow the
    /// connection, and none at all removes the channel's entry.
    pub(crate) fn set(
        &self,
        channel: &str,
        decode: Option<&str>,
        encode: Option<&str>,
    ) -> Result<(), String> {
        if !valid_channel(channel) {
            return Err(format!("Invalid channel name: {channel}"));
        }
        let decode = decode.map(resolve).transpose()?;
        let encode = encode.map(resolve).transpose()?;
        let Ok(mut state) = self.state.write() else {
            return Err("Character encodings are unavailable.".into());
        };
        if state.utf8_only
            && [decode, encode]
                .into_iter()
                .flatten()
                .any(|encoding| encoding != encoding_rs::UTF_8)
        {
            return Err(
                "The server requires UTF-8 (UTF8ONLY); only UTF-8 can be set for a channel.".into(),
            );
        }
        let mapping = state.mapping;
        state
            .entries
            .retain(|entry| !mapping.same(&entry.channel, channel));
        if decode.is_some() || encode.is_some() {
            state.entries.push(Entry {
                channel: channel.to_owned(),
                decode,
                encode,
            });
        }
        self.any.store(!state.entries.is_empty(), Ordering::Relaxed);
        Ok(())
    }

    /// The network compares channel names with this mapping from now on.
    pub(crate) fn set_casemapping(&self, mapping: CaseMapping) {
        if let Ok(mut state) = self.state.write() {
            state.mapping = mapping;
        }
    }

    /// The server announced `UTF8ONLY`. Returns whether that switches off an
    /// override that is not UTF-8.
    pub(crate) fn set_utf8_only(&self) -> bool {
        let Ok(mut state) = self.state.write() else {
            return false;
        };
        let was = state.utf8_only;
        state.utf8_only = true;
        !was && state.entries.iter().any(|entry| {
            [entry.decode, entry.encode]
                .into_iter()
                .flatten()
                .any(|encoding| encoding != encoding_rs::UTF_8)
        })
    }

    /// How many queued lines `UTF8ONLY` keeps from being written: those
    /// still waiting and those the codec has already skipped.
    pub(crate) fn unsent_for_utf8_only(&self) -> usize {
        let waiting = self.pins.lock().map_or(0, |pins| {
            pins.values()
                .flatten()
                .filter(|pin| pin.is_some_and(|encoding| encoding != encoding_rs::UTF_8))
                .count()
        });
        waiting + self.skipped.load(Ordering::Relaxed)
    }

    pub(crate) fn utf8_only(&self) -> bool {
        self.state.read().is_ok_and(|state| state.utf8_only)
    }
}

impl BodyCharsets for ChannelCharsets {
    fn is_empty(&self) -> bool {
        !self.any.load(Ordering::Relaxed) && self.pinned.load(Ordering::Relaxed) == 0
    }

    fn refused_line(&self) {
        self.refused.fetch_add(1, Ordering::Relaxed);
        self.refusal.notify_one();
    }

    fn take_pin(&self, line: &str) -> Option<Pin> {
        if self.pinned.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let encoding = self.take(line, false)?;
        // A pin keeps what the user set, not what the server allows: a line
        // checked for another encoding before `UTF8ONLY` is not written.
        if self.utf8_only() && encoding.is_some_and(|encoding| encoding != encoding_rs::UTF_8) {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            return Some(Pin::Skip);
        }
        Some(Pin::Encoding(encoding))
    }

    fn decoder(&self, target: &str) -> Option<&'static Encoding> {
        let state = self.state.read().ok()?;
        state.allowed(state.find(target)?.decode)
    }

    fn encoder(&self, target: &str) -> Option<&'static Encoding> {
        let state = self.state.read().ok()?;
        state.allowed(state.find(target)?.encode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn converted(text: &str) -> (String, String) {
        let (mut text, mut shown) = (text.to_owned(), text.to_owned());
        apply_char_refs(&mut text, &mut shown);
        (text, shown)
    }

    #[test]
    fn unencodable_scalars_become_decimal_references_in_order() {
        for (input, want) in [
            ("こんにちは😀", "こんにちは&#128512;"),
            ("🍒おいしい", "&#127826;おいしい"),
            ("ABCあいう", "ABCあいう"),
            ("既に &#128512; という表記", "既に &#128512; という表記"),
            // ZWJ sequence: each scalar on its own, nothing guessed.
            ("👨\u{200d}👩", "&#128104;&#8205;&#128105;"),
            ("☺\u{fe0f}", "&#9786;&#65039;"),
        ] {
            let (text, shown) = converted(input);
            assert_eq!(text, want, "{input}");
            assert_eq!(shown, want, "{input}");
        }
    }

    #[test]
    fn only_the_description_of_an_action_is_converted() {
        let mut text = "\u{1}ACTION 笑う😀\u{1}".to_owned();
        let mut shown = "* 笑う😀".to_owned();
        apply_char_refs(&mut text, &mut shown);
        assert_eq!(text, "\u{1}ACTION 笑う&#128512;\u{1}");
        assert_eq!(shown, "* 笑う&#128512;");
        // Other CTCP stays as it is, so it is still refused.
        let (text, _) = converted("\u{1}VERSION 😀\u{1}");
        assert_eq!(text, "\u{1}VERSION 😀\u{1}");
    }

    fn one(channel: &str, decode: Option<&str>, encode: Option<&str>) -> ChannelCharsets {
        ChannelCharsets::new(&[ChannelEncoding {
            channel: channel.into(),
            decode: decode.map(str::to_owned),
            encode: encode.map(str::to_owned),
        }])
        .unwrap()
    }

    #[test]
    fn each_side_is_looked_up_on_its_own_under_the_case_mapping() {
        let charsets = one("#Jp[x]", Some("ISO-2022-JP"), None);
        assert!(!charsets.is_empty());
        assert_eq!(charsets.decoder("#jp{x}"), Some(encoding_rs::ISO_2022_JP));
        assert_eq!(charsets.encoder("#jp{x}"), None);
        assert_eq!(charsets.decoder("#other"), None);
        // `~` and `^` are the same only under RFC 1459 itself.
        let charsets = one("#a~", Some("Big5"), None);
        assert!(charsets.decoder("#a^").is_some());
        charsets.set_casemapping(CaseMapping::Ascii);
        assert!(charsets.decoder("#a^").is_none());
    }

    #[test]
    fn configured_names_only_the_default_mapping_equates_stay_apart() {
        let entry = |channel: &str, label: &str| ChannelEncoding {
            channel: channel.into(),
            decode: Some(label.into()),
            encode: None,
        };
        let charsets =
            ChannelCharsets::new(&[entry("#a[", "Shift_JIS"), entry("#a{", "UTF-8")]).unwrap();
        charsets.set_casemapping(CaseMapping::Ascii);
        assert_eq!(charsets.decoder("#a["), Some(encoding_rs::SHIFT_JIS));
        assert_eq!(charsets.decoder("#a{"), Some(encoding_rs::UTF_8));
        // The same name twice is one entry, the later one.
        let charsets = ChannelCharsets::new(&[entry("#a", "Big5"), entry("#a", "GBK")]).unwrap();
        assert_eq!(charsets.decoder("#a"), Some(encoding_rs::GBK));
    }

    #[test]
    fn a_pin_outlives_changes_and_is_taken_once_in_order() {
        let charsets = one("#a", None, Some("GBK"));
        let line = "PRIVMSG #a :x\r\n";
        charsets.pin(line, Some(encoding_rs::BIG5));
        charsets.pin(line, None);
        charsets.set("#a", None, None).unwrap();
        assert!(!charsets.is_empty());
        assert_eq!(charsets.take_pin("PRIVMSG #b :x\r\n"), None);
        assert_eq!(
            charsets.take_pin(line),
            Some(Pin::Encoding(Some(encoding_rs::BIG5)))
        );
        charsets.unpin(line);
        assert_eq!(charsets.take_pin(line), None);
        assert!(charsets.is_empty());
    }

    #[test]
    fn utf8_only_keeps_a_pinned_legacy_line_from_being_written() {
        let charsets = one("#a", None, Some("Shift_JIS"));
        let line = "PRIVMSG #a :x\r\n";
        charsets.pin(line, Some(encoding_rs::SHIFT_JIS));
        charsets.pin(line, Some(encoding_rs::UTF_8));
        charsets.pin(line, None);
        charsets.set_utf8_only();
        assert_eq!(charsets.unsent_for_utf8_only(), 1);
        assert_eq!(charsets.take_pin(line), Some(Pin::Skip));
        assert_eq!(charsets.unsent_for_utf8_only(), 1);
        assert_eq!(
            charsets.take_pin(line),
            Some(Pin::Encoding(Some(encoding_rs::UTF_8)))
        );
        assert_eq!(charsets.take_pin(line), Some(Pin::Encoding(None)));
    }

    #[test]
    fn setting_replaces_and_clearing_removes() {
        let charsets = one("#a", Some("Big5"), Some("Big5"));
        charsets.set("#A", None, Some("GBK")).unwrap();
        assert_eq!(charsets.decoder("#a"), None);
        assert_eq!(charsets.encoder("#a"), Some(encoding_rs::GBK));
        charsets.set("#a", None, None).unwrap();
        assert!(charsets.is_empty());
        assert!(charsets.set("#a", Some("nope"), None).is_err());
        assert!(charsets.set("nick", Some("Big5"), None).is_err());
        assert!(charsets.is_empty());
    }

    #[test]
    fn utf8_only_ignores_other_encodings_and_refuses_new_ones() {
        let charsets = one("#a", Some("Big5"), Some("UTF-8"));
        assert!(charsets.set_utf8_only());
        // Announced again, or nothing to switch off: nothing to report.
        assert!(!charsets.set_utf8_only());
        assert_eq!(charsets.decoder("#a"), None);
        assert_eq!(charsets.encoder("#a"), Some(encoding_rs::UTF_8));
        assert!(charsets.set("#b", Some("GBK"), None).is_err());
        assert!(charsets.set("#b", Some("UTF-8"), None).is_ok());
        assert!(charsets.utf8_only());
    }
}
