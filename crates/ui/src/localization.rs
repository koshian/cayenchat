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
        let mut text = self.text(key);
        for (name, value) in values {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }

    pub fn preference_label(&self, preference: Language) -> String {
        self.text(match preference {
            Language::System => "language_system",
            Language::Japanese => "language_japanese",
            Language::English => "language_english",
        })
    }
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
}
