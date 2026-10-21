// SPDX-License-Identifier: GPL-3.0-only
//! The first-run setup (#521, D060): a choice of quick setup, manual setup
//! and import, drawn in the chat window like the transfer dialog.
//!
//! It opens only when there was no settings file at startup. Cancelling it
//! writes and connects nothing. Quick setup adds one ordinary server profile
//! to the settings through the paths the settings window uses (the settings
//! file, `apply_settings_change`, `connect_profile`); it has no connection
//! path of its own. Manual setup opens the connection settings and the import
//! is the one of the File menu (D059).

use cayenchat_storage::{PRESETS, ServerProfile, Settings, TextEncoding};
use gpui::{prelude::*, *};

use crate::{ChatWindow, TextInput, button_order, dialog, secrets, settings_file, theme};

const BOX_MAX_WIDTH: Pixels = px(480.);
/// What quick setup joins; the server is the first preset.
const CHANNEL: &str = "#CayenChat";
const ENCODING: TextEncoding = TextEncoding::Iso2022Jp;

enum Step {
    Choose,
    /// Asks for the nickname and shows what will be connected.
    Easy {
        error: Option<String>,
    },
    /// The settings are being written in the background.
    Saving,
    /// The profile was saved but the connection could not start.
    Failed {
        error: String,
        profile: String,
    },
}

pub(crate) struct SetupWizard {
    open: bool,
    step: Step,
    nickname: Entity<TextInput>,
    easy: FocusHandle,
    manual: FocusHandle,
    import: FocusHandle,
    later: FocusHandle,
    /// The primary and secondary button of the Easy and Failed steps.
    primary: FocusHandle,
    secondary: FocusHandle,
    /// The server a test connects to instead of IRCnet.
    #[cfg(test)]
    redirect: Option<(String, u16)>,
}

impl SetupWizard {
    pub(crate) fn new(cx: &mut Context<ChatWindow>) -> Self {
        Self {
            open: false,
            step: Step::Choose,
            nickname: cx.new(|cx| TextInput::new_field("", "", false, cx)),
            easy: cx.focus_handle().tab_stop(true),
            manual: cx.focus_handle().tab_stop(true),
            import: cx.focus_handle().tab_stop(true),
            later: cx.focus_handle().tab_stop(true),
            primary: cx.focus_handle().tab_stop(true),
            secondary: cx.focus_handle().tab_stop(true),
            #[cfg(test)]
            redirect: None,
        }
    }

    /// The controls of the current step in Tab order.
    fn handles(&self, cx: &App) -> Vec<FocusHandle> {
        match self.step {
            Step::Choose => vec![
                self.easy.clone(),
                self.manual.clone(),
                self.import.clone(),
                self.later.clone(),
            ],
            Step::Easy { .. } => vec![
                self.nickname.read(cx).focus_handle(cx),
                self.primary.clone(),
                self.secondary.clone(),
            ],
            Step::Failed { .. } => vec![self.primary.clone(), self.secondary.clone()],
            Step::Saving => Vec::new(),
        }
    }

    pub(crate) fn nickname(&self) -> &Entity<TextInput> {
        &self.nickname
    }

    #[cfg(test)]
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
}

/// Adds the quick-setup server to `settings` and returns its profile id. The
/// profile is an ordinary one: the first preset's host with the standard
/// connection settings of a new server, ISO-2022-JP, `#CayenChat` as an
/// auto-join channel and a connection at startup. The `USER` username is the
/// nickname the user chose and the real name is left to the built-in default,
/// so nothing is taken from the operating system.
pub(crate) fn add_easy_server(settings: &mut Settings, nickname: &str) -> String {
    let profile: &mut ServerProfile = settings.add_server(PRESETS[0].host);
    profile.encoding = ENCODING;
    profile.channels = CHANNEL.into();
    profile.nickname = nickname.into();
    profile.username = nickname.into();
    profile.connect_on_startup = true;
    profile.id.clone()
}

impl ChatWindow {
    /// Opens the wizard at its first step.
    pub(crate) fn open_setup_wizard(&mut self, cx: &mut Context<Self>) {
        self.setup_wizard.open = true;
        self.setup_wizard.step = Step::Choose;
        cx.notify();
    }

    /// Whether the wizard is on screen: not while an import is picked, read
    /// or confirmed (it comes back if that ends without servers), and not
    /// once there are servers, except to report a failed connection.
    fn setup_wizard_visible(&self) -> bool {
        self.setup_wizard.open
            && self.transfer_dialog.is_idle()
            && (self.saved.servers.is_empty()
                || matches!(self.setup_wizard.step, Step::Failed { .. }))
    }

    fn wizard_choose_easy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.setup_wizard.step = Step::Easy { error: None };
        window.focus(&self.setup_wizard.nickname.read(cx).focus_handle(cx));
        cx.notify();
    }

    fn wizard_choose_manual(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.setup_wizard.open = false;
        self.open_settings_for(None, window, cx);
        cx.notify();
    }

    fn wizard_choose_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.import_settings(&crate::ImportSettings, window, cx);
    }

    /// Closes the wizard; nothing was written.
    fn wizard_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.setup_wizard.open = false;
        window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
        cx.notify();
    }

    fn wizard_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.setup_wizard.step = Step::Choose;
        window.focus(&self.setup_wizard.easy);
        cx.notify();
    }

    /// Whether the nickname field has focus; Enter connects and Tab moves on
    /// then, instead of acting on the chat window's draft.
    pub(crate) fn wizard_nickname_focused(&self, window: &Window, cx: &App) -> bool {
        self.setup_wizard.open
            && matches!(self.setup_wizard.step, Step::Easy { .. })
            && self
                .setup_wizard
                .nickname
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
    }

    pub(crate) fn wizard_move_focus(&self, backwards: bool, window: &mut Window, cx: &App) {
        let handles = self.setup_wizard.handles(cx);
        if handles.is_empty() {
            return;
        }
        let current = handles.iter().position(|handle| handle.is_focused(window));
        let next = match (current, backwards) {
            (Some(index), false) => (index + 1) % handles.len(),
            (Some(index), true) => (index + handles.len() - 1) % handles.len(),
            (None, _) => 0,
        };
        window.focus(&handles[next]);
    }

    /// "Connect with these settings": checks the nickname as a connection
    /// does, saves a new server profile and connects it. A failure at any
    /// point before the save leaves the settings as they were.
    pub(crate) fn wizard_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.setup_wizard.step, Step::Easy { .. }) {
            return;
        }
        if self.setup_wizard.nickname.read(cx).is_composing() {
            return;
        }
        let nickname = self.setup_wizard.nickname.read(cx).text().trim().to_owned();
        let invalid = if nickname.is_empty() {
            Some(self.i18n.text("wizard_nickname_required"))
        } else {
            // The checks of a connection, on a copy.
            let mut probe = self.saved.clone();
            let id = add_easy_server(&mut probe, &nickname);
            probe.profile(&id).and_then(|profile| {
                crate::saved_connection_config(profile, probe.language, &secrets::store(cx)).err()
            })
        };
        if let Some(error) = invalid {
            self.setup_wizard.step = Step::Easy { error: Some(error) };
            cx.notify();
            return;
        }
        self.setup_wizard.step = Step::Saving;
        let stand_in = self.saved.clone();
        #[cfg(test)]
        let redirect = self.setup_wizard.redirect.clone();
        let saving = cx.background_spawn(async move {
            // The file is the truth when there is one.
            let mut settings = settings_file::load().ok().flatten().unwrap_or(stand_in);
            let id = add_easy_server(&mut settings, &nickname);
            // A test connects to a server of its own, not to IRCnet.
            #[cfg(test)]
            if let Some((host, port)) = redirect {
                let profile = settings.servers.last_mut().expect("just added");
                profile.host = host;
                profile.port = port;
            }
            settings_file::save(&settings).map(|()| (settings, id))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = saving.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((settings, id)) => this.wizard_saved(settings, id, window, cx),
                Err(error) => {
                    let lines = [
                        this.i18n.format("wizard_save_failed", &[("error", &error)]),
                        this.i18n.text("wizard_unchanged"),
                    ];
                    this.setup_wizard.step = Step::Easy {
                        error: Some(lines.join("\n")),
                    };
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// The profile is in the file: show it in this window and connect.
    fn wizard_saved(
        &mut self,
        settings: Settings,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.saved.clone();
        self.apply_settings_change(&previous, settings.clone(), true, window, cx);
        let connected = settings
            .profile(&id)
            .ok_or_else(|| self.i18n.text("not_connected"))
            .and_then(|profile| {
                crate::saved_connection_config(profile, settings.language, &secrets::store(cx))
            })
            .and_then(|config| self.connect_profile(&id, config, window, cx));
        match connected {
            Ok(()) => {
                self.setup_wizard.open = false;
                self.setup_wizard
                    .nickname
                    .update(cx, |input, cx| input.set_text("", cx));
            }
            Err(error) => {
                self.setup_wizard.step = Step::Failed { error, profile: id };
            }
        }
        cx.notify();
    }

    fn wizard_open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Step::Failed { profile, .. } = &self.setup_wizard.step else {
            return;
        };
        let profile = profile.clone();
        self.setup_wizard.open = false;
        self.open_settings_for(Some(profile), window, cx);
        cx.notify();
    }

    fn wizard_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.setup_wizard.step {
            Step::Choose | Step::Failed { .. } => self.wizard_dismiss(window, cx),
            Step::Easy { .. } => self.wizard_back(window, cx),
            Step::Saving => {}
        }
    }

    pub(crate) fn render_setup_wizard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.setup_wizard_visible() {
            return None;
        }
        // Focus goes to the first control when it is not already inside.
        let handles = self.setup_wizard.handles(cx);
        if !handles.is_empty() && !handles.iter().any(|handle| handle.is_focused(window)) {
            window.focus(&handles[0]);
        }
        let theme = theme::current(cx);
        let box_width = dialog::box_width(BOX_MAX_WIDTH, window);
        let inner = box_width - px(26.);
        let mut body = div().flex().flex_col().gap_2().w(inner);
        match &self.setup_wizard.step {
            Step::Choose => {
                body = body
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(self.i18n.text("wizard_intro")),
                    )
                    .child(self.choice_card(
                        "wizard-easy",
                        "wizard_easy_title",
                        "wizard_easy_hint",
                        true,
                        &self.setup_wizard.easy,
                        |this, window, cx| this.wizard_choose_easy(window, cx),
                        &theme,
                        cx,
                    ))
                    .child(self.choice_card(
                        "wizard-manual",
                        "wizard_manual_title",
                        "wizard_manual_hint",
                        false,
                        &self.setup_wizard.manual,
                        |this, window, cx| this.wizard_choose_manual(window, cx),
                        &theme,
                        cx,
                    ))
                    .child(self.choice_card(
                        "wizard-import",
                        "wizard_import_title",
                        "wizard_import_hint",
                        false,
                        &self.setup_wizard.import,
                        |this, window, cx| this.wizard_choose_import(window, cx),
                        &theme,
                        cx,
                    ))
                    .child(div().flex().child(dialog::cancel_button(
                        "wizard-later",
                        self.i18n.text("wizard_later"),
                        &self.setup_wizard.later,
                        &theme,
                        |this, window, cx| this.wizard_dismiss(window, cx),
                        cx,
                    )));
            }
            Step::Easy { error } => {
                let row = |label: &str, value: String| {
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .w(px(110.))
                                .flex_none()
                                .text_color(theme.text_secondary)
                                .child(label.to_owned()),
                        )
                        .child(div().text_color(theme.text).child(value))
                };
                body = body
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(self.i18n.text("wizard_easy_detail")),
                    )
                    .child(row(
                        &self.i18n.text("wizard_server_label"),
                        PRESETS[0].host.into(),
                    ))
                    .child(row(
                        &self.i18n.text("wizard_encoding_label"),
                        ENCODING.label().into(),
                    ))
                    .child(row(&self.i18n.text("wizard_channel_label"), CHANNEL.into()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .child(self.i18n.text("wizard_nickname_label")),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .px_1()
                                    .border_1()
                                    .border_color(theme.border)
                                    .child(self.setup_wizard.nickname.clone()),
                            )
                            .child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(self.i18n.text("wizard_nickname_hint")),
                            ),
                    )
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(self.i18n.text("wizard_startup_hint")),
                    )
                    .when_some(error.clone(), |d, error| {
                        d.child(div().text_color(theme.warning).child(error))
                    })
                    .child(
                        button_order::row(div().flex().gap_2())
                            .child(dialog::confirm_button(
                                "wizard-connect",
                                self.i18n.text("wizard_connect"),
                                &self.setup_wizard.primary,
                                &theme,
                                |this, window, cx| this.wizard_connect(window, cx),
                                cx,
                            ))
                            .child(dialog::cancel_button(
                                "wizard-back",
                                self.i18n.text("wizard_back"),
                                &self.setup_wizard.secondary,
                                &theme,
                                |this, window, cx| this.wizard_back(window, cx),
                                cx,
                            )),
                    );
            }
            Step::Saving => {
                body = body.child(
                    div()
                        .text_color(theme.text)
                        .child(self.i18n.text("wizard_saving")),
                );
            }
            Step::Failed { error, .. } => {
                body = body
                    .child(div().text_color(theme.warning).child(error.clone()))
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(self.i18n.text("wizard_failed_saved")),
                    )
                    .child(
                        button_order::row(div().flex().gap_2())
                            .child(dialog::confirm_button(
                                "wizard-open-settings",
                                self.i18n.text("wizard_open_settings"),
                                &self.setup_wizard.primary,
                                &theme,
                                |this, window, cx| this.wizard_open_settings(window, cx),
                                cx,
                            ))
                            .child(dialog::cancel_button(
                                "wizard-close",
                                self.i18n.text("transfer_close"),
                                &self.setup_wizard.secondary,
                                &theme,
                                |this, window, cx| this.wizard_dismiss(window, cx),
                                cx,
                            )),
                    );
            }
        }
        let title = match self.setup_wizard.step {
            Step::Failed { .. } => self.i18n.text("wizard_failed_title"),
            _ => self.i18n.text("wizard_title"),
        };
        Some(
            dialog::overlay("setup-wizard", &theme)
                .pb(px(16.))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            if !this.setup_wizard.nickname.read(cx).is_composing() {
                                this.wizard_escape(window, cx);
                            }
                        }
                        "tab" => {
                            let backwards = event.keystroke.modifiers.shift;
                            this.wizard_move_focus(backwards, window, cx);
                        }
                        // Typing goes on to the nickname field.
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .child(
                    dialog::dialog_box(&theme)
                        .debug_selector(|| "setup-wizard-box".into())
                        .max_w(box_width)
                        .max_h_full()
                        .h_auto()
                        .child(div().flex_none().font_weight(FontWeight::BOLD).child(title))
                        .child(
                            div()
                                .id("setup-wizard-body")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .child(body),
                        ),
                )
                .into_any_element(),
        )
    }

    /// One of the three large choices; the recommended one is marked.
    #[allow(clippy::too_many_arguments)]
    fn choice_card(
        &self,
        id: &'static str,
        title: &str,
        hint: &str,
        recommended: bool,
        focus: &FocusHandle,
        on_press: impl Fn(&mut ChatWindow, &mut Window, &mut Context<ChatWindow>) + Copy + 'static,
        theme: &theme::Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let text = theme.text;
        div()
            .id(id)
            .debug_selector(|| id.into())
            .track_focus(focus)
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .border_1()
            .border_color(if recommended {
                theme.selected
            } else {
                theme.border
            })
            .when(recommended, |d| d.bg(theme.selected))
            .focus(move |s| s.border_color(text))
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.text)
                            .child(self.i18n.text(title)),
                    )
                    .when(recommended, |d| {
                        d.child(
                            div()
                                .px_1()
                                .border_1()
                                .border_color(theme.text)
                                .text_color(theme.text)
                                .child(self.i18n.text("wizard_recommended")),
                        )
                    }),
            )
            .child(
                div()
                    .text_color(if recommended {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(self.i18n.text(hint)),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    on_press(this, window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| on_press(this, window, cx)))
    }
}

#[cfg(test)]
mod tests {
    use cayenchat_storage::{Language, Settings, TextEncoding, ThemeMode, transfer};
    use gpui::TestAppContext;

    use super::add_easy_server;
    use crate::{ChatWindow, settings_file, settings_file::TestFile, settings_with_channels};

    /// Whether the wizard is on screen. Not read from `debug_bounds`, which
    /// keeps the bounds of elements drawn in earlier frames.
    fn visible(chat: &gpui::Entity<ChatWindow>, cx: &mut gpui::VisualTestContext) -> bool {
        chat.read_with(cx, |chat, _| chat.setup_wizard_visible())
    }

    fn english() -> Settings {
        let mut settings = Settings::default();
        settings.language = Language::English;
        settings
    }

    fn window<'a>(
        settings: &Settings,
        cx: &'a mut TestAppContext,
    ) -> (gpui::Entity<ChatWindow>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            cx.set_global(crate::theme::Theme::new(
                ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        cx.run_until_parked();
        (chat, cx)
    }

    #[test]
    fn the_quick_setup_profile_is_the_ircnet_one_without_system_identity() {
        let mut settings = english();
        let id = add_easy_server(&mut settings, "Tester");
        let profile = settings.profile(&id).unwrap();
        assert_eq!(profile.host, "irc.ircnet.com");
        assert_eq!(profile.encoding, TextEncoding::Iso2022Jp);
        assert_eq!(profile.channels, "#CayenChat");
        assert_eq!(profile.channels(), ["#CayenChat"]);
        assert!(profile.connect_on_startup);
        assert_eq!(profile.nickname, "Tester");
        assert_eq!(profile.username, "Tester");
        assert!(profile.realname.is_empty());
        assert!(profile.id.starts_with("custom-"));
        assert_eq!(settings.selected_server, id);
        // The rest is what any new server has.
        let mut plain = english();
        let plain_id = plain.add_server("irc.ircnet.com").id.clone();
        let (a, b) = (profile, plain.profile(&plain_id).unwrap());
        assert_eq!((a.port, a.use_tls), (b.port, b.use_tls));
        assert_eq!(a.verify_tls_certificates, b.verify_tls_certificates);
        assert_eq!(a.remember_passwords, b.remember_passwords);
    }

    #[test]
    fn only_a_missing_settings_file_is_a_first_run() {
        assert!(crate::is_first_run(&Ok(None)));
        assert!(!crate::is_first_run(&Ok(Some(Settings::default()))));
        assert!(!crate::is_first_run(&Err("damaged".into())));
    }

    #[gpui::test]
    fn the_choices_are_shown_with_quick_setup_first_and_dismissing_saves_nothing(
        cx: &mut TestAppContext,
    ) {
        let file = TestFile::with(&english());
        let (chat, cx) = window(&english(), cx);
        chat.update(cx, |chat, cx| chat.open_setup_wizard(cx));
        cx.run_until_parked();
        for id in [
            "wizard-easy",
            "wizard-manual",
            "wizard-import",
            "wizard-later",
        ] {
            assert!(cx.debug_bounds(id).is_some(), "{id} is drawn");
        }
        let easy = cx.debug_bounds("wizard-easy").unwrap();
        let manual = cx.debug_bounds("wizard-manual").unwrap();
        assert!(easy.top() < manual.top(), "quick setup comes first");
        chat.update_in(cx, |chat, window, _| {
            assert!(
                chat.setup_wizard.easy.is_focused(window),
                "and is preselected"
            );
        });

        chat.update_in(cx, |chat, window, cx| chat.wizard_dismiss(window, cx));
        cx.run_until_parked();
        assert!(!visible(&chat, cx));
        assert!(settings_file::load().unwrap().unwrap().servers.is_empty());
        chat.read_with(cx, |chat, _| {
            assert!(chat.saved.servers.is_empty());
            assert!(chat.sessions.is_empty());
        });
        drop(file);
    }

    #[gpui::test]
    fn quick_setup_asks_for_a_nickname_and_saves_and_connects_only_when_told(
        cx: &mut TestAppContext,
    ) {
        let file = TestFile::with(&english());
        let (chat, cx) = window(&english(), cx);
        // A server of the test's own stands in for IRCnet.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        chat.update(cx, |chat, cx| {
            chat.setup_wizard.redirect = Some(("127.0.0.1".into(), port));
            chat.open_setup_wizard(cx);
        });
        chat.update_in(cx, |chat, window, cx| chat.wizard_choose_easy(window, cx));
        cx.run_until_parked();
        // The fixed settings are on screen; showing them connects nothing.
        assert!(cx.debug_bounds("wizard-connect").is_some());
        chat.read_with(cx, |chat, _| assert!(chat.sessions.is_empty()));

        // No nickname: refused, nothing saved.
        chat.update_in(cx, |chat, window, cx| chat.wizard_connect(window, cx));
        cx.run_until_parked();
        assert!(settings_file::load().unwrap().unwrap().servers.is_empty());
        chat.read_with(cx, |chat, _| {
            assert!(matches!(
                chat.setup_wizard.step,
                super::Step::Easy { error: Some(_) }
            ));
        });

        // A nickname a connection refuses is refused the same way.
        chat.update(cx, |chat, cx| {
            chat.setup_wizard
                .nickname
                .update(cx, |input, cx| input.set_text("two words", cx))
        });
        chat.update_in(cx, |chat, window, cx| chat.wizard_connect(window, cx));
        cx.run_until_parked();
        assert!(settings_file::load().unwrap().unwrap().servers.is_empty());

        // A failing write keeps the settings and says so.
        chat.update(cx, |chat, cx| {
            chat.setup_wizard
                .nickname
                .update(cx, |input, cx| input.set_text("tester", cx))
        });
        settings_file::fail_saves(true);
        chat.update_in(cx, |chat, window, cx| chat.wizard_connect(window, cx));
        cx.run_until_parked();
        settings_file::fail_saves(false);
        chat.read_with(cx, |chat, _| {
            let super::Step::Easy { error: Some(error) } = &chat.setup_wizard.step else {
                panic!("the wizard stays on the nickname step");
            };
            assert!(error.contains("cannot write"), "{error}");
            assert!(chat.saved.servers.is_empty());
            assert!(chat.sessions.is_empty());
        });
        assert!(settings_file::load().unwrap().unwrap().servers.is_empty());

        // Connect: saved, shown and connected, with nothing of the system.
        chat.update_in(cx, |chat, window, cx| chat.wizard_connect(window, cx));
        cx.run_until_parked();
        let saved = settings_file::load().unwrap().unwrap();
        assert_eq!(saved.servers.len(), 1);
        let profile = &saved.servers[0];
        assert_eq!(profile.nickname, "tester");
        assert_eq!(profile.username, "tester");
        assert!(profile.realname.is_empty());
        assert_eq!(profile.encoding, TextEncoding::Iso2022Jp);
        assert_eq!(profile.channels(), ["#CayenChat"]);
        chat.read_with(cx, |chat, _| {
            assert!(!chat.setup_wizard.is_open());
            assert_eq!(chat.saved.servers, saved.servers);
            let session = chat.sessions.values().next().unwrap();
            assert_eq!(session.profile_id, profile.id);
            assert!(session.irc.is_some(), "the connection was started");
        });
        assert!(!visible(&chat, cx));
        drop(listener);
        drop(file);
    }

    #[gpui::test]
    fn the_wizard_steps_aside_for_an_import_and_returns_when_it_ends_without_servers(
        cx: &mut TestAppContext,
    ) {
        let file = TestFile::with(&english());
        let (chat, cx) = window(&english(), cx);
        chat.update(cx, |chat, cx| chat.open_setup_wizard(cx));
        cx.run_until_parked();
        let directory = std::env::temp_dir().join(format!(
            "cayenchat-ui-wizard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&directory).unwrap();

        // A failed import shows its error over the wizard; closing it brings
        // the three choices back, with nothing changed.
        let bad = directory.join("bad.json");
        std::fs::write(&bad, r#"{"version": 999, "servers": []}"#).unwrap();
        chat.update_in(cx, |chat, window, cx| chat.read_import(bad, window, cx));
        cx.run_until_parked();
        assert!(!visible(&chat, cx));
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(false, window, cx)
        });
        cx.run_until_parked();
        assert!(visible(&chat, cx));
        assert!(settings_file::load().unwrap().unwrap().servers.is_empty());

        // An import that is cancelled at its confirmation does the same.
        let good = directory.join("good.json");
        transfer::export_to(&good, &english_with_server()).unwrap();
        chat.update_in(cx, |chat, window, cx| {
            chat.read_import(good.clone(), window, cx)
        });
        cx.run_until_parked();
        assert!(!visible(&chat, cx));
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(false, window, cx)
        });
        cx.run_until_parked();
        assert!(visible(&chat, cx));
        assert!(settings_file::load().unwrap().unwrap().servers.is_empty());

        // A confirmed import leaves the wizard gone.
        chat.update_in(cx, |chat, window, cx| chat.read_import(good, window, cx));
        cx.run_until_parked();
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(true, window, cx)
        });
        cx.run_until_parked();
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(false, window, cx)
        });
        cx.run_until_parked();
        assert!(!visible(&chat, cx));
        assert_eq!(settings_file::load().unwrap().unwrap().servers.len(), 1);
        std::fs::remove_dir_all(directory).unwrap();
        drop(file);
    }

    fn english_with_server() -> Settings {
        let mut settings = settings_with_channels("#imported");
        settings.language = Language::English;
        settings
    }
}
