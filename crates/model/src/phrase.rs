// SPDX-License-Identifier: MPL-2.0
//! Japanese phrase (bunsetsu) boundaries for line wrapping, from Google's
//! BudouX model (v0.9.3, Apache-2.0; see `data/README.md`).
//!
//! The scoring follows the documented algorithm of BudouX's `parser.py`:
//! at every position the weights of the surrounding unigrams, bigrams and
//! trigrams are added to a base score of minus half the sum of all weights,
//! and a score above zero starts a new phrase. Nothing here changes the text;
//! offsets are UTF-8 byte offsets into it.

use std::collections::HashMap;
use std::sync::OnceLock;

const MODEL_JSON: &str = include_str!("../data/budoux-ja-v0.9.3.json");

/// Feature groups in the order the scorer uses them.
const GROUPS: [&str; 13] = [
    "UW1", "UW2", "UW3", "UW4", "UW5", "UW6", "BW1", "BW2", "BW3", "TW1", "TW2", "TW3", "TW4",
];

struct Model {
    groups: [HashMap<Box<str>, i64>; 13],
    /// Twice the base score's magnitude: the sum of every weight, so the
    /// comparison `score > 0` stays in integers.
    total: i64,
}

fn model() -> &'static Model {
    static MODEL: OnceLock<Model> = OnceLock::new();
    MODEL.get_or_init(|| {
        let parsed: HashMap<String, HashMap<String, i64>> =
            serde_json::from_str(MODEL_JSON).expect("embedded BudouX model is valid JSON");
        let mut total = 0;
        let groups = GROUPS.map(|name| {
            parsed
                .get(name)
                .into_iter()
                .flatten()
                .map(|(feature, weight)| {
                    total += weight;
                    (feature.as_str().into(), *weight)
                })
                .collect()
        });
        Model { groups, total }
    })
}

/// Byte offsets at which BudouX starts a new phrase (never 0). Empty for
/// text without Japanese characters, which is not analysed at all.
pub fn boundaries(text: &str) -> Vec<usize> {
    if !text.chars().any(is_kana) {
        return Vec::new();
    }
    scan(text)
        .into_iter()
        .filter_map(|(at, _, split)| split.then_some(at))
        .collect()
}

/// Byte offsets between two Japanese characters where BudouX does *not*
/// start a phrase: a wrapper that would otherwise break between any two such
/// characters should keep them together there. Text around the Japanese
/// (ASCII words, URLs, emoji) contributes nothing, so it wraps as before.
pub fn joins(text: &str) -> Vec<u32> {
    if !text.chars().any(is_kana) {
        return Vec::new();
    }
    scan(text)
        .into_iter()
        .filter_map(|(at, both_japanese, split)| {
            (both_japanese && !split)
                .then(|| u32::try_from(at).ok())
                .flatten()
        })
        .collect()
}

/// For each character after the first: its byte offset, whether it and its
/// predecessor are both Japanese, and whether BudouX splits before it.
fn scan(text: &str) -> Vec<(usize, bool, bool)> {
    let model = model();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let [
        uw1,
        uw2,
        uw3,
        uw4,
        uw5,
        uw6,
        bw1,
        bw2,
        bw3,
        tw1,
        tw2,
        tw3,
        tw4,
    ] = &model.groups;
    let mut key = String::new();
    let mut weight = |group: &HashMap<Box<str>, i64>, from: usize, to: usize| -> i64 {
        key.clear();
        key.extend(chars[from..to].iter().map(|(_, c)| c));
        group.get(key.as_str()).copied().unwrap_or(0)
    };
    let n = chars.len();
    let mut out = Vec::with_capacity(n.saturating_sub(1));
    for i in 1..n {
        let mut score = 0;
        if i > 2 {
            score += weight(uw1, i - 3, i - 2);
        }
        if i > 1 {
            score += weight(uw2, i - 2, i - 1);
        }
        score += weight(uw3, i - 1, i);
        score += weight(uw4, i, i + 1);
        if i + 1 < n {
            score += weight(uw5, i + 1, i + 2);
        }
        if i + 2 < n {
            score += weight(uw6, i + 2, i + 3);
        }
        if i > 1 {
            score += weight(bw1, i - 2, i);
        }
        score += weight(bw2, i - 1, i + 1);
        if i + 1 < n {
            score += weight(bw3, i, i + 2);
        }
        if i > 2 {
            score += weight(tw1, i - 3, i);
        }
        if i > 1 {
            score += weight(tw2, i - 2, i + 1);
        }
        if i + 1 < n {
            score += weight(tw3, i - 1, i + 2);
        }
        if i + 2 < n {
            score += weight(tw4, i, i + 3);
        }
        // score - total / 2 > 0
        let split = 2 * score > model.total;
        out.push((
            chars[i].0,
            is_japanese(chars[i - 1].1) && is_japanese(chars[i].1),
            split,
        ));
    }
    out
}

/// Hiragana, katakana (including the small and half-width forms). Kanji are
/// shared with Chinese, so text is treated as Japanese only when it has
/// kana; kanji-only text (and Chinese) is left to the normal wrapper. A
/// mixed Chinese/Japanese text is analysed as Japanese.
fn is_kana(c: char) -> bool {
    matches!(c,
        '\u{3041}'..='\u{30FF}'
        | '\u{31F0}'..='\u{31FF}'
        | '\u{FF66}'..='\u{FF9F}')
}

/// Kana, kanji, CJK punctuation and full-width forms: the characters a
/// wrapper may otherwise break between freely. Half-width Hangul
/// (U+FFA0..) is not included.
pub fn is_japanese(c: char) -> bool {
    is_kana(c)
        || matches!(c,
            '\u{3000}'..='\u{303F}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF00}'..='\u{FF9F}'
            | '\u{20000}'..='\u{2FFFF}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let mut start = 0;
        for at in boundaries(text) {
            parts.push(&text[start..at]);
            start = at;
        }
        parts.push(&text[start..]);
        parts
    }

    /// Expected values come from the official v0.9.3 `parser.py` run with the
    /// same `ja.json`.
    #[test]
    fn matches_official_parser() {
        let cases: [(&str, &[&str]); 5] = [
            ("今日は天気です。", &["今日は", "天気です。"]),
            (
                "私は遠くから来た友達と一緒に、美味しいラーメンを食べに行きました。",
                &[
                    "私は",
                    "遠くから",
                    "来た",
                    "友達と",
                    "一緒に、",
                    "美味しい",
                    "ラーメンを",
                    "食べに",
                    "行きました。",
                ],
            ),
            (
                "Hello世界、これはテストです。",
                &["Hello世界、", "これは", "テストです。"],
            ),
            (
                "「括弧」の中の文章（注釈つき）を確認する。",
                &["「括弧」の", "中の", "文章", "（注釈つき）を", "確認する。"],
            ),
            (
                "メッセージの折り返しを自然な文節位置で行います",
                &[
                    "メッセージの",
                    "折り返しを",
                    "自然な",
                    "文節位置で",
                    "行います",
                ],
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(split(text), expected, "{text}");
        }
    }

    #[test]
    fn text_without_japanese_is_not_analysed() {
        assert!(boundaries("hello world https://example.com/a").is_empty());
        assert!(joins("hello 😀 world").is_empty());
        assert!(boundaries("").is_empty());
        assert!(boundaries("あ").is_empty());
    }

    #[test]
    fn chinese_and_hangul_are_not_treated_as_japanese() {
        assert!(joins("这是一个中文消息测试。").is_empty());
        assert!(boundaries("这是一个中文消息测试。").is_empty());
        assert!(joins("東京都庁舎前").is_empty());
        assert!(joins("ﾡﾢﾣﾤ한국어").is_empty());
        // Kana makes the text Japanese; half-width Hangul still never joins.
        assert!(!joins("今日は天気です。").is_empty());
        assert!(!is_japanese('\u{FFA0}'));
    }

    #[test]
    fn joins_are_the_japanese_pairs_budoux_keeps_together() {
        // 今日は|天気です。 — the only phrase break is at byte 9; every other
        // gap between two characters is a join.
        let text = "今日は天気です。";
        assert_eq!(joins(text), [3, 6, 12, 15, 18, 21]);
    }

    #[test]
    fn ascii_next_to_japanese_is_not_joined() {
        // Only Japanese/Japanese gaps are reported.
        let text = "abc日本語def";
        for at in joins(text) {
            let before = text[..at as usize].chars().next_back().unwrap();
            let after = text[at as usize..].chars().next().unwrap();
            assert!(is_japanese(before) && is_japanese(after));
        }
    }

    #[test]
    fn offsets_are_utf8_bytes_with_mixed_scripts_and_emoji() {
        let text = "😀私は遠くから来た";
        for at in boundaries(text)
            .into_iter()
            .chain(joins(text).into_iter().map(|a| a as usize))
        {
            assert!(text.is_char_boundary(at));
        }
    }
}
