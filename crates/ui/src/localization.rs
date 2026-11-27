// SPDX-License-Identifier: GPL-3.0-only
use cayenchat_storage::Language;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

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

    /// The reason of a failed link or send in this language. The core and
    /// upload crates give a fixed English text for what they classify; any
    /// other reason (a server's own wording) is returned as it is.
    pub fn reason(&self, reason: String) -> String {
        use cayenchat_irc_core as irc;
        use cayenchat_upload as upload;
        let key = match reason.as_str() {
            irc::REASON_LOST => "reason_lost",
            irc::REASON_CERTIFICATE => "reason_certificate",
            irc::REASON_TIMEOUT => "reason_timeout",
            irc::REASON_GENERIC => "reason_generic",
            irc::REASON_QUEUE_FULL => "reason_queue_full",
            upload::NETWORK_TIMEOUT => "upload_net_timeout",
            upload::NETWORK_HOST_NOT_FOUND => "upload_net_host_not_found",
            upload::NETWORK_CONNECTION_FAILED => "upload_net_connection_failed",
            upload::NETWORK_REPLY_TOO_LARGE => "upload_net_reply_too_large",
            upload::NETWORK_OTHER => "upload_net_other",
            _ => return reason,
        };
        self.text(key)
    }

    /// Fills `{nickname}` for display: the nickname comes from a server, so
    /// bidirectional controls are neutralized. Callers keep the raw value.
    pub fn format_nickname(&self, key: &str, nickname: &str) -> String {
        self.format(
            key,
            &[(
                "nickname",
                &cayenchat_model::display::neutralize_bidi(nickname),
            )],
        )
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

/// The English texts of the bundled catalog whose Japanese text is `shown`
/// (#623): the Japanese settings search also finds a setting by its English
/// name or description. It reads the bundled catalogs, so no second list of
/// names exists; a text that is not a bundled Japanese one (English mode, a
/// formatted text, a catalog edited on disk) has none.
pub fn english_of(shown: &str) -> &'static [String] {
    static REVERSE: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    let reverse = REVERSE.get_or_init(|| {
        let (english, japanese) = (parse(ENGLISH), parse(JAPANESE));
        let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
        for (key, text) in &japanese {
            let Some(en) = english.get(key).filter(|en| *en != text) else {
                continue;
            };
            let entry = reverse.entry(text.clone()).or_default();
            if !entry.contains(en) {
                entry.push(en.clone());
            }
        }
        reverse
    });
    reverse.get(shown).map_or(&[], Vec::as_slice)
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
    #[cfg(unix_desktop)]
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
    fn english_of_gives_the_english_text_of_a_japanese_one() {
        let japanese = Localizer::new(Language::Japanese);
        let english = Localizer::new(Language::English);
        let shown = japanese.text("ircv3_avatar_url");
        assert_eq!(english_of(&shown), [english.text("ircv3_avatar_url")]);
        // English text and an unknown text have nothing to add.
        assert!(english_of(&english.text("ircv3_avatar_url")).is_empty());
        assert!(english_of("no such text").is_empty());
    }

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
    fn classified_reasons_are_translated_and_other_reasons_kept() {
        for (catalog, expected) in [(JAPANESE, "サーバーとの接続が切れました。"), (ENGLISH, "")]
        {
            let strings = Arc::new(parse(catalog));
            let localizer = Localizer {
                strings: strings.clone(),
                bundled: strings.clone(),
                fallback: Arc::new(parse(ENGLISH)),
            };
            let lost = localizer.reason(cayenchat_irc_core::REASON_LOST.into());
            if expected.is_empty() {
                assert_eq!(lost, cayenchat_irc_core::REASON_LOST);
            } else {
                assert_eq!(lost, expected);
            }
            for reason in [
                cayenchat_irc_core::REASON_CERTIFICATE,
                cayenchat_irc_core::REASON_TIMEOUT,
                cayenchat_irc_core::REASON_GENERIC,
                cayenchat_irc_core::REASON_QUEUE_FULL,
                cayenchat_upload::NETWORK_TIMEOUT,
                cayenchat_upload::NETWORK_HOST_NOT_FOUND,
                cayenchat_upload::NETWORK_CONNECTION_FAILED,
                cayenchat_upload::NETWORK_REPLY_TOO_LARGE,
                cayenchat_upload::NETWORK_OTHER,
            ] {
                let shown = localizer.reason(reason.into());
                assert!(
                    !shown.is_empty() && !shown.starts_with("reason_"),
                    "{shown}"
                );
                assert!(!shown.starts_with("upload_net_"), "{shown}");
            }
            assert_eq!(
                localizer.reason("Nickname in use".into()),
                "Nickname in use"
            );
        }
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
