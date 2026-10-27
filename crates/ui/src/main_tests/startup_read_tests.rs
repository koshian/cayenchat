// SPDX-License-Identifier: GPL-3.0-only
//! When the credential store is first read: not before the window is drawn,
//! and for a server that does not connect at startup, only when the user
//! connects it (#648).
use super::ChatWindow;
use cayenchat_app::ConnectionStatus;
use cayenchat_model::NetworkId;
use cayenchat_storage::{Secret, Settings};
use gpui::TestAppContext;

const NET: NetworkId = NetworkId(1);

/// One server whose password and SASL password are saved and noted, aimed at
/// a port nobody listens on.
fn settings(connect_on_startup: bool) -> Settings {
    let mut settings = crate::settings_with_channels("#a");
    let profile = &mut settings.servers[0];
    profile.host = "127.0.0.1".into();
    profile.port = 1;
    profile.use_tls = true;
    profile.nickname = "me".into();
    profile.username = "me".into();
    profile.sasl_enabled = true;
    profile.sasl_username = "acct".into();
    profile.connect_on_startup = connect_on_startup;
    profile.set_server_password_saved(true);
    profile.set_sasl_password_saved(true);
    profile.note_channel_key("#a", true);
    settings
}

fn open_chat(
    cx: &mut TestAppContext,
    settings: Settings,
) -> (
    gpui::Entity<ChatWindow>,
    std::sync::Arc<crate::secrets::PasswordWatch>,
    &mut gpui::VisualTestContext,
) {
    let watch = cx.update(|cx| {
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
        let (watch, store) = crate::secrets::PasswordWatch::install(cx);
        let profile = &settings.servers[0];
        store
            .set(&profile.server_password_key(), &Secret::new("server"))
            .unwrap();
        store
            .set(&profile.sasl_password_key(), &Secret::new("sasl"))
            .unwrap();
        watch
    });
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    (chat, watch, cx)
}

fn stop(chat: &mut ChatWindow) {
    if let Some(connection) = chat.sessions.get_mut(&NET).and_then(|s| s.irc.take()) {
        let _ = connection.disconnect();
    }
}

fn log_has(chat: &ChatWindow, text: &str) -> bool {
    chat.state
        .server_messages(NET)
        .iter()
        .any(|message| message.text == text)
}

#[gpui::test]
fn without_a_startup_server_the_store_is_not_read(cx: &mut TestAppContext) {
    let (chat, watch, cx) = open_chat(cx, settings(false));
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| assert!(chat.startup_connections.is_empty()));
    assert_eq!(watch.touches() + watch.key_touches(), 0);

    // The first read is the Connect of the user.
    chat.update_in(cx, |chat, window, cx| chat.reconnect(NET, window, cx));
    cx.run_until_parked();
    assert_eq!(watch.touches(), 2, "the server and the SASL password");
    assert!(watch.key_touches() > 0);
    chat.update(cx, |chat, _| stop(chat));
}

#[gpui::test]
fn a_startup_server_is_read_after_the_window_is_drawn(cx: &mut TestAppContext) {
    let (chat, watch, cx) = open_chat(cx, settings(true));
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.startup_connections, [NET], "waiting for the start");
    });
    assert_eq!(watch.touches() + watch.key_touches(), 0, "not in new");

    chat.update_in(cx, |chat, window, cx| {
        chat.connect_at_startup(window, cx);
        let connecting = chat.i18n.text("event_connecting");
        assert!(log_has(chat, &connecting), "the log says it is starting");
    });
    chat.read_with(cx, |chat, _| {
        // Visible although a channel may be selected: status line and tree.
        assert_eq!(chat.state.status(NET), Some(&ConnectionStatus::Connecting));
        assert!(chat.sessions[&NET].starting);
    });
    // The test platform draws no frames, and nothing else reads: not even
    // after a long wait.
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(watch.touches() + watch.key_touches(), 0, "before a frame");

    // What the frame after the first one runs.
    chat.update_in(cx, |chat, window, cx| {
        chat.connect_startup_servers(window, cx)
    });
    assert_eq!(watch.touches(), 2, "the server and the SASL password");
    assert!(watch.key_touches() > 0);
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty(), "nothing was missing");
        stop(chat);
    });
}

#[gpui::test]
fn a_server_connected_by_hand_before_the_start_is_left_alone(cx: &mut TestAppContext) {
    let (chat, watch, cx) = open_chat(cx, settings(true));
    chat.update_in(cx, |chat, window, cx| {
        chat.connect_at_startup(window, cx);
        chat.reconnect(NET, window, cx);
    });
    let by_hand = watch.touches();
    assert_eq!(by_hand, 2);
    chat.update_in(cx, |chat, window, cx| {
        chat.connect_startup_servers(window, cx)
    });
    assert_eq!(watch.touches(), by_hand, "not read a second time");
    chat.update(cx, |chat, _| stop(chat));
}
