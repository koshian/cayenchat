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

use cayenchat_app::ConnectionStatus;
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
    /// The SASL private key (ECDSA) is not there. No dialog can supply it:
    /// the connection is not started and the user is pointed to the
    /// settings.
    pub key: bool,
}

impl Missing {
    /// A password the dialog can ask for is missing.
    pub(crate) fn any(self) -> bool {
        self.server || self.sasl
    }

    /// The SASL password or key is missing.
    pub(crate) fn sasl_secret(self) -> bool {
        self.sasl || self.key
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
    let sasl_missing = config
        .sasl
        .as_ref()
        .is_some_and(|credentials| credentials.password.is_empty());
    let keyed = uses_key(config);
    Missing {
        server: server_needed && config.server_password.is_none(),
        sasl: sasl_missing && !keyed,
        key: sasl_missing && keyed,
    }
}

/// Whether the SASL mechanism of `config` signs with a key (its "password"
/// is then the stored key, which no dialog asks for).
pub(crate) fn uses_key(config: &ConnectionConfig) -> bool {
    config.sasl.as_ref().is_some_and(|credentials| {
        credentials.mechanism == cayenchat_irc_core::SaslMechanism::EcdsaNist256pChallenge
    })
}

/// Whether `config` still connects the way `profile` says: same server,
/// port, TLS and SASL account and mechanism. The profile's stored secrets
/// belong to that identity; a `config` that differs (the settings changed
/// while the session kept its old connection) must not be given them, or a
/// password or key could go to another server or into another mechanism.
pub(crate) fn matches_profile(profile: &ServerProfile, config: &ConnectionConfig) -> bool {
    let sasl = match (&config.sasl, profile.sasl_enabled) {
        (None, false) => true,
        (Some(credentials), true) => {
            credentials.username == profile.sasl_username
                && credentials.mechanism == crate::sasl_mechanism(profile.sasl_mechanism)
        }
        _ => false,
    };
    sasl && config.host == profile.host
        && config.port == profile.port
        && config.use_tls == profile.use_tls
}

/// The passwords `config` sends that the profile does not note as saved:
/// the ones a refusal of the other password would otherwise lose.
pub(crate) fn unsaved_secrets(
    profile: Option<&ServerProfile>,
    config: &ConnectionConfig,
) -> (Option<Secret>, Option<Secret>) {
    let server = config
        .server_password
        .as_deref()
        .filter(|password| !password.is_empty())
        .filter(|_| !profile.is_some_and(ServerProfile::server_password_saved))
        .map(Secret::new);
    // A key is always in the store; nothing of it is kept for a dialog.
    let sasl = config
        .sasl
        .as_ref()
        .filter(|_| !uses_key(config))
        .map(|credentials| credentials.password.as_str())
        .filter(|password| !password.is_empty())
        .filter(|_| !profile.is_some_and(ServerProfile::sasl_password_saved))
        .map(Secret::new);
    (server, sasl)
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

/// Why the last connection ended in a refusal the dialog answers (instead of
/// an error notice).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// 464. `sent`: a server password had been sent, so it was wrong.
    Server { sent: bool },
    /// SASL authentication failed; the SASL password was wrong.
    Sasl,
}

impl Refusal {
    /// The refusal a `Refused` event's reason stands for, if the dialog
    /// answers it. `server_password_sent`: the connection sent a password.
    pub(crate) fn of(reason: &str, server_password_sent: bool) -> Option<Self> {
        if reason == cayenchat_irc_core::PASSWORD_REFUSED {
            Some(Self::Server {
                sent: server_password_sent,
            })
        } else if reason == cayenchat_irc_core::SASL_FAILED {
            Some(Self::Sasl)
        } else {
            None
        }
    }

    /// The password of the refusal was given and wrong.
    fn rejected(self) -> bool {
        matches!(self, Self::Server { sent: true } | Self::Sasl)
    }
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
    /// The SASL account, beside the SASL password, to be corrected with it.
    pub(crate) account: Option<Entity<TextInput>>,
    /// The server refused the password given before.
    pub(crate) rejected: bool,
    /// SASL authentication failed: the account or its password is wrong.
    sasl_failed: bool,
    pub(crate) error: Option<String>,
    focus_pending: bool,
}

impl CredentialPrompt {
    /// The line saying what is wanted: a refused password or SASL login, the
    /// server password (alone or beside the SASL one), or passwords in
    /// general.
    pub(crate) fn title_key(&self) -> &'static str {
        if self.sasl_failed {
            "credential_prompt_sasl_rejected"
        } else if self.rejected {
            "credential_prompt_rejected"
        } else if self.server.is_some() {
            "credential_prompt_server"
        } else {
            "credential_prompt_title"
        }
    }

    /// The fields in the order they are filled in.
    pub(crate) fn inputs(&self) -> impl Iterator<Item = &Entity<TextInput>> {
        self.server
            .iter()
            .map(|field| &field.input)
            .chain(self.account.iter())
            .chain(self.sasl.iter().map(|field| &field.input))
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
        if missing.key {
            return Err(self.i18n.text("sasl_key_required"));
        }
        if missing.any() {
            self.open_credential_prompt(network, config, Resume::Fresh, missing, None, cx);
            return Ok(());
        }
        self.apply_connection(network, config, window, cx)
    }

    /// Starts the reconnect of `network` with the passwords the store has,
    /// read when the connection starts, and asks for the rest.
    ///
    /// `refused`: why the last connection ended in a refusal the dialog
    /// answers. The stored password that was refused (the server password
    /// for 464, the SASL password for a SASL failure) is not used again; the
    /// dialog asks for it.
    pub(crate) fn reconnect_with_stored_secrets(
        &mut self,
        network: NetworkId,
        config: ConnectionConfig,
        refused: Option<Refusal>,
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
        // The settings changed after this connection was made: the stored
        // secrets are those of the new settings. Reconnecting with them
        // would mix the two, so the user connects again with the new ones.
        if profile
            .as_ref()
            .is_some_and(|profile| !matches_profile(profile, &config))
        {
            let text = self.i18n.text("reconnect_settings_changed");
            self.state.append_server_message(network, text.clone());
            self.raise_error(Some(network), text, crate::notices::Origin::Background, cx);
            return;
        }
        let store = secrets::store(cx);
        let read = match profile {
            None => (Ok(None), Ok(None)),
            Some(profile) => {
                // The store is read only for what the profile notes as saved.
                let server_refused = matches!(refused, Some(Refusal::Server { .. }));
                let sasl_refused = refused == Some(Refusal::Sasl);
                let server = if server_refused || !profile.server_password_saved() {
                    Ok(None)
                } else {
                    store.get(&profile.server_password_key())
                };
                let sasl = if profile.sasl_enabled && profile.sasl_secret_saved() && !sasl_refused {
                    store.get(&profile.sasl_secret_key())
                } else {
                    Ok(None)
                };
                let found =
                    |result: &Result<Option<Secret>, _>| result.as_ref().ok().map(Option::is_some);
                let noted = (
                    found(&server).filter(|_| !server_refused),
                    found(&sasl).filter(|_| profile.sasl_enabled && !sasl_refused),
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
        refused: Option<Refusal>,
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
        // A stored key that cannot sign counts as missing.
        let sasl = sasl.filter(|secret| {
            !uses_key(&config) || cayenchat_irc_core::sasl_key_is_valid(secret.expose())
        });
        let server_needed = matches!(refused, Some(Refusal::Server { .. }))
            || self
                .sessions
                .get(&network)
                .is_some_and(|session| session.server_password_used);
        let missing = fill(&mut config, server.as_ref(), sasl.as_ref(), server_needed);
        if missing.key {
            // No key to sign with: say so instead of connecting.
            let text = self.i18n.text("sasl_key_required");
            self.state.append_server_message(network, text.clone());
            self.raise_error(Some(network), text, crate::notices::Origin::Background, cx);
            return;
        }
        if missing.any() {
            let resume = Resume::Reconnect { announce };
            self.open_credential_prompt(network, config, resume, missing, refused, cx);
        } else {
            self.start_reconnect_with(network, config, announce, cx);
        }
    }

    /// The server refused registration with 464 or SASL failed. The dialog
    /// is the notice: the password is asked for, so a server that wants one
    /// need not be told in advance. The refusal stays in the server log.
    ///
    /// Only the refused password is asked for. The other one is read from
    /// the store if it is saved there; an unsaved one is the value the
    /// refused connection sent, kept until now and handed on to the dialog's
    /// configuration, which drops it when the dialog closes.
    pub(crate) fn password_refused(
        &mut self,
        network: NetworkId,
        refusal: Refusal,
        cx: &mut Context<Self>,
    ) {
        let (server, sasl) = self
            .sessions
            .get_mut(&network)
            .map(|session| std::mem::take(&mut session.sent_secrets))
            .unwrap_or_default();
        if let Some(mut config) = self.reconnect_config(network) {
            match refusal {
                Refusal::Server { .. } => apply_secrets(&mut config, None, sasl.as_ref()),
                Refusal::Sasl => apply_secrets(&mut config, server.as_ref(), None),
            }
            self.reconnect_with_stored_secrets(network, config, Some(refusal), false, cx);
        }
    }

    fn open_credential_prompt(
        &mut self,
        network: NetworkId,
        config: ConnectionConfig,
        resume: Resume,
        missing: Missing,
        refused: Option<Refusal>,
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
        // The account is shown with its configured name, to be corrected
        // with the password (it is no secret).
        let account = missing.sasl.then(|| {
            let name = config
                .sasl
                .as_ref()
                .map(|credentials| credentials.username.clone())
                .unwrap_or_default();
            cx.new(|cx| {
                TextInput::new_field(
                    &self.i18n.text("sasl_account_placeholder"),
                    &name,
                    false,
                    cx,
                )
            })
        });
        self.credential_prompts.push(CredentialPrompt {
            network,
            config,
            resume,
            server,
            sasl,
            account,
            rejected: refused.is_some_and(Refusal::rejected),
            sasl_failed: refused == Some(Refusal::Sasl),
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
        {
            let inputs: Vec<_> = prompt.inputs().collect();
            if let Some(at) = inputs
                .iter()
                .position(|input| input.read(cx).focus_handle(cx).is_focused(window))
                && let Some(next) = inputs[at + 1..]
                    .iter()
                    .find(|input| input.read(cx).text().is_empty())
            {
                window.focus(&next.focus_handle(cx));
                return;
            }
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
        let account = prompt
            .account
            .as_ref()
            .map(|input| input.read(cx).text().trim().to_owned());
        if [&server, &sasl]
            .into_iter()
            .flatten()
            .any(|secret| secret.is_empty())
            || account.as_ref().is_some_and(String::is_empty)
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
        if let (Some(account), Some(credentials)) = (&account, config.sasl.as_mut()) {
            credentials.username.clone_from(account);
        }
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
        // The prompt holds the configuration of when it opened. If the
        // settings changed since, what was typed would go to the old server
        // under the new settings' name.
        let stale = self.sessions.get(&network).is_some_and(|session| {
            self.saved
                .profile(&session.profile_id)
                .is_some_and(|profile| {
                    !matches_profile(profile, &self.credential_prompts[index].config)
                })
        });
        if stale {
            self.close_credential_prompt(index, window, cx);
            let text = self.i18n.text("reconnect_settings_changed");
            self.raise_error(Some(network), text, crate::notices::Origin::User, cx);
            return;
        }
        self.close_credential_prompt(index, window, cx);
        if let Some(account) = &account {
            self.save_sasl_account(network, account);
        }
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

    /// The SASL account corrected in the dialog is written to the profile
    /// (and the copy reconnects start from), as if it had been changed in the
    /// settings window, which follows the file. It is no secret.
    fn save_sasl_account(&mut self, network: NetworkId, account: &str) {
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        if let Some(credentials) = session
            .active_config
            .as_mut()
            .and_then(|config| config.sasl.as_mut())
        {
            credentials.username = account.to_owned();
        }
        let mut settings = self.saved.clone();
        let Some(profile) = settings.profile_mut(&session.profile_id) else {
            return;
        };
        if profile.sasl_username != account {
            profile.sasl_username = account.to_owned();
            if settings_file::save(&settings).is_ok() {
                self.saved = settings;
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
            let cancelled = self.i18n.text("event_password_cancelled");
            // A start shown as connecting (D068, Launch) ends here.
            let waiting = self
                .sessions
                .get(&network)
                .is_some_and(|session| session.irc.is_none());
            if waiting && self.state.status(network) == Some(&ConnectionStatus::Connecting) {
                self.state
                    .set_status(network, ConnectionStatus::Disconnected(cancelled.clone()));
            }
            self.state.append_server_message(network, cancelled);
        }
        cx.notify();
    }

    fn close_credential_prompt(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut prompt = self.credential_prompts.remove(index);
        // Neither what was typed nor a password carried over from the
        // refused connection stays.
        prompt.config.forget_secrets();
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
        // The first field still empty gets the focus: the account is filled in.
        if let Some(index) = pending
            && !typing
            && let Some(input) = self.credential_prompts[index]
                .inputs()
                .find(|input| input.read(cx).text().is_empty())
                .or_else(|| self.credential_prompts[index].inputs().next())
        {
            window.focus(&input.focus_handle(cx));
        }
        let theme = theme::current(cx);
        let border = theme.border;
        let mut dialog = dialog::prompt_list("credential-prompts", origin, window, &theme);
        for (index, prompt) in self.credential_prompts.iter().enumerate() {
            let network = prompt.network;
            let server = self.prompt_server_label(network);
            // One line says what is wanted: a refused password, the server
            // password alone, or passwords in general.
            let title = self.i18n.text(prompt.title_key());
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
                if slot == 1
                    && let Some(account) = &prompt.account
                {
                    fields = fields
                        .child(div().child(self.i18n.text("credential_prompt_account")))
                        .child(account.clone());
                }
                let remember = field.remember;
                fields = fields
                    .child(div().child(label))
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
                        .child(
                            div()
                                .when(prompt.rejected, |d| d.text_color(theme.warning))
                                .child(title),
                        )
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
    let before = (profile.server_password_saved(), profile.sasl_secret_saved());
    if let Some(found) = server.filter(|found| *found != before.0) {
        profile.set_server_password_saved(found);
    }
    if let Some(found) = sasl.filter(|found| *found != before.1) {
        profile.set_sasl_secret_saved(found);
    }
    before != (profile.server_password_saved(), profile.sasl_secret_saved())
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
    use super::{
        Missing, apply_secrets, fill, matches_profile, note_found, unsaved_secrets,
        while_incomplete,
    };
    use cayenchat_irc_core::{ConnectionConfig, SaslCredentials};
    use cayenchat_storage::Secret;

    fn config(sasl: bool) -> ConnectionConfig {
        let mut config = ConnectionConfig::tls("irc.example".into(), "nick".into(), Vec::new());
        if sasl {
            config.sasl = Some(SaslCredentials {
                mechanism: Default::default(),
                username: "acct".into(),
                password: String::new(),
            });
        }
        config
    }

    /// The profile `config(sasl)` was made from.
    fn profile(sasl: bool) -> cayenchat_storage::ServerProfile {
        let config = config(sasl);
        let mut profile = cayenchat_storage::ServerProfile::default();
        profile.host = config.host;
        profile.port = config.port;
        profile.use_tls = config.use_tls;
        profile.sasl_enabled = sasl;
        profile.sasl_username = "acct".into();
        profile.sasl_mechanism = cayenchat_storage::SaslMechanism::Plain;
        profile
    }

    #[test]
    fn a_configuration_matches_the_profile_it_was_made_from() {
        assert!(matches_profile(&profile(true), &config(true)));
        assert!(matches_profile(&profile(false), &config(false)));
    }

    #[test]
    fn a_changed_profile_does_not_match_the_old_configuration() {
        let old = config(true);
        let mut changed = profile(true);
        changed.sasl_mechanism = cayenchat_storage::SaslMechanism::EcdsaNist256pChallenge;
        assert!(!matches_profile(&changed, &old));
        let mut changed = profile(true);
        changed.sasl_mechanism = cayenchat_storage::SaslMechanism::ScramSha256;
        assert!(!matches_profile(&changed, &old));
        let mut changed = profile(true);
        changed.sasl_username = "other".into();
        assert!(!matches_profile(&changed, &old));
        let mut changed = profile(true);
        changed.host = "other.example".into();
        assert!(!matches_profile(&changed, &old));
        let mut changed = profile(true);
        changed.port += 1;
        assert!(!matches_profile(&changed, &old));
        let mut changed = profile(true);
        changed.use_tls = !changed.use_tls;
        assert!(!matches_profile(&changed, &old));
        assert!(!matches_profile(&profile(false), &old));
        assert!(!matches_profile(&profile(true), &config(false)));
    }

    #[test]
    fn a_sasl_configuration_without_its_password_is_missing_it() {
        let mut config = config(true);
        let missing = fill(&mut config, None, None, false);
        assert_eq!(
            missing,
            Missing {
                server: false,
                sasl: true,
                key: false
            }
        );
        let missing = fill(&mut config, None, Some(&Secret::new("pw")), false);
        assert!(!missing.any());
        assert_eq!(config.sasl.unwrap().password, "pw");
    }

    #[test]
    fn a_missing_key_is_no_case_for_the_password_dialog() {
        let mut config = config(true);
        config.sasl.as_mut().unwrap().mechanism =
            cayenchat_irc_core::SaslMechanism::EcdsaNist256pChallenge;
        let missing = fill(&mut config, None, None, false);
        assert_eq!(
            missing,
            Missing {
                server: false,
                sasl: false,
                key: true
            }
        );
        assert!(!missing.any() && missing.sasl_secret());
        // With the key there, nothing is missing and nothing of it is kept
        // for a dialog after a refusal.
        let key = cayenchat_irc_core::generate_sasl_key().unwrap();
        let missing = fill(&mut config, None, Some(&Secret::new(key.as_str())), false);
        assert!(!missing.sasl_secret());
        assert_eq!(unsaved_secrets(None, &config), (None, None));
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
