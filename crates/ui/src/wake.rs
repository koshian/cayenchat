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
//! The OS is asked first: IOKit power messages (`kIOMessageSystemWillSleep`,
//! on a thread of their own) and `NSWorkspaceDidWakeNotification` on macOS,
//! the suspend/resume callback
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
    pub wake: mpsc::UnboundedReceiver<WakeSource>,
    /// One notice before each sleep that the OS announces. A sleep that is
    /// not announced (forced, no logind) gives none.
    pub sleep: mpsc::UnboundedReceiver<SleepNotice>,
}

/// What noticed a wake-up. A DarkWake (issue #780) moves the wall clock like
/// any other wake, so the source tells whether the OS announced it or only the
/// clock check saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeSource {
    /// An OS wake notice.
    Os,
    /// The wall clock jumped.
    Clock,
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

/// How far the registration for the OS sleep notices got when the start waited
/// for it.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
enum Registration {
    /// The OS accepted it.
    Registered,
    /// The OS refused it, or the thread ended without an answer.
    Failed,
    /// No answer within the wait; it may still come, or never.
    Pending,
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
impl Registration {
    fn from_ready(ready: Result<bool, std::sync::mpsc::RecvTimeoutError>) -> Self {
        use std::sync::mpsc::RecvTimeoutError;
        match ready {
            Ok(true) => Self::Registered,
            Ok(false) | Err(RecvTimeoutError::Disconnected) => Self::Failed,
            Err(RecvTimeoutError::Timeout) => Self::Pending,
        }
    }

    /// Only a confirmed registration lets the clock check slow down.
    fn is_registered(&self) -> bool {
        *self == Self::Registered
    }
}

/// Starts watching.
pub fn install() -> Notices {
    let (sender, wake) = mpsc::unbounded();
    let (sleep_sender, sleep) = mpsc::unbounded();
    let native = platform::watch(sender.clone(), sleep_sender);
    let interval = if native { NATIVE_POLL } else { FALLBACK_POLL };
    log::info!("wake watching: OS notices {native}, clock check every {interval:?}");
    let spawned = std::thread::Builder::new()
        .name("wake-clock".into())
        .spawn(move || {
            let mut clock = ClockWatch::new(interval, SystemTime::now);
            loop {
                std::thread::sleep(interval);
                if clock.jumped() && sender.unbounded_send(WakeSource::Clock).is_err()
                    || sender.is_closed()
                {
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
        sender: mpsc::UnboundedSender<WakeSource>,
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
        sender: &mpsc::UnboundedSender<WakeSource>,
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
                    if sender.unbounded_send(WakeSource::Os).is_err() {
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
        wake: mpsc::UnboundedSender<WakeSource>,
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
            let _ = senders.wake.unbounded_send(WakeSource::Os);
        }
        0
    }

    pub fn watch(
        sender: mpsc::UnboundedSender<WakeSource>,
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
    use core_foundation::base::TCFType;
    use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
    use core_foundation_sys::runloop::CFRunLoopSourceRef;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
    use objc2_foundation::NSNotification;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicU32, Ordering};

    type IoConnect = u32;
    type IoObject = u32;
    type IoReturn = i32;
    type NotificationPort = *mut c_void;
    type InterestCallback = extern "C" fn(*mut c_void, IoObject, u32, *mut c_void);

    /// `kIOMessageCanSystemSleep`: the system asks whether it may idle-sleep.
    const CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
    /// `kIOMessageSystemWillSleep`: the sleep starts once every client allows it.
    const SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegisterForSystemPower(
            refcon: *mut c_void,
            notify_port: *mut NotificationPort,
            callback: InterestCallback,
            notifier: *mut IoObject,
        ) -> IoConnect;
        fn IONotificationPortGetRunLoopSource(notify: NotificationPort) -> CFRunLoopSourceRef;
        fn IOAllowPowerChange(root_port: IoConnect, notification_id: isize) -> IoReturn;
    }

    /// What the power callback needs; leaked, as the registration lasts as
    /// long as the process.
    struct Context {
        sleep: mpsc::UnboundedSender<SleepNotice>,
        /// The root power domain port, set before the run loop starts.
        root_port: AtomicU32,
    }

    extern "C" fn on_power_message(
        refcon: *mut c_void,
        _service: IoObject,
        message_type: u32,
        argument: *mut c_void,
    ) {
        // Other messages (sleep refused, wake steps) need no answer; the log
        // keeps the order of the ones that do, for a sleep that went by
        // without a QUIT.
        if message_type != CAN_SYSTEM_SLEEP && message_type != SYSTEM_WILL_SLEEP {
            log::debug!("IOKit power message {message_type:#x} ignored");
            return;
        }
        // SAFETY: `refcon` is the leaked `Context` given at registration, which
        // is never freed.
        let context = unsafe { &*refcon.cast::<Context>() };
        if message_type == SYSTEM_WILL_SLEEP {
            match announce_sleep(&context.sleep) {
                Some(finished) => {
                    log::info!("IOKit: system will sleep, waiting for the UI");
                    // This is the dedicated power thread, not the UI thread,
                    // so it may block until the UI is done or the limit
                    // passes.
                    let result = finished.recv_timeout(SLEEP_ACK_LIMIT);
                    log::info!("IOKit: UI done or limit reached ({result:?}), allowing sleep");
                }
                None => log::info!("IOKit: system will sleep, the UI no longer listens"),
            }
        } else {
            log::info!("IOKit: idle-sleep query, allowed");
        }
        // Exactly once per message, also when the UI is gone: without it the
        // sleep is held back for 30 s. An idle-sleep query is never refused.
        // SAFETY: the port came from `IORegisterForSystemPower` and the
        // argument is the notification id the system passed for this message.
        unsafe {
            IOAllowPowerChange(context.root_port.load(Ordering::Acquire), argument as isize);
        }
    }

    /// Registers for power messages and runs the thread's run loop; reports
    /// whether the registration worked.
    fn power_thread(
        sleep: mpsc::UnboundedSender<SleepNotice>,
        ready: std::sync::mpsc::Sender<bool>,
    ) {
        let context = Box::leak(Box::new(Context {
            sleep,
            root_port: AtomicU32::new(0),
        }));
        let mut port: NotificationPort = std::ptr::null_mut();
        let mut notifier: IoObject = 0;
        // SAFETY: the out pointers are valid for the call and `context` is
        // leaked, so it outlives every callback.
        let root_port = unsafe {
            IORegisterForSystemPower(
                (context as *mut Context).cast(),
                &mut port,
                on_power_message,
                &mut notifier,
            )
        };
        if root_port == 0 || port.is_null() {
            log::info!("not watching for sleep: IORegisterForSystemPower failed");
            let _ = ready.send(false);
            return;
        }
        context.root_port.store(root_port, Ordering::Release);
        // SAFETY: `port` is a live notification port; its source is retained
        // by the port, which is never destroyed.
        let source = unsafe { IONotificationPortGetRunLoopSource(port) };
        let run_loop = CFRunLoop::get_current();
        // SAFETY: the mode is a valid constant and the source is live.
        unsafe {
            core_foundation_sys::runloop::CFRunLoopAddSource(
                run_loop.as_concrete_TypeRef(),
                source,
                kCFRunLoopCommonModes,
            );
        }
        let _ = ready.send(true);
        CFRunLoop::run_current();
    }

    pub fn watch(
        sender: mpsc::UnboundedSender<WakeSource>,
        sleep: mpsc::UnboundedSender<SleepNotice>,
    ) -> bool {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let woke = RcBlock::new(move |_: NonNull<NSNotification>| {
            let _ = sender.unbounded_send(WakeSource::Os);
        });
        // SAFETY: the name is a valid notification name, no object or queue
        // is given (the block runs on the posting thread) and the block is
        // `Send` and `'static`.
        let wake_observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidWakeNotification),
                None,
                None,
                &woke,
            )
        };
        // Observing lasts as long as the process.
        std::mem::forget(wake_observer);

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("wake-iokit".into())
            .spawn(move || power_thread(sleep, ready_tx));
        if let Err(error) = spawned {
            log::warn!("could not watch for sleep: {error}");
            return false;
        }
        // Only a confirmed registration earns the slow clock check; a failed
        // or unanswered one keeps the short interval, so a wake-up the
        // notices miss is still found soon.
        let registration = Registration::from_ready(ready_rx.recv_timeout(Duration::from_secs(2)));
        log::info!("sleep notices from IOKit: {registration:?}");
        registration.is_registered()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod platform {
    use super::*;

    pub fn watch(
        _sender: mpsc::UnboundedSender<WakeSource>,
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
    fn only_a_confirmed_registration_slows_the_clock_check() {
        use std::sync::mpsc::{RecvTimeoutError, channel};
        let (sender, receiver) = channel();
        sender.send(true).unwrap();
        let registered = Registration::from_ready(receiver.recv_timeout(Duration::ZERO));
        assert_eq!(registered, Registration::Registered);
        assert!(registered.is_registered());
        // Refused, or the thread ended without answering.
        let refused = Registration::from_ready(Ok(false));
        assert_eq!(refused, Registration::Failed);
        assert!(!refused.is_registered());
        let gone = Registration::from_ready(Err(RecvTimeoutError::Disconnected));
        assert_eq!(gone, Registration::Failed);
        // Unanswered is not the same as failed, but is not trusted either.
        let (_held, receiver) = channel::<bool>();
        let late = Registration::from_ready(receiver.recv_timeout(Duration::from_millis(1)));
        assert_eq!(late, Registration::Pending);
        assert!(!late.is_registered());
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
