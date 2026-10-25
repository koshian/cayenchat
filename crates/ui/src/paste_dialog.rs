// SPDX-License-Identifier: GPL-3.0-only
//! The question of a multi-line paste into a message draft (#605), drawn in the
//! chat window when "confirm multi-line paste" is on.
//!
//! The pasted text is shown in one multi-line editor. Sending sends each of its
//! non-empty lines as its own PRIVMSG, or NOTICE when its
//! box is ticked; it goes to the conversation the paste was made in and never
//! through the draft, so a pasted `/command` is only text and the draft is not
//! touched. One quote button puts the quote mark and a space before every
//! non-empty line, or, when every non-empty line already starts with them,
//! takes one such prefix off each. The keyboard focus starts on Send, so Enter
//! sends and Escape cancels; Enter in the editor is a line break and Ctrl+Enter
//! sends.

use cayenchat_model::{ConversationId, NetworkId, display::neutralize_bidi};
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, FocusNextField, FocusPreviousField, button_order, dialog, input::TextInput,
    notices, settings_theme, theme,
};

const BOX_WIDTH: Pixels = px(520.);
/// Lines the editor shows before it scrolls.
const BODY_ROWS: usize = 8;
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

/// `line` without one leading `quote` and space; other lines are unchanged.
pub(crate) fn remove_quote(line: &str, quote: &str) -> String {
    unquoted(line, quote).unwrap_or(line).to_owned()
}

fn unquoted<'a>(line: &'a str, quote: &str) -> Option<&'a str> {
    line.strip_prefix(quote)?.strip_prefix(' ')
}

/// Whether the quote button removes: there is a non-empty line and every
/// non-empty line starts with `quote` and a space. Empty lines do not count.
pub(crate) fn removes_quote(text: &str, quote: &str) -> bool {
    let mut lines = text.split('\n').filter(|line| !line.trim().is_empty());
    lines.next().is_some_and(|first| {
        unquoted(first, quote).is_some() && lines.all(|line| unquoted(line, quote).is_some())
    })
}

/// `text` after the quote button: one more level of quote on every non-empty
/// line, or one level less when [`removes_quote`].
pub(crate) fn toggle_quote(text: &str, quote: &str) -> String {
    let edit = if removes_quote(text, quote) {
        remove_quote
    } else {
        add_quote
    };
    text.split('\n')
        .map(|line| edit(line, quote))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` without its empty lines.
fn without_empty_lines(text: &str) -> String {
    text.split('\n')
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The messages the lines make: each non-empty line, without trailing blanks.
pub(crate) fn message_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    lines
        .into_iter()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) struct PastePrompt {
    network: NetworkId,
    conversation: ConversationId,
    target: String,
    body: Entity<TextInput>,
    pub(crate) quote: Entity<TextInput>,
    notice: bool,
    quote_button: FocusHandle,
    empty: FocusHandle,
    notice_box: FocusHandle,
    cancel: FocusHandle,
    confirm: FocusHandle,
    previous: Option<FocusHandle>,
    focus_pending: bool,
    error: Option<String>,
    /// Redraw when the text or the quote changes (the button's name follows).
    _observers: [Subscription; 2],
}

impl PastePrompt {
    pub(crate) fn network(&self) -> NetworkId {
        self.network
    }

    #[cfg(test)]
    pub(crate) fn body(&self) -> &Entity<TextInput> {
        &self.body
    }

    #[cfg(test)]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Every input of the dialog, for the composition check.
    pub(crate) fn inputs(&self) -> impl Iterator<Item = &Entity<TextInput>> {
        [&self.body, &self.quote].into_iter()
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
        let text = lines.join("\n");
        let body = cx.new(|cx| TextInput::new_multiline(&text, BODY_ROWS, cx));
        let quote = cx.new(|cx| TextInput::new_field("", DEFAULT_QUOTE, false, cx).bordered());
        let observers = [
            cx.observe(&body, |_, _, cx| cx.notify()),
            cx.observe(&quote, |_, _, cx| cx.notify()),
        ];
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        self.paste_prompt = Some(PastePrompt {
            network,
            conversation: id,
            target: name,
            body,
            quote,
            notice: false,
            quote_button: cx.focus_handle().tab_stop(true),
            empty: cx.focus_handle().tab_stop(true),
            notice_box: cx.focus_handle().tab_stop(true),
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
            previous: window.focused(cx),
            focus_pending: true,
            error: None,
            _observers: observers,
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

    /// Enter in the quote field toggles the quote; Ctrl+Enter (or the NOTICE
    /// key) in the editor sends. Whether a field of the dialog had the focus.
    pub(crate) fn submit_paste_prompt_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(prompt) = self.paste_prompt.as_ref() else {
            return false;
        };
        if prompt.quote.focus_handle(cx).is_focused(window) {
            self.toggle_paste_quote(cx);
            true
        } else if prompt.body.focus_handle(cx).is_focused(window) {
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

    /// Whether the quote button would remove the quote now.
    fn paste_removes_quote(&self, cx: &App) -> bool {
        self.paste_prompt.as_ref().is_some_and(|prompt| {
            self.paste_quote(cx)
                .is_some_and(|quote| removes_quote(prompt.body.read(cx).text(), &quote))
        })
    }

    /// The quote button: adds or removes one level of quote in the text;
    /// nothing without a quote mark. The text change can be undone.
    pub(crate) fn toggle_paste_quote(&mut self, cx: &mut Context<Self>) {
        let Some(quote) = self.paste_quote(cx) else {
            return;
        };
        let Some(prompt) = self.paste_prompt.as_ref() else {
            return;
        };
        let edited = toggle_quote(prompt.body.read(cx).text(), &quote);
        prompt
            .body
            .update(cx, |body, cx| body.replace_text(&edited, cx));
    }

    pub(crate) fn remove_empty_paste_lines(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.paste_prompt.as_ref() else {
            return;
        };
        let edited = without_empty_lines(prompt.body.read(cx).text());
        prompt
            .body
            .update(cx, |body, cx| body.replace_text(&edited, cx));
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
        let lines = message_lines(prompt.body.read(cx).text().split('\n'));
        if lines.is_empty() {
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
            } else {
                connection.send_lines(&name, &lines, notice, private)
            }
        });
        match result {
            Ok(()) => {
                if lines.iter().any(|line| notices::is_spoken(line)) {
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

    /// Tab / Shift+Tab through the fields, the buttons and the box.
    fn step_paste_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.paste_prompt.as_ref() else {
            return;
        };
        let order = [
            prompt.body.focus_handle(cx),
            prompt.quote.focus_handle(cx),
            prompt.quote_button.clone(),
            prompt.empty.clone(),
            prompt.notice_box.clone(),
            prompt.cancel.clone(),
            prompt.confirm.clone(),
        ];
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
        let quote_label = if self.paste_removes_quote(cx) {
            "paste_dialog_remove_quote"
        } else {
            "paste_dialog_add_quote"
        };
        let quote_button = dialog::cancel_button(
            "paste-dialog-quote",
            self.i18n.text(quote_label),
            &prompt.quote_button,
            &theme,
            |this, _, cx| this.toggle_paste_quote(cx),
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
                        .child(prompt.body.clone())
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(self.i18n.text("paste_dialog_quote"))
                                .child(div().w(px(64.)).child(prompt.quote.clone()))
                                .child(quote_button)
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
    use super::{add_quote, message_lines, remove_quote, removes_quote, split_lines, toggle_quote};

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
    fn the_button_removes_only_when_every_text_line_is_quoted() {
        assert!(!removes_quote("a\nb", ">"));
        assert!(removes_quote("> a\n> b", ">"));
        // Empty lines do not count, quoted or not.
        assert!(removes_quote("> a\n\n  \n> b", ">"));
        assert!(!removes_quote("> a\nb", ">"));
        assert!(!removes_quote("", ">"));
        assert!(!removes_quote("\n \n", ">"));
        // No space after the mark: not quoted.
        assert!(!removes_quote(">a", ">"));
        assert!(removes_quote("| a", "|"));
        assert!(!removes_quote("> a", "|"));
    }

    #[test]
    fn toggling_changes_one_level_and_a_partial_quote_gets_a_second() {
        assert_eq!(toggle_quote("a\n\nb", ">"), "> a\n\n> b");
        assert_eq!(toggle_quote("> a\n\n> b", ">"), "a\n\nb");
        // Part quoted: every non-empty line gets one more level.
        assert_eq!(toggle_quote("> a\nb", ">"), "> > a\n> b");
        // Nested: one level at a time.
        assert_eq!(toggle_quote("> > a", ">"), "> a");
        assert_eq!(toggle_quote("> a", ">"), "a");
        assert_eq!(toggle_quote("a", ">"), "> a");
        // Empty lines are never touched; no text lines, nothing to quote.
        assert_eq!(toggle_quote("\n  \n", ">"), "\n  \n");
        // A changed quote mark follows the same rule.
        assert_eq!(toggle_quote("> a", "|"), "| > a");
        assert_eq!(toggle_quote("| > a", "|"), "> a");
    }

    #[test]
    fn each_non_empty_line_is_a_message() {
        assert_eq!(message_lines(["a ", "", " ", " b"]), ["a", " b"]);
        assert!(message_lines([""]).is_empty());
    }
}
