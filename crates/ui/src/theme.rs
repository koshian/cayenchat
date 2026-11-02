// SPDX-License-Identifier: GPL-3.0-only
//! Light and dark color themes.
//!
//! The effective theme follows the saved preference: the system appearance
//! (macOS/Windows appearance, or the XDG desktop portal `color-scheme` on
//! Linux, all reported by GPUI) or an explicit light or dark choice. It lives
//! in a GPUI global so every window reads the same colors.

use cayenchat_storage::{Appearance, ThemeMode, color_value};
use gpui::{App, Global, Hsla, Rgba, WindowAppearance, hsla, rgb, rgba};

/// Log pane colors chosen in the Appearance settings for the active theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneColors {
    pub member_list: Rgba,
    pub channel_log: Rgba,
    pub channel_alternate: Rgba,
    pub channel_event: Rgba,
    pub notice: Rgba,
    pub highlight: Rgba,
    pub combined_log: Rgba,
    pub combined_alternate: Rgba,
    /// Background of the hover popups (full URL, character reference
    /// original), with its opacity as alpha; their text stays opaque.
    pub url_tooltip: Rgba,
    /// Text and underline of URLs in the channel logs only; `Theme::link`
    /// stays for the rest of the UI.
    pub log_link: Rgba,
    /// Background behind URLs in the channel logs; `None` is automatic.
    pub log_link_background: Option<Rgba>,
    /// Link text color in the hover popups only.
    pub url_tooltip_link: Rgba,
    /// Plain text color in the hover popups.
    pub url_tooltip_text: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub text: Rgba,
    /// Secondary text such as descriptions and diagnostics.
    pub text_secondary: Rgba,
    /// Disabled items and unjoined channels.
    pub text_muted: Rgba,
    pub placeholder: Hsla,
    pub time: Rgba,
    pub nickname: Rgba,
    pub link: Rgba,
    pub warning: Rgba,
    pub border: Rgba,
    pub separator: Rgba,
    /// Settings and WHOIS window background.
    pub window: Rgba,
    /// Inputs, menus, buttons and cards.
    pub surface: Rgba,
    pub tab_inactive: Rgba,
    pub channel_tree: Rgba,
    pub selected: Rgba,
    /// Hover over selectable rows in the channel tree and menus.
    pub hover_strong: Rgba,
    /// Hover over log and list rows.
    pub hover: Rgba,
    pub text_selection: Rgba,
    pub panes: PaneColors,
}

impl Global for Theme {}

impl Theme {
    pub fn new(mode: ThemeMode, system: WindowAppearance, appearance: &Appearance) -> Self {
        let dark = match mode {
            ThemeMode::System => matches!(
                system,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
        };
        if dark {
            Self::dark(appearance)
        } else {
            Self::light(appearance)
        }
    }

    fn light(appearance: &Appearance) -> Self {
        Self {
            dark: false,
            text: rgb(0x20262d),
            text_secondary: rgb(0x52606c),
            text_muted: rgb(0x8a9097),
            placeholder: hsla(0., 0., 0., 0.2),
            time: rgb(0x747b82),
            nickname: rgb(0x315b83),
            link: rgb(0x0645ad),
            warning: rgb(0x9a4b28),
            border: rgb(0xb7bdc4),
            separator: rgb(0xd8dde3),
            window: rgb(0xf5f6f8),
            surface: rgb(0xffffff),
            tab_inactive: rgb(0xe8ebef),
            channel_tree: rgb(0xeaf3ff),
            selected: rgb(0xcbdbea),
            hover_strong: rgb(0xdce5ee),
            hover: rgb(0xe8eff6),
            text_selection: rgba(0x3311ff30),
            panes: PaneColors {
                member_list: color(&appearance.member_list_background, 0xffffff),
                channel_log: color(&appearance.channel_log_background, 0xffffff),
                channel_alternate: color(&appearance.channel_log_alternate, 0xf2f5ff),
                channel_event: color(&appearance.channel_event_color, 0x007d00),
                notice: color(&appearance.notice_color, 0x7a838c),
                highlight: color(&appearance.highlight_color, 0xd46a8e),
                combined_log: color(&appearance.combined_log_background, 0xf9fafb),
                combined_alternate: color(&appearance.combined_log_alternate, 0xf2f5ff),
                url_tooltip: translucent(
                    color(&appearance.url_tooltip_color, 0x1b1d20),
                    appearance.url_tooltip_opacity,
                ),
                log_link: color(&appearance.link_color, 0x0645ad),
                log_link_background: color_value(&appearance.link_background).map(rgb),
                url_tooltip_link: color(&appearance.url_tooltip_link_color, 0x78aaff),
                url_tooltip_text: color(&appearance.url_tooltip_text_color, 0xe3e6ea),
            },
        }
    }

    fn dark(appearance: &Appearance) -> Self {
        let colors = &appearance.dark;
        Self {
            dark: true,
            text: rgb(0xe3e6ea),
            text_secondary: rgb(0xa3acb6),
            text_muted: rgb(0x767d85),
            placeholder: hsla(0., 0., 1., 0.25),
            time: rgb(0x8c949c),
            nickname: rgb(0x86b4e3),
            link: rgb(0x78aaff),
            warning: rgb(0xe39a66),
            border: rgb(0x3d434b),
            separator: rgb(0x353a41),
            window: rgb(0x202225),
            surface: rgb(0x2a2d32),
            tab_inactive: rgb(0x25282c),
            channel_tree: rgb(0x1d2630),
            selected: rgb(0x35506c),
            hover_strong: rgb(0x2d3b4a),
            hover: rgb(0x2c3238),
            text_selection: rgba(0x5a8dee55),
            panes: PaneColors {
                member_list: color(&colors.member_list_background, 0x1f2124),
                channel_log: color(&colors.channel_log_background, 0x1f2124),
                channel_alternate: color(&colors.channel_log_alternate, 0x272b31),
                channel_event: color(&colors.channel_event_color, 0x6cc46c),
                notice: color(&colors.notice_color, 0x8c949c),
                highlight: color(&colors.highlight_color, 0xefa0be),
                combined_log: color(&colors.combined_log_background, 0x24272b),
                combined_alternate: color(&colors.combined_log_alternate, 0x2c3036),
                url_tooltip: translucent(
                    color(&colors.url_tooltip_color, 0x1b1d20),
                    appearance.url_tooltip_opacity,
                ),
                log_link: color(&colors.link_color, 0x78aaff),
                log_link_background: color_value(&colors.link_background).map(rgb),
                url_tooltip_link: color(&colors.url_tooltip_link_color, 0x78aaff),
                url_tooltip_text: color(&colors.url_tooltip_text_color, 0xe3e6ea),
            },
        }
    }
}

fn color(value: &str, fallback: u32) -> Rgba {
    rgb(color_value(value).unwrap_or(fallback))
}

/// `color` at `percent` opacity.
fn translucent(color: Rgba, percent: u8) -> Rgba {
    Rgba {
        a: f32::from(percent.min(100)) / 100.,
        ..color
    }
}

pub fn current(cx: &App) -> Theme {
    *cx.global::<Theme>()
}

/// Recomputes the theme and redraws every window when it changes.
pub fn apply(mode: ThemeMode, appearance: &Appearance, cx: &mut App) {
    let theme = Theme::new(mode, cx.window_appearance(), appearance);
    if cx.try_global::<Theme>() != Some(&theme) {
        cx.set_global(theme);
        // Re-read native settings colors on light/dark changes, after the
        // app palette is installed so explicit mode overrides stay effective.
        if crate::settings_theme::current(cx).is_some() {
            crate::settings_theme::refresh(cx);
        }
        cx.refresh_windows();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_selects_light_or_dark_and_uses_each_themes_pane_colors() {
        let appearance = Appearance {
            channel_log_background: "#FAFAFA".into(),
            dark: cayenchat_storage::DarkColors {
                channel_log_background: "#101010".into(),
                ..Default::default()
            },
            ..Default::default()
        };

        let light = Theme::new(ThemeMode::System, WindowAppearance::Light, &appearance);
        assert!(!light.dark);
        assert_eq!(light.panes.channel_log, rgb(0xfafafa));

        let dark = Theme::new(
            ThemeMode::System,
            WindowAppearance::VibrantDark,
            &appearance,
        );
        assert!(dark.dark);
        assert_eq!(dark.panes.channel_log, rgb(0x101010));

        assert!(Theme::new(ThemeMode::Dark, WindowAppearance::Light, &appearance).dark);
        assert!(!Theme::new(ThemeMode::Light, WindowAppearance::Dark, &appearance).dark);
    }

    #[test]
    fn url_tooltip_link_color_is_per_theme_and_leaves_the_log_link_alone() {
        let defaults = Appearance::default();
        let light = Theme::new(ThemeMode::Light, WindowAppearance::Light, &defaults);
        let dark = Theme::new(ThemeMode::Dark, WindowAppearance::Dark, &defaults);
        assert_eq!(light.panes.url_tooltip_link, rgb(0x78aaff));
        assert_eq!(dark.panes.url_tooltip_link, rgb(0x78aaff));

        let appearance = Appearance {
            url_tooltip_link_color: "#FFEE00".into(),
            dark: cayenchat_storage::DarkColors {
                url_tooltip_link_color: "#102030".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let light = Theme::new(ThemeMode::Light, WindowAppearance::Light, &appearance);
        let dark = Theme::new(ThemeMode::Dark, WindowAppearance::Dark, &appearance);
        assert_eq!(light.panes.url_tooltip_link, rgb(0xffee00));
        assert_eq!(dark.panes.url_tooltip_link, rgb(0x102030));
        assert_eq!(light.link, rgb(0x0645ad));
        assert_eq!(dark.link, rgb(0x78aaff));
    }
}
