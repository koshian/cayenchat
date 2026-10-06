//! The Application page: how the app itself behaves, apart from any server.

use cayenchat_storage::{Language, LinuxDisplay};
use gpui::{prelude::*, *};

use crate::{SettingsTab, SettingsWindow, account_settings, settings_theme};

impl SettingsWindow {
    fn language_selector(&self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let mut selector = div().flex().flex_col().child(
            div()
                .id("language-select")
                .px_2()
                .py_1()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(format!(
                    "{}  ▾",
                    self.i18n.preference_label(self.settings.values.language)
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings.language_list_open = !this.settings.language_list_open;
                    cx.notify();
                })),
        );
        if self.settings.language_list_open {
            let mut menu = div()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface);
            for (index, language) in [Language::System, Language::Japanese, Language::English]
                .into_iter()
                .enumerate()
            {
                menu = menu.child(
                    div()
                        .id(("language-option", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .when(self.settings.values.language == language, |d| {
                            d.bg(theme.selected)
                        })
                        .child(self.i18n.preference_label(language))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.settings.language_list_open = false;
                            this.select_language(language, window, cx)
                        })),
                );
            }
            selector = selector.child(menu);
        }
        selector
    }

    pub(crate) fn render_application_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let language_selector = self.language_selector(cx);
        let autostart_toggle = self.render_autostart_toggle(cx);
        account_settings::panel(cx)
            .child(self.tab_heading("application_tab"))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("application_intro")),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("language")),
                    )
                    .child(language_selector),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("language_hint")),
            )
            .child(autostart_toggle)
            .child(
                div()
                    .id("restore-window-layout")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.restore_window_layout,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("restore_window_layout"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.restore_window_layout;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            // macOS has its own menu bar that is always shown.
            .when(!cfg!(target_os = "macos"), |d| {
                d.child(
                    div()
                        .id("menu-bar-auto-hide")
                        .ml(px(158.))
                        .flex()
                        .gap_2()
                        .cursor_pointer()
                        .child(settings_theme::checkbox(
                            self.settings.values.menu_bar_auto_hide,
                            true,
                            cx,
                        ))
                        .child(self.i18n.text("menu_bar_auto_hide"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let hide = !this.settings.values.menu_bar_auto_hide;
                            this.settings.values.menu_bar_auto_hide = hide;
                            cx.notify();
                        })),
                )
            })
            .when(cfg!(target_os = "linux"), |d| {
                d.child(self.option_row(
                    "linux_display",
                    [
                        (LinuxDisplay::Wayland, "linux_display_wayland"),
                        (LinuxDisplay::X11, "linux_display_x11"),
                    ],
                    self.settings.values.linux_display,
                    |settings, display| settings.linux_display = display,
                    cx,
                ))
                .child(
                    div()
                        .ml(px(158.))
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("linux_display_hint")),
                )
            })
            .child(
                div()
                    .id("image-previews")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.appearance.image_previews,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("image_previews"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.image_previews;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("image_previews_hint")),
            )
            .child(
                div()
                    .id("user-avatars")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.appearance.user_avatars,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("user_avatars"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.user_avatars;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("user_avatars_hint")),
            )
            .child(
                div()
                    .id("reiwa-mode")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.appearance.reiwa_mode,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("reiwa_mode"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.reiwa_mode;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("compact-urls")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.appearance.compact_urls,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("compact_urls"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.compact_urls;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("compact_urls_hint")),
            )
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(self.reset_footer(SettingsTab::Application, cx))
    }
}
