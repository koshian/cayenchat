// SPDX-License-Identifier: GPL-3.0-only
//! The "Leave #channel?" question of a double click on a joined channel row
//! (#451), drawn in the chat window.
//!
//! Like the quit question, the keyboard focus starts on Cancel, so Enter,
//! Space and Escape cancel; leaving needs a click, Enter in the reason field,
//! or Tab to the confirm button and Enter. The reason field starts empty and
//! shows the server's configured part message as its placeholder; an empty
//! field leaves with that default, and a typed reason is used for this one
//! PART only.

use std::time::{Duration, Instant};

use cayenchat_model::{ConversationId, NetworkId, display::neutralize_bidi};
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, FocusNextField, FocusPreviousField, button_order, dialog, input::TextInput, theme,
};

const BOX_WIDTH: Pixels = px(340.);
/// Longest wait for the server's answer to a PART before the row can be
/// double-clicked again.
const PENDING_PART: Duration = Duration::from_secs(10);
/// Most channels remembered as being left, so the map stays bounded.
const PENDING_PARTS_LIMIT: usize = 32;

pub(crate) struct PartPrompt {
    network: NetworkId,
    conversation: ConversationId,
    pub(crate) channel: String,
    server: String,
    pub(crate) input: Entity<TextInput>,
    cancel: FocusHandle,
    confirm: FocusHandle,
    previous: Option<FocusHandle>,
    focus_pending: bool,
    pub(crate) error: Option<String>,
}

impl PartPrompt {
    pub(crate) fn network(&self) -> NetworkId {
        self.network
    }
}

/// Channels with a PART sent and no answer yet, by case-folded name.
pub(crate) fn expire_pending(parts: &mut std::collections::HashMap<String, Instant>) {
    parts.retain(|_, sent| sent.elapsed() < PENDING_PART);
}

impl ChatWindow {
    /// A double click on a channel row: joins a channel we are not in, asks
    /// before leaving one we are in. Does nothing for a private conversation,
    /// a server that is not registered, or a JOIN/PART still unanswered.
    pub(crate) fn activate_channel_row(
        &mut self,
        id: ConversationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conversation) = self.state.conversation(id) else {
            return;
        };
        if conversation.is_private() || self.part_prompt.is_some() {
            return;
        }
        let (network, name) = (conversation.network, conversation.name.clone());
        if self.registered_connection(network).is_err() {
            return;
        }
        let folded = self.state.casemapping(network).fold(&name);
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        expire_pending(&mut session.pending_parts);
        if session.pending_parts.contains_key(&folded) {
            return;
        }
        if self.state.is_active_channel(id) {
            self.open_part_prompt(id, window, cx);
        } else if !session.pending_joins.contains(&folded) {
            self.join_or_part(network, &name, true, cx);
        }
    }

    /// Whether the question's reason field has the keyboard focus.
    pub(crate) fn part_prompt_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.part_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.input.focus_handle(cx).is_focused(window))
    }

    fn open_part_prompt(
        &mut self,
        id: ConversationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conversation) = self.state.conversation(id) else {
            return;
        };
        let (network, channel) = (conversation.network, conversation.name.clone());
        let server = self
            .state
            .networks()
            .iter()
            .find(|candidate| candidate.id == network)
            .map(|network| self.network_label(network))
            .unwrap_or_default();
        // What an empty field sends: the part message of the connection.
        let configured = self
            .sessions
            .get(&network)
            .and_then(|session| session.active_config.as_ref())
            .map(|config| config.part_message.trim())
            .filter(|message| !message.is_empty());
        let placeholder = configured.map_or_else(
            || self.i18n.text("part_message_placeholder"),
            |message| neutralize_bidi(message).into_owned(),
        );
        let input = cx.new(|cx| TextInput::new_field(&placeholder, "", false, cx));
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        self.part_prompt = Some(PartPrompt {
            network,
            conversation: id,
            channel,
            server,
            input,
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
            previous: window.focused(cx),
            focus_pending: true,
            error: None,
        });
        cx.notify();
    }

    /// Closes the question without leaving, putting the focus back.
    pub(crate) fn close_part_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prompt) = self.part_prompt.take()
            && let Some(handle) = prompt.previous
        {
            window.focus(&handle);
        }
        cx.notify();
    }

    /// Sends the PART the question was about. An empty reason leaves with the
    /// connection's default; a typed one is checked like `/part` and used for
    /// this PART only. A failure keeps the question open with the reason.
    pub(crate) fn confirm_part_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.part_prompt.as_ref() else {
            return;
        };
        if prompt.input.read(cx).is_composing() {
            return;
        }
        let (network, id, channel) = (prompt.network, prompt.conversation, prompt.channel.clone());
        let reason = prompt.input.read(cx).text().trim().to_owned();
        // The channel may have been left or the server lost while it was open.
        if !self.state.is_active_channel(id) {
            self.close_part_prompt(window, cx);
            return;
        }
        let result = self.registered_connection(network).and_then(|connection| {
            connection.part_channel(&channel, (!reason.is_empty()).then_some(reason.as_str()))
        });
        match result {
            Ok(()) => {
                let folded = self.state.casemapping(network).fold(&channel);
                if let Some(session) = self.sessions.get_mut(&network) {
                    expire_pending(&mut session.pending_parts);
                    if session.pending_parts.len() >= PENDING_PARTS_LIMIT {
                        session.pending_parts.clear();
                    }
                    session.pending_parts.insert(folded, Instant::now());
                }
                self.note_user_command(network, &["PART"]);
                self.prompt_feedback = None;
                self.close_part_prompt(window, cx);
            }
            Err(error) => {
                if let Some(prompt) = self.part_prompt.as_mut() {
                    prompt.error = Some(error);
                }
                cx.notify();
            }
        }
    }

    /// Tab / Shift+Tab: only the field and the two buttons are reachable.
    fn step_part_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.part_prompt.as_ref() else {
            return;
        };
        let order = [
            prompt.input.focus_handle(cx),
            prompt.cancel.clone(),
            prompt.confirm.clone(),
        ];
        let step = if forward { 1 } else { order.len() - 1 };
        let at = order.iter().position(|handle| handle.is_focused(window));
        window.focus(&order[at.map_or(0, |at| (at + step) % order.len())]);
    }

    pub(crate) fn render_part_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self.part_prompt.as_mut()?;
        if prompt.focus_pending {
            prompt.focus_pending = false;
            window.focus(&prompt.cancel);
        }
        let theme = theme::current(cx);
        let prompt = self.part_prompt.as_ref()?;
        let title = self.i18n.format(
            "part_dialog_title",
            &[("channel", &neutralize_bidi(&prompt.channel))],
        );
        let server = self.i18n.format(
            "part_dialog_server",
            &[("server", &neutralize_bidi(&prompt.server))],
        );
        let box_width = dialog::box_width(BOX_WIDTH, window);
        let confirm = dialog::confirm_button(
            "part-dialog-confirm",
            self.i18n.text("part_dialog_confirm"),
            &prompt.confirm,
            &theme,
            |this, window, cx| this.confirm_part_prompt(window, cx),
            cx,
        );
        let cancel = dialog::cancel_button(
            "part-dialog-cancel",
            self.i18n.text("cancel"),
            &prompt.cancel,
            &theme,
            |this, window, cx| this.close_part_prompt(window, cx),
            cx,
        );
        Some(
            dialog::overlay("part-dialog", &theme)
                // Only the keys handled here are consumed; typing reaches the field.
                .key_context("PartDialog")
                .on_action(cx.listener(|this, _: &FocusNextField, window, cx| {
                    this.step_part_focus(true, window, cx);
                }))
                .on_action(cx.listener(|this, _: &FocusPreviousField, window, cx| {
                    this.step_part_focus(false, window, cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_part_prompt(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    dialog::dialog_box(&theme)
                        .debug_selector(|| "part-dialog-box".into())
                        .w(box_width)
                        .child(div().font_weight(FontWeight::BOLD).child(title))
                        .child(div().child(server))
                        .child(div().child(self.i18n.text("part_dialog_reason")))
                        .child(prompt.input.clone())
                        .when_some(prompt.error.clone(), |d, error| {
                            d.child(div().text_color(theme.warning).child(error))
                        })
                        .child(
                            button_order::row(div().flex().gap_2())
                                .child(confirm)
                                .child(cancel),
                        ),
                )
                .into_any_element(),
        )
    }
}
