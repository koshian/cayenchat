// SPDX-License-Identifier: GPL-3.0-only
//! Tells the UI that the machine has woken from sleep (issue #437).
//!
//! A connection can die while the machine sleeps without the client OS ever
//! reporting it, so after waking the UI asks each connection to prove it is
//! alive. This module only notices the wake-up and sends `()` through a
//! channel; it knows nothing about connections.
//!
//! The OS is asked first: `NSWorkspaceDidWakeNotification` on macOS, a
//! suspend/resume callback from `PowerRegisterSuspendResumeNotification` on
//! Windows (no window involved) and logind's `PrepareForSleep(false)` on
//! Linux. As a fallback for when no such notice comes (no logind, a missed
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

/// Starts watching. The receiver gets one `()` per wake-up notice, possibly
/// several for one wake-up.
pub fn install() -> mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = mpsc::unbounded();
    let native = platform::watch(sender.clone());
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
    receiver
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
    pub fn watch(sender: mpsc::UnboundedSender<()>) -> bool {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("wake-logind".into())
            .spawn(move || {
                if let Err(error) = listen(&sender, &ready_tx) {
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

    fn listen(
        sender: &mpsc::UnboundedSender<()>,
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
        let _ = ready.send(true);
        for signal in signals {
            // `true` announces the sleep, `false` the wake-up.
            if signal.body().deserialize::<bool>() == Ok(false)
                && sender.unbounded_send(()).is_err()
            {
                break;
            }
        }
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
        DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND,
    };

    /// Called by the system on one of its threads.
    unsafe extern "system" fn on_power_event(
        context: *const c_void,
        kind: u32,
        _setting: *const c_void,
    ) -> u32 {
        if kind == PBT_APMRESUMEAUTOMATIC || kind == PBT_APMRESUMESUSPEND {
            // SAFETY: `context` is the sender leaked in `watch`, never freed.
            let sender = unsafe { &*context.cast::<mpsc::UnboundedSender<()>>() };
            let _ = sender.unbounded_send(());
        }
        0
    }

    pub fn watch(sender: mpsc::UnboundedSender<()>) -> bool {
        let context = Box::into_raw(Box::new(sender));
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
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
    use objc2_foundation::NSNotification;
    use std::ptr::NonNull;

    pub fn watch(sender: mpsc::UnboundedSender<()>) -> bool {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            let _ = sender.unbounded_send(());
        });
        // SAFETY: the name is a valid notification name, no object or queue
        // is given (the block runs on the posting thread) and the block is
        // `Send` and `'static`.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidWakeNotification),
                None,
                None,
                &block,
            )
        };
        // Observing lasts as long as the process.
        std::mem::forget(observer);
        true
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod platform {
    use super::*;

    pub fn watch(_sender: mpsc::UnboundedSender<()>) -> bool {
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
