//! Settings tabs for credential storage and IRC image upload accounts.

use cayenchat_storage::{
    CredentialBackendKind, CredentialStore, Secret, SecretKey, Settings,
    credentials::{self, SystemBackend},
};
use gpui::{prelude::*, *};

use crate::{SettingsWindow, secrets, theme};

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

impl SettingsWindow {
    /// Checks the system store off the UI thread; D-Bus can take a while to
    /// answer when no Secret Service is running.
    pub(crate) fn probe_system_store(&mut self, cx: &mut Context<Self>) {
        self.system_store = None;
        let probe = cx.background_spawn(async { SystemBackend::probe() });
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
            match SystemBackend::probe() {
                Ok(()) => self.switch_credential_backend(kind, cx),
                Err(error) => {
                    self.system_store = Some(Err(error.to_string()));
                    self.feedback = Some(secrets::error_text(&self.i18n, &error));
                    cx.notify();
                }
            }
            return;
        }
        // Plaintext storage is never chosen without an explicit confirmation.
        let answer = window.prompt(
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
    fn switch_credential_backend(&mut self, kind: CredentialBackendKind, cx: &mut Context<Self>) {
        let from = secrets::store(cx);
        let to = CredentialStore::open(kind);
        let mut saved = match cayenchat_storage::load() {
            Ok(saved) => saved.unwrap_or_else(|| self.settings.values.clone()),
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
                return;
            }
        };
        let mut keys = all_secret_keys(&saved);
        keys.extend(all_secret_keys(&self.settings.values));
        keys.sort();
        keys.dedup();
        let report = match credentials::migrate(&from, &to, &keys) {
            Ok(report) => report,
            Err(error) => {
                self.feedback = Some(secrets::error_text(&self.i18n, &error));
                cx.notify();
                return;
            }
        };
        saved.credential_backend = kind;
        if let Err(error) = cayenchat_storage::save(&saved) {
            // Keep using the old backend; the copies in the new one are harmless.
            self.feedback = Some(error);
            cx.notify();
            return;
        }
        self.settings.values.credential_backend = kind;
        cx.set_global(secrets::Credentials(to));
        self.feedback = Some(if report.source_unavailable {
            self.i18n.text("credential_switched_source_unavailable")
        } else {
            self.i18n.format(
                "credential_switched",
                &[("count", &report.moved.to_string())],
            )
        });
        self.show_selected_server(cx);
        self.refresh_upload_account(cx);
    }

    pub(crate) fn render_credential_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = theme::current(cx);
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
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("credentials_tab")),
            )
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
            .when_some(self.feedback.clone(), |d, feedback| {
                d.child(div().pt_2().text_color(theme.warning).child(feedback))
            })
    }
}

impl SettingsWindow {
    /// Re-reads whether the selected provider has a saved account credential.
    pub(crate) fn refresh_upload_account(&mut self, cx: &mut Context<Self>) {
        self.upload_connected = self
            .settings
            .values
            .image_upload
            .provider
            .as_deref()
            .is_some_and(|id| {
                secrets::store(cx)
                    .contains(&SecretKey::uploader_token(id))
                    .unwrap_or(false)
            });
        self.upload_token_open = false;
        cx.notify();
    }

    /// Applies a provider choice at once, like the account buttons do.
    fn select_upload_provider(&mut self, provider: Option<&'static str>, cx: &mut Context<Self>) {
        let provider = provider.map(str::to_owned);
        let result = cayenchat_storage::load().and_then(|saved| {
            let mut saved = saved.unwrap_or_else(|| self.settings.values.clone());
            saved.image_upload.provider = provider.clone();
            cayenchat_storage::save(&saved)
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

    fn connect_upload_account(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.settings.values.image_upload.provider.clone() else {
            return;
        };
        let token = self.upload_token.read(cx).text().trim().to_owned();
        if token.is_empty() {
            self.feedback = Some(self.i18n.text("image_token_required"));
            cx.notify();
            return;
        }
        match secrets::store(cx).set(&SecretKey::uploader_token(&provider), &Secret::new(token)) {
            Ok(()) => {
                self.upload_token
                    .update(cx, |field, cx| field.set_text("", cx));
                self.refresh_upload_account(cx);
                self.feedback = Some(self.i18n.text("image_account_saved"));
            }
            Err(error) => self.feedback = Some(secrets::error_text(&self.i18n, &error)),
        }
        cx.notify();
    }

    fn disconnect_upload_account(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.settings.values.image_upload.provider.clone() else {
            return;
        };
        match secrets::store(cx).delete(&SecretKey::uploader_token(&provider)) {
            Ok(()) => {
                self.refresh_upload_account(cx);
                self.feedback = Some(self.i18n.text("image_account_disconnected"));
            }
            Err(error) => self.feedback = Some(secrets::error_text(&self.i18n, &error)),
        }
        cx.notify();
    }

    pub(crate) fn render_image_upload_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = theme::current(cx);
        let selected = self.settings.values.image_upload.provider.clone();
        let label = |key: &str| div().w(px(150.)).flex_shrink_0().child(self.i18n.text(key));
        let button = |id: &'static str, text: String| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(text)
        };
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
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("image_upload_tab")),
            )
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
                            .ml(px(158.))
                            .text_color(theme.text_secondary)
                            .child(self.i18n.text(&format!("image_setup_{}", info.id))),
                    )
                    .child(
                        div()
                            .id("upload-setup-page")
                            .ml(px(158.))
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
                        div()
                            .ml(px(158.))
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("upload-token-save")
                                    .px_3()
                                    .py_1()
                                    .bg(theme.selected)
                                    .cursor_pointer()
                                    .child(self.i18n.text("image_token_save"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.connect_upload_account(cx)
                                    })),
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
        panel.when_some(self.feedback.clone(), |d, feedback| {
            d.child(div().pt_2().text_color(theme.warning).child(feedback))
        })
    }
}

/// The bordered panel below the settings tabs.
pub(crate) fn panel(cx: &App) -> Div {
    let theme = theme::current(cx);
    div()
        .w(px(680.))
        .p_4()
        .mb_4()
        .bg(theme.surface)
        .border_1()
        .border_t_0()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap_2()
}
