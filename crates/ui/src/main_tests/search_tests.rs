// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, FindInLog, Worker, themed_bindings};
use cayenchat_app::{
    DiagnosticsView, Kind, Selection,
    search::{Scope, Source},
};
use cayenchat_irc_core::Event;
use cayenchat_model::{NetworkId, Provenance};
use cayenchat_storage::ChannelNumberModifier;
use gpui::{Entity, EntityInputHandler as _, Focusable as _, TestAppContext, VisualTestContext};
use std::time::Duration;

const FIND: &str = if cfg!(target_os = "macos") {
    "cmd-f"
} else {
    "ctrl-f"
};

fn open_chat(cx: &mut TestAppContext) -> (Entity<ChatWindow>, &mut VisualTestContext) {
    open_chat_with_keys(cx, false)
}

fn open_chat_with_keys(
    cx: &mut TestAppContext,
    emacs: bool,
) -> (Entity<ChatWindow>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::theme::apply(
            cayenchat_storage::ThemeMode::Light,
            &cayenchat_storage::Appearance::default(),
            cx,
        );
        crate::input::bind_keys(emacs, cx);
        cx.bind_keys(themed_bindings(
            ChannelNumberModifier::Ctrl,
            &Default::default(),
            emacs,
        ));
    });
    let settings = crate::settings_with_channels("#a,#b");
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
        let first = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(first));
        for (channel, text) in [
            ("#a", "first needle"),
            ("#b", "NEEDLE in b"),
            ("#a", "nothing here"),
            ("#a", "needle and Needle again"),
        ] {
            chat.state.append_channel_message(
                NetworkId(1),
                channel,
                "bob",
                text,
                Kind::Message,
                Provenance::Live,
            );
        }
        cx.notify();
    });
    cx.run_until_parked();
    (chat, cx)
}

/// Lets the debounce pass and the scan finish.
fn settle(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
}

fn selected_name(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) -> String {
    chat.read_with(cx, |chat, _| {
        chat.state.selected_channel().unwrap().name.clone()
    })
}

fn search_state(
    chat: &Entity<ChatWindow>,
    cx: &mut VisualTestContext,
) -> (usize, Option<u64>, String) {
    chat.read_with(cx, |chat, _| {
        let search = chat.search.as_ref().unwrap();
        (
            search.hits().len(),
            search.current().map(|hit| hit.sequence),
            search.term().to_owned(),
        )
    })
}

#[gpui::test]
fn the_shortcut_opens_the_bar_and_focuses_its_field_without_a_search(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    assert!(chat.read_with(cx, |chat, _| chat.search.is_none()));
    cx.simulate_keystrokes(FIND);
    cx.run_until_parked();
    cx.update(|window, cx| {
        chat.read_with(cx, |chat, cx| {
            assert!(chat.search.as_ref().is_some_and(|s| s.is_open()));
            let input = chat.search.as_ref().unwrap().input();
            assert!(input.focus_handle(cx).is_focused(window));
        });
    });
    // Opening again keeps the field focused and does not search anything.
    cx.simulate_keystrokes(FIND);
    assert_eq!(search_state(&chat, cx).0, 0);
}

#[gpui::test]
fn typing_searches_this_channel_and_enter_steps_through_hits_without_sending(
    cx: &mut TestAppContext,
) {
    let (chat, cx) = open_chat(cx);
    let before = chat.read_with(cx, |chat, _| chat.state.latest_sequence());
    cx.simulate_keystrokes(FIND);
    cx.simulate_input("needle");
    settle(cx);
    let (count, current, term) = search_state(&chat, cx);
    assert_eq!(
        (count, term.as_str()),
        (2, "needle"),
        "#b's line is not in scope"
    );
    // The newest hit is chosen first.
    let hits: Vec<u64> = chat.read_with(cx, |chat, _| {
        chat.search
            .as_ref()
            .unwrap()
            .hits()
            .iter()
            .map(|h| h.sequence)
            .collect()
    });
    assert_eq!(current, hits.last().copied());

    cx.simulate_keystrokes("enter");
    assert_eq!(
        search_state(&chat, cx).1,
        Some(hits[0]),
        "wraps to the first"
    );
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(search_state(&chat, cx).1, Some(hits[1]), "and back");
    cx.simulate_keystrokes("down");
    assert_eq!(search_state(&chat, cx).1, Some(hits[0]));
    // Nothing was sent or added to the log.
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.state.latest_sequence()),
        before
    );
    assert_eq!(selected_name(&chat, cx), "#a");
}

#[gpui::test]
fn a_changed_or_empty_term_updates_the_hits_and_clears_them(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    cx.simulate_keystrokes(FIND);
    cx.simulate_input("needle and");
    settle(cx);
    assert_eq!(search_state(&chat, cx).0, 1);
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    settle(cx);
    let (count, current, _) = search_state(&chat, cx);
    assert_eq!(count, 2, "{current:?}");
    cx.simulate_input("zzz");
    settle(cx);
    assert_eq!(search_state(&chat, cx), (0, None, "needlezzz".into()));
    // Clearing empties the field and keeps focus in it.
    chat.update_in(cx, |chat, window, cx| chat.search_clear(window, cx));
    settle(cx);
    assert_eq!(search_state(&chat, cx), (0, None, String::new()));
    chat.read_with(cx, |chat, _| {
        assert!(chat.search_ranges("needle", 0).0.is_empty());
    });
}

#[gpui::test]
fn all_scope_moves_to_hits_in_other_conversations(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    cx.simulate_keystrokes(FIND);
    chat.update_in(cx, |chat, window, cx| {
        chat.search_set_scope(Scope::All, window, cx)
    });
    cx.simulate_input("needle");
    settle(cx);
    assert_eq!(search_state(&chat, cx).0, 3);
    // The chronologically second hit is in #b.
    cx.simulate_keystrokes("enter");
    assert_eq!(selected_name(&chat, cx), "#a");
    cx.simulate_keystrokes("enter");
    assert_eq!(selected_name(&chat, cx), "#b");
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(selected_name(&chat, cx), "#a");
    // Focus stays in the search field across the selection change.
    cx.update(|window, cx| {
        chat.read_with(cx, |chat, cx| {
            let input = chat.search.as_ref().unwrap().input();
            assert!(input.focus_handle(cx).is_focused(window));
        });
    });
    // The matches are drawn in whichever log is shown.
    chat.read_with(cx, |chat, _| {
        let sequence = chat.search.as_ref().unwrap().current().unwrap().sequence;
        let (ranges, current) = chat.search_ranges("a Needle Needle", sequence);
        assert_eq!(ranges, vec![2..8, 9..15]);
        assert!(current);
    });
}

#[gpui::test]
fn escape_closes_the_bar_and_returns_focus_to_the_draft(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    cx.simulate_keystrokes(FIND);
    cx.simulate_input("needle");
    settle(cx);
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        chat.read_with(cx, |chat, cx| {
            assert!(!chat.search.as_ref().is_some_and(|s| s.is_open()));
            let draft = &chat.inputs[&chat.state.selection()];
            assert!(draft.focus_handle(cx).is_focused(window));
            assert_eq!(
                draft.read(cx).text(),
                "",
                "the keyword never reached the draft"
            );
            // A closed search draws no highlights.
            assert!(chat.search_ranges("needle", 0).0.is_empty());
        });
    });
}

#[gpui::test]
fn an_unfinished_composition_is_not_searched_until_it_is_committed(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.open_search(&FindInLog, window, cx)
    });
    let input = chat.read_with(cx, |chat, _| chat.search.as_ref().unwrap().input().clone());
    input.update_in(cx, |input, window, cx| {
        input.replace_and_mark_text_in_range(None, "ne", None, window, cx)
    });
    settle(cx);
    assert_eq!(search_state(&chat, cx).2, "", "still composing");
    input.update_in(cx, |input, window, cx| {
        input.replace_text_in_range(None, "needle", window, cx)
    });
    settle(cx);
    assert_eq!(search_state(&chat, cx).2, "needle");
    assert_eq!(search_state(&chat, cx).0, 2);
}

#[gpui::test]
fn lines_that_arrive_or_vanish_never_misdirect_the_search(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    cx.simulate_keystrokes(FIND);
    cx.simulate_input("needle");
    settle(cx);
    let (_, current, _) = search_state(&chat, cx);
    // A new matching line arrives: the count follows, the position stays.
    chat.update(cx, |chat, cx| {
        chat.state.append_channel_message(
            NetworkId(1),
            "#a",
            "bob",
            "another NEEDLE",
            Kind::Message,
            Provenance::Live,
        );
        cx.notify();
    });
    settle(cx);
    let (count, now, _) = search_state(&chat, cx);
    assert_eq!(count, 3);
    assert_eq!(now, current);
    // The conversation of the current hit goes away: stepping is a no-op or
    // lands on a line that still exists, never a panic.
    chat.update_in(cx, |chat, window, cx| {
        let id = chat.state.conversations()[1].id;
        chat.state.close_private(id);
        chat.search_move(true, window, cx);
        chat.search_move(false, window, cx);
    });
    settle(cx);
    assert!(matches!(
        chat.read_with(cx, |chat, _| chat.state.selection()),
        Selection::Channel(_)
    ));
}

#[gpui::test]
fn lines_arriving_all_the_time_do_not_hold_the_result_back(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    cx.simulate_keystrokes(FIND);
    cx.simulate_input("needle");
    // A busy channel: a new line every 100 ms while the search settles.
    for n in 0..30 {
        chat.update(cx, |chat, cx| {
            chat.state.append_channel_message(
                NetworkId(1),
                "#a",
                "bob",
                &format!("chatter {n}"),
                Kind::Message,
                Provenance::Live,
            );
            cx.notify();
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }
    let (count, current, _) = search_state(&chat, cx);
    assert_eq!(count, 2);
    assert!(current.is_some(), "the hit was chosen while lines arrived");
    // A line in another channel does not even restart a search of this one.
    let revision = chat.read_with(cx, |chat, _| chat.search_revision(Scope::Selected));
    chat.update(cx, |chat, cx| {
        chat.state.append_channel_message(
            NetworkId(1),
            "#b",
            "bob",
            "needle elsewhere",
            Kind::Message,
            Provenance::Live,
        );
        cx.notify();
    });
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.search_revision(Scope::Selected)),
        revision
    );
}

#[gpui::test]
fn ending_a_composition_without_new_text_searches_what_is_shown(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.open_search(&FindInLog, window, cx)
    });
    let input = chat.read_with(cx, |chat, _| chat.search.as_ref().unwrap().input().clone());
    input.update_in(cx, |input, window, cx| {
        input.replace_and_mark_text_in_range(None, "needle", None, window, cx)
    });
    settle(cx);
    assert_eq!(search_state(&chat, cx).2, "");
    input.update_in(cx, |input, window, cx| input.unmark_text(window, cx));
    settle(cx);
    assert_eq!(search_state(&chat, cx).2, "needle");
    assert_eq!(search_state(&chat, cx).0, 2);
}

#[cfg(unix_desktop)]
#[gpui::test]
fn the_emacs_key_theme_keeps_ctrl_f_for_the_cursor_and_opens_search_with_f3(
    cx: &mut TestAppContext,
) {
    let (chat, cx) = open_chat_with_keys(cx, true);
    cx.simulate_input("ab");
    cx.simulate_keystrokes("ctrl-a ctrl-f");
    assert!(chat.read_with(cx, |chat, _| chat.search.is_none()));
    cx.simulate_input("X");
    cx.update(|_, cx| {
        chat.read_with(cx, |chat, cx| {
            assert_eq!(chat.inputs[&chat.state.selection()].read(cx).text(), "aXb");
        });
    });
    cx.simulate_keystrokes("f3");
    assert!(chat.read_with(cx, |chat, _| {
        chat.search.as_ref().is_some_and(|s| s.is_open())
    }));
}

#[test]
fn styling_a_line_full_of_matches_takes_linear_time() {
    use crate::theme::Theme;
    use cayenchat_storage::{Appearance, ThemeMode};
    use gpui::WindowAppearance;

    let theme = Theme::new(
        ThemeMode::Light,
        WindowAppearance::Light,
        &Appearance::default(),
    );
    let text = "a ".repeat(8_000);
    // Separate matches, so none merge into one run.
    let found = cayenchat_app::search::Query::new("a")
        .unwrap()
        .ranges(&text);
    assert_eq!(found.len(), 8_000);
    let start = std::time::Instant::now();
    let _ = super::styled_log_text("", &text, &[], &[], &[], &found, true, None, &theme);
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[gpui::test]
fn a_match_far_down_a_tall_line_is_scrolled_into_view_without_another_key(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        let text = format!("{}INNER_TARGET", "long ".repeat(20_000));
        for text in [text.as_str(), "short last line"] {
            chat.state.append_channel_message(
                NetworkId(1),
                "#a",
                "bob",
                text,
                Kind::Message,
                Provenance::Live,
            );
        }
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes(FIND);
    cx.simulate_input("INNER_TARGET");
    settle(cx);
    // Frames, as the window would draw them; no key is pressed meanwhile.
    for _ in 0..4 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
    let (count, current, _) = search_state(&chat, cx);
    assert_eq!((count, current.is_some()), (1, true));
    chat.read_with(cx, |chat, _| {
        let search = chat.search.as_ref().unwrap();
        assert!(search.revealed(), "the match was painted inside the log");
    });
}

#[gpui::test]
fn a_match_in_a_tall_server_log_line_is_revealed_when_coming_from_all(cx: &mut TestAppContext) {
    tall_server_line_is_revealed(Scope::All, cx);
}

#[gpui::test]
fn a_match_in_a_tall_server_log_line_is_revealed_in_this_log(cx: &mut TestAppContext) {
    tall_server_line_is_revealed(Scope::Selected, cx);
}

fn tall_server_line_is_revealed(scope: Scope, cx: &mut TestAppContext) {
    {
        let (chat, cx) = open_chat(cx);
        chat.update(cx, |chat, cx| {
            let text = format!("{}SERVER_TARGET", "long ".repeat(20_000));
            chat.state.append_server_message(NetworkId(1), text);
            chat.state
                .append_server_message(NetworkId(1), "short last line".into());
            if scope == Scope::Selected {
                chat.state
                    .dispatch(cayenchat_app::Command::SelectServer(NetworkId(1)));
            }
            cx.notify();
        });
        cx.run_until_parked();
        chat.update_in(cx, |chat, window, cx| {
            chat.open_search(&FindInLog, window, cx)
        });
        chat.update_in(cx, |chat, window, cx| {
            chat.search_set_scope(scope, window, cx)
        });
        cx.simulate_input("SERVER_TARGET");
        settle(cx);
        for _ in 0..4 {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        chat.read_with(cx, |chat, _| {
            assert!(matches!(chat.state.selection(), Selection::Server(_)));
            let search = chat.search.as_ref().unwrap();
            assert!(search.revealed(), "the match was painted inside the log");
        });
    }
}

#[gpui::test]
fn the_toolbar_magnifier_toggles_the_bar(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    let open = |chat: &Entity<ChatWindow>, cx: &mut VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            chat.search.as_ref().is_some_and(|search| search.is_open())
        })
    };
    chat.update_in(cx, |chat, window, cx| chat.toggle_search(window, cx));
    assert!(open(&chat, cx));
    chat.update_in(cx, |chat, window, cx| chat.toggle_search(window, cx));
    assert!(!open(&chat, cx));
    chat.update_in(cx, |chat, window, cx| chat.toggle_search(window, cx));
    assert!(open(&chat, cx));
}

fn record(chat: &mut ChatWindow, view: DiagnosticsView, text: &str) {
    chat.state.record_diagnostic(
        NetworkId(1),
        view,
        std::time::SystemTime::UNIX_EPOCH,
        None,
        text.into(),
    );
}

/// The server selected, the connection diagnostics on, and a line each of
/// the two views.
fn with_diagnostics(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) {
    chat.update(cx, |chat, cx| {
        chat.debug_enabled = true;
        record(chat, DiagnosticsView::Overview, "Sleep notice: idle");
        record(chat, DiagnosticsView::Overview, "Handshake done");
        record(chat, DiagnosticsView::Protocol, "← :s NOTICE * :Wake-up");
        chat.state
            .dispatch(cayenchat_app::Command::SelectServer(NetworkId(1)));
        cx.notify();
    });
    cx.run_until_parked();
}

#[gpui::test]
fn this_log_searches_the_diagnostics_view_shown_and_all_reaches_the_other(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    with_diagnostics(&chat, cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.open_search(&FindInLog, window, cx)
    });
    cx.simulate_input("sleep notice");
    settle(cx);
    for _ in 0..4 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
    chat.read_with(cx, |chat, _| {
        let search = chat.search.as_ref().unwrap();
        assert_eq!(search.hits().len(), 1);
        assert_eq!(
            search.hits()[0].source,
            Source::Diagnostics(NetworkId(1), DiagnosticsView::Overview)
        );
        assert!(search.revealed(), "the diagnostics line was painted");
    });
    // The IRC protocol view is not shown, so "this log" does not read it.
    cx.simulate_input(" ");
    chat.update_in(cx, |chat, window, cx| chat.search_clear(window, cx));
    cx.simulate_input("wake-up");
    settle(cx);
    assert_eq!(search_state(&chat, cx).0, 0);
    // All logs: the hit is in the other view; going to it switches views.
    chat.update_in(cx, |chat, window, cx| {
        chat.search_set_scope(Scope::All, window, cx)
    });
    settle(cx);
    for _ in 0..4 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.search.as_ref().unwrap().hits().len(), 1);
        assert_eq!(chat.diagnostics_view, DiagnosticsView::Protocol);
        assert!(chat.search.as_ref().unwrap().revealed());
    });
}

#[gpui::test]
fn diagnostics_are_not_searched_while_they_are_off(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    with_diagnostics(&chat, cx);
    chat.update(cx, |chat, _| chat.debug_enabled = false);
    chat.update_in(cx, |chat, window, cx| {
        chat.open_search(&FindInLog, window, cx)
    });
    chat.update_in(cx, |chat, window, cx| {
        chat.search_set_scope(Scope::All, window, cx)
    });
    cx.simulate_input("handshake");
    settle(cx);
    assert_eq!(search_state(&chat, cx).0, 0);
}

#[gpui::test]
fn a_selection_copies_only_the_diagnostics_lines_it_covers(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    with_diagnostics(&chat, cx);
    chat.update(cx, |chat, cx| {
        let source = chat.state.selection();
        let rows: Vec<_> = chat
            .state
            .diagnostics(NetworkId(1), DiagnosticsView::Overview)
            .iter()
            .map(|line| (line.sequence, line.render()))
            .collect();
        // From the middle of the first line to the middle of the second.
        let from = rows[0].1.find("Sleep").unwrap();
        chat.log_selection = Some(crate::LogSelection {
            source,
            anchor: crate::LogPosition {
                row: rows[0].0,
                byte: from,
            },
            cursor: crate::LogPosition {
                row: rows[1].0,
                byte: rows[1].1.len() - 3,
            },
            diagnostics_end: chat.state.next_diagnostic_sequence(),
        });
        chat.copy_selected_log(cx);
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some(format!("Sleep notice: idle\n{}", &rows[1].1[..rows[1].1.len() - 3]).as_str())
        );
        // Another view, diagnostics turned off or lines of a new connection
        // end the selection: nothing more is copied.
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("kept".into()));
        let selection = chat.log_selection;
        chat.diagnostics_view = DiagnosticsView::Protocol;
        chat.drop_diagnostics_selection(None);
        assert!(chat.log_selection.is_none());
        chat.diagnostics_view = DiagnosticsView::Overview;
        chat.log_selection = selection;
        chat.drop_diagnostics_selection(Some(NetworkId(2)));
        assert!(chat.log_selection.is_some(), "another server's attempt");
        chat.drop_diagnostics_selection(Some(NetworkId(1)));
        assert!(chat.log_selection.is_none());
        chat.copy_selected_log(cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("kept")
        );
    });
}

#[gpui::test]
fn a_selection_runs_from_the_diagnostics_into_the_server_messages(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    with_diagnostics(&chat, cx);
    chat.update(cx, |chat, cx| {
        chat.state
            .append_server_message(NetworkId(1), "first line".into());
        chat.state
            .append_server_message(NetworkId(1), "second line".into());
        let source = chat.state.selection();
        let diagnostic = chat
            .state
            .diagnostics(NetworkId(1), DiagnosticsView::Overview)[1]
            .clone();
        let messages = chat.state.server_messages(NetworkId(1));
        let second = messages[messages.len() - 1].sequence;
        let text = diagnostic.render();
        let from = text.find("Handshake").unwrap();
        // The messages the server log already held lie between, whole.
        let expected: Vec<_> = std::iter::once("Handshake done".to_owned())
            .chain(
                messages[..messages.len() - 1]
                    .iter()
                    .map(|message| message.text.clone()),
            )
            .chain(std::iter::once("second".to_owned()))
            .collect();
        let expected = expected.join("\n");
        assert!(expected.contains("\nfirst line\nsecond"));
        // The last diagnostics line to the middle of the second message.
        chat.log_selection = Some(crate::LogSelection {
            source,
            anchor: crate::LogPosition {
                row: diagnostic.sequence,
                byte: from,
            },
            cursor: crate::LogPosition {
                row: second,
                byte: 6,
            },
            diagnostics_end: chat.state.next_diagnostic_sequence(),
        });
        chat.copy_selected_log(cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some(expected.as_str())
        );
        // The same range from the other end copies the same.
        let selection = chat.log_selection.as_mut().unwrap();
        std::mem::swap(&mut selection.anchor, &mut selection.cursor);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("old".into()));
        chat.copy_selected_log(cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some(expected.as_str())
        );
    });
}

#[gpui::test]
fn a_channel_selection_copies_only_the_diagnostics_line_shown(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        chat.debug_enabled = true;
        record(chat, DiagnosticsView::Overview, "Sleep notice: idle");
        record(chat, DiagnosticsView::Overview, "Handshake done");
        let channel = chat.state.selected_channel().unwrap().id;
        chat.state.append_conversation_message(
            channel,
            "bob",
            "hello world",
            cayenchat_app::Kind::Message,
            Default::default(),
            cayenchat_app::Unread::Leave,
        );
        // Registered: only the newest diagnostics line is drawn.
        let shown = chat.main_layout().diagnostics;
        assert_eq!(shown, 1..2);
        let lines = chat
            .state
            .diagnostics(NetworkId(1), DiagnosticsView::Overview);
        let hidden = lines[0].sequence;
        let row = chat
            .state
            .selected_channel()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .sequence;
        chat.log_selection = Some(crate::LogSelection {
            source: chat.state.selection(),
            anchor: crate::LogPosition {
                row: hidden,
                byte: 0,
            },
            cursor: crate::LogPosition { row, byte: 5 },
            diagnostics_end: chat.state.next_diagnostic_sequence(),
        });
        chat.copy_selected_log(cx);
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        let drawn = chat
            .state
            .diagnostics(NetworkId(1), DiagnosticsView::Overview)[1]
            .render();
        // The hidden line is not copied; the channel's earlier lines are.
        let copied = copied.unwrap();
        assert!(copied.starts_with(&format!("{drawn}\n")), "{copied}");
        assert!(copied.ends_with("\nhello"), "{copied}");
        assert!(!copied.contains("Sleep notice"), "{copied}");
    });
}
