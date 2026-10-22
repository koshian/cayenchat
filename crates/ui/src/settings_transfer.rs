// SPDX-License-Identifier: GPL-3.0-only
//! Exporting the settings to a file and importing them from one (#520,
//! D059), as questions drawn in the chat window like the quit dialog.
//!
//! Nothing is written before the user has read what will happen: an export
//! shows what the file contains, an import is read and checked first and then
//! confirmed with its consequences. The storage crate does the work
//! (`cayenchat_storage::transfer`); this module is the dialog around it.

use std::path::{Path, PathBuf};

use cayenchat_model::display;
use cayenchat_storage::{ServerProfile, transfer};
use gpui::{prelude::*, *};

use crate::{ChatWindow, button_order, dialog, settings_file, theme};

const BOX_MAX_WIDTH: Pixels = px(420.);
/// Longest name or host shown for an imported server, in characters.
const SHOWN_NAME_MAX: usize = 64;
/// How many imported servers are listed whatever their settings; further ones
/// are listed only when their connection settings are unsafe.
const SERVERS_LISTED: usize = 16;

/// One imported server as the confirmation shows it.
struct ServerRow {
    endpoint: String,
    warnings: Vec<String>,
}

/// A value from the file made safe to draw: controls (including line breaks)
/// become spaces, bidirectional controls are neutralized and a long value is
/// cut. Done where the value is drawn, whatever the import check allowed.
fn shown(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let flat = display::neutralize_bidi_owned(flat);
    let mut chars = flat.trim().chars();
    let mut out: String = chars.by_ref().take(SHOWN_NAME_MAX).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}
/// The file name the save dialog starts with.
const EXPORT_FILE_NAME: &str = "cayenchat-settings.json";

/// An import that was read and found valid, waiting for the confirmation.
struct PendingImport {
    path: PathBuf,
    import: transfer::Import,
}

enum Step {
    /// Tells what an export contains before the save dialog opens.
    ExportNotice,
    ImportConfirm(Box<PendingImport>),
    /// The file is being written in the background; nothing can be answered
    /// and no other transfer starts until it is done.
    Working,
    /// The end of an export or an import, successful or not.
    Result {
        title: String,
        lines: Vec<String>,
    },
}

/// The open question, its buttons and where the focus was before.
pub(crate) struct TransferDialog {
    step: Option<Step>,
    cancel: FocusHandle,
    confirm: FocusHandle,
    previous: Option<FocusHandle>,
    focus_pending: bool,
    /// An import is being picked or read. Claimed before the picker opens so
    /// that a slow pick or read cannot overlap another transfer, and released
    /// when its outcome is shown.
    reading: bool,
}

impl TransferDialog {
    pub(crate) fn new(cx: &mut Context<ChatWindow>) -> Self {
        Self {
            step: None,
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
            previous: None,
            focus_pending: false,
            reading: false,
        }
    }

    fn is_working(&self) -> bool {
        self.reading || matches!(self.step, Some(Step::Working))
    }

    /// Nothing is shown, picked or written.
    pub(crate) fn is_idle(&self) -> bool {
        self.step.is_none() && !self.reading
    }

    #[cfg(test)]
    pub(crate) fn result(&self) -> Option<&[String]> {
        match &self.step {
            Some(Step::Result { lines, .. }) => Some(lines),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_confirming_import(&self) -> bool {
        matches!(self.step, Some(Step::ImportConfirm(_)))
    }
}

impl ChatWindow {
    /// A file is being written for an export or import.
    pub(crate) fn transfer_dialog_busy(&self) -> bool {
        self.transfer_dialog.is_working()
    }

    fn show_transfer(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        if self.transfer_dialog.step.is_none() {
            self.transfer_dialog.previous = window.focused(cx);
        }
        self.transfer_dialog.step = Some(step);
        self.transfer_dialog.focus_pending = true;
        cx.notify();
    }

    fn close_transfer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transfer_dialog.step = None;
        if let Some(handle) = self.transfer_dialog.previous.take() {
            window.focus(&handle);
        }
        cx.notify();
    }

    fn transfer_failed(
        &mut self,
        title: &str,
        error: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_transfer(
            Step::Result {
                title: self.i18n.text(title),
                lines: vec![error.to_owned(), self.i18n.text("transfer_unchanged")],
            },
            window,
            cx,
        );
    }

    /// File → Export Settings…: explains what the file contains first.
    pub(crate) fn export_settings(
        &mut self,
        _: &crate::ExportSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.transfer_dialog.step.is_none() && !self.transfer_dialog.reading {
            window.activate_window();
            self.show_transfer(Step::ExportNotice, window, cx);
        }
    }

    /// File → Import Settings…: asks for the file, reads and checks it.
    pub(crate) fn import_settings(
        &mut self,
        _: &crate::ImportSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.transfer_dialog.step.is_some() || self.transfer_dialog.reading {
            return;
        }
        self.transfer_dialog.reading = true;
        window.activate_window();
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
            filter: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = chosen.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.read_import(path, window, cx);
                    }
                }
                Ok(Ok(None)) => this.transfer_dialog.reading = false,
                Ok(Err(error)) => {
                    this.transfer_dialog.reading = false;
                    this.transfer_failed("transfer_import_title", &error.to_string(), window, cx)
                }
                Err(_) => {
                    this.transfer_dialog.reading = false;
                    this.transfer_failed(
                        "transfer_import_title",
                        &this.i18n.text("transfer_picker_failed"),
                        window,
                        cx,
                    )
                }
            });
        })
        .detach();
    }

    /// Reads `path` in the background and shows what it holds for
    /// confirmation. Nothing is changed.
    pub(crate) fn read_import(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transfer_dialog.reading = true;
        let reading = {
            let path = path.clone();
            cx.background_spawn(async move { transfer::read_import(&path) })
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = reading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.transfer_dialog.reading = false;
                match result {
                    Ok(import) => this.show_transfer(
                        Step::ImportConfirm(Box::new(PendingImport { path, import })),
                        window,
                        cx,
                    ),
                    Err(error) => this.transfer_failed("transfer_import_title", &error, window, cx),
                }
            });
        })
        .detach();
    }

    fn choose_export_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = ["HOME", "USERPROFILE"]
            .into_iter()
            .find_map(std::env::var_os)
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(std::env::temp_dir);
        let chosen = cx.prompt_for_new_path(&directory, Some(EXPORT_FILE_NAME));
        cx.spawn_in(window, async move |this, cx| {
            let result = chosen.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.transfer_dialog.reading = false;
                match result {
                    Ok(Ok(Some(path))) => this.write_export(&path, window, cx),
                    Ok(Ok(None)) => this.close_transfer(window, cx),
                    Ok(Err(error)) => this.transfer_failed(
                        "transfer_export_title",
                        &error.to_string(),
                        window,
                        cx,
                    ),
                    Err(_) => this.transfer_failed(
                        "transfer_export_title",
                        &this.i18n.text("transfer_picker_failed"),
                        window,
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    /// Writes a copy of the saved settings to `path` in the background.
    pub(crate) fn write_export(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_transfer(Step::Working, window, cx);
        let path = path.to_owned();
        // The file is the truth (a settings window saves into it); this
        // window's copy stands in until there is one.
        let stand_in = self.saved.clone();
        let writing = cx.background_spawn(async move {
            let settings = settings_file::load()?.unwrap_or(stand_in);
            transfer::export_to(&path, &settings).map(|()| path)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = writing.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(path) => this.show_transfer(
                    Step::Result {
                        title: this.i18n.text("transfer_export_title"),
                        lines: vec![
                            this.i18n.format(
                                "transfer_export_done",
                                &[("path", &path.display().to_string())],
                            ),
                            this.i18n.text("transfer_export_caution"),
                        ],
                    },
                    window,
                    cx,
                ),
                Err(error) => this.transfer_failed("transfer_export_title", &error, window, cx),
            });
        })
        .detach();
    }

    /// Replaces the settings with the confirmed import and applies them to
    /// this window. No server connects.
    fn apply_import(
        &mut self,
        pending: PendingImport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // An open settings window would save its older form over the import.
        if let Some(handle) = self.settings_window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.show_transfer(Step::Working, window, cx);
        // The backup is synced to disk and the file replaced off the UI
        // thread; the running state follows only when that succeeded.
        let stand_in = self.saved.clone();
        let replacing = cx.background_spawn(async move {
            let current = settings_file::load().ok().flatten().unwrap_or(stand_in);
            let settings = pending.import.into_settings(Some(&current));
            settings_file::replace(&settings).map(|backup| (settings, backup))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = replacing.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((settings, backup)) => {
                    let previous = this.saved.clone();
                    this.apply_settings_change(&previous, settings, true, window, cx);
                    let mut lines = vec![this.i18n.text("transfer_import_done")];
                    lines.push(match backup {
                        Some(backup) => this.i18n.format(
                            "transfer_import_backup",
                            &[("path", &backup.display().to_string())],
                        ),
                        None => this.i18n.text("transfer_import_no_backup"),
                    });
                    lines.push(this.i18n.text("transfer_import_credentials_after"));
                    this.show_transfer(
                        Step::Result {
                            title: this.i18n.text("transfer_import_title"),
                            lines,
                        },
                        window,
                        cx,
                    );
                }
                Err(error) => this.transfer_failed("transfer_import_title", &error, window, cx),
            });
        })
        .detach();
    }

    pub(crate) fn answer_transfer(
        &mut self,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.transfer_dialog.step, None | Some(Step::Working)) {
            return;
        }
        let Some(step) = self.transfer_dialog.step.take() else {
            return;
        };
        match (step, confirmed) {
            (Step::ExportNotice, true) => {
                // The save dialog is open: the notice can no longer be
                // answered, and nothing else starts, until it is done.
                self.transfer_dialog.reading = true;
                self.transfer_dialog.step = Some(Step::Working);
                self.choose_export_path(window, cx);
            }
            (Step::ImportConfirm(pending), true) => self.apply_import(*pending, window, cx),
            _ => self.close_transfer(window, cx),
        }
        cx.notify();
    }

    /// Where each imported server really connects, with a warning for each
    /// unsafe connection setting; the display name never replaces the host.
    fn import_servers(&self, pending: &PendingImport) -> Vec<ServerRow> {
        let servers = &pending.import.settings.servers;
        let unsafe_connection =
            |s: &ServerProfile| !s.use_tls || !s.verify_tls_certificates || s.allow_plaintext_pass;
        let mut rows: Vec<ServerRow> = servers
            .iter()
            .enumerate()
            .filter(|(index, s)| *index < SERVERS_LISTED || unsafe_connection(s))
            .map(|(_, s)| {
                let host = shown(&s.host);
                let port = s.port.to_string();
                let name = shown(&s.display_name);
                let endpoint = if name.is_empty() {
                    self.i18n.format(
                        "transfer_import_server",
                        &[("host", &host), ("port", &port)],
                    )
                } else {
                    self.i18n.format(
                        "transfer_import_server_named",
                        &[("name", &name), ("host", &host), ("port", &port)],
                    )
                };
                let mut warnings = Vec::new();
                if !s.use_tls {
                    warnings.push(self.i18n.text("transfer_import_warn_plain"));
                } else if !s.verify_tls_certificates {
                    warnings.push(self.i18n.text("transfer_import_warn_unverified"));
                }
                if s.allow_plaintext_pass {
                    warnings.push(self.i18n.text("transfer_import_warn_plain_pass"));
                }
                ServerRow { endpoint, warnings }
            })
            .collect();
        let hidden = servers.len() - rows.len();
        if hidden > 0 {
            rows.push(ServerRow {
                endpoint: self
                    .i18n
                    .format("transfer_import_more", &[("count", &hidden.to_string())]),
                warnings: Vec::new(),
            });
        }
        rows
    }

    /// The summary and consequences of a valid import, one paragraph each.
    fn import_lines(&self, pending: &PendingImport) -> Vec<String> {
        let import = &pending.import;
        let settings = &import.settings;
        let startup = settings
            .servers
            .iter()
            .filter(|server| server.connect_on_startup)
            .count();
        let mut lines = vec![
            self.i18n.format(
                "transfer_import_summary",
                &[
                    ("path", &pending.path.display().to_string()),
                    ("servers", &settings.servers.len().to_string()),
                ],
            ),
            self.i18n.text("transfer_import_replaces"),
            self.i18n.text("transfer_import_credentials"),
        ];
        if startup > 0 {
            lines.push(self.i18n.format(
                "transfer_import_startup",
                &[("count", &startup.to_string())],
            ));
        }
        if import.dropped_local_paths {
            lines.push(self.i18n.text("transfer_import_paths"));
        }
        lines.push(self.i18n.text("transfer_import_platform"));
        lines
    }

    pub(crate) fn render_transfer_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let step = self.transfer_dialog.step.as_ref()?;
        // The title, the paragraphs, and the confirm button's label (none
        // when the step only says what happened).
        // The servers an import lists go after the first paragraph.
        let mut servers = Vec::new();
        let (title, lines, confirm_label) = match step {
            Step::ExportNotice => (
                self.i18n.text("transfer_export_title"),
                vec![
                    self.i18n.text("transfer_export_detail"),
                    self.i18n.text("transfer_export_caution"),
                ],
                Some(self.i18n.text("transfer_export_choose")),
            ),
            Step::ImportConfirm(pending) => {
                servers = self.import_servers(pending);
                (
                    self.i18n.text("transfer_import_title"),
                    self.import_lines(pending),
                    Some(self.i18n.text("transfer_import_confirm")),
                )
            }
            Step::Working => (
                String::new(),
                vec![self.i18n.text("transfer_working")],
                None,
            ),
            Step::Result { title, lines } => (title.clone(), lines.clone(), None),
        };
        let text_width = dialog::box_width(BOX_MAX_WIDTH, window) - px(26.);
        let theme = theme::current(cx);
        let mut blocks: Vec<AnyElement> = Vec::new();
        let mut lines = lines.into_iter();
        let paragraph = |line: String| {
            div()
                .w(text_width)
                .text_color(theme.text)
                .child(line)
                .into_any_element()
        };
        blocks.extend(lines.next().map(paragraph));
        blocks.extend(servers.into_iter().map(|row| {
            div()
                .w(text_width)
                .flex()
                .flex_col()
                .text_color(theme.text)
                .child(div().child(row.endpoint))
                .children(row.warnings.into_iter().map(|warning| {
                    div()
                        .text_color(theme.warning)
                        .font_weight(FontWeight::BOLD)
                        .child(format!("⚠ {warning}"))
                }))
                .into_any_element()
        }));
        blocks.extend(lines.map(paragraph));
        let working = matches!(step, Step::Working);
        if self.transfer_dialog.focus_pending {
            self.transfer_dialog.focus_pending = false;
            window.focus(&self.transfer_dialog.cancel);
        }
        let box_width = dialog::box_width(BOX_MAX_WIDTH, window);
        let cancel = dialog::cancel_button(
            "transfer-dialog-cancel",
            self.i18n.text(if confirm_label.is_some() {
                "cancel"
            } else {
                "transfer_close"
            }),
            &self.transfer_dialog.cancel,
            &theme,
            |this, window, cx| this.answer_transfer(false, window, cx),
            cx,
        );
        let has_confirm = confirm_label.is_some();
        let buttons = match confirm_label {
            Some(label) => {
                let confirm = dialog::confirm_button(
                    "transfer-dialog-confirm",
                    label,
                    &self.transfer_dialog.confirm,
                    &theme,
                    |this, window, cx| this.answer_transfer(true, window, cx),
                    cx,
                );
                button_order::row(div().flex().gap_2())
                    .child(confirm)
                    .child(cancel)
            }
            None => div().flex().gap_2().child(cancel),
        };
        Some(
            dialog::overlay("transfer-dialog", &theme)
                .pb(px(16.))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => this.answer_transfer(false, window, cx),
                        // Only the buttons shown are reachable by Tab.
                        "tab" if has_confirm => {
                            let (cancel, confirm) =
                                (&this.transfer_dialog.cancel, &this.transfer_dialog.confirm);
                            let next = if cancel.is_focused(window) {
                                confirm
                            } else {
                                cancel
                            };
                            window.focus(next);
                        }
                        _ => {}
                    }
                    cx.stop_propagation();
                }))
                .child(
                    dialog::dialog_box(&theme)
                        .debug_selector(|| "transfer-dialog-box".into())
                        .max_w(box_width)
                        .max_h_full()
                        .h_auto()
                        .when(!title.is_empty(), |this| {
                            this.child(div().flex_none().font_weight(FontWeight::BOLD).child(title))
                        })
                        // The text scrolls in a small window; the buttons
                        // below it stay in view.
                        .child(
                            div()
                                .id("transfer-dialog-text")
                                .flex()
                                .flex_col()
                                .gap_2()
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .children(blocks),
                        )
                        .when(!working, |this| {
                            this.child(div().flex_none().child(buttons))
                        }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cayenchat_storage::{Language, Settings, ThemeMode, transfer};
    use gpui::TestAppContext;

    use crate::{ChatWindow, settings_file, settings_file::TestFile, settings_with_channels};

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

    fn directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cayenchat-ui-transfer-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn english(mut settings: Settings) -> Settings {
        settings.language = Language::English;
        settings
    }

    #[test]
    fn shown_values_lose_controls_and_bidi_marks_and_are_cut() {
        assert_eq!(
            super::shown("irc\u{202E}ten\r\n\u{7}\u{0}"),
            "irc\u{200B}ten"
        );
        let long = "a".repeat(500);
        let cut = super::shown(&long);
        assert_eq!(cut.chars().count(), super::SHOWN_NAME_MAX + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(
            super::shown(&"b".repeat(super::SHOWN_NAME_MAX))
                .chars()
                .count(),
            64
        );
    }

    #[gpui::test]
    fn the_import_confirmation_lists_real_hosts_and_unsafe_settings(cx: &mut TestAppContext) {
        let current = english(settings_with_channels("#old"));
        let _file = TestFile::with(&current);
        let (chat, cx) = window(&current, cx);

        let mut other = english(settings_with_channels("#new"));
        let template = other.servers[0].clone();
        other.servers.clear();
        for index in 0..40 {
            let mut server = template.clone();
            server.id = format!("s{index}");
            server.use_tls = true;
            server.allow_plaintext_pass = false;
            server.host = format!("irc{index}.example");
            // Only the 30th is unsafe, beyond the listed head.
            server.verify_tls_certificates = index != 30;
            if index == 0 {
                server.display_name = "IRCnet\u{202E}".into();
                server.host = "evil\u{202E}.example\nline".into();
            }
            other.servers.push(server);
        }
        let directory = directory("import-servers");
        let path = directory.join("cayenchat-settings.json");
        transfer::export_to(&path, &other).unwrap();

        chat.update_in(cx, |chat, window, cx| {
            chat.read_import(path.clone(), window, cx)
        });
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            let Some(super::Step::ImportConfirm(pending)) = &chat.transfer_dialog.step else {
                panic!("not confirming");
            };
            let rows = chat.import_servers(pending);
            // 16 listed, the unsafe 31st, and the rest counted.
            assert_eq!(rows.len(), 16 + 1 + 1);
            assert!(rows[0].endpoint.contains("IRCnet"));
            assert!(rows[0].endpoint.contains("evil\u{200B}.example line"));
            assert!(!rows[0].endpoint.contains('\u{202E}'));
            assert!(rows[0].warnings.is_empty());
            assert_eq!(rows[16].warnings.len(), 1);
            assert!(rows[16].endpoint.contains("irc30.example:"));
            assert!(rows[17].endpoint.contains("23"));
        });
    }

    #[gpui::test]
    fn an_import_is_confirmed_first_and_then_replaces_the_servers_without_connecting(
        cx: &mut TestAppContext,
    ) {
        let current = english(settings_with_channels("#old"));
        let file = TestFile::with(&current);
        let (chat, cx) = window(&current, cx);

        let mut other = english(settings_with_channels("#new,-#off"));
        other.servers[0].host = "irc.elsewhere.example".into();
        other.servers[0].connect_on_startup = true;
        other.theme = ThemeMode::Dark;
        let directory = directory("import");
        let path = directory.join("cayenchat-settings.json");
        transfer::export_to(&path, &other).unwrap();

        chat.update_in(cx, |chat, window, cx| {
            chat.read_import(path.clone(), window, cx)
        });
        cx.run_until_parked();
        // Read and checked, but the current settings are untouched.
        chat.read_with(cx, |chat, _| {
            assert!(chat.transfer_dialog.is_confirming_import())
        });
        assert!(cx.debug_bounds("transfer-dialog-confirm").is_some());
        assert_eq!(settings_file::load().unwrap().unwrap().theme, current.theme);
        assert_eq!(
            settings_file::load().unwrap().unwrap().servers[0].host,
            current.servers[0].host
        );

        // Cancelling changes nothing either.
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(false, window, cx)
        });
        assert_eq!(
            settings_file::load().unwrap().unwrap().servers[0].host,
            current.servers[0].host
        );

        chat.update_in(cx, |chat, window, cx| {
            chat.read_import(path.clone(), window, cx)
        });
        cx.run_until_parked();
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(true, window, cx)
        });
        cx.run_until_parked();

        let saved = settings_file::load().unwrap().unwrap();
        assert_eq!(saved.theme, ThemeMode::Dark);
        assert_eq!(saved.servers.len(), 1);
        assert_eq!(saved.servers[0].host, "irc.elsewhere.example");
        assert_eq!(saved.servers[0].channels, "#new,-#off");
        // Not the id of any server this machine had, so none of its stored
        // credentials can be taken for the imported server's.
        assert_ne!(saved.servers[0].id, current.servers[0].id);
        assert_ne!(saved.servers[0].id, other.servers[0].id);
        chat.read_with(cx, |chat, _| {
            let lines = chat.transfer_dialog.result().expect("the result is shown");
            assert!(lines.iter().any(|line| line.contains("backup.json")));
            assert!(chat.sessions.values().all(|s| s.irc.is_none()));
            assert_eq!(chat.sessions.len(), 1);
            assert_eq!(chat.saved.servers[0].id, saved.servers[0].id);
            assert!(
                chat.sessions
                    .values()
                    .all(|session| session.profile_id == saved.servers[0].id)
            );
        });
        drop(file);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn a_bad_file_or_a_failed_write_leaves_the_settings_alone(cx: &mut TestAppContext) {
        let current = english(settings_with_channels("#old"));
        let file = TestFile::with(&current);
        let (chat, cx) = window(&current, cx);
        let directory = directory("refused");

        // A file from a newer version is refused with the current settings
        // reported as unchanged.
        let newer = directory.join("newer.json");
        std::fs::write(&newer, r#"{"version": 999, "servers": []}"#).unwrap();
        chat.update_in(cx, |chat, window, cx| chat.read_import(newer, window, cx));
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            let lines = chat.transfer_dialog.result().expect("an error is shown");
            assert!(lines[0].contains("newer"), "{lines:?}");
            assert!(lines.last().unwrap().contains("not changed"));
        });
        assert_eq!(
            settings_file::load().unwrap().unwrap().servers,
            current.clone().servers
        );

        // A good file whose write fails: the error is shown, nothing changes.
        let good = directory.join("good.json");
        transfer::export_to(&good, &english(settings_with_channels("#new"))).unwrap();
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(false, window, cx)
        });
        chat.update_in(cx, |chat, window, cx| chat.read_import(good, window, cx));
        cx.run_until_parked();
        settings_file::fail_saves(true);
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(true, window, cx)
        });
        cx.run_until_parked();
        settings_file::fail_saves(false);
        chat.read_with(cx, |chat, _| {
            let lines = chat.transfer_dialog.result().expect("an error is shown");
            assert!(lines.last().unwrap().contains("not changed"));
            assert_eq!(chat.saved.servers, current.servers);
        });
        assert_eq!(
            settings_file::load().unwrap().unwrap().servers,
            current.servers
        );
        drop(file);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn an_export_is_a_copy_without_secrets_and_leaves_the_settings_file(cx: &mut TestAppContext) {
        let mut current = english(settings_with_channels("#a"));
        current.servers[0].remember_passwords = true;
        let file = TestFile::with(&current);
        let (chat, cx) = window(&current, cx);
        let directory = directory("export");
        let path = directory.join("cayenchat-settings.json");

        chat.update_in(cx, |chat, window, cx| {
            chat.export_settings(&crate::ExportSettings, window, cx)
        });
        assert!(cx.debug_bounds("transfer-dialog-confirm").is_some());
        chat.update_in(cx, |chat, window, cx| chat.write_export(&path, window, cx));
        // The file is written in the background; until then only a notice shows.
        chat.read_with(cx, |chat, _| assert!(chat.transfer_dialog_busy()));
        assert!(cx.debug_bounds("transfer-dialog-confirm").is_none());
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| assert!(!chat.transfer_dialog_busy()));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(&current.servers[0].host));
        assert!(!text.to_lowercase().contains("password\":"));
        // It reads back as the same settings.
        let again = transfer::read_import(&path).unwrap();
        assert_eq!(again.settings.servers[0].host, current.servers[0].host);
        assert_eq!(settings_file::load().unwrap().unwrap(), current);
        chat.read_with(cx, |chat, _| {
            assert!(chat.transfer_dialog.result().is_some())
        });
        drop(file);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn the_confirmation_keeps_its_buttons_in_a_small_window_and_ignores_answers_while_writing(
        cx: &mut TestAppContext,
    ) {
        let current = english(settings_with_channels("#old"));
        let file = TestFile::with(&current);
        let (chat, cx) = window(&current, cx);
        let mut other = english(settings_with_channels("#new"));
        other.servers[0].connect_on_startup = true;
        other.conversation_log.directory =
            Some(PathBuf::from("/a/very/long/folder/of/another/machine"));
        let directory = directory("small");
        let path = directory.join("cayenchat-settings.json");
        transfer::export_to(&path, &other).unwrap();

        cx.simulate_resize(gpui::size(gpui::px(640.), gpui::px(300.)));
        chat.update_in(cx, |chat, window, cx| chat.read_import(path, window, cx));
        cx.run_until_parked();
        let window_height = cx.update(|window, _| window.viewport_size().height);
        for id in ["transfer-dialog-confirm", "transfer-dialog-cancel"] {
            let bounds = cx.debug_bounds(id).expect("the button is drawn");
            assert!(bounds.bottom() <= window_height, "{id} is below the window");
        }

        // While the file is written, answers and further imports are ignored.
        chat.update_in(cx, |chat, window, cx| {
            chat.answer_transfer(true, window, cx);
            assert!(chat.transfer_dialog_busy());
            chat.answer_transfer(false, window, cx);
            assert!(chat.transfer_dialog_busy());
        });
        assert!(cx.debug_bounds("transfer-dialog-confirm").is_none());
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| assert!(!chat.transfer_dialog_busy()));
        assert_eq!(
            settings_file::load().unwrap().unwrap().servers[0].channels,
            "#new"
        );
        drop(file);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn an_open_save_dialog_holds_the_transfer(cx: &mut TestAppContext) {
        let current = english(settings_with_channels("#old"));
        let file = TestFile::with(&current);
        let (chat, cx) = window(&current, cx);
        chat.update_in(cx, |chat, window, cx| {
            chat.export_settings(&crate::ExportSettings, window, cx);
            chat.answer_transfer(true, window, cx);
            // The save dialog is open: the notice cannot be cancelled and
            // nothing else starts, so its late outcome cannot touch them.
            assert!(chat.transfer_dialog_busy());
            chat.answer_transfer(false, window, cx);
            assert!(chat.transfer_dialog_busy());
            chat.export_settings(&crate::ExportSettings, window, cx);
            chat.import_settings(&crate::ImportSettings, window, cx);
            assert!(matches!(
                chat.transfer_dialog.step,
                Some(super::Step::Working)
            ));
        });
        drop(file);
    }

    #[gpui::test]
    fn a_pending_read_blocks_other_transfers_and_the_settings_window(cx: &mut TestAppContext) {
        let current = english(settings_with_channels("#old"));
        let file = TestFile::with(&current);
        let (chat, cx) = window(&current, cx);
        let other = english(settings_with_channels("#new"));
        let directory = directory("pending");
        let path = directory.join("cayenchat-settings.json");
        transfer::export_to(&path, &other).unwrap();

        // The read has not finished: nothing else may start.
        chat.update_in(cx, |chat, window, cx| {
            chat.read_import(path, window, cx);
            assert!(chat.transfer_dialog_busy());
            chat.import_settings(&crate::ImportSettings, window, cx);
            chat.export_settings(&crate::ExportSettings, window, cx);
            assert!(chat.transfer_dialog.step.is_none());
        });
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            assert!(!chat.transfer_dialog_busy());
            assert!(chat.transfer_dialog.is_confirming_import());
        });
        assert_eq!(
            settings_file::load().unwrap().unwrap().servers[0].channels,
            "#old"
        );
        drop(file);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
