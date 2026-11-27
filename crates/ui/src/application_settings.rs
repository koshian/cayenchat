// SPDX-License-Identifier: GPL-3.0-only
//! The Application page: how the app itself behaves, apart from any server.
//! Drawn as Kit groups (#622); the rows the Kit has no part for (the
//! language, which also retitles the window, the login startup, which the
//! system answers, and the buttons) are built here.

use cayenchat_storage::Language;
use gpui::{prelude::*, *};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

use crate::{
    ExportSettings, ImportSettings, SettingsTab, SettingsWindow, autostart, settings_items,
    settings_kit::{EnglishSearch as _, element_item, field_item, group, page},
    settings_theme,
};

/// The languages in the order of the dropdown; its values are the indexes.
const LANGUAGES: [Language; 3] = [Language::System, Language::Japanese, Language::English];

impl SettingsWindow {
    /// The Application tab: language and startup, the window, behavior, text
    /// and links, the conversation logs and the settings file.
    pub(crate) fn application_page(
        &mut self,
        title: String,
        this: &WeakEntity<Self>,
        cx: &mut Context<Self>,
    ) -> SettingPage {
        let log_status = self.log_status_text(cx);
        let directory = self.log_directory_text();
        let log_hint = self.i18n.text("conversation_log_hint");
        let mut log = self.kit_table(settings_items::APPLICATION_LOG, this);
        log.push(field_item(
            this,
            self.i18n.text("conversation_log_directory"),
            directory,
            |this, _, cx| this.log_directory_buttons(cx).into_any_element(),
        ));
        if let Some(status) = log_status {
            log.push(element_item(this, move |_, _, cx| {
                div()
                    .id("conversation-log-status")
                    .text_color(settings_theme::palette(cx).warning)
                    .child(status.clone())
                    .into_any_element()
            }));
        }
        let i18n = &self.i18n;
        page(
            title,
            [
                group(
                    i18n.text("group_app_startup"),
                    Some(i18n.text("application_intro")),
                    vec![self.language_item(this), self.autostart_item(this)],
                ),
                group(
                    i18n.text("group_app_window"),
                    None,
                    self.kit_table(settings_items::APPLICATION_WINDOW, this),
                ),
                group(
                    i18n.text("group_app_behavior"),
                    None,
                    self.kit_table(settings_items::APPLICATION_BEHAVIOR, this),
                ),
                group(
                    i18n.text("group_app_text"),
                    None,
                    self.kit_table(settings_items::APPLICATION_TEXT, this),
                ),
                group(i18n.text("conversation_log"), Some(log_hint), log),
                group(
                    i18n.text("transfer_item"),
                    None,
                    vec![field_item(
                        this,
                        i18n.text("transfer_heading"),
                        i18n.text("transfer_hint"),
                        |this, _, cx| this.settings_transfer_buttons(cx).into_any_element(),
                    )],
                ),
                SettingGroup::new().item(crate::settings_kit::footer_item(
                    this,
                    SettingsTab::Application,
                )),
            ],
            cx,
        )
    }

    /// The language dropdown. A change also retitles the window and the
    /// fields, which need the window, so it is made once the Kit's setter
    /// has returned.
    fn language_item(&self, this: &WeakEntity<Self>) -> SettingItem {
        let labels = LANGUAGES
            .iter()
            .enumerate()
            .map(|(index, language)| {
                (
                    index.to_string().into(),
                    self.i18n.preference_label(*language).into(),
                )
            })
            .collect();
        let selected = LANGUAGES
            .iter()
            .position(|language| *language == self.settings.values.language)
            .unwrap_or(0)
            .to_string();
        let this = this.clone();
        SettingItem::new(
            self.i18n.text("language"),
            SettingField::dropdown(
                labels,
                move |_| selected.clone().into(),
                move |value, cx| {
                    let Some(language) = value
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| LANGUAGES.get(index).copied())
                    else {
                        return;
                    };
                    let Ok(window) = this.read_with(cx, |this, _| this.window) else {
                        return;
                    };
                    let this = this.clone();
                    cx.defer(move |cx| {
                        let _ = window.update(cx, |_, window, cx| {
                            let _ = this
                                .update(cx, |this, cx| this.select_language(language, window, cx));
                        });
                    });
                },
            ),
        )
        .english(&[
            &self.i18n.text("language"),
            &self.i18n.text("language_hint"),
        ])
        .description(self.i18n.text("language_hint"))
    }

    /// The login startup switch. The system answers it, so the switch shows
    /// what the system reports and is off limits while that is read or
    /// changed.
    fn autostart_item(&self, this: &WeakEntity<Self>) -> SettingItem {
        let status = self
            .autostart
            .as_ref()
            .and_then(|status| status.as_ref().ok())
            .copied();
        let usable = !self.autostart_busy
            && status.is_some_and(|status| status != autostart::AutostartStatus::Unavailable);
        let note = match &self.autostart {
            Some(Ok(autostart::AutostartStatus::DisabledByUser)) => {
                Some(if autostart::reenable_in_system_settings() {
                    "autostart_disabled_in_system"
                } else {
                    "autostart_disabled_by_user"
                })
            }
            Some(Ok(autostart::AutostartStatus::Unavailable)) => Some("autostart_unavailable"),
            _ => None,
        };
        let mut description = self.i18n.text("autostart_hint");
        if let Some(note) = note {
            description = format!("{description} {}", self.i18n.text(note));
        }
        if let Some(Err(error)) = &self.autostart {
            description = format!(
                "{description} {}",
                self.i18n.format("autostart_error", &[("error", error)])
            );
        }
        let checked = status.is_some_and(autostart::AutostartStatus::is_on);
        let this = this.clone();
        SettingItem::new(
            self.i18n.text("autostart"),
            SettingField::switch(
                move |_| checked,
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.toggle_autostart(cx));
                },
            ),
        )
        .english(&[
            &self.i18n.text("autostart"),
            &self.i18n.text("autostart_hint"),
        ])
        .description(description)
        .disabled(!usable)
    }

    /// Export and import of the settings file (#520); the questions are
    /// drawn in the chat window, which the buttons ask.
    fn settings_transfer_buttons(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                settings_theme::button("export-settings", false, cx)
                    .child(self.i18n.text("menu_export_settings"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let _ = this.owner.update(cx, |owner, window, cx| {
                            owner.export_settings(&ExportSettings, window, cx)
                        });
                    })),
            )
            .child(
                settings_theme::button("import-settings", false, cx)
                    .child(self.i18n.text("menu_import_settings"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let _ = this.owner.update(cx, |owner, window, cx| {
                            owner.import_settings(&ImportSettings, window, cx)
                        });
                    })),
            )
    }
}
