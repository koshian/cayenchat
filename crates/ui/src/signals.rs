//! Ends the application cleanly on SIGTERM, SIGHUP and SIGINT (issue #285).
//!
//! Neither this application nor GPUI handles signals, so by default they end the
//! process at once and no QUIT is sent. A dedicated thread waits for the signals
//! and wakes the UI thread through a channel, so nothing polls. The UI then
//! quits like Cmd+Q does, which saves the layout and sends QUIT. Two safeguards
//! make sure the process still ends: a watchdog exits after [`GRACE`] when the
//! UI thread does not finish (it may be blocked), and a second signal exits at
//! once.

use futures_channel::mpsc;
use futures_util::StreamExt;
use gpui::App;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::time::Duration;

/// How long the application may take to quit after the first signal. It
/// covers the 2 s QUIT wait with some room.
const GRACE: Duration = Duration::from_secs(3);

/// Starts the signal thread. The returned receiver gets one item per signal
/// until the first one is acted on; give it to [`quit_on_signal`].
pub fn install() -> Option<mpsc::UnboundedReceiver<()>> {
    let (sender, receiver) = mpsc::unbounded();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    // The handlers are registered inside the thread, so when the thread cannot
    // be spawned no handler exists and the default action stays in force.
    let spawned = std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            let mut signals = match Signals::new([SIGTERM, SIGHUP, SIGINT]) {
                Ok(signals) => signals,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            let mut first = true;
            for signal in signals.forever() {
                if !first {
                    std::process::exit(128 + signal);
                }
                first = false;
                // Without a watchdog there is no safeguard, so end at once.
                let watchdog = std::thread::Builder::new()
                    .name("signal-watchdog".into())
                    .spawn(move || {
                        std::thread::sleep(GRACE);
                        std::process::exit(128 + signal);
                    });
                if watchdog.is_err() {
                    std::process::exit(128 + signal);
                }
                // A closed receiver means the UI is gone; the watchdog ends us.
                let _ = sender.unbounded_send(());
            }
        });
    let failure = match (spawned, ready_rx.recv()) {
        (Ok(_), Ok(Ok(()))) => return Some(receiver),
        (Err(error), _) | (_, Ok(Err(error))) => error,
        (_, Err(_)) => std::io::Error::other("signal thread ended early"),
    };
    log::warn!("could not handle termination signals: {failure}");
    None
}

/// Quits the application when the first signal arrives.
pub fn quit_on_signal(mut receiver: mpsc::UnboundedReceiver<()>, cx: &mut App) {
    cx.spawn(async move |cx| {
        if receiver.next().await.is_some() {
            let _ = cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    //! The test binary re-runs itself as a child that installs the handlers,
    //! so that the real process-wide signal behaviour is checked.
    use super::*;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::Instant;

    const MODE_VAR: &str = "CAYENCHAT_SIGNAL_TEST_MODE";

    /// Child side. `quit` ends normally once notified, like the UI does;
    /// `stuck` never reads the notification, like a blocked UI thread.
    #[test]
    fn signal_child() {
        let Ok(mode) = std::env::var(MODE_VAR) else {
            return;
        };
        let mut receiver = install().expect("install handlers");
        println!("ready");
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if mode == "quit" && matches!(receiver.try_recv(), Ok(())) {
                std::process::exit(0);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::process::exit(99);
    }

    fn spawn_child(mode: &str) -> Child {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "signals::tests::signal_child", "--nocapture"])
            .env(MODE_VAR, mode)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // Wait until the handlers are installed.
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            use std::io::BufRead;
            assert!(out.read_line(&mut line).unwrap() > 0, "child ended early");
            if line.trim() == "ready" {
                break;
            }
        }
        // The child prints nothing after "ready", so the pipe can be closed.
        drop(out);
        child
    }

    fn send(child: &Child, signal: i32) {
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    }

    fn wait(mut child: Child, limit: Duration) -> (ExitStatus, Duration) {
        let start = Instant::now();
        while start.elapsed() < limit {
            if let Some(status) = child.try_wait().unwrap() {
                return (status, start.elapsed());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("child still running after {limit:?}");
    }

    fn code(status: ExitStatus) -> Option<i32> {
        status.code()
    }

    #[test]
    fn each_signal_reaches_the_quit_path() {
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            let child = spawn_child("quit");
            send(&child, signal);
            let (status, elapsed) = wait(child, Duration::from_secs(2));
            assert_eq!(code(status), Some(0), "signal {signal}");
            assert!(elapsed < Duration::from_secs(1));
        }
    }

    #[test]
    fn watchdog_ends_a_stuck_process() {
        let child = spawn_child("stuck");
        send(&child, libc::SIGTERM);
        let (status, elapsed) = wait(child, Duration::from_secs(6));
        assert_eq!(code(status), Some(128 + libc::SIGTERM));
        assert!(elapsed >= GRACE - Duration::from_millis(200), "{elapsed:?}");
    }

    #[test]
    fn second_signal_ends_a_stuck_process_at_once() {
        let child = spawn_child("stuck");
        send(&child, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(200));
        send(&child, libc::SIGINT);
        let (status, elapsed) = wait(child, Duration::from_secs(2));
        assert_eq!(code(status), Some(128 + libc::SIGINT));
        assert!(elapsed < Duration::from_secs(1));
    }
}
