//! Desktop preferences that shape the client-side title bar.
//!
//! On Linux these come from the XDG desktop portal Settings interface, which
//! GNOME uses to publish its window-manager preferences to applications that
//! draw their own title bars. The light/dark color scheme is read by GPUI and
//! handled in `theme`. Other platforms keep the defaults and never draw a
//! client-side title bar.

use gpui::{App, Global};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonLayout {
    pub left: Vec<WindowButton>,
    pub right: Vec<WindowButton>,
}

impl Default for ButtonLayout {
    fn default() -> Self {
        Self::parse(":minimize,maximize,close")
    }
}

impl ButtonLayout {
    /// Parses the GNOME `button-layout` format, such as `appmenu:close`.
    /// Unknown entries (`appmenu`, `icon`, `spacer`) are ignored.
    pub fn parse(value: &str) -> Self {
        let side = |part: &str| {
            part.split(',')
                .filter_map(|name| match name.trim() {
                    "minimize" => Some(WindowButton::Minimize),
                    "maximize" => Some(WindowButton::Maximize),
                    "close" => Some(WindowButton::Close),
                    _ => None,
                })
                .collect()
        };
        let (left, right) = value.split_once(':').unwrap_or(("", value));
        Self {
            left: side(left),
            right: side(right),
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DoubleClick {
    #[default]
    ToggleMaximize,
    Minimize,
    Menu,
    None,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl DoubleClick {
    pub fn parse(value: &str) -> Self {
        match value {
            "minimize" => Self::Minimize,
            "menu" => Self::Menu,
            "none" | "lower" => Self::None,
            _ => Self::ToggleMaximize,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopSettings {
    pub buttons: ButtonLayout,
    pub double_click: DoubleClick,
    /// Family from the desktop interface font, for the title text.
    pub title_font: Option<String>,
    /// Whether the GTK key theme is `Emacs`, for draft editing keys.
    pub emacs_keys: bool,
}

impl Global for DesktopSettings {}

/// Extracts the family from a Pango font description such as
/// `Adwaita Sans 11` or `Cantarell Bold 11`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn font_family(description: &str) -> Option<String> {
    const STYLES: [&str; 12] = [
        "bold",
        "semi-bold",
        "semibold",
        "medium",
        "light",
        "regular",
        "italic",
        "oblique",
        "heavy",
        "book",
        "thin",
        "condensed",
    ];
    let mut words: Vec<&str> = description.split_whitespace().collect();
    if words.last().is_some_and(|word| word.parse::<f32>().is_ok()) {
        words.pop();
    }
    while words
        .last()
        .is_some_and(|word| STYLES.contains(&word.to_ascii_lowercase().as_str()))
    {
        words.pop();
    }
    (!words.is_empty()).then(|| words.join(" "))
}

/// Reads `gtk-key-theme-name` from GTK 3's `settings.ini`, the fallback for
/// desktops whose portal does not publish `gtk-key-theme`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn settings_ini_key_theme(contents: &str) -> Option<String> {
    let mut in_settings = false;
    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') {
            in_settings = line == "[Settings]";
        } else if in_settings
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "gtk-key-theme-name"
        {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn settings_ini_emacs() -> bool {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".config"))
        });
    config
        .and_then(|config| std::fs::read_to_string(config.join("gtk-3.0/settings.ini")).ok())
        .and_then(|contents| settings_ini_key_theme(&contents))
        .is_some_and(|theme| theme == "Emacs")
}

pub fn current(cx: &App) -> DesktopSettings {
    cx.try_global::<DesktopSettings>()
        .cloned()
        .unwrap_or_default()
}

/// Loads the portal settings and follows later changes.
pub fn watch(cx: &mut App) {
    cx.set_global(DesktopSettings {
        #[cfg(target_os = "linux")]
        emacs_keys: settings_ini_emacs(),
        ..Default::default()
    });
    #[cfg(target_os = "linux")]
    portal::watch(cx);
}

#[cfg(target_os = "linux")]
mod portal {
    use super::{ButtonLayout, DesktopSettings, DoubleClick, font_family, settings_ini_emacs};
    use ashpd::desktop::settings::Settings;
    use futures_util::StreamExt;
    use gpui::App;

    const WM: &str = "org.gnome.desktop.wm.preferences";
    const INTERFACE: &str = "org.gnome.desktop.interface";

    async fn read(settings: &Settings<'_>) -> DesktopSettings {
        let string =
            async |namespace: &str, key: &str| settings.read::<String>(namespace, key).await.ok();
        DesktopSettings {
            buttons: string(WM, "button-layout")
                .await
                .map(|value| ButtonLayout::parse(&value))
                .unwrap_or_default(),
            double_click: string(WM, "action-double-click-titlebar")
                .await
                .map(|value| DoubleClick::parse(&value))
                .unwrap_or_default(),
            title_font: string(INTERFACE, "font-name")
                .await
                .and_then(|value| font_family(&value)),
            emacs_keys: string(INTERFACE, "gtk-key-theme")
                .await
                .map_or_else(settings_ini_emacs, |theme| theme == "Emacs"),
        }
    }

    pub fn watch(cx: &mut App) {
        cx.spawn(async move |cx| {
            // Without a portal (or outside a desktop session) the defaults stay.
            let Ok(settings) = Settings::new().await else {
                return;
            };
            let apply = |values: DesktopSettings, cx: &mut gpui::AsyncApp| {
                cx.update(|cx| {
                    let previous = cx.try_global::<DesktopSettings>();
                    if previous != Some(&values) {
                        let keys_changed =
                            previous.map(|p| p.emacs_keys) != Some(values.emacs_keys);
                        cx.set_global(values);
                        if keys_changed {
                            crate::rebind_shortcuts(cx);
                        }
                        cx.refresh_windows();
                    }
                })
                .is_ok()
            };
            if !apply(read(&settings).await, cx) {
                return;
            }
            let Ok(mut changes) = settings.receive_setting_changed().await else {
                return;
            };
            while let Some(change) = changes.next().await {
                let relevant = match change.namespace() {
                    WM => matches!(
                        change.key(),
                        "button-layout" | "action-double-click-titlebar"
                    ),
                    INTERFACE => matches!(change.key(), "font-name" | "gtk-key-theme"),
                    _ => false,
                };
                if relevant && !apply(read(&settings).await, cx) {
                    return;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gnome_button_layouts_and_font_descriptions() {
        assert_eq!(
            ButtonLayout::parse("appmenu:close"),
            ButtonLayout {
                left: vec![],
                right: vec![WindowButton::Close],
            }
        );
        assert_eq!(
            ButtonLayout::parse("close,minimize,maximize:icon"),
            ButtonLayout {
                left: vec![
                    WindowButton::Close,
                    WindowButton::Minimize,
                    WindowButton::Maximize
                ],
                right: vec![],
            }
        );
        assert_eq!(
            ButtonLayout::default().right,
            [
                WindowButton::Minimize,
                WindowButton::Maximize,
                WindowButton::Close
            ]
        );
        assert_eq!(
            font_family("Adwaita Sans 11").as_deref(),
            Some("Adwaita Sans")
        );
        assert_eq!(
            font_family("Cantarell Bold 11").as_deref(),
            Some("Cantarell")
        );
        assert_eq!(font_family("11"), None);
        assert_eq!(
            settings_ini_key_theme(
                "[Settings]\ngtk-theme-name=Adwaita\ngtk-key-theme-name = Emacs\n"
            )
            .as_deref(),
            Some("Emacs")
        );
        assert_eq!(
            settings_ini_key_theme("[Other]\ngtk-key-theme-name=Emacs\n"),
            None
        );
        assert_eq!(DoubleClick::parse("minimize"), DoubleClick::Minimize);
        assert_eq!(
            DoubleClick::parse("toggle-maximize"),
            DoubleClick::ToggleMaximize
        );
    }
}
