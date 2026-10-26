// SPDX-License-Identifier: GPL-3.0-only
//! The "Quit CayenChat?" question, drawn in the chat window (#378).
//!
//! The native prompt cannot keep both the OS's button order and a safe
//! Enter: the Windows task dialog and AppKit make the first button the
//! default, and that is also the one shown first/rightmost. Here the order
//! comes from [`button_order::row`] and the keyboard focus starts on Cancel,
//! so Enter, Space and Escape cancel on every OS. Quitting needs a click, or
//! Tab to the confirm button and Enter.

use gpui::{prelude::*, *};

use crate::{ChatWindow, button_order, dialog, theme};

const QUIT_BOX_MAX_WIDTH: Pixels = px(360.);
/// Padding and border on both sides of the text inside the box.
const QUIT_BOX_CHROME: Pixels = px(26.);

/// Focus handles of the two buttons, and where the focus was before.
pub(crate) struct QuitDialog {
    cancel: FocusHandle,
    confirm: FocusHandle,
    previous: Option<FocusHandle>,
    focus_pending: bool,
}

impl QuitDialog {
    pub(crate) fn new(cx: &mut Context<ChatWindow>) -> Self {
        Self {
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
            previous: None,
            focus_pending: false,
        }
    }
}

impl ChatWindow {
    pub(crate) fn open_quit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_prompt_open = true;
        self.quit_dialog.previous = window.focused(cx);
        self.quit_dialog.focus_pending = true;
        cx.notify();
    }

    /// Closes the question; `quit` is whether "Quit" was chosen. Cancelling
    /// puts the focus back where the user was typing.
    pub(crate) fn answer_quit_dialog(
        &mut self,
        quit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.quit_prompt_open {
            return;
        }
        self.quit_prompt_open = false;
        let previous = self.quit_dialog.previous.take();
        if quit {
            crate::quit_app(cx);
        } else if let Some(handle) = previous {
            window.focus(&handle);
        }
        cx.notify();
    }

    pub(crate) fn render_quit_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.quit_prompt_open {
            return None;
        }
        if self.quit_dialog.focus_pending {
            self.quit_dialog.focus_pending = false;
            window.focus(&self.quit_dialog.cancel);
        }
        let theme = theme::current(cx);
        let connected = self.sessions.values().any(|session| session.irc.is_some());
        let detail = self.i18n.text(if connected {
            "quit_detail_connected"
        } else {
            "quit_detail"
        });
        // A box sized by its content measures wrapped text at the one-line
        // width, so the wrapped lines overflow it. When the detail does not
        // fit on one line it gets the width the capped box offers.
        let detail_width = {
            let style = window.text_style();
            let run = style.to_run(detail.len());
            window
                .text_system()
                .shape_line(
                    detail.clone().into(),
                    style.font_size.to_pixels(window.rem_size()),
                    &[run],
                    None,
                )
                .width
        };
        // The box also stays inside a window narrower than its usual cap.
        let box_width = dialog::box_width(QUIT_BOX_MAX_WIDTH, window);
        let wraps = detail_width > box_width - QUIT_BOX_CHROME;
        let confirm = dialog::confirm_button(
            "quit-dialog-confirm",
            self.i18n.text("quit_confirm"),
            &self.quit_dialog.confirm,
            &theme,
            |this, window, cx| this.answer_quit_dialog(true, window, cx),
            cx,
        );
        let cancel = dialog::cancel_button(
            "quit-dialog-cancel",
            self.i18n.text("cancel"),
            &self.quit_dialog.cancel,
            &theme,
            |this, window, cx| this.answer_quit_dialog(false, window, cx),
            cx,
        );
        Some(
            dialog::overlay("quit-dialog", &theme)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => this.answer_quit_dialog(false, window, cx),
                        // Only the two buttons are reachable by Tab.
                        "tab" => {
                            let (cancel, confirm) =
                                (&this.quit_dialog.cancel, &this.quit_dialog.confirm);
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
                        .debug_selector(|| "quit-dialog-box".into())
                        // Fit the text; a fixed width left empty space beside it.
                        .max_w(box_width)
                        .h_auto()
                        .child(
                            div()
                                .font_weight(FontWeight::BOLD)
                                .child(self.i18n.text("quit_title")),
                        )
                        .child(
                            div()
                                .debug_selector(|| "quit-dialog-detail".into())
                                .when(wraps, |d| d.w(box_width - QUIT_BOX_CHROME))
                                .child(detail),
                        )
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
