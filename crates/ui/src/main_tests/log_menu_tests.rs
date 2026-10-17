// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, LogMenuChoice, LogPosition, LogSelection, Worker};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::{ClipboardItem, EntityInputHandler, Focusable, TestAppContext, point, px};

fn open_chat(cx: &mut TestAppContext) -> (gpui::Entity<ChatWindow>, &mut gpui::VisualTestContext) {
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
            ],
            Worker::Running,
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
    });
    (chat, cx)
}

#[gpui::test]
fn the_menu_names_only_a_valid_other_speaker(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let at = point(px(10.), px(10.));
        chat.open_log_menu(at, Some("bob"), window, cx);
        assert_eq!(
            chat.log_menu.as_ref().unwrap().sender.as_deref(),
            Some("bob")
        );
        for refused in ["Me", "#a", "a b", ""] {
            chat.open_log_menu(at, Some(refused), window, cx);
            assert!(
                chat.log_menu.as_ref().unwrap().sender.is_none(),
                "{refused}"
            );
        }
        chat.open_log_menu(at, None, window, cx);
        assert!(chat.log_menu.as_ref().unwrap().sender.is_none());
        assert!(chat.dismiss_menus());
        assert!(chat.log_menu.is_none());
    });
}

#[gpui::test]
fn opening_a_private_conversation_selects_it_and_sends_nothing(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let at = point(px(10.), px(10.));
        for _ in 0..2 {
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            chat.open_log_menu(at, Some("Bob"), window, cx);
            chat.log_menu_choice(LogMenuChoice::Private, window, cx);
            let selected = chat.state.selected_channel().unwrap();
            assert!(selected.is_private());
            assert_eq!(selected.name, "Bob");
            assert!(selected.messages.is_empty());
        }
        // The second open found the first conversation.
        let privates = chat
            .state
            .conversations()
            .iter()
            .filter(|c| c.is_private())
            .count();
        assert_eq!(privates, 1);
        // A private conversation has no channel actions.
        chat.open_log_menu(at, Some("Bob"), window, cx);
        assert!(chat.log_menu.as_ref().unwrap().private);
    });
}

#[gpui::test]
fn opening_the_menu_keeps_the_selection_and_copy_uses_it(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let channel = chat.state.selected_channel().unwrap().id;
        chat.state.append_conversation_message(
            channel,
            "bob",
            "hello world",
            cayenchat_app::Kind::Message,
            Default::default(),
            cayenchat_app::Unread::Leave,
        );
        let selection = LogSelection {
            source: cayenchat_app::Selection::Channel(channel),
            anchor: LogPosition { row: 0, byte: 6 },
            cursor: LogPosition { row: 0, byte: 11 },
        };
        chat.log_selection = Some(selection);
        cx.write_to_clipboard(ClipboardItem::new_string("old".into()));
        chat.open_log_menu(point(px(5.), px(5.)), Some("bob"), window, cx);
        assert!(chat.has_log_selection(cayenchat_app::Selection::Channel(channel)));
        assert!(chat.log_selection.is_some());
        chat.log_menu_choice(LogMenuChoice::Copy, window, cx);
        assert!(chat.log_selection.is_some());
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
            Some("world")
        );
    });
}

#[gpui::test]
fn mode_and_topic_open_prompts_without_sending(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.state.set_topic(NetworkId(1), "#a", "old topic");
        let at = point(px(10.), px(10.));
        chat.open_log_menu(at, None, window, cx);
        chat.log_menu_choice(LogMenuChoice::Topic, window, cx);
        let prompt = chat.member_prompt.as_ref().unwrap();
        assert_eq!(prompt.nickname, "#a");
        assert_eq!(prompt.input.read(cx).text(), "old topic");
        chat.cancel_member_prompt(window, cx);
        assert!(chat.member_prompt.is_none());
        chat.open_log_menu(at, None, window, cx);
        chat.log_menu_choice(LogMenuChoice::Mode, window, cx);
        assert!(
            chat.member_prompt
                .as_ref()
                .unwrap()
                .input
                .read(cx)
                .text()
                .is_empty()
        );
        chat.open_log_menu(at, None, window, cx);
        chat.log_menu_choice(LogMenuChoice::Nick, window, cx);
        assert_eq!(
            chat.member_prompt.as_ref().unwrap().input.read(cx).text(),
            "me"
        );
    });
}

#[gpui::test]
fn escape_closes_the_menu_while_the_message_input_has_focus(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let input = chat.inputs[&chat.state.selection()].clone();
        window.focus(&input.focus_handle(cx));
        input.update(cx, |input, cx| input.set_text("draft", cx));
        chat.open_log_menu(point(px(10.), px(10.)), Some("bob"), window, cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    chat.read_with(cx, |chat, cx| {
        assert!(chat.log_menu.is_none());
        assert_eq!(
            chat.inputs[&chat.state.selection()].read(cx).text(),
            "draft"
        );
    });
}

#[gpui::test]
fn pasting_waits_for_a_finished_composition(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let input = chat.inputs[&chat.state.selection()].clone();
        window.focus(&input.focus_handle(cx));
        cx.write_to_clipboard(ClipboardItem::new_string("pasted".into()));
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "かな", None, window, cx)
        });
        assert!(input.read(cx).is_composing());
        let at = point(px(10.), px(10.));
        chat.open_log_menu(at, None, window, cx);
        assert!(!chat.log_menu.as_ref().unwrap().can_paste);
        // A composition started after the menu opened is protected as well.
        input.update(cx, |input, cx| input.unmark_text(window, cx));
        chat.open_log_menu(at, None, window, cx);
        assert!(chat.log_menu.as_ref().unwrap().can_paste);
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "かな", None, window, cx)
        });
        chat.log_menu_choice(LogMenuChoice::Paste, window, cx);
        let input = input.read(cx);
        assert!(input.is_composing());
        assert_eq!(input.text(), "かなかな");
    });
}

#[gpui::test]
fn pasting_replaces_the_selection_and_needs_text(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let input = chat.inputs[&chat.state.selection()].clone();
        input.update(cx, |input, cx| {
            input.set_text("keep SEL end", cx);
            input.select_everything(cx);
        });
        let at = point(px(10.), px(10.));
        cx.write_to_clipboard(ClipboardItem::new_string("new".into()));
        chat.open_log_menu(at, None, window, cx);
        assert!(chat.log_menu.as_ref().unwrap().can_paste);
        chat.log_menu_choice(LogMenuChoice::Paste, window, cx);
        assert_eq!(input.read(cx).text(), "new");
        cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::empty()));
        chat.open_log_menu(at, None, window, cx);
        assert!(!chat.log_menu.as_ref().unwrap().can_paste);
        chat.log_menu_choice(LogMenuChoice::Paste, window, cx);
        assert_eq!(input.read(cx).text(), "new");
    });
}

#[gpui::test]
fn clicking_the_edit_prompt_does_not_reach_the_message_input(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    cx.simulate_resize(gpui::size(px(1000.), px(700.)));
    let viewport = cx.update(|window, _| window.viewport_size());
    chat.update_in(cx, |chat, window, cx| {
        // Over the message input at the bottom of the window.
        chat.state.set_topic(NetworkId(1), "#a", "old topic");
        chat.open_log_menu(
            point(px(500.), viewport.height - px(140.)),
            None,
            window,
            cx,
        );
        chat.log_menu_choice(LogMenuChoice::Topic, window, cx);
    });
    cx.run_until_parked();
    let prompt = cx.debug_bounds("member-prompt").expect("prompt drawn");
    // The empty part of the button row, on top of the message input.
    let at = point(prompt.right() - px(12.), prompt.bottom() - px(14.));
    cx.simulate_mouse_move(at, None, gpui::Modifiers::default());
    cx.simulate_click(at, gpui::Modifiers::default());
    cx.update(|window, cx| {
        chat.read_with(cx, |chat, cx| {
            let prompt = chat.member_prompt.as_ref().expect("prompt stays open");
            assert!(prompt.input.focus_handle(cx).is_focused(window));
            assert_eq!(prompt.input.read(cx).text(), "old topic");
            let draft = &chat.inputs[&chat.state.selection()];
            assert!(!draft.focus_handle(cx).is_focused(window));
            assert!(draft.read(cx).text().is_empty());
        });
    });
}

#[gpui::test]
fn server_log_selection_is_copied_and_not_taken_for_a_channel(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let channel = chat.state.selected_channel().unwrap().id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectServer(NetworkId(1)));
        let base = chat.state.server_messages(NetworkId(1)).len();
        chat.state
            .append_server_message(NetworkId(1), "first line".into());
        chat.state
            .append_server_message(NetworkId(1), "second line".into());
        let source = cayenchat_app::Selection::Server(NetworkId(1));
        chat.log_selection = Some(LogSelection {
            source,
            anchor: LogPosition { row: base, byte: 6 },
            cursor: LogPosition {
                row: base + 1,
                byte: 6,
            },
        });
        assert!(chat.has_log_selection(source));
        cx.write_to_clipboard(ClipboardItem::new_string("old".into()));
        chat.open_log_menu(point(px(5.), px(5.)), None, window, cx);
        assert!(chat.log_menu.is_some());
        chat.log_menu_choice(LogMenuChoice::Copy, window, cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
            Some("line\nsecond")
        );
        // Another log never copies it.
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        cx.write_to_clipboard(ClipboardItem::new_string("old".into()));
        chat.copy_selected_log(cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
            Some("old")
        );
    });
}
