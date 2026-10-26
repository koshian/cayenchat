// SPDX-License-Identifier: GPL-3.0-only
//! The credential store is read for the channels noted as keyed and no
//! others (#643).
use super::{ChatWindow, Worker, saved_connection_config, settings_file};
use crate::channel_keys::KeyJoin;
use cayenchat_irc_core::{ConnectionConfig, Event};
use cayenchat_model::NetworkId;
use cayenchat_storage::{
    CredentialBackendKind, CredentialError, CredentialStore, Language, Secret, SecretKey, Settings,
    credentials::{CredentialBackend, MemoryBackend},
};
use gpui::TestAppContext;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const NET: NetworkId = NetworkId(1);

/// A memory store that counts how often it is read.
struct Counting {
    inner: MemoryBackend,
    reads: Arc<AtomicUsize>,
}

impl CredentialBackend for Counting {
    fn kind(&self) -> CredentialBackendKind {
        self.inner.kind()
    }
    fn is_memory(&self) -> bool {
        true
    }
    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, CredentialError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(key)
    }
    fn set(&self, key: &SecretKey, value: &Secret) -> Result<(), CredentialError> {
        self.inner.set(key, value)
    }
    fn delete(&self, key: &SecretKey) -> Result<(), CredentialError> {
        self.inner.delete(key)
    }
}

fn counting_store() -> (CredentialStore, Arc<AtomicUsize>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let store = CredentialStore::with_backend(Arc::new(Counting {
        inner: MemoryBackend::new(CredentialBackendKind::System),
        reads: reads.clone(),
    }));
    (store, reads)
}

fn settings(channels: &str, noted: Option<&[&str]>) -> Settings {
    let mut settings = super::support::settings_with_channels(channels);
    let profile = settings.selected_profile_mut().unwrap();
    profile.nickname = "alice".into();
    profile.username = "ident".into();
    profile.keyed_channels =
        noted.map(|names| names.iter().map(|name| (*name).to_owned()).collect());
    settings
}

fn save_key(store: &CredentialStore, settings: &Settings, channel: &str, key: &str) {
    let id = &settings.selected_profile().unwrap().id;
    store
        .set(&SecretKey::channel_key(id, channel), &Secret::new(key))
        .unwrap();
}

fn connect(settings: &Settings, store: &CredentialStore) -> ConnectionConfig {
    saved_connection_config(
        settings.selected_profile().unwrap(),
        Language::English,
        store,
    )
    .unwrap()
}

#[test]
fn connecting_without_a_saved_key_does_not_read_the_store() {
    let (store, reads) = counting_store();
    let settings = settings("#a,#b", Some(&[]));
    // A key the note does not know of is not looked for.
    save_key(&store, &settings, "#a", "secret");
    let config = connect(&settings, &store);
    assert!(config.channel_keys.is_empty());
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
fn connecting_reads_only_the_keys_that_are_noted() {
    let (store, reads) = counting_store();
    let settings = settings("#a,#b,#c", Some(&["#B"]));
    save_key(&store, &settings, "#a", "other");
    save_key(&store, &settings, "#b", "secret");
    let config = connect(&settings, &store);
    assert_eq!(config.channel_keys.len(), 1);
    assert_eq!(config.channel_keys["#b"], "secret");
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[test]
fn a_file_without_notes_is_scanned_once_so_saved_keys_survive() {
    let (store, reads) = counting_store();
    let settings = settings("#a,#b", None);
    save_key(&store, &settings, "#b", "secret");
    let config = connect(&settings, &store);
    assert_eq!(config.channel_keys["#b"], "secret");
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

struct Opened {
    chat: gpui::Entity<ChatWindow>,
    store: CredentialStore,
    reads: Arc<AtomicUsize>,
    _file: settings_file::TestFile,
}

fn open_chat<'a>(
    settings: Settings,
    keys: &[(&str, &str)],
    cx: &'a mut TestAppContext,
) -> (Opened, &'a mut gpui::VisualTestContext) {
    let (store, reads) = counting_store();
    cx.update(|cx| {
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
        cx.set_global(crate::secrets::Credentials(store.clone()));
    });
    for (channel, key) in keys {
        save_key(&store, &settings, channel, key);
    }
    let file = settings_file::TestFile::with(&settings);
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let channels = settings.selected_profile().unwrap().channels();
    chat.update(cx, |chat, _| {
        let config = ConnectionConfig::tls("irc.example.org".into(), "me".into(), channels);
        chat.sessions.get_mut(&NET).unwrap().active_config = Some(config);
    });
    reads.store(0, Ordering::SeqCst);
    (
        Opened {
            chat,
            store,
            reads,
            _file: file,
        },
        cx,
    )
}

fn noted(chat: &ChatWindow) -> Option<Vec<String>> {
    settings_file::load()
        .unwrap()
        .unwrap()
        .profile(&chat.sessions[&NET].profile_id)
        .unwrap()
        .keyed_channels
        .clone()
}

fn registered(chat: &mut ChatWindow, cx: &mut gpui::Context<ChatWindow>) {
    chat.handle_events(
        NET,
        vec![Event::Registered {
            nickname: "me".into(),
        }],
        Worker::Running,
        cx,
    );
}

/// The settings as the settings window saves them after `change`.
fn apply(
    opened: &Opened,
    change: impl FnOnce(&mut cayenchat_storage::ServerProfile),
    cx: &mut gpui::VisualTestContext,
) {
    opened.chat.update(cx, |chat, cx| {
        let mut settings = chat.saved.clone();
        change(settings.selected_profile_mut().unwrap());
        chat.apply_servers(settings, cx);
    });
    cx.run_until_parked();
}

#[gpui::test]
fn saving_settings_does_not_read_the_store_without_a_saved_key(cx: &mut TestAppContext) {
    let (opened, cx) = open_chat(settings("#a", Some(&[])), &[("#a", "unnoted")], cx);
    apply(&opened, |p| p.quit_message = "bye".into(), cx);
    apply(&opened, |p| p.display_name = "x".into(), cx);
    assert_eq!(opened.reads.load(Ordering::SeqCst), 0);
    // A channel added to the list that has no note is not looked for either.
    apply(&opened, |p| p.channels = "#a,#b".into(), cx);
    assert_eq!(opened.reads.load(Ordering::SeqCst), 0);
}

#[gpui::test]
fn saving_settings_with_a_saved_key_reads_only_when_a_keyed_channel_is_added(
    cx: &mut TestAppContext,
) {
    let (opened, cx) = open_chat(
        settings("#a", Some(&["#b"])),
        &[("#b", "secret"), ("#a", "unnoted")],
        cx,
    );
    apply(&opened, |p| p.quit_message = "bye".into(), cx);
    apply(&opened, |p| p.channels = "#a,#c".into(), cx);
    assert_eq!(opened.reads.load(Ordering::SeqCst), 0);
    apply(&opened, |p| p.channels = "#a,#c,#B".into(), cx);
    assert_eq!(opened.reads.load(Ordering::SeqCst), 1);
    opened.chat.update(cx, |chat, _| {
        let config = chat.sessions[&NET].active_config.as_ref().unwrap();
        assert_eq!(config.channel_keys["#B"], "secret");
        assert!(!config.channel_keys.contains_key("#a"));
    });
}

#[gpui::test]
fn the_first_connection_of_an_old_file_writes_the_notes_down(cx: &mut TestAppContext) {
    let (opened, cx) = open_chat(settings("#a,#b", None), &[], cx);
    opened.chat.update(cx, |chat, cx| {
        let config = chat.sessions.get_mut(&NET).unwrap().active_config.as_mut();
        config
            .unwrap()
            .channel_keys
            .insert("#b".into(), "secret".into());
        registered(chat, cx);
        assert_eq!(noted(chat), Some(vec!["#b".to_owned()]));
        // Later registrations (reconnects) change nothing.
        registered(chat, cx);
        assert_eq!(noted(chat), Some(vec!["#b".to_owned()]));
    });
    assert_eq!(opened.reads.load(Ordering::SeqCst), 0);
}

#[gpui::test]
fn a_note_without_a_key_is_corrected_when_connecting(cx: &mut TestAppContext) {
    let (opened, cx) = open_chat(settings("#a,#b", Some(&["#a", "#b", "#gone"])), &[], cx);
    opened.chat.update(cx, |chat, cx| {
        let config = chat.sessions.get_mut(&NET).unwrap().active_config.as_mut();
        config
            .unwrap()
            .channel_keys
            .insert("#b".into(), "secret".into());
        registered(chat, cx);
        // "#gone" is not in the list, so it was not read and stays.
        assert_eq!(noted(chat), Some(vec!["#b".to_owned(), "#gone".to_owned()]));
    });
}

#[gpui::test]
fn saving_a_key_notes_it_and_forgetting_it_removes_the_note(cx: &mut TestAppContext) {
    let (opened, cx) = open_chat(settings("#a", Some(&[])), &[], cx);
    let join = |remember, stale| KeyJoin {
        channel: "#a".into(),
        typed: Some(Secret::new("hunter2")),
        remember,
        stale,
    };
    let joined = || Event::Joined {
        channel: "#a".into(),
    };
    opened.chat.update(cx, |chat, cx| {
        registered(chat, cx);
        let joins = &mut chat.sessions.get_mut(&NET).unwrap().key_joins;
        joins.clear();
        joins.push(join(true, false));
        chat.handle_events(NET, vec![joined()], Worker::Running, cx);
    });
    cx.run_until_parked();
    opened.chat.update(cx, |chat, _| {
        let id = chat.sessions[&NET].profile_id.clone();
        assert_eq!(noted(chat), Some(vec!["#a".to_owned()]));
        assert!(
            opened
                .store
                .get(&SecretKey::channel_key(&id, "#a"))
                .unwrap()
                .is_some()
        );
    });
    opened.chat.update(cx, |chat, cx| {
        chat.sessions
            .get_mut(&NET)
            .unwrap()
            .key_joins
            .push(join(false, true));
        chat.handle_events(NET, vec![joined()], Worker::Running, cx);
    });
    cx.run_until_parked();
    opened.chat.update(cx, |chat, _| {
        assert_eq!(noted(chat), Some(Vec::new()));
    });
}
