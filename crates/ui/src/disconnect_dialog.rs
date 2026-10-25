// SPDX-License-Identifier: GPL-3.0-only
//! The "Disconnect from <server>?" question of a double click on a registered
//! server row (#625), drawn in the chat window.
//!
//! Like the quit and part questions, the keyboard focus starts on Cancel, so
//! Enter, Space and Escape cancel; disconnecting needs a click, or Tab to the
//! confirm button and Enter. The prompt is bound to the row that was clicked,
//! so selecting another row while it is open changes nothing. "Don't ask
//! again" is saved only when disconnecting is confirmed.

use cayenchat_app::ConnectionStatus;
use cayenchat_model::{NetworkId, display::neutralize_bidi};
use gpui::{prelude::*, *};

use crate::{ChatWindow, button_order, dialog, theme};

const BOX_WIDTH: Pixels = px(340.);

pub(crate) struct DisconnectPrompt {
    network: NetworkId,
    server: String,
    pub(crate) dont_ask: bool,
    cancel: FocusHandle,
    confirm: FocusHandle,
    dont_ask_box: FocusHandle,
    previous: Option<FocusHandle>,
    focus_pending: bool,
}

impl DisconnectPrompt {
    pub(crate) fn network(&self) -> NetworkId {
        self.network
    }
}

impl ChatWindow {
    /// A double click on a server row: connects a server not used in this
    /// run, reconnects a dropped one, asks before disconnecting a registered
    /// one (unless switched off), and does nothing while it is connecting or
    /// waiting to retry, or while a question is open.
    pub(crate) fn activate_server_row(
        &mut self,
        network: NetworkId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disconnect_prompt.is_some() {
            return;
        }
        let Some(session) = self.sessions.get(&network) else {
            return;
        };
        if session.retry_pending {
            return;
        }
        if session.irc.is_none() {
            self.reconnect(network, window, cx);
        } else if self.is_registered_status(network) {
            if self.confirm_server_dblclick_disconnect {
                self.open_disconnect_prompt(network, window, cx);
            } else {
                self.disconnect(network, cx);
            }
        }
    }

    fn is_registered_status(&self, network: NetworkId) -> bool {
        matches!(
            self.state.status(network),
            Some(ConnectionStatus::Registered)
        )
    }

    fn open_disconnect_prompt(
        &mut self,
        network: NetworkId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let server = self
            .state
            .networks()
            .iter()
            .find(|candidate| candidate.id == network)
            .map(|network| self.network_label(network))
            .unwrap_or_default();
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        self.disconnect_prompt = Some(DisconnectPrompt {
            network,
            server,
            dont_ask: false,
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
            dont_ask_box: cx.focus_handle().tab_stop(true),
            previous: window.focused(cx),
            focus_pending: true,
        });
        cx.notify();
    }

    /// Closes the question without disconnecting, putting the focus back.
    pub(crate) fn close_disconnect_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prompt) = self.disconnect_prompt.take()
            && let Some(handle) = prompt.previous
        {
            window.focus(&handle);
        }
        cx.notify();
    }

    /// Disconnects the server the question was about, and remembers "Don't
    /// ask again" if ticked. A server that dropped meanwhile is left alone.
    pub(crate) fn confirm_disconnect_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.disconnect_prompt.as_ref() else {
            return;
        };
        let (network, dont_ask) = (prompt.network, prompt.dont_ask);
        self.close_disconnect_prompt(window, cx);
        // Only a disconnect that was really requested remembers the choice.
        if self.is_registered_status(network) && self.disconnect(network, cx) && dont_ask {
            self.confirm_server_dblclick_disconnect = false;
            self.save_view_setting(
                |settings| settings.confirm_server_dblclick_disconnect = false,
                cx,
            );
        }
    }

    pub(crate) fn toggle_disconnect_dont_ask(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.disconnect_prompt.as_mut() {
            prompt.dont_ask = !prompt.dont_ask;
            cx.notify();
        }
    }

    /// Tab / Shift+Tab: only the two buttons and the checkbox are reachable.
    fn step_disconnect_focus(&mut self, forward: bool, window: &mut Window) {
        let Some(prompt) = self.disconnect_prompt.as_ref() else {
            return;
        };
        let order = [&prompt.cancel, &prompt.confirm, &prompt.dont_ask_box];
        let step = if forward { 1 } else { order.len() - 1 };
        let at = order.iter().position(|handle| handle.is_focused(window));
        window.focus(order[at.map_or(0, |at| (at + step) % order.len())]);
    }

    pub(crate) fn render_disconnect_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self.disconnect_prompt.as_mut()?;
        if prompt.focus_pending {
            prompt.focus_pending = false;
            window.focus(&prompt.cancel);
        }
        let theme = theme::current(cx);
        let prompt = self.disconnect_prompt.as_ref()?;
        let title = self.i18n.format(
            "disconnect_dialog_title",
            &[("server", &neutralize_bidi(&prompt.server))],
        );
        let box_width = dialog::box_width(BOX_WIDTH, window);
        let confirm = dialog::confirm_button(
            "disconnect-dialog-confirm",
            self.i18n.text("disconnect_dialog_confirm"),
            &prompt.confirm,
            &theme,
            |this, window, cx| this.confirm_disconnect_prompt(window, cx),
            cx,
        );
        let cancel = dialog::cancel_button(
            "disconnect-dialog-cancel",
            self.i18n.text("cancel"),
            &prompt.cancel,
            &theme,
            |this, window, cx| this.close_disconnect_prompt(window, cx),
            cx,
        );
        let dont_ask = dialog::check_row(
            "disconnect-dialog-dont-ask",
            self.i18n.text("dont_ask_again"),
            prompt.dont_ask,
            &prompt.dont_ask_box,
            &theme,
            |this, _, cx| this.toggle_disconnect_dont_ask(cx),
            cx,
        );
        Some(
            dialog::overlay("disconnect-dialog", &theme)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => this.close_disconnect_prompt(window, cx),
                        "tab" => {
                            this.step_disconnect_focus(!event.keystroke.modifiers.shift, window)
                        }
                        _ => {}
                    }
                    cx.stop_propagation();
                }))
                .child(
                    dialog::dialog_box(&theme)
                        .debug_selector(|| "disconnect-dialog-box".into())
                        .w(box_width)
                        .child(div().font_weight(FontWeight::BOLD).child(title))
                        .child(dont_ask)
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
