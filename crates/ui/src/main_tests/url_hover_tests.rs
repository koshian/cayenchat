// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::{Modifiers, TestAppContext, point, px};

#[gpui::test]
fn the_pointer_is_a_hand_over_a_url_in_the_channel_log(cx: &mut TestAppContext) {
    crate::support::install_test_globals(cx);
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
    crate::support::install_test_globals(cx);
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
    crate::support::install_test_globals(cx);
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
    crate::support::install_test_globals(cx);
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
            diagnostics_end: u64::MAX,
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
    crate::support::install_test_globals(cx);
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
                diagnostics_end: u64::MAX,
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

#[gpui::test]
fn copying_several_lines_carries_the_time_and_sender_drawn_on_each(cx: &mut TestAppContext) {
    use std::time::{Duration, UNIX_EPOCH};
    crate::support::install_test_globals(cx);
    let mut settings = crate::settings_with_channels("#a");
    settings.appearance.decode_char_refs = true;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let at = |seconds: u64| Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds));
    let message = |text: &str, seconds: u64| Event::ChannelMessage {
        channel: "#a".into(),
        sender: "bob".into(),
        text: text.into(),
        notice: false,
        server_time: at(seconds),
        msgid: None,
        account: None,
        replayed: false,
    };
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
                message("one &#127826;", 0),
                message("two", 10),
                message("three", 120),
                message("\u{1}ACTION waves\u{1}", 130),
            ],
            Worker::Running,
            cx,
        );
        let id = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(id));
        chat.state
            .append_channel_activity(NetworkId(1), "#a", "carol joined".into());
        id
    });
    let times: Vec<String> = chat.update(cx, |chat, _| {
        let channel = chat.state.selected_channel().unwrap();
        assert_eq!(channel.messages.len(), 5);
        channel
            .messages
            .iter()
            .map(|m| m.time.to_string())
            .collect()
    });
    let copied = |from: (usize, usize),
                  to: (usize, usize),
                  reiwa: bool,
                  cx: &mut gpui::VisualTestContext| {
        chat.update(cx, |chat, cx| {
            chat.appearance.reiwa_mode = reiwa;
            let sequences: Vec<u64> = chat
                .state
                .selected_channel()
                .unwrap()
                .messages
                .iter()
                .map(|m| m.sequence)
                .collect();
            chat.log_selection = Some(crate::LogSelection {
                source: cayenchat_app::Selection::Channel(id),
                anchor: crate::LogPosition {
                    row: sequences[from.0],
                    byte: from.1,
                },
                cursor: crate::LogPosition {
                    row: sequences[to.0],
                    byte: to.1,
                },
                diagnostics_end: u64::MAX,
            });
            chat.copy_selected_log(cx);
        });
        cx.read_from_clipboard().and_then(|item| item.text())
    };
    let t = &times;
    assert_eq!(
        copied((0, 0), (4, 12), false, cx),
        Some(format!(
            "{0} bob: one 🍒\n{1} bob: two\n{2} bob: three\n{3} * bob waves\n{4} carol joined",
            t[0], t[1], t[2], t[3], t[4]
        ))
    );
    // Reiwa mode: a continuation in the same minute has neither, another
    // minute only the time; the header line and the others as drawn.
    assert_eq!(
        copied((0, 0), (4, 12), true, cx),
        Some(format!(
            "{0} bob: one 🍒\ntwo\n{1} three\n{2} * bob waves\n{3} carol joined",
            t[0], t[2], t[3], t[4]
        ))
    );
    // Partial first and last lines keep their time and sender.
    assert_eq!(
        copied((0, 1), (3, 2), false, cx),
        Some(format!(
            "{0} bob: ne 🍒\n{1} bob: two\n{2} bob: three\n{3} * bob wa",
            t[0], t[1], t[2], t[3]
        ))
    );
    // One line is still only the selected part of the body.
    assert_eq!(copied((2, 1), (2, 4), true, cx).as_deref(), Some("hre"));
}
