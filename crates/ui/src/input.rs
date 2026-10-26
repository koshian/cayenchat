// SPDX-License-Identifier: Apache-2.0
// Adapted from gpui 0.2.2 examples/input.rs, Copyright Zed Industries.
// Licensed under Apache-2.0; see THIRD_PARTY_NOTICES.md and licenses/GPUI-APACHE-2.0.txt.
// Changes: compact styling, scoped shortcuts, focus, single-line input and IME fixes.
use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle,
    Window, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::*;

use crate::multiline::WrappedText;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordBackward,
        DeleteWordForward,
        DeleteToBeginning,
        DeleteToEnd,
        DeleteLine,
        DeleteWhitespace,
        CollapseWhitespace,
        Undo,
        Redo,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        UploadImage,
        // Only bound in a multi-line input (key context `MultilineInput`).
        Up,
        Down,
        SelectUp,
        SelectDown,
        InsertNewline,
        DocumentStart,
        DocumentEnd,
        SelectDocumentStart,
        SelectDocumentEnd,
    ]
);

/// The right-click menu's labels, in the interface language. Set by the
/// windows that own a `Localizer`; English until then.
#[derive(Clone)]
pub struct MenuLabels {
    undo: String,
    redo: String,
    cut: String,
    copy: String,
    paste: String,
    delete: String,
    select_all: String,
    upload: String,
    clear: String,
}

impl MenuLabels {
    pub fn new(i18n: &crate::localization::Localizer) -> Self {
        Self {
            undo: i18n.text("menu_undo"),
            redo: i18n.text("menu_redo"),
            cut: i18n.text("menu_cut"),
            copy: i18n.text("menu_copy"),
            paste: i18n.text("menu_paste"),
            delete: i18n.text("input_menu_delete"),
            select_all: i18n.text("menu_select_all"),
            upload: i18n.text("toolbar_upload"),
            clear: i18n.text("input_menu_clear"),
        }
    }
}

impl gpui::Global for MenuLabels {}

/// An open right-click menu: where it was opened and what the clipboard held
/// at that moment (reading it on every frame would be wasteful).
struct InputMenu {
    position: Point<Pixels>,
    clipboard_text: bool,
    clipboard_image: bool,
}

#[derive(Clone, Copy)]
enum MenuChoice {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Upload,
    Clear,
}

/// Whether pasting one long Amazon product link into a message draft inserts
/// its short form (`shorten_amazon_urls`, #315).
pub struct ShortenPastedAmazon(pub bool);

impl gpui::Global for ShortenPastedAmazon {}

/// Whether pasting several lines into a message draft asks first
/// (`confirm_multiline_paste`, #605).
pub struct ConfirmMultilinePaste(pub bool);

impl gpui::Global for ConfirmMultilinePaste {}

/// A multi-line text pasted into a draft that confirms such pastes. Nothing
/// was inserted; the enclosing window asks, or calls
/// [`TextInput::insert_pasted`] when it cannot.
pub struct MultilinePaste(pub String);

impl gpui::EventEmitter<MultilinePaste> for TextInput {}

/// Pasted text as one line: every line break (a CRLF counts once) becomes a
/// single space.
pub fn join_pasted_lines(text: &str) -> String {
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

#[derive(Clone)]
struct EditSnapshot {
    content: SharedString,
    selection: Range<usize>,
    reversed: bool,
}

#[derive(Clone)]
struct NickCompletion {
    start: usize,
    end: usize,
    matches: Vec<String>,
    index: usize,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// How far the text is shifted left so the cursor stays visible.
    scroll_x: Pixels,
    is_selecting: bool,
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
    completion: Option<NickCompletion>,
    secret: bool,
    /// Draft inputs let an image-only paste bubble up to the chat window's
    /// attachment flow instead of ignoring it.
    attachments: bool,
    /// A message draft, the only input that shortens pasted Amazon links.
    draft: bool,
    double_click_selects_all: bool,
    prefers_direct_input: bool,
    native_settings_style: bool,
    /// Draws a frame around the field (the dialog's fields).
    bordered: bool,
    /// A wrapped, vertically scrolling text of several lines; Enter is
    /// bound to a line break by the window that hosts it.
    multiline: bool,
    /// A quote mark that a new line repeats (with a space) when the line the
    /// caret is on starts with it (multi-line only).
    line_quote: Option<String>,
    /// How many lines the box shows (multi-line only).
    rows: usize,
    wrapped: Option<WrappedText>,
    /// How far the text is shifted up (multi-line only).
    scroll_y: Pixels,
    /// The cursor the view last followed, so a wheel scroll is not undone.
    followed: Option<(usize, usize)>,
    /// The x Up and Down aim for while the caret moves between lines.
    goal_x: Option<Pixels>,
    menu: Option<InputMenu>,
}

impl TextInput {
    pub fn is_composing(&self) -> bool {
        self.marked_range.is_some()
    }

    /// Pastes the clipboard at the caret, replacing the selection, exactly as
    /// the Paste shortcut does. Nothing is sent.
    pub fn paste_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.paste(&Paste, window, cx);
    }

    pub fn complete_nickname(
        &mut self,
        members: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.marked_range.is_some() || !self.selected_range.is_empty() {
            return;
        }
        let cursor = self.cursor_offset();
        if let Some(previous) = self.completion.clone()
            && cursor == previous.end
            && self.content[previous.start..previous.end] == previous.matches[previous.index]
        {
            let index = (previous.index + 1) % previous.matches.len();
            let replacement = previous.matches[index].clone();
            self.selected_range = previous.start..previous.end;
            self.replace_text_in_range(None, &replacement, window, cx);
            self.completion = Some(NickCompletion {
                start: previous.start,
                end: previous.start + replacement.len(),
                matches: previous.matches,
                index,
            });
            return;
        }

        let start = nickname_start(&self.content, cursor);
        let prefix = &self.content[start..cursor];
        let matches = nickname_matches(prefix, members);
        if matches.is_empty() {
            return;
        }
        let replacement = matches[0].clone();
        self.selected_range = start..cursor;
        self.replace_text_in_range(None, &replacement, window, cx);
        self.completion = Some(NickCompletion {
            start,
            end: start + replacement.len(),
            matches,
            index: 0,
        });
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn selection(&self) -> Range<usize> {
        self.selected_range.clone()
    }

    /// Where the input was last painted.
    #[cfg(test)]
    pub fn last_bounds(&self) -> Option<Bounds<Pixels>> {
        self.last_bounds
    }

    /// Lets an image-only paste reach the enclosing view's attachment
    /// handler instead of being ignored.
    pub fn accept_pasted_images(&mut self) {
        self.attachments = true;
    }

    /// A double click selects the whole text. Meant for short codes such as
    /// `#RRGGBB`; other inputs keep the plain click behavior.
    pub fn select_all_on_double_click(&mut self) {
        self.double_click_selects_all = true;
    }

    /// Detaches the platform IME while this input has focus, for fields that
    /// mostly take ASCII (nicknames, hosts, color codes). It only helps typing:
    /// pasted or existing text is never restricted. Off by default.
    pub fn prefer_direct_input(&mut self) {
        self.prefers_direct_input = true;
    }

    pub fn set_placeholder(&mut self, placeholder: &str, cx: &mut Context<Self>) {
        self.placeholder = placeholder.to_owned().into();
        cx.notify();
    }

    pub fn clear_after_send(&mut self, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.completion = None;
        self.undo.clear();
        self.redo.clear();
        cx.notify();
    }

    /// Inserts `link` at the cursor without replacing any selection (which is
    /// dropped), keeping one space between it and neighbouring text. The cursor
    /// stays where it was, before the inserted text. Undo restores the
    /// previous text.
    pub fn insert_link(&mut self, link: &str, cx: &mut Context<Self>) {
        self.record_edit();
        let at = self.cursor_offset();
        let before = self.content[..at].chars().next_back();
        let after = self.content[at..].chars().next();
        let text = cayenchat_app::attachments::link_insertion(before, after, link);
        self.content = (self.content[..at].to_owned() + &text + &self.content[at..]).into();
        self.selected_range = at..at;
        self.selection_reversed = false;
        self.marked_range = None;
        self.completion = None;
        cx.notify();
    }

    pub fn set_text(&mut self, value: &str, cx: &mut Context<Self>) {
        self.content = value.to_owned().into();
        self.selected_range = value.len()..value.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.completion = None;
        self.goal_x = None;
        self.undo.clear();
        self.redo.clear();
        cx.notify();
    }

    /// Replaces the whole text as one edit that can be undone. The caret stays
    /// at its offset, or at the end of a shorter text.
    pub fn replace_text(&mut self, value: &str, cx: &mut Context<Self>) {
        if self.content != value {
            self.record_edit();
        }
        let mut cursor = self.cursor_offset().min(value.len());
        while !value.is_char_boundary(cursor) {
            cursor -= 1;
        }
        self.content = value.to_owned().into();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.completion = None;
        self.goal_x = None;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_everything(cx);
    }

    /// Selects the whole text, so typing replaces it.
    pub fn select_everything(&mut self, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    /// Where Home goes: the start of the text, or of the line (up to a line
    /// break, not of the wrapped row) the caret is on in a multi-line input.
    /// A wrapped row's end has the same offset as the next row's start, so
    /// stopping there would draw the caret on the wrong row.
    fn home_offset(&self) -> usize {
        if !self.multiline {
            return 0;
        }
        let before = &self.content[..self.cursor_offset()];
        before.rfind('\n').map_or(0, |at| at + 1)
    }

    /// Where End goes (see [`Self::home_offset`]).
    fn end_offset(&self) -> usize {
        if !self.multiline {
            return self.content.len();
        }
        let cursor = self.cursor_offset();
        self.content[cursor..]
            .find('\n')
            .map_or(self.content.len(), |at| cursor + at)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.home_offset(), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.end_offset(), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.home_offset(), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.end_offset(), cx);
    }

    fn document_start(&mut self, _: &DocumentStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn document_end(&mut self, _: &DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_document_start(
        &mut self,
        _: &SelectDocumentStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(0, cx);
    }

    fn select_document_end(
        &mut self,
        _: &SelectDocumentEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.content.len(), cx);
    }

    /// Moves the caret `rows` rows down (up when negative), keeping to the
    /// same x; past the first or last row it goes to the start or end of the
    /// text. Selects up to there when `select`.
    fn move_rows(&mut self, rows: isize, select: bool, cx: &mut Context<Self>) {
        let Some(wrapped) = self.wrapped.as_ref() else {
            return;
        };
        let here = wrapped.point_for_offset(self.cursor_offset());
        let x = self.goal_x.unwrap_or(here.x);
        let y = here.y + wrapped.line_height() * rows as f32 + wrapped.line_height() / 2.;
        let target = if y < px(0.) {
            0
        } else if y >= wrapped.height() {
            self.content.len()
        } else {
            wrapped.offset_in_row(point(x, y))
        };
        if select {
            self.select_to(target, cx);
        } else {
            self.move_to(target, cx);
        }
        self.goal_x = Some(x);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_rows(-1, false, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_rows(1, false, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_rows(-1, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_rows(1, true, cx);
    }

    fn insert_newline(&mut self, _: &InsertNewline, window: &mut Window, cx: &mut Context<Self>) {
        let mut line = String::from("\n");
        if let Some(quote) = self.line_quote.as_deref() {
            let cursor = self.selected_range.start;
            let start = self.content[..cursor].rfind('\n').map_or(0, |at| at + 1);
            let prefix = quote.len() + 1;
            // Only after the quote and its space; inside them a break just
            // splits the line.
            if cursor >= start + prefix
                && self.content[start..cursor]
                    .strip_prefix(quote)
                    .is_some_and(|rest| rest.starts_with(' '))
            {
                line.push_str(quote);
                line.push(' ');
            }
        }
        self.replace_text_in_range(None, &line, window, cx);
    }

    /// The hard line (up to its line break) around `offset` in a multi-line
    /// input; in a single-line input the whole text.
    fn hard_line(&self, offset: usize) -> Range<usize> {
        if !self.multiline {
            return 0..self.content.len();
        }
        let start = self.content[..offset].rfind('\n').map_or(0, |at| at + 1);
        let end = self.content[offset..]
            .find('\n')
            .map_or(self.content.len(), |at| offset + at);
        start..end
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selected_range.is_empty() {
            word_boundary_left(&self.content, self.cursor_offset())
        } else {
            self.selected_range.start
        };
        self.move_to(offset, cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selected_range.is_empty() {
            word_boundary_right(&self.content, self.cursor_offset())
        } else {
            self.selected_range.end
        };
        self.move_to(offset, cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(word_boundary_left(&self.content, self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(word_boundary_right(&self.content, self.cursor_offset()), cx);
    }

    fn delete_word_backward(
        &mut self,
        _: &DeleteWordBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(word_boundary_left(&self.content, self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_forward(
        &mut self,
        _: &DeleteWordForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(word_boundary_right(&self.content, self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_beginning(
        &mut self,
        _: &DeleteToBeginning,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(self.hard_line(self.cursor_offset()).start, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    /// Deletes to the end of the line; at the end of a line of a multi-line
    /// input it deletes the line break instead, as Emacs does.
    fn delete_to_end(&mut self, _: &DeleteToEnd, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            let end = self.hard_line(cursor).end;
            let end = if end == cursor && cursor < self.content.len() {
                self.next_boundary(cursor)
            } else {
                end
            };
            self.select_to(end, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_line(&mut self, _: &DeleteLine, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.hard_line(self.cursor_offset());
        self.move_to(line.start, cx);
        self.select_to(line.end, cx);
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_whitespace(
        &mut self,
        _: &DeleteWhitespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_whitespace("", window, cx);
    }

    fn collapse_whitespace(
        &mut self,
        _: &CollapseWhitespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_whitespace(" ", window, cx);
    }

    /// Replaces the whitespace run around the cursor, like GTK's
    /// `delete-from-cursor (whitespace)` used by the Emacs key theme.
    fn replace_whitespace(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let range = whitespace_around(&self.content, self.cursor_offset());
        if self.content[range.clone()] == *text {
            return;
        }
        self.move_to(range.start, cx);
        self.select_to(range.end, cx);
        self.replace_text_in_range(None, text, window, cx);
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            content: self.content.clone(),
            selection: self.selected_range.clone(),
            reversed: self.selection_reversed,
        }
    }

    fn record_edit(&mut self) {
        if self.marked_range.is_none() {
            if self.undo.len() == 100 {
                self.undo.remove(0);
            }
            self.undo.push(self.snapshot());
            self.redo.clear();
        }
    }

    fn restore(&mut self, snapshot: EditSnapshot, cx: &mut Context<Self>) {
        self.content = snapshot.content;
        self.selected_range = snapshot.selection;
        self.selection_reversed = snapshot.reversed;
        self.marked_range = None;
        self.completion = None;
        self.goal_x = None;
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    pub(crate) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if self.double_click_selects_all && event.click_count >= 2 && !event.modifiers.shift {
            // Not a drag start, so moving the mouse keeps the selection.
            self.is_selecting = false;
            self.select_everything(cx);
            return;
        }
        self.is_selecting = true;

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    /// Opens the edit menu at the pointer. The field takes the focus, the
    /// selection stays, so a selected text can be cut or copied from it.
    fn open_menu(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        let item = cx.read_from_clipboard();
        self.menu = Some(InputMenu {
            position: event.position,
            clipboard_text: item.as_ref().is_some_and(|item| item.text().is_some()),
            clipboard_image: item
                .as_ref()
                .is_some_and(|item| clipboard_image(item).is_some()),
        });
        cx.notify();
    }

    /// Runs a menu entry as the matching action on the focused field, so it
    /// does exactly what the shortcut does.
    fn choose(&mut self, choice: MenuChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        cx.notify();
        window.focus(&self.focus_handle);
        let action: Box<dyn gpui::Action> = match choice {
            MenuChoice::Undo => Box::new(Undo),
            MenuChoice::Redo => Box::new(Redo),
            MenuChoice::Cut => Box::new(Cut),
            MenuChoice::Copy => Box::new(Copy),
            MenuChoice::Paste => Box::new(Paste),
            MenuChoice::Delete => Box::new(Delete),
            MenuChoice::SelectAll => Box::new(SelectAll),
            MenuChoice::Upload => Box::new(UploadImage),
            MenuChoice::Clear => Box::new(DeleteLine),
        };
        window.dispatch_action(action, cx);
    }

    /// The menu's entries in groups, each with whether it can be used now.
    fn menu_entries(
        &self,
        menu: &InputMenu,
        labels: &MenuLabels,
    ) -> Vec<Vec<(MenuChoice, String, bool)>> {
        let selected = !self.selected_range.is_empty();
        // A secret never reaches the clipboard (see `copy`).
        let may_copy = selected && !self.secret;
        let can_paste = menu.clipboard_text || (self.attachments && menu.clipboard_image);
        let mut groups = vec![
            vec![
                (MenuChoice::Undo, labels.undo.clone(), !self.undo.is_empty()),
                (MenuChoice::Redo, labels.redo.clone(), !self.redo.is_empty()),
            ],
            vec![
                (MenuChoice::Cut, labels.cut.clone(), may_copy),
                (MenuChoice::Copy, labels.copy.clone(), may_copy),
                (MenuChoice::Paste, labels.paste.clone(), can_paste),
                (MenuChoice::Delete, labels.delete.clone(), selected),
            ],
            vec![(
                MenuChoice::SelectAll,
                labels.select_all.clone(),
                !self.content.is_empty(),
            )],
        ];
        if self.draft {
            groups.push(vec![
                (MenuChoice::Upload, labels.upload.clone(), true),
                (
                    MenuChoice::Clear,
                    labels.clear.clone(),
                    !self.content.is_empty(),
                ),
            ]);
        }
        groups
    }

    fn render_menu(&self, menu: &InputMenu, cx: &mut Context<Self>) -> gpui::AnyElement {
        let labels = cx.try_global::<MenuLabels>().cloned().unwrap_or_else(|| {
            MenuLabels::new(&crate::localization::Localizer::new(
                cayenchat_storage::Language::English,
            ))
        });
        let theme = crate::theme::current(cx);
        let mut popup = div()
            .id("input-context-menu")
            .occlude()
            .w(px(200.))
            .p_1()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(theme.text)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }));
        for (group, entries) in self.menu_entries(menu, &labels).into_iter().enumerate() {
            if group > 0 {
                popup = popup.child(div().my_1().border_t_1().border_color(theme.separator));
            }
            for (index, (choice, label, enabled)) in entries.into_iter().enumerate() {
                popup = popup.child(
                    div()
                        .id(("input-menu-item", group * 8 + index))
                        .px_2()
                        .py_1()
                        .child(label)
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(theme.hover_strong))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.choose(choice, window, cx)
                                }))
                        })
                        .when(!enabled, |d| d.text_color(theme.text_muted)),
                );
            }
        }
        crate::snapped_menu(menu.position, popup).into_any_element()
    }

    /// Scrolls a multi-line input by `dy` (down when positive), within the
    /// text.
    fn scroll_by(&mut self, dy: Pixels, cx: &mut Context<Self>) {
        let (Some(wrapped), Some(bounds)) = (self.wrapped.as_ref(), self.last_bounds) else {
            return;
        };
        let most = (wrapped.height() - bounds.size.height).max(px(0.));
        let scroll_y = (self.scroll_y + dy).clamp(px(0.), most);
        if scroll_y != self.scroll_y {
            self.scroll_y = scroll_y;
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.is_selecting {
            if let Some(bounds) = self.last_bounds.filter(|_| self.multiline) {
                // A drag past the top or bottom scrolls on.
                let beyond =
                    event.position.y - event.position.y.clamp(bounds.top(), bounds.bottom());
                self.scroll_by(beyond, cx);
            }
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    /// Inserts pasted text at the caret as one line (shortening a lone long
    /// Amazon link in a draft when that is on).
    pub fn insert_pasted(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let short = (self.draft && cx.try_global::<ShortenPastedAmazon>().is_some_and(|g| g.0))
            .then(|| cayenchat_app::amazon::shorten_pasted(text))
            .flatten();
        let start = self.selected_range.start;
        if self.multiline {
            self.replace_text_in_range(None, text, window, cx);
        } else {
            self.replace_text_in_range(None, &join_pasted_lines(text), window, cx);
        }
        if let Some(short) = short {
            // A separate edit, so one undo gives back the long link.
            self.selected_range = start..self.selected_range.end;
            self.replace_text_in_range(None, &short, window, cx);
        }
    }

    /// Text wins when the clipboard holds both text and an image, matching
    /// what text fields elsewhere paste. An image-only clipboard in a draft
    /// propagates the action to the chat window's attachment flow.
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            if self.draft
                && text.trim().contains(['\r', '\n'])
                && cx
                    .try_global::<ConfirmMultilinePaste>()
                    .is_some_and(|g| g.0)
            {
                cx.emit(MultilinePaste(text));
                return;
            }
            self.insert_pasted(&text, window, cx);
        } else if self.attachments && clipboard_image(&item).is_some() {
            cx.propagate();
        }
    }

    // A secret never reaches the clipboard, where clipboard history and other
    // applications could read it; copy and cut do nothing, like the system
    // password fields.
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.secret && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.secret && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.completion = None;
        self.goal_x = None;
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if self.multiline {
            let (Some(bounds), Some(wrapped)) = (self.last_bounds.as_ref(), self.wrapped.as_ref())
            else {
                return 0;
            };
            let y = position.y.clamp(bounds.top(), bounds.bottom());
            return wrapped.offset_for_point(point(
                position.x - bounds.left(),
                y - bounds.top() + self.scroll_y,
            ));
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        // Past either edge a drag selects to that end of the text. The text
        // scrolls only when painted, so a pointer held still outside the box
        // would otherwise stop at the first visible character.
        if position.x < bounds.left() {
            return 0;
        }
        if position.x > bounds.right() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left() + self.scroll_x)
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.completion = None;
        self.goal_x = None;
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    /// Inserted text as the input keeps it: line breaks become spaces, or in
    /// a multi-line input all become `\n`.
    fn single_line_or_normalized(&self, text: &str) -> String {
        if self.multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n'], " ")
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

/// The first image in a clipboard item, if any.
pub fn clipboard_image(item: &ClipboardItem) -> Option<&gpui::Image> {
    item.entries().iter().find_map(|entry| match entry {
        gpui::ClipboardEntry::Image(image) => Some(image),
        _ => None,
    })
}

fn word_boundary_left(text: &str, offset: usize) -> usize {
    text.unicode_word_indices()
        .take_while(|(start, _)| *start < offset)
        .last()
        .map(|(start, _)| start)
        .unwrap_or(0)
}

fn word_boundary_right(text: &str, offset: usize) -> usize {
    text.unicode_word_indices()
        .find(|(start, word)| start + word.len() > offset)
        .map(|(start, word)| start + word.len())
        .unwrap_or(text.len())
}

fn whitespace_around(text: &str, offset: usize) -> Range<usize> {
    let start = text[..offset].trim_end_matches(char::is_whitespace).len();
    let end = text.len() - text[offset..].trim_start_matches(char::is_whitespace).len();
    start..end
}

fn nickname_start(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_alphanumeric() && !"_-[]\\`^{}|".contains(*ch))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0)
}

fn nickname_matches(prefix: &str, members: &[String]) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let prefix = prefix.to_lowercase();
    members
        .iter()
        .map(|member| member.trim_start_matches(|ch| "@+%&~".contains(ch)))
        .filter(|nick| nick.to_lowercase().starts_with(&prefix))
        .map(str::to_owned)
        .collect()
}

impl EntityInputHandler for TextInput {
    fn prefers_direct_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.prefers_direct_input
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        let range_utf16 = self.range_to_utf16(&range);
        // The input method sees a secret only as masks of the same length.
        let text = if self.secret {
            "*".repeat(range_utf16.len())
        } else {
            self.content[range].to_string()
        };
        actual_range.replace(range_utf16);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // Observers (the log search) hold back while composing, so ending the
        // composition without a text change must still tell them.
        if self.marked_range.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Return is handled by an action and must not insert a stray space.
        if !self.multiline && (new_text == "\n" || new_text == "\r") {
            return;
        }
        self.completion = None;
        self.goal_x = None;
        let new_text = self.single_line_or_normalized(new_text);
        let new_text = new_text.as_str();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        if &self.content[range.clone()] != new_text {
            self.record_edit();
        }
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.completion = None;
        self.goal_x = None;
        let new_text = self.single_line_or_normalized(new_text);
        let new_text = new_text.as_str();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.record_edit();
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|selection| {
                let offset = |count| {
                    new_text
                        .chars()
                        .scan(0, |units, ch| {
                            let start = *units;
                            *units += ch.len_utf16();
                            Some((start, ch.len_utf8()))
                        })
                        .take_while(|(units, _)| *units < count)
                        .map(|(_, bytes)| bytes)
                        .sum::<usize>()
                };
                range.start + offset(selection.start)..range.start + offset(selection.end)
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        self.selection_reversed = false;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Another pane may stand in for this input (the log forwards its
        // input method here), so `bounds` can be that pane's; the painted
        // bounds are always this input's.
        let bounds = self.last_bounds.unwrap_or(bounds);
        let range = self.range_from_utf16(&range_utf16);
        if let Some(wrapped) = self.wrapped.as_ref().filter(|_| self.multiline) {
            // The candidate window goes under the range's first row.
            let origin = bounds.origin - point(px(0.), self.scroll_y);
            let start = origin + wrapped.point_for_offset(range.start);
            let end = origin + wrapped.point_for_offset(range.end);
            let right = if end.y == start.y { end.x } else { start.x };
            return Some(Bounds::from_corners(
                start,
                point(right, start.y + wrapped.line_height()),
            ));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start) - self.scroll_x,
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end) - self.scroll_x,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        if self.content.is_empty() {
            return Some(0);
        }
        if let Some(wrapped) = self.wrapped.as_ref().filter(|_| self.multiline) {
            let at =
                wrapped.offset_for_point(gpui::point(line_point.x, line_point.y + self.scroll_y));
            return Some(self.offset_to_utf16(at));
        }
        let last_layout = self.last_layout.as_ref()?;
        let mut utf8_index = last_layout
            .index_for_x(line_point.x + self.scroll_x)?
            .min(self.content.len());
        while !self.content.is_char_boundary(utf8_index) {
            utf8_index -= 1;
        }
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll_x: Pixels,
    /// A multi-line input's layout, the selection's rectangles and the
    /// vertical scroll it was drawn with.
    wrapped: Option<WrappedText>,
    rects: Vec<PaintQuad>,
    scroll_y: Pixels,
    followed: Option<(usize, usize)>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl TextElement {
    /// The multi-line input: shapes the wrapped text, scrolls it to keep the
    /// caret in view, and prepares the caret and the selection.
    fn prepaint_multiline(
        &self,
        display_text: SharedString,
        runs: &[TextRun],
        font_size: Pixels,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let input = self.input.read(cx);
        let cursor = input.cursor_offset();
        let followed = (cursor, input.content.len());
        let line_height = window.line_height();
        // Room for the caret after the last character of a row.
        let caret_width = px(2.);
        let wrap_width = (bounds.size.width - caret_width).max(px(1.));
        let lines = window
            .text_system()
            .shape_text(display_text, font_size, runs, Some(wrap_width), None)
            .unwrap_or_default();
        let wrapped = WrappedText::new(lines, line_height);

        // The view follows the caret when it or the text changed; otherwise a
        // scroll by the wheel stays.
        let mut scroll_y = input.scroll_y;
        let caret = wrapped.point_for_offset(cursor);
        if input.followed != Some(followed) {
            if caret.y < scroll_y {
                scroll_y = caret.y;
            }
            if caret.y + line_height > scroll_y + bounds.size.height {
                scroll_y = caret.y + line_height - bounds.size.height;
            }
        }
        let scroll_y = scroll_y
            .min((wrapped.height() - bounds.size.height).max(px(0.)))
            .max(px(0.));

        let origin = bounds.origin - point(px(0.), scroll_y);
        let (caret, rects) = if input.selected_range.is_empty() {
            let quad = fill(
                Bounds::new(origin + caret, size(caret_width, line_height)),
                caret_color(
                    input.palette(cx).dark,
                    input.native_settings_style.then(|| {
                        crate::settings_theme::current(cx)
                            .map(|n| {
                                gpui::Hsla::from(crate::settings_theme::color(n.input.caret_color))
                            })
                            .unwrap_or_else(gpui::blue)
                    }),
                    window.text_style().color,
                ),
            );
            (Some(quad), Vec::new())
        } else {
            let rects = wrapped
                .rects_for_range(&input.selected_range)
                .into_iter()
                .map(|rect| {
                    fill(
                        Bounds::new(origin + rect.origin, rect.size),
                        input.palette(cx).text_selection,
                    )
                })
                .collect();
            (None, rects)
        };
        PrepaintState {
            line: None,
            cursor: caret,
            selection: None,
            scroll_x: px(0.),
            wrapped: Some(wrapped),
            rects,
            scroll_y,
            followed: Some(followed),
        }
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = if self.input.read(cx).multiline {
            relative(1.).into()
        } else {
            window.line_height().into()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), input.palette(cx).placeholder)
        } else if input.secret {
            // Keep the display byte length aligned with the UTF-8 editing offsets.
            ("*".repeat(content.len()).into(), style.color)
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        if input.multiline {
            return self.prepaint_multiline(display_text, &runs, font_size, bounds, window, cx);
        }
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        // Keep the cursor inside the box; the text scrolls only when it is
        // longer than the box.
        let caret_width = px(2.);
        let visible = (bounds.size.width - caret_width).max(px(0.));
        let mut scroll_x = input.scroll_x;
        if cursor_pos - scroll_x > visible {
            scroll_x = cursor_pos - visible;
        }
        if cursor_pos < scroll_x {
            scroll_x = cursor_pos;
        }
        let scroll_x = scroll_x.min((line.width - visible).max(px(0.))).max(px(0.));
        let origin_x = bounds.left() - scroll_x;
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(origin_x + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    caret_color(
                        input.palette(cx).dark,
                        input.native_settings_style.then(|| {
                            crate::settings_theme::current(cx)
                                .map(|n| {
                                    gpui::Hsla::from(crate::settings_theme::color(
                                        n.input.caret_color,
                                    ))
                                })
                                .unwrap_or_else(gpui::blue)
                        }),
                        style.color,
                    ),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            origin_x + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            origin_x + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    input.palette(cx).text_selection,
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
            scroll_x,
            wrapped: None,
            rects: Vec::new(),
            scroll_y: px(0.),
            followed: None,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        crate::focus_reveal::note(&focus_handle, bounds, window, cx);
        if let Some(wrapped) = prepaint.wrapped.take() {
            self.paint_multiline(bounds, wrapped, prepaint, window, cx);
            return;
        }
        let scroll_x = prepaint.scroll_x;
        let line = prepaint.line.take().unwrap();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection)
            }
            let origin = point(bounds.left() - scroll_x, bounds.top());
            line.paint(origin, window.line_height(), window, cx)
                .unwrap();

            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
            input.scroll_x = scroll_x;
        });

        self.track_drag(window);
    }
}

impl TextElement {
    fn paint_multiline(
        &self,
        bounds: Bounds<Pixels>,
        wrapped: WrappedText,
        prepaint: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for rect in prepaint.rects.drain(..) {
                window.paint_quad(rect);
            }
            wrapped.paint(bounds.origin - point(px(0.), prepaint.scroll_y), window, cx);
            if focus_handle.is_focused(window)
                && let Some(caret) = prepaint.cursor.take()
            {
                window.paint_quad(caret);
            }
        });
        let (scroll_y, followed) = (prepaint.scroll_y, prepaint.followed);
        self.input.update(cx, |input, _cx| {
            input.wrapped = Some(wrapped);
            input.last_bounds = Some(bounds);
            input.scroll_y = scroll_y;
            input.followed = followed;
        });
        self.track_drag(window);
    }

    /// A drag keeps selecting after the pointer leaves the field (a div's
    /// own move handler only fires while hovered, which dropped the first
    /// characters when the pointer overshot the left edge) and ends on a
    /// release anywhere in the window. The release position is applied
    /// too: a fast drag coalesces move events, so the last one can stop
    /// short of where the button came up.
    fn track_drag(&self, window: &mut Window) {
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Capture {
                input.update(cx, |input, cx| input.on_mouse_move(event, cx));
            }
        });
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Capture {
                input.update(cx, |input, cx| {
                    if input.is_selecting {
                        input.select_to(input.index_for_mouse_position(event.position), cx);
                    }
                    input.is_selecting = false;
                });
            }
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The platform ends a composition when focus moves to another field
        // and does not deliver the commit here; keep the text, drop the mark.
        if self.marked_range.is_some() && !self.focus_handle.is_focused(window) {
            self.marked_range = None;
            cx.notify();
        }
        let menu = self.menu.as_ref().map(|menu| self.render_menu(menu, cx));
        let theme = self.palette(cx);
        let native = self
            .native_settings_style
            .then(|| crate::settings_theme::current(cx))
            .flatten();
        let focused = native.is_some() && self.focus_handle.is_focused(window);
        let font_size = native.map_or(13., |n| n.input.font.size);
        let line_height = (font_size * 1.4).max(20.);
        // The menu is outside the field, which clips what it draws.
        div()
            .w_full()
            .child(
                div()
                    .flex()
                    .key_context(if self.multiline {
                        "TextInput MultilineInput"
                    } else {
                        "TextInput"
                    })
                    .track_focus(&self.focus_handle(cx))
                    // Escape closes the menu before anything else sees the key.
                    .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" && this.menu.take().is_some() {
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::open_menu))
                    .cursor(CursorStyle::IBeam)
                    .on_action(cx.listener(Self::backspace))
                    .on_action(cx.listener(Self::delete))
                    .on_action(cx.listener(Self::left))
                    .on_action(cx.listener(Self::right))
                    .on_action(cx.listener(Self::select_left))
                    .on_action(cx.listener(Self::select_right))
                    .on_action(cx.listener(Self::select_all))
                    .on_action(cx.listener(Self::home))
                    .on_action(cx.listener(Self::end))
                    .on_action(cx.listener(Self::select_home))
                    .on_action(cx.listener(Self::select_end))
                    .on_action(cx.listener(Self::word_left))
                    .on_action(cx.listener(Self::word_right))
                    .on_action(cx.listener(Self::select_word_left))
                    .on_action(cx.listener(Self::select_word_right))
                    .on_action(cx.listener(Self::delete_word_backward))
                    .on_action(cx.listener(Self::delete_word_forward))
                    .on_action(cx.listener(Self::delete_to_beginning))
                    .on_action(cx.listener(Self::delete_to_end))
                    .on_action(cx.listener(Self::delete_line))
                    .on_action(cx.listener(Self::delete_whitespace))
                    .on_action(cx.listener(Self::collapse_whitespace))
                    .on_action(cx.listener(Self::undo))
                    .on_action(cx.listener(Self::redo))
                    .on_action(cx.listener(Self::show_character_palette))
                    .on_action(cx.listener(Self::up))
                    .on_action(cx.listener(Self::down))
                    .on_action(cx.listener(Self::select_up))
                    .on_action(cx.listener(Self::select_down))
                    .on_action(cx.listener(Self::insert_newline))
                    .on_action(cx.listener(Self::document_start))
                    .on_action(cx.listener(Self::document_end))
                    .on_action(cx.listener(Self::select_document_start))
                    .on_action(cx.listener(Self::select_document_end))
                    .on_scroll_wheel(cx.listener(
                        |this, event: &gpui::ScrollWheelEvent, window, cx| {
                            if this.multiline {
                                let delta = event.delta.pixel_delta(window.line_height());
                                this.scroll_by(-delta.y, cx);
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .on_action(cx.listener(Self::paste))
                    .on_action(cx.listener(Self::cut))
                    .on_action(cx.listener(Self::copy))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .bg(theme.window)
                    .w_full()
                    .overflow_hidden()
                    .line_height(px(line_height))
                    .text_size(px(font_size))
                    .when(self.native_settings_style, |d| d.text_color(theme.text))
                    .when_some(native, |d, n| d.font_family(n.input.font.family.clone()))
                    .child(
                        div()
                            .h(px(if self.multiline {
                                line_height * self.rows as f32 + 10.
                            } else {
                                native.map_or(28., |n| n.input.min_height.max(line_height + 10.))
                            }))
                            .w_full()
                            .p(px(4.))
                            .bg(theme.surface)
                            .when(self.bordered, |d| d.border_1().border_color(theme.border))
                            .when_some(native, |d, n| {
                                d.rounded(px(n.input.border.corner_radius))
                                    .border(px(n.input.border.line_width.max(1.)))
                                    .border_color(crate::settings_theme::color(if focused {
                                        n.input
                                            .focus_border_color
                                            .unwrap_or(n.defaults.focus_ring_color)
                                    } else {
                                        n.input.border.color
                                    }))
                            })
                            .child(TextElement { input: cx.entity() }),
                    ),
            )
            .children(menu)
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TextInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: "Message draft — offline mock, sending disabled".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.),
            is_selecting: false,
            undo: Vec::new(),
            redo: Vec::new(),
            completion: None,
            secret: false,
            attachments: false,
            draft: false,
            double_click_selects_all: false,
            prefers_direct_input: false,
            native_settings_style: false,
            bordered: false,
            multiline: false,
            line_quote: None,
            rows: 1,
            wrapped: None,
            scroll_y: px(0.),
            followed: None,
            goal_x: None,
            menu: None,
        }
    }

    /// A message draft, which also accepts pasted images.
    pub fn new_live(placeholder: &str, cx: &mut Context<Self>) -> Self {
        let mut input = Self::new(cx);
        input.placeholder = placeholder.to_owned().into();
        input.attachments = true;
        input.draft = true;
        input
    }

    fn palette(&self, cx: &App) -> crate::theme::Theme {
        if self.native_settings_style {
            crate::settings_theme::input_palette(cx)
        } else {
            crate::theme::current(cx)
        }
    }

    pub fn new_settings_field(
        placeholder: &str,
        value: &str,
        secret: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = Self::new_field(placeholder, value, secret, cx);
        input.native_settings_style = true;
        // Tab order follows paint order within the settings window.
        input.focus_handle = cx.focus_handle().tab_stop(true);
        input
    }

    pub fn new_field(placeholder: &str, value: &str, secret: bool, cx: &mut Context<Self>) -> Self {
        let mut input = Self::new(cx);
        input.placeholder = placeholder.to_owned().into();
        input.content = value.to_owned().into();
        input.selected_range = value.len()..value.len();
        input.secret = secret;
        input
    }

    /// A field with a frame, so that it reads as an input.
    pub fn bordered(mut self) -> Self {
        self.bordered = true;
        self
    }

    /// A framed text of several lines that wraps and scrolls inside a box of
    /// `rows` lines.
    pub fn new_multiline(value: &str, rows: usize, cx: &mut Context<Self>) -> Self {
        let mut input = Self::new_field("", value, false, cx).bordered();
        input.multiline = true;
        input.rows = rows.max(1);
        input
    }

    /// Makes Enter continue a quoted line with `quote` and a space.
    pub fn set_line_quote(&mut self, quote: Option<String>) {
        self.line_quote = quote;
    }
}

/// Binds draft editing keys. `emacs` adds the GTK Emacs key theme on Linux;
/// macOS always has its Emacs-style Control bindings.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub fn bind_keys(emacs: bool, cx: &mut App) {
    // GPUI 0.2.2 does not forward macOS text command selectors to this editor.
    // Keep editing bindings local to the focused input and reserve app shortcuts.
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("secondary-a", SelectAll, Some("TextInput")),
        KeyBinding::new("secondary-v", Paste, Some("TextInput")),
        KeyBinding::new("secondary-c", Copy, Some("TextInput")),
        KeyBinding::new("secondary-x", Cut, Some("TextInput")),
        KeyBinding::new("secondary-z", Undo, Some("TextInput")),
        KeyBinding::new("secondary-shift-z", Redo, Some("TextInput")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("ctrl-a", Home, Some("TextInput")),
        KeyBinding::new("ctrl-e", End, Some("TextInput")),
        KeyBinding::new("ctrl-b", Left, Some("TextInput")),
        KeyBinding::new("ctrl-f", Right, Some("TextInput")),
        KeyBinding::new("ctrl-h", Backspace, Some("TextInput")),
        KeyBinding::new("ctrl-d", Delete, Some("TextInput")),
        KeyBinding::new("ctrl-k", DeleteToEnd, Some("TextInput")),
        KeyBinding::new("ctrl-w", DeleteWordBackward, Some("TextInput")),
        KeyBinding::new("alt-left", WordLeft, Some("TextInput")),
        KeyBinding::new("alt-right", WordRight, Some("TextInput")),
        KeyBinding::new("alt-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("alt-shift-right", SelectWordRight, Some("TextInput")),
        KeyBinding::new("alt-backspace", DeleteWordBackward, Some("TextInput")),
        KeyBinding::new("alt-delete", DeleteWordForward, Some("TextInput")),
        KeyBinding::new("cmd-left", Home, Some("TextInput")),
        KeyBinding::new("cmd-right", End, Some("TextInput")),
        KeyBinding::new("cmd-shift-left", SelectHome, Some("TextInput")),
        KeyBinding::new("cmd-shift-right", SelectEnd, Some("TextInput")),
        KeyBinding::new("cmd-shift-up", SelectHome, Some("TextInput")),
        KeyBinding::new("cmd-shift-down", SelectEnd, Some("TextInput")),
        KeyBinding::new("cmd-backspace", DeleteToBeginning, Some("TextInput")),
        KeyBinding::new("alt-up", Home, Some("TextInput")),
        KeyBinding::new("alt-down", End, Some("TextInput")),
    ]);
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cx.bind_keys([
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("shift-home", SelectHome, Some("TextInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("TextInput")),
        KeyBinding::new("ctrl-left", WordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-right", WordRight, Some("TextInput")),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some("TextInput")),
        KeyBinding::new("ctrl-backspace", DeleteWordBackward, Some("TextInput")),
        KeyBinding::new("ctrl-delete", DeleteWordForward, Some("TextInput")),
        KeyBinding::new("ctrl-y", Redo, Some("TextInput")),
    ]);
    // Mirrors GTK 3's gtk-keys.css.emacs for GtkEntry. Bound last so these
    // win over Select All (Ctrl+A) and Redo (Ctrl+Y); GTK likewise moves
    // Select All to Ctrl+/ under this theme.
    #[cfg(target_os = "linux")]
    if emacs {
        cx.bind_keys([
            KeyBinding::new("ctrl-b", Left, Some("TextInput")),
            KeyBinding::new("ctrl-shift-b", SelectLeft, Some("TextInput")),
            KeyBinding::new("ctrl-f", Right, Some("TextInput")),
            KeyBinding::new("ctrl-shift-f", SelectRight, Some("TextInput")),
            KeyBinding::new("alt-b", WordLeft, Some("TextInput")),
            KeyBinding::new("alt-shift-b", SelectWordLeft, Some("TextInput")),
            KeyBinding::new("alt-f", WordRight, Some("TextInput")),
            KeyBinding::new("alt-shift-f", SelectWordRight, Some("TextInput")),
            KeyBinding::new("ctrl-a", Home, Some("TextInput")),
            KeyBinding::new("ctrl-shift-a", SelectHome, Some("TextInput")),
            KeyBinding::new("ctrl-e", End, Some("TextInput")),
            KeyBinding::new("ctrl-shift-e", SelectEnd, Some("TextInput")),
            KeyBinding::new("ctrl-w", Cut, Some("TextInput")),
            KeyBinding::new("ctrl-y", Paste, Some("TextInput")),
            KeyBinding::new("ctrl-d", Delete, Some("TextInput")),
            KeyBinding::new("alt-d", DeleteWordForward, Some("TextInput")),
            KeyBinding::new("ctrl-k", DeleteToEnd, Some("TextInput")),
            KeyBinding::new("ctrl-u", DeleteLine, Some("TextInput")),
            KeyBinding::new("ctrl-h", Backspace, Some("TextInput")),
            KeyBinding::new("alt-\\", DeleteWhitespace, Some("TextInput")),
            KeyBinding::new("alt-space", CollapseWhitespace, Some("TextInput")),
            KeyBinding::new("ctrl-/", SelectAll, Some("TextInput")),
        ]);
    }
}

/// Binds the keys of a multi-line input. Called after every other binding, so
/// that Enter, Up and Down win over the chat draft's (same depth, later wins).
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub fn bind_multiline_keys(emacs: bool, cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", InsertNewline, Some("MultilineInput")),
        KeyBinding::new("shift-enter", InsertNewline, Some("MultilineInput")),
        KeyBinding::new("up", Up, Some("MultilineInput")),
        KeyBinding::new("down", Down, Some("MultilineInput")),
        KeyBinding::new("shift-up", SelectUp, Some("MultilineInput")),
        KeyBinding::new("shift-down", SelectDown, Some("MultilineInput")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("ctrl-p", Up, Some("MultilineInput")),
        KeyBinding::new("ctrl-n", Down, Some("MultilineInput")),
        KeyBinding::new("ctrl-shift-p", SelectUp, Some("MultilineInput")),
        KeyBinding::new("ctrl-shift-n", SelectDown, Some("MultilineInput")),
        KeyBinding::new("cmd-up", DocumentStart, Some("MultilineInput")),
        KeyBinding::new("cmd-down", DocumentEnd, Some("MultilineInput")),
        KeyBinding::new("cmd-shift-up", SelectDocumentStart, Some("MultilineInput")),
        KeyBinding::new("cmd-shift-down", SelectDocumentEnd, Some("MultilineInput")),
    ]);
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-home", DocumentStart, Some("MultilineInput")),
        KeyBinding::new("ctrl-end", DocumentEnd, Some("MultilineInput")),
        KeyBinding::new(
            "ctrl-shift-home",
            SelectDocumentStart,
            Some("MultilineInput"),
        ),
        KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, Some("MultilineInput")),
    ]);
    #[cfg(target_os = "linux")]
    if emacs {
        cx.bind_keys([
            KeyBinding::new("ctrl-p", Up, Some("MultilineInput")),
            KeyBinding::new("ctrl-n", Down, Some("MultilineInput")),
            KeyBinding::new("ctrl-shift-p", SelectUp, Some("MultilineInput")),
            KeyBinding::new("ctrl-shift-n", SelectDown, Some("MultilineInput")),
        ]);
    }
}

/// Caret color: the text color on dark backgrounds, where blue and the OS
/// theme's caret color are hard to see; otherwise the OS color or blue.
fn caret_color(dark: bool, native: Option<gpui::Hsla>, text: gpui::Hsla) -> gpui::Hsla {
    if dark {
        text
    } else {
        native.unwrap_or_else(gpui::blue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn composition_mark_is_dropped_when_the_field_is_not_focused(cx: &mut gpui::TestAppContext) {
        use gpui::EntityInputHandler as _;
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new(cx));
        input.update_in(cx, |input, window, cx| {
            input.replace_and_mark_text_in_range(None, "かな", None, window, cx);
            assert!(input.is_composing());
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert!(!input.is_composing());
            assert_eq!(input.content.as_ref(), "かな");
        });
    }

    /// Dragging from the end past the left edge of the field, where the
    /// pointer is no longer over it, still selects the first character.
    #[gpui::test]
    fn drag_past_the_left_edge_selects_from_the_first_character(cx: &mut gpui::TestAppContext) {
        use gpui::{Modifiers, MouseButton};

        struct Host(Entity<TextInput>);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().pl(px(100.)).w(px(300.)).child(self.0.clone())
            }
        }

        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.content = "example".into();
            input
        });
        let (_host, cx) = cx.add_window_view(|_, _| Host(input.clone()));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        let end = point(px(100. + 4. + 150.), px(10.));
        cx.simulate_mouse_down(end, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(px(100. + 4. + 20.), px(10.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        // Outside the field: the pointer overshoots to the left.
        cx.simulate_mouse_move(
            point(px(20.), px(10.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(
            point(px(20.), px(10.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range, 0..7);
            assert!(!input.is_selecting);
        });
    }

    /// A fast drag can deliver no move event past the field's inner part; the
    /// release position alone must extend the selection to the start.
    #[gpui::test]
    fn release_position_extends_the_selection(cx: &mut gpui::TestAppContext) {
        use gpui::{Modifiers, MouseButton};

        struct Host(Entity<TextInput>);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().pl(px(100.)).w(px(300.)).child(self.0.clone())
            }
        }

        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.content = "example".into();
            input
        });
        let (_host, cx) = cx.add_window_view(|_, _| Host(input.clone()));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        cx.simulate_mouse_down(
            point(px(100. + 4. + 150.), px(10.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(
            point(px(100. + 4. + 60.), px(10.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(
            point(px(20.), px(10.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range, 0..7);
        });
    }

    /// A scrolled long text must still select to its start when the pointer
    /// is held outside the box, where no repaint scrolls it back.
    #[gpui::test]
    fn drag_outside_a_scrolled_text_selects_to_the_start(cx: &mut gpui::TestAppContext) {
        use gpui::{Modifiers, MouseButton};

        struct Host(Entity<TextInput>);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().pl(px(100.)).w(px(300.)).child(self.0.clone())
            }
        }

        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let text = "START-".to_string() + &"abcdefghijklmnopqrstuvwxyz-".repeat(8) + "END";
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.content = text.clone().into();
            input.selected_range = text.len()..text.len();
            input
        });
        let (_host, cx) = cx.add_window_view(|_, _| Host(input.clone()));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        input.read_with(cx, |input, _| assert!(input.scroll_x > px(0.)));

        cx.simulate_mouse_down(
            point(px(100. + 4. + 180.), px(10.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(
            point(px(20.), px(10.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range.start, 0);
        });
    }

    #[test]
    fn caret_uses_text_color_in_dark_inputs_only() {
        let text = gpui::white();
        let native = gpui::black();
        assert_eq!(caret_color(true, None, text), text);
        assert_eq!(caret_color(true, Some(native), text), text);
        assert_eq!(caret_color(false, None, text), gpui::blue());
        assert_eq!(caret_color(false, Some(native), text), native);
    }

    #[test]
    fn word_motion_respects_unicode_boundaries_and_skips_spaces() {
        let text = "hello  世界 🙂 rust";
        assert_eq!(
            word_boundary_left(text, text.len()),
            text.find("rust").unwrap()
        );
        assert_eq!(
            word_boundary_left(text, text.find("rust").unwrap()),
            text.find('界').unwrap()
        );
        assert_eq!(word_boundary_right(text, 0), "hello".len());
        assert_eq!(
            word_boundary_right(text, "hello".len()),
            text.find('界').unwrap()
        );
    }

    #[test]
    fn whitespace_run_spans_both_sides_of_the_cursor() {
        assert_eq!(whitespace_around("a  \t b", 2), 1..5);
        assert_eq!(whitespace_around("a b", 0), 0..0);
        assert_eq!(whitespace_around("  ", 2), 0..2);
    }

    #[test]
    fn nickname_candidates_use_current_word_and_strip_roles() {
        let draft = "hello, al";
        assert_eq!(nickname_start(draft, draft.len()), 7);
        assert_eq!(
            nickname_matches(
                &draft[7..],
                &["@alice".into(), "alex".into(), "+bob".into()]
            ),
            ["alice", "alex"]
        );
    }

    #[gpui::test]
    fn secret_fields_keep_their_text_from_the_clipboard_and_input_method(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let text_seen = |secret: bool, cx: &mut gpui::TestAppContext| {
            let (input, cx) =
                cx.add_window_view(|_, cx| TextInput::new_field("", "pä55", secret, cx));
            cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
            input.update_in(cx, |input, window, cx| {
                input.selected_range = 0..input.content.len();
                input.copy(&Copy, window, cx);
                let copied = cx.read_from_clipboard().and_then(|item| item.text());
                input.cut(&Cut, window, cx);
                let shown = input.text_for_range(0..4, &mut None, window, cx);
                (copied, input.content.to_string(), shown)
            })
        };
        assert_eq!(
            text_seen(true, cx),
            (Some("before".into()), "pä55".into(), Some("****".into()))
        );
        assert_eq!(
            text_seen(false, cx),
            (Some("pä55".into()), String::new(), Some(String::new()))
        );
    }

    const LONG_AMAZON: &str = "https://www.amazon.co.jp/SomeProduct/dp/B012345678/ref=sr_1_1?keywords=example&qid=1234567890123&sr=8-1";
    const SHORT_AMAZON: &str = "https://www.amazon.co.jp/dp/B012345678";

    /// The text of a draft (or another input) after pasting `clipboard` into
    /// "a  b" with the cursor between the spaces, and after one and two undos.
    fn paste_into(
        draft: bool,
        shorten: bool,
        clipboard: &str,
        cx: &mut gpui::TestAppContext,
    ) -> [String; 3] {
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
            cx.set_global(ShortenPastedAmazon(shorten));
        });
        let (input, cx) = cx.add_window_view(|_, cx| {
            if draft {
                TextInput::new_live("", cx)
            } else {
                TextInput::new_field("", "", false, cx)
            }
        });
        cx.write_to_clipboard(ClipboardItem::new_string(clipboard.into()));
        input.update_in(cx, |input, window, cx| {
            input.content = "a  b".into();
            input.selected_range = 2..2;
            input.paste(&Paste, window, cx);
            let pasted = input.content.to_string();
            input.undo(&Undo, window, cx);
            let once = input.content.to_string();
            input.undo(&Undo, window, cx);
            [pasted, once, input.content.to_string()]
        })
    }

    #[gpui::test]
    fn pasting_one_long_amazon_link_into_a_draft_inserts_the_short_one(
        cx: &mut gpui::TestAppContext,
    ) {
        for clipboard in [LONG_AMAZON.to_owned(), format!("\u{3000}{LONG_AMAZON}\n")] {
            let [pasted, once, twice] = paste_into(true, true, &clipboard, cx);
            assert_eq!(pasted, format!("a {SHORT_AMAZON} b"));
            // One undo gives back the long link, the next what was there before.
            assert_eq!(once, format!("a {} b", clipboard.replace('\n', " ")));
            assert_eq!(twice, "a  b");
        }
    }

    #[gpui::test]
    fn other_pastes_are_inserted_as_they_are(cx: &mut gpui::TestAppContext) {
        let mixed = format!("see {LONG_AMAZON} now");
        assert_eq!(
            paste_into(true, true, &mixed, cx)[0],
            format!("a {mixed} b")
        );
        // Option off, or an input that is not a message draft.
        for (draft, shorten) in [(true, false), (false, true)] {
            assert_eq!(
                paste_into(draft, shorten, LONG_AMAZON, cx)[0],
                format!("a {LONG_AMAZON} b")
            );
        }
    }

    /// A drawn multi-line input of `text`, 200px wide, so that its layout
    /// exists for the caret to move in.
    fn multiline<'a>(
        text: &str,
        cx: &'a mut gpui::TestAppContext,
    ) -> (Entity<TextInput>, &'a mut gpui::VisualTestContext) {
        struct Host(Entity<TextInput>);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().w(px(200.)).child(self.0.clone())
            }
        }
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let text = text.to_owned();
        let input = cx.new(|cx| TextInput::new_multiline(&text, 4, cx));
        let host = input.clone();
        let (_, cx) = cx.add_window_view(|_, _| Host(host));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (input, cx)
    }

    #[gpui::test]
    fn a_multiline_input_keeps_its_line_breaks(cx: &mut gpui::TestAppContext) {
        let (input, cx) = multiline("ab", cx);
        input.update_in(cx, |input, window, cx| {
            input.insert_newline(&InsertNewline, window, cx);
            input.replace_text_in_range(None, "c\r\nd\re", window, cx);
            assert_eq!(input.text(), "ab\nc\nd\ne");
            input.undo(&Undo, window, cx);
            assert_eq!(input.text(), "ab\n");
        });
        // A single-line input still turns line breaks into spaces.
        let (single, cx) = cx.add_window_view(|_, cx| TextInput::new_field("", "", false, cx));
        single.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "a\nb", window, cx);
            assert_eq!(input.text(), "a b");
        });
    }

    #[gpui::test]
    fn enter_continues_a_quoted_line(cx: &mut gpui::TestAppContext) {
        let (input, cx) = multiline("> a\nb\n> > c", cx);
        input.update_in(cx, |input, window, cx| {
            input.set_line_quote(Some(">".to_owned()));
            input.move_to(3, cx);
            input.insert_newline(&InsertNewline, window, cx);
            assert_eq!(input.text(), "> a\n> \nb\n> > c");
            // An unquoted line, and a caret inside the quote, add nothing.
            input.move_to(8, cx);
            input.insert_newline(&InsertNewline, window, cx);
            assert_eq!(input.text(), "> a\n> \nb\n\n> > c");
            input.move_to(1, cx);
            input.insert_newline(&InsertNewline, window, cx);
            assert_eq!(input.text(), ">\n a\n> \nb\n\n> > c");
        });
    }

    #[gpui::test]
    fn up_and_down_keep_the_column_across_lines(cx: &mut gpui::TestAppContext) {
        let (input, cx) = multiline("abcd\n\nef", cx);
        input.update_in(cx, |input, window, cx| {
            input.move_to(3, cx);
            input.down(&Down, window, cx);
            assert_eq!(input.selection(), 5..5, "the empty line");
            input.down(&Down, window, cx);
            assert_eq!(input.selection(), 8..8, "back to the goal column, clamped");
            input.down(&Down, window, cx);
            assert_eq!(input.selection(), 8..8, "the last line's end");
            input.up(&Up, window, cx);
            input.up(&Up, window, cx);
            assert_eq!(input.selection(), 3..3);
            input.select_down(&SelectDown, window, cx);
            assert_eq!(input.selection(), 3..5);
            input.move_to(1, cx);
            input.up(&Up, window, cx);
            assert_eq!(input.selection(), 0..0, "above the first line: its start");
        });
    }

    #[gpui::test]
    fn an_edit_resets_the_column_up_and_down_keep(cx: &mut gpui::TestAppContext) {
        let (input, cx) = multiline("abcdef\nabcdef\nabcdef", cx);
        input.update_in(cx, |input, window, cx| {
            input.move_to(2, cx);
            input.down(&Down, window, cx);
            assert_eq!(input.selection(), 9..9);
            input.replace_text_in_range(None, "ZZ", window, cx);
            assert_eq!(input.selection(), 11..11);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        input.update_in(cx, |input, window, cx| {
            input.down(&Down, window, cx);
            // Column 4 (after the typed ZZ) on the third line, not the old column 2.
            assert_eq!(input.selection(), 20..20, "the column after the edit");
            input.select_to(18, cx);
            assert_eq!(&input.text()[input.selection()], "cd");
        });
    }

    #[gpui::test]
    fn up_and_down_leave_a_short_wrapped_row(cx: &mut gpui::TestAppContext) {
        let text = "abc abcdefghijklmnopqrstuvwxyz ".repeat(4);
        let (input, cx) = multiline(&text, cx);
        input.update_in(cx, |input, window, cx| {
            input.move_to(0, cx);
            input.down(&Down, window, cx);
            let second_row = input.selection().start;
            for _ in 0..10 {
                input.right(&Right, window, cx);
            }
            input.goal_x = None;
            let mut at = input.selection().start;
            let mut steps = 0;
            while at > 0 {
                input.up(&Up, window, cx);
                let now = input.selection().start;
                assert!(now < at, "Up stays at {at}");
                at = now;
                steps += 1;
                assert!(steps < 10);
            }
            assert!(second_row > 0);
            input.select_up(&SelectUp, window, cx);
            input.move_to(second_row + 10, cx);
            input.select_up(&SelectUp, window, cx);
            assert!(!input.selection().is_empty());
            input.move_to(0, cx);
            let mut at = 0;
            for _ in 0..3 {
                input.down(&Down, window, cx);
                let now = input.selection().start;
                assert!(now > at, "Down stays at {at}");
                at = now;
            }
        });
    }

    #[gpui::test]
    fn home_and_end_go_to_the_line_ends_not_the_wrapped_row_ends(cx: &mut gpui::TestAppContext) {
        let text = "abcdefghijklmnopqrstuvwxyz".repeat(4);
        let (input, cx) = multiline(&text, cx);
        input.update_in(cx, |input, window, cx| {
            input.move_to(0, cx);
            input.down(&Down, window, cx);
            let second_row = input.selection().start;
            assert!(second_row > 1 && second_row < text.len());
            input.move_to(0, cx);
            input.select_end(&SelectEnd, window, cx);
            assert_eq!(&input.text()[input.selection()], text.as_str());
            input.move_to(second_row + 1, cx);
            input.home(&Home, window, cx);
            assert_eq!(input.selection(), 0..0);
        });
    }

    #[gpui::test]
    fn home_and_end_stay_on_the_line_and_the_kill_keys_follow_emacs(cx: &mut gpui::TestAppContext) {
        let (input, cx) = multiline("abc\ndef", cx);
        input.update_in(cx, |input, window, cx| {
            input.move_to(5, cx);
            input.home(&Home, window, cx);
            assert_eq!(input.selection(), 4..4);
            input.end(&End, window, cx);
            assert_eq!(input.selection(), 7..7);
            input.document_start(&DocumentStart, window, cx);
            assert_eq!(input.selection(), 0..0);
            input.end(&End, window, cx);
            assert_eq!(input.selection(), 3..3);
            // Ctrl+K: to the end of the line, then the line break.
            input.move_to(1, cx);
            input.delete_to_end(&DeleteToEnd, window, cx);
            assert_eq!(input.text(), "a\ndef");
            input.delete_to_end(&DeleteToEnd, window, cx);
            assert_eq!(input.text(), "adef");
        });
    }

    #[gpui::test]
    fn a_long_line_wraps_into_rows_the_caret_can_cross(cx: &mut gpui::TestAppContext) {
        let words = "word ".repeat(30);
        let (input, cx) = multiline(words.trim_end(), cx);
        input.update_in(cx, |input, window, cx| {
            let rows = input.wrapped.as_ref().unwrap().height() / px(20.);
            assert!(rows > 3., "{rows} rows");
            input.move_to(0, cx);
            input.down(&Down, window, cx);
            let second = input.selection().start;
            assert!(second > 0 && second < input.text().len());
            input.up(&Up, window, cx);
            assert_eq!(input.selection(), 0..0);
        });
    }

    #[gpui::test]
    fn inserted_links_leave_the_cursor_in_place(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new_live("", cx));
        // (text, selection, expected text, expected cursor)
        let cases = [
            ("", 0..0, "L", 0),
            ("画像はこちら", 18..18, "画像はこちら L", 18),
            ("画像はこちら", 0..0, "L 画像はこちら", 0),
            ("これは参考資料です", 9..9, "これは L 参考資料です", 9),
            ("a b", 2..2, "a L b", 2),
            ("a b", 1..1, "a L b", 1),
            // A selection is kept as text; the link goes to the cursor end.
            ("keep me", 0..4, "keep L me", 4),
        ];
        for (text, selection, expected, cursor) in cases {
            input.update(cx, |input, cx| {
                input.content = text.into();
                input.selected_range = selection;
                input.insert_link("L", cx);
                assert_eq!(input.text(), expected, "{text:?}");
                assert_eq!(input.selected_range, cursor..cursor, "{text:?}");
            });
        }
        input.update_in(cx, |input, window, cx| {
            input.content = "abc".into();
            input.selected_range = 1..1;
            input.insert_link("L", cx);
            input.undo(&Undo, window, cx);
            assert_eq!(input.text(), "abc");
        });
    }

    fn set_theme(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
    }

    /// Which entries of the menu can be used, by position in the list.
    fn menu_state(
        input: &Entity<TextInput>,
        cx: &mut gpui::VisualTestContext,
    ) -> Vec<(String, bool)> {
        input.read_with(cx, |input, _| {
            let menu = InputMenu {
                position: point(px(0.), px(0.)),
                clipboard_text: true,
                clipboard_image: false,
            };
            let labels = MenuLabels::new(&crate::localization::Localizer::new(
                cayenchat_storage::Language::English,
            ));
            input
                .menu_entries(&menu, &labels)
                .into_iter()
                .flatten()
                .map(|(_, label, enabled)| (label, enabled))
                .collect()
        })
    }

    fn enabled(state: &[(String, bool)]) -> Vec<&str> {
        state
            .iter()
            .filter(|(_, enabled)| *enabled)
            .map(|(label, _)| label.as_str())
            .collect()
    }

    #[gpui::test]
    fn menu_entries_are_greyed_out_when_they_cannot_apply(cx: &mut gpui::TestAppContext) {
        set_theme(cx);
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new_field("", "", false, cx));
        assert_eq!(enabled(&menu_state(&input, cx)), ["Paste"]);
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "abc", window, cx);
        });
        assert_eq!(
            enabled(&menu_state(&input, cx)),
            ["Undo", "Paste", "Select All"]
        );
        input.update(cx, |input, _| input.selected_range = 0..2);
        assert_eq!(
            enabled(&menu_state(&input, cx)),
            ["Undo", "Cut", "Copy", "Paste", "Delete", "Select All"]
        );
        input.update_in(cx, |input, window, cx| input.undo(&Undo, window, cx));
        assert_eq!(enabled(&menu_state(&input, cx)), ["Redo", "Paste"]);
        // Only a draft has the entries for the message box.
        let all = menu_state(&input, cx);
        assert_eq!(all.len(), 7);
        let (draft, cx) = cx.add_window_view(|_, cx| TextInput::new_live("", cx));
        let all = menu_state(&draft, cx);
        assert_eq!(all.len(), 9);
        assert_eq!(enabled(&all), ["Paste", "Upload image…"]);
    }

    #[gpui::test]
    fn a_secret_field_cannot_cut_or_copy_from_the_menu(cx: &mut gpui::TestAppContext) {
        set_theme(cx);
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new_field("", "pw", true, cx));
        input.update(cx, |input, _| input.selected_range = 0..2);
        let state = menu_state(&input, cx);
        assert_eq!(
            enabled(&state),
            ["Paste", "Delete", "Select All"],
            "{state:?}"
        );
    }

    #[gpui::test]
    fn menu_entries_do_what_their_actions_do(cx: &mut gpui::TestAppContext) {
        set_theme(cx);
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new_field("", "hello", false, cx));
        cx.write_to_clipboard(ClipboardItem::new_string("X".into()));
        let choose = |choice, cx: &mut gpui::VisualTestContext| {
            input.update_in(cx, |input, window, cx| input.choose(choice, window, cx));
            cx.run_until_parked();
        };
        input.update(cx, |input, _| input.selected_range = 1..3);
        choose(MenuChoice::Copy, cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("el".into())
        );
        choose(MenuChoice::Cut, cx);
        input.read_with(cx, |input, _| assert_eq!(input.text(), "hlo"));
        choose(MenuChoice::Paste, cx);
        input.read_with(cx, |input, _| assert_eq!(input.text(), "hello"));
        choose(MenuChoice::Undo, cx);
        input.read_with(cx, |input, _| assert_eq!(input.text(), "hlo"));
        choose(MenuChoice::Redo, cx);
        input.read_with(cx, |input, _| assert_eq!(input.text(), "hello"));
        choose(MenuChoice::SelectAll, cx);
        input.read_with(cx, |input, _| assert_eq!(input.selection(), 0..5));
        choose(MenuChoice::Delete, cx);
        input.read_with(cx, |input, _| assert_eq!(input.text(), ""));
    }

    #[gpui::test]
    fn the_menu_cannot_copy_a_secret_to_the_clipboard(cx: &mut gpui::TestAppContext) {
        set_theme(cx);
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new_field("", "pw", true, cx));
        cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
        input.update(cx, |input, _| input.selected_range = 0..2);
        for choice in [MenuChoice::Copy, MenuChoice::Cut] {
            input.update_in(cx, |input, window, cx| input.choose(choice, window, cx));
            cx.run_until_parked();
        }
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("before".into())
        );
        input.read_with(cx, |input, _| assert_eq!(input.text(), "pw"));
    }

    #[gpui::test]
    fn clearing_a_draft_from_the_menu_can_be_undone(cx: &mut gpui::TestAppContext) {
        set_theme(cx);
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new_live("", cx));
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "hello", window, cx);
            input.choose(MenuChoice::Clear, window, cx);
        });
        cx.run_until_parked();
        input.read_with(cx, |input, _| assert_eq!(input.text(), ""));
        input.update_in(cx, |input, window, cx| {
            input.choose(MenuChoice::Undo, window, cx)
        });
        cx.run_until_parked();
        input.read_with(cx, |input, _| assert_eq!(input.text(), "hello"));
    }

    #[gpui::test]
    fn a_right_click_opens_the_menu_and_escape_closes_it(cx: &mut gpui::TestAppContext) {
        set_theme(cx);
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new_field("", "hello", false, cx));
        input.update(cx, |input, _| input.selected_range = 1..3);
        let at = input.read_with(cx, |input, _| input.last_bounds().unwrap().center());
        cx.simulate_mouse_down(at, MouseButton::Right, gpui::Modifiers::none());
        input.read_with(cx, |input, _| {
            assert!(input.menu.is_some());
            // The selection is left alone.
            assert_eq!(input.selection(), 1..3);
        });
        cx.simulate_keystrokes("escape");
        input.read_with(cx, |input, _| assert!(input.menu.is_none()));
    }

    #[test]
    fn application_shortcuts_parse() {
        use cayenchat_storage::ChannelNumberModifier;
        for modifier in [ChannelNumberModifier::Alt, ChannelNumberModifier::Super] {
            assert!(crate::shortcut_bindings(modifier, &Default::default()).len() >= 30);
        }
        let bindings = crate::shortcut_bindings(ChannelNumberModifier::Ctrl, &Default::default());
        assert!(bindings.len() >= 30);
        let settings = bindings
            .iter()
            .find(|binding| binding.action().name().ends_with("OpenSettings"))
            .unwrap();
        #[cfg(target_os = "macos")]
        let shortcut = gpui::Keystroke::parse("cmd-,").unwrap();
        #[cfg(not(target_os = "macos"))]
        let shortcut = gpui::Keystroke::parse("ctrl-,").unwrap();
        assert_eq!(settings.match_keystrokes(&[shortcut]), Some(false));
    }
}
