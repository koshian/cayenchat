// SPDX-License-Identifier: GPL-3.0-only
//! When the credential store is first read: not before the window is drawn,
//! and for a server that does not connect at startup, only when the user
//! connects it (#648).
use super::ChatWindow;
use cayenchat_app::ConnectionStatus;
use cayenchat_model::NetworkId;
use cayenchat_storage::{Secret, Settings};
use gpui::TestAppContext;

const NET: NetworkId = NetworkId(1);

/// One server whose password and SASL password are saved and noted, aimed at
/// a port nobody listens on.
fn settings(connect_on_startup: bool) -> Settings {
    let mut settings = crate::settings_with_channels("#a");
    let profile = &mut settings.servers[0];
    profile.host = "127.0.0.1".into();
    profile.port = 1;
    profile.use_tls = true;
    profile.nickname = "me".into();
    profile.username = "me".into();
    profile.sasl_enabled = true;
    profile.sasl_username = "acct".into();
    profile.connect_on_startup = connect_on_startup;
    profile.set_server_password_saved(true);
    profile.set_sasl_password_saved(true);
    profile.note_channel_key("#a", true);
    settings
}

fn open_chat(
    cx: &mut TestAppContext,
    settings: Settings,
) -> (
    gpui::Entity<ChatWindow>,
    std::sync::Arc<crate::secrets::PasswordWatch>,
    &mut gpui::VisualTestContext,
) {
    let watch = cx.update(|cx| {
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
        let (watch, store) = crate::secrets::PasswordWatch::install(cx);
        let profile = &settings.servers[0];
        store
            .set(&profile.server_password_key(), &Secret::new("server"))
            .unwrap();
        store
            .set(&profile.sasl_password_key(), &Secret::new("sasl"))
            .unwrap();
        watch
    });
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    (chat, watch, cx)
}

fn stop(chat: &mut ChatWindow) {
    if let Some(connection) = chat.sessions.get_mut(&NET).and_then(|s| s.irc.take()) {
        let _ = connection.disconnect();
    }
}

fn log_has(chat: &ChatWindow, text: &str) -> bool {
    chat.state
        .server_messages(NET)
        .iter()
        .any(|message| message.text == text)
}

#[gpui::test]
fn without_a_startup_server_the_store_is_not_read(cx: &mut TestAppContext) {
    let (chat, watch, cx) = open_chat(cx, settings(false));
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| assert!(chat.startup_connections.is_empty()));
    assert_eq!(watch.touches() + watch.key_touches(), 0);

    // The first read is the Connect of the user.
    chat.update_in(cx, |chat, window, cx| chat.reconnect(NET, window, cx));
    cx.run_until_parked();
    cx.run_until_parked();
    assert_eq!(watch.touches(), 2, "the server and the SASL password");
    assert!(watch.key_touches() > 0);
    chat.update(cx, |chat, _| stop(chat));
}

#[gpui::test]
fn a_startup_server_is_read_after_the_window_is_drawn(cx: &mut TestAppContext) {
    let (chat, watch, cx) = open_chat(cx, settings(true));
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.startup_connections, [NET], "waiting for the start");
    });
    assert_eq!(watch.touches() + watch.key_touches(), 0, "not in new");

    chat.update_in(cx, |chat, window, cx| {
        chat.connect_at_startup(window, cx);
        let connecting = chat.i18n.text("event_connecting");
        assert!(log_has(chat, &connecting), "the log says it is starting");
    });
    chat.read_with(cx, |chat, _| {
        // Visible although a channel may be selected: status line and tree.
        assert_eq!(chat.state.status(NET), Some(&ConnectionStatus::Connecting));
        assert!(chat.sessions[&NET].starting);
    });
    // The test platform draws no frames, and nothing else reads: not even
    // after a long wait.
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(watch.touches() + watch.key_touches(), 0, "before a frame");

    // What the frame after the first one runs.
    chat.update_in(cx, |chat, window, cx| {
        chat.connect_startup_servers(window, cx)
    });
    cx.run_until_parked();
    assert_eq!(watch.touches(), 2, "the server and the SASL password");
    assert!(watch.key_touches() > 0);
    chat.update(cx, |chat, _| {
        assert!(chat.credential_prompts.is_empty(), "nothing was missing");
        stop(chat);
    });
}

#[gpui::test]
fn a_server_connected_by_hand_before_the_start_is_left_alone(cx: &mut TestAppContext) {
    let (chat, watch, cx) = open_chat(cx, settings(true));
    chat.update_in(cx, |chat, window, cx| {
        chat.connect_at_startup(window, cx);
        chat.reconnect(NET, window, cx);
    });
    cx.run_until_parked();
    let by_hand = watch.touches();
    assert_eq!(by_hand, 2);
    chat.update_in(cx, |chat, window, cx| {
        chat.connect_startup_servers(window, cx)
    });
    cx.run_until_parked();
    assert_eq!(watch.touches(), by_hand, "not read a second time");
    chat.update(cx, |chat, _| stop(chat));
}

#[gpui::test]
fn an_unreadable_store_asks_and_cancelling_ends_the_start(cx: &mut TestAppContext) {
    let (chat, watch, cx) = open_chat(cx, settings(true));
    watch.refuse_reads();
    chat.update_in(cx, |chat, window, cx| {
        chat.connect_at_startup(window, cx);
        chat.connect_startup_servers(window, cx);
    });
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.credential_prompts.len(), 1, "asked, not an error");
        assert!(chat.settings_window.is_none());
        assert!(chat.sessions[&NET].irc.is_none(), "not connected yet");
        let profile = &chat.saved.profile(&chat.sessions[&NET].profile_id).unwrap();
        assert!(profile.sasl_password_saved(), "the note is left alone");
    });
    chat.update_in(cx, |chat, window, cx| {
        chat.cancel_credential_prompt(NET, window, cx)
    });
    chat.read_with(cx, |chat, _| {
        assert!(matches!(
            chat.state.status(NET),
            Some(ConnectionStatus::Disconnected(_))
        ));
        assert!(chat.sessions[&NET].irc.is_none());
    });
}

/// A store that answers a read only when the test lets it, like a system
/// store waiting for the user to allow it (#640).
struct GatedBackend {
    inner: cayenchat_storage::credentials::MemoryBackend,
    gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    reads: std::sync::atomic::AtomicUsize,
}

impl GatedBackend {
    /// Installs a gated store with the store thread of a real run (not the
    /// immediate one of tests); `release` lets one read go.
    fn install(
        cx: &mut gpui::App,
    ) -> (
        std::sync::mpsc::Sender<()>,
        std::sync::Arc<Self>,
        cayenchat_storage::CredentialStore,
    ) {
        let (release, gate) = std::sync::mpsc::channel();
        let backend = std::sync::Arc::new(Self {
            inner: cayenchat_storage::credentials::MemoryBackend::new(
                cayenchat_storage::CredentialBackendKind::System,
            ),
            gate: std::sync::Mutex::new(gate),
            reads: Default::default(),
        });
        let store = cayenchat_storage::CredentialStore::with_backend(backend.clone());
        cx.set_global(crate::secrets::Credentials(store.clone()));
        cx.set_global(crate::secrets::Worker::start(store.clone()));
        (release, backend, store)
    }
}

impl cayenchat_storage::credentials::CredentialBackend for GatedBackend {
    fn kind(&self) -> cayenchat_storage::CredentialBackendKind {
        self.inner.kind()
    }

    fn is_memory(&self) -> bool {
        true
    }

    fn get(
        &self,
        key: &cayenchat_storage::SecretKey,
    ) -> Result<Option<Secret>, cayenchat_storage::CredentialError> {
        // A read on the UI thread would wait here and fail the test.
        self.gate
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the test lets the read go");
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.get(key)
    }

    fn set(
        &self,
        key: &cayenchat_storage::SecretKey,
        value: &Secret,
    ) -> Result<(), cayenchat_storage::CredentialError> {
        self.inner.set(key, value)
    }

    fn delete(
        &self,
        key: &cayenchat_storage::SecretKey,
    ) -> Result<(), cayenchat_storage::CredentialError> {
        self.inner.delete(key)
    }
}

fn set_theme(cx: &mut gpui::App) {
    cx.set_global(crate::theme::Theme::new(
        cayenchat_storage::ThemeMode::Light,
        gpui::WindowAppearance::Light,
        &cayenchat_storage::Appearance::default(),
    ));
}

#[gpui::test]
fn a_slow_store_does_not_stop_the_window(cx: &mut TestAppContext) {
    let release = cx.update(|cx| {
        set_theme(cx);
        GatedBackend::install(cx).0
    });
    let (chat, cx) = cx
        .add_window_view(|window, cx| ChatWindow::with_settings(settings(false), None, window, cx));
    cx.executor().allow_parking();

    // Connect reads two passwords and a channel key, and the store waits
    // for the user. The
    // call returns, and the window goes on: menus, tabs and windows work.
    chat.update_in(cx, |chat, window, cx| chat.reconnect(NET, window, cx));
    chat.update_in(cx, |chat, window, cx| {
        assert!(chat.sessions[&NET].reading_saved);
        assert!(chat.credential_prompts.is_empty(), "nothing decided yet");
        chat.open_settings_for(None, window, cx);
    });
    chat.read_with(cx, |chat, _| {
        assert!(chat.settings_window.is_some(), "a window opens meanwhile");
        assert!(chat.sessions[&NET].irc.is_none(), "not connected yet");
    });

    // The user allows it: the connection goes on by itself.
    for _ in 0..3 {
        release.send(()).unwrap();
    }
    let started = std::time::Instant::now();
    while chat.read_with(cx, |chat, _| chat.sessions[&NET].reading_saved) {
        assert!(started.elapsed().as_secs() < 20, "the read finished");
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.run_until_parked();
    }
    chat.update(cx, |chat, _| stop(chat));
}

/// What was read waits in the queue while the profile is edited (another
/// authentication mechanism, say): it does not fit the profile any more, so
/// it is read again instead of being applied to it.
#[gpui::test]
fn a_read_for_a_changed_profile_is_read_again(cx: &mut TestAppContext) {
    let (release, backend) = cx.update(|cx| {
        set_theme(cx);
        let (release, backend, _) = GatedBackend::install(cx);
        (release, backend)
    });
    let (chat, cx) = cx
        .add_window_view(|window, cx| ChatWindow::with_settings(settings(false), None, window, cx));
    cx.executor().allow_parking();
    let reads = || backend.reads.load(std::sync::atomic::Ordering::SeqCst);

    chat.update_in(cx, |chat, window, cx| chat.reconnect(NET, window, cx));
    chat.update(cx, |chat, _| {
        let id = chat.sessions[&NET].profile_id.clone();
        chat.saved.profile_mut(&id).unwrap().nickname = "other".into();
    });
    for _ in 0..3 {
        release.send(()).unwrap();
    }
    let wait = |until: &dyn Fn() -> bool| {
        let started = std::time::Instant::now();
        while !until() {
            assert!(started.elapsed().as_secs() < 20, "the reads finished");
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.run_until_parked();
        }
    };
    wait(&|| reads() == 3);
    cx.run_until_parked();
    // The first result was dropped, and the next read waits for its gate.
    chat.read_with(cx, |chat, _| {
        assert!(chat.sessions[&NET].reading_saved, "read again");
        assert!(chat.sessions[&NET].irc.is_none());
    });
    for _ in 0..3 {
        release.send(()).unwrap();
    }
    wait(&|| !chat.read_with(cx, |chat, _| chat.sessions[&NET].reading_saved));
    assert_eq!(reads(), 6);
    chat.update(cx, |chat, _| stop(chat));
}

/// A change of the store is whole: what is queued while the secrets move
/// lands in the new store, not in the old one whose copies are deleted.
#[gpui::test]
fn a_write_queued_during_a_move_lands_in_the_new_store(cx: &mut TestAppContext) {
    use cayenchat_storage::CredentialStore;
    use cayenchat_storage::credentials::MemoryBackend;
    let key = settings(false).servers[0].server_password_key();
    let to = CredentialStore::with_backend(std::sync::Arc::new(MemoryBackend::new(
        cayenchat_storage::CredentialBackendKind::LocalFile,
    )));
    let (release, _from) = cx.update(|cx| {
        let (release, _, from) = GatedBackend::install(cx);
        from.set(&key, &Secret::new("old")).unwrap();
        let (to, moved) = (to.clone(), key.clone());
        // The move waits for its read; the write is queued behind it.
        drop(crate::secrets::run_switching(cx, move |from| {
            let copied = cayenchat_storage::credentials::copy_secrets(
                from,
                &to,
                std::slice::from_ref(&moved),
            );
            copied.unwrap().remove_originals(from);
            ((), Some(to))
        }));
        let key = key.clone();
        drop(crate::secrets::run(cx, move |store| {
            store.set(&key, &Secret::new("new"))
        }));
        (release, from)
    });
    release.send(()).unwrap();
    let started = std::time::Instant::now();
    while to.get(&key).unwrap().is_none_or(|v| v.expose() != "new") {
        assert!(started.elapsed().as_secs() < 20, "the write went through");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // The store the UI asks about is the worker's, not a second copy that a
    // closed window could have left behind.
    cx.update(|cx| {
        assert_eq!(
            crate::secrets::store(cx).kind(),
            cayenchat_storage::CredentialBackendKind::LocalFile
        );
    });
}
