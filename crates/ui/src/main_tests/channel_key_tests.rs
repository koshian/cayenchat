// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use crate::channel_keys::KeyJoin;
use cayenchat_irc_core::{ChannelActivityKind, ConnectionConfig, Event};
use cayenchat_model::NetworkId;
use cayenchat_storage::{CredentialStore, Secret, SecretKey};
use gpui::TestAppContext;

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
