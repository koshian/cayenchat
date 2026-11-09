// SPDX-License-Identifier: GPL-3.0-only
//! The application's credential store as a GPUI global. Every window reads
//! and writes secrets through it; nothing else opens a backend.
//!
//! Startup installs the chosen backend before any window opens, and tests
//! install an in-memory store. Nothing falls back to the system store when
//! the global is missing: on macOS the Keychain asks for the login password
//! the moment a test binary reads the user's "CayenChat" item.

use std::future::Future;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use cayenchat_storage::credentials::{MemoryBackend, SystemBackend};
use cayenchat_storage::{CredentialBackendKind, CredentialError, CredentialStore};
use gpui::{App, Global};

use crate::localization::Localizer;

/// The store of an app without a worker (tests); production has a `Worker`.
pub struct Credentials(pub CredentialStore);

impl Global for Credentials {}

pub fn install(kind: CredentialBackendKind, cx: &mut App) {
    let store = CredentialStore::open(kind);
    // The worker is the only owner of the store: a move of the secrets
    // changes the store it holds, and a second copy in `Credentials` would
    // go stale. `Credentials` is for tests without a worker.
    cx.set_global(Worker::start(store));
}

type Job = Box<dyn FnOnce() + Send>;

/// The one thread that reads and writes the credential store (D087). The
/// operating system's store may stop for a permission dialog or a D-Bus
/// answer, and two threads taking turns at its lock would stop the UI
/// thread too (#638), so the UI thread never calls the store: it queues a
/// job here and gets the result as an event (`run`).
#[derive(Clone)]
pub struct Worker {
    queue: mpsc::Sender<Job>,
    /// The store the next job uses. Only the store thread changes it, at the
    /// end of a job that switches the store, so every job sees the store
    /// that is current at its place in the queue, not the one that was
    /// installed when it was queued.
    current: Arc<Mutex<CredentialStore>>,
}

impl Global for Worker {}

impl Worker {
    pub fn start(store: CredentialStore) -> Self {
        let (queue, jobs) = mpsc::channel::<Job>();
        #[cfg(test)]
        let test_file = crate::settings_file::share();
        let spawned = std::thread::Builder::new()
            .name("credential-store".into())
            .spawn(move || {
                #[cfg(test)]
                crate::settings_file::adopt(test_file);
                while let Ok(job) = jobs.recv() {
                    // A panic ends that job only. Its result is never sent
                    // (the caller's receiver closes, so it sees a failure,
                    // `stopped`), a switch of the store it was making is not
                    // applied, and it is not run again. The jobs after it
                    // go on. The panic message is not printed here beyond
                    // the default hook; jobs hold secrets only inside the
                    // store, never in their panic payload.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                        eprintln!("CayenChat: a credential store job panicked");
                    }
                }
            });
        if let Err(error) = spawned {
            // The closure, with the queue's receiver, is dropped: every
            // later send fails and its job is dropped, which closes the
            // caller's receiver. The jobs fail; nothing falls back to the
            // UI thread.
            eprintln!("CayenChat: credential store thread: {error}");
        }
        Self {
            queue,
            current: Arc::new(Mutex::new(store)),
        }
    }

    /// Queues `job` against the current store. The receiver closes without
    /// a value if the job panics or the thread is gone.
    fn submit<T, F>(&self, job: F) -> futures_channel::oneshot::Receiver<T>
    where
        T: Send + 'static,
        F: FnOnce(&CredentialStore) -> (T, Option<CredentialStore>) + Send + 'static,
    {
        let (done, result) = futures_channel::oneshot::channel();
        let current = self.current.clone();
        let queued: Job = Box::new(move || {
            let store = lock(&current).clone();
            let (value, switched) = job(&store);
            if let Some(switched) = switched {
                *lock(&current) = switched;
            }
            let _ = done.send(value);
        });
        let _ = self.queue.send(queued);
        result
    }
}

/// Runs `job` against the current store on the store thread and resolves
/// with its result, `None` if the thread is gone. Jobs run one at a time in
/// the order they were queued, and each uses the store that is current at
/// its place in the queue: a switch of the store (`run_switching`) applies
/// to the jobs queued after it and to none before it. Without a worker
/// (tests with an in-memory store) the job runs at once against the
/// installed store; the result is still delivered through the returned
/// future.
pub fn run<T, F>(cx: &App, job: F) -> impl Future<Output = Option<T>> + use<T, F>
where
    T: Send + 'static,
    F: FnOnce(&CredentialStore) -> T + Send + 'static,
{
    run_switching(cx, move |store| (job(store), None))
}

/// `run` for a job that may switch the store: the one it returns, if any,
/// becomes the current store as the job ends, before the next job starts.
/// This keeps a move of the secrets to another store whole: what is queued
/// while it runs lands in the new store, not in the old one it deletes.
pub fn run_switching<T, F>(cx: &App, job: F) -> impl Future<Output = Option<T>> + use<T, F>
where
    T: Send + 'static,
    F: FnOnce(&CredentialStore) -> (T, Option<CredentialStore>) + Send + 'static,
{
    let installed = store(cx);
    let result = match cx.try_global::<Worker>() {
        Some(worker) => worker.submit(job),
        None => {
            let (done, result) = futures_channel::oneshot::channel();
            let (value, _) = job(&installed);
            let _ = done.send(value);
            result
        }
    };
    async move { result.await.ok() }
}

fn lock(current: &Mutex<CredentialStore>) -> std::sync::MutexGuard<'_, CredentialStore> {
    current
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The error for a job that never ran or did not finish because the store
/// thread is gone or the job panicked. What the job did to the store is
/// unknown, so it is not a success.
pub fn stopped() -> CredentialError {
    CredentialError::Access("the credential store thread stopped or the job failed".into())
}

/// Waits, up to `limit`, for the jobs queued so far, so that a write made
/// just before quitting is not lost with the process.
pub fn flush(cx: &App, limit: Duration) {
    let Some(worker) = cx.try_global::<Worker>() else {
        return;
    };
    let (done, finished) = mpsc::channel();
    if worker
        .queue
        .send(Box::new(move || {
            let _ = done.send(());
        }))
        .is_ok()
    {
        let _ = finished.recv_timeout(limit);
    }
}

/// Installs an empty in-memory store and returns it. Tests call this before
/// anything reads a secret.
#[cfg(test)]
pub fn install_memory(cx: &mut App) -> CredentialStore {
    let store = memory_store(CredentialBackendKind::System);
    cx.set_global(Credentials(store.clone()));
    store
}

/// The installed store. A missing global is a bug, never a reason to open
/// the system store.
pub fn store(cx: &App) -> CredentialStore {
    // With the worker, the store it uses is the only one: a switch is made
    // there, by the job that moves the secrets, so no window has to carry
    // the outcome over (a window closed meanwhile would leave a second,
    // stale answer behind).
    if let Some(worker) = cx.try_global::<Worker>() {
        return lock(&worker.current).clone();
    }
    match cx.try_global::<Credentials>() {
        Some(credentials) => credentials.0.clone(),
        None => panic!("no credential store is installed"),
    }
}

/// Opens `kind` to switch to it. While the installed store is in memory
/// (tests), the new one is in memory too, so a switch reaches neither the
/// operating system nor the disk.
pub fn open(kind: CredentialBackendKind, cx: &App) -> CredentialStore {
    if store(cx).is_memory() {
        memory_store(kind)
    } else {
        CredentialStore::open(kind)
    }
}

/// A check of whether the operating system's store works, run on the store
/// thread like every other use of a store. While the installed store is in
/// memory it always succeeds without asking the operating system.
pub fn probe_system(cx: &App) -> impl Future<Output = Result<(), CredentialError>> + use<> {
    let in_memory = store(cx).is_memory();
    let probing = run(cx, move |_| {
        if in_memory {
            Ok(())
        } else {
            SystemBackend::probe()
        }
    });
    async move { probing.await.unwrap_or_else(|| Err(stopped())) }
}

fn memory_store(kind: CredentialBackendKind) -> CredentialStore {
    CredentialStore::with_backend(Arc::new(MemoryBackend::new(kind)))
}

/// User-facing text for a credential failure. Errors never contain secrets.
pub fn error_text(i18n: &Localizer, error: &CredentialError) -> String {
    let detail = error.to_string();
    match error {
        CredentialError::Unavailable(_) => {
            i18n.format("credential_error_unavailable", &[("error", &detail)])
        }
        _ => i18n.format("credential_error", &[("error", &detail)]),
    }
}

/// Where the local credential file is, for explanations.
pub fn local_path_text() -> String {
    cayenchat_storage::credentials::local_credentials_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| error)
}

/// Localization key describing this platform's secure store.
pub fn system_store_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "credential_system_macos"
    } else if cfg!(target_os = "windows") {
        "credential_system_windows"
    } else {
        "credential_system_linux"
    }
}
/// A test store that counts the reads and deletions of passwords, which is
/// what can make the operating system ask the user for permission. Channel
/// keys and tokens are not counted: other paths own them.
#[cfg(test)]
pub struct PasswordWatch {
    inner: MemoryBackend,
    touches: std::sync::atomic::AtomicUsize,
    key_touches: std::sync::atomic::AtomicUsize,
    refuse_reads: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl PasswordWatch {
    /// Installs the store; the returned handle counts.
    pub fn install(cx: &mut App) -> (Arc<Self>, CredentialStore) {
        let watch = Arc::new(Self {
            inner: MemoryBackend::new(CredentialBackendKind::System),
            touches: Default::default(),
            key_touches: Default::default(),
            refuse_reads: Default::default(),
        });
        let store = CredentialStore::with_backend(watch.clone());
        cx.set_global(Credentials(store.clone()));
        (watch, store)
    }

    pub fn touches(&self) -> usize {
        self.touches.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The reads and deletions of channel keys, which `touches` leaves out.
    pub fn key_touches(&self) -> usize {
        self.key_touches.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Makes every read fail, as when the system store denies access.
    pub fn refuse_reads(&self) {
        self.refuse_reads
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn count(&self, key: &cayenchat_storage::SecretKey) {
        use cayenchat_storage::SecretKey;
        if matches!(key, SecretKey::ChannelKey { .. }) {
            self.key_touches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        if matches!(
            key,
            SecretKey::ServerPassword { .. } | SecretKey::SaslPassword { .. }
        ) {
            self.touches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
impl cayenchat_storage::credentials::CredentialBackend for PasswordWatch {
    fn kind(&self) -> CredentialBackendKind {
        self.inner.kind()
    }

    fn is_memory(&self) -> bool {
        true
    }

    fn get(
        &self,
        key: &cayenchat_storage::SecretKey,
    ) -> Result<Option<cayenchat_storage::Secret>, CredentialError> {
        self.count(key);
        if self.refuse_reads.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(CredentialError::Access("denied".into()));
        }
        self.inner.get(key)
    }

    fn set(
        &self,
        key: &cayenchat_storage::SecretKey,
        value: &cayenchat_storage::Secret,
    ) -> Result<(), CredentialError> {
        self.inner.set(key, value)
    }

    fn delete(&self, key: &cayenchat_storage::SecretKey) -> Result<(), CredentialError> {
        self.count(key);
        self.inner.delete(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_storage::{Secret, SecretKey};

    fn worker() -> Worker {
        Worker::start(memory_store(CredentialBackendKind::System))
    }

    fn after_queue(worker: &Worker) {
        let (done, finished) = mpsc::channel();
        worker
            .queue
            .send(Box::new(move || {
                let _ = done.send(());
            }))
            .unwrap();
        finished.recv_timeout(Duration::from_secs(10)).unwrap();
    }

    #[test]
    fn a_panicking_job_fails_alone_and_the_next_job_runs() {
        let worker = worker();
        let key = SecretKey::server_password("secrets-test-panic");
        let panicking = worker.submit(|_| -> ((), Option<CredentialStore>) { panic!("boom") });
        let switching = worker.submit(|_| -> ((), _) {
            if true {
                panic!("boom while switching");
            }
            ((), Some(memory_store(CredentialBackendKind::LocalFile)))
        });
        let ok = worker.submit({
            let key = key.clone();
            move |store| (store.set(&key, &Secret::new("x")).is_ok(), None)
        });
        after_queue(&worker);
        use futures_util::FutureExt;
        assert!(matches!(panicking.now_or_never(), Some(Err(_))));
        assert!(matches!(switching.now_or_never(), Some(Err(_))));
        assert_eq!(ok.now_or_never().and_then(Result::ok), Some(true));
        // The store the panicking switch would have installed is not current.
        assert_eq!(lock(&worker.current).kind(), CredentialBackendKind::System);
    }

    #[test]
    fn a_failed_thread_spawn_fails_jobs_without_waiting() {
        // A worker whose thread never ran: its queue has no receiver.
        let (queue, jobs) = mpsc::channel::<Job>();
        drop(jobs);
        let worker = Worker {
            queue,
            current: Arc::new(Mutex::new(memory_store(CredentialBackendKind::System))),
        };
        let job = worker.submit(|_| ((), None));
        use futures_util::FutureExt;
        assert!(matches!(job.now_or_never(), Some(Err(_))));
    }

    #[gpui::test]
    #[should_panic(expected = "no credential store is installed")]
    fn without_an_installed_store_nothing_is_opened(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            store(cx);
        });
    }

    #[gpui::test]
    fn an_in_memory_store_keeps_switches_and_probes_in_memory(cx: &mut gpui::TestAppContext) {
        let key = SecretKey::server_password("secrets-test-never-the-real-store");
        cx.update(|cx| {
            let installed = install_memory(cx);
            // Checked before anything is stored or even read: a real store
            // fails here without being touched.
            assert!(installed.is_memory(), "tests must not use a real store");
            installed.set(&key, &Secret::new("hunter2")).unwrap();
            // Every handle reaches the same store.
            assert!(store(cx).get(&key).unwrap().is_some());

            for kind in [
                CredentialBackendKind::System,
                CredentialBackendKind::LocalFile,
            ] {
                let switched = open(kind, cx);
                assert!(switched.is_memory());
                assert_eq!(switched.kind(), kind);
                assert_eq!(switched.get(&key).unwrap(), None);
            }
            // No worker is installed, so the job has already run.
            assert_eq!(
                futures_util::FutureExt::now_or_never(probe_system(cx)),
                Some(Ok(()))
            );
        });
    }
}
