// SPDX-License-Identifier: GPL-3.0-only
//! Connections that need a password the store does not hold: the dialog, what
//! is saved, and what a session keeps (#635).
use super::{ChatWindow, Worker};
use cayenchat_irc_core::{ConnectionConfig, Event, PASSWORD_REFUSED, SaslCredentials};
use cayenchat_model::NetworkId;
use cayenchat_storage::{CredentialStore, Secret};
use gpui::TestAppContext;

const NET: NetworkId = NetworkId(1);

fn open_chat(
    cx: &mut TestAppContext,
) -> (
    gpui::Entity<ChatWindow>,
    CredentialStore,
    &mut gpui::VisualTestContext,
) {
    let store = cx.update(|cx| {
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
        crate::secrets::install_memory(cx)
    });
    let mut settings = crate::settings_with_channels("#a");
    let profile = &mut settings.servers[0];
    profile.nickname = "me".into();
    profile.username = "me".into();
    profile.host = "127.0.0.1".into();
    profile.port = 1;
    profile.use_tls = true;
    profile.sasl_enabled = true;
    profile.sasl_username = "acct".into();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, _| {
        chat.sessions.get_mut(&NET).unwrap().active_config = Some(config(true));
    });
    (chat, store, cx)
}

/// What a session keeps: the account, no password.
fn config(sasl: bool) -> ConnectionConfig {
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
    config.port = 1;
    if sasl {
        config.sasl = Some(SaslCredentials {
            mechanism: Default::default(),
            username: "acct".into(),
            password: String::new(),
        });
    }
    config
}

fn type_password(
    chat: &mut ChatWindow,
    sasl: bool,
    text: &str,
    cx: &mut gpui::Context<ChatWindow>,
) {
    let prompt = &chat.credential_prompts[0];
    let field = if sasl { &prompt.sasl } else { &prompt.server };
    field
        .as_ref()
        .unwrap()
        .input
        .update(cx, |input, cx| input.set_text(text, cx));
}

fn stop(chat: &mut ChatWindow) {
    if let Some(connection) = chat.sessions.get_mut(&NET).and_then(|s| s.irc.take()) {
        let _ = connection.disconnect();
    }
}

#[gpui::test]
fn a_reconnect_without_a_saved_sasl_password_asks_and_cancel_stops_it(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| chat.start_reconnect(NET, false, cx));
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert_eq!(chat.credential_prompts.len(), 1);
        let prompt = &chat.credential_prompts[0];
        assert!(prompt.sasl.is_some() && prompt.server.is_none());
        assert!(!prompt.sasl.as_ref().unwrap().remember, "off by default");
        assert!(chat.sessions[&NET].irc.is_none(), "nothing connects yet");
    });
    chat.update_in(cx, |chat, window, cx| {
        chat.cancel_credential_prompt(NET, window, cx)
    });
    chat.update(cx, |chat, cx| {
        assert!(chat.credential_prompts.is_empty());
        assert!(chat.sessions[&NET].manual_disconnect);
        // Neither the retry timer nor the wake-up starts it again.
        chat.schedule_retry(NET, cx);
        assert!(!chat.sessions[&NET].retry_pending);
        assert!(chat.sessions[&NET].irc.is_none());
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| assert!(chat.credential_prompts.is_empty()));
}

#[gpui::test]
fn a_reconnect_after_the_settings_changed_does_not_use_the_new_secrets(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        let profile = &mut chat.saved.servers[0];
        profile.sasl_mechanism = cayenchat_storage::SaslMechanism::EcdsaNist256pChallenge;
        profile.sasl_key_saved = true;
        chat.start_reconnect(NET, false, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty(), "no dialog either");
        let session = &chat.sessions[&NET];
        assert!(
            session.irc.is_none(),
            "nothing connects with mixed settings"
        );
        assert!(!session.retry_pending);
    });
}

#[gpui::test]
fn a_dialog_open_while_the_settings_change_sends_and_saves_nothing(cx: &mut TestAppContext) {
    for fresh in [false, true] {
        let (chat, store, cx) = open_chat(cx);
        let key = chat.read_with(cx, |chat, _| chat.saved.servers[0].sasl_password_key());
        chat.update_in(cx, |chat, window, cx| {
            if fresh {
                let missing = crate::connect_auth::Missing {
                    sasl: true,
                    ..Default::default()
                };
                chat.connect_fresh(NET, config(true), missing, window, cx)
                    .unwrap();
            } else {
                chat.start_reconnect(NET, false, cx);
            }
        });
        cx.run_until_parked();
        chat.update_in(cx, |chat, window, cx| {
            assert_eq!(chat.credential_prompts.len(), 1);
            chat.credential_prompts[0].sasl.as_mut().unwrap().remember = true;
            type_password(chat, true, "typed", cx);
            chat.saved.servers[0].host = "other.example".into();
            chat.submit_credential_prompt(NET, window, cx);
        });
        cx.run_until_parked();
        chat.update(cx, |chat, _| {
            assert!(chat.credential_prompts.is_empty(), "fresh: {fresh}");
            assert!(chat.sessions[&NET].irc.is_none(), "fresh: {fresh}");
        });
        assert!(!store.contains(&key).unwrap(), "fresh: {fresh}");
    }
}

#[gpui::test]
fn a_password_typed_in_the_dialog_is_used_but_not_saved_or_kept(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    let key = chat.read_with(cx, |chat, _| chat.saved.servers[0].sasl_password_key());
    chat.update(cx, |chat, cx| chat.start_reconnect(NET, false, cx));
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        // Empty is refused and the dialog stays.
        chat.submit_credential_prompt(NET, window, cx);
        assert_eq!(chat.credential_prompts.len(), 1);
        assert!(chat.credential_prompts[0].error.is_some());
        type_password(chat, true, "typed", cx);
        chat.submit_credential_prompt(NET, window, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty());
        let session = &chat.sessions[&NET];
        assert!(session.irc.is_some(), "the attempt went ahead");
        let sasl = session
            .active_config
            .as_ref()
            .unwrap()
            .sasl
            .as_ref()
            .unwrap();
        assert_eq!(sasl.password, "", "the session keeps no password");
        stop(chat);
    });
    assert!(!store.contains(&key).unwrap(), "unchecked: not saved");
}

#[gpui::test]
fn a_checked_password_is_saved_and_then_not_asked_for_again(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    let key = chat.read_with(cx, |chat, _| chat.saved.servers[0].sasl_password_key());
    chat.update(cx, |chat, cx| chat.start_reconnect(NET, false, cx));
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        chat.credential_prompts[0].sasl.as_mut().unwrap().remember = true;
        type_password(chat, true, "typed", cx);
        chat.submit_credential_prompt(NET, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(store.get(&key).unwrap().unwrap().expose(), "typed");
    chat.update(cx, |chat, cx| {
        stop(chat);
        chat.start_reconnect(NET, false, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty());
        assert!(chat.sessions[&NET].irc.is_some());
        stop(chat);
    });
}

#[gpui::test]
fn a_connection_that_needs_nothing_shows_no_dialog(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    let (sasl_key, server_key) = chat.read_with(cx, |chat, _| {
        (
            chat.saved.servers[0].sasl_password_key(),
            chat.saved.servers[0].server_password_key(),
        )
    });
    store.set(&sasl_key, &Secret::new("saved")).unwrap();
    chat.update(cx, |chat, cx| {
        chat.saved.servers[0].set_sasl_password_saved(true);
        // The server password was never used, so it is not asked for.
        assert!(!chat.sessions[&NET].server_password_used);
        chat.start_reconnect(NET, false, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty());
        assert!(chat.sessions[&NET].irc.is_some());
        stop(chat);
    });
    assert!(!store.contains(&server_key).unwrap());
}

#[gpui::test]
fn a_server_password_used_before_is_asked_for_when_it_is_not_saved(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    let sasl_key = chat.read_with(cx, |chat, _| chat.saved.servers[0].sasl_password_key());
    store.set(&sasl_key, &Secret::new("saved")).unwrap();
    chat.update(cx, |chat, cx| {
        chat.saved.servers[0].set_sasl_password_saved(true);
        chat.sessions.get_mut(&NET).unwrap().server_password_used = true;
        chat.start_reconnect(NET, false, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        let prompt = &chat.credential_prompts[0];
        assert!(prompt.server.is_some(), "the server password is missing");
        assert!(prompt.sasl.is_none(), "the saved SASL password is used");
    });
}

#[gpui::test]
fn a_464_asks_for_the_server_password_and_does_not_retry(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NET,
            vec![Event::Refused(PASSWORD_REFUSED.into())],
            Worker::Running,
            cx,
        );
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(!chat.sessions[&NET].retry_pending);
        let prompt = &chat.credential_prompts[0];
        assert!(prompt.server.is_some());
        assert!(!prompt.rejected, "none had been sent");
        assert!(prompt.sasl.is_some(), "the SASL password is missing too");
        assert_eq!(prompt.title_key(), "credential_prompt_server");
        assert!(chat.notices.all().is_empty(), "the dialog is the notice");
        let log = chat.state.server_messages(NET);
        assert!(log.iter().any(|m| m.text.contains("464")), "still logged");
    });
}

#[gpui::test]
fn a_464_after_sending_a_password_says_it_seems_wrong(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        chat.sessions.get_mut(&NET).unwrap().server_password_used = true;
        chat.handle_events(
            NET,
            vec![Event::Refused(PASSWORD_REFUSED.into())],
            Worker::Running,
            cx,
        );
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts[0].rejected);
        assert_eq!(
            chat.credential_prompts[0].title_key(),
            "credential_prompt_rejected"
        );
        assert!(chat.credential_prompts[0].server.is_some());
        assert!(chat.notices.all().is_empty());
    });
}

#[gpui::test]
fn a_sasl_failure_asks_for_the_sasl_password_without_reading_the_store(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    let sasl_key = chat.read_with(cx, |chat, _| chat.saved.servers[0].sasl_password_key());
    store.set(&sasl_key, &Secret::new("saved")).unwrap();
    chat.update(cx, |chat, cx| {
        chat.saved.servers[0].set_sasl_password_saved(true);
        chat.handle_events(
            NET,
            vec![Event::Refused(cayenchat_irc_core::SASL_FAILED.into())],
            Worker::Running,
            cx,
        );
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(!chat.sessions[&NET].retry_pending, "no automatic retry");
        let prompt = &chat.credential_prompts[0];
        assert!(prompt.sasl.is_some(), "the saved password is not reused");
        assert!(prompt.server.is_none());
        assert!(prompt.rejected);
        assert!(chat.notices.all().is_empty(), "the dialog is the notice");
    });
}

#[gpui::test]
fn another_refusal_still_raises_the_error_dialog(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NET,
            vec![Event::Refused("Server requires UTF-8.".into())],
            Worker::Running,
            cx,
        );
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty());
        assert!(!chat.notices.all().is_empty());
    });
}

#[gpui::test]
fn a_refused_sasl_key_raises_an_error_and_no_password_dialog(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NET,
            vec![Event::Refused(cayenchat_irc_core::SASL_KEY_REFUSED.into())],
            Worker::Running,
            cx,
        );
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty(), "no password to ask for");
        assert!(!chat.sessions[&NET].retry_pending, "no automatic retry");
        let notices = chat.notices.all();
        assert!(!notices.is_empty());
        let text = format!("{notices:?}");
        assert!(text.contains(&chat.i18n.text("sasl_key_refused")), "{text}");
    });
}

#[test]
fn only_a_password_failure_is_answered_by_the_dialog() {
    use crate::connect_auth::Refusal;
    assert_eq!(
        Refusal::of(cayenchat_irc_core::SASL_FAILED, false),
        Some(Refusal::Sasl)
    );
    assert_eq!(
        Refusal::of(cayenchat_irc_core::SASL_KEY_REFUSED, false),
        None
    );
}

#[gpui::test]
fn a_connection_keeps_no_password_in_the_session(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let mut config = config(true);
        config.server_password = Some("server".into());
        config.sasl.as_mut().unwrap().password = "sasl".into();
        chat.apply_connection(NET, config, window, cx).unwrap();
        let session = &chat.sessions[&NET];
        let kept = session.active_config.as_ref().unwrap();
        assert!(kept.server_password.is_none());
        assert_eq!(kept.sasl.as_ref().unwrap().password, "");
        assert_eq!(kept.sasl.as_ref().unwrap().username, "acct");
        assert!(session.server_password_used, "only the fact is kept");
        stop(chat);
    });
}

#[gpui::test]
fn a_reconnect_reads_only_the_passwords_noted_as_saved(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    let (watch, store) = cx.update(|_, cx| crate::secrets::PasswordWatch::install(cx));
    let key = chat.read_with(cx, |chat, _| chat.saved.servers[0].sasl_password_key());
    store.set(&key, &Secret::new("stored")).unwrap();

    // Not noted: the store is left alone and the dialog asks.
    chat.update(cx, |chat, cx| chat.start_reconnect(NET, false, cx));
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert_eq!(chat.credential_prompts.len(), 1);
        chat.credential_prompts.clear();
    });
    assert_eq!(watch.touches(), 0);

    // Noted, but SASL is off: still not read.
    chat.update(cx, |chat, cx| {
        let profile = &mut chat.saved.servers[0];
        profile.set_sasl_password_saved(true);
        profile.sasl_enabled = false;
        chat.sessions.get_mut(&NET).unwrap().active_config = Some(config(false));
        chat.start_reconnect(NET, false, cx);
    });
    cx.run_until_parked();
    assert_eq!(watch.touches(), 0);
    chat.update(cx, |chat, _| stop(chat));

    // Noted and SASL on: read and used, with no dialog.
    chat.update(cx, |chat, cx| {
        chat.saved.servers[0].sasl_enabled = true;
        chat.sessions.get_mut(&NET).unwrap().active_config = Some(config(true));
        chat.start_reconnect(NET, false, cx);
    });
    cx.run_until_parked();
    assert_eq!(watch.touches(), 1);
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty());
        stop(chat);
    });
}

/// Types into the SASL account field of the first dialog.
fn type_account(chat: &mut ChatWindow, text: &str, cx: &mut gpui::Context<ChatWindow>) {
    chat.credential_prompts[0]
        .account
        .as_ref()
        .unwrap()
        .update(cx, |input, cx| input.set_text(text, cx));
}

fn sent(chat: &ChatWindow) -> (Option<String>, Option<String>) {
    let (server, sasl) = &chat.sessions[&NET].sent_secrets;
    let text = |secret: &Option<Secret>| secret.as_ref().map(|s| s.expose().to_owned());
    (text(server), text(sasl))
}

#[gpui::test]
fn a_464_asks_only_for_the_server_password_and_sends_the_typed_sasl_password_again(
    cx: &mut TestAppContext,
) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| chat.start_reconnect(NET, false, cx));
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        type_password(chat, true, "typed", cx);
        chat.submit_credential_prompt(NET, window, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert_eq!(
            sent(chat),
            (None, Some("typed".into())),
            "kept while it runs"
        );
        stop(chat);
    });
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NET,
            vec![Event::Refused(PASSWORD_REFUSED.into())],
            Worker::Running,
            cx,
        );
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        let prompt = &chat.credential_prompts[0];
        assert!(prompt.server.is_some(), "the refused password is asked for");
        assert!(prompt.sasl.is_none() && prompt.account.is_none());
        assert_eq!(sent(chat), (None, None), "handed to the dialog");
        type_password(chat, false, "server", cx);
        chat.submit_credential_prompt(NET, window, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty());
        assert_eq!(
            sent(chat),
            (Some("server".into()), Some("typed".into())),
            "the SASL password went along"
        );
        stop(chat);
    });
}

#[gpui::test]
fn a_sasl_failure_asks_for_the_account_and_password_and_sends_the_server_password_again(
    cx: &mut TestAppContext,
) {
    let (chat, _store, cx) = open_chat(cx);
    let _file =
        crate::settings_file::TestFile::with(&chat.read_with(cx, |chat, _| chat.saved.clone()));
    chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&NET).unwrap();
        session.server_password_used = true;
        session.sent_secrets = (Some(Secret::new("server")), Some(Secret::new("wrong")));
        chat.handle_events(
            NET,
            vec![Event::Refused(cayenchat_irc_core::SASL_FAILED.into())],
            Worker::Running,
            cx,
        );
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        let prompt = &chat.credential_prompts[0];
        assert!(prompt.server.is_none(), "the server password is carried");
        assert!(prompt.sasl.is_some());
        assert_eq!(prompt.title_key(), "credential_prompt_sasl_rejected");
        let account = prompt.account.as_ref().unwrap().read(cx).text();
        assert_eq!(account, "acct", "the configured account is filled in");
        // An empty account is refused.
        type_account(chat, " ", cx);
        type_password(chat, true, "right", cx);
        chat.submit_credential_prompt(NET, window, cx);
        assert_eq!(chat.credential_prompts.len(), 1);
        type_account(chat, "other", cx);
        chat.submit_credential_prompt(NET, window, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty());
        assert_eq!(sent(chat), (Some("server".into()), Some("right".into())));
        assert_eq!(chat.saved.servers[0].sasl_username, "other");
        let kept = chat.sessions[&NET].active_config.as_ref().unwrap();
        assert_eq!(kept.sasl.as_ref().unwrap().username, "other");
        stop(chat);
    });
    let file = crate::settings_file::load().unwrap().unwrap();
    assert_eq!(
        file.servers[0].sasl_username, "other",
        "written to the profile"
    );
}

#[gpui::test]
fn sent_passwords_are_dropped_unless_a_refusal_uses_them(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    let hold = |chat: &mut ChatWindow| {
        chat.sessions.get_mut(&NET).unwrap().sent_secrets =
            (Some(Secret::new("a")), Some(Secret::new("b")));
    };
    for events in [
        vec![Event::Registered {
            nickname: "me".into(),
        }],
        vec![Event::Disconnected("lost".into())],
        vec![Event::Refused("Server requires UTF-8.".into())],
        vec![Event::Closed("bye".into())],
    ] {
        chat.update(cx, |chat, cx| {
            hold(chat);
            chat.handle_events(NET, events, Worker::Running, cx);
            assert_eq!(sent(chat), (None, None));
            chat.credential_prompts.clear();
            stop(chat);
        });
        cx.run_until_parked();
    }
    chat.update(cx, |chat, _| {
        hold(chat);
        chat.sessions.get_mut(&NET).unwrap().close();
        assert_eq!(sent(chat), (None, None), "a manual disconnect");
    });
}

#[gpui::test]
fn cancelling_the_dialog_drops_the_carried_password(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&NET).unwrap();
        session.sent_secrets = (None, Some(Secret::new("typed")));
        chat.handle_events(
            NET,
            vec![Event::Refused(PASSWORD_REFUSED.into())],
            Worker::Running,
            cx,
        );
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        assert!(chat.credential_prompts[0].sasl.is_none());
        chat.cancel_credential_prompt(NET, window, cx);
        assert!(chat.credential_prompts.is_empty());
        assert_eq!(sent(chat), (None, None));
        assert!(chat.sessions[&NET].manual_disconnect);
    });
}

#[gpui::test]
fn a_connection_that_cannot_start_does_not_keep_the_sent_passwords(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        // An empty nickname fails the synchronous check of the connection.
        let mut bad = config(true);
        bad.nickname = String::new();
        bad.server_password = Some("server".into());
        bad.sasl.as_mut().unwrap().password = "typed".into();
        chat.start_reconnect_with(NET, bad, false, cx);
        let session = &chat.sessions[&NET];
        assert!(session.irc.is_none());
        assert!(session.retry_pending, "the failure waits for a retry");
        assert_eq!(sent(chat), (None, None));
    });
}

#[gpui::test]
fn stopping_a_pending_retry_drops_the_sent_passwords(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&NET).unwrap();
        session.sent_secrets = (Some(Secret::new("a")), Some(Secret::new("b")));
        session.retry_pending = true;
        chat.disconnect(NET, cx);
        assert_eq!(sent(chat), (None, None));
    });
}

#[test]
fn the_profile_mechanism_reaches_the_connection_config() {
    use cayenchat_irc_core::SaslMechanism as Wire;
    use cayenchat_storage::SaslMechanism as Saved;
    let mut profile = cayenchat_storage::ServerProfile {
        sasl_enabled: true,
        sasl_username: "account".into(),
        username: "ident".into(),
        ..Default::default()
    };
    for (saved, wire) in [
        (Saved::Plain, Wire::Plain),
        (Saved::ScramSha1, Wire::ScramSha1),
        (Saved::ScramSha256, Wire::ScramSha256),
        (Saved::ScramSha512, Wire::ScramSha512),
        (Saved::EcdsaNist256pChallenge, Wire::EcdsaNist256pChallenge),
    ] {
        profile.sasl_mechanism = saved;
        let config = crate::connection_config_without_secrets(
            &profile,
            cayenchat_storage::Language::English,
        )
        .unwrap();
        assert_eq!(config.sasl.unwrap().mechanism, wire);
    }
}

/// A dialog is open for a refused connection, then another reconnect finds
/// the password in the store and registers (#722). Returns the store key of
/// the SASL password.
fn dialog_left_open_by_a_reconnect_that_registers(
    chat: &gpui::Entity<ChatWindow>,
    store: &CredentialStore,
    cx: &mut gpui::VisualTestContext,
) -> cayenchat_storage::SecretKey {
    chat.update(cx, |chat, cx| chat.start_reconnect(NET, false, cx));
    cx.run_until_parked();
    let key = chat.update(cx, |chat, cx| {
        assert_eq!(chat.credential_prompts.len(), 1);
        // The password is saved by another path, then Reconnect uses it.
        let key = chat.saved.servers[0].sasl_password_key();
        store.set(&key, &Secret::new("stored")).unwrap();
        chat.saved.servers[0].set_sasl_password_saved(true);
        chat.start_reconnect(NET, false, cx);
        key
    });
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        assert!(chat.sessions[&NET].irc.is_some(), "the reconnect ran");
        chat.handle_events(
            NET,
            vec![Event::Registered {
                nickname: "me".into(),
            }],
            Worker::Running,
            cx,
        );
    });
    key
}

#[gpui::test]
fn a_dialog_does_not_outlive_a_reconnect_that_registers(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    dialog_left_open_by_a_reconnect_that_registers(&chat, &store, cx);
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty(), "#722");
        stop(chat);
    });
}

#[gpui::test]
fn cancelling_a_leftover_dialog_does_not_stop_the_running_session(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    dialog_left_open_by_a_reconnect_that_registers(&chat, &store, cx);
    chat.update_in(cx, |chat, window, cx| {
        chat.cancel_credential_prompt(NET, window, cx)
    });
    chat.update(cx, |chat, _| {
        assert!(!chat.sessions[&NET].manual_disconnect, "#722");
        stop(chat);
    });
}

#[gpui::test]
fn submitting_a_leftover_dialog_does_not_overwrite_the_store(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    let key = dialog_left_open_by_a_reconnect_that_registers(&chat, &store, cx);
    chat.update_in(cx, |chat, window, cx| {
        if chat.credential_prompts.is_empty() {
            return;
        }
        chat.credential_prompts[0].sasl.as_mut().unwrap().remember = true;
        type_password(chat, true, "old-input", cx);
        chat.submit_credential_prompt(NET, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(store.get(&key).unwrap().unwrap().expose(), "stored", "#722");
    chat.update(cx, |chat, _| stop(chat));
}

// Reproduces #723, still open: the dialog connects with the configuration it
// opened with, and only host, port, TLS and SASL changes are caught as stale.
#[gpui::test]
#[ignore = "#723: the dialog's configuration snapshot ignores later channel changes"]
fn a_dialog_connects_with_the_settings_saved_while_it_was_open(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let missing = crate::connect_auth::Missing {
            sasl: true,
            ..Default::default()
        };
        chat.connect_fresh(NET, config(true), missing, window, cx)
            .unwrap();
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        assert_eq!(chat.credential_prompts.len(), 1);
        type_password(chat, true, "typed", cx);
        let mut settings = chat.saved.clone();
        settings.servers[0].channels = "#b".into();
        chat.saved = settings.clone();
        chat.apply_servers(settings, cx);
        chat.submit_credential_prompt(NET, window, cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        let used = &chat.sessions[&NET].active_config.as_ref().unwrap().channels;
        assert_eq!(used, &["#b".to_owned()], "#723");
        stop(chat);
    });
}

#[gpui::test]
fn a_dialog_left_open_does_not_outlive_a_reconnect_by_another_path(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(cx);
    let key = chat.read_with(cx, |chat, _| chat.saved.servers[0].sasl_password_key());
    chat.update(cx, |chat, cx| chat.start_reconnect(NET, false, cx));
    cx.run_until_parked();
    chat.update(cx, |chat, _| assert_eq!(chat.credential_prompts.len(), 1));
    // The password reaches the store while the dialog is open.
    store.set(&key, &Secret::new("saved")).unwrap();
    chat.update(cx, |chat, cx| {
        chat.saved.servers[0].set_sasl_password_saved(true);
        chat.start_reconnect(NET, false, cx);
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        assert!(chat.credential_prompts.is_empty(), "the dialog is gone");
        assert!(chat.sessions[&NET].irc.is_some());
        // What a stale dialog's buttons would have done is nothing.
        chat.cancel_credential_prompt(NET, window, cx);
        assert!(!chat.sessions[&NET].manual_disconnect);
        stop(chat);
    });
    assert_eq!(store.get(&key).unwrap().unwrap().expose(), "saved");
}
