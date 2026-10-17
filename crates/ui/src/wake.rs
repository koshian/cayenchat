// SPDX-License-Identifier: GPL-3.0-only
//! Tells the UI that the machine is about to sleep (issue #445) and that it
//! has woken from sleep (issue #437).
//!
//! A connection can die while the machine sleeps without the client OS ever
//! reporting it, so before sleeping the UI quits its connections and after
//! waking it reconnects them, or asks each to prove it is alive when no
//! notice of the sleep came. This module only notices the two events and
//! sends them through channels; it knows nothing about connections.
//!
//! The OS is asked first: `NSWorkspaceWillSleepNotification` and
//! `NSWorkspaceDidWakeNotification` on macOS, the suspend/resume callback
//! from `PowerRegisterSuspendResumeNotification` on Windows (no window
//! involved) and logind's `PrepareForSleep` on Linux, where a delay
//! inhibitor lock gives the UI time before the sleep starts. The OS is held
//! back while the UI works: the notice carries a [`SleepNotice`] that the
//! UI drops when it is done, and the OS-side handler waits for that, at
//! most [`SLEEP_ACK_LIMIT`]. As a fallback for when no such notice comes (no logind, a missed
//! notification) a thread sleeps and compares the wall clock with the time
//! it slept: a large jump means the machine was suspended meanwhile. It runs
//! every [`NATIVE_POLL`] when the OS notice is available and every
//! [`FALLBACK_POLL`] when it is not. Notices may repeat; the receiver
//! coalesces them.

use futures_channel::mpsc;
use std::time::{Duration, SystemTime};

/// Poll interval of the clock check next to an OS notice.
const NATIVE_POLL: Duration = Duration::from_secs(60);
/// Poll interval of the clock check when the OS gives no notice.
const FALLBACK_POLL: Duration = Duration::from_secs(5);
/// How much longer than its interval a sleep may take on the wall clock
/// before it counts as a suspension; a loaded machine or a small clock
/// adjustment does not reach it.
const SLACK: Duration = Duration::from_secs(10);

/// How long the OS-side handler waits for the UI to finish its work before
/// sleep. The UI itself waits for the servers for about a second.
const SLEEP_ACK_LIMIT: Duration = Duration::from_millis(1500);

/// What [`install`] watches for.
pub struct Notices {
    /// One `()` per wake-up notice, possibly several for one wake-up.
    pub wake: mpsc::UnboundedReceiver<()>,
    /// One notice before each sleep that the OS announces. A sleep that is
    /// not announced (forced, no logind) gives none.
    pub sleep: mpsc::UnboundedReceiver<SleepNotice>,
}

/// The machine is about to sleep. The OS goes on when this is dropped (or
/// after [`SLEEP_ACK_LIMIT`]).
pub struct SleepNotice {
    _done: std::sync::mpsc::Sender<()>,
}

/// Sends a [`SleepNotice`]; the receiver reports `Disconnected` once the UI
/// has dropped it. `None` when the UI no longer listens.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows", target_os = "macos")),
    allow(dead_code)
)]
fn announce_sleep(
    sender: &mpsc::UnboundedSender<SleepNotice>,
) -> Option<std::sync::mpsc::Receiver<()>> {
    let (done, finished) = std::sync::mpsc::channel();
    sender
        .unbounded_send(SleepNotice { _done: done })
        .ok()
        .map(|()| finished)
}

/// Starts watching.
pub fn install() -> Notices {
    let (sender, wake) = mpsc::unbounded();
    let (sleep_sender, sleep) = mpsc::unbounded();
    let native = platform::watch(sender.clone(), sleep_sender);
    let interval = if native { NATIVE_POLL } else { FALLBACK_POLL };
    let spawned = std::thread::Builder::new()
        .name("wake-clock".into())
        .spawn(move || {
            let mut clock = ClockWatch::new(interval, SystemTime::now);
            loop {
                std::thread::sleep(interval);
                if clock.jumped() && sender.unbounded_send(()).is_err() || sender.is_closed() {
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not watch the clock for wake-ups: {error}");
    }
    Notices { wake, sleep }
}

/// Notices a suspension as a jump of the wall clock past the poll interval.
struct ClockWatch<F: Fn() -> SystemTime> {
    interval: Duration,
    now: F,
    last: SystemTime,
}

impl<F: Fn() -> SystemTime> ClockWatch<F> {
    fn new(interval: Duration, now: F) -> Self {
        let last = now();
        Self {
            interval,
            now,
            last,
        }
    }

    /// Whether more time passed since the last call than one interval and the
    /// slack account for. A clock set backwards is not a jump.
    fn jumped(&mut self) -> bool {
        let now = (self.now)();
        let elapsed = now.duration_since(self.last).unwrap_or_default();
        self.last = now;
        elapsed >= self.interval + SLACK
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    const LOGIND: &str = "org.freedesktop.login1";

    /// Subscribes to logind's `PrepareForSleep` on the system bus; `false`
    /// when there is no logind to ask.
    pub fn watch(
        sender: mpsc::UnboundedSender<()>,
        sleep: mpsc::UnboundedSender<SleepNotice>,
    ) -> bool {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("wake-logind".into())
            .spawn(move || {
                if let Err(error) = listen(&sender, &sleep, &ready_tx) {
                    log::info!("not watching logind for wake-ups: {error}");
                    let _ = ready_tx.send(false);
                }
            });
        // Bounded, so a stuck bus cannot hold up the start of the application.
        spawned.is_ok()
            && ready_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap_or(false)
    }

    /// A lock that delays the sleep until it is dropped (or logind's
    /// `InhibitDelayMaxSec`, 5 s by default, passes). `None` when logind
    /// refuses; the notice still comes, only without a head start.
    fn delay_sleep(manager: &zbus::blocking::Proxy<'_>) -> Option<zbus::zvariant::OwnedFd> {
        match manager.call(
            "Inhibit",
            &(
                "sleep",
                "CayenChat",
                "Quitting the IRC servers before sleep",
                "delay",
            ),
        ) {
            Ok(lock) => Some(lock),
            Err(error) => {
                log::info!("could not delay sleep for the QUIT: {error}");
                None
            }
        }
    }

    fn listen(
        sender: &mpsc::UnboundedSender<()>,
        sleep: &mpsc::UnboundedSender<SleepNotice>,
        ready: &std::sync::mpsc::Sender<bool>,
    ) -> zbus::Result<()> {
        let connection = zbus::blocking::Connection::system()?;
        let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
        if !bus.name_has_owner(LOGIND.try_into()?)? {
            let _ = ready.send(false);
            return Ok(());
        }
        let manager = zbus::blocking::Proxy::new(
            &connection,
            LOGIND,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )?;
        let signals = manager.receive_signal("PrepareForSleep")?;
        let mut lock = delay_sleep(&manager);
        let _ = ready.send(true);
        for signal in signals {
            // `true` announces the sleep, `false` the wake-up.
            match signal.body().deserialize::<bool>() {
                Ok(true) => {
                    if let Some(finished) = announce_sleep(sleep) {
                        let _ = finished.recv_timeout(SLEEP_ACK_LIMIT);
                    }
                    // Let the sleep go on.
                    lock = None;
                }
                Ok(false) => {
                    if lock.is_none() {
                        lock = delay_sleep(&manager);
                    }
                    if sender.unbounded_send(()).is_err() {
                        break;
                    }
                }
                Err(_) => {}
            }
        }
        drop(lock);
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::ffi::c_void;
    use windows_sys::Win32::System::Power::{
        DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, PowerRegisterSuspendResumeNotification,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
    };

    struct Senders {
        wake: mpsc::UnboundedSender<()>,
        sleep: mpsc::UnboundedSender<SleepNotice>,
    }

    /// Called by the system on one of its threads.
    unsafe extern "system" fn on_power_event(
        context: *const c_void,
        kind: u32,
        _setting: *const c_void,
    ) -> u32 {
        // SAFETY: `context` is the `Senders` leaked in `watch`, never freed.
        let senders = unsafe { &*context.cast::<Senders>() };
        if kind == PBT_APMSUSPEND {
            // The system suspends once this returns.
            if let Some(finished) = announce_sleep(&senders.sleep) {
                let _ = finished.recv_timeout(SLEEP_ACK_LIMIT);
            }
        } else if kind == PBT_APMRESUMEAUTOMATIC || kind == PBT_APMRESUMESUSPEND {
            let _ = senders.wake.unbounded_send(());
        }
        0
    }

    pub fn watch(
        sender: mpsc::UnboundedSender<()>,
        sleep: mpsc::UnboundedSender<SleepNotice>,
    ) -> bool {
        let context = Box::into_raw(Box::new(Senders {
            wake: sender,
            sleep,
        }));
        let mut parameters = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(on_power_event),
            Context: context.cast(),
        };
        let mut registration = std::ptr::null_mut();
        // SAFETY: `parameters` outlives the call; the callback only uses the
        // leaked sender, so the registration is never removed.
        let status = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                (&mut parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast(),
                &mut registration,
            )
        };
        if status != 0 {
            log::info!(
                "not watching for wake-ups: PowerRegisterSuspendResumeNotification returned {status}"
            );
            // SAFETY: not registered, so nothing else holds the pointer.
            drop(unsafe { Box::from_raw(context) });
            return false;
        }
        true
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use block2::RcBlock;
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSNotification, NSRunLoop, NSThread};
    use std::ptr::NonNull;
    use std::sync::mpsc::TryRecvError;

    /// Waits for the UI to finish. The notice is posted on the main thread,
    /// which is also the UI thread: blocking it would keep the UI from ever
    /// getting to the notice, so the run loop is kept turning meanwhile.
    fn wait_for_ui(finished: std::sync::mpsc::Receiver<()>) {
        if !NSThread::isMainThread_class() {
            let _ = finished.recv_timeout(SLEEP_ACK_LIMIT);
            return;
        }
        let deadline = std::time::Instant::now() + SLEEP_ACK_LIMIT;
        let run_loop = NSRunLoop::currentRunLoop();
        while matches!(finished.try_recv(), Err(TryRecvError::Empty))
            && std::time::Instant::now() < deadline
        {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
            // SAFETY: the default mode is a valid run loop mode.
            let _ = run_loop.runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &until);
        }
    }

    pub fn watch(
        sender: mpsc::UnboundedSender<()>,
        sleep: mpsc::UnboundedSender<SleepNotice>,
    ) -> bool {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let woke = RcBlock::new(move |_: NonNull<NSNotification>| {
            let _ = sender.unbounded_send(());
        });
        let will_sleep = RcBlock::new(move |_: NonNull<NSNotification>| {
            if let Some(finished) = announce_sleep(&sleep) {
                wait_for_ui(finished);
            }
        });
        // SAFETY: the names are valid notification names, no object or queue
        // is given (the blocks run on the posting thread) and the blocks are
        // `Send` and `'static`.
        let (wake_observer, sleep_observer) = unsafe {
            (
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceDidWakeNotification),
                    None,
                    None,
                    &woke,
                ),
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceWillSleepNotification),
                    None,
                    None,
                    &will_sleep,
                ),
            )
        };
        // Observing lasts as long as the process.
        std::mem::forget(wake_observer);
        std::mem::forget(sleep_observer);
        true
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod platform {
    use super::*;

    pub fn watch(
        _sender: mpsc::UnboundedSender<()>,
        _sleep: mpsc::UnboundedSender<SleepNotice>,
    ) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + seconds)
    }

    #[test]
    fn a_wall_clock_jump_past_the_interval_is_a_suspension() {
        let time = Cell::new(0);
        let mut clock = ClockWatch::new(Duration::from_secs(5), || at(time.get()));
        // Ordinary ticks, also a late one on a loaded machine.
        for seconds in [5, 10, 19] {
            time.set(seconds);
            assert!(!clock.jumped(), "{seconds}");
        }
        // 5 s of sleep took an hour of wall time.
        time.set(19 + 3600);
        assert!(clock.jumped());
        // The jump is reported once.
        time.set(19 + 3605);
        assert!(!clock.jumped());
    }

    #[test]
    fn a_clock_set_backwards_is_not_a_suspension() {
        let time = Cell::new(100);
        let mut clock = ClockWatch::new(Duration::from_secs(60), || at(time.get()));
        time.set(10);
        assert!(!clock.jumped());
        time.set(70);
        assert!(!clock.jumped());
    }
}
