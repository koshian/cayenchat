// SPDX-License-Identifier: GPL-3.0-only
use super::{connection_config, forget_removed_profiles, startup_connections};
use cayenchat_storage::{
    CredentialBackendKind, CredentialStore, Secret, SecretKey, Settings, credentials::MemoryBackend,
};
use std::sync::Arc;

fn memory_store() -> CredentialStore {
    CredentialStore::with_backend(Arc::new(MemoryBackend::new(CredentialBackendKind::System)))
}

#[test]
fn startup_uses_only_saved_credentials() {
    let store = memory_store();
    let mut settings = Settings::default();
    settings.add_server(cayenchat_storage::PRESETS[0].host);
    settings.selected_profile_mut().unwrap().nickname = "alice".into();
    settings.selected_profile_mut().unwrap().username = "ident".into();
    assert!(startup_connections(&settings).is_empty());
    let config_of = |settings: &Settings| {
        super::saved_connection_config(
            settings.selected_profile().unwrap(),
            settings.language,
            &store,
        )
    };

    let profile = settings.selected_profile_mut().unwrap();
    profile.connect_on_startup = true;
    profile.sasl_enabled = true;
    profile.sasl_username = "account".into();
    profile.use_tls = true;
    // No saved SASL password: the connection is asked for it, not refused.
    let (config, missing) = config_of(&settings).unwrap();
    assert!(missing.sasl && !missing.server);
    assert!(config.server_password.is_none());
    assert_eq!(config.sasl.unwrap().password, "");

    // Stored but not noted as saved: the store is not read.
    let profile = settings.selected_profile().unwrap().clone();
    store
        .set(&profile.sasl_password_key(), &Secret::new("secret"))
        .unwrap();
    store
        .set(
            &profile.server_password_key(),
            &Secret::new("server-secret"),
        )
        .unwrap();
    let (_, missing) = config_of(&settings).unwrap();
    assert!(missing.sasl, "not read without the note");
    let profile = settings.selected_profile_mut().unwrap();
    profile.set_server_password_saved(true);
    profile.set_sasl_password_saved(true);
    assert_eq!(
        startup_connections(&settings),
        [settings.selected_server.clone()]
    );
    let (config, missing) = config_of(&settings).unwrap();
    assert!(!missing.any());
    assert_eq!(config.host, "irc.ircnet.com");
    assert_eq!(config.nickname, "alice");
    assert_eq!(config.username, "ident");
    assert_eq!(config.server_password.as_deref(), Some("server-secret"));
    let sasl = config.sasl.unwrap();
    assert_eq!(sasl.username, "account");
    assert_eq!(sasl.password, "secret");
}

#[test]
fn every_server_marked_for_startup_connects_with_its_own_identity() {
    let mut settings = Settings::default();
    settings.add_server("");
    let custom = settings.selected_profile_mut().unwrap();
    custom.host = "irc.example.org".into();
    custom.nickname = "bob".into();
    custom.username = "bob".into();
    custom.channels = "#b".into();
    custom.connect_on_startup = true;
    let preset = settings.add_server(cayenchat_storage::PRESETS[0].host);
    preset.nickname = "alice".into();
    preset.username = "alice".into();
    preset.channels = "#a".into();
    preset.connect_on_startup = true;
    let store = memory_store();
    // Tree order: the order the servers were added.
    let hosts: Vec<_> = startup_connections(&settings)
        .iter()
        .map(|id| {
            let profile = settings.profile(id).unwrap();
            let (config, _) =
                super::saved_connection_config(profile, settings.language, &store).unwrap();
            (config.host, config.nickname, config.channels)
        })
        .collect();
    assert_eq!(
        hosts,
        [
            (
                "irc.example.org".to_owned(),
                "bob".to_owned(),
                vec!["#b".to_owned()]
            ),
            (
                "irc.ircnet.com".to_owned(),
                "alice".to_owned(),
                vec!["#a".to_owned()]
            ),
        ]
    );
}

#[test]
fn irc_metadata_keeps_a_usable_account_only() {
    let meta =
        |account| super::irc_message_meta(None, None, account, cayenchat_model::Provenance::Live);
    assert_eq!(meta(Some("alice")).account.unwrap().as_str(), "alice");
    assert!(meta(Some("*")).account.is_none());
    assert!(meta(Some("")).account.is_none());
    assert!(meta(None).account.is_none());
    // The same account text on two networks is just two messages.
    assert_eq!(meta(Some("alice")).account, meta(Some("alice")).account);
}

#[test]
fn the_saved_realname_is_used_for_every_connection() {
    let mut settings = Settings::default();
    settings.add_server(cayenchat_storage::PRESETS[0].host);
    let profile = settings.selected_profile_mut().unwrap();
    profile.nickname = "alice".into();
    profile.username = "ident".into();
    let language = settings.language;
    // Unset keeps the built-in default on the wire.
    let config =
        connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
    assert_eq!(config.realname, "");
    assert_eq!(config.username, "ident");
    settings.selected_profile_mut().unwrap().realname = "Alice Liddell".into();
    // A reconnect builds its configuration from the persisted value.
    let config =
        connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
    assert_eq!(config.realname, "Alice Liddell");
    assert_eq!(config.nickname, "alice");
    assert_eq!(config.username, "ident");
    settings.selected_profile_mut().unwrap().realname = "two\nlines".into();
    assert!(connection_config(settings.selected_profile().unwrap(), language, None, None).is_err());
}

#[test]
fn each_server_sends_its_own_quit_message() {
    let mut settings = Settings::default();
    settings.add_server(cayenchat_storage::PRESETS[0].host);
    let profile = settings.selected_profile_mut().unwrap();
    profile.nickname = "alice".into();
    profile.username = "ident".into();
    let language = settings.language;
    let config =
        connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
    assert_eq!(config.quit_message, "");
    settings.selected_profile_mut().unwrap().quit_message = "Back soon".into();
    let config =
        connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
    assert_eq!(config.quit_message, "Back soon");
}

#[test]
fn each_server_sends_its_own_part_message() {
    let mut settings = Settings::default();
    settings.add_server(cayenchat_storage::PRESETS[0].host);
    let profile = settings.selected_profile_mut().unwrap();
    profile.nickname = "alice".into();
    profile.username = "ident".into();
    let language = settings.language;
    let config =
        connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
    assert_eq!(config.part_message, "");
    settings.selected_profile_mut().unwrap().part_message = "またね".into();
    let config =
        connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
    assert_eq!(config.part_message, "またね");
    assert_eq!(config.quit_message, "");
}

#[test]
fn nickname_and_username_stay_independent_without_credentials() {
    let mut settings = Settings::default();
    settings.add_server(cayenchat_storage::PRESETS[0].host);
    settings.selected_profile_mut().unwrap().nickname = "alice".into();
    settings.selected_profile_mut().unwrap().username = "someone".into();
    let language = settings.language;
    let config =
        connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
    assert_eq!(config.nickname, "alice");
    assert_eq!(config.username, "someone");
    assert!(config.server_password.is_none());
    assert!(config.sasl.is_none());
    settings.selected_profile_mut().unwrap().username.clear();
    assert!(connection_config(settings.selected_profile().unwrap(), language, None, None).is_err());
}

#[test]
fn plaintext_server_password_needs_the_profile_opt_in() {
    let mut settings = Settings::default();
    let profile = settings.add_server("znc.lan");
    profile.nickname = "alice".into();
    profile.username = "alice".into();
    let language = settings.language;
    let password = || Some(Secret::new("alice/net:secret"));
    let profile = settings.selected_profile().unwrap();
    assert!(!profile.use_tls);
    assert!(connection_config(profile, language, password(), None).is_err());
    settings
        .selected_profile_mut()
        .unwrap()
        .allow_plaintext_pass = true;
    let config = connection_config(
        settings.selected_profile().unwrap(),
        language,
        password(),
        None,
    )
    .unwrap();
    assert!(!config.use_tls && config.allow_plaintext_pass);
    assert_eq!(config.server_password.as_deref(), Some("alice/net:secret"));
}

#[test]
fn replacing_a_profile_before_saving_never_inherits_its_passwords() {
    // Cover both legacy sequential IDs and IDs assigned to new profiles.
    for legacy in [true, false] {
        let store = memory_store();
        let mut previous = Settings::default();
        previous.add_server("");
        previous.selected_profile_mut().unwrap().host = "old.example.org".into();
        if legacy {
            previous.selected_profile_mut().unwrap().id = "custom-1".into();
            previous.selected_server = "custom-1".into();
        }
        previous
            .selected_profile_mut()
            .unwrap()
            .set_server_password_saved(true);
        previous
            .selected_profile_mut()
            .unwrap()
            .set_sasl_password_saved(true);
        let removed = previous.selected_profile().unwrap().clone();
        for key in [removed.server_password_key(), removed.sasl_password_key()] {
            store.set(&key, &Secret::new("old-password")).unwrap();
        }

        let mut next = previous.clone();
        next.remove_selected_server();
        next.add_server("");
        next.selected_profile_mut().unwrap().host = "new.example.org".into();
        next.selected_profile_mut().unwrap().sasl_enabled = true;

        // Connecting reads saved credentials before committing settings.
        let i18n = super::Localizer::new(next.language);
        let (server, sasl) =
            super::saved_connection_secrets(next.selected_profile().unwrap(), &store, &i18n)
                .unwrap();
        assert!(server.is_none());
        assert!(sasl.is_none());
        assert_ne!(next.selected_profile().unwrap().id, removed.id);

        forget_removed_profiles(&previous, &next, &store).unwrap();
        assert!(!store.contains(&removed.server_password_key()).unwrap());
        assert!(!store.contains(&removed.sasl_password_key()).unwrap());
    }
}

#[test]
fn removed_profiles_lose_their_saved_passwords() {
    let store = memory_store();
    let mut previous = Settings::default();
    previous.add_server("");
    previous.selected_profile_mut().unwrap().host = "irc.example.org".into();
    previous
        .selected_profile_mut()
        .unwrap()
        .set_server_password_saved(true);
    let removed = previous.selected_profile().unwrap().clone();
    let kept = SecretKey::server_password(cayenchat_storage::IRCNET_ID);
    store
        .set(&removed.server_password_key(), &Secret::new("x"))
        .unwrap();
    store.set(&kept, &Secret::new("y")).unwrap();
    let mut next = previous.clone();
    next.remove_selected_server();
    forget_removed_profiles(&previous, &next, &store).unwrap();
    assert!(store.get(&removed.server_password_key()).unwrap().is_none());
    assert!(store.get(&kept).unwrap().is_some());
}

/// Settings with a kept profile and a second, removable one, both holding a
/// key for `#shared`. Returns them with the removed profile's id and the kept
/// profile's key.
fn removable_profile_with_keys(
    store: &CredentialStore,
    removed_notes: Option<&[&str]>,
) -> (Settings, Settings, String, SecretKey) {
    let mut previous = Settings::default();
    previous.add_server("kept.example.org");
    let kept_id = previous.selected_profile().unwrap().id.clone();
    previous.add_server("irc.example.org");
    let removed = previous.selected_profile_mut().unwrap();
    removed.channels = "#auto,-#off".into();
    removed.keyed_channels = removed_notes.map(|notes| notes.iter().map(|n| (*n).into()).collect());
    let removed_id = removed.id.clone();
    let kept = previous.profile_mut(&kept_id).unwrap();
    kept.keyed_channels = Some(vec!["#shared".into()]);
    let kept_key = SecretKey::channel_key(&kept_id, "#shared");
    store.set(&kept_key, &Secret::new("kept")).unwrap();
    let mut next = previous.clone();
    next.remove_selected_server();
    (previous, next, removed_id, kept_key)
}

#[test]
fn removed_profiles_lose_every_noted_channel_key_but_not_other_profiles() {
    let store = memory_store();
    let (previous, next, removed_id, kept_key) =
        removable_profile_with_keys(&store, Some(&["#shared", "#Other"]));
    let keys = ["#shared", "#other", "#auto"].map(|c| SecretKey::channel_key(&removed_id, c));
    for key in &keys {
        store.set(key, &Secret::new("k")).unwrap();
    }
    forget_removed_profiles(&previous, &next, &store).unwrap();
    // Noted channels go (case-insensitively); an un-noted one is out of scope.
    assert!(store.get(&keys[0]).unwrap().is_none());
    assert!(store.get(&keys[1]).unwrap().is_none());
    assert!(store.get(&keys[2]).unwrap().is_some());
    // The kept profile's key for the same channel name stays.
    assert_eq!(store.get(&kept_key).unwrap().unwrap().expose(), "kept");
}

#[test]
fn removed_profiles_from_old_settings_lose_the_auto_join_channel_keys() {
    let store = memory_store();
    let (previous, next, removed_id, kept_key) = removable_profile_with_keys(&store, None);
    let [auto, off] = ["#auto", "#off"].map(|c| SecretKey::channel_key(&removed_id, c));
    let shared = SecretKey::channel_key(&removed_id, "#shared");
    for key in [&auto, &off, &shared] {
        store.set(key, &Secret::new("k")).unwrap();
    }
    forget_removed_profiles(&previous, &next, &store).unwrap();
    // The first scan would read only the enabled auto-join channels.
    assert!(store.get(&auto).unwrap().is_none());
    assert!(store.get(&off).unwrap().is_some());
    assert!(store.get(&shared).unwrap().is_some());
    assert!(store.get(&kept_key).unwrap().is_some());
}

#[test]
fn saving_settings_keeps_the_saved_passwords_of_a_kept_server() {
    let store = memory_store();
    let mut previous = Settings::default();
    previous.add_server("");
    previous.selected_profile_mut().unwrap().host = "irc.example.org".into();
    let profile = previous.selected_profile().unwrap().clone();
    store
        .set(&profile.server_password_key(), &Secret::new("x"))
        .unwrap();
    // Neither an unchanged save nor an edited, kept server touches them.
    forget_removed_profiles(&previous, &previous, &store).unwrap();
    let mut next = previous.clone();
    next.selected_profile_mut().unwrap().nickname = "other".into();
    forget_removed_profiles(&previous, &next, &store).unwrap();
    assert!(store.get(&profile.server_password_key()).unwrap().is_some());
}

#[test]
fn a_failed_deletion_is_reported_so_save_can_retry() {
    let unavailable = CredentialStore::with_backend(Arc::new(MemoryBackend::unavailable(
        CredentialBackendKind::System,
    )));
    let mut previous = Settings::default();
    previous.add_server("irc.example.org");
    previous
        .selected_profile_mut()
        .unwrap()
        .set_server_password_saved(true);
    let mut next = previous.clone();
    next.remove_selected_server();
    assert!(forget_removed_profiles(&previous, &next, &unavailable).is_err());
    // Nothing to forget once nothing changed.
    assert!(forget_removed_profiles(&previous, &previous, &unavailable).is_ok());
}

/// A store that counts every read and delete, which is what can make the
/// operating system ask the user for permission.
struct CountingBackend {
    inner: MemoryBackend,
    touches: std::sync::atomic::AtomicUsize,
}

impl CountingBackend {
    fn touch(&self) {
        self.touches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl cayenchat_storage::credentials::CredentialBackend for CountingBackend {
    fn kind(&self) -> CredentialBackendKind {
        self.inner.kind()
    }

    fn is_memory(&self) -> bool {
        true
    }

    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, cayenchat_storage::CredentialError> {
        self.touch();
        self.inner.get(key)
    }

    fn set(
        &self,
        key: &SecretKey,
        value: &Secret,
    ) -> Result<(), cayenchat_storage::CredentialError> {
        self.inner.set(key, value)
    }

    fn delete(&self, key: &SecretKey) -> Result<(), cayenchat_storage::CredentialError> {
        self.touch();
        self.inner.delete(key)
    }
}

#[test]
fn the_store_is_touched_only_for_passwords_noted_as_saved() {
    let backend = Arc::new(CountingBackend {
        inner: MemoryBackend::new(CredentialBackendKind::System),
        touches: Default::default(),
    });
    let store = CredentialStore::with_backend(backend.clone());
    let touches = || backend.touches.load(std::sync::atomic::Ordering::SeqCst);
    let mut previous = Settings::default();
    previous.add_server("irc.example.org");
    let profile = previous.selected_profile_mut().unwrap();
    profile.nickname = "alice".into();
    profile.username = "ident".into();
    profile.sasl_enabled = true;
    profile.sasl_username = "account".into();
    profile.use_tls = true;
    profile.connect_on_startup = true;
    let profile = previous.selected_profile().unwrap().clone();
    for key in [profile.server_password_key(), profile.sasl_password_key()] {
        store.set(&key, &Secret::new("stored")).unwrap();
    }
    let i18n = super::Localizer::new(previous.language);

    // Nothing noted: connecting at startup reads nothing, and asks for SASL.
    let (_, missing) = super::saved_connection_config(&profile, previous.language, &store).unwrap();
    assert!(missing.sasl);
    let (server, sasl) = super::saved_connection_secrets(&profile, &store, &i18n).unwrap();
    assert!(server.is_none() && sasl.is_none());
    // Neither does removing the server delete anything.
    let mut next = previous.clone();
    next.remove_selected_server();
    forget_removed_profiles(&previous, &next, &store).unwrap();
    assert_eq!(touches(), 0);

    // Noted: the password is read and used, and deleted with the server.
    let mut noted = profile.clone();
    noted.set_server_password_saved(true);
    noted.set_sasl_password_saved(true);
    let (server, sasl) = super::saved_connection_secrets(&noted, &store, &i18n).unwrap();
    assert!(server.is_some() && sasl.is_some());
    assert_eq!(touches(), 2);
    let mut previous = previous.clone();
    *previous.selected_profile_mut().unwrap() = noted;
    forget_removed_profiles(&previous, &next, &store).unwrap();
    assert_eq!(touches(), 4);
}

fn ecdsa_profile(settings: &mut Settings) -> cayenchat_storage::ServerProfile {
    settings.add_server("irc.example.org");
    let profile = settings.selected_profile_mut().unwrap();
    profile.nickname = "alice".into();
    profile.username = "ident".into();
    profile.use_tls = true;
    profile.sasl_enabled = true;
    profile.sasl_username = "account".into();
    profile.sasl_mechanism = cayenchat_storage::SaslMechanism::EcdsaNist256pChallenge;
    profile.clone()
}

#[test]
fn the_sasl_key_is_read_only_when_noted_and_a_bad_one_counts_as_missing() {
    let backend = Arc::new(CountingBackend {
        inner: MemoryBackend::new(CredentialBackendKind::System),
        touches: Default::default(),
    });
    let store = CredentialStore::with_backend(backend.clone());
    let touches = || backend.touches.load(std::sync::atomic::Ordering::SeqCst);
    let mut settings = Settings::default();
    let mut profile = ecdsa_profile(&mut settings);
    let key = cayenchat_irc_core::generate_sasl_key().unwrap();
    store
        .set(&profile.sasl_key_key(), &Secret::new(key.as_str()))
        .unwrap();

    // Not noted: the store is not read, and the connection is stopped
    // before it starts, not sent to the password dialog.
    let (_, missing, _) =
        super::saved_connection_config_read(&profile, settings.language, &store).unwrap();
    assert!(missing.key && !missing.sasl && !missing.any());
    assert_eq!(touches(), 0);

    // Noted: the key is read and put into the configuration.
    profile.sasl_key_saved = true;
    let (config, missing, _) =
        super::saved_connection_config_read(&profile, settings.language, &store).unwrap();
    assert!(!missing.sasl_secret());
    assert_eq!(touches(), 1);
    let sasl = config.sasl.unwrap();
    assert_eq!(
        sasl.mechanism,
        cayenchat_irc_core::SaslMechanism::EcdsaNist256pChallenge
    );
    assert_eq!(sasl.password, key.as_str());

    // A stored value that is not a key is as good as none.
    store
        .set(&profile.sasl_key_key(), &Secret::new("not a key"))
        .unwrap();
    let (config, missing, _) =
        super::saved_connection_config_read(&profile, settings.language, &store).unwrap();
    assert!(missing.key);
    assert!(config.sasl.unwrap().password.is_empty());

    // The password's own note does not make the key readable.
    profile.sasl_key_saved = false;
    profile.set_sasl_password_saved(true);
    let before = touches();
    let (_, missing, _) =
        super::saved_connection_config_read(&profile, settings.language, &store).unwrap();
    assert!(missing.key);
    assert_eq!(touches(), before);
}

#[test]
fn removing_a_server_deletes_its_sasl_key_but_only_when_noted() {
    let store = memory_store();
    let mut previous = Settings::default();
    let mut profile = ecdsa_profile(&mut previous);
    let key = cayenchat_irc_core::generate_sasl_key().unwrap();
    store
        .set(&profile.sasl_key_key(), &Secret::new(key.as_str()))
        .unwrap();
    let mut next = previous.clone();
    next.remove_selected_server();
    // No note, no deletion.
    forget_removed_profiles(&previous, &next, &store).unwrap();
    assert!(store.contains(&profile.sasl_key_key()).unwrap());
    profile.sasl_key_saved = true;
    *previous.selected_profile_mut().unwrap() = profile.clone();
    forget_removed_profiles(&previous, &next, &store).unwrap();
    assert!(!store.contains(&profile.sasl_key_key()).unwrap());
}

#[test]
fn the_private_key_never_reaches_the_settings_file_or_debug_output() {
    let mut settings = Settings::default();
    let mut profile = ecdsa_profile(&mut settings);
    profile.sasl_key_saved = true;
    *settings.selected_profile_mut().unwrap() = profile.clone();
    let key = cayenchat_irc_core::generate_sasl_key().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    cayenchat_storage::save_to(&path, &settings).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"sasl_key_saved\": true"), "{text}");
    assert!(!text.contains(key.as_str()));
    // The configuration's debug output shows no key either.
    let mut config = super::connection_config_without_secrets(&profile, settings.language).unwrap();
    super::connect_auth::apply_secrets(&mut config, None, Some(&Secret::new(key.as_str())));
    assert!(!format!("{config:?}").contains(key.as_str()));
}
