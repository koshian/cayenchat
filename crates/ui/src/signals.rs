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
    let mut signals = match Signals::new([SIGTERM, SIGHUP, SIGINT]) {
        Ok(signals) => signals,
        Err(error) => {
            log::warn!("could not handle termination signals: {error}");
            return None;
        }
    };
    let (sender, receiver) = mpsc::unbounded();
    let spawned = std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            let mut first = true;
            for signal in signals.forever() {
                if !first {
                    std::process::exit(128 + signal);
                }
                first = false;
                std::thread::spawn(move || {
                    std::thread::sleep(GRACE);
                    std::process::exit(128 + signal);
                });
                // A closed receiver means the UI is gone; the watchdog ends us.
                let _ = sender.unbounded_send(());
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not handle termination signals: {error}");
        return None;
    }
    Some(receiver)
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
