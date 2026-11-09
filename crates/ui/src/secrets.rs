// SPDX-License-Identifier: GPL-3.0-only
//! The application's credential store as a GPUI global. Every window reads
//! and writes secrets through it; nothing else opens a backend.
//!
//! Startup installs the chosen backend before any window opens, and tests
//! install an in-memory store. Nothing falls back to the system store when
//! the global is missing: on macOS the Keychain asks for the login password
//! the moment a test binary reads the user's "CayenChat" item.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::task::{Context, Poll, Waker};
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

/// The credential jobs whose outcome is not yet in the application: counted
/// from the moment one is queued until its caller has taken the result and
/// run what follows it (the note in the settings file, the report). An
/// ordinary quit waits for the count to reach zero (#982); the queue alone
/// is not enough, since the worker finishes a job before the UI thread has
/// recorded it.
#[derive(Default)]
struct Outstanding {
    count: AtomicUsize,
    /// Set by a forced quit: nothing is waited for any more.
    abandoned: AtomicBool,
    /// A write or delete failed (or its job was lost) since the user last
    /// acknowledged it: the quit that waited must not go on by itself.
    failed: AtomicBool,
    /// The tasks waiting for the count to reach zero (a quit, a move of the
    /// secrets). A task polled again replaces its own waker, so repeated
    /// polls do not pile wakers up.
    waiting: Mutex<Vec<Waker>>,
}

impl Outstanding {
    fn hold(self: &Arc<Self>) -> Hold {
        self.count.fetch_add(1, Ordering::SeqCst);
        Hold(self.clone())
    }

    fn waiting(&self) -> MutexGuard<'_, Vec<Waker>> {
        self.waiting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One count of `Outstanding`. A job holds one until it has run (or is
/// dropped unrun), and the future that delivers its result holds another
/// until that future has produced the result: the code after the `await`
/// runs in the same poll, before the UI thread can look at the count again.
struct Hold(Arc<Outstanding>);

impl Drop for Hold {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            let wakers = std::mem::take(&mut *self.0.waiting());
            wakers.into_iter().for_each(Waker::wake);
        }
    }
}

/// Resolves when no credential job is outstanding.
struct Idle(Arc<Outstanding>);

impl Future for Idle {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // The waker is registered before the count is read, under the lock
        // the last `Hold` takes to wake, so a release cannot slip between.
        let mut waiting = self.0.waiting();
        if self.0.count.load(Ordering::SeqCst) == 0 || self.0.abandoned.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        if !waiting.iter().any(|waker| waker.will_wake(cx.waker())) {
            waiting.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

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
    outstanding: Arc<Outstanding>,
    /// Set when a job that switches the store panicked: how far it moved the
    /// secrets and the setting is unknown, so `current` may not be the store
    /// the settings file names. Every later job fails without running until
    /// the app is restarted; none writes to a store that may be the wrong
    /// one.
    failed: Arc<AtomicBool>,
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
                    // go on, unless it was a switch of the store (`failed`).
                    // The panic message is not printed here beyond
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
            outstanding: Default::default(),
            failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Queues `job` against the current store. The receiver closes without
    /// a value if the job panics, the worker has failed or the thread is
    /// gone. `switching` is for a job that moves the secrets: if it panics,
    /// the worker fails (see `failed`).
    fn submit<T, F>(&self, switching: bool, job: F) -> futures_channel::oneshot::Receiver<T>
    where
        T: Send + 'static,
        F: FnOnce(&CredentialStore) -> (T, Option<CredentialStore>) + Send + 'static,
    {
        let (done, result) = futures_channel::oneshot::channel();
        let current = self.current.clone();
        let failed = self.failed.clone();
        let outstanding = self.outstanding.clone();
        // Held until the job has run or been dropped unrun.
        let running = self.outstanding.hold();
        let queued: Job = Box::new(move || {
            let _running = running;
            if failed.load(Ordering::SeqCst) {
                return;
            }
            // A panic leaves the flag set; a job that ends, with an error or
            // not, clears it.
            failed.store(switching, Ordering::SeqCst);
            let store = lock(&current).clone();
            let writes_failed = store.write_failures();
            let (value, switched) = job(&store);
            if store.write_failures() != writes_failed
                || switched.as_ref().is_some_and(|s| s.write_failures() > 0)
            {
                outstanding.failed.store(true, Ordering::SeqCst);
            }
            if let Some(switched) = switched {
                *lock(&current) = switched;
            }
            failed.store(false, Ordering::SeqCst);
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
    dispatch(cx, false, move |store| (job(store), None))
}

/// `run` for a job that may switch the store: the one it returns, if any,
/// becomes the current store as the job ends, before the next job starts.
/// This keeps a move of the secrets to another store whole: what is queued
/// while it runs lands in the new store, not in the old one it deletes.
///
/// If such a job panics, the secrets and the setting may be half moved, so
/// the worker fails: this and every later job resolves with `None` until the
/// app restarts, rather than write to a store that may be the wrong one.
pub fn run_switching<T, F>(cx: &App, job: F) -> impl Future<Output = Option<T>> + use<T, F>
where
    T: Send + 'static,
    F: FnOnce(&CredentialStore) -> (T, Option<CredentialStore>) + Send + 'static,
{
    dispatch(cx, true, job)
}

fn dispatch<T, F>(cx: &App, switching: bool, job: F) -> impl Future<Output = Option<T>> + use<T, F>
where
    T: Send + 'static,
    F: FnOnce(&CredentialStore) -> (T, Option<CredentialStore>) + Send + 'static,
{
    let installed = store(cx);
    let mut delivering = None;
    let result = match cx.try_global::<Worker>() {
        Some(worker) => {
            delivering = Some(worker.outstanding.hold());
            worker.submit(switching, job)
        }
        None => {
            let (done, result) = futures_channel::oneshot::channel();
            let (value, _) = job(&installed);
            let _ = done.send(value);
            result
        }
    };
    let outstanding = delivering.as_ref().map(|hold| hold.0.clone());
    async move {
        let value = result.await.ok();
        if value.is_none()
            && let Some(outstanding) = outstanding
        {
            // The job panicked or never ran: what it did is unknown.
            outstanding.failed.store(true, Ordering::SeqCst);
        }
        // Released as the result is handed over: what the caller does with
        // it runs in this same poll.
        drop(delivering);
        value
    }
}

/// Whether a credential job is still outstanding: queued, running, or done
/// but not yet taken in by its caller.
pub fn pending(cx: &App) -> bool {
    cx.try_global::<Worker>().is_some_and(|worker| {
        let outstanding = &worker.outstanding;
        outstanding.count.load(Ordering::SeqCst) > 0
            && !outstanding.abandoned.load(Ordering::SeqCst)
    })
}

/// Resolves when no credential job is outstanding any more (at once if none
/// is). Event driven: the release of the last job wakes it.
pub fn idle(cx: &App) -> impl Future<Output = ()> + use<> {
    let outstanding = cx
        .try_global::<Worker>()
        .map(|worker| worker.outstanding.clone());
    async move {
        if let Some(outstanding) = outstanding {
            Idle(outstanding).await;
        }
    }
}

/// Stops waiting for outstanding jobs: the user chose to quit anyway.
pub fn abandon(cx: &App) {
    if let Some(worker) = cx.try_global::<Worker>() {
        worker.outstanding.abandoned.store(true, Ordering::SeqCst);
        let wakers = std::mem::take(&mut *worker.outstanding.waiting());
        wakers.into_iter().for_each(Waker::wake);
    }
}

/// Whether a save or delete failed while the quit waited, and the user has
/// not yet been shown it.
pub fn failed(cx: &App) -> bool {
    cx.try_global::<Worker>()
        .is_some_and(|worker| worker.outstanding.failed.load(Ordering::SeqCst))
}

/// The user has seen the failure (cancelled the quit): later quits are
/// not held for it.
pub fn acknowledge_failure(cx: &App) {
    if let Some(worker) = cx.try_global::<Worker>() {
        worker.outstanding.failed.store(false, Ordering::SeqCst);
    }
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

/// Waits, up to `limit`, for the jobs queued so far. This is the last resort
/// of the quits that cannot ask (the system's shutdown, a signal) and says
/// nothing of the outcome; an ordinary quit waits with `idle` first and shows
/// what it waits for. Does nothing after `abandon`.
pub fn flush(cx: &App, limit: Duration) {
    let Some(worker) = cx.try_global::<Worker>() else {
        return;
    };
    if worker.outstanding.abandoned.load(Ordering::SeqCst) {
        return;
    }
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
        let panicking = worker.submit(false, |_| -> ((), Option<CredentialStore>) {
            panic!("boom")
        });
        let ok = worker.submit(false, {
            let key = key.clone();
            move |store| (store.set(&key, &Secret::new("x")).is_ok(), None)
        });
        after_queue(&worker);
        use futures_util::FutureExt;
        assert!(matches!(panicking.now_or_never(), Some(Err(_))));
        assert_eq!(ok.now_or_never().and_then(Result::ok), Some(true));
    }

    #[test]
    fn a_job_that_ends_with_an_error_does_not_fail_the_worker() {
        let worker = worker();
        let switch = worker.submit(true, |_| (Err::<(), _>(stopped()), None));
        let next = worker.submit(false, |_| ((), None));
        after_queue(&worker);
        use futures_util::FutureExt;
        assert!(matches!(switch.now_or_never(), Some(Ok(Err(_)))));
        assert!(matches!(next.now_or_never(), Some(Ok(()))));
    }

    #[test]
    fn a_panic_while_switching_the_store_fails_every_later_job() {
        let worker = worker();
        let key = SecretKey::server_password("secrets-test-panic-switch");
        // As the real move does: something is done to the new store, then
        // the job panics before it returns the store to switch to.
        let target = memory_store(CredentialBackendKind::LocalFile);
        let switching = worker.submit(true, {
            let target = target.clone();
            let key = key.clone();
            move |_| -> ((), Option<CredentialStore>) {
                target.set(&key, &Secret::new("old")).unwrap();
                panic!("boom while deleting the originals");
            }
        });
        let later_write = worker.submit(false, {
            let key = key.clone();
            move |store| (store.set(&key, &Secret::new("new")).is_ok(), None)
        });
        after_queue(&worker);
        let after = worker.submit(false, |_| ((), None));
        after_queue(&worker);
        use futures_util::FutureExt;
        assert!(matches!(switching.now_or_never(), Some(Err(_))));
        // Neither the queued write nor a later job ran: no secret went to
        // the store that may be the wrong one, and each failed, not hung.
        assert!(matches!(later_write.now_or_never(), Some(Err(_))));
        assert!(matches!(after.now_or_never(), Some(Err(_))));
        assert_eq!(lock(&worker.current).kind(), CredentialBackendKind::System);
        assert_eq!(
            lock(&worker.current).get(&key).unwrap(),
            None,
            "the store in use was not written"
        );
    }

    #[test]
    fn every_task_waiting_for_idle_is_woken() {
        use std::task::Wake;
        struct Flag(AtomicBool);
        impl Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let outstanding: Arc<Outstanding> = Default::default();
        let hold = outstanding.hold();
        let flags: Vec<_> = (0..2)
            .map(|_| Arc::new(Flag(AtomicBool::new(false))))
            .collect();
        for flag in &flags {
            let waker = Waker::from(flag.clone());
            let mut cx = Context::from_waker(&waker);
            assert!(
                Pin::new(&mut Idle(outstanding.clone()))
                    .poll(&mut cx)
                    .is_pending()
            );
        }
        drop(hold);
        assert!(flags.iter().all(|flag| flag.0.load(Ordering::SeqCst)));
    }

    #[test]
    fn a_failed_thread_spawn_fails_jobs_without_waiting() {
        // A worker whose thread never ran: its queue has no receiver.
        let (queue, jobs) = mpsc::channel::<Job>();
        drop(jobs);
        let worker = Worker {
            queue,
            current: Arc::new(Mutex::new(memory_store(CredentialBackendKind::System))),
            outstanding: Default::default(),
            failed: Arc::new(AtomicBool::new(false)),
        };
        let job = worker.submit(false, |_| ((), None));
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
