// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use crate::channel_keys::KeyJoin;
use cayenchat_irc_core::{ChannelActivityKind, ConnectionConfig, Event};
use cayenchat_model::NetworkId;
use cayenchat_storage::{CredentialStore, Secret, SecretKey};
use gpui::TestAppContext;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::mpsc;

const NET: NetworkId = NetworkId(1);

fn open_chat<'a>(
    saved_key: Option<&str>,
    cx: &'a mut TestAppContext,
) -> (
    gpui::Entity<ChatWindow>,
    CredentialStore,
    &'a mut gpui::VisualTestContext,
) {
    let store = cx.update(|cx| {
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
        crate::secrets::install_memory(cx)
    });
    let settings = crate::settings_with_channels("#Secret");
    let profile = settings.servers[0].id.clone();
    if let Some(key) = saved_key {
        store
            .set(
                &SecretKey::channel_key(&profile, "#secret"),
                &Secret::new(key),
            )
            .unwrap();
    }
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, _| {
        let mut config = ConnectionConfig::tls(
            "irc.example.org".into(),
            "me".into(),
            vec!["#Secret".into()],
        );
        if let Some(key) = saved_key {
            config.channel_keys.insert("#Secret".into(), key.into());
        }
        chat.sessions.get_mut(&NET).unwrap().active_config = Some(config);
    });
    (chat, store, cx)
}

fn event(chat: &mut ChatWindow, event: Event, cx: &mut gpui::Context<ChatWindow>) {
    chat.handle_events(NET, vec![event], Worker::Running, cx);
}

fn registered() -> Event {
    Event::Registered {
        nickname: "me".into(),
    }
}

fn refused(channel: &str, bad_key: bool) -> Event {
    Event::ChannelActivity {
        channel: channel.into(),
        actor: "server".into(),
        kind: ChannelActivityKind::JoinFailed {
            banned: false,
            bad_key,
            reason: "Cannot join channel".into(),
        },
        server_time: None,
    }
}

fn joined(channel: &str) -> Event {
    Event::Joined {
        channel: channel.into(),
    }
}

fn key_of(store: &CredentialStore, chat: &ChatWindow) -> Option<String> {
    let profile = chat.sessions[&NET].profile_id.clone();
    store
        .get(&SecretKey::channel_key(&profile, "#secret"))
        .unwrap()
        .map(|key| key.expose().to_owned())
}

fn local_connection() -> (
    cayenchat_irc_core::Connection,
    ConnectionConfig,
    mpsc::Receiver<Option<String>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (join_tx, join_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = join_tx.send(None);
                    return;
                }
                Ok(_) if line.starts_with("JOIN ") => {
                    let _ = join_tx.send(Some(line.trim_end().to_owned()));
                    return;
                }
                Ok(_) => {}
            }
        }
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
    config.port = port;
    config.use_tls = false;
    let connection = cayenchat_irc_core::Connection::connect(config.clone()).unwrap();
    (connection, config, join_rx)
}

#[gpui::test]
fn only_475_for_our_own_join_opens_the_dialog(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        // Other refusals cannot be cured by a key.
        event(chat, refused("#secret", false), cx);
        assert!(chat.key_prompts.is_empty());
        event(chat, registered(), cx);
        // A channel we never asked for.
        event(chat, refused("#other", true), cx);
        assert!(chat.key_prompts.is_empty());
        // The auto-join of a differently cased name, answered once.
        event(chat, refused("#SECRET", true), cx);
        assert_eq!(chat.key_prompts.len(), 1);
        event(chat, refused("#SECRET", true), cx);
        assert_eq!(chat.key_prompts.len(), 1, "no second dialog from a replay");
    });
    // The dialog draws (masked field, checkbox, buttons).
    cx.update(|window, cx| window.draw(cx).clear());
}

#[gpui::test]
fn delayed_join_key_read_uses_the_registered_connection_and_drops_replaced_connection(
    cx: &mut TestAppContext,
) {
    let (chat, store, cx) = open_chat(None, cx);
    let profile = chat.read_with(cx, |chat, _| chat.sessions[&NET].profile_id.clone());
    store
        .set(
            &SecretKey::channel_key(&profile, "#Secret"),
            &Secret::new("saved-key"),
        )
        .unwrap();

    let (old_connection, old_config, _old_join) = local_connection();
    chat.update(cx, |chat, cx| {
        chat.saved.servers[0].keyed_channels = Some(vec!["#Secret".into()]);
        let session = chat.sessions.get_mut(&NET).unwrap();
        let mut old_config = old_config;
        old_config.channels.clear();
        session.active_config = Some(old_config);
        session.irc = Some(old_connection);
        event(chat, registered(), cx);
        chat.join_channels(NET, &["#Secret".into()], cx).unwrap();
    });
    let (new_connection, new_config, new_join) = local_connection();
    chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&NET).unwrap();
        session.generation += 1;
        let mut new_config = new_config;
        new_config.host = "localhost".into();
        session.active_config = Some(new_config);
        session.irc = Some(new_connection);
        event(chat, registered(), cx);
    });
    cx.run_until_parked();

    assert_eq!(
        new_join
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        None,
        "a JOIN with the old connection's key must not reach the replacement"
    );
    chat.update(cx, |chat, _| {
        assert!(chat.sessions[&NET].pending_joins.is_empty());
        assert!(chat.sessions[&NET].key_joins.is_empty());
    });
}

#[gpui::test]
fn delayed_join_key_read_sends_the_key_when_its_connection_is_still_current(
    cx: &mut TestAppContext,
) {
    let (chat, store, cx) = open_chat(None, cx);
    let profile = chat.read_with(cx, |chat, _| chat.sessions[&NET].profile_id.clone());
    store
        .set(
            &SecretKey::channel_key(&profile, "#Secret"),
            &Secret::new("saved-key"),
        )
        .unwrap();
    let (connection, config, join_rx) = local_connection();
    chat.update(cx, |chat, cx| {
        chat.saved.servers[0].keyed_channels = Some(vec!["#Secret".into()]);
        let session = chat.sessions.get_mut(&NET).unwrap();
        let mut config = config;
        config.channels.clear();
        session.active_config = Some(config);
        session.irc = Some(connection);
        event(chat, registered(), cx);
        chat.join_channels(NET, &["#Secret".into()], cx).unwrap();
    });
    cx.run_until_parked();
    assert_eq!(
        join_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        Some("JOIN #Secret saved-key".into())
    );
}

#[gpui::test]
fn a_refresh_snapshot_captured_during_a_key_write_cannot_restore_a_forgotten_key(
    cx: &mut TestAppContext,
) {
    let (chat, store, cx) = open_chat(Some("old-key"), cx);
    chat.update(cx, |chat, _| {
        chat.saved.servers[0].keyed_channels = Some(vec!["#Secret".into()]);
    });
    chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&NET).unwrap();
        session.key_joins.push(KeyJoin {
            channel: "#Secret".into(),
            typed: Some(Secret::new("old-key")),
            remember: false,
            stale: true,
        });
        chat.key_join_succeeded(NET, "#Secret", cx);
    });
    let (profile, read, generation, revision, old_snapshot) = chat.read_with(cx, |chat, _| {
        let profile = chat
            .saved
            .profile(&chat.sessions[&NET].profile_id)
            .unwrap()
            .clone();
        let generation = chat.sessions[&NET].generation;
        let session = &chat.sessions[&NET];
        let revision = session.channel_key_revision;
        assert_eq!(session.channel_key_writes_pending, 1);
        let snapshot = store
            .get(&SecretKey::channel_key(&profile.id, "#Secret"))
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.expose(), "old-key");
        (
            profile.clone(),
            profile.channels_to_read_keys_for(),
            generation,
            revision,
            BTreeMap::from([("#Secret".to_owned(), snapshot.expose().to_owned())]),
        )
    });
    cx.run_until_parked();
    assert!(
        store
            .get(&SecretKey::channel_key(&profile.id, "#Secret"))
            .unwrap()
            .is_none(),
        "the forget completes before the old refresh snapshot is applied"
    );
    chat.update(cx, |chat, _| {
        assert_eq!(
            chat.sessions[&NET].channel_key_revision,
            revision.wrapping_add(1),
            "a successful forget invalidates earlier refresh snapshots"
        );
        assert!(
            !chat.sessions[&NET]
                .active_config
                .as_ref()
                .unwrap()
                .channel_keys
                .contains_key("#Secret")
        );
    });

    chat.update(cx, |chat, _| {
        chat.finish_key_refresh(NET, &profile, &read, generation, revision, old_snapshot);
        assert!(
            !chat.sessions[&NET]
                .active_config
                .as_ref()
                .unwrap()
                .channel_keys
                .contains_key("#Secret")
        );
        assert!(!chat.saved.servers[0].channel_key_noted("#Secret"));
    });
}

#[gpui::test]
fn successful_key_writes_for_different_channels_both_update_the_config(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(None, cx);
    let profile = chat.read_with(cx, |chat, _| chat.sessions[&NET].profile_id.clone());
    store
        .set(
            &SecretKey::channel_key(&profile, "#a"),
            &Secret::new("old-a"),
        )
        .unwrap();
    chat.update(cx, |chat, cx| {
        chat.saved.servers[0].keyed_channels = Some(vec!["#a".into()]);
        let session = chat.sessions.get_mut(&NET).unwrap();
        let config = session.active_config.as_mut().unwrap();
        config.channels = vec!["#a".into(), "#b".into()];
        config.channel_keys.insert("#a".into(), "old-a".into());
        session.key_joins = vec![
            KeyJoin {
                channel: "#a".into(),
                typed: Some(Secret::new("rejected-a")),
                remember: false,
                stale: true,
            },
            KeyJoin {
                channel: "#b".into(),
                typed: Some(Secret::new("key-b")),
                remember: true,
                stale: false,
            },
        ];
        chat.key_join_succeeded(NET, "#a", cx);
        chat.key_join_succeeded(NET, "#b", cx);
    });
    cx.run_until_parked();

    chat.update(cx, |chat, _| {
        let session = &chat.sessions[&NET];
        let config = session.active_config.as_ref().unwrap();
        assert!(!config.channel_keys.contains_key("#a"));
        assert_eq!(
            config.channel_keys.get("#b").map(String::as_str),
            Some("key-b")
        );
        assert!(!chat.saved.servers[0].channel_key_noted("#a"));
        assert!(chat.saved.servers[0].channel_key_noted("#b"));
    });
    assert!(
        store
            .get(&SecretKey::channel_key(&profile, "#a"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .get(&SecretKey::channel_key(&profile, "#b"))
            .unwrap()
            .unwrap()
            .expose(),
        "key-b"
    );
}

#[test]
fn join_targets_match_only_the_same_generation() {
    let mut session = crate::session::ServerSession::new("profile".into());
    session.active_config = Some(ConnectionConfig::tls(
        "irc.example.org".into(),
        "me".into(),
        vec![],
    ));
    let captured = session.generation;
    assert_eq!(session.generation, captured);

    // A new destination always comes with a new generation.
    session.generation += 1;
    session.active_config.as_mut().unwrap().host = "irc.other.example".into();
    assert_ne!(session.generation, captured);
}

#[gpui::test]
fn a_dialog_does_not_survive_the_connection(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        event(chat, refused("#secret", true), cx);
        assert_eq!(chat.key_prompts.len(), 1);
        chat.end_key_tracking(NET);
        assert!(chat.key_prompts.is_empty());
        assert!(chat.sessions[&NET].key_joins.is_empty());
        // The late answer of the old connection asks for nothing.
        event(chat, refused("#secret", true), cx);
        assert!(chat.key_prompts.is_empty());
    });
}

#[gpui::test]
fn a_typed_key_is_saved_only_after_our_join(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        let join = KeyJoin {
            channel: "#secret".into(),
            typed: Some(Secret::new("hunter2")),
            remember: true,
            stale: false,
        };
        chat.sessions.get_mut(&NET).unwrap().key_joins.push(join);
        assert_eq!(key_of(&store, chat), None, "sending saves nothing");
        // The key is wrong again: nothing is saved and the dialog returns.
        event(chat, refused("#secret", true), cx);
        assert_eq!(key_of(&store, chat), None);
        assert_eq!(chat.key_prompts.len(), 1);
        let join = KeyJoin {
            channel: "#secret".into(),
            typed: Some(Secret::new("hunter2")),
            remember: true,
            stale: false,
        };
        chat.sessions.get_mut(&NET).unwrap().key_joins.push(join);
        event(chat, joined("#SECRET"), cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert_eq!(key_of(&store, chat).as_deref(), Some("hunter2"));
        let config = chat.sessions[&NET].active_config.as_ref().unwrap();
        assert_eq!(config.channel_keys["#Secret"], "hunter2");
    });
}

#[gpui::test]
fn without_remembering_nothing_is_saved_and_a_stale_key_is_removed(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(Some("old"), cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        // The saved key is refused: it stays until the user decides.
        event(chat, refused("#secret", true), cx);
        assert_eq!(chat.key_prompts.len(), 1);
        assert_eq!(key_of(&store, chat).as_deref(), Some("old"));
        chat.sessions
            .get_mut(&NET)
            .unwrap()
            .key_joins
            .push(KeyJoin {
                channel: "#secret".into(),
                typed: Some(Secret::new("new")),
                remember: false,
                stale: true,
            });
        event(chat, joined("#secret"), cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert_eq!(key_of(&store, chat), None);
        let config = chat.sessions[&NET].active_config.as_ref().unwrap();
        assert!(config.channel_keys.is_empty());
    });
}

#[gpui::test]
fn a_saved_key_that_works_is_left_alone(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(Some("good"), cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        event(chat, joined("#secret"), cx);
        assert_eq!(key_of(&store, chat).as_deref(), Some("good"));
        assert!(chat.key_prompts.is_empty());
    });
}

#[gpui::test]
fn a_failing_store_is_reported_and_the_join_stands(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(None, cx);
    cx.update(|_, cx| {
        let broken = CredentialStore::with_backend(std::sync::Arc::new(
            cayenchat_storage::credentials::MemoryBackend::unavailable(
                cayenchat_storage::CredentialBackendKind::System,
            ),
        ));
        cx.set_global(crate::secrets::Credentials(broken));
    });
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        chat.sessions.get_mut(&NET).unwrap().key_joins.clear();
        chat.sessions
            .get_mut(&NET)
            .unwrap()
            .key_joins
            .push(KeyJoin {
                channel: "#secret".into(),
                typed: Some(Secret::new("hunter2")),
                remember: true,
                stale: false,
            });
        event(chat, joined("#secret"), cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        let log: Vec<&str> = chat
            .state
            .server_messages(NET)
            .iter()
            .map(|message| message.text.as_str())
            .collect();
        assert!(
            log.iter()
                .any(|line| line.contains("#secret") && line.contains("test backend"))
        );
        assert!(log.iter().all(|line| !line.contains("hunter2")));
        let notices = chat.notices.all();
        assert!(!notices.is_empty());
        assert!(notices.iter().all(|n| !n.detail.contains("hunter2")));
    });
}

#[gpui::test]
fn an_empty_or_malformed_key_keeps_the_dialog_open(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        event(chat, refused("#secret", true), cx);
    });
    chat.update_in(cx, |chat, window, cx| {
        for text in ["", "two words", "a,b"] {
            chat.key_prompts[0]
                .input()
                .update(cx, |input, cx| input.set_text(text, cx));
            chat.submit_key_prompt(NET, "#secret", window, cx);
            assert_eq!(chat.key_prompts.len(), 1, "{text:?}");
        }
        chat.cancel_key_prompt(NET, "#secret", window, cx);
        assert!(chat.key_prompts.is_empty());
        assert!(chat.sessions[&NET].key_joins.is_empty());
    });
}

#[test]
fn only_a_plain_join_of_channels_is_looked_up() {
    use crate::channel_keys::plain_join_channels;
    assert_eq!(
        plain_join_channels("/join #a,&b"),
        Some(vec!["#a".to_owned(), "&b".to_owned()])
    );
    assert_eq!(
        plain_join_channels("/ JOIN   #a "),
        Some(vec!["#a".to_owned()])
    );
    for text in [
        "/join",
        "/join #a key",
        "/join #a,",
        "/join nochannel",
        "/joinx #a",
        "/msg #a hi",
        "join #a",
    ] {
        assert_eq!(plain_join_channels(text), None, "{text}");
    }
}

#[gpui::test]
fn the_choice_not_to_remember_survives_a_refused_retry(cx: &mut TestAppContext) {
    let (chat, store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        event(chat, refused("#secret", true), cx);
        assert!(chat.key_prompts[0].remember, "a new dialog starts ON");
        chat.key_prompts[0].remember = false;
        // The dialog is submitted (and closed); the key is wrong.
        chat.key_prompts.clear();
        chat.sessions
            .get_mut(&NET)
            .unwrap()
            .key_joins
            .push(KeyJoin {
                channel: "#secret".into(),
                typed: Some(Secret::new("wrong")),
                remember: false,
                stale: false,
            });
        event(chat, refused("#secret", true), cx);
        assert!(!chat.key_prompts[0].remember);
        // The right key, with the choice carried over, saves nothing.
        chat.key_prompts.clear();
        chat.sessions
            .get_mut(&NET)
            .unwrap()
            .key_joins
            .push(KeyJoin {
                channel: "#secret".into(),
                typed: Some(Secret::new("right")),
                remember: false,
                stale: false,
            });
        event(chat, joined("#secret"), cx);
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| assert_eq!(key_of(&store, chat), None));
}

#[gpui::test]
fn names_are_matched_under_the_casemapping_announced_later(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, cx| {
        // 001 arrives under the default mapping, the first 005 changes it.
        chat.sessions
            .get_mut(&NET)
            .unwrap()
            .active_config
            .as_mut()
            .unwrap()
            .channels = vec!["#x[".into(), "#x{".into(), "#needs[".into()];
        event(chat, registered(), cx);
        event(
            chat,
            Event::CaseMapping(cayenchat_model::names::CaseMapping::Ascii),
            cx,
        );
        // `[` and `{` are different channels now; each has its own JOIN.
        assert_eq!(chat.sessions[&NET].key_joins.len(), 3);
        event(chat, refused("#x{", true), cx);
        assert_eq!(chat.key_prompts.len(), 1);
        assert_eq!(chat.sessions[&NET].key_joins.len(), 2);
        event(chat, refused("#x[", true), cx);
        assert_eq!(chat.key_prompts.len(), 2);
        // Same name under ascii: matched, with case ignored.
        event(chat, refused("#NEEDS[", true), cx);
        assert_eq!(chat.key_prompts.len(), 3);
        assert!(chat.sessions[&NET].key_joins.is_empty());
    });
}

#[gpui::test]
fn prompt_headings_use_the_display_name_without_bidi_controls(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, _| {
        let name = chat.state.networks()[0].name.clone();
        assert_eq!(chat.prompt_server_label(NET), name, "no display name");
        let profile = chat.sessions[&NET].profile_id.clone();
        chat.saved
            .servers
            .iter_mut()
            .find(|server| server.id == profile)
            .unwrap()
            .display_name = "Home\u{202e}Net".into();
        assert_eq!(chat.prompt_server_label(NET), "Home\u{200b}Net");
    });
}

#[gpui::test]
fn a_475_that_opens_the_dialog_is_not_logged_in_the_channel(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        chat.note_join(NET, "#Other", false);
        event(chat, refused("#Other", true), cx);
        assert_eq!(chat.key_prompts.len(), 1);
        assert!(chat.state.channel_id(NET, "#Other").is_none());
        assert!(chat.sessions[&NET].pending_joins.is_empty());
    });
}

#[gpui::test]
fn a_refusal_that_a_key_cannot_cure_is_still_logged(cx: &mut TestAppContext) {
    let (chat, _store, cx) = open_chat(None, cx);
    chat.update(cx, |chat, cx| {
        event(chat, registered(), cx);
        chat.note_join(NET, "#Other", false);
        event(chat, refused("#Other", false), cx);
        assert!(chat.key_prompts.is_empty());
        assert!(chat.state.channel_id(NET, "#Other").is_some());
    });
}
