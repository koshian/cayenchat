// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker, themed_bindings};
use cayenchat_app::Kind;
use cayenchat_irc_core::Event;
use cayenchat_model::{NetworkId, Provenance};
use cayenchat_storage::ChannelNumberModifier;
use gpui::{Entity, Focusable as _, ListOffset, TestAppContext, VisualTestContext, px};

const JUMP: &str = if cfg!(target_os = "macos") {
    "cmd-j"
} else {
    "ctrl-end"
};

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
        crate::input::bind_keys(false, cx);
        cx.bind_keys(themed_bindings(
            ChannelNumberModifier::Ctrl,
            &Default::default(),
            false,
        ));
    });
    let settings = crate::settings_with_channels("#a,#b");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        let events = vec![
            Event::Registered {
                nickname: "me".into(),
            },
            Event::Joined {
                channel: "#a".into(),
            },
        ];
        chat.handle_events(NetworkId(1), events, Worker::Running, cx);
        let first = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(first));
        for n in 0..lines {
            chat.state.append_channel_message(
                NetworkId(1),
                "#a",
                "bob",
                &format!("line {n}"),
                Kind::Message,
                Provenance::Live,
            );
        }
        cx.notify();
    });
    draw(cx);
    (chat, cx)
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..2 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
}

fn button_shown(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) -> bool {
    chat.read_with(cx, |chat, _| chat.latest_button_drawn)
}

fn scroll_to_top(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) {
    chat.update(cx, |chat, _| {
        chat.channel_log_lists[&chat.state.selection()]
            .state
            .scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
    });
    draw(cx);
}

fn behind(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) -> bool {
    chat.read_with(cx, |chat, _| {
        chat.channel_log_lists[&chat.state.selection()].is_behind_latest()
    })
}

#[gpui::test]
fn the_shortcut_returns_to_the_latest_line_and_leaves_the_draft_alone(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, 200);
    assert!(!behind(&chat, cx));
    assert!(!button_shown(&chat, cx), "following the bottom");

    cx.simulate_input("draft");
    scroll_to_top(&chat, cx);
    assert!(behind(&chat, cx));
    assert!(button_shown(&chat, cx));

    cx.simulate_keystrokes(JUMP);
    draw(cx);
    assert!(!behind(&chat, cx));
    assert!(!button_shown(&chat, cx));
    // The draft, its caret and the focus stayed.
    chat.update_in(cx, |chat, window, cx| {
        let input = chat.inputs[&chat.state.selection()].clone();
        assert!(input.focus_handle(cx).is_focused(window));
        assert_eq!(input.read(cx).text(), "draft");
    });
    cx.simulate_input("!");
    chat.update(cx, |chat, cx| {
        assert_eq!(
            chat.inputs[&chat.state.selection()].read(cx).text(),
            "draft!"
        );
    });

    // New lines are followed again.
    chat.update(cx, |chat, cx| {
        chat.state.append_channel_message(
            NetworkId(1),
            "#a",
            "bob",
            "newest",
            Kind::Message,
            Provenance::Live,
        );
        cx.notify();
    });
    draw(cx);
    assert!(!behind(&chat, cx));
}

#[gpui::test]
fn the_button_returns_to_the_latest_line(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, 200);
    scroll_to_top(&chat, cx);
    let center = cx.debug_bounds("scroll-to-latest").unwrap().center();
    cx.simulate_click(center, gpui::Modifiers::default());
    draw(cx);
    assert!(!behind(&chat, cx));
    assert!(!button_shown(&chat, cx));
}

#[gpui::test]
fn a_log_that_fits_has_no_button_and_the_shortcut_does_nothing(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, 2);
    assert!(!behind(&chat, cx));
    assert!(!button_shown(&chat, cx));
    cx.simulate_keystrokes(JUMP);
    draw(cx);
    assert!(!behind(&chat, cx));
    assert!(!button_shown(&chat, cx));
}

#[gpui::test]
fn an_empty_log_is_left_alone(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, 0);
    cx.simulate_keystrokes(JUMP);
    draw(cx);
    assert!(!behind(&chat, cx));
    assert!(!button_shown(&chat, cx));
}

#[gpui::test]
fn the_button_follows_a_resize_without_further_input(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, 200);
    // One wheel step above the end of the log: the button shows.
    chat.update(cx, |chat, _| {
        chat.channel_log_lists[&chat.state.selection()]
            .state
            .scroll_to(ListOffset {
                item_ix: 150,
                offset_in_item: px(0.),
            });
    });
    draw(cx);
    assert!(button_shown(&chat, cx));

    // A window tall enough to show everything from here: only the real
    // updates run, none is forced.
    cx.simulate_resize(gpui::size(px(1000.), px(6000.)));
    cx.run_until_parked();
    assert!(!behind(&chat, cx));
    assert!(!button_shown(&chat, cx));
}

#[gpui::test]
fn a_dialog_keeps_the_shortcut_from_moving_the_log(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx, 200);
    scroll_to_top(&chat, cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.export_settings(&crate::ExportSettings, window, cx)
    });
    draw(cx);
    assert!(chat.read_with(cx, |chat, _| !chat.transfer_dialog.is_idle()));

    cx.simulate_keystrokes(JUMP);
    draw(cx);
    assert!(behind(&chat, cx), "the log stayed where it was");
    assert!(button_shown(&chat, cx));
}
