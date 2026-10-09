//! Web links in message text, found once when a message is added
//! (`Message::links`) rather than on every draw.

use std::ops::Range;

/// The `http(s)` links in `text`: each byte range with its parsed URL.
/// Sentence punctuation, brackets that close outside the link and Japanese
/// punctuation after a link are not part of it.
pub fn find(text: &str) -> Vec<(Range<usize>, String)> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let next = ["https://", "http://"]
            .into_iter()
            .filter_map(|scheme| text[cursor..].find(scheme).map(|offset| cursor + offset))
            .min();
        let Some(start) = next else { break };
        // An IPv6 literal host such as `https://[::1]/` keeps its brackets.
        let host_start = start + text[start..].find("://").map_or(0, |i| i + 3);
        let scan_from = match text[host_start..].strip_prefix('[') {
            Some(rest) => rest
                .find(']')
                .map_or(host_start, |close| host_start + 1 + close + 1),
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

    #[test]
    fn long_runs_of_closing_brackets_are_trimmed_in_linear_time() {
        let text = format!("https://example.org/{}", ")".repeat(16_000));
        let started = std::time::Instant::now();
        let urls = find(&text);
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].1, "https://example.org/");
        assert_eq!(urls[0].0, 0.."https://example.org/".len());
        let text = format!("https://example.org/(a{}", ")".repeat(16_000));
        assert_eq!(find(&text)[0].1, "https://example.org/(a)");
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
