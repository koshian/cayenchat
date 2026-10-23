// SPDX-License-Identifier: GPL-3.0-only
use super::ChatWindow;
use crate::help_command::{Request, parse};
use cayenchat_model::NetworkId;
use gpui::{TestAppContext, VisualTestContext};

#[test]
fn only_the_help_word_matches() {
    assert_eq!(parse("/help"), Some(Request::List));
    assert_eq!(parse("  /HeLp  "), Some(Request::List));
    assert_eq!(parse("/help topic"), Some(Request::Usage));
    assert_eq!(parse("/help keys now"), Some(Request::Usage));
    assert_eq!(parse("/helper"), None);
    assert_eq!(parse("/helpme x"), None);
    assert_eq!(parse("help"), None);
    assert_eq!(parse("/raw help"), None);
    assert_eq!(parse("/msg bob /help"), None);
}

fn open_chat(cx: &mut TestAppContext) -> (gpui::Entity<ChatWindow>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a");
    cx.add_window_view(|window, cx| ChatWindow::with_settings(settings.clone(), None, window, cx))
}

#[gpui::test]
fn help_lists_commands_in_the_server_log_while_disconnected(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        let selection = chat.state.selection();
        let before = chat.state.server_messages(NetworkId(1)).len();
        chat.run_help(Request::List).unwrap();
        let lines = &chat.state.server_messages(NetworkId(1))[before..];
        assert!(lines.len() > 10);
        assert!(lines.iter().any(|line| line.text.starts_with("/join")));
        assert!(lines.iter().any(|line| line.text.starts_with("/charset")));
        assert_eq!(chat.state.selection(), selection);
        let before = chat.state.server_messages(NetworkId(1)).len();
        chat.run_help(Request::Usage).unwrap();
        assert_eq!(chat.state.server_messages(NetworkId(1)).len(), before + 1);
        cx.notify();
    });
}
