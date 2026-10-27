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
