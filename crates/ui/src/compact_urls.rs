// SPDX-License-Identifier: GPL-3.0-only
//! Presentation-only rewriting of a log message: long URLs shortened and
//! HTML numeric character references drawn as their characters.
//!
//! Only the presentation changes: [`Compact`] builds the text to draw from a
//! message's text and maps byte positions between the two, so selection,
//! searching and saving keep working on the original text. Copying a
//! selection uses the drawn characters of decoded references
//! ([`Compact::copy_text`]).

use std::ops::Range;

/// Longest label, in characters, shown for a URL before it is shortened. The
/// host is never cut, so a long host can exceed it.
const LIMIT: usize = 28;

/// Room kept before every URL for the link icon. The log paints the icon
/// (`icons/ui/link.svg`) over this em space; it is never copied.
pub(crate) const ICON: &str = "\u{2003}";

/// Text the log shows in place of the original.
struct Span {
    original: Range<usize>,
    shown: Range<usize>,
    /// Characters of decoded character references, as opposed to a URL.
    decoded: bool,
    /// A URL whose label leaves out part of it.
    cut: bool,
}

/// A message's text as drawn, with the URLs found in it.
pub(crate) struct Compact {
    text: String,
    spans: Vec<Span>,
}

/// The label for `url`, a URL as written in the text, and whether it is cut.
/// A long URL loses its scheme and shows the whole host, then as much of the
/// rest as fits, ended with an ellipsis; a short one is kept as written.
fn label(url: &str) -> (String, bool) {
    let bare = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    if bare.chars().count() <= LIMIT {
        return (format!("{ICON}{url}"), false);
    }
    let host_end = bare.find(['/', '?', '#']).unwrap_or(bare.len());
    let (host, rest) = bare.split_at(host_end);
    let room = LIMIT.saturating_sub(host.chars().count() + 1);
    let kept: String = rest.chars().take(room).collect();
    (format!("{ICON}{host}{kept}…"), true)
}

impl Compact {
    /// Draws the URLs of `text` (original byte ranges) in short form when
    /// `shorten`, and its numeric character references as characters when
    /// `decode`. References inside URLs are never decoded.
    pub(crate) fn new(
        text: &str,
        urls: &[(Range<usize>, String)],
        shorten: bool,
        decode: bool,
    ) -> Self {
        // (original range, shown text, decoded reference, cut URL)
        let mut replacements: Vec<(Range<usize>, String, bool, bool)> = Vec::new();
        if shorten {
            replacements.extend(urls.iter().map(|(range, _)| {
                let (label, cut) = label(&text[range.clone()]);
                (range.clone(), label, false, cut)
            }));
        }
        if decode {
            replacements.extend(
                crate::char_refs::find(text, urls)
                    .into_iter()
                    .map(|(range, shown)| (range, shown, true, false)),
            );
            replacements.sort_by_key(|(range, ..)| range.start);
        }
        let mut shown = String::new();
        let mut spans = Vec::new();
        let mut cursor = 0;
        for (range, label, decoded, cut) in replacements {
            shown.push_str(&text[cursor..range.start]);
            let start = shown.len();
            shown.push_str(&label);
            spans.push(Span {
                original: range.clone(),
                shown: start..shown.len(),
                decoded,
                cut,
            });
            cursor = range.end;
        }
        shown.push_str(&text[cursor..]);
        Self { text: shown, spans }
    }

    /// The text to copy for the original byte range `range`: what is drawn,
    /// except that short URLs keep their full text.
    pub(crate) fn copy_text(&self, text: &str, range: Range<usize>) -> String {
        let mut copied = String::new();
        let mut cursor = range.start;
        for span in self.spans.iter().filter(|span| span.decoded) {
            if span.original.end <= cursor || span.original.start >= range.end {
                continue;
            }
            // Selections snap to the ends of a replacement.
            copied.push_str(&text[cursor..span.original.start.max(cursor)]);
            copied.push_str(&self.text[span.shown.clone()]);
            cursor = span.original.end;
        }
        copied.push_str(&text[cursor.min(range.end)..range.end]);
        copied
    }

    /// The decoded character references: where their characters are drawn
    /// and the text as received.
    pub(crate) fn decoded<'a>(
        &'a self,
        text: &'a str,
    ) -> impl Iterator<Item = (Range<usize>, &'a str)> + 'a {
        self.spans
            .iter()
            .filter(|span| span.decoded)
            .map(|span| (span.shown.clone(), &text[span.original.clone()]))
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The original byte for a byte of the drawn text. Inside a short URL it
    /// is the URL's start or end, whichever is nearer.
    pub(crate) fn original(&self, byte: usize) -> usize {
        let mut delta: isize = 0;
        for span in &self.spans {
            if byte >= span.shown.end {
                delta = span.original.end as isize - span.shown.end as isize;
            } else if byte > span.shown.start {
                let middle = span.shown.start + span.shown.len() / 2;
                return if byte < middle {
                    span.original.start
                } else {
                    span.original.end
                };
            } else {
                break;
            }
        }
        (byte as isize + delta) as usize
    }

    /// The drawn range covering an original byte range; a short URL the range
    /// touches is covered whole.
    pub(crate) fn shown_range(&self, range: Range<usize>) -> Range<usize> {
        self.shown(range.start, false)..self.shown(range.end, true)
    }

    fn shown(&self, byte: usize, end: bool) -> usize {
        let mut delta: isize = 0;
        for span in &self.spans {
            if byte >= span.original.end {
                delta = span.shown.end as isize - span.original.end as isize;
            } else if byte > span.original.start {
                return if end {
                    span.shown.end
                } else {
                    span.shown.start
                };
            } else {
                break;
            }
        }
        (byte as isize + delta) as usize
    }

    /// `urls` (original ranges) as ranges of the drawn text.
    pub(crate) fn shown_urls(
        &self,
        urls: &[(Range<usize>, String)],
    ) -> Vec<(Range<usize>, String)> {
        urls.iter()
            .map(|(range, url)| (self.shown_range(range.clone()), url.clone()))
            .collect()
    }

    /// Where every URL is drawn with its icon, shortened or not.
    pub(crate) fn linked(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.spans
            .iter()
            .filter(|span| !span.decoded)
            .map(|span| span.shown.clone())
    }

    /// The full URLs whose short form is drawn, with where.
    pub(crate) fn shortened<'a>(
        &'a self,
        urls: &'a [(Range<usize>, String)],
    ) -> impl Iterator<Item = (Range<usize>, &'a str)> + 'a {
        self.spans
            .iter()
            .filter(|span| !span.decoded && span.cut)
            .filter_map(|span| {
                urls.iter()
                    .find(|(range, _)| *range == span.original)
                    .map(|(_, url)| (span.shown.clone(), url.as_str()))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<(Range<usize>, String)> {
        cayenchat_app::links::find(text)
    }

    fn compact(text: &str) -> Compact {
        Compact::new(text, &urls(text), true, false)
    }

    #[test]
    fn short_urls_are_kept_whole_with_an_icon() {
        let text = "see https://example.com/a now";
        let compact = compact(text);
        // Not cut, but marked like the long ones.
        assert_eq!(compact.text(), "see \u{2003}https://example.com/a now");
        assert_eq!(compact.shortened(&urls(text)).count(), 0);
        assert_eq!(compact.linked().next(), Some(4..28));
        assert_eq!(compact.original(4), 4);
        assert_eq!(compact.original(28), 25);
        assert_eq!(compact.shown_range(4..25), 4..28);
    }

    #[test]
    fn a_long_path_is_cut_after_the_host() {
        let text = "https://github.com/koshian/cayenchat/issues/151";
        let compact = compact(text);
        assert_eq!(compact.text(), "\u{2003}github.com/koshian/cayencha…");
    }

    #[test]
    fn a_long_query_is_cut_too() {
        let compact = compact("http://example.com/?utm_source=a&utm_medium=b&utm_campaign=c");
        assert_eq!(compact.text(), "\u{2003}example.com/?utm_source=a&u…");
    }

    #[test]
    fn a_long_host_is_never_cut() {
        let host = "very-long-subdomain.another-long-label.example.org";
        let text = format!("https://{host}/path/to/page");
        let compact = compact(&text);
        assert_eq!(compact.text(), format!("\u{2003}{host}…"));
    }

    #[test]
    fn unicode_is_cut_on_characters() {
        let text = "https://例え.jp/日本語のパスがとても長いページ/さらに/続くよどこまでも";
        let compact = compact(text);
        assert_eq!(
            compact.text(),
            "\u{2003}例え.jp/日本語のパスがとても長いページ/さらに/続…"
        );
    }

    #[test]
    fn disabled_draws_the_text_unchanged() {
        let text = "https://github.com/koshian/cayenchat/issues/151";
        let compact = Compact::new(text, &urls(text), false, false);
        assert_eq!(compact.text(), text);
    }

    #[test]
    fn positions_map_around_a_short_url() {
        let text = "go https://github.com/koshian/cayenchat/issues/151 now";
        let compact = compact(text);
        let url_end = text.find(" now").unwrap();
        assert_eq!(
            compact.text(),
            "go \u{2003}github.com/koshian/cayencha… now"
        );
        let shown_end = compact.text().find(" now").unwrap();
        // Before and after the URL positions shift; inside they snap to its ends.
        assert_eq!(compact.original(2), 2);
        assert_eq!(compact.original(3), 3);
        assert_eq!(compact.original(4), 3);
        assert_eq!(compact.original(shown_end - 1), url_end);
        assert_eq!(compact.original(shown_end), url_end);
        assert_eq!(compact.original(shown_end + 4), url_end + 4);
        assert_eq!(compact.shown_range(0..2), 0..2);
        assert_eq!(compact.shown_range(5..9), 3..shown_end);
        assert_eq!(
            compact.shown_range(url_end..text.len()),
            shown_end..compact.text().len()
        );
    }

    #[test]
    fn the_full_url_is_kept_for_a_shortened_one() {
        let text = "https://github.com/koshian/cayenchat/issues/151?tab=a";
        let found = urls(text);
        let compact = Compact::new(text, &found, true, false);
        let shortened: Vec<_> = compact.shortened(&found).collect();
        assert_eq!(shortened.len(), 1);
        assert_eq!(shortened[0].1, found[0].1);
        assert_eq!(compact.shown_urls(&found)[0].0, 0..compact.text().len());
    }

    #[test]
    fn the_highlight_and_tooltip_cover_the_drawn_label() {
        let text = "see https://github.com/koshian/cayenchat/issues/151?tab=a ok";
        let found = urls(text);
        let compact = Compact::new(text, &found, true, false);
        // Rendering looks the URLs up by their original ranges.
        let shortened: Vec<_> = compact.shortened(&found).collect();
        assert_eq!(shortened.len(), 1);
        let (range, url) = &shortened[0];
        assert_eq!(url, &found[0].1);
        assert_eq!(*range, compact.shown_urls(&found)[0].0);
        assert!(compact.text()[range.clone()].contains("github.com"));
        // Drawn ranges never match original ranges, so they must not be used.
        let drawn = compact.shown_urls(&found);
        assert_eq!(compact.shortened(&drawn).count(), 0);
    }
}

#[cfg(test)]
mod decoded_tests {
    use super::*;

    fn decoded(text: &str) -> Compact {
        Compact::new(text, &cayenchat_app::links::find(text), false, true)
    }

    #[test]
    fn references_are_drawn_as_characters() {
        let text = "&#x1F352;&#x1F352;&#127826; 大当たり";
        let compact = decoded(text);
        assert_eq!(compact.text(), "🍒🍒🍒 大当たり");
        let runs: Vec<_> = compact.decoded(text).collect();
        assert_eq!(runs, [(0..12, "&#x1F352;&#x1F352;&#127826;")]);
    }

    #[test]
    fn disabled_leaves_references_as_written() {
        let text = "&#x1F352; hi";
        let compact = Compact::new(text, &[], true, false);
        assert_eq!(compact.text(), text);
        assert_eq!(compact.decoded(text).count(), 0);
    }

    #[test]
    fn positions_map_around_a_reference() {
        let text = "a &#x1F352; b";
        let compact = decoded(text);
        assert_eq!(compact.text(), "a 🍒 b");
        assert_eq!(compact.original(2), 2);
        assert_eq!(compact.original(6), 11);
        assert_eq!(compact.original(8), 13);
        // A selection touching the character covers all of it.
        assert_eq!(compact.shown_range(3..5), 2..6);
        assert_eq!(compact.shown_range(11..13), 6..8);
    }

    #[test]
    fn copying_uses_the_drawn_characters() {
        let text = "x &#x1F352;&#127826; y";
        let compact = decoded(text);
        assert_eq!(compact.copy_text(text, 0..text.len()), "x 🍒🍒 y");
        assert_eq!(compact.copy_text(text, 2..20), "🍒🍒");
        assert_eq!(compact.copy_text(text, 0..2), "x ");
        assert_eq!(decoded("plain").copy_text("plain", 1..4), "lai");
    }

    #[test]
    fn urls_and_references_compose() {
        let text = "&#65; https://github.com/koshian/cayenchat/issues/151?x=&#66 &#67;";
        let found = cayenchat_app::links::find(text);
        let compact = Compact::new(text, &found, true, true);
        assert_eq!(compact.text(), "A \u{2003}github.com/koshian/cayencha… C");
        assert_eq!(compact.shortened(&found).count(), 1);
        assert_eq!(compact.decoded(text).count(), 2);
        let copied = compact.copy_text(text, 0..text.len());
        assert_eq!(
            copied,
            "A https://github.com/koshian/cayenchat/issues/151?x=&#66 C"
        );
    }

    #[test]
    fn urls_with_references_are_not_altered() {
        let text = "https://example.com/?a=&#65;";
        let found = cayenchat_app::links::find(text);
        let compact = Compact::new(text, &found, false, true);
        assert_eq!(compact.text(), text);
    }
}
