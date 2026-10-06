//! "Restore Defaults" on the settings tabs (#134). A tab with defaults has
//! one reset that covers exactly the settings it shows; it is disabled while
//! they all have their default values.

use cayenchat_storage::{Appearance, Ircv3Preferences, Settings};
use gpui::{prelude::*, *};

use crate::{SettingsTab, SettingsWindow, settings_theme};

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
            // Login startup is the system's state, not a setting here, so
            // it is neither compared nor reset.
            SettingsTab::Application => {
                current.language == defaults.language
                    && current.restore_window_layout == defaults.restore_window_layout
                    && (cfg!(target_os = "macos")
                        || current.menu_bar_auto_hide == defaults.menu_bar_auto_hide)
                    && (!cfg!(target_os = "linux")
                        || current.linux_display == defaults.linux_display)
            }
            SettingsTab::Appearance => {
                let appearance = Appearance {
                    saved_colors: current.appearance.saved_colors.clone(),
                    ..defaults.appearance
                };
                current.appearance == appearance && current.theme == defaults.theme
            }
            SettingsTab::Keyboard => {
                current.channel_number_modifier == defaults.channel_number_modifier
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
    pub(crate) fn reset_tab(
        &mut self,
        tab: SettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let defaults = Settings::default();
        self.feedback = None;
        match tab {
            SettingsTab::Connection | SettingsTab::Credentials => {}
            SettingsTab::Application => {
                self.select_language(defaults.language, window, cx);
                self.settings.language_list_open = false;
                let values = &mut self.settings.values;
                values.restore_window_layout = defaults.restore_window_layout;
                // Settings hidden on this platform keep their saved values.
                if !cfg!(target_os = "macos") {
                    values.menu_bar_auto_hide = defaults.menu_bar_auto_hide;
                }
                if cfg!(target_os = "linux") {
                    values.linux_display = defaults.linux_display;
                }
            }
            SettingsTab::Appearance => {
                let appearance = Appearance {
                    saved_colors: std::mem::take(&mut self.settings.values.appearance.saved_colors),
                    ..defaults.appearance
                };
                self.show_appearance(&appearance, cx);
                let values = &mut self.settings.values;
                values.appearance = appearance;
                values.theme = defaults.theme;
                self.font_picker = None;
                self.color_picker = None;
            }
            SettingsTab::Keyboard => {
                let values = &mut self.settings.values;
                values.channel_number_modifier = defaults.channel_number_modifier;
                if cfg!(target_os = "linux") {
                    values.text_key_theme = defaults.text_key_theme;
                }
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
            (&form.notice_color, &appearance.notice_color),
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
            (&form.dark_notice_color, &dark.notice_color),
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

    /// Asks, then resets: a reset can discard many choices at once, and the
    /// button is easy to click by mistake.
    fn request_reset(&mut self, tab: SettingsTab, window: &mut Window, cx: &mut Context<Self>) {
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
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| this.reset_tab(tab, window, cx));
            }
        })
        .detach();
    }

    /// A tab's heading.
    pub(crate) fn tab_heading(&self, title_key: &str) -> Div {
        div()
            .text_size(px(20.))
            .font_weight(FontWeight::BOLD)
            .child(self.i18n.text(title_key))
    }

    /// The end of a tab with its reset at the right, in the same place on
    /// every tab. It is disabled while everything the tab shows has its
    /// default.
    pub(crate) fn reset_footer(&self, tab: SettingsTab, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let default = self.tab_is_default(tab, cx);
        let button = settings_theme::button("reset-defaults", false, cx)
            .debug_selector(|| "reset-defaults".into())
            .child(self.i18n.text("reset_to_defaults"));
        div().flex().justify_end().mt_2().child(if default {
            button
                .cursor_default()
                .opacity(0.5)
                .text_color(theme.text_secondary)
        } else {
            // A destructive action: warning color, at the right end, behind a
            // confirmation.
            button
                .text_color(theme.warning)
                .border_color(theme.warning)
                .on_click(cx.listener(move |this, _, window, cx| {
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
            values.appearance.compact_urls = true;
            values.appearance.saved_colors.push("#ABCDEF".into());
            values.theme = ThemeMode::Dark;
            values.notifications.mentions = false;
            cx.notify();
        });
        assert!(!is_default(&form, SettingsTab::Appearance, cx));
        assert!(!is_default(&form, SettingsTab::Notifications, cx));

        form.update_in(cx, |form, window, cx| {
            form.reset_tab(SettingsTab::Appearance, window, cx)
        });
        form.read_with(cx, |form, cx| {
            let defaults = Appearance::default();
            assert_eq!(
                form.settings.main_log_background.read(cx).text(),
                defaults.main_log_background
            );
            assert_eq!(form.settings.values.theme, ThemeMode::System);
            assert!(!form.settings.values.appearance.alternate_rows);
            assert!(!form.settings.values.appearance.compact_urls);
            // The palette is the user's own; another tab's settings stay.
            assert_eq!(form.settings.values.appearance.saved_colors, ["#ABCDEF"]);
            assert!(!form.settings.values.notifications.mentions);
        });
        assert!(is_default(&form, SettingsTab::Appearance, cx));

        form.update_in(cx, |form, window, cx| {
            form.settings
                .keywords
                .update(cx, |field, cx| field.set_text("cat", cx));
            form.reset_tab(SettingsTab::Notifications, window, cx);
        });
        form.read_with(cx, |form, cx| {
            assert_eq!(form.settings.values.notifications, Notifications::default());
            assert_eq!(form.settings.keywords.read(cx).text(), "");
        });
        assert!(is_default(&form, SettingsTab::Notifications, cx));
    }

    /// Moving a setting between pages must move it between resets too: each
    /// reset leaves exactly its own page default and the others untouched.
    #[gpui::test]
    fn each_page_reset_covers_its_own_items_and_no_others(cx: &mut gpui::TestAppContext) {
        use cayenchat_storage::{ChannelNumberModifier, Language, LinuxDisplay, Settings};

        // Each page's items, changed from their defaults.
        let dirty: [(SettingsTab, fn(&mut Settings)); 8] = [
            (SettingsTab::Application, |values| {
                values.language = Language::English;
                values.restore_window_layout = !values.restore_window_layout;
                values.menu_bar_auto_hide = !values.menu_bar_auto_hide;
                values.linux_display = LinuxDisplay::X11;
            }),
            (SettingsTab::Appearance, |values| {
                values.theme = ThemeMode::Dark;
                values.appearance.compact_urls = !values.appearance.compact_urls;
            }),
            (SettingsTab::Keyboard, |values| {
                values.channel_number_modifier = ChannelNumberModifier::Alt;
            }),
            (SettingsTab::Shortcuts, |values| {
                values.keybindings.insert("a".into(), "b".into());
            }),
            (SettingsTab::Notifications, |values| {
                values.notifications.mentions = !values.notifications.mentions;
            }),
            (SettingsTab::Ircv3, |values| {
                let profile = values.selected_profile_mut().unwrap();
                profile.ircv3.message_tags = !profile.ircv3.message_tags;
            }),
            (SettingsTab::ImageUpload, |values| {
                values.image_upload.provider = Some("provider".into());
            }),
            (SettingsTab::Experimental, |values| {
                values.experimental.debug_logging = !values.experimental.debug_logging;
            }),
        ];
        let (form, cx) = form(SettingsTab::Application, cx);
        let is_default =
            |form: &gpui::Entity<SettingsWindow>, tab, cx: &mut gpui::VisualTestContext| {
                form.read_with(cx, |form, cx| form.tab_is_default(tab, cx))
            };

        for (reset, _) in dirty {
            form.update(cx, |form, cx| {
                for (_, change) in dirty {
                    change(&mut form.settings.values);
                }
                cx.notify();
            });
            for (tab, _) in dirty {
                assert!(!is_default(&form, tab, cx), "dirty before reset");
            }

            form.update_in(cx, |form, window, cx| form.reset_tab(reset, window, cx));
            for (tab, _) in dirty {
                assert_eq!(
                    is_default(&form, tab, cx),
                    tab == reset,
                    "only the reset page returns to its default"
                );
            }

            // Back to defaults for the next round.
            form.update_in(cx, |form, window, cx| {
                for (tab, _) in dirty {
                    form.reset_tab(tab, window, cx);
                }
            });
        }

        // Login startup is the system's state: the reset neither starts a
        // change of it nor re-reads it.
        let startup = form.read_with(cx, |form, _| form.autostart.clone());
        form.update_in(cx, |form, window, cx| {
            form.reset_tab(SettingsTab::Application, window, cx);
            assert!(!form.autostart_busy);
        });
        assert_eq!(
            form.read_with(cx, |form, _| form.autostart.clone()),
            startup
        );
    }

    /// Settings the platform does not show are not part of that page's reset.
    #[gpui::test]
    fn reset_keeps_settings_hidden_on_this_platform(cx: &mut gpui::TestAppContext) {
        use cayenchat_storage::{LinuxDisplay, Settings, TextKeyTheme};

        let (form, cx) = form(SettingsTab::Application, cx);
        form.update_in(cx, |form, window, cx| {
            let values = &mut form.settings.values;
            values.menu_bar_auto_hide = !values.menu_bar_auto_hide;
            values.linux_display = LinuxDisplay::X11;
            values.text_key_theme = TextKeyTheme::Emacs;
            form.reset_tab(SettingsTab::Application, window, cx);
            form.reset_tab(SettingsTab::Keyboard, window, cx);
            let (values, defaults) = (&form.settings.values, Settings::default());
            assert_eq!(
                values.menu_bar_auto_hide == defaults.menu_bar_auto_hide,
                !cfg!(target_os = "macos")
            );
            assert_eq!(
                values.linux_display == defaults.linux_display,
                cfg!(target_os = "linux")
            );
            assert_eq!(
                values.text_key_theme == defaults.text_key_theme,
                cfg!(target_os = "linux")
            );
        });
    }

    #[gpui::test]
    fn the_button_is_inert_until_something_differs(cx: &mut gpui::TestAppContext) {
        let (form, cx) = form(SettingsTab::Notifications, cx);
        let click = |cx: &mut gpui::VisualTestContext| {
            let center = cx.debug_bounds("reset-defaults").unwrap().center();
            cx.simulate_click(center, gpui::Modifiers::default());
        };
        // A reset clears the feedback line; a click that does nothing leaves it.
        let marked = |form: &gpui::Entity<SettingsWindow>, cx: &mut gpui::VisualTestContext| {
            form.read_with(cx, |form, _| form.feedback.is_some())
        };
        form.update(cx, |form, cx| {
            form.feedback = Some("marker".into());
            cx.notify();
        });
        cx.run_until_parked();
        click(cx);
        assert!(
            !cx.has_pending_prompt() && marked(&form, cx),
            "nothing differs, so the click is ignored"
        );

        form.update(cx, |form, cx| {
            form.settings.values.notifications.mentions = false;
            cx.notify();
        });
        cx.run_until_parked();
        let mentions = |form: &gpui::Entity<SettingsWindow>, cx: &mut gpui::VisualTestContext| {
            form.read_with(cx, |form, _| form.settings.values.notifications.mentions)
        };

        // The reset waits for an answer; cancelling leaves everything.
        click(cx);
        assert!(cx.has_pending_prompt());
        let cancel = form.read_with(cx, |form, _| form.i18n.text("cancel"));
        cx.simulate_prompt_answer(&cancel);
        cx.run_until_parked();
        assert!(!mentions(&form, cx) && marked(&form, cx));

        click(cx);
        let confirm = form.read_with(cx, |form, _| form.i18n.text("reset_to_defaults"));
        cx.simulate_prompt_answer(&confirm);
        cx.run_until_parked();
        assert!(mentions(&form, cx) && !marked(&form, cx));
    }

    #[gpui::test]
    fn up_and_down_move_through_the_categories(cx: &mut gpui::TestAppContext) {
        let (form, cx) = form(SettingsTab::Connection, cx);
        form.update_in(cx, |form, window, _| window.focus(&form.nav_focus));
        let tab = |form: &gpui::Entity<SettingsWindow>, cx: &mut gpui::VisualTestContext| {
            form.read_with(cx, |form, _| form.tab)
        };
        cx.simulate_keystrokes("down");
        assert!(tab(&form, cx) == SettingsTab::Application);
        cx.simulate_keystrokes("up up");
        assert!(tab(&form, cx) == SettingsTab::Connection);
    }

    #[gpui::test]
    fn a_key_recording_and_the_category_list_do_not_disturb_each_other(
        cx: &mut gpui::TestAppContext,
    ) {
        let (form, cx) = form(SettingsTab::Shortcuts, cx);
        cx.update(|window, _| window.activate_window());
        form.update_in(cx, |form, window, cx| {
            window.focus(&form.nav_focus);
            form.toggle_shortcut_recording("next_channel", cx);
        });
        cx.run_until_parked();
        let state = |form: &gpui::Entity<SettingsWindow>, cx: &mut gpui::VisualTestContext| {
            form.read_with(cx, |form, _| {
                (
                    form.tab == SettingsTab::Shortcuts,
                    form.shortcut_recording.is_some(),
                )
            })
        };

        // Up and Down belong to the recording: refused as keys, and the
        // category stays.
        cx.simulate_keystrokes("down up");
        assert_eq!(state(&form, cx), (true, true));

        // Choosing another category ends the recording.
        let notifications = cx.debug_bounds("notifications-tab").unwrap().center();
        cx.simulate_click(notifications, gpui::Modifiers::default());
        form.read_with(cx, |form, _| {
            assert!(form.tab == SettingsTab::Notifications);
            assert!(form.shortcut_recording.is_none());
        });
    }

    #[gpui::test]
    fn the_connection_button_is_at_the_right_and_connects_without_a_connection(
        cx: &mut gpui::TestAppContext,
    ) {
        let (form, cx) = form(SettingsTab::Connection, cx);
        let viewport = cx.update(|window, _| window.viewport_size());
        let button = cx.debug_bounds("connection-button").expect("drawn");
        // The page is at most 720 px wide beside the 190 px list, with 16 px
        // of padding; the button ends at its right edge.
        let edge = viewport.width.min(gpui::px(190. + 720.)) - gpui::px(16.);
        assert!(
            button.right() > edge - gpui::px(24.),
            "right-aligned: {button:?} in {viewport:?}"
        );
        assert!(form.read_with(cx, |form, _| !form.connected_shown));
        let back = cx.debug_bounds("back-button").expect("drawn");
        assert!(back.left() < button.left());
    }

    #[gpui::test]
    fn the_connection_button_becomes_disconnect_in_the_same_place(cx: &mut gpui::TestAppContext) {
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
            form.tab = SettingsTab::Connection;
            form
        });
        cx.run_until_parked();
        let before = cx.debug_bounds("connection-button").expect("drawn");
        assert!(!form.read_with(cx, |form, _| form.connected_shown));

        // A reconnect scheduled for the shown server counts as connected.
        owner
            .update(cx, |chat, _, cx| {
                let network = chat
                    .network_of_profile(&settings.selected_server)
                    .expect("a session for the server");
                chat.sessions.get_mut(&network).unwrap().retry_pending = true;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        assert!(form.read_with(cx, |form, _| form.connected_shown));
        let after = cx.debug_bounds("connection-button").expect("drawn");
        assert_eq!(before.right(), after.right(), "the button does not move");
        assert!(cx.debug_bounds("connection-switch").is_none());
    }

    #[gpui::test]
    fn the_chat_window_opens_the_settings_from_its_own_update(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

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
        // As at startup without servers and on Ctrl+,: the settings window
        // is made while the chat window's update is running.
        let form = owner
            .update(cx, |_, _, cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    cx.new(|cx| SettingsWindow::new(owner, settings.clone(), window, cx))
                })
            })
            .unwrap()
            .expect("the settings window opens");
        cx.run_until_parked();
        let shown = |cx: &mut gpui::TestAppContext| {
            form.update(cx, |form, _, _| form.connected_shown).unwrap()
        };
        assert!(!shown(cx));
        let entity = form.update(cx, |_, _, cx| cx.entity()).unwrap();
        let notified = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = notified.clone();
        cx.update(|cx| cx.observe(&entity, move |_, _| flag.set(true)).detach());

        // The button still follows the chat window: a reconnect scheduled
        // for the shown server turns it on.
        owner
            .update(cx, |chat, _, cx| {
                let network = chat
                    .network_of_profile(&settings.selected_server)
                    .expect("a session for the server");
                chat.sessions.get_mut(&network).unwrap().retry_pending = true;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            notified.get(),
            "the chat window's change reaches the settings window"
        );
        assert!(shown(cx));
    }
}
