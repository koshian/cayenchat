// SPDX-License-Identifier: GPL-3.0-only
use super::ChatWindow;
use gpui::{ClipboardItem, Context, TestAppContext, Window};

fn install_globals(cx: &mut TestAppContext, confirm: bool) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
        cx.set_global(crate::input::ConfirmMultilinePaste(confirm));
    });
}

fn lines_shown(chat: &ChatWindow, cx: &gpui::App) -> Vec<String> {
    let prompt = chat.paste_prompt.as_ref().unwrap();
    prompt
        .body()
        .read(cx)
        .text()
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// Pastes three lines (CRLF, an empty one, a trailing break) into the draft
/// of the only channel.
fn paste_lines(chat: &mut ChatWindow, window: &mut Window, cx: &mut Context<ChatWindow>) {
    cx.write_to_clipboard(ClipboardItem::new_string("one\r\ntwo\n\nthree\n".into()));
    let id = chat.state.conversations()[0].id;
    chat.state
        .dispatch(cayenchat_app::Command::SelectChannel(id));
    let input = chat.inputs[&chat.state.selection()].clone();
    input.update(cx, |input, cx| input.paste_clipboard(window, cx));
}

#[gpui::test]
fn a_multiline_paste_is_joined_with_spaces_when_switched_off(cx: &mut TestAppContext) {
    install_globals(cx, false);
    let settings = crate::settings_with_channels("#a");
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update_in(cx, |chat, window, cx| {
        paste_lines(chat, window, cx);
        let input = chat.inputs[&chat.state.selection()].clone();
        // One space per line break, an empty line included.
        assert_eq!(input.read(cx).text(), "one two  three ");
        assert!(chat.pending_paste.is_none());
    });
}

fn drive_dialog(chat: &mut ChatWindow, window: &mut Window, cx: &mut Context<ChatWindow>) {
    // The frame drawn after the paste opened the question.
    assert!(chat.pending_paste.is_none());
    let input = chat.inputs[&chat.state.selection()].clone();
    assert_eq!(input.read(cx).text(), "", "nothing inserted");
    assert_eq!(lines_shown(chat, cx), ["one", "two", "", "three"]);
    let quote = chat.paste_prompt.as_ref().unwrap().quote.clone();
    assert_eq!(quote.read(cx).text(), ">");
    // One button: it adds a level, then removes it again.
    chat.toggle_paste_quote(cx);
    assert_eq!(lines_shown(chat, cx), ["> one", "> two", "", "> three"]);
    chat.toggle_paste_quote(cx);
    assert_eq!(lines_shown(chat, cx), ["one", "two", "", "three"]);
    chat.remove_empty_paste_lines(cx);
    assert_eq!(lines_shown(chat, cx), ["one", "two", "three"]);
    // Not connected: the question stays open with the reason.
    chat.confirm_paste_prompt(window, cx);
    assert!(chat.paste_prompt.as_ref().unwrap().error().is_some());
    chat.close_paste_prompt(window, cx);
    assert!(chat.paste_prompt.is_none());
}

#[gpui::test]
fn a_multiline_paste_opens_the_dialog_when_switched_on(cx: &mut TestAppContext) {
    install_globals(cx, true);
    let mut settings = crate::settings_with_channels("#a");
    // The window sets the global from the saved settings.
    settings.confirm_multiline_paste = true;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update_in(cx, paste_lines);
    chat.update_in(cx, drive_dialog);
}

/// Lines added in the editor after the dialog opened are checked again when
/// sending, before any connection is asked.
#[gpui::test]
fn too_many_lines_cannot_be_sent(cx: &mut TestAppContext) {
    install_globals(cx, true);
    let mut settings = crate::settings_with_channels("#a");
    settings.confirm_multiline_paste = true;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update_in(cx, paste_lines);
    chat.update_in(cx, |chat, window, cx| {
        let many = vec!["x"; super::paste_dialog::MAX_LINES + 1].join("\n");
        chat.paste_prompt
            .as_ref()
            .unwrap()
            .body()
            .update(cx, |body, cx| body.replace_text(&many, cx));
        chat.confirm_paste_prompt(window, cx);
        let error = chat.paste_prompt.as_ref().unwrap().error().unwrap();
        assert!(error.contains("100") && error.contains("101"), "{error}");
    });
}
