// SPDX-License-Identifier: GPL-3.0-only
//! The questions of a double click on a server or channel row (#625).
use super::{ChatWindow, Worker};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::{Entity, TestAppContext, VisualTestContext};

fn install_globals(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
}

/// A window whose first server is registered and has joined `#a`; the
/// listener keeps the socket open.
fn registered<'a>(
    settings: &cayenchat_storage::Settings,
    cx: &'a mut TestAppContext,
) -> (
    Entity<ChatWindow>,
    &'a mut VisualTestContext,
    std::net::TcpListener,
) {
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config =
        cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
    config.port = listener.local_addr().unwrap().port();
    config.use_tls = false;
    let connection = cayenchat_irc_core::Connection::connect(config.clone()).unwrap();
    chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&NetworkId(1)).unwrap();
        session.irc = Some(connection);
        session.active_config = Some(config);
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
            ],
            Worker::Running,
            cx,
        );
    });
    (chat, cx, listener)
}

#[gpui::test]
fn a_server_double_click_asks_and_only_a_confirmed_dont_ask_is_saved(cx: &mut TestAppContext) {
    install_globals(cx);
    let settings = crate::settings_with_channels("#a");
    let _file = crate::settings_file::TestFile::with(&settings);
    let (chat, cx, _listener) = registered(&settings, cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.activate_server_row(NetworkId(1), window, cx);
        assert!(chat.disconnect_prompt.is_some());
        assert!(!chat.sessions[&NetworkId(1)].manual_disconnect);
        // A second double click does not open another question.
        chat.activate_server_row(NetworkId(1), window, cx);

        // Cancelling, even with the box ticked, changes nothing.
        chat.toggle_disconnect_dont_ask(cx);
        chat.close_disconnect_prompt(window, cx);
        assert!(chat.disconnect_prompt.is_none());
        assert!(!chat.sessions[&NetworkId(1)].manual_disconnect);
        assert!(chat.confirm_server_dblclick_disconnect);
        let saved = crate::settings_file::load().unwrap().unwrap();
        assert!(saved.confirm_server_dblclick_disconnect);

        // Confirming with the box ticked disconnects and saves that one item.
        chat.activate_server_row(NetworkId(1), window, cx);
        chat.toggle_disconnect_dont_ask(cx);
        chat.confirm_disconnect_prompt(window, cx);
        assert!(chat.disconnect_prompt.is_none());
        assert!(chat.sessions[&NetworkId(1)].manual_disconnect);
        let saved = crate::settings_file::load().unwrap().unwrap();
        assert!(!saved.confirm_server_dblclick_disconnect);
        assert!(saved.confirm_channel_dblclick_part);
        assert!(!chat.confirm_server_dblclick_disconnect);
    });
}

#[gpui::test]
fn a_server_double_click_disconnects_at_once_when_switched_off(cx: &mut TestAppContext) {
    install_globals(cx);
    let mut settings = crate::settings_with_channels("#a");
    settings.confirm_server_dblclick_disconnect = false;
    let (chat, cx, _listener) = registered(&settings, cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.activate_server_row(NetworkId(1), window, cx);
        assert!(chat.disconnect_prompt.is_none());
        assert!(chat.sessions[&NetworkId(1)].manual_disconnect);
    });
}

#[gpui::test]
fn a_channel_double_click_leaves_at_once_when_switched_off(cx: &mut TestAppContext) {
    install_globals(cx);
    let mut settings = crate::settings_with_channels("#a");
    settings.confirm_channel_dblclick_part = false;
    let (chat, cx, _listener) = registered(&settings, cx);
    chat.update_in(cx, |chat, window, cx| {
        let a = chat
            .state
            .conversations()
            .iter()
            .find(|c| c.name == "#a")
            .unwrap()
            .id;
        chat.activate_channel_row(a, window, cx);
        assert!(chat.part_prompt.is_none());
        assert!(
            chat.sessions[&NetworkId(1)]
                .pending_parts
                .contains_key("#a")
        );
    });
}

#[gpui::test]
fn the_channel_dont_ask_is_saved_only_when_leaving_is_confirmed(cx: &mut TestAppContext) {
    install_globals(cx);
    let settings = crate::settings_with_channels("#a");
    let _file = crate::settings_file::TestFile::with(&settings);
    let (chat, cx, _listener) = registered(&settings, cx);
    chat.update_in(cx, |chat, window, cx| {
        let a = chat
            .state
            .conversations()
            .iter()
            .find(|c| c.name == "#a")
            .unwrap()
            .id;
        chat.activate_channel_row(a, window, cx);
        chat.toggle_part_dont_ask(cx);
        chat.close_part_prompt(window, cx);
        assert!(chat.confirm_channel_dblclick_part);
        assert!(
            crate::settings_file::load()
                .unwrap()
                .unwrap()
                .confirm_channel_dblclick_part
        );

        chat.activate_channel_row(a, window, cx);
        chat.toggle_part_dont_ask(cx);
        chat.confirm_part_prompt(window, cx);
        assert!(chat.part_prompt.is_none());
        let saved = crate::settings_file::load().unwrap().unwrap();
        assert!(!saved.confirm_channel_dblclick_part);
        assert!(saved.confirm_server_dblclick_disconnect);
        assert!(!chat.confirm_channel_dblclick_part);
    });
}

#[gpui::test]
fn a_dont_ask_is_not_saved_when_nothing_was_disconnected(cx: &mut TestAppContext) {
    install_globals(cx);
    let settings = crate::settings_with_channels("#a");
    let _file = crate::settings_file::TestFile::with(&settings);
    let (chat, cx, _listener) = registered(&settings, cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.activate_server_row(NetworkId(1), window, cx);
        chat.toggle_disconnect_dont_ask(cx);
        // The server is lost while the question is open.
        chat.handle_events(
            NetworkId(1),
            vec![Event::Disconnected("lost".into())],
            Worker::Running,
            cx,
        );
        chat.confirm_disconnect_prompt(window, cx);
        assert!(chat.disconnect_prompt.is_none());
        assert!(!chat.sessions[&NetworkId(1)].manual_disconnect);
        assert!(chat.confirm_server_dblclick_disconnect);
        let saved = crate::settings_file::load().unwrap().unwrap();
        assert!(saved.confirm_server_dblclick_disconnect);
    });
}
