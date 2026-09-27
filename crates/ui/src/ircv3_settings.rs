//! Settings tab for opt-in IRCv3 features, chosen per server.
//!
//! This tab is the current home of IRCv3 preferences that need an explicit
//! opt-in. Each feature is one [`Ircv3Feature`] row reading and writing its
//! own field of [`Ircv3Preferences`]; moving a feature to another tab, or
//! enabling it by default, changes only its row or storage default and never
//! the protocol code.

use cayenchat_storage::{Ircv3Preferences, TextEncoding};
use gpui::{prelude::*, *};

use crate::{SettingsWindow, account_settings::panel, settings_theme};

/// One opt-in preference as the settings window shows it.
pub(crate) struct Ircv3Feature {
    pub id: &'static str,
    pub label_key: &'static str,
    pub hint_key: &'static str,
    pub get: fn(&Ircv3Preferences) -> bool,
    pub toggle: fn(&mut Ircv3Preferences),
}

/// Features shown on the IRCv3 tab, in display order.
pub(crate) const IRCV3_FEATURES: [Ircv3Feature; 3] = [
    Ircv3Feature {
        id: "ircv3-server-time",
        label_key: "ircv3_server_time",
        hint_key: "ircv3_server_time_hint",
        get: |preferences| preferences.server_time,
        toggle: |preferences| preferences.server_time = !preferences.server_time,
    },
    Ircv3Feature {
        id: "ircv3-message-tags",
        label_key: "ircv3_message_tags",
        hint_key: "ircv3_message_tags_hint",
        get: |preferences| preferences.message_tags,
        toggle: |preferences| preferences.message_tags = !preferences.message_tags,
    },
    Ircv3Feature {
        id: "ircv3-batch",
        label_key: "ircv3_batch",
        hint_key: "ircv3_batch_hint",
        get: |preferences| preferences.batch,
        toggle: |preferences| preferences.batch = !preferences.batch,
    },
];

impl SettingsWindow {
    pub(crate) fn render_ircv3_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let hint = |key: &str| {
            div()
                .ml(px(26.))
                .text_color(theme.text_secondary)
                .child(self.i18n.text(key))
        };
        let mut panel = panel(cx)
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("ircv3_tab")),
            )
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("ircv3_intro")),
            );
        let Some(profile) = self.settings.values.selected_profile().cloned() else {
            return panel.child(div().child(self.i18n.text("ircv3_no_server")));
        };

        // Which server is being configured, and a way to pick another.
        let mut servers = div().flex().flex_wrap().gap_1();
        for (index, server) in self.settings.values.ordered_servers().enumerate() {
            let id = server.id.clone();
            let selected = id == profile.id;
            servers = servers.child(
                div()
                    .id(("ircv3-server", index))
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.bg(theme.selected).font_weight(FontWeight::BOLD)
                    })
                    .when(!selected, |d| d.hover(|d| d.bg(theme.hover)))
                    .child(server_label(&server.host, server.port, &self.i18n))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_server(id.clone(), cx)),
                    ),
            );
        }
        panel = panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("ircv3_server")),
                    )
                    .child(servers),
            )
            .child(div().font_weight(FontWeight::BOLD).child(self.i18n.format(
                "ircv3_configuring",
                &[(
                    "server",
                    &server_label(&profile.host, profile.port, &self.i18n),
                )],
            )));

        for feature in &IRCV3_FEATURES {
            let toggle = feature.toggle;
            panel = panel
                .child(
                    div()
                        .id(feature.id)
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .child(settings_theme::checkbox(
                            (feature.get)(&profile.ircv3),
                            true,
                            cx,
                        ))
                        .child(self.i18n.text(feature.label_key))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(profile) = this.settings.values.selected_profile_mut() {
                                toggle(&mut profile.ircv3);
                                cx.notify();
                            }
                        })),
                )
                .child(hint(feature.hint_key));
        }
        if profile.encoding != TextEncoding::Utf8 {
            panel = panel.child(div().text_color(theme.warning).child(self.i18n.format(
                "ircv3_legacy_encoding",
                &[("encoding", profile.encoding.label())],
            )));
        }
        panel
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("ircv3_next_connection")),
            )
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
    }
}

fn server_label(host: &str, port: u16, i18n: &crate::localization::Localizer) -> String {
    if host.is_empty() {
        i18n.text("new_server")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::IRCV3_FEATURES;
    use cayenchat_storage::Ircv3Preferences;

    #[test]
    fn each_feature_row_toggles_only_its_own_preference() {
        let catalogs = [
            include_str!("../../../locales/en.json"),
            include_str!("../../../locales/ja.json"),
        ];
        for (index, feature) in IRCV3_FEATURES.iter().enumerate() {
            let mut preferences = Ircv3Preferences::default();
            assert!(!(feature.get)(&preferences), "off by default");
            (feature.toggle)(&mut preferences);
            assert!((feature.get)(&preferences));
            for (other_index, other) in IRCV3_FEATURES.iter().enumerate() {
                if other_index != index {
                    assert!(!(other.get)(&preferences), "{} is independent", other.id);
                }
            }
            for catalog in catalogs {
                for key in [feature.label_key, feature.hint_key] {
                    assert!(catalog.contains(&format!("\"{key}\"")), "{key}");
                }
            }
        }
    }
}
