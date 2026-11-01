// SPDX-License-Identifier: GPL-3.0-only
//! Double and triple clicks in the logs select a phrase, word or line, and a
//! drag after them grows by the same unit.
use super::{ChatWindow, Worker};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::{
    ClipboardItem, Modifiers, MouseButton, MouseDownEvent, Pixels, Point, TestAppContext,
    VisualTestContext, point, px,
};

const TEXT: &str = "alpha beta gamma";

fn open_channel<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
) -> (gpui::Entity<ChatWindow>, &'a mut VisualTestContext) {
    crate::support::install_test_globals(cx);
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
                Event::ChannelMessage {
                    channel: "#a".into(),
                    sender: "bob".into(),
                    text: text.into(),
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
    (chat, cx)
}

fn press(cx: &mut VisualTestContext, at: Point<Pixels>, click_count: usize) {
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: at,
        modifiers: Modifiers::none(),
        click_count,
        first_mouse: false,
    });
}

fn release(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
}

/// A point on the text, past the nickname prefix.
fn on_text(cx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    let bounds = cx.debug_bounds(selector).expect("row drawn");
    point(bounds.origin.x + px(60.), bounds.center().y)
}

/// Far to the right of the text on its line.
fn past_the_end(cx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    let bounds = cx.debug_bounds(selector).expect("row drawn");
    point(bounds.right() - px(2.), bounds.center().y)
}

fn copied(chat: &gpui::Entity<ChatWindow>, cx: &mut VisualTestContext) -> Option<String> {
    cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
    chat.update(cx, |chat, cx| chat.copy_selected_log(cx));
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .filter(|text| !text.is_empty())
}

fn is_a_word_of_the_text(copy: &str) -> bool {
    TEXT.split(' ').any(|word| word == copy)
}

#[gpui::test]
fn a_double_click_selects_a_word_and_a_drag_grows_by_words(cx: &mut TestAppContext) {
    let (chat, cx) = open_channel(cx, TEXT);
    let at = on_text(cx, "message-text-0");
    press(cx, at, 2);
    let word = copied(&chat, cx).expect("a word is selected");
    assert!(is_a_word_of_the_text(&word), "{word:?}");

    // To the end of the line: from the word's start to the end, whole words.
    let end = past_the_end(cx, "message-text-0");
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    release(cx, end);
    let grown = copied(&chat, cx).expect("still selected");
    assert_eq!(grown, TEXT[TEXT.find(&word).unwrap()..]);
}

#[gpui::test]
fn a_triple_click_selects_the_whole_line(cx: &mut TestAppContext) {
    let (chat, cx) = open_channel(cx, TEXT);
    let at = on_text(cx, "message-text-0");
    press(cx, at, 3);
    release(cx, at);
    assert_eq!(copied(&chat, cx).as_deref(), Some(TEXT));
}

#[gpui::test]
fn a_single_click_and_drag_still_select_by_characters(cx: &mut TestAppContext) {
    let (chat, cx) = open_channel(cx, TEXT);
    let at = on_text(cx, "message-text-0");
    press(cx, at, 1);
    assert_eq!(copied(&chat, cx), None);
    let end = past_the_end(cx, "message-text-0");
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    release(cx, end);
    let copy = copied(&chat, cx).expect("selected");
    assert!(
        TEXT.ends_with(&copy) && copy.len() > "gamma".len(),
        "{copy:?}"
    );
}

#[gpui::test]
fn a_double_click_on_a_link_selects_nothing_and_leaves_it_to_open(cx: &mut TestAppContext) {
    let (chat, cx) = open_channel(cx, "https://example.org/x");
    let at = on_text(cx, "message-text-0");
    press(cx, at, 2);
    release(cx, at);
    assert_eq!(copied(&chat, cx), None);
}

#[gpui::test]
fn a_double_click_selects_a_word_in_the_server_log(cx: &mut TestAppContext) {
    crate::support::install_test_globals(cx);
    let settings = crate::settings_with_channels("#a");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        let network = NetworkId(1);
        chat.state
            .dispatch(cayenchat_app::Command::SelectServer(network));
        chat.state.append_server_message(network, TEXT.into());
        cx.notify();
    });
    cx.run_until_parked();
    // The time column comes first; the last row is the one appended.
    let selector: &'static str = (0..8)
        .map(|index| &*Box::leak(format!("server-text-{index}").into_boxed_str()))
        .find(|selector| cx.debug_bounds(selector).is_some())
        .expect("a server row is drawn");
    let bounds = cx.debug_bounds(selector).unwrap();
    let at = point(bounds.origin.x + px(8.), bounds.center().y);
    press(cx, at, 2);
    release(cx, at);
    let word = copied(&chat, cx).expect("a word is selected");
    assert!(is_a_word_of_the_text(&word), "{word:?}");
    press(cx, at, 3);
    release(cx, at);
    assert_eq!(copied(&chat, cx).as_deref(), Some(TEXT));
}
