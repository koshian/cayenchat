//! Minimal stderr logger so GPUI's own warnings and errors (renderer, display
//! server, fonts) are visible. Without one, the `log` records are dropped and
//! platform failures, especially on Linux, leave no trace.

use log::{LevelFilter, Log, Metadata, Record};

struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{} {}] {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

/// Installs the logger. `RUST_LOG=error|warn|info|debug|trace|off` sets the
/// level; the default is `warn`.
pub fn init() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(LevelFilter::Warn);
    if log::set_logger(&StderrLogger).is_ok() {
        log::set_max_level(level);
    }
}
