// SPDX-License-Identifier: GPL-3.0-only
//! The question of a multi-line paste into a message draft (#605), drawn in the
//! chat window when "confirm multi-line paste" is on.
//!
//! The pasted lines are shown one editable field each. Sending joins the
//! non-empty lines with single spaces into one PRIVMSG, or one NOTICE when its
//! box is ticked; it goes to the conversation the paste was made in and never
//! through the draft, so a pasted `/command` is only text and the draft is not
//! touched. The quote buttons put the quote mark and a space before every
//! non-empty line, or take that prefix off again. The keyboard focus starts on
//! Send, so Enter sends and Escape cancels.

use cayenchat_model::{ConversationId, NetworkId, display::neutralize_bidi};
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, FocusNextField, FocusPreviousField, button_order, dialog, input::TextInput,
    notices, settings_theme, theme,
};

const BOX_WIDTH: Pixels = px(520.);
const LINES_HEIGHT: Pixels = px(220.);
/// Most lines the dialog edits; a longer paste is inserted as one line, as
/// when the dialog is off, so the number of fields stays bounded.
pub(crate) const MAX_LINES: usize = 100;
const DEFAULT_QUOTE: &str = ">";

/// The lines of pasted text: CRLF, CR and LF all end a line, and one trailing
/// line break does not start another.
pub(crate) fn split_lines(text: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    text.split('\n').map(str::to_owned).collect()
}

/// `line` with `quote` and a space in front; an empty line stays empty.
pub(crate) fn add_quote(line: &str, quote: &str) -> String {
    if line.trim().is_empty() {
        line.to_owned()
    } else {
        format!("{quote} {line}")
    }
}

/// `line` without a leading `quote` and space; other lines are unchanged.
pub(crate) fn remove_quote(line: &str, quote: &str) -> String {
    line.strip_prefix(quote)
        .and_then(|rest| rest.strip_prefix(' '))
        .unwrap_or(line)
        .to_owned()
}

/// The message the lines make: the non-empty ones joined with one space.
pub(crate) fn joined<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    lines
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) struct PastePrompt {
    network: NetworkId,
    conversation: ConversationId,
    target: String,
    lines: Vec<Entity<TextInput>>,
    pub(crate) quote: Entity<TextInput>,
    notice: bool,
    add: FocusHandle,
    remove: FocusHandle,
    empty: FocusHandle,
    notice_box: FocusHandle,
    cancel: FocusHandle,
    confirm: FocusHandle,
    previous: Option<FocusHandle>,
    focus_pending: bool,
    error: Option<String>,
}

impl PastePrompt {
    pub(crate) fn network(&self) -> NetworkId {
        self.network
    }

    /// Every input of the dialog, for the composition check.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> impl Iterator<Item = &Entity<TextInput>> {
        self.lines.iter()
    }

    #[cfg(test)]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn inputs(&self) -> impl Iterator<Item = &Entity<TextInput>> {
        self.lines.iter().chain(std::iter::once(&self.quote))
    }
}

impl ChatWindow {
    /// A multi-line paste into the draft of `input`: asks about it when the
    /// draft belongs to a channel or private conversation and the paste is not
    /// too long, else inserts it as one line.
    pub(crate) fn open_paste_prompt(
        &mut self,
        input: &Entity<TextInput>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let conversation = self
            .inputs
            .iter()
            .find(|(_, candidate)| *candidate == input)
            .and_then(|(selection, _)| match selection {
                crate::Selection::Channel(id) => self.state.conversation(*id),
                _ => None,
            })
            .map(|conversation| {
                (
                    conversation.id,
                    conversation.network,
                    conversation.name.clone(),
                )
            });
        let lines = split_lines(text);
        let Some((id, network, name)) =
            conversation.filter(|_| self.paste_prompt.is_none() && lines.len() <= MAX_LINES)
        else {
            input.update(cx, |input, cx| input.insert_pasted(text, window, cx));
            return;
        };
        let lines = lines
            .iter()
            .map(|line| cx.new(|cx| TextInput::new_field("", line, false, cx)))
            .collect();
        let quote = cx.new(|cx| TextInput::new_field("", DEFAULT_QUOTE, false, cx));
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        self.paste_prompt = Some(PastePrompt {
            network,
            conversation: id,
            target: name,
            lines,
            quote,
            notice: false,
            add: cx.focus_handle().tab_stop(true),
            remove: cx.focus_handle().tab_stop(true),
            empty: cx.focus_handle().tab_stop(true),
            notice_box: cx.focus_handle().tab_stop(true),
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
            previous: window.focused(cx),
            focus_pending: true,
            error: None,
        });
        cx.notify();
    }

    /// Closes the question without sending, putting the focus back.
    pub(crate) fn close_paste_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prompt) = self.paste_prompt.take()
            && let Some(handle) = prompt.previous
        {
            window.focus(&handle);
        }
        cx.notify();
    }

    /// Enter in one of the dialog's fields: sends from a line, adds the quote
    /// from the quote field. Whether a field of the dialog had the focus.
    pub(crate) fn submit_paste_prompt_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(prompt) = self.paste_prompt.as_ref() else {
            return false;
        };
        if prompt.quote.focus_handle(cx).is_focused(window) {
            self.edit_paste_lines(add_quote, cx);
            true
        } else if prompt
            .lines
            .iter()
            .any(|line| line.focus_handle(cx).is_focused(window))
        {
            self.confirm_paste_prompt(window, cx);
            true
        } else {
            false
        }
    }

    fn paste_quote(&self, cx: &App) -> Option<String> {
        let quote = self
            .paste_prompt
            .as_ref()?
            .quote
            .read(cx)
            .text()
            .trim()
            .to_owned();
        (!quote.is_empty()).then_some(quote)
    }

    /// Rewrites every line with `edit(line, quote)`; nothing without a quote.
    pub(crate) fn edit_paste_lines(
        &mut self,
        edit: impl Fn(&str, &str) -> String,
        cx: &mut Context<Self>,
    ) {
        let Some(quote) = self.paste_quote(cx) else {
            return;
        };
        let Some(prompt) = self.paste_prompt.as_ref() else {
            return;
        };
        for line in &prompt.lines {
            let edited = edit(line.read(cx).text(), &quote);
            line.update(cx, |line, cx| line.set_text(&edited, cx));
        }
    }

    pub(crate) fn remove_empty_paste_lines(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.paste_prompt.as_mut() else {
            return;
        };
        prompt
            .lines
            .retain(|line| !line.read(cx).text().trim().is_empty());
        cx.notify();
    }

    fn toggle_paste_notice(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.paste_prompt.as_mut() {
            prompt.notice = !prompt.notice;
            cx.notify();
        }
    }

    /// Sends the lines to the conversation the paste was made in. A failure
    /// keeps the question open with the reason.
    pub(crate) fn confirm_paste_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.paste_prompt.as_ref() else {
            return;
        };
        if prompt.inputs().any(|input| input.read(cx).is_composing()) {
            return;
        }
        let (network, id, notice) = (prompt.network, prompt.conversation, prompt.notice);
        let text = joined(prompt.lines.iter().map(|line| line.read(cx).text()));
        let text = text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Some(conversation) = self.state.conversation(id) else {
            self.close_paste_prompt(window, cx);
            return;
        };
        let (name, private) = (conversation.name.clone(), conversation.is_private());
        let result = self.registered_connection(network).and_then(|connection| {
            if !self.state.is_active_channel(id) {
                Err(self.i18n.text("wait_join"))
            } else if private {
                connection.send_private_message(&name, &text, notice)
            } else {
                connection.send_message(&name, &text, notice)
            }
        });
        match result {
            Ok(()) => {
                if notices::is_spoken(&text) {
                    self.note_spoken(network);
                }
                self.close_paste_prompt(window, cx);
            }
            Err(error) => {
                if let Some(prompt) = self.paste_prompt.as_mut() {
                    prompt.error = Some(error);
                }
                cx.notify();
            }
        }
    }

    /// Tab / Shift+Tab through the fields, the quote buttons and the box.
    fn step_paste_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.paste_prompt.as_ref() else {
            return;
        };
        let order: Vec<FocusHandle> = prompt
            .lines
            .iter()
            .map(|line| line.focus_handle(cx))
            .chain([
                prompt.quote.focus_handle(cx),
                prompt.add.clone(),
                prompt.remove.clone(),
                prompt.empty.clone(),
                prompt.notice_box.clone(),
                prompt.cancel.clone(),
                prompt.confirm.clone(),
            ])
            .collect();
        let step = if forward { 1 } else { order.len() - 1 };
        let at = order.iter().position(|handle| handle.is_focused(window));
        window.focus(&order[at.map_or(0, |at| (at + step) % order.len())]);
    }

    pub(crate) fn render_paste_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self.paste_prompt.as_mut()?;
        if prompt.focus_pending {
            prompt.focus_pending = false;
            window.focus(&prompt.confirm);
        }
        let theme = theme::current(cx);
        let prompt = self.paste_prompt.as_ref()?;
        let title = self.i18n.format(
            "paste_dialog_title",
            &[("target", &neutralize_bidi(&prompt.target))],
        );
        let box_width = dialog::box_width(BOX_WIDTH, window);
        let confirm = dialog::confirm_button(
            "paste-dialog-confirm",
            self.i18n.text("paste_dialog_send"),
            &prompt.confirm,
            &theme,
            |this, window, cx| this.confirm_paste_prompt(window, cx),
            cx,
        );
        let cancel = dialog::cancel_button(
            "paste-dialog-cancel",
            self.i18n.text("cancel"),
            &prompt.cancel,
            &theme,
            |this, window, cx| this.close_paste_prompt(window, cx),
            cx,
        );
        let add = dialog::cancel_button(
            "paste-dialog-add-quote",
            self.i18n.text("paste_dialog_add_quote"),
            &prompt.add,
            &theme,
            |this, _, cx| this.edit_paste_lines(add_quote, cx),
            cx,
        );
        let remove = dialog::cancel_button(
            "paste-dialog-remove-quote",
            self.i18n.text("paste_dialog_remove_quote"),
            &prompt.remove,
            &theme,
            |this, _, cx| this.edit_paste_lines(remove_quote, cx),
            cx,
        );
        let empty = dialog::cancel_button(
            "paste-dialog-remove-empty",
            self.i18n.text("paste_dialog_remove_empty"),
            &prompt.empty,
            &theme,
            |this, _, cx| this.remove_empty_paste_lines(cx),
            cx,
        );
        let text_color = theme.text;
        let notice = div()
            .id("paste-dialog-notice")
            .debug_selector(|| "paste-dialog-notice".into())
            .track_focus(&prompt.notice_box)
            .flex()
            .gap_2()
            .px_1()
            .border_1()
            .border_color(theme.surface)
            .focus(move |s| s.border_color(text_color))
            .cursor_pointer()
            .child(settings_theme::checkbox(prompt.notice, true, cx))
            .child(self.i18n.text("paste_dialog_notice"))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.toggle_paste_notice(cx);
                    cx.stop_propagation();
                }
            }))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_paste_notice(cx)));
        Some(
            dialog::overlay("paste-dialog", &theme)
                // Only the keys handled here are consumed; typing reaches the fields.
                .key_context("PasteDialog")
                .on_action(cx.listener(|this, _: &FocusNextField, window, cx| {
                    this.step_paste_focus(true, window, cx);
                }))
                .on_action(cx.listener(|this, _: &FocusPreviousField, window, cx| {
                    this.step_paste_focus(false, window, cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_paste_prompt(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    dialog::dialog_box(&theme)
                        .debug_selector(|| "paste-dialog-box".into())
                        .w(box_width)
                        .child(div().font_weight(FontWeight::BOLD).child(title))
                        .child(div().child(self.i18n.text("paste_dialog_hint")))
                        .child(
                            div()
                                .id("paste-dialog-lines")
                                .max_h(LINES_HEIGHT)
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(prompt.lines.iter().cloned()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(self.i18n.text("paste_dialog_quote"))
                                .child(div().w(px(48.)).child(prompt.quote.clone()))
                                .child(add)
                                .child(remove)
                                .child(empty),
                        )
                        .child(notice)
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

#[cfg(test)]
mod tests {
    // Not `super::*`: it brings in `gpui::test`, which shadows `#[test]`.
    use super::{add_quote, joined, remove_quote, split_lines};

    #[test]
    fn lines_split_on_any_break_and_ignore_one_trailing_break() {
        assert_eq!(split_lines("a\r\nb\rc\nd\n"), ["a", "b", "c", "d"]);
        assert_eq!(split_lines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(split_lines("a\n\n"), ["a", ""]);
    }

    #[test]
    fn quotes_are_added_to_text_lines_and_removed_again() {
        assert_eq!(add_quote("hello", ">"), "> hello");
        assert_eq!(add_quote("> hello", ">"), "> > hello");
        assert_eq!(add_quote("", ">"), "");
        assert_eq!(add_quote("  ", ">"), "  ");
        assert_eq!(remove_quote("> hello", ">"), "hello");
        assert_eq!(remove_quote("> > hello", ">"), "> hello");
        assert_eq!(remove_quote(">hello", ">"), ">hello");
        assert_eq!(remove_quote("hello", ">"), "hello");
        assert_eq!(remove_quote("|> hello", "|>"), "hello");
    }

    #[test]
    fn lines_join_with_one_space_and_skip_empty_ones() {
        assert_eq!(joined(["a", "", " ", "b"]), "a b");
        assert_eq!(joined([""]), "");
    }
}
