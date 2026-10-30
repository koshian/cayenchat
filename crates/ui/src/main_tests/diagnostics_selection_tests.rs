// SPDX-License-Identifier: GPL-3.0-only
//! Selecting, copying and searching the connection diagnostics rows (#790).
use super::{ChatWindow, DiagnosticsView, Worker, session::DiagnosticLine, themed_bindings};
use cayenchat_app::{Selection, search::Scope};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::{ClipboardItem, Entity, TestAppContext, VisualTestContext};
use std::time::{Duration, SystemTime};

const NETWORK: NetworkId = NetworkId(1);

fn line(text: &str) -> DiagnosticLine {
    DiagnosticLine {
        at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
        elapsed: None,
        text: text.into(),
    }
}

fn open_chat(cx: &mut TestAppContext) -> (Entity<ChatWindow>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::theme::apply(
            cayenchat_storage::ThemeMode::Light,
            &cayenchat_storage::Appearance::default(),
            cx,
        );
        crate::input::bind_keys(false, cx);
        cx.bind_keys(themed_bindings(
            cayenchat_storage::ChannelNumberModifier::Ctrl,
            &Default::default(),
            false,
        ));
    });
    let settings = crate::settings_with_channels("#a");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NETWORK,
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
        chat.state
            .dispatch(cayenchat_app::Command::SelectServer(NETWORK));
        chat.set_debug(true, cx);
        let session = chat.sessions.get_mut(&NETWORK).unwrap();
        session.clear_diagnostics();
        session.record(DiagnosticsView::Overview, line("Sleep notice: going down"));
        session.record(DiagnosticsView::Overview, line("Connecting"));
        session.record(DiagnosticsView::Overview, line("Wake-up: resumed"));
        session.record(DiagnosticsView::Protocol, line("→ CAP LS 302"));
        cx.notify();
    });
    cx.run_until_parked();
    (chat, cx)
}

fn settle(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
}

fn text_of(chat: &ChatWindow, view: DiagnosticsView, index: usize) -> String {
    let session = chat.sessions.get(&NETWORK).unwrap();
    super::diagnostic_text(&session.lines(view)[index].render())
}

#[gpui::test]
fn a_part_of_a_line_and_whole_lines_are_copied_as_drawn(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    let (first, third, notice_at) = chat.read_with(cx, |chat, _| {
        let session = chat.sessions.get(&NETWORK).unwrap();
        let id = session.first_id(DiagnosticsView::Overview);
        let text = text_of(chat, DiagnosticsView::Overview, 0);
        (id, id + 2, text.find("Sleep").unwrap())
    });
    // Nothing selected: copying leaves the clipboard alone.
    cx.write_to_clipboard(ClipboardItem::new_string("kept".into()));
    chat.update(cx, |chat, cx| chat.copy_selected_log(cx));
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "kept");

    chat.update_in(cx, |chat, window, cx| {
        chat.start_diagnostics_selection(first, notice_at, window, cx);
        chat.extend_diagnostics_selection(first, notice_at + "Sleep notice".len(), cx);
        chat.finish_log_selection();
        chat.copy_selected_log(cx);
    });
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().unwrap(),
        "Sleep notice"
    );

    // Several lines: the ends are cut, the lines between are whole.
    let (expected, ()) = chat.read_with(cx, |chat, _| {
        let a = text_of(chat, DiagnosticsView::Overview, 0);
        let b = text_of(chat, DiagnosticsView::Overview, 1);
        let c = text_of(chat, DiagnosticsView::Overview, 2);
        (
            format!("{}\n{b}\n{}", &a[notice_at..], &c[..c.len() - 3]),
            (),
        )
    });
    chat.update_in(cx, |chat, window, cx| {
        let end = chat
            .sessions
            .get(&NETWORK)
            .map(|_| text_of(chat, DiagnosticsView::Overview, 2).len() - 3)
            .unwrap();
        chat.start_diagnostics_selection(first, notice_at, window, cx);
        chat.extend_diagnostics_selection(third, end, cx);
        chat.finish_log_selection();
        chat.copy_selected_log(cx);
    });
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), expected);
}

#[gpui::test]
fn a_selection_never_copies_another_line_after_a_switch_or_a_trim(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    let first = chat.read_with(cx, |chat, _| {
        chat.sessions
            .get(&NETWORK)
            .unwrap()
            .first_id(DiagnosticsView::Overview)
    });
    chat.update_in(cx, |chat, window, cx| {
        let at = text_of(chat, DiagnosticsView::Overview, 1)
            .find("Connecting")
            .unwrap();
        chat.start_diagnostics_selection(first + 1, at, window, cx);
        chat.extend_diagnostics_selection(first + 1, at + 4, cx);
        chat.finish_log_selection();
    });
    // The other view shows other lines: nothing is copied from it.
    cx.write_to_clipboard(ClipboardItem::new_string("kept".into()));
    chat.update_in(cx, |chat, window, cx| {
        chat.toggle_diagnostics_view(&super::ToggleDiagnosticsView, window, cx);
        chat.copy_selected_log(cx);
    });
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "kept");
    chat.update_in(cx, |chat, window, cx| {
        chat.toggle_diagnostics_view(&super::ToggleDiagnosticsView, window, cx);
    });
    // The lines before it are trimmed away: the id still names "Connecting".
    chat.update(cx, |chat, _| {
        let session = chat.sessions.get_mut(&NETWORK).unwrap();
        for n in 0..crate::session::DIAGNOSTIC_LIMIT - 2 {
            session.record(DiagnosticsView::Overview, line(&format!("later {n}")));
        }
        assert_eq!(session.first_id(DiagnosticsView::Overview), first + 1);
    });
    chat.update(cx, |chat, cx| {
        chat.copy_selected_log(cx);
    });
    let copied = cx.read_from_clipboard().unwrap().text().unwrap();
    assert!(copied.ends_with("Conn"), "{copied}");
    // One more line trims "Connecting" itself: nothing is copied.
    cx.write_to_clipboard(ClipboardItem::new_string("kept".into()));
    chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&NETWORK).unwrap();
        session.record(DiagnosticsView::Overview, line("one more"));
        session.record(DiagnosticsView::Overview, line("and more"));
        chat.copy_selected_log(cx);
    });
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "kept");
}

#[gpui::test]
fn this_log_finds_the_lines_on_show_and_all_finds_the_other_view(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.open_search(&super::FindInLog, window, cx)
    });
    cx.simulate_input("wake-up");
    settle(cx);
    chat.read_with(cx, |chat, _| {
        let search = chat.search.as_ref().unwrap();
        assert_eq!(search.diagnostics.len(), 1);
        assert_eq!(
            search.current_diagnostic,
            search.diagnostics.first().copied()
        );
        let text = text_of(chat, DiagnosticsView::Overview, 2);
        let (ranges, current) = chat.search_diagnostic_ranges(
            &text,
            NETWORK,
            DiagnosticsView::Overview,
            search.diagnostics[0].id,
        );
        assert_eq!(ranges.len(), 1);
        assert!(current);
    });

    // "CAP LS" is only in the protocol view: not found in this log's view,
    // found by All, which switches the view to show it.
    cx.simulate_input(" ");
    chat.update_in(cx, |chat, window, cx| {
        chat.search_clear(window, cx);
    });
    cx.simulate_input("cap ls");
    settle(cx);
    chat.read_with(cx, |chat, _| {
        assert!(chat.search.as_ref().unwrap().diagnostics.is_empty());
    });
    chat.update_in(cx, |chat, window, cx| {
        chat.search_set_scope(Scope::All, window, cx)
    });
    settle(cx);
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.diagnostics_view, DiagnosticsView::Protocol);
        assert_eq!(chat.state.selection(), Selection::Server(NETWORK));
        assert_eq!(chat.search.as_ref().unwrap().diagnostics.len(), 1);
    });
}

#[gpui::test]
fn a_trimmed_line_is_skipped_by_the_search(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.open_search(&super::FindInLog, window, cx)
    });
    cx.simulate_input("sleep notice");
    settle(cx);
    chat.update(cx, |chat, _| {
        let session = chat.sessions.get_mut(&NETWORK).unwrap();
        for n in 0..crate::session::DIAGNOSTIC_LIMIT {
            session.record(DiagnosticsView::Overview, line(&format!("later {n}")));
        }
        assert!(session.line_by_id(DiagnosticsView::Overview, 0).is_none());
    });
    let before = chat.read_with(cx, |chat, _| {
        chat.search.as_ref().unwrap().current_diagnostic
    });
    chat.update_in(cx, |chat, window, cx| chat.search_move(true, window, cx));
    // The hit's line is gone: the move does not land on another line.
    chat.read_with(cx, |chat, _| {
        let search = chat.search.as_ref().unwrap();
        assert_eq!(search.current_diagnostic, before);
        assert!(search.current().is_none());
    });
}
