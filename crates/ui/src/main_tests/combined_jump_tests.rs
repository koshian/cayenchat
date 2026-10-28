// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use cayenchat_app::{Command, Kind};
use cayenchat_irc_core::Event;
use cayenchat_model::{ConversationId, NetworkId, Provenance};
use gpui::{Entity, ListOffset, TestAppContext, VisualTestContext, point, px};

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..2 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
}

/// `#a` is selected and holds one line; `#b` holds `lines` lines.
fn open_chat(
    cx: &mut TestAppContext,
    lines: usize,
) -> (Entity<ChatWindow>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::theme::apply(
            cayenchat_storage::ThemeMode::Light,
            &cayenchat_storage::Appearance::default(),
            cx,
        );
    });
    let settings = crate::settings_with_channels("#a,#b");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        let events = ["#a", "#b"]
            .into_iter()
            .map(|channel| Event::Joined {
                channel: channel.into(),
            })
            .collect();
        chat.handle_events(NetworkId(1), events, Worker::Running, cx);
        for n in 0..lines {
            chat.state.append_channel_message(
                NetworkId(1),
                "#b",
                "bob",
                &format!("line {n}"),
                Kind::Message,
                Provenance::Live,
            );
        }
        chat.state.append_channel_message(
            NetworkId(1),
            "#a",
            "bob",
            "here",
            Kind::Message,
            Provenance::Live,
        );
        let a = chat.state.conversations()[0].id;
        chat.state.dispatch(Command::SelectChannel(a));
        cx.notify();
    });
    draw(cx);
    (chat, cx)
}

/// `#b`, the index and the sequence of its first conversation line (the
/// combined log's oldest row; the join notice before it is activity).
fn first_line(
    chat: &Entity<ChatWindow>,
    cx: &mut VisualTestContext,
) -> (ConversationId, usize, u64) {
    chat.read_with(cx, |chat, _| {
        let b = chat.state.conversations()[1].id;
        let messages = &chat.state.conversation(b).unwrap().messages;
        let index = messages.iter().position(|m| !m.activity).unwrap();
        (b, index, messages[index].sequence)
    })
}

fn show_first_row(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) {
    chat.update(cx, |chat, _| {
        chat.sub_list.state.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
    });
    draw(cx);
}

fn double_click_first_row(cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds("sub-text-0").expect("combined row drawn");
    let position = point(bounds.origin.x + bounds.size.width / 2., bounds.center().y);
    for click_count in [1, 2] {
        cx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count,
        });
    }
}

#[gpui::test]
fn a_double_click_scrolls_to_the_line_and_flashes_it_without_following(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, 300);
    // The oldest row of the combined log is `#b`'s first line.
    let (b, index, sequence) = first_line(&chat, cx);
    show_first_row(&chat, cx);
    double_click_first_row(cx);
    draw(cx);
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.state.selected_channel().map(|c| c.id), Some(b));
        assert!(matches!(chat.reveal_flash, Some((id, s, _)) if id == b && s == sequence));
        let list = &chat.main_lists[&chat.state.selection()];
        assert_eq!(
            list.message_rows_above(list.state.logical_scroll_top().item_ix),
            index
        );
        assert!(list.is_behind_latest(), "not pulled back to the latest");
    });
    // A new line neither follows nor moves the view.
    chat.update(cx, |chat, cx| {
        chat.state.append_channel_message(
            NetworkId(1),
            "#b",
            "bob",
            "newest",
            Kind::Message,
            Provenance::Live,
        );
        cx.notify();
    });
    draw(cx);
    chat.read_with(cx, |chat, _| {
        let list = &chat.main_lists[&chat.state.selection()];
        assert_eq!(
            list.message_rows_above(list.state.logical_scroll_top().item_ix),
            index
        );
    });
}
