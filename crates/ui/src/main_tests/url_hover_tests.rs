// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::{Modifiers, TestAppContext, point, px};

#[gpui::test]
fn the_pointer_is_a_hand_over_a_url_in_the_channel_log(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let message = |text: &str| Event::ChannelMessage {
        channel: "#a".into(),
        sender: "bob".into(),
        text: text.into(),
        notice: false,
        server_time: None,
        msgid: None,
        account: None,
        replayed: false,
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                message("https://example.org/x"),
                message("hello there"),
            ],
            Worker::Running,
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        cx.notify();
    });
    cx.run_until_parked();
    let over = |row: usize, cx: &mut gpui::VisualTestContext| {
        let selector: &'static str = ["message-text-0", "message-text-1"][row];
        let bounds = cx.debug_bounds(selector).expect("row drawn");
        // Past the "bob: " prefix the nickname shares the text with.
        let at = point(bounds.origin.x + px(60.), bounds.center().y);
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| chat.url_hover.is_some())
    };
    assert!(over(0, cx), "on the link");
    assert!(!over(1, cx), "on plain text");
    assert!(over(0, cx), "back on the link");
}

#[gpui::test]
fn the_combined_log_flows_channel_network_and_nickname_into_the_text(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a,#b");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::Joined {
                    channel: "#b".into(),
                },
                Event::ChannelMessage {
                    channel: "#b".into(),
                    sender: "bob".into(),
                    text: "hello".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        cx.notify();
    });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("sub-text-0").expect("combined row drawn");
    // No fixed channel name column (162 px by default) before the text:
    // only the time column and its gap precede it.
    assert!(
        bounds.origin.x < px(100.),
        "text starts at {:?}",
        bounds.origin.x
    );
    chat.read_with(cx, |chat, _| assert_eq!(chat.sub_rows.len(), 1));
}

#[gpui::test]
fn a_decoded_reference_shows_its_original_and_copies_only_that(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.appearance.decode_char_refs = true;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::ChannelMessage {
                    channel: "#a".into(),
                    sender: "bob".into(),
                    text: "&#x1F352;&#127826; hi".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        cx.notify();
    });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("message-text-0").expect("row drawn");
    // Past the "bob: " prefix, on the first drawn character.
    let at = point(bounds.origin.x + px(40.), bounds.center().y);
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("char-ref-original").is_some(),
        "the bubble shows the original text"
    );
    let copy = cx.debug_bounds("char-ref-copy").expect("copy button");
    cx.simulate_mouse_move(copy.center(), None, Modifiers::none());
    cx.simulate_click(copy.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("&#x1F352;&#127826;")
    );
}

#[gpui::test]
fn selecting_decoded_text_copies_the_characters_shown(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.appearance.decode_char_refs = true;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::ChannelMessage {
                    channel: "#a".into(),
                    sender: "bob".into(),
                    text: "&#x1F352;&#127826; hi".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        let id = chat.state.conversations()[0].id;
        let first = &chat.state.selected_channel().unwrap().messages[0];
        let (row, end) = (first.sequence, first.text.len());
        chat.log_selection = Some(crate::LogSelection {
            source: cayenchat_app::Selection::Channel(id),
            anchor: crate::LogPosition { row, byte: 0 },
            cursor: crate::LogPosition { row, byte: end },
        });
        chat.copy_selected_log(cx);
    });
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("🍒🍒 hi")
    );
}

#[gpui::test]
fn copying_an_action_adds_the_prefix_only_when_it_is_selected_whole(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let id = chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::ChannelMessage {
                    channel: "#a".into(),
                    sender: "bob".into(),
                    text: "\u{1}ACTION waves\u{1}".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        assert_eq!(
            chat.state.selected_channel().unwrap().messages[0].text,
            "waves"
        );
        channel
    });
    let copied = |from: usize, to: usize, cx: &mut gpui::VisualTestContext| {
        chat.update(cx, |chat, cx| {
            let row = chat.state.selected_channel().unwrap().messages[0].sequence;
            chat.log_selection = Some(crate::LogSelection {
                source: cayenchat_app::Selection::Channel(id),
                anchor: crate::LogPosition { row, byte: from },
                cursor: crate::LogPosition { row, byte: to },
            });
            chat.copy_selected_log(cx);
        });
        cx.read_from_clipboard().and_then(|item| item.text())
    };
    assert_eq!(copied(0, 5, cx).as_deref(), Some("* bob waves"));
    assert_eq!(copied(0, 1, cx).as_deref(), Some("w"));
    assert_eq!(copied(1, 4, cx).as_deref(), Some("ave"));
    assert_eq!(copied(1, 5, cx).as_deref(), Some("aves"));
}
