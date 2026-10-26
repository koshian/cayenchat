// SPDX-License-Identifier: GPL-3.0-only
//! Passwords a connection needs but the credential store does not hold (#635).
//!
//! A connection never keeps an unsaved password for later: the copy of its
//! configuration that reconnects use has the passwords removed
//! ([`ConnectionConfig::forget_secrets`]). A reconnect, a connection started
//! at launch and a connection the server answered with 464 read what the
//! store has and ask for the rest in one dialog per server. Nothing typed
//! there is stored unless its box is checked (off by default), and Cancel
//! drops the attempt and keeps the server from retrying by itself, so a
//! reconnect loop never opens the dialog again and again.
//!
//! What a connection needs is known from its settings (SASL on) and from
//! what happened: a server password is asked for only when the previous
//! connection used one or the server refused registration with 464, never
//! unconditionally.

use cayenchat_irc_core::ConnectionConfig;
use cayenchat_model::NetworkId;
use cayenchat_storage::{Secret, SecretKey, ServerProfile};
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, button_order, dialog, input::TextInput, secrets, settings_file, settings_theme,
    theme,
};

/// Which passwords a configuration still lacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Missing {
    pub server: bool,
    pub sasl: bool,
}

impl Missing {
    pub(crate) fn any(self) -> bool {
        self.server || self.sasl
    }
}

/// Puts the non-empty passwords into `config`.
pub(crate) fn apply_secrets(
    config: &mut ConnectionConfig,
    server: Option<&Secret>,
    sasl: Option<&Secret>,
) {
    if let Some(password) = server.filter(|value| !value.is_empty()) {
        config.server_password = Some(password.expose().to_owned());
    }
    if let (Some(credentials), Some(password)) =
        (config.sasl.as_mut(), sasl.filter(|value| !value.is_empty()))
    {
        credentials.password = password.expose().to_owned();
    }
}

/// Puts the passwords into `config` and says which ones it still lacks.
/// `server_needed`: the server password is asked for if it is missing.
pub(crate) fn fill(
    config: &mut ConnectionConfig,
    server: Option<&Secret>,
    sasl: Option<&Secret>,
    server_needed: bool,
) -> Missing {
    apply_secrets(config, server, sasl);
    Missing {
        server: server_needed && config.server_password.is_none(),
        sasl: config
            .sasl
            .as_ref()
            .is_some_and(|credentials| credentials.password.is_empty()),
    }
}

/// Runs `check` on `config` as if the SASL password it lacks were there, so
/// a validation that needs a complete configuration can run before the
/// dialog. The placeholder is removed again.
pub(crate) fn while_incomplete<R>(
    config: &mut ConnectionConfig,
    check: impl FnOnce(&mut ConnectionConfig) -> R,
) -> R {
    let placeholder = config
        .sasl
        .as_mut()
        .filter(|credentials| credentials.password.is_empty())
        .map(|credentials| credentials.password = "-".into())
        .is_some();
    let result = check(config);
    if placeholder && let Some(credentials) = config.sasl.as_mut() {
        credentials.password.clear();
    }
    result
}

/// How the connection continues once the dialog is answered.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resume {
    /// A connection that did not exist in this run: a fresh session.
    Fresh,
    /// The server's last connection, again. `announce`: a failure to start
    /// it is shown, because the user asked for it.
    Reconnect { announce: bool },
}

pub(crate) struct PasswordField {
    pub(crate) input: Entity<TextInput>,
    /// "Save this password"; off until the user checks it.
    pub(crate) remember: bool,
}

/// The dialog asking for the passwords of one server.
pub(crate) struct CredentialPrompt {
    pub network: NetworkId,
    /// The configuration to connect with, without passwords.
    config: ConnectionConfig,
    resume: Resume,
    pub(crate) server: Option<PasswordField>,
    pub(crate) sasl: Option<PasswordField>,
    /// The server refused the password given before.
    pub(crate) rejected: bool,
    pub(crate) error: Option<String>,
    focus_pending: bool,
}

impl CredentialPrompt {
    pub(crate) fn inputs(&self) -> impl Iterator<Item = &Entity<TextInput>> {
        self.server
            .iter()
            .chain(self.sasl.iter())
            .map(|field| &field.input)
    }
}

impl ChatWindow {
    /// Connects a server that has no connection in this run from its saved
    /// settings, asking first for the passwords it lacks.
    pub(crate) fn connect_fresh(
        &mut self,
        network: NetworkId,
        config: ConnectionConfig,
        missing: Missing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if missing.any() {
            self.open_credential_prompt(network, config, Resume::Fresh, missing, false, cx);
            return Ok(());
        }
        self.apply_connection(network, config, window, cx)
    }

    /// Starts the reconnect of `network` with the passwords the store has,
    /// read when the connection starts, and asks for the rest.
    ///
    /// `refused`: the server rejected registration with 464 (`Some(true)`
    /// when a password had been sent, `Some(false)` when none was). The
    /// stored server password is then not used again; the dialog asks for it.
    pub(crate) fn reconnect_with_stored_secrets(
        &mut self,
        network: NetworkId,
        config: ConnectionConfig,
        refused: Option<bool>,
        announce: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        session.retry_pending = false;
        // A newer attempt, Disconnect or Connect makes this one stale.
        session.retry_token += 1;
        let token = session.retry_token;
        let profile = self.saved.profile(&session.profile_id).cloned();
        let store = secrets::store(cx);
        let read = match profile {
            None => (Ok(None), Ok(None)),
            Some(profile) => {
                // The store is read only for what the profile notes as saved.
                let server = if refused.is_some() || !profile.server_password_saved() {
                    Ok(None)
                } else {
                    store.get(&profile.server_password_key())
                };
                let sasl = if profile.sasl_enabled && profile.sasl_password_saved() {
                    store.get(&profile.sasl_password_key())
                } else {
                    Ok(None)
                };
                let found =
                    |result: &Result<Option<Secret>, _>| result.as_ref().ok().map(Option::is_some);
                let noted = (
                    found(&server).filter(|_| refused.is_none()),
                    found(&sasl).filter(|_| profile.sasl_enabled),
                );
                self.note_stored_passwords(&profile.id, noted.0, noted.1);
                (server, sasl)
            }
        };
        self.stored_secrets_read(network, token, config, refused, announce, read, cx);
    }

    /// A read of the credential store showed which passwords of the profile
    /// it holds (`None`: not read). The note in the settings file, which
    /// lets the settings window show the state without reading the store,
    /// is corrected when it differs. A failed write leaves it: it is only a
    /// note, and the next read corrects it again.
    pub(crate) fn note_stored_passwords(
        &mut self,
        profile_id: &str,
        server: Option<bool>,
        sasl: Option<bool>,
    ) {
        let mut settings = self.saved.clone();
        let Some(profile) = settings.profile_mut(profile_id) else {
            return;
        };
        if note_found(profile, server, sasl) && settings_file::save(&settings).is_ok() {
            self.saved = settings;
        }
    }

    #[allow(clippy::too_many_arguments)] // One attempt's state and the read result.
    fn stored_secrets_read(
        &mut self,
        network: NetworkId,
        token: u64,
        mut config: ConnectionConfig,
        refused: Option<bool>,
        announce: bool,
        (server, sasl): (
            Result<Option<Secret>, cayenchat_storage::CredentialError>,
            Result<Option<Secret>, cayenchat_storage::CredentialError>,
        ),
        cx: &mut Context<Self>,
    ) {
        let current = self
            .sessions
            .get(&network)
            .is_some_and(|session| session.retry_token == token && !session.manual_disconnect);
        if !current {
            return;
        }
        // An unreadable store is the same as an empty one here: the dialog
        // takes the password, and the failure is reported.
        let mut read = |result: Result<Option<Secret>, _>| match result {
            Ok(secret) => secret,
            Err(error) => {
                let text = secrets::error_text(&self.i18n, &error);
                self.state.append_server_message(network, text);
                None
            }
        };
        let (server, sasl) = (read(server), read(sasl));
        let server_needed = refused.is_some()
            || self
                .sessions
                .get(&network)
                .is_some_and(|session| session.server_password_used);
        let missing = fill(&mut config, server.as_ref(), sasl.as_ref(), server_needed);
        if missing.any() {
            let rejected = refused == Some(true);
            let resume = Resume::Reconnect { announce };
            self.open_credential_prompt(network, config, resume, missing, rejected, cx);
        } else {
            self.start_reconnect_with(network, config, announce, cx);
        }
    }

    /// The server refused registration with 464. The server password is
    /// asked for, so a server that wants one need not be told in advance;
    /// `rejected` says the one that was sent was wrong.
    pub(crate) fn password_refused(
        &mut self,
        network: NetworkId,
        rejected: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(config) = self.reconnect_config(network) {
            self.reconnect_with_stored_secrets(network, config, Some(rejected), false, cx);
        }
    }

    fn open_credential_prompt(
        &mut self,
        network: NetworkId,
        config: ConnectionConfig,
        resume: Resume,
        missing: Missing,
        rejected: bool,
        cx: &mut Context<Self>,
    ) {
        self.credential_prompts
            .retain(|prompt| prompt.network != network);
        let field = |placeholder: &str, cx: &mut Context<Self>| PasswordField {
            input: cx.new(|cx| TextInput::new_field(placeholder, "", true, cx)),
            remember: false,
        };
        let server = missing
            .server
            .then(|| field(&self.i18n.text("server_password"), cx));
        let sasl = missing
            .sasl
            .then(|| field(&self.i18n.text("sasl_password"), cx));
        self.credential_prompts.push(CredentialPrompt {
            network,
            config,
            resume,
            server,
            sasl,
            rejected,
            error: None,
            focus_pending: true,
        });
        self.state
            .append_server_message(network, self.i18n.text("event_password_wanted"));
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        cx.notify();
    }

    fn credential_prompt_index(&self, network: NetworkId) -> Option<usize> {
        self.credential_prompts
            .iter()
            .position(|prompt| prompt.network == network)
    }

    /// The server whose password field has focus.
    pub(crate) fn focused_credential_prompt(&self, window: &Window, cx: &App) -> Option<NetworkId> {
        self.credential_prompts
            .iter()
            .find(|prompt| {
                prompt
                    .inputs()
                    .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
            })
            .map(|prompt| prompt.network)
    }

    /// Enter in a password field: moves to the next empty one, or connects.
    pub(crate) fn enter_credential_prompt(
        &mut self,
        network: NetworkId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = self
            .credential_prompt_index(network)
            .map(|index| &self.credential_prompts[index])
            && let [first, second] = prompt.inputs().collect::<Vec<_>>()[..]
            && first.read(cx).focus_handle(cx).is_focused(window)
            && second.read(cx).text().is_empty()
        {
            window.focus(&second.focus_handle(cx));
            return;
        }
        self.submit_credential_prompt(network, window, cx);
    }

    pub(crate) fn submit_credential_prompt(
        &mut self,
        network: NetworkId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.credential_prompt_index(network) else {
            return;
        };
        let prompt = &self.credential_prompts[index];
        if prompt.inputs().any(|input| input.read(cx).is_composing()) {
            return;
        }
        let typed = |field: &Option<PasswordField>| {
            field
                .as_ref()
                .map(|field| Secret::new(field.input.read(cx).text()))
        };
        let (server, sasl) = (typed(&prompt.server), typed(&prompt.sasl));
        if [&server, &sasl]
            .into_iter()
            .flatten()
            .any(|secret| secret.is_empty())
        {
            self.credential_prompts[index].error =
                Some(self.i18n.text("credential_prompt_required"));
            self.credential_prompts[index].focus_pending = true;
            cx.notify();
            return;
        }
        let prompt = &self.credential_prompts[index];
        // A SASL password not asked for, because the store had it, is read
        // by the reconnect that follows; here the fields cover the rest.
        let mut config = prompt.config.clone();
        apply_secrets(&mut config, server.as_ref(), sasl.as_ref());
        let remember = |field: &Option<PasswordField>| field.as_ref().is_some_and(|f| f.remember);
        let saves = (
            remember(&prompt.server).then(|| server.clone()).flatten(),
            remember(&prompt.sasl).then(|| sasl.clone()).flatten(),
        );
        let resume = prompt.resume;
        let validated =
            crate::connect_auth::while_incomplete(&mut config, |config| config.validate());
        if let Err(error) = validated {
            self.credential_prompts[index].error = Some(error);
            cx.notify();
            return;
        }
        self.close_credential_prompt(index, window, cx);
        self.save_prompted_secrets(network, saves, cx);
        match resume {
            Resume::Fresh => {
                if let Err(error) = self.apply_connection(network, config, window, cx) {
                    self.raise_error(Some(network), error, crate::notices::Origin::User, cx);
                }
            }
            Resume::Reconnect { announce } => {
                self.start_reconnect_with(network, config, announce, cx);
            }
        }
    }

    /// Saves the passwords whose box was checked. A
    /// failure is reported; the connection goes on, and a saved password
    /// that was there is not touched by a failed write.
    fn save_prompted_secrets(
        &mut self,
        network: NetworkId,
        saves: (Option<Secret>, Option<Secret>),
        cx: &mut Context<Self>,
    ) {
        let (server, sasl) = saves;
        if server.is_none() && sasl.is_none() {
            return;
        }
        let Some(profile) = self
            .sessions
            .get(&network)
            .and_then(|session| self.saved.profile(&session.profile_id))
            .cloned()
        else {
            return;
        };
        let store = secrets::store(cx);
        let mut first = Ok(());
        let mut saved = (None, None);
        for (key, secret) in saved_keys(&profile, server, sasl) {
            let written = store.set(&key, &secret);
            if written.is_ok() {
                if key == profile.server_password_key() {
                    saved.0 = Some(true);
                } else {
                    saved.1 = Some(true);
                }
            }
            if let Err(error) = written {
                first = first.and(Err(error));
            }
        }
        self.note_stored_passwords(&profile.id, saved.0, saved.1);
        if let Err(error) = first {
            let text = self.i18n.format(
                "credential_save_failed",
                &[("error", &secrets::error_text(&self.i18n, &error))],
            );
            self.state.append_server_message(network, text.clone());
            self.raise_error(Some(network), text, crate::notices::Origin::User, cx);
        }
    }

    /// Gives up: the attempt is dropped and the server does not try again
    /// by itself until the user connects it.
    pub(crate) fn cancel_credential_prompt(
        &mut self,
        network: NetworkId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.credential_prompt_index(network) {
            self.close_credential_prompt(index, window, cx);
            if let Some(session) = self.sessions.get_mut(&network) {
                session.manual_disconnect = true;
                session.retry_pending = false;
                session.retry_token += 1;
            }
            self.state
                .append_server_message(network, self.i18n.text("event_password_cancelled"));
        }
        cx.notify();
    }

    fn close_credential_prompt(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = self.credential_prompts.remove(index);
        // What was typed does not stay in the field.
        for input in prompt.inputs() {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        match self
            .credential_prompts
            .first()
            .and_then(|p| p.inputs().next())
        {
            Some(next) => window.focus(&next.focus_handle(cx)),
            None => window.focus(&self.inputs[&self.state.selection()].focus_handle(cx)),
        }
    }

    /// One centered dialog with a block per server waiting for passwords.
    pub(crate) fn render_credential_prompts(
        &mut self,
        origin: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.credential_prompts.is_empty() {
            return None;
        }
        let typing = self.focused_credential_prompt(window, cx).is_some();
        let pending = self
            .credential_prompts
            .iter()
            .position(|prompt| prompt.focus_pending);
        for prompt in &mut self.credential_prompts {
            prompt.focus_pending = false;
        }
        if let Some(index) = pending
            && !typing
            && let Some(input) = self.credential_prompts[index].inputs().next()
        {
            window.focus(&input.focus_handle(cx));
        }
        let theme = theme::current(cx);
        let border = theme.border;
        let mut dialog = dialog::prompt_list("credential-prompts", origin, window, &theme);
        for (index, prompt) in self.credential_prompts.iter().enumerate() {
            let network = prompt.network;
            let server = self.prompt_server_label(network);
            let hint = prompt
                .rejected
                .then(|| self.i18n.text("credential_prompt_rejected"));
            let mut fields = div().flex().flex_col().gap_1();
            for (slot, field, label) in [
                (
                    0usize,
                    prompt.server.as_ref(),
                    self.i18n.text("server_password"),
                ),
                (
                    1usize,
                    prompt.sasl.as_ref(),
                    self.i18n.text("sasl_password"),
                ),
            ] {
                let Some(field) = field else { continue };
                let heading = if slot == 1 {
                    let account = prompt
                        .config
                        .sasl
                        .as_ref()
                        .map(|credentials| credentials.username.as_str())
                        .unwrap_or_default();
                    self.i18n.format(
                        "credential_prompt_sasl",
                        &[(
                            "account",
                            &cayenchat_model::display::neutralize_bidi(account),
                        )],
                    )
                } else {
                    label
                };
                let remember = field.remember;
                fields = fields
                    .child(div().child(heading))
                    .child(field.input.clone())
                    .child(
                        div()
                            .id(("credential-prompt-save", index * 2 + slot))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .child(settings_theme::checkbox(remember, true, cx))
                            .child(self.i18n.text("credential_prompt_save"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(prompt) = this
                                    .credential_prompts
                                    .iter_mut()
                                    .find(|prompt| prompt.network == network)
                                    && let Some(field) = if slot == 0 {
                                        prompt.server.as_mut()
                                    } else {
                                        prompt.sasl.as_mut()
                                    }
                                {
                                    field.remember = !field.remember;
                                }
                                cx.notify();
                            })),
                    );
            }
            dialog = dialog
                .when(index > 0, |d| {
                    d.child(div().border_t_1().border_color(theme.separator))
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().font_weight(FontWeight::BOLD).child(server))
                        .child(self.i18n.text("credential_prompt_title"))
                        .when_some(hint, |d, hint| {
                            d.child(div().text_color(theme.warning).child(hint))
                        })
                        .child(fields)
                        .when_some(prompt.error.clone(), |d, error| {
                            d.child(div().text_color(theme.warning).child(error))
                        })
                        .child(
                            button_order::row(div().flex().gap_2())
                                .child(
                                    div()
                                        .id(("credential-prompt-submit", index))
                                        .px_2()
                                        .py_1()
                                        .bg(theme.selected)
                                        .cursor_pointer()
                                        .child(self.i18n.text("credential_prompt_submit"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.submit_credential_prompt(network, window, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .id(("credential-prompt-cancel", index))
                                        .px_2()
                                        .py_1()
                                        .border_1()
                                        .border_color(border)
                                        .cursor_pointer()
                                        .child(self.i18n.text("cancel"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.cancel_credential_prompt(network, window, cx)
                                        })),
                                ),
                        ),
                );
        }
        Some(dialog.into_any_element())
    }
}

/// The store keys and values to write for the checked boxes.
/// Sets the profile's notes of stored passwords to what a read found.
/// Returns whether anything changed.
pub(crate) fn note_found(
    profile: &mut ServerProfile,
    server: Option<bool>,
    sasl: Option<bool>,
) -> bool {
    let before = (
        profile.server_password_saved(),
        profile.sasl_password_saved(),
    );
    if let Some(found) = server.filter(|found| *found != before.0) {
        profile.set_server_password_saved(found);
    }
    if let Some(found) = sasl.filter(|found| *found != before.1) {
        profile.set_sasl_password_saved(found);
    }
    before
        != (
            profile.server_password_saved(),
            profile.sasl_password_saved(),
        )
}

fn saved_keys(
    profile: &ServerProfile,
    server: Option<Secret>,
    sasl: Option<Secret>,
) -> Vec<(SecretKey, Secret)> {
    let mut keys = Vec::new();
    if let Some(secret) = server {
        keys.push((profile.server_password_key(), secret));
    }
    if let Some(secret) = sasl {
        keys.push((profile.sasl_password_key(), secret));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::{Missing, apply_secrets, fill, note_found, while_incomplete};
    use cayenchat_irc_core::{ConnectionConfig, SaslCredentials};
    use cayenchat_storage::Secret;

    fn config(sasl: bool) -> ConnectionConfig {
        let mut config = ConnectionConfig::tls("irc.example".into(), "nick".into(), Vec::new());
        if sasl {
            config.sasl = Some(SaslCredentials {
                username: "acct".into(),
                password: String::new(),
            });
        }
        config
    }

    #[test]
    fn a_sasl_configuration_without_its_password_is_missing_it() {
        let mut config = config(true);
        let missing = fill(&mut config, None, None, false);
        assert_eq!(
            missing,
            Missing {
                server: false,
                sasl: true
            }
        );
        let missing = fill(&mut config, None, Some(&Secret::new("pw")), false);
        assert!(!missing.any());
        assert_eq!(config.sasl.unwrap().password, "pw");
    }

    #[test]
    fn the_server_password_is_missing_only_when_it_is_needed() {
        let mut config = config(false);
        assert!(!fill(&mut config, None, None, false).any());
        assert!(fill(&mut config, None, None, true).server);
        assert!(!fill(&mut config, Some(&Secret::new("pw")), None, true).any());
        assert_eq!(config.server_password.as_deref(), Some("pw"));
    }

    #[test]
    fn an_empty_secret_does_not_count_as_given() {
        let mut config = config(true);
        let missing = fill(
            &mut config,
            Some(&Secret::new("")),
            Some(&Secret::new("")),
            true,
        );
        assert!(missing.server && missing.sasl);
        assert!(config.server_password.is_none());
    }

    #[test]
    fn the_placeholder_for_validation_does_not_stay() {
        let mut config = config(true);
        assert!(config.validate().is_err());
        let result = while_incomplete(&mut config, |config| config.validate());
        assert!(result.is_ok());
        assert!(config.sasl.unwrap().password.is_empty());
    }

    #[test]
    fn forgetting_secrets_keeps_the_account() {
        let mut config = config(true);
        apply_secrets(
            &mut config,
            Some(&Secret::new("a")),
            Some(&Secret::new("b")),
        );
        config.forget_secrets();
        assert!(config.server_password.is_none());
        let sasl = config.sasl.unwrap();
        assert_eq!(
            (sasl.username.as_str(), sasl.password.as_str()),
            ("acct", "")
        );
    }

    #[test]
    fn a_read_corrects_the_note_of_stored_passwords() {
        let mut settings = cayenchat_storage::Settings::default();
        let mut profile = settings.add_server("irc.example").clone();
        // Nothing read, nothing changed.
        assert!(!note_found(&mut profile, None, None));
        // The store holds a password the note lacks.
        assert!(note_found(&mut profile, Some(true), Some(false)));
        assert!(profile.server_password_saved() && !profile.sasl_password_saved());
        // Reading the same again changes nothing.
        assert!(!note_found(&mut profile, Some(true), Some(false)));
        // A noted password that is gone is cleared; the other stays.
        assert!(note_found(&mut profile, Some(false), None));
        assert!(!profile.server_password_saved());
        // The old choice counts as both and changes one at a time.
        profile.remember_passwords = true;
        assert!(profile.server_password_saved() && profile.sasl_password_saved());
        assert!(note_found(&mut profile, None, Some(false)));
        assert!(profile.server_password_saved() && !profile.sasl_password_saved());
    }
}
