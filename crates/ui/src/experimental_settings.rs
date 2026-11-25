// SPDX-License-Identifier: GPL-3.0-only
//! Application-wide experimental diagnostics, independent of IRC wire traces.
//! Drawn as Kit groups (#622); the debug log rows are built here because the
//! switch may first ask for a file and the file row holds a chooser.

use gpui::{prelude::*, *};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

use crate::{
    SettingsTab, SettingsWindow, diagnostics, settings_items,
    settings_kit::{element_item, field_item, footer_item, group, page},
    settings_theme,
};

impl SettingsWindow {
    fn choose_stderr_file(&mut self, enable: bool, cx: &mut Context<Self>) {
        let current = self.settings.values.experimental.stderr_file.as_deref();
        let directory = current
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(std::env::temp_dir);
        let name = current
            .and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("cayenchat-debug.log");
        let chosen = cx.prompt_for_new_path(&directory, Some(name));
        cx.spawn(async move |this, cx| {
            let result = chosen.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(path))) => {
                        this.settings.values.experimental.stderr_file = Some(path);
                        if enable {
                            this.settings.values.experimental.debug_logging = true;
                        }
                        this.feedback = None;
                        this.autosave_now(None, cx);
                    }
                    Ok(Ok(None)) => return,
                    Ok(Err(error)) => {
                        this.feedback = Some(
                            this.i18n
                                .format("debug_log_error", &[("error", &error.to_string())]),
                        );
                    }
                    Err(_) => {
                        this.feedback = Some(this.i18n.text("debug_log_picker_failed"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The debug log switch. Turning it on with no file chosen asks for one
    /// first, and the log is on only once one is chosen.
    fn toggle_debug_logging(&mut self, cx: &mut Context<Self>) {
        let settings = &mut self.settings.values.experimental;
        if !settings.debug_logging && settings.stderr_file.is_none() {
            self.choose_stderr_file(true, cx);
            return;
        }
        settings.debug_logging = !settings.debug_logging;
        self.feedback = None;
        self.autosave_now(None, cx);
        cx.notify();
    }

    /// The Experimental tab: the automatic phrase switch, then the debug log.
    pub(crate) fn experimental_page(
        &mut self,
        title: String,
        this: &WeakEntity<Self>,
        cx: &App,
    ) -> SettingPage {
        let i18n = &self.i18n;
        let settings = &self.settings.values.experimental;
        let enabled = settings.debug_logging;
        let path = settings
            .stderr_file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| i18n.text("debug_log_no_file"));
        let active = diagnostics::configuration();
        let status = match active.stderr_file {
            Some(path) => i18n.format("debug_log_active", &[("path", &path.display().to_string())]),
            None => i18n.text("debug_log_inactive"),
        };
        let toggle_target = this.clone();
        let switch = SettingItem::new(
            i18n.text("debug_logging"),
            SettingField::switch(
                move |_| enabled,
                move |_, cx| {
                    let _ = toggle_target.update(cx, |this, cx| this.toggle_debug_logging(cx));
                },
            ),
        )
        .description(i18n.text("debug_logging_hint"));
        let file = field_item(
            this,
            i18n.text("debug_log_file"),
            i18n.text("debug_log_file_hint"),
            move |this, _, cx| {
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(div().id("debug-log-path").child(path.clone()))
                    .child(
                        settings_theme::button("choose-stderr-file", false, cx)
                            .debug_selector(|| "choose-stderr-file".into())
                            .child(this.i18n.text("debug_log_choose"))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.choose_stderr_file(false, cx)),
                            ),
                    )
                    .into_any_element()
            },
        );
        let status = element_item(this, move |_, _, cx| {
            div()
                .text_color(settings_theme::palette(cx).text_secondary)
                .child(status.clone())
                .into_any_element()
        });
        page(
            title,
            [
                group(
                    i18n.text("group_exp_display"),
                    None,
                    self.kit_table(settings_items::EXPERIMENTAL, this),
                ),
                group(
                    i18n.text("group_exp_debug_log"),
                    Some(i18n.text("debug_log_hint")),
                    vec![switch, file, status],
                ),
                SettingGroup::new().item(footer_item(this, SettingsTab::Experimental)),
            ],
            cx,
        )
    }
}
