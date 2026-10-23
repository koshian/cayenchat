// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, ShortcutPrefs, apply_shortcuts};
use cayenchat_storage::SendKey;
use gpui::{Focusable as _, TestAppContext, VisualTestContext};

/// A chat window that is not connected: a send that is attempted fails and
/// opens the error dialog, which is how these tests see it.
fn open_chat(
    cx: &mut TestAppContext,
    send_key: SendKey,
) -> (gpui::Entity<ChatWindow>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
        apply_shortcuts(ShortcutPrefs::default(), cx);
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.language = cayenchat_storage::Language::English;
    settings.send_key = send_key;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();
    type_draft(&chat, cx, "hello");
    (chat, cx)
}

fn type_draft(chat: &gpui::Entity<ChatWindow>, cx: &mut VisualTestContext, text: &str) {
    let input = chat.read_with(cx, |chat, _| chat.inputs[&chat.state.selection()].clone());
    input.update(cx, |input, cx| input.set_text(text, cx));
    let handle = input.read_with(cx, |input, cx| input.focus_handle(cx));
    cx.update(|window, _| window.focus(&handle));
    cx.run_until_parked();
}

/// A whole press: the key down, then the key up.
fn press(cx: &mut VisualTestContext, keys: &str) {
    cx.simulate_keystrokes(keys);
    let keystroke = gpui::Keystroke::parse(keys).unwrap();
    cx.simulate_event(gpui::KeyUpEvent { keystroke });
    cx.run_until_parked();
}

/// Whether a send was attempted since the last call (the failure dialog is
/// open), closing the dialog again.
fn attempted(cx: &mut VisualTestContext) -> bool {
    let shown = cx.has_pending_prompt();
    if shown {
        cx.simulate_prompt_answer("Close");
        cx.run_until_parked();
    }
    shown
}

fn draft(chat: &gpui::Entity<ChatWindow>, cx: &mut VisualTestContext) -> String {
    let input = chat.read_with(cx, |chat, _| chat.inputs[&chat.state.selection()].clone());
    input.read_with(cx, |input, _| input.text().to_owned())
}

#[gpui::test]
fn enter_mode_sends_with_enter_only(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, SendKey::Enter);
    press(cx, "ctrl-enter");
    assert!(!attempted(cx), "Ctrl+Enter is no longer a send key");
    assert_eq!(draft(&chat, cx), "hello");
    press(cx, "enter");
    assert!(attempted(cx));
    assert_eq!(draft(&chat, cx), "hello", "a failed send keeps the draft");
}

#[gpui::test]
fn ctrl_enter_mode_sends_with_ctrl_enter_only(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, SendKey::CtrlEnter);
    press(cx, "enter");
    press(cx, "enter");
    assert!(!attempted(cx), "Enter sends nothing, however often");
    press(cx, "ctrl-enter");
    assert!(attempted(cx));
    assert_eq!(draft(&chat, cx), "hello");
}

#[gpui::test]
fn double_enter_mode_waits_for_the_second_enter(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, SendKey::DoubleEnter);
    press(cx, "enter");
    assert!(!attempted(cx), "the first Enter only waits");
    assert_eq!(draft(&chat, cx), "hello", "and inserts nothing");
    press(cx, "enter");
    assert!(attempted(cx), "the second one sends");
    // Sent once: the pair is used up.
    press(cx, "enter");
    assert!(!attempted(cx));
    press(cx, "ctrl-enter");
    press(cx, "ctrl-enter");
    assert!(!attempted(cx), "Ctrl+Enter is not Enter twice");
}

#[gpui::test]
fn double_enter_mode_ignores_a_held_key(cx: &mut TestAppContext) {
    let (_chat, cx) = open_chat(cx, SendKey::DoubleEnter);
    // Key downs without a key up in between are the auto-repeat.
    for _ in 0..4 {
        cx.simulate_keystrokes("enter");
    }
    assert!(!attempted(cx));
}

#[gpui::test]
fn another_key_edit_or_channel_change_starts_double_enter_over(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, SendKey::DoubleEnter);

    press(cx, "enter");
    press(cx, "left");
    press(cx, "enter");
    assert!(!attempted(cx), "another key between the two");
    press(cx, "enter");
    assert!(attempted(cx), "that second Enter was a new first one");

    press(cx, "enter");
    type_draft(&chat, cx, "changed");
    press(cx, "enter");
    assert!(!attempted(cx), "the draft was edited between the two");

    // Another channel and back is not the same wait either (the Enter
    // above is waiting).
    let (first, second) = chat.read_with(cx, |chat, _| {
        let channels = chat.state.conversations();
        (channels[0].id, channels[1].id)
    });
    for id in [second, first] {
        chat.update(cx, |chat, cx| {
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(id));
            cx.notify();
        });
        cx.run_until_parked();
    }
    type_draft(&chat, cx, "changed");
    press(cx, "enter");
    assert!(!attempted(cx));
}

#[gpui::test]
fn losing_the_focus_or_changing_the_setting_starts_double_enter_over(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, SendKey::DoubleEnter);

    press(cx, "enter");
    cx.update(|window, _| window.blur());
    cx.run_until_parked();
    type_draft(&chat, cx, "hello");
    press(cx, "enter");
    assert!(!attempted(cx), "the focus left the draft in between");

    chat.update(cx, |chat, _| {
        chat.send_key = SendKey::Enter;
        chat.double_enter.reset();
    });
    press(cx, "enter");
    assert!(attempted(cx), "a changed setting applies to the next key");
}

#[gpui::test]
fn an_empty_draft_sends_nothing_and_does_not_wait(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, SendKey::DoubleEnter);
    type_draft(&chat, cx, "");
    press(cx, "enter");
    press(cx, "enter");
    assert!(!attempted(cx));
    assert!(chat.read_with(cx, |chat, _| !chat.double_enter.is_waiting()));
}
