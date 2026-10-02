//! Application-wide experimental diagnostics, independent of IRC wire traces.

use gpui::{prelude::*, *};

use crate::{SettingsTab, SettingsWindow, account_settings, diagnostics, settings_theme};

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

    pub(crate) fn render_experimental_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let settings = &self.settings.values.experimental;
        let enabled = settings.debug_logging;
        let path = settings
            .stderr_file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| self.i18n.text("debug_log_no_file"));
        let active = diagnostics::configuration();
        let status = match active.stderr_file {
            Some(path) => self
                .i18n
                .format("debug_log_active", &[("path", &path.display().to_string())]),
            None => self.i18n.text("debug_log_inactive"),
        };
        account_settings::panel(cx)
            .child(self.tab_heading(SettingsTab::Experimental, "experimental_tab", cx))
            .child(
                div()
                    .id("debug-logging-toggle")
                    .debug_selector(|| "debug-logging-toggle".into())
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(enabled, true, cx))
                    .child(self.i18n.text("debug_logging"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let settings = &mut this.settings.values.experimental;
                        if !settings.debug_logging && settings.stderr_file.is_none() {
                            this.choose_stderr_file(true, cx);
                            return;
                        }
                        settings.debug_logging = !settings.debug_logging;
                        this.feedback = None;
                        this.autosave_now(None, cx);
                        cx.notify();
                    })),
            )
            .child(self.i18n.text("debug_log_file"))
            .child(div().id("debug-log-path").child(path))
            .child(
                settings_theme::button("choose-stderr-file", false, cx)
                    .debug_selector(|| "choose-stderr-file".into())
                    .child(self.i18n.text("debug_log_choose"))
                    .on_click(cx.listener(|this, _, _, cx| this.choose_stderr_file(false, cx))),
            )
            .child(div().text_color(theme.text_secondary).child(status))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("debug_log_hint")),
            )
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
    }
}
