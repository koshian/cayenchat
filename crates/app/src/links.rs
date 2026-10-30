// SPDX-License-Identifier: MPL-2.0
//! Web links in message text, found once when a message is added
//! (`Message::links`) rather than on every draw.

use std::ops::Range;

/// The `http(s)` links in `text`: each byte range with its parsed URL.
/// Sentence punctuation, brackets that close outside the link and Japanese
/// punctuation after a link are not part of it.
pub fn find(text: &str) -> Vec<(Range<usize>, String)> {
    let mut found = Vec::new();
    let mut cursor = 0;
    let mut https = Lookahead::default();
    let mut http = Lookahead::default();
    let mut close = Lookahead::default();
    while cursor < text.len() {
        let next = [
            https.find(text, "https://", cursor),
            http.find(text, "http://", cursor),
        ]
        .into_iter()
        .flatten()
        .min();
        let Some(start) = next else { break };
        // An IPv6 literal host such as `https://[::1]/` keeps its brackets.
        let host_start = start + "://".len() + text[start..].find("://").unwrap_or(0);
        let scan_from = match text[host_start..].strip_prefix('[') {
            Some(_) => close
                .find(text, "]", host_start + 1)
                .map_or(host_start, |at| at + 1),
            None => host_start,
        };
        let mut end = text[scan_from..]
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace() || "<>[]\"'。、".contains(*ch))
            .map(|(offset, _)| scan_from + offset)
            .unwrap_or(text.len());
        // Counted once and updated as characters are trimmed, so a run of
        // closing brackets stays linear.
        let opens = text[start..end].matches('(').count();
        let mut closes = text[start..end].matches(')').count();
        while end > start {
            let Some(last) = text[..end].chars().last() else {
                break;
            };
            let closes_bracket = last == ')' && closes > opens;
            if !(closes_bracket || ".,;:!?}」』".contains(last)) {
                break;
            }
            if last == ')' {
                closes -= 1;
            }
            end -= last.len_utf8();
        }
        let candidate = &text[start..end];
        if let Ok(url) = url::Url::parse(candidate)
            && matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
        {
            found.push((start..end, url.into()));
        }
        cursor = end.max(start + 1);
    }
    found
}

/// The next occurrence of one pattern at or after a position that only moves
/// forward. The last result is reused, and a search that found nothing is
/// never repeated, so many candidates cost one pass over the text.
#[derive(Default)]
struct Lookahead(Option<Option<usize>>);

impl Lookahead {
    fn find(&mut self, text: &str, pattern: &str, from: usize) -> Option<usize> {
        match self.0 {
            Some(None) => return None,
            Some(Some(at)) if at >= from => return Some(at),
            _ => {}
        }
        let at = text.get(from..)?.find(pattern).map(|offset| from + offset);
        self.0 = Some(at);
        at
    }
}

#[cfg(test)]
mod tests {
    use super::find;

    #[test]
    fn finds_only_web_urls_without_sentence_punctuation() {
        let text =
            "see https://example.org/a?q=1, and http://example.jp/path。 ftp://example.org/x";
        let urls = find(text);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].1, "https://example.org/a?q=1");
        assert_eq!(urls[1].1, "http://example.jp/path");
        assert_eq!(&text[urls[0].0.clone()], urls[0].1);
    }

    #[test]
    fn markdown_links_and_brackets_do_not_leak_into_urls() {
        let text = " [https://x.com/a/status/1](https://x.com/a/status/1)";
        let urls = find(text);
        assert_eq!(urls.len(), 2);
        assert!(
            urls.iter()
                .all(|(_, url)| url == "https://x.com/a/status/1")
        );
        let text = "(see https://example.org/a_(b)) https://example.org/c)";
        let urls = find(text);
        assert_eq!(urls[0].1, "https://example.org/a_(b)");
        assert_eq!(urls[1].1, "https://example.org/c");
    }

    /// The best of several runs of `find` on the text built for `scale`
    /// repetitions. The minimum drops slices lost to a busy host.
    fn best_time(build: &dyn Fn(usize) -> String, scale: usize) -> std::time::Duration {
        let text = build(scale);
        (0..5)
            .map(|_| {
                let started = std::time::Instant::now();
                std::hint::black_box(find(std::hint::black_box(&text)));
                started.elapsed()
            })
            .min()
            .unwrap()
    }

    /// Fails when the time grows much faster than the input. An input 8 times
    /// larger should take about 8 times longer (a quadratic scan 64 times),
    /// so the limit of 24 times sits between them whatever the host speed.
    /// The small time has a floor so timer granularity cannot inflate the ratio.
    /// A loaded host can still stall one measurement for milliseconds (a CI
    /// run saw 1 ms and 32 ms), so the pair is measured again and only a
    /// ratio that stays over the limit every time fails. A quadratic scan
    /// does, since it is slow in every attempt.
    fn assert_linear(build: &dyn Fn(usize) -> String, small: usize) {
        let floor = std::time::Duration::from_micros(200);
        let mut last = String::new();
        for _ in 0..4 {
            let base = best_time(build, small).max(floor);
            let large = best_time(build, small * 8);
            if large < base * 24 {
                return;
            }
            last = format!("{base:?} for {small}, {large:?} for 8x");
        }
        panic!("{last}");
    }

    #[test]
    fn long_runs_of_closing_brackets_are_trimmed_in_linear_time() {
        assert_linear(
            &|n| format!("https://example.org/{}", ")".repeat(n * 10)),
            1_000,
        );
        let text = format!("https://example.org/{}", ")".repeat(16_000));
        let urls = find(&text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].1, "https://example.org/");
        assert_eq!(urls[0].0, 0.."https://example.org/".len());
        let text = format!("https://example.org/(a{}", ")".repeat(16_000));
        assert_eq!(find(&text)[0].1, "https://example.org/(a)");
    }

    #[test]
    fn many_candidates_are_found_in_linear_time() {
        for unit in [
            "https://[ ",
            "https://[ http:// ",
            "http://example.org/ ",
            "https://x ",
        ] {
            assert_linear(&|n| unit.repeat(n), 400);
        }
        for text in [
            "https://[ ".repeat(1_600),
            "https://[ http:// ".repeat(900),
            "http://example.org/ ".repeat(800),
            "https://x ".repeat(1_600),
        ] {
            let urls = find(&text);
            assert_eq!(
                urls.len(),
                text.matches("http://example.org/").count() + text.matches("https://x").count()
            );
        }
    }

    #[test]
    fn ipv6_literal_hosts_keep_their_brackets() {
        let text =
            "[https://[2001:db8::1]:8080/path](https://[2001:db8::1]:8080/path) http://[::1]/";
        let urls = find(text);
        assert_eq!(urls.len(), 3);
        assert_eq!(urls[0].1, "https://[2001:db8::1]:8080/path");
        assert_eq!(urls[1].1, "https://[2001:db8::1]:8080/path");
        assert_eq!(urls[2].1, "http://[::1]/");
    }
}
