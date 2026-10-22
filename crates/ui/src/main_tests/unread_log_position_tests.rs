// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use cayenchat_app::{Command, Kind, Selection};
use cayenchat_irc_core::Event;
use cayenchat_model::{NetworkId, Provenance};
use cayenchat_storage::UnreadLogPosition;
use gpui::{Entity, ListOffset, TestAppContext, VisualTestContext, px};

/// Two channels; `#b` has a log scrolled to its top and `#a` is selected.
fn open_chat(
    position: UnreadLogPosition,
    cx: &mut TestAppContext,
) -> (Entity<ChatWindow>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::theme::apply(
            cayenchat_storage::ThemeMode::Light,
            &cayenchat_storage::Appearance::default(),
            cx,
        );
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.unread_log_position = position;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        let mut events = vec![Event::Registered {
            nickname: "me".into(),
        }];
        for channel in ["#a", "#b"] {
            events.push(Event::Joined {
                channel: channel.into(),
            });
        }
        chat.handle_events(NetworkId(1), events, Worker::Running, cx);
        let (a, b) = (
            chat.state.conversations()[0].id,
            chat.state.conversations()[1].id,
        );
        chat.state.dispatch(Command::SelectChannel(b));
        for n in 0..30 {
            append(chat, "#b", &format!("line {n}"));
        }
        chat.sync_log_lists();
        chat.main_lists[&Selection::Channel(b)]
            .state
            .scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
        chat.state.dispatch(Command::SelectChannel(a));
        cx.notify();
    });
    cx.run_until_parked();
    (chat, cx)
}

fn append(chat: &mut ChatWindow, channel: &str, text: &str) {
    chat.state.append_channel_message(
        NetworkId(1),
        channel,
        "bob",
        text,
        Kind::Message,
        Provenance::Live,
    );
}

/// Selects `#b` and returns whether its log follows the bottom afterwards.
fn select_b_follows_bottom(
    chat: &Entity<ChatWindow>,
    unread: bool,
    cx: &mut VisualTestContext,
) -> bool {
    chat.update_in(cx, |chat, window, cx| {
        let b = chat.state.conversations()[1].id;
        if unread {
            append(chat, "#b", "new");
        }
        assert_eq!(chat.state.is_unread(b), unread);
        chat.dispatch(Command::SelectChannel(b), window, cx);
        let list = &chat.main_lists[&Selection::Channel(b)];
        list.state.logical_scroll_top().item_ix >= list.state.item_count()
    })
}

#[gpui::test]
fn latest_scrolls_an_unread_channel_to_its_end(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(UnreadLogPosition::Latest, cx);
    assert!(select_b_follows_bottom(&chat, true, cx));
}

#[gpui::test]
fn latest_keeps_the_position_of_a_read_channel(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(UnreadLogPosition::Latest, cx);
    assert!(!select_b_follows_bottom(&chat, false, cx));
}

#[gpui::test]
fn previous_keeps_the_position_of_an_unread_channel(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(UnreadLogPosition::Previous, cx);
    assert!(!select_b_follows_bottom(&chat, true, cx));
}
