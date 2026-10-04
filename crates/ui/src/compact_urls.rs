//! Compact display of long URLs in the channel log.
//!
//! Only the presentation changes: [`Compact`] builds the text to draw from a
//! message's text and maps byte positions between the two, so selection,
//! copying, searching and saving keep working on the original text.

use std::ops::Range;

/// Longest label, in characters, shown for a URL before it is shortened. The
/// host is never cut, so a long host can exceed it.
const LIMIT: usize = 28;

/// Marks a shortened URL as a link, drawn before its label.
const ICON: &str = "↗ ";

/// A URL the log shows in place of its text.
struct Span {
    original: Range<usize>,
    shown: Range<usize>,
}

/// A message's text as drawn, with the URLs found in it.
pub(crate) struct Compact {
    text: String,
    spans: Vec<Span>,
}

/// The label for `url`, a URL as written in the text, when it is long enough
/// to shorten: no scheme, the whole host, then as much of the rest as fits,
/// ended with an ellipsis.
fn label(url: &str) -> Option<String> {
    let bare = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    if bare.chars().count() <= LIMIT {
        return None;
    }
    let host_end = bare.find(['/', '?', '#']).unwrap_or(bare.len());
    let (host, rest) = bare.split_at(host_end);
    let room = LIMIT.saturating_sub(host.chars().count() + 1);
    let kept: String = rest.chars().take(room).collect();
    Some(format!("{ICON}{host}{kept}…"))
}

impl Compact {
    /// Draws the URLs of `text` (original byte ranges) in short form when
    /// `enabled`.
    pub(crate) fn new(text: &str, urls: &[(Range<usize>, String)], enabled: bool) -> Self {
        let mut shown = String::new();
        let mut spans = Vec::new();
        let mut cursor = 0;
        if enabled {
            for (range, _) in urls {
                let Some(label) = label(&text[range.clone()]) else {
                    continue;
                };
                shown.push_str(&text[cursor..range.start]);
                let start = shown.len();
                shown.push_str(&label);
                spans.push(Span {
                    original: range.clone(),
                    shown: start..shown.len(),
                });
                cursor = range.end;
            }
        }
        shown.push_str(&text[cursor..]);
        Self { text: shown, spans }
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

    /// The full URLs whose short form is drawn, with where.
    pub(crate) fn shortened<'a>(
        &'a self,
        urls: &'a [(Range<usize>, String)],
    ) -> impl Iterator<Item = (Range<usize>, &'a str)> + 'a {
        self.spans.iter().filter_map(|span| {
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
        crate::log_urls(text)
    }

    fn compact(text: &str) -> Compact {
        Compact::new(text, &urls(text), true)
    }

    #[test]
    fn short_urls_stay_as_written() {
        let text = "see https://example.com/a now";
        let compact = compact(text);
        assert_eq!(compact.text(), text);
        assert_eq!(compact.shortened(&urls(text)).count(), 0);
    }

    #[test]
    fn a_long_path_is_cut_after_the_host() {
        let text = "https://github.com/koshian/cayenchat/issues/151";
        let compact = compact(text);
        assert_eq!(compact.text(), "↗ github.com/koshian/cayencha…");
    }

    #[test]
    fn a_long_query_is_cut_too() {
        let compact = compact("http://example.com/?utm_source=a&utm_medium=b&utm_campaign=c");
        assert_eq!(compact.text(), "↗ example.com/?utm_source=a&u…");
    }

    #[test]
    fn a_long_host_is_never_cut() {
        let host = "very-long-subdomain.another-long-label.example.org";
        let text = format!("https://{host}/path/to/page");
        let compact = compact(&text);
        assert_eq!(compact.text(), format!("↗ {host}…"));
    }

    #[test]
    fn unicode_is_cut_on_characters() {
        let text = "https://例え.jp/日本語のパスがとても長いページ/さらに/続くよどこまでも";
        let compact = compact(text);
        assert_eq!(
            compact.text(),
            "↗ 例え.jp/日本語のパスがとても長いページ/さらに/続…"
        );
    }

    #[test]
    fn disabled_draws_the_text_unchanged() {
        let text = "https://github.com/koshian/cayenchat/issues/151";
        let compact = Compact::new(text, &urls(text), false);
        assert_eq!(compact.text(), text);
    }

    #[test]
    fn positions_map_around_a_short_url() {
        let text = "go https://github.com/koshian/cayenchat/issues/151 now";
        let compact = compact(text);
        let url_end = text.find(" now").unwrap();
        assert_eq!(compact.text(), "go ↗ github.com/koshian/cayencha… now");
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
        let compact = Compact::new(text, &found, true);
        let shortened: Vec<_> = compact.shortened(&found).collect();
        assert_eq!(shortened.len(), 1);
        assert_eq!(shortened[0].1, found[0].1);
        assert_eq!(compact.shown_urls(&found)[0].0, 0..compact.text().len());
    }

    #[test]
    fn the_highlight_and_tooltip_cover_the_drawn_label() {
        let text = "see https://github.com/koshian/cayenchat/issues/151?tab=a ok";
        let found = urls(text);
        let compact = Compact::new(text, &found, true);
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
