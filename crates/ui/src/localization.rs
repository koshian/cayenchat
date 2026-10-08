use cayenchat_storage::Language;
use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

const ENGLISH: &str = include_str!("../../../locales/en.json");
const JAPANESE: &str = include_str!("../../../locales/ja.json");

#[derive(Clone)]
pub struct Localizer {
    strings: Arc<HashMap<String, String>>,
    bundled: Arc<HashMap<String, String>>,
    fallback: Arc<HashMap<String, String>>,
}

impl Localizer {
    pub fn new(preference: Language) -> Self {
        let language = match preference {
            Language::System => language_for_tag(sys_locale::get_locale().as_deref()),
            explicit => explicit,
        };
        let fallback = Arc::new(parse(ENGLISH));
        let bundled = if language == Language::English {
            fallback.clone()
        } else {
            Arc::new(parse(JAPANESE))
        };
        let filename = if language == Language::Japanese {
            "ja"
        } else {
            "en"
        };
        let strings = Arc::new(read_catalog(filename).unwrap_or_else(|| (*bundled).clone()));
        Self {
            strings,
            bundled,
            fallback,
        }
    }

    pub fn text(&self, key: &str) -> String {
        self.strings
            .get(key)
            .or_else(|| self.bundled.get(key))
            .or_else(|| self.fallback.get(key))
            .cloned()
            .unwrap_or_else(|| key.to_owned())
    }

    pub fn format(&self, key: &str, values: &[(&str, &str)]) -> String {
        fill(&self.text(key), values)
    }

    pub fn preference_label(&self, preference: Language) -> String {
        self.text(match preference {
            Language::System => "language_system",
            Language::Japanese => "language_japanese",
            Language::English => "language_english",
        })
    }
}

/// Replaces each `{name}` in `template` with its value in one pass over the
/// template. Values often come from servers or other users, so a value that
/// itself contains `{name}` is inserted as it is, never expanded. Unknown
/// placeholders stay as written.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut text = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after.find('}').and_then(|close| {
            let name = &after[..close];
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value, close))
        });
        match value {
            Some((value, close)) => {
                text.push_str(value);
                rest = &after[close + 1..];
            }
            None => {
                text.push('{');
                rest = after;
            }
        }
    }
    text.push_str(rest);
    text
}

fn language_for_tag(tag: Option<&str>) -> Language {
    if tag.is_some_and(|tag| tag.to_ascii_lowercase().starts_with("ja")) {
        Language::Japanese
    } else {
        Language::English
    }
}

fn parse(source: &str) -> HashMap<String, String> {
    serde_json::from_str(source).expect("bundled locale catalog must be valid JSON")
}

fn read_catalog(language: &str) -> Option<HashMap<String, String>> {
    let filename = format!("{language}.json");
    let mut paths = Vec::<PathBuf>::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        paths.push(directory.join("locales").join(&filename));
        paths.push(directory.join("../Resources/locales").join(&filename));
        #[cfg(debug_assertions)]
        paths.push(directory.join("../../locales").join(&filename));
    }
    #[cfg(target_os = "linux")]
    paths.push(PathBuf::from("/usr/share/cayenchat/locales").join(&filename));
    paths.into_iter().find_map(|path| {
        fs::read_to_string(path)
            .ok()
            .and_then(|source| serde_json::from_str(&source).ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_have_matching_keys_and_locale_detection_is_specific() {
        let english = parse(ENGLISH);
        let japanese = parse(JAPANESE);
        assert_eq!(
            english.keys().collect::<std::collections::HashSet<_>>(),
            japanese.keys().collect()
        );
        assert_eq!(language_for_tag(Some("ja_JP.UTF-8")), Language::Japanese);
        assert_eq!(language_for_tag(Some("ja-JP")), Language::Japanese);
        assert_eq!(language_for_tag(Some("en-US")), Language::English);
        assert_eq!(language_for_tag(None), Language::English);
    }

    #[test]
    fn values_are_inserted_once_and_never_expanded() {
        let quit = "{nick} quit ({reason})";
        assert_eq!(
            fill(quit, &[("nick", "{reason}"), ("reason", "bye")]),
            "{reason} quit (bye)"
        );
        assert_eq!(
            fill(quit, &[("reason", "{nick}"), ("nick", "bob")]),
            "bob quit ({nick})"
        );
        // Unknown placeholders and lone braces stay as written.
        assert_eq!(fill("{a} {b} { x}", &[("a", "1")]), "1 {b} { x}");
        assert_eq!(fill("{a", &[("a", "1")]), "{a");
        assert_eq!(fill("日本{a}語", &[("a", "🙂")]), "日本🙂語");
    }
}
