// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::TestAppContext;

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

#[gpui::test]
fn a_double_click_on_a_joined_row_asks_before_leaving(cx: &mut TestAppContext) {
    install_globals(cx);
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.servers[0].part_message = "bye bye".into();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let id = |chat: &ChatWindow, name: &str| {
        chat.state
            .conversations()
            .iter()
            .find(|c| c.name == name)
            .unwrap()
            .id
    };
    // Not connected: nothing happens.
    chat.update_in(cx, |chat, window, cx| {
        let a = id(chat, "#a");
        chat.activate_channel_row(a, window, cx);
        assert!(chat.part_prompt.is_none());
        assert!(chat.feedback.is_none());
    });
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config =
        cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
    config.port = listener.local_addr().unwrap().port();
    config.use_tls = false;
    config.part_message = "bye bye".into();
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
    chat.update_in(cx, |chat, window, cx| {
        let (a, b) = (id(chat, "#a"), id(chat, "#b"));
        // The row clicked is the one acted on, whatever is selected.
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(b));
        chat.activate_channel_row(a, window, cx);
        let prompt = chat.part_prompt.as_ref().unwrap();
        assert_eq!(prompt.channel, "#a");
        assert_eq!(prompt.input.read(cx).text(), "");
        // A second double click does not open another or send anything.
        chat.activate_channel_row(a, window, cx);
        chat.activate_channel_row(b, window, cx);
        assert_eq!(chat.part_prompt.as_ref().unwrap().channel, "#a");

        // Cancelling leaves nothing pending.
        chat.close_part_prompt(window, cx);
        assert!(chat.part_prompt.is_none());
        assert!(chat.sessions[&NetworkId(1)].pending_parts.is_empty());

        // A reason with a line break is refused and the question stays.
        chat.activate_channel_row(a, window, cx);
        chat.part_prompt
            .as_ref()
            .unwrap()
            .input
            .update(cx, |input, cx| input.set_text("one\r\ntwo", cx));
        chat.confirm_part_prompt(window, cx);
        let input = chat.part_prompt.as_ref().unwrap().input.clone();
        if input.read(cx).text().contains('\n') {
            assert!(chat.part_prompt.as_ref().unwrap().error.is_some());
            assert!(chat.sessions[&NetworkId(1)].pending_parts.is_empty());
        }

        // Confirming with a reason leaves once; the row is busy until answered.
        input.update(cx, |input, cx| input.set_text("later", cx));
        chat.confirm_part_prompt(window, cx);
        assert!(chat.part_prompt.is_none());
        assert!(
            chat.sessions[&NetworkId(1)]
                .pending_parts
                .contains_key("#a")
        );
        chat.activate_channel_row(a, window, cx);
        assert!(chat.part_prompt.is_none());
    });
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![Event::Parted {
                channel: "#a".into(),
            }],
            Worker::Running,
            cx,
        );
        assert!(chat.sessions[&NetworkId(1)].pending_parts.is_empty());
    });
}
