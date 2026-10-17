// SPDX-License-Identifier: GPL-3.0-only
//! Pieces shared by the questions drawn inside the chat window: the quit
//! (#378) and part (#451) dialogs, and the nickname and channel key prompts.

use gpui::{prelude::*, *};

use crate::{ChatWindow, theme::Theme};

/// Space kept free on both sides of a box in a narrow window.
const BOX_MARGIN: Pixels = px(32.);

/// The layer covering the window behind a dialog box: nothing under it can be
/// clicked or typed into, and the box is centered at the top. The caller adds
/// the key handling and the box.
pub(crate) fn overlay(id: &'static str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(|| id.into())
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .justify_center()
        // Without this the row stretches the box to the window height.
        .items_start()
        .pt(px(16.))
        .bg(Rgba {
            a: 0.6,
            ..theme.surface
        })
}

/// The width of a box capped at `max`, kept inside a narrow window.
pub(crate) fn box_width(max: Pixels, window: &Window) -> Pixels {
    max.min((window.viewport_size().width - BOX_MARGIN).max(px(0.)))
}

/// The frame of a dialog box or prompt list; the caller sets padding and size.
pub(crate) fn dialog_frame(theme: &Theme) -> Div {
    div()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
        .shadow_md()
        .flex()
        .flex_col()
        .gap_2()
}

/// [`dialog_frame`] with the padding of the quit and part dialogs.
pub(crate) fn dialog_box(theme: &Theme) -> Div {
    dialog_frame(theme).p_3()
}

/// The box of the nickname and channel key prompts, centered near the top of
/// the window. `origin` is the offset of the element it is positioned in.
pub(crate) fn prompt_list(
    id: &'static str,
    origin: Point<Pixels>,
    window: &Window,
    theme: &Theme,
) -> Stateful<Div> {
    let viewport = window.viewport_size();
    let width = px(360.);
    dialog_frame(theme)
        .id(id)
        .absolute()
        .left(((viewport.width - width) / 2.).max(px(0.)) - origin.x)
        .top(px(60.).min(viewport.height / 4.) - origin.y)
        .w(width)
        .p_2()
}

/// A focusable button that calls `on_press` on a click, Enter or Space.
fn button(
    id: &'static str,
    label: String,
    focus: &FocusHandle,
    on_press: impl Fn(&mut ChatWindow, &mut Window, &mut Context<ChatWindow>) + Copy + 'static,
    cx: &mut Context<ChatWindow>,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(|| id.into())
        .track_focus(focus)
        .px_2()
        .py_1()
        .border_1()
        .cursor_pointer()
        .child(label)
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                on_press(this, window, cx);
                cx.stop_propagation();
            }
        }))
        .on_click(cx.listener(move |this, _, window, cx| on_press(this, window, cx)))
}

/// The filled button that accepts the question.
pub(crate) fn confirm_button(
    id: &'static str,
    label: String,
    focus: &FocusHandle,
    theme: &Theme,
    on_press: impl Fn(&mut ChatWindow, &mut Window, &mut Context<ChatWindow>) + Copy + 'static,
    cx: &mut Context<ChatWindow>,
) -> Stateful<Div> {
    let text = theme.text;
    button(id, label, focus, on_press, cx)
        .bg(theme.selected)
        .border_color(theme.selected)
        .focus(move |s| s.border_color(text))
}

/// The outlined button that dismisses the question.
pub(crate) fn cancel_button(
    id: &'static str,
    label: String,
    focus: &FocusHandle,
    theme: &Theme,
    on_press: impl Fn(&mut ChatWindow, &mut Window, &mut Context<ChatWindow>) + Copy + 'static,
    cx: &mut Context<ChatWindow>,
) -> Stateful<Div> {
    let text = theme.text;
    button(id, label, focus, on_press, cx)
        .border_color(theme.border)
        .focus(move |s| s.border_color(text))
}
