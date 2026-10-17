// SPDX-License-Identifier: GPL-3.0-only
//! Channel keys (`+k`): asking for one when a JOIN is refused with 475, and
//! keeping it in the credential store once the JOIN worked.
//!
//! A JOIN we sent is tracked in [`ServerSession::key_joins`] until the
//! server answers it. Only a 475 for a tracked JOIN opens the key dialog, so
//! another server's or another channel's refusal never does. The key typed
//! in the dialog is saved only when our own JOIN is reported back, never
//! when it was merely sent or refused. A key never reaches the settings
//! file, a log, the diagnostics or the input history.

use std::collections::BTreeMap;

use cayenchat_irc_core::{ConnectionConfig, valid_channel_key};
use cayenchat_model::{NetworkId, names::CaseMapping};
use cayenchat_storage::{CredentialStore, Secret, SecretKey, ServerProfile};
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, button_order, input::TextInput, secrets, session::ServerSession, settings_theme,
    theme,
};

/// JOINs tracked per server, so a long auto-join list does not grow it
/// without bound.
const MAX_TRACKED: usize = 256;
/// Channels one `/join` may name for a stored key to be looked up for each.
const MAX_JOIN_CHANNELS: usize = 32;

/// A JOIN waiting for the server's answer.
pub struct KeyJoin {
    /// The channel as it was requested. Names are compared under the
    /// server's current casemapping, which may change after the JOIN was
    /// sent (CASEMAPPING arrives in the first ISUPPORT).
    pub channel: String,
    /// The key typed in the dialog for this attempt, if it came from there.
    pub typed: Option<Secret>,
    /// Whether to save `typed` once the JOIN worked.
    pub remember: bool,
    /// A saved key was used (now or in an earlier attempt of this dialog)
    /// and the server refused it; it is replaced or removed by the outcome.
    pub stale: bool,
}

/// The dialog asking for the key of one channel on one server.
pub struct KeyPrompt {
    pub network: NetworkId,
    pub channel: String,
    stale: bool,
    rejected: bool,
    pub(crate) remember: bool,
    input: Entity<TextInput>,
    error: Option<String>,
    focus_pending: bool,
}

/// The channels of a plain `/join #a,#b` (no key given), or `None` for any
/// other form of the command, which is sent as typed.
pub fn plain_join_channels(text: &str) -> Option<Vec<String>> {
    let mut words = text.strip_prefix('/')?.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("join") {
        return None;
    }
    let channels = words.next()?;
    if words.next().is_some() {
        return None;
    }
    let channels: Vec<String> = channels.split(',').map(str::to_owned).collect();
    (channels.len() <= MAX_JOIN_CHANNELS
        && channels
            .iter()
            .all(|name| cayenchat_irc_core::valid_channel(name)))
    .then_some(channels)
}

/// Gives `config` the saved keys of the auto-join channels of `profile`. A
/// store that cannot be read, or a saved key the connection cannot send,
/// just means no key: the 475 then asks for it.
pub fn use_stored_keys(
    config: &mut ConnectionConfig,
    profile: &ServerProfile,
    store: &CredentialStore,
) {
    config.channel_keys = stored_channel_keys(profile, store);
    if config.validate().is_err() {
        config.channel_keys.clear();
    }
}

fn stored_channel_keys(
    profile: &ServerProfile,
    store: &CredentialStore,
) -> BTreeMap<String, String> {
    profile
        .channels()
        .into_iter()
        .filter_map(|channel| {
            let key = store
                .get(&SecretKey::channel_key(&profile.id, &channel))
                .ok()??;
            valid_channel_key(key.expose()).then(|| (channel, key.expose().to_owned()))
        })
        .collect()
}

impl ChatWindow {
    /// Sends `JOIN` for each channel with its saved key, if there is one.
    /// The keys are read off the UI thread; the JOINs go out when they are in.
    pub(crate) fn join_channels(
        &mut self,
        network: NetworkId,
        channels: &[String],
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let profile = self
            .sessions
            .get(&network)
            .map(|session| session.profile_id.clone())
            .ok_or_else(|| self.i18n.text("not_connected"))?;
        self.registered_connection(network)?;
        let store = secrets::store(cx);
        let channels = channels.to_vec();
        let reading = cx.background_spawn({
            let profile = profile.clone();
            async move {
                channels
                    .into_iter()
                    .map(|channel| {
                        let saved = store
                            .get(&SecretKey::channel_key(&profile, &channel))
                            .ok()
                            .flatten()
                            .filter(|key| valid_channel_key(key.expose()));
                        (channel, saved)
                    })
                    .collect::<Vec<_>>()
            }
        });
        cx.spawn(async move |this, cx| {
            let keyed = reading.await;
            let _ = this.update(cx, |this, cx| this.send_joins(network, &profile, keyed, cx));
        })
        .detach();
        Ok(())
    }

    /// Sends the JOINs whose keys were read, unless the connection went away
    /// or became another server's meanwhile.
    fn send_joins(
        &mut self,
        network: NetworkId,
        profile: &str,
        keyed: Vec<(String, Option<Secret>)>,
        cx: &mut Context<Self>,
    ) {
        if self
            .sessions
            .get(&network)
            .is_none_or(|session| session.profile_id != profile)
        {
            return;
        }
        for (channel, saved) in keyed {
            let sent = self.registered_connection(network).and_then(|connection| {
                connection.join_channel(&channel, saved.as_ref().map(Secret::expose))
            });
            match sent {
                Ok(()) => self.note_join(network, &channel, saved.is_some()),
                Err(error) => {
                    self.set_feedback(Some(error));
                    break;
                }
            }
        }
        cx.notify();
    }

    /// Remembers a JOIN just sent: a refusal of a channel with no conversation
    /// yet can be shown, and a 475 for it asks for the key. `stale` says a
    /// saved key was sent.
    pub(crate) fn note_join(&mut self, network: NetworkId, channel: &str, stale: bool) {
        let casemapping = self.state.casemapping(network);
        let folded = casemapping.fold(channel);
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        if session.pending_joins.len() >= 32 {
            session.pending_joins.clear();
        }
        session.pending_joins.insert(folded);
        track(session, KeyJoin::untyped(channel, stale));
    }

    /// A new connection registered: its auto-join JOINs (with the saved keys
    /// it was configured with) are about to be answered.
    pub(crate) fn start_key_tracking(&mut self, network: NetworkId) {
        self.key_prompts.retain(|prompt| prompt.network != network);
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        session.key_joins.clear();
        let Some(config) = session.active_config.as_ref() else {
            return;
        };
        let joins: Vec<KeyJoin> = config
            .channels
            .iter()
            .map(|channel| KeyJoin::untyped(channel, config.channel_keys.contains_key(channel)))
            .collect();
        for join in joins {
            track(session, join);
        }
    }

    /// The connection ended: its JOINs will never be answered.
    pub(crate) fn end_key_tracking(&mut self, network: NetworkId) {
        self.key_prompts.retain(|prompt| prompt.network != network);
        if let Some(session) = self.sessions.get_mut(&network) {
            session.key_joins.clear();
        }
    }

    /// Takes the tracked JOIN of `channel`, if there is one.
    fn take_key_join(&mut self, network: NetworkId, channel: &str) -> Option<KeyJoin> {
        let casemapping = self.state.casemapping(network);
        let folded = casemapping.fold(channel);
        let session = self.sessions.get_mut(&network)?;
        let index = session
            .key_joins
            .iter()
            .position(|join| casemapping.fold(&join.channel) == folded)?;
        Some(session.key_joins.remove(index))
    }

    fn key_prompt_index(&self, network: NetworkId, channel: &str) -> Option<usize> {
        let casemapping = self.state.casemapping(network);
        self.key_prompts.iter().position(|prompt| {
            prompt.network == network && same_channel(casemapping, &prompt.channel, channel)
        })
    }

    /// The server refused our JOIN of `channel`. Only 475 for a JOIN we
    /// sent asks for a key; 471, 473, 474 and the like cannot be cured by one.
    pub(crate) fn key_join_refused(
        &mut self,
        network: NetworkId,
        channel: &str,
        bad_key: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(join) = self.take_key_join(network, channel) else {
            return;
        };
        if !bad_key {
            return;
        }
        let rejected = join.typed.is_some();
        if let Some(index) = self.key_prompt_index(network, channel) {
            let prompt = &mut self.key_prompts[index];
            prompt.stale |= join.stale;
            prompt.rejected = rejected;
            prompt.focus_pending = true;
            return;
        }
        let placeholder = self.i18n.text("key_prompt_placeholder");
        let input = cx.new(|cx| TextInput::new_field(&placeholder, "", true, cx));
        self.key_prompts.push(KeyPrompt {
            network,
            channel: channel.to_owned(),
            stale: join.stale,
            rejected,
            // The choice made for the attempt that was refused stands.
            remember: if rejected { join.remember } else { true },
            input,
            error: None,
            focus_pending: true,
        });
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        cx.notify();
    }

    /// We are in `channel`: the JOIN worked, so the key typed for it may now
    /// be saved, or a saved key that stopped working removed. The store is
    /// written off the UI thread.
    pub(crate) fn key_join_succeeded(
        &mut self,
        network: NetworkId,
        channel: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .sessions
            .get(&network)
            .map(|session| session.profile_id.clone())
        else {
            return;
        };
        let Some(join) = self.take_key_join(network, channel) else {
            return;
        };
        let change = match (join.typed, join.remember) {
            (Some(typed), true) => StoreChange::Save(typed),
            (Some(_), false) if join.stale => StoreChange::Forget,
            _ => return,
        };
        let store = secrets::store(cx);
        let key = SecretKey::channel_key(&profile, channel);
        let channel = channel.to_owned();
        let remembered = match &change {
            StoreChange::Save(typed) => Some(typed.expose().to_owned()),
            StoreChange::Forget => None,
        };
        let writing = cx.background_spawn(async move {
            match change {
                StoreChange::Save(typed) => store
                    .set(&key, &typed)
                    .map_err(|error| ("channel_key_save_failed", error)),
                StoreChange::Forget => store
                    .delete(&key)
                    .map_err(|error| ("channel_key_forget_failed", error)),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = writing.await;
            let _ = this.update(cx, |this, cx| {
                this.key_store_written(network, &profile, &channel, remembered, result, cx)
            });
        })
        .detach();
    }

    /// The store answered a save or removal.
    fn key_store_written(
        &mut self,
        network: NetworkId,
        profile: &str,
        channel: &str,
        key: Option<String>,
        result: Result<(), (&'static str, cayenchat_storage::CredentialError)>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                // Only the connection the key was confirmed on is updated.
                if self
                    .sessions
                    .get(&network)
                    .is_some_and(|session| session.profile_id == profile)
                {
                    self.sync_config_key(network, channel, key);
                }
            }
            Err((message, error)) => {
                let text = self.i18n.format(
                    message,
                    &[
                        (
                            "channel",
                            &cayenchat_model::display::neutralize_bidi(channel),
                        ),
                        ("error", &secrets::error_text(&self.i18n, &error)),
                    ],
                );
                self.state.append_server_message(network, text.clone());
                self.set_feedback(Some(text));
            }
        }
        cx.notify();
    }

    /// Gives the running connection's configuration the saved keys of its
    /// profile's auto-join channels, read off the UI thread, so reconnects
    /// use them.
    pub(crate) fn refresh_stored_keys(
        &mut self,
        network: NetworkId,
        profile: &ServerProfile,
        cx: &mut Context<Self>,
    ) {
        let store = secrets::store(cx);
        let profile = profile.clone();
        let reading = cx.background_spawn({
            let profile = profile.clone();
            async move { stored_channel_keys(&profile, &store) }
        });
        cx.spawn(async move |this, cx| {
            let keys = reading.await;
            let _ = this.update(cx, |this, _| {
                let Some(session) = this.sessions.get_mut(&network) else {
                    return;
                };
                if session.profile_id != profile.id {
                    return;
                }
                if let Some(config) = session.active_config.as_mut() {
                    let before = std::mem::replace(&mut config.channel_keys, keys);
                    if config.validate().is_err() {
                        config.channel_keys = before;
                        config.channel_keys.clear();
                    }
                }
            });
        })
        .detach();
    }

    /// Keeps the configuration reconnects reuse in step with the store.
    fn sync_config_key(&mut self, network: NetworkId, channel: &str, key: Option<String>) {
        let casemapping = self.state.casemapping(network);
        let Some(config) = self
            .sessions
            .get_mut(&network)
            .and_then(|session| session.active_config.as_mut())
        else {
            return;
        };
        let Some(name) = config
            .channels
            .iter()
            .find(|name| same_channel(casemapping, name, channel))
            .cloned()
        else {
            return;
        };
        match key {
            Some(key) => config.channel_keys.insert(name, key),
            None => config.channel_keys.remove(&name),
        };
    }

    /// The server whose key field has focus, with the channel.
    pub(crate) fn focused_key_prompt(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<(NetworkId, String)> {
        self.key_prompts
            .iter()
            .find(|prompt| prompt.input.read(cx).focus_handle(cx).is_focused(window))
            .map(|prompt| (prompt.network, prompt.channel.clone()))
    }

    pub(crate) fn submit_key_prompt(
        &mut self,
        network: NetworkId,
        channel: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.key_prompt_index(network, channel) else {
            return;
        };
        let prompt = &self.key_prompts[index];
        if prompt.input.read(cx).is_composing() {
            return;
        }
        let key = Secret::new(prompt.input.read(cx).text().trim());
        let channel = prompt.channel.clone();
        let (remember, stale) = (prompt.remember, prompt.stale);
        let sent = if key.is_empty() {
            Err(self.i18n.text("key_prompt_required"))
        } else if !valid_channel_key(key.expose()) {
            Err(self.i18n.text("key_prompt_invalid"))
        } else {
            self.registered_connection(network)
                .and_then(|connection| connection.join_channel(&channel, Some(key.expose())))
        };
        match sent {
            Ok(()) => {
                let casemapping = self.state.casemapping(network);
                if let Some(session) = self.sessions.get_mut(&network) {
                    if session.pending_joins.len() >= 32 {
                        session.pending_joins.clear();
                    }
                    session.pending_joins.insert(casemapping.fold(&channel));
                    track(
                        session,
                        KeyJoin {
                            channel,
                            typed: Some(key),
                            remember,
                            stale,
                        },
                    );
                }
                self.close_key_prompt(index, window, cx);
            }
            Err(error) => self.key_prompts[index].error = Some(error),
        }
        cx.notify();
    }

    /// Gives up: nothing is joined or saved.
    pub(crate) fn cancel_key_prompt(
        &mut self,
        network: NetworkId,
        channel: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.key_prompt_index(network, channel) {
            self.close_key_prompt(index, window, cx);
        }
        cx.notify();
    }

    fn close_key_prompt(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.key_prompts.remove(index);
        match self.key_prompts.first() {
            Some(next) => window.focus(&next.input.focus_handle(cx)),
            None => window.focus(&self.inputs[&self.state.selection()].focus_handle(cx)),
        }
    }

    /// One centered dialog with a row per channel waiting for a key.
    pub(crate) fn render_key_prompts(
        &mut self,
        origin: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.key_prompts.is_empty() {
            return None;
        }
        let typing = self.focused_key_prompt(window, cx).is_some();
        let pending = self
            .key_prompts
            .iter()
            .position(|prompt| prompt.focus_pending);
        for prompt in &mut self.key_prompts {
            prompt.focus_pending = false;
        }
        if let Some(index) = pending
            && !typing
        {
            window.focus(&self.key_prompts[index].input.focus_handle(cx));
        }
        let theme = theme::current(cx);
        let border = theme.border;
        let viewport = window.viewport_size();
        let width = px(360.);
        let mut dialog = div()
            .id("key-prompts")
            .absolute()
            .left(((viewport.width - width) / 2.).max(px(0.)) - origin.x)
            .top(px(60.).min(viewport.height / 4.) - origin.y)
            .w(width)
            .p_2()
            .bg(theme.surface)
            .border_1()
            .border_color(border)
            .shadow_md()
            .flex()
            .flex_col()
            .gap_2();
        for (index, prompt) in self.key_prompts.iter().enumerate() {
            let network = prompt.network;
            let channel = prompt.channel.clone();
            let server = self.prompt_server_label(network);
            let title = self.i18n.format(
                "key_prompt_title",
                &[(
                    "channel",
                    &cayenchat_model::display::neutralize_bidi(&prompt.channel),
                )],
            );
            let hint = if prompt.rejected {
                Some(self.i18n.text("key_prompt_rejected"))
            } else if prompt.stale {
                Some(self.i18n.text("key_prompt_stale"))
            } else {
                None
            };
            let remember = prompt.remember;
            let (submit_channel, cancel_channel, toggle_channel) =
                (channel.clone(), channel.clone(), channel);
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
                        .child(title)
                        .when_some(hint, |d, hint| {
                            d.child(div().text_color(theme.warning).child(hint))
                        })
                        .child(prompt.input.clone())
                        .child(
                            div()
                                .id(("key-prompt-remember", index))
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .child(settings_theme::checkbox(remember, true, cx))
                                .child(self.i18n.text("key_prompt_remember"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(prompt) = this.key_prompts.iter_mut().find(|p| {
                                        p.network == network && p.channel == toggle_channel
                                    }) {
                                        prompt.remember = !prompt.remember;
                                    }
                                    cx.notify();
                                })),
                        )
                        .when_some(prompt.error.clone(), |d, error| {
                            d.child(div().text_color(theme.warning).child(error))
                        })
                        .child(
                            button_order::row(div().flex().gap_2())
                                .child(
                                    div()
                                        .id(("key-prompt-submit", index))
                                        .px_2()
                                        .py_1()
                                        .bg(theme.selected)
                                        .cursor_pointer()
                                        .child(self.i18n.text("key_prompt_submit"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.submit_key_prompt(
                                                network,
                                                &submit_channel,
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                                .child(
                                    div()
                                        .id(("key-prompt-cancel", index))
                                        .px_2()
                                        .py_1()
                                        .border_1()
                                        .border_color(border)
                                        .cursor_pointer()
                                        .child(self.i18n.text("key_prompt_cancel"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.cancel_key_prompt(
                                                network,
                                                &cancel_channel,
                                                window,
                                                cx,
                                            )
                                        })),
                                ),
                        ),
                );
        }
        Some(dialog.into_any_element())
    }
}

impl KeyPrompt {
    pub fn input(&self) -> &Entity<TextInput> {
        &self.input
    }
}

impl KeyJoin {
    fn untyped(channel: &str, stale: bool) -> Self {
        Self {
            channel: channel.to_owned(),
            typed: None,
            remember: false,
            stale,
        }
    }
}

/// Tracks `join`, replacing an earlier one for the same channel. Only names
/// differing in ASCII case count as the same here: `#x[` and `#x{` are one
/// channel under the default mapping but two under `ascii`, which the server
/// may announce later.
fn track(session: &mut ServerSession, join: KeyJoin) {
    let same = session
        .key_joins
        .iter()
        .position(|other| other.channel.eq_ignore_ascii_case(&join.channel));
    match same {
        Some(index) => session.key_joins[index] = join,
        None if session.key_joins.len() < MAX_TRACKED => session.key_joins.push(join),
        None => {}
    }
}

fn same_channel(casemapping: CaseMapping, a: &str, b: &str) -> bool {
    casemapping.fold(a) == casemapping.fold(b)
}

/// What the store is asked to do once a JOIN worked.
enum StoreChange {
    Save(Secret),
    Forget,
}
