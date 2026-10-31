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
    .0
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

/// The notes this window holds.
fn noted(chat: &ChatWindow) -> Option<Vec<String>> {
    chat.saved
        .profile(&chat.sessions[&NET].profile_id)
        .unwrap()
        .keyed_channels
        .clone()
}

/// The notes in the settings file.
fn noted_on_file(chat: &ChatWindow) -> Option<Vec<String>> {
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

#[gpui::test]
fn renaming_a_channel_only_in_case_keeps_its_key_without_reading(cx: &mut TestAppContext) {
    let (opened, cx) = open_chat(settings("#a", Some(&["#a"])), &[("#a", "secret")], cx);
    opened.chat.update(cx, |chat, _| {
        let config = chat.sessions.get_mut(&NET).unwrap().active_config.as_mut();
        config
            .unwrap()
            .channel_keys
            .insert("#a".into(), "secret".into());
    });
    apply(&opened, |p| p.channels = "#A".into(), cx);
    assert_eq!(opened.reads.load(Ordering::SeqCst), 0);
    opened.chat.update(cx, |chat, _| {
        let config = chat.sessions[&NET].active_config.as_ref().unwrap();
        assert_eq!(config.channel_keys.len(), 1);
        assert_eq!(config.channel_keys["#A"], "secret");
    });
}

#[test]
fn a_join_reads_the_store_only_for_a_noted_channel() {
    let (store, reads) = counting_store();
    let settings = settings("#a", Some(&["#a"]));
    save_key(&store, &settings, "#a", "secret");
    save_key(&store, &settings, "#b", "unnoted");
    reads.store(0, Ordering::SeqCst);
    let id = &settings.selected_profile().unwrap().id;
    let found = crate::channel_keys::join_keys(&store, id, vec![("#b".into(), false)]);
    assert!(found[0].1.is_none());
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let found = crate::channel_keys::join_keys(&store, id, vec![("#a".into(), true)]);
    assert_eq!(found[0].1.as_ref().map(|k| k.expose()), Some("secret"));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn a_corrected_note_still_lets_the_connection_use_the_key(cx: &mut TestAppContext) {
    // The key is saved but the file has no note of it.
    let (opened, cx) = open_chat(settings("#a,#b", None), &[("#b", "secret")], cx);
    opened.chat.update(cx, |chat, cx| {
        let first = connect(&chat.saved, &opened.store);
        assert_eq!(first.channel_keys["#b"], "secret");
        chat.sessions
            .get_mut(&NET)
            .unwrap()
            .active_config
            .as_mut()
            .unwrap()
            .channel_keys
            .extend(first.channel_keys);
        registered(chat, cx);
        assert_eq!(noted(chat), Some(vec!["#b".to_owned()]));
        // The reconnect, which reads the corrected note, finds the same key.
        let again = connect(&chat.saved, &opened.store);
        assert_eq!(again.channel_keys.len(), 1);
        assert_eq!(again.channel_keys["#b"], "secret");
    });
}

#[gpui::test]
fn reading_the_store_does_not_save_the_settings_file(cx: &mut TestAppContext) {
    let (opened, cx) = open_chat(
        settings("#a,#b", Some(&["#a", "#b"])),
        &[("#b", "secret")],
        cx,
    );
    let before = settings_file::load().unwrap().unwrap();
    opened.chat.update(cx, |chat, cx| {
        let config = chat.sessions.get_mut(&NET).unwrap().active_config.as_mut();
        config
            .unwrap()
            .channel_keys
            .insert("#b".into(), "secret".into());
        registered(chat, cx);
        // Corrected here, not in the file.
        assert_eq!(noted(chat), Some(vec!["#b".to_owned()]));
        assert_eq!(
            noted_on_file(chat),
            Some(vec!["#a".to_owned(), "#b".to_owned()])
        );
    });
    cx.run_until_parked();
    assert_eq!(settings_file::load().unwrap().unwrap(), before);
}

#[gpui::test]
fn corrected_notes_are_written_with_the_next_settings_save(cx: &mut TestAppContext) {
    let (opened, cx) = open_chat(
        settings("#a,#b", Some(&["#a", "#b"])),
        &[("#b", "secret")],
        cx,
    );
    opened.chat.update(cx, |chat, cx| {
        let config = chat.sessions.get_mut(&NET).unwrap().active_config.as_mut();
        config
            .unwrap()
            .channel_keys
            .insert("#b".into(), "secret".into());
        registered(chat, cx);
        // What the settings window does when it saves.
        let mut file = settings_file::load().unwrap().unwrap();
        chat.overlay_pending_notes(&mut file);
        settings_file::save(&file).unwrap();
        chat.notes_written();
        assert_eq!(noted_on_file(chat), Some(vec!["#b".to_owned()]));
        assert!(chat.pending_notes.is_empty());
    });
}

#[gpui::test]
fn notes_converge_on_the_first_scan_after_a_restart(cx: &mut TestAppContext) {
    // The corrections of the last run never reached the file: it still has
    // no notes, and the key is saved.
    let (opened, cx) = open_chat(settings("#a,#b", None), &[("#b", "secret")], cx);
    let restarted = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings_file::load().unwrap().unwrap(), None, window, cx)
    });
    let (restarted, cx) = restarted;
    restarted.update(cx, |chat, _| {
        let profile = chat.saved.selected_profile().unwrap();
        assert!(profile.channel_key_scan_pending());
        let config = connect(&chat.saved, &opened.store);
        assert_eq!(config.channel_keys["#b"], "secret");
    });
    restarted.update(cx, |chat, cx| {
        let config = ConnectionConfig::tls(
            "irc.example.org".into(),
            "me".into(),
            "#a,#b".split(',').map(str::to_owned).collect(),
        );
        chat.sessions.get_mut(&NET).unwrap().active_config = Some(config);
        chat.sessions
            .get_mut(&NET)
            .unwrap()
            .active_config
            .as_mut()
            .unwrap()
            .channel_keys
            .insert("#b".into(), "secret".into());
        registered(chat, cx);
        assert_eq!(noted(chat), Some(vec!["#b".to_owned()]));
    });
}
