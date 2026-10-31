// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::TestAppContext;

fn install_globals(cx: &mut TestAppContext) {
    crate::support::install_test_globals(cx);
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
        assert!(chat.notices.all().is_empty());
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

        // A reason longer than a line is refused and the question stays.
        chat.activate_channel_row(a, window, cx);
        chat.part_prompt
            .as_ref()
            .unwrap()
            .input
            .update(cx, |input, cx| input.set_text(&"x".repeat(600), cx));
        chat.confirm_part_prompt(window, cx);
        let input = chat.part_prompt.as_ref().unwrap().input.clone();
        assert!(chat.part_prompt.as_ref().unwrap().error.is_some());
        assert!(chat.sessions[&NetworkId(1)].pending_parts.is_empty());

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

/// Reads the PART lines the server end of `socket` has received.
fn parts_received(socket: &std::net::TcpStream) -> Vec<String> {
    use std::io::BufRead;
    socket
        .set_read_timeout(Some(std::time::Duration::from_millis(500)))
        .unwrap();
    let mut reader = std::io::BufReader::new(socket);
    let mut parts = Vec::new();
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
        if line.starts_with("PART ") {
            parts.push(line.trim_end().to_owned());
        }
        line.clear();
    }
    parts
}

#[gpui::test]
fn the_reason_is_typed_tabbed_and_sent_literally(cx: &mut TestAppContext) {
    install_globals(cx);
    cx.update(|cx| {
        crate::input::bind_keys(false, cx);
        cx.bind_keys(crate::shortcut_bindings(
            cayenchat_storage::ChannelNumberModifier::Ctrl,
            &Default::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config =
        cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
    config.port = listener.local_addr().unwrap().port();
    config.use_tls = false;
    let connection = cayenchat_irc_core::Connection::connect(config.clone()).unwrap();
    let (socket, _) = listener.accept().unwrap();
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
    let open = |cx: &mut gpui::VisualTestContext| {
        chat.update_in(cx, |chat, window, cx| {
            let a = chat
                .state
                .conversations()
                .iter()
                .find(|c| c.name == "#a")
                .unwrap()
                .id;
            chat.activate_channel_row(a, window, cx);
        });
        cx.run_until_parked();
    };
    let state = |chat: &gpui::Entity<ChatWindow>, cx: &mut gpui::VisualTestContext| {
        chat.update_in(cx, |chat, window, cx| {
            let prompt = chat.part_prompt.as_ref().map(|p| p.input.clone());
            (
                prompt
                    .as_ref()
                    .map(|input| input.read(cx).text().to_owned()),
                prompt.is_some_and(|input| {
                    gpui::Focusable::focus_handle(&input, cx).is_focused(window)
                }),
            )
        })
    };
    open(cx);
    // Focus starts on Cancel; Shift+Tab passes the "Don't ask again" box to
    // the field, where typing works.
    cx.simulate_keystrokes("shift-tab shift-tab");
    assert_eq!(state(&chat, cx), (Some(String::new()), true));
    cx.simulate_input(":smile");
    assert_eq!(state(&chat, cx).0.as_deref(), Some(":smile"));
    // Tab leaves the field for the box instead of completing; Shift+Tab returns.
    cx.simulate_keystrokes("tab");
    assert!(!state(&chat, cx).1);
    cx.simulate_keystrokes("shift-tab");
    assert!(state(&chat, cx).1);
    // Enter in the field leaves; the box was never ticked.
    cx.simulate_keystrokes("enter");
    assert!(chat.read_with(cx, |chat, _| chat.part_prompt.is_none()));
    assert_eq!(parts_received(&socket), vec!["PART #a ::smile".to_owned()]);
}
