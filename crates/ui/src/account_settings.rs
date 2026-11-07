// SPDX-License-Identifier: GPL-3.0-only
//! Settings tabs for credential storage and IRC image upload accounts.

use cayenchat_storage::{CredentialBackendKind, Secret, SecretKey, Settings, credentials};
use gpui::{prelude::*, *};

use crate::{
    SettingsTab, SettingsWindow, os_text::WindowOsText, secrets, settings_file, settings_theme,
};

/// Every secret this configuration may have stored, for moving them when the
/// credential backend changes.
pub fn all_secret_keys(settings: &Settings) -> Vec<SecretKey> {
    let mut keys = settings.connection_secret_keys();
    keys.extend(
        cayenchat_upload::providers()
            .iter()
            .map(|provider| SecretKey::uploader_token(provider.id)),
    );
    keys
}

/// Why moving the secrets to another store failed.
enum MoveError {
    Store(cayenchat_storage::CredentialError),
    Settings(String),
}

impl From<cayenchat_storage::CredentialError> for MoveError {
    fn from(error: cayenchat_storage::CredentialError) -> Self {
        Self::Store(error)
    }
}

/// Runs on the store thread: copies the secrets from `from` to `to`, records
/// `kind` in the settings file, then deletes the originals (or, if the
/// choice cannot be recorded, the copies). The file is read after the copy,
/// right before it is written (see below). Returns
/// whether the old store was unreadable, and how many secrets moved.
fn move_secrets(
    from: &cayenchat_storage::CredentialStore,
    to: &cayenchat_storage::CredentialStore,
    kind: CredentialBackendKind,
    form: &Settings,
    mut keys: Vec<SecretKey>,
) -> Result<(bool, usize), MoveError> {
    // Moving to the store already in use changes nothing; copying onto
    // itself and deleting the originals would erase the secrets.
    if from.kind() == kind {
        return Ok((false, 0));
    }
    let saved = settings_file::load().map_err(MoveError::Settings)?;
    keys.extend(all_secret_keys(saved.as_ref().unwrap_or(form)));
    keys.sort();
    keys.dedup();
    let copied = credentials::copy_secrets(from, to, &keys)?;
    // The copy can take long (a permission dialog per secret), so the file
    // is read again only now and written at once: a setting another window
    // saved during the copy is kept. Only the gap between this read and the
    // write remains, as for every other read-modify-write of the file.
    let mut saved = settings_file::load()
        .map_err(MoveError::Settings)?
        .unwrap_or_else(|| form.clone());
    saved.credential_backend = kind;
    if let Err(error) = settings_file::save(&saved) {
        copied.remove_copies(to);
        return Err(MoveError::Settings(error));
    }
    let moved = (copied.source_unavailable, copied.count());
    copied.remove_originals(from);
    Ok(moved)
}

impl SettingsWindow {
    /// Checks the system store off the UI thread; D-Bus can take a while to
    /// answer when no Secret Service is running.
    pub(crate) fn probe_system_store(&mut self, cx: &mut Context<Self>) {
        self.system_store = None;
        let probe = secrets::probe_system(cx);
        cx.spawn(async move |this, cx| {
            let result = probe.await.map_err(|error| error.to_string());
            let _ = this.update(cx, |this, cx| {
                this.system_store = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn select_credential_backend(
        &mut self,
        kind: CredentialBackendKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if secrets::store(cx).kind() == kind {
            return;
        }
        if kind == CredentialBackendKind::System {
            let probe = secrets::probe_system(cx);
            cx.spawn(async move |this, cx| {
                let result = probe.await;
                let _ = this.update(cx, |this, cx| match result {
                    Ok(()) => this.switch_credential_backend(kind, cx),
                    Err(error) => {
                        this.system_store = Some(Err(error.to_string()));
                        this.feedback = Some(secrets::error_text(&this.i18n, &error));
                        cx.notify();
                    }
                });
            })
            .detach();
            return;
        }
        // Plaintext storage is never chosen without an explicit confirmation.
        let answer = window.os_prompt(
            PromptLevel::Warning,
            &self.i18n.text("credential_switch_title"),
            Some(&self.i18n.format(
                "credential_switch_detail",
                &[("path", &secrets::local_path_text())],
            )),
            &[
                PromptButton::ok(self.i18n.text("credential_switch_confirm")),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| this.switch_credential_backend(kind, cx));
            }
        })
        .detach();
    }

    /// Moves saved secrets to the new backend and records the choice.
    pub(crate) fn switch_credential_backend(
        &mut self,
        kind: CredentialBackendKind,
        cx: &mut Context<Self>,
    ) {
        // A second switch waits for the first.
        if self.store_busy() {
            return;
        }
        let to = secrets::open(kind, cx);
        let form = self.settings.values.clone();
        let keys_of = all_secret_keys(&form);
        // The whole move is one job of the store thread, so a password saved
        // or removed while it runs is queued before it (and copied) or after
        // it (and goes to the new store), never in between. Copy first,
        // record the choice, and only then delete the originals, so a
        // failure before the choice is saved leaves every secret in the
        // backend still in use.
        self.store_wait(cx);
        let moving = {
            let to = to.clone();
            secrets::run_switching(cx, move |from| {
                let same = from.kind() == kind;
                let moved = move_secrets(from, &to, kind, &form, keys_of);
                let switched = (moved.is_ok() && !same).then_some(to);
                (moved, switched)
            })
        };
        cx.spawn(async move |this, cx| {
            let moved = moving
                .await
                .unwrap_or_else(|| Err(secrets::stopped().into()));
            let _ = this.update(cx, |this, cx| {
                this.store_done(cx);
                this.credential_backend_moved(kind, to, moved, cx)
            });
        })
        .detach();
    }

    /// The store thread moved the secrets (or failed to): shows the outcome
    /// and, once moved, makes the new store the one the UI asks about.
    fn credential_backend_moved(
        &mut self,
        kind: CredentialBackendKind,
        to: cayenchat_storage::CredentialStore,
        moved: Result<(bool, usize), MoveError>,
        cx: &mut Context<Self>,
    ) {
        let (source_unavailable, count) = match moved {
            Ok(moved) => moved,
            Err(error) => {
                self.feedback = Some(match error {
                    MoveError::Store(error) => secrets::error_text(&self.i18n, &error),
                    MoveError::Settings(error) => error,
                });
                cx.notify();
                return;
            }
        };
        self.settings.values.credential_backend = kind;
        // The store thread made `to` current when the job ended; without a
        // worker (tests) the global is the only owner.
        if !cx.has_global::<secrets::Worker>() {
            cx.set_global(secrets::Credentials(to));
        }
        self.feedback = Some(if source_unavailable {
            self.i18n.text("credential_switched_source_unavailable")
        } else {
            self.i18n
                .format("credential_switched", &[("count", &count.to_string())])
        });
        self.show_selected_server(cx);
        self.refresh_upload_account(cx);
    }

    pub(crate) fn render_credential_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let current = secrets::store(cx).kind();
        let option = |kind: CredentialBackendKind, id: &'static str, label: String| {
            let selected = current == kind;
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .child(if selected { "◉" } else { "○" })
                .child(
                    div()
                        .when(selected, |d| d.font_weight(FontWeight::BOLD))
                        .child(label),
                )
                .when(selected, |d| {
                    d.child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(self.i18n.text("credential_in_use")),
                    )
                })
        };
        let status = match &self.system_store {
            None => (
                theme.text_secondary,
                self.i18n.text("credential_system_checking"),
            ),
            Some(Ok(())) => (
                theme.text_secondary,
                self.i18n.text("credential_system_available"),
            ),
            Some(Err(error)) => (
                theme.warning,
                self.i18n
                    .format("credential_system_unavailable", &[("error", error)]),
            ),
        };
        let note = |text: String, color: Rgba| div().ml(px(22.)).text_color(color).child(text);
        panel(cx)
            .child(self.tab_heading("credentials_tab"))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("credentials_intro")),
            )
            .child(
                option(
                    CredentialBackendKind::System,
                    "credential-system",
                    self.i18n.text("credential_system"),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.select_credential_backend(CredentialBackendKind::System, window, cx)
                })),
            )
            .child(note(
                self.i18n.text(secrets::system_store_key()),
                theme.text_secondary,
            ))
            .child(note(status.1, status.0))
            .child(
                option(
                    CredentialBackendKind::LocalFile,
                    "credential-local-file",
                    self.i18n.text("credential_local"),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.select_credential_backend(CredentialBackendKind::LocalFile, window, cx)
                })),
            )
            .child(note(
                self.i18n.format(
                    "credential_local_detail",
                    &[("path", &secrets::local_path_text())],
                ),
                theme.warning,
            ))
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().pt_2().text_color(theme.warning).child(feedback))
            })
    }
}

impl SettingsWindow {
    /// Shows whether the selected provider is noted as having a saved
    /// account credential. The store is not read for this (#642): an upload
    /// corrects the note when the store disagrees.
    pub(crate) fn refresh_upload_account(&mut self, cx: &mut Context<Self>) {
        let upload = &self.settings.values.image_upload;
        self.upload_connected = upload
            .provider
            .as_deref()
            .is_some_and(|id| upload.token_noted(id));
        self.upload_token_open = false;
        cx.notify();
    }

    /// Writes the note for `provider`'s token to the settings file and, once
    /// that worked, to the form and the copy this window saved.
    fn note_upload_token(&mut self, provider: &str, saved: bool) -> Result<(), String> {
        let mut file = settings_file::load()?.unwrap_or_else(|| self.settings.values.clone());
        if file.image_upload.note_token(provider, saved) {
            settings_file::save(&file)?;
        }
        let notes = file.image_upload.token_saved;
        self.settings.values.image_upload.token_saved = notes.clone();
        self.saved.image_upload.token_saved = notes;
        Ok(())
    }

    /// Applies a provider choice at once, like the account buttons do.
    pub(crate) fn select_upload_provider(
        &mut self,
        provider: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let provider = provider.map(str::to_owned);
        let result = settings_file::load().and_then(|saved| {
            let mut saved = saved.unwrap_or_else(|| self.settings.values.clone());
            saved.image_upload.provider = provider.clone();
            settings_file::save(&saved)
        });
        match result {
            Ok(()) => {
                self.settings.values.image_upload.provider = provider.clone();
                self.feedback = None;
                let _ = self
                    .owner
                    .update(cx, |owner, _, _| owner.image_provider = provider);
                self.refresh_upload_account(cx);
            }
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
            }
        }
    }

    pub(crate) fn connect_upload_account(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.settings.values.image_upload.provider.clone() else {
            return;
        };
        let token = self.upload_token.read(cx).text().trim().to_owned();
        if token.is_empty() {
            self.feedback = Some(self.i18n.text("image_token_required"));
            cx.notify();
            return;
        }
        let key = SecretKey::uploader_token(&provider);
        let writing = secrets::run(cx, move |store| store.set(&key, &Secret::new(token)));
        self.store_wait(cx);
        cx.spawn(async move |this, cx| {
            let result = writing.await.unwrap_or_else(|| Err(secrets::stopped()));
            let _ = this.update(cx, |this, cx| {
                this.store_done(cx);
                match result {
                    Ok(()) => {
                        this.upload_token
                            .update(cx, |field, cx| field.set_text("", cx));
                        let noted = this.note_upload_token(&provider, true);
                        this.refresh_upload_account(cx);
                        this.feedback = Some(match noted {
                            Ok(()) => this.i18n.text("image_account_saved"),
                            Err(error) => error,
                        });
                    }
                    Err(error) => this.feedback = Some(secrets::error_text(&this.i18n, &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn disconnect_upload_account(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.settings.values.image_upload.provider.clone() else {
            return;
        };
        let key = SecretKey::uploader_token(&provider);
        let deleting = secrets::run(cx, move |store| store.delete(&key));
        self.store_wait(cx);
        cx.spawn(async move |this, cx| {
            let result = deleting.await.unwrap_or_else(|| Err(secrets::stopped()));
            let _ = this.update(cx, |this, cx| {
                this.store_done(cx);
                match result {
                    Ok(()) => {
                        let noted = this.note_upload_token(&provider, false);
                        this.refresh_upload_account(cx);
                        this.feedback = Some(match noted {
                            Ok(()) => this.i18n.text("image_account_disconnected"),
                            Err(error) => error,
                        });
                    }
                    Err(error) => this.feedback = Some(secrets::error_text(&this.i18n, &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_image_upload_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let selected = self.settings.values.image_upload.provider.clone();
        let heading = self.tab_heading("image_upload_tab");
        let footer = self.reset_footer(SettingsTab::ImageUpload, cx);
        let avatar_return = self.render_avatar_return(cx);
        let label = |key: &str| settings_theme::label_column().child(self.i18n.text(key));
        let button =
            |id: &'static str, text: String| settings_theme::button(id, false, cx).child(text);
        let mut choices = div().flex().gap_1().child(
            button(
                "upload-provider-none",
                self.i18n.text("image_provider_none"),
            )
            .when(selected.is_none(), |d| d.bg(theme.selected))
            .on_click(cx.listener(|this, _, _, cx| this.select_upload_provider(None, cx))),
        );
        for (index, provider) in cayenchat_upload::providers().iter().enumerate() {
            let id = provider.id;
            choices =
                choices.child(
                    div()
                        .id(("upload-provider", index))
                        .px_2()
                        .py_1()
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .when(selected.as_deref() == Some(id), |d| d.bg(theme.selected))
                        .child(provider.name)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_upload_provider(Some(id), cx)
                        })),
                );
        }
        let info = selected.as_deref().and_then(cayenchat_upload::provider);
        let mut panel = panel(cx)
            .child(heading)
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("image_upload_intro")),
            )
            .child(
                div()
                    .text_color(theme.warning)
                    .child(self.i18n.text("image_upload_privacy")),
            )
            .children(avatar_return)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("image_provider"))
                    .child(choices),
            );
        if let Some(info) = info {
            let connected = self.upload_connected;
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("image_account"))
                    .child(self.i18n.text(if connected {
                        "image_account_connected"
                    } else {
                        "image_account_not_connected"
                    }))
                    .child(
                        button(
                            "upload-account-connect",
                            self.i18n.text(if connected {
                                "image_account_reconnect"
                            } else {
                                "image_account_connect"
                            }),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.upload_token_open = true;
                            this.feedback = None;
                            window.focus(&this.upload_token.focus_handle(cx));
                            cx.notify();
                        })),
                    )
                    .when(connected, |d| {
                        d.child(
                            button(
                                "upload-account-disconnect",
                                self.i18n.text("image_account_disconnect"),
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.disconnect_upload_account(cx)),
                            ),
                        )
                    }),
            );
            if self.upload_token_open {
                let setup_url = info.setup_url;
                panel = panel
                    .child(
                        div()
                            .ml(settings_theme::LABEL_INDENT)
                            .text_color(theme.text_secondary)
                            .child(self.i18n.text(&format!("image_setup_{}", info.id))),
                    )
                    .child(
                        div()
                            .id("upload-setup-page")
                            .ml(settings_theme::LABEL_INDENT)
                            .text_color(theme.link)
                            .cursor_pointer()
                            .child(format!(
                                "{} ({setup_url})",
                                self.i18n.text("image_open_setup_page")
                            ))
                            .on_click(move |_, _, cx| cx.open_url(setup_url)),
                    )
                    .child(crate::settings_field(
                        &self.i18n.text("image_token_label"),
                        self.upload_token.clone(),
                    ))
                    .child(
                        crate::button_order::row(
                            div().ml(settings_theme::LABEL_INDENT).flex().gap_2(),
                        )
                        .child(
                            settings_theme::button("upload-token-save", true, cx)
                                .child(self.i18n.text("image_token_save"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.connect_upload_account(cx)),
                                ),
                        )
                        .child(
                            button("upload-token-cancel", self.i18n.text("cancel")).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.upload_token_open = false;
                                    this.upload_token
                                        .update(cx, |field, cx| field.set_text("", cx));
                                    cx.notify();
                                }),
                            ),
                        ),
                    );
            }
        }
        panel
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().pt_2().text_color(theme.warning).child(feedback))
            })
            .child(footer)
    }
}

/// The bordered panel below the settings tabs.
pub(crate) fn panel(_: &App) -> Div {
    div().w_full().flex().flex_col().gap_2()
}
