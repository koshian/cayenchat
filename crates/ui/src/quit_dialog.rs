// SPDX-License-Identifier: GPL-3.0-only
//! The "Quit CayenChat?" question, drawn in the chat window (#378).
//!
//! The native prompt cannot keep both the OS's button order and a safe
//! Enter: the Windows task dialog and AppKit make the first button the
//! default, and that is also the one shown first/rightmost. Here the order
//! comes from [`button_order::row`] and the keyboard focus starts on Cancel,
//! so Enter, Space and Escape cancel on every OS. Quitting needs a click, or
//! Tab to the confirm button and Enter.

use std::time::Duration;

use gpui::{prelude::*, *};

use crate::{ChatWindow, button_order, dialog, theme};

const QUIT_BOX_MAX_WIDTH: Pixels = px(360.);
/// Padding and border on both sides of the text inside the box.
const QUIT_BOX_CHROME: Pixels = px(26.);

/// How long a quit waits for the credential store before it offers to quit
/// anyway (the wait it used to make silently, #982).
const SECRETS_PATIENCE: Duration = Duration::from_secs(2);

/// Focus handles of the two buttons, and where the focus was before.
pub(crate) struct QuitDialog {
    cancel: FocusHandle,
    confirm: FocusHandle,
    previous: Option<FocusHandle>,
    focus_pending: bool,
    /// An ordinary quit that is held back because the credential store still
    /// owes a result; the quit goes on by itself when it is in.
    waiting: Option<SecretsWait>,
}

struct SecretsWait {
    /// The store did not answer within `SECRETS_PATIENCE`: the user may now
    /// choose between waiting on and quitting anyway.
    slow: bool,
    task: Task<()>,
}

impl QuitDialog {
    pub(crate) fn new(cx: &mut Context<ChatWindow>) -> Self {
        Self {
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
            previous: None,
            focus_pending: false,
            waiting: None,
        }
    }

    /// Whether the choice to quit anyway is on offer.
    #[cfg(test)]
    pub(crate) fn offers_force(&self) -> bool {
        self.waiting.as_ref().is_some_and(|wait| wait.slow)
    }

    pub(crate) fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }
}

impl ChatWindow {
    /// Holds an ordinary quit back until the credential store has taken in
    /// every save and delete (#982), saying what it waits for. The quit
    /// itself is `quit_app`, run again when the store is idle.
    pub(crate) fn wait_for_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_dialog.waiting.is_some() {
            return;
        }
        window.activate_window();
        self.quit_dialog.previous = window.focused(cx);
        self.quit_dialog.focus_pending = true;
        self.watch_secrets(window, cx);
        cx.notify();
    }

    /// Waits for the store with no polling: the release of the last job wakes
    /// the task. After `SECRETS_PATIENCE` the dialog offers the choice.
    fn watch_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = cx.spawn_in(window, async move |this, cx| {
            let Ok(idle) = this.update(cx, |_, cx| crate::secrets::idle(cx)) else {
                return;
            };
            let idle = Box::pin(idle);
            let timer = cx.background_executor().timer(SECRETS_PATIENCE);
            match futures_util::future::select(idle, Box::pin(timer)).await {
                futures_util::future::Either::Left(_) => {}
                futures_util::future::Either::Right((_, still)) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(wait) = this.quit_dialog.waiting.as_mut() {
                            wait.slow = true;
                        }
                        cx.notify();
                    });
                    still.await;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| this.secrets_settled(window, cx));
        });
        self.quit_dialog.waiting = Some(SecretsWait { slow: false, task });
    }

    /// The store is idle: the quit goes on through the usual checks, which
    /// also catch a failed save that left the settings window with an error.
    fn secrets_settled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(wait) = self.quit_dialog.waiting.take() {
            wait.task.detach();
        }
        if let Some(handle) = self.quit_dialog.previous.take() {
            window.focus(&handle);
        }
        cx.notify();
        crate::quit_app(cx);
    }

    /// Gives up the quit; the saves go on in the background.
    fn cancel_secrets_wait(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_dialog.waiting = None;
        if let Some(handle) = self.quit_dialog.previous.take() {
            window.focus(&handle);
        }
        cx.notify();
    }

    /// "Keep waiting": the question goes away and comes back after another
    /// `SECRETS_PATIENCE` if the store is still busy.
    fn keep_waiting_for_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_dialog.focus_pending = true;
        self.watch_secrets(window, cx);
        cx.notify();
    }

    /// "Quit anyway": the explicit forced quit. A save or delete still
    /// waiting for the store may be lost or left undone.
    fn quit_without_secrets(&mut self, cx: &mut Context<Self>) {
        crate::secrets::abandon(cx);
        cx.quit();
    }

    fn render_secrets_wait(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let slow = self.quit_dialog.waiting.as_ref().is_some_and(|w| w.slow);
        if self.quit_dialog.focus_pending {
            self.quit_dialog.focus_pending = false;
            window.focus(&self.quit_dialog.cancel);
        }
        let theme = theme::current(cx);
        let box_width = dialog::box_width(QUIT_BOX_MAX_WIDTH, window);
        let buttons = if slow {
            button_order::row(div().flex().gap_2())
                .child(dialog::confirm_button(
                    "quit-wait-force",
                    self.i18n.text("quit_wait_force"),
                    &self.quit_dialog.confirm,
                    &theme,
                    |this, _, cx| this.quit_without_secrets(cx),
                    cx,
                ))
                .child(dialog::cancel_button(
                    "quit-wait-keep",
                    self.i18n.text("quit_wait_keep"),
                    &self.quit_dialog.cancel,
                    &theme,
                    |this, window, cx| this.keep_waiting_for_secrets(window, cx),
                    cx,
                ))
        } else {
            button_order::row(div().flex().gap_2()).child(dialog::cancel_button(
                "quit-wait-cancel",
                self.i18n.text("cancel"),
                &self.quit_dialog.cancel,
                &theme,
                |this, window, cx| this.cancel_secrets_wait(window, cx),
                cx,
            ))
        };
        dialog::overlay("quit-wait-dialog", &theme)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" if slow => this.keep_waiting_for_secrets(window, cx),
                    "escape" => this.cancel_secrets_wait(window, cx),
                    "tab" if slow => {
                        let (cancel, confirm) =
                            (&this.quit_dialog.cancel, &this.quit_dialog.confirm);
                        window.focus(if cancel.is_focused(window) {
                            confirm
                        } else {
                            cancel
                        });
                    }
                    _ => {}
                }
                cx.stop_propagation();
            }))
            .child(
                dialog::dialog_box(&theme)
                    .debug_selector(|| "quit-wait-box".into())
                    .max_w(box_width)
                    .h_auto()
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child(self.i18n.text("quit_wait_title")),
                    )
                    .child(div().debug_selector(|| "quit-wait-detail".into()).child(
                        self.i18n.text(if slow {
                            "quit_wait_slow"
                        } else {
                            "quit_wait_detail"
                        }),
                    ))
                    .child(buttons),
            )
            .into_any_element()
    }

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
        if self.quit_dialog.is_waiting() {
            return Some(self.render_secrets_wait(window, cx));
        }
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
