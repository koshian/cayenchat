// SPDX-License-Identifier: GPL-3.0-only
use super::ChatWindow;
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::{Focusable as _, TestAppContext};

fn install_globals(cx: &mut TestAppContext) {
    crate::support::install_test_globals(cx);
}

#[gpui::test]
fn a_failed_send_opens_one_dialog_and_keeps_the_draft(cx: &mut TestAppContext) {
    install_globals(cx);
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();
    let input = chat.read_with(cx, |chat, _| chat.inputs[&chat.state.selection()].clone());
    input.update(cx, |input, cx| input.set_text("hello", cx));
    chat.update_in(cx, |chat, window, cx| chat.send_draft(false, window, cx));
    cx.run_until_parked();
    let (_, detail) = cx
        .pending_prompt()
        .expect("the failure is shown in a dialog");
    let reason = chat.read_with(cx, |chat, _| chat.i18n.text("not_connected"));
    assert!(detail.contains(&reason), "{detail}");

    // The same failure again while the dialog is open adds nothing.
    chat.update_in(cx, |chat, window, cx| chat.send_draft(false, window, cx));
    cx.run_until_parked();
    assert_eq!(chat.read_with(cx, |chat, _| chat.notices.all().len()), 1);

    // Closing changes nothing else: the draft is as typed and focused.
    cx.simulate_prompt_answer("Close");
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());
    assert_eq!(
        input.read_with(cx, |input, _| input.text().to_owned()),
        "hello"
    );
    assert!(chat.read_with(cx, |chat, _| chat.notices.all().is_empty()));
    assert!(
        cx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)),
        "the draft has the focus again"
    );

    // What the user triggers again is shown again.
    chat.update_in(cx, |chat, window, cx| chat.send_draft(false, window, cx));
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Close");
}

#[gpui::test]
fn different_failures_wait_for_the_open_dialog(cx: &mut TestAppContext) {
    install_globals(cx);
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        chat.report(Some("first".into()), cx);
        chat.report(Some("second".into()), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        cx.pending_prompt().map(|(_, detail)| detail),
        Some("first".into())
    );
    cx.simulate_prompt_answer("Close");
    cx.run_until_parked();
    assert_eq!(
        cx.pending_prompt().map(|(_, detail)| detail),
        Some("second".into())
    );
    cx.simulate_prompt_answer("Close");
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
fn server_rejections_open_a_dialog_only_as_the_answer_to_the_user(cx: &mut TestAppContext) {
    install_globals(cx);
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();
    let network = chat.read_with(cx, |chat, _| chat.state.networks()[0].id);
    let reject = |code, command: Option<&str>, reason: &str| Event::CommandRejected {
        code,
        command: command.map(str::to_owned),
        reason: reason.to_owned(),
    };

    // Nothing was sent by the user: only the log (written from the
    // `ServerLine`) has it.
    chat.update(cx, |chat, cx| {
        chat.handle_event(network, reject(417, None, "Input line was too long"), cx)
    });
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());

    // Another server's command does not explain it.
    chat.update(cx, |chat, cx| {
        chat.note_spoken(NetworkId(network.0 + 1));
        chat.handle_event(network, reject(417, None, "Input line was too long"), cx)
    });
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());

    chat.update(cx, |chat, cx| {
        chat.note_spoken(network);
        chat.handle_event(network, reject(417, None, "Input line was too long"), cx)
    });
    cx.run_until_parked();
    let (title, detail) = cx
        .pending_prompt()
        .expect("the answer to the user's command");
    assert!(!title.is_empty());
    assert!(detail.contains("417") && detail.contains("Input line was too long"));
    cx.simulate_prompt_answer("Close");
    cx.run_until_parked();

    // One command is answered once; a second reply stays in the log.
    chat.update(cx, |chat, cx| {
        chat.handle_event(network, reject(400, Some("MODE"), "Cannot do that"), cx)
    });
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());

    chat.update(cx, |chat, cx| {
        chat.note_spoken(network);
        chat.handle_event(network, reject(400, Some("PRIVMSG"), "Cannot do that"), cx)
    });
    cx.run_until_parked();
    let (_, detail) = cx.pending_prompt().expect("a 400 for a new command");
    assert!(detail.contains("400 PRIVMSG") && detail.contains("Cannot do that"));
    cx.simulate_prompt_answer("Close");
}

#[gpui::test]
fn only_what_the_draft_said_is_answered_by_a_dialog(cx: &mut TestAppContext) {
    install_globals(cx);
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    // A connection to a local listener that never answers: sends are queued
    // on it, nothing reaches a server.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config =
        cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
    config.port = listener.local_addr().unwrap().port();
    config.use_tls = false;
    let connection = cayenchat_irc_core::Connection::connect(config.clone()).unwrap();
    let network = NetworkId(1);
    chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&network).unwrap();
        session.active_config = Some(config);
        session.irc = Some(connection);
        chat.handle_events(
            network,
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::PrivateMessage {
                    sender: "bob".into(),
                    text: "hi".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            super::Worker::Running,
            cx,
        );
    });
    let reject = |code, command: Option<&str>| Event::CommandRejected {
        code,
        command: command.map(str::to_owned),
        reason: "Refused".to_owned(),
    };
    let select = |cx: &mut gpui::VisualTestContext, private: bool| {
        chat.update(cx, |chat, _| {
            let id = chat
                .state
                .conversations()
                .iter()
                .find(|c| c.is_private() == private)
                .expect("a conversation")
                .id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(id));
        });
    };
    let send = |cx: &mut gpui::VisualTestContext, text: &str| {
        let input = chat.read_with(cx, |chat, _| chat.inputs[&chat.state.selection()].clone());
        input.update(cx, |input, cx| input.set_text(text, cx));
        chat.update_in(cx, |chat, window, cx| chat.send_draft(false, window, cx));
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt(), "a send shows no dialog by itself");
    };
    let reply = |cx: &mut gpui::VisualTestContext, event: Event| {
        chat.update(cx, |chat, cx| chat.handle_event(network, event, cx));
        cx.run_until_parked();
    };
    let answered = |cx: &mut gpui::VisualTestContext| {
        let shown = cx.has_pending_prompt();
        if shown {
            cx.simulate_prompt_answer("Close");
            cx.run_until_parked();
        }
        shown
    };

    // A message or `/me`, in a channel or a query, is answered once.
    for private in [false, true] {
        select(cx, private);
        for (text, code, command) in [
            ("hello", 417, None),
            ("hello", 400, Some("PRIVMSG")),
            ("/me waves", 417, None),
            ("/me waves", 400, Some("NOTICE")),
        ] {
            send(cx, text);
            reply(cx, reject(code, command));
            assert!(answered(cx), "{text} {code}");
            reply(cx, reject(code, command));
            assert!(!answered(cx), "once per send: {text} {code}");
        }
    }

    // A 400 for another command is only in the log and keeps the send.
    send(cx, "hello");
    reply(cx, reject(400, Some("CHATHISTORY")));
    assert!(!answered(cx));
    reply(cx, reject(417, None));
    assert!(answered(cx));

    // So does a 400 that names no command.
    send(cx, "hello");
    reply(cx, reject(400, None));
    assert!(!answered(cx));
    reply(cx, reject(417, None));
    assert!(answered(cx));

    // Commands and other paths are not answered, whatever the reply.
    for text in [
        "/msg bob hi",
        "/notice bob hi",
        "/names #a",
        "/ctcp bob VERSION",
        "/raw PRIVMSG #a :hi",
        "/join #b",
        "/part #b",
    ] {
        send(cx, text);
        reply(cx, reject(417, None));
        assert!(!answered(cx), "{text}");
        reply(cx, reject(400, Some("PRIVMSG")));
        assert!(!answered(cx), "{text}");
    }
}
