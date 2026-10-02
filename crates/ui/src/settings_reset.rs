//! "Restore Defaults" on the settings tabs (#134). A tab with defaults has
//! one reset that covers exactly the settings it shows; it is disabled while
//! they all have their default values.

use cayenchat_storage::{Appearance, Ircv3Preferences, Settings};
use gpui::{prelude::*, *};

use crate::{SettingsTab, SettingsWindow, settings_theme};

impl SettingsTab {
    /// Whether the tab has a "Restore Defaults". Connection holds the user's
    /// servers and Credentials moves saved secrets between stores, so neither
    /// has a reset.
    fn has_reset(self) -> bool {
        !matches!(self, Self::Connection | Self::Credentials)
    }
}

impl SettingsWindow {
    /// Whether everything `tab` shows already has its default value. The
    /// color palette is the user's own data and is not part of Appearance.
    pub(crate) fn tab_is_default(&self, tab: SettingsTab, cx: &App) -> bool {
        let defaults = Settings::default();
        let Ok(current) = self.settings.snapshot(cx) else {
            return false;
        };
        match tab {
            SettingsTab::Connection | SettingsTab::Credentials => true,
            SettingsTab::Appearance => {
                let appearance = Appearance {
                    saved_colors: current.appearance.saved_colors.clone(),
                    ..defaults.appearance
                };
                current.appearance == appearance
                    && current.theme == defaults.theme
                    && current.restore_window_layout == defaults.restore_window_layout
                    && (!cfg!(target_os = "linux")
                        || current.linux_display == defaults.linux_display)
            }
            SettingsTab::Keyboard => {
                current.channel_number_modifier == defaults.channel_number_modifier
                    && current.menu_bar_auto_hide == defaults.menu_bar_auto_hide
                    && (!cfg!(target_os = "linux")
                        || current.text_key_theme == defaults.text_key_theme)
            }
            SettingsTab::Shortcuts => current.keybindings.is_empty(),
            SettingsTab::Notifications => current.notifications == defaults.notifications,
            SettingsTab::Ircv3 => current
                .selected_profile()
                .is_none_or(|profile| profile.ircv3 == Ircv3Preferences::default()),
            SettingsTab::ImageUpload => current.image_upload == defaults.image_upload,
            SettingsTab::Experimental => current.experimental == defaults.experimental,
        }
    }

    /// Puts everything `tab` shows back to its default. Like any other
    /// change, it is saved and applied by the autosave.
    pub(crate) fn reset_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        let defaults = Settings::default();
        self.feedback = None;
        match tab {
            SettingsTab::Connection | SettingsTab::Credentials => {}
            SettingsTab::Appearance => {
                let appearance = Appearance {
                    saved_colors: std::mem::take(&mut self.settings.values.appearance.saved_colors),
                    ..defaults.appearance
                };
                self.show_appearance(&appearance, cx);
                let values = &mut self.settings.values;
                values.appearance = appearance;
                values.theme = defaults.theme;
                values.restore_window_layout = defaults.restore_window_layout;
                values.linux_display = defaults.linux_display;
                self.font_picker = None;
                self.color_picker = None;
            }
            SettingsTab::Keyboard => {
                let values = &mut self.settings.values;
                values.channel_number_modifier = defaults.channel_number_modifier;
                values.text_key_theme = defaults.text_key_theme;
                values.menu_bar_auto_hide = defaults.menu_bar_auto_hide;
                // This window's own bar follows at once, before it is saved.
                self.menu_bar.set_always(!defaults.menu_bar_auto_hide);
            }
            SettingsTab::Shortcuts => {
                self.settings.values.keybindings.clear();
                self.shortcut_recording = None;
            }
            SettingsTab::Notifications => {
                self.settings.values.notifications = defaults.notifications;
                self.settings
                    .keywords
                    .update(cx, |field, cx| field.set_text("", cx));
            }
            SettingsTab::Ircv3 => {
                if let Some(profile) = self.settings.values.selected_profile_mut() {
                    profile.ircv3 = Ircv3Preferences::default();
                    // Peer avatars are off again, so nothing is shared.
                    profile.peer_avatar_url.clear();
                }
            }
            SettingsTab::ImageUpload => self.select_upload_provider(None, cx),
            SettingsTab::Experimental => {
                self.settings.values.experimental = defaults.experimental;
                self.autosave_now(None, cx);
            }
        }
        cx.notify();
    }

    /// Fills the appearance text fields from `appearance`.
    fn show_appearance(&self, appearance: &Appearance, cx: &mut Context<Self>) {
        let form = &self.settings;
        let dark = &appearance.dark;
        let width = appearance.sub_log_name_width.to_string();
        for (field, value) in [
            (
                &form.member_list_background,
                &appearance.member_list_background,
            ),
            (&form.main_log_background, &appearance.main_log_background),
            (&form.main_log_alternate, &appearance.main_log_alternate),
            (&form.channel_event_color, &appearance.channel_event_color),
            (&form.highlight_color, &appearance.highlight_color),
            (&form.sub_log_background, &appearance.sub_log_background),
            (&form.sub_log_alternate, &appearance.sub_log_alternate),
            (
                &form.dark_member_list_background,
                &dark.member_list_background,
            ),
            (&form.dark_main_log_background, &dark.main_log_background),
            (&form.dark_main_log_alternate, &dark.main_log_alternate),
            (&form.dark_channel_event_color, &dark.channel_event_color),
            (&form.dark_highlight_color, &dark.highlight_color),
            (&form.dark_sub_log_background, &dark.sub_log_background),
            (&form.dark_sub_log_alternate, &dark.sub_log_alternate),
            (&form.main_log_font, &appearance.main_log_font),
            (&form.sub_log_font, &appearance.sub_log_font),
            (&form.member_font, &appearance.member_font),
            (&form.channel_font, &appearance.channel_font),
            (&form.input_font, &appearance.input_font),
            (&form.time_font, &appearance.time_font),
            (&form.sub_log_name_width, &width),
        ] {
            field.update(cx, |field, cx| field.set_text(value, cx));
        }
    }

    /// Asks before a reset that discards a lot of choices, then resets.
    fn request_reset(&mut self, tab: SettingsTab, window: &mut Window, cx: &mut Context<Self>) {
        if tab != SettingsTab::Appearance {
            self.reset_tab(tab, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &self.i18n.text("reset_confirm_title"),
            Some(&self.i18n.text("reset_confirm_detail")),
            &[
                PromptButton::ok(self.i18n.text("reset_to_defaults")),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| this.reset_tab(tab, cx));
            }
        })
        .detach();
    }

    /// A tab's heading with its "Restore Defaults" at the right.
    pub(crate) fn tab_heading(
        &self,
        tab: SettingsTab,
        title_key: &str,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = settings_theme::palette(cx);
        let heading = div().flex().items_center().gap_2().child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(20.))
                .font_weight(FontWeight::BOLD)
                .child(self.i18n.text(title_key)),
        );
        if !tab.has_reset() {
            return heading;
        }
        let default = self.tab_is_default(tab, cx);
        let button = settings_theme::button("reset-defaults", false, cx)
            .debug_selector(|| "reset-defaults".into())
            .child(self.i18n.text("reset_to_defaults"));
        heading.child(if default {
            button
                .cursor_default()
                .opacity(0.5)
                .text_color(theme.text_secondary)
        } else {
            button.on_click(cx.listener(move |this, _, window, cx| {
                this.request_reset(tab, window, cx);
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use cayenchat_storage::{Appearance, Notifications, ThemeMode};

    use super::{SettingsTab, SettingsWindow};

    fn form(
        tab: SettingsTab,
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<SettingsWindow>, &mut gpui::VisualTestContext) {
        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let owner = cx.add_window(|window, cx| {
            crate::ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let (form, cx) = cx.add_window_view(|window, cx| {
            let mut form = SettingsWindow::new(owner, settings.clone(), window, cx);
            form.tab = tab;
            form
        });
        cx.run_until_parked();
        (form, cx)
    }

    #[gpui::test]
    fn a_tab_resets_only_what_it_shows(cx: &mut gpui::TestAppContext) {
        let (form, cx) = form(SettingsTab::Appearance, cx);
        let is_default =
            |form: &gpui::Entity<SettingsWindow>, tab, cx: &mut gpui::VisualTestContext| {
                form.read_with(cx, |form, cx| form.tab_is_default(tab, cx))
            };
        assert!(is_default(&form, SettingsTab::Appearance, cx));

        form.update(cx, |form, cx| {
            let field = form.settings.main_log_background.clone();
            field.update(cx, |field, cx| field.set_text("#123456", cx));
            let values = &mut form.settings.values;
            values.appearance.alternate_rows = true;
            values.appearance.saved_colors.push("#ABCDEF".into());
            values.theme = ThemeMode::Dark;
            values.notifications.mentions = false;
            cx.notify();
        });
        assert!(!is_default(&form, SettingsTab::Appearance, cx));
        assert!(!is_default(&form, SettingsTab::Notifications, cx));

        form.update(cx, |form, cx| form.reset_tab(SettingsTab::Appearance, cx));
        form.read_with(cx, |form, cx| {
            let defaults = Appearance::default();
            assert_eq!(
                form.settings.main_log_background.read(cx).text(),
                defaults.main_log_background
            );
            assert_eq!(form.settings.values.theme, ThemeMode::System);
            assert!(!form.settings.values.appearance.alternate_rows);
            // The palette is the user's own; another tab's settings stay.
            assert_eq!(form.settings.values.appearance.saved_colors, ["#ABCDEF"]);
            assert!(!form.settings.values.notifications.mentions);
        });
        assert!(is_default(&form, SettingsTab::Appearance, cx));

        form.update(cx, |form, cx| {
            form.settings
                .keywords
                .update(cx, |field, cx| field.set_text("cat", cx));
            form.reset_tab(SettingsTab::Notifications, cx);
        });
        form.read_with(cx, |form, cx| {
            assert_eq!(form.settings.values.notifications, Notifications::default());
            assert_eq!(form.settings.keywords.read(cx).text(), "");
        });
        assert!(is_default(&form, SettingsTab::Notifications, cx));
    }

    #[gpui::test]
    fn the_button_is_inert_until_something_differs(cx: &mut gpui::TestAppContext) {
        let (form, cx) = form(SettingsTab::Notifications, cx);
        let click = |cx: &mut gpui::VisualTestContext| {
            let center = cx.debug_bounds("reset-defaults").unwrap().center();
            cx.simulate_click(center, gpui::Modifiers::default());
        };
        form.update(cx, |form, cx| {
            form.settings.values.notifications.private_messages = false;
            cx.notify();
        });
        cx.run_until_parked();
        click(cx);
        assert!(form.read_with(cx, |form, _| {
            form.settings.values.notifications.private_messages
        }));

        // Changing a value a second time leaves nothing to restore.
        form.update(cx, |form, cx| {
            form.settings.values.notifications.private_messages = true;
            cx.notify();
        });
        cx.run_until_parked();
        form.update(cx, |form, cx| {
            form.settings.values.notifications.mentions = false;
            cx.notify();
        });
        cx.run_until_parked();
        click(cx);
        assert!(form.read_with(cx, |form, _| form.settings.values.notifications.mentions));
    }

    #[gpui::test]
    fn up_and_down_move_through_the_categories(cx: &mut gpui::TestAppContext) {
        let (form, cx) = form(SettingsTab::Connection, cx);
        form.update_in(cx, |form, window, _| window.focus(&form.nav_focus));
        let tab = |form: &gpui::Entity<SettingsWindow>, cx: &mut gpui::VisualTestContext| {
            form.read_with(cx, |form, _| form.tab)
        };
        cx.simulate_keystrokes("down");
        assert!(tab(&form, cx) == SettingsTab::Appearance);
        cx.simulate_keystrokes("up up");
        assert!(tab(&form, cx) == SettingsTab::Connection);
    }
}
