//! Minimal stderr logger so GPUI's own warnings and errors (renderer, display
//! server, fonts) are visible. Without one, the `log` records are dropped and
//! platform failures, especially on Linux, leave no trace.

use log::{LevelFilter, Log, Metadata, Record};

struct StderrLogger;

/// Crates that handle credentials or authenticated requests. Their debug
/// output describes requests and credential entries, so it stays off even
/// with `RUST_LOG=debug`; warnings and errors still appear.
const QUIET_TARGETS: [&str; 8] = [
    "ureq",
    "keyring_core",
    "rustls",
    "keyring",
    "apple_native_keyring_store",
    "windows_native_keyring_store",
    "zbus_secret_service_keyring_store",
    "secret_service",
];

fn quiet(target: &str) -> bool {
    QUIET_TARGETS
        .iter()
        .any(|quiet| target == *quiet || target.starts_with(&format!("{quiet}::")))
}

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
            && (metadata.level() <= log::Level::Warn || !quiet(metadata.target()))
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

#[cfg(test)]
mod tests {
    use super::quiet;

    #[test]
    fn credential_and_http_crates_are_quiet() {
        assert!(quiet("ureq::run"));
        assert!(quiet("keyring_core"));
        assert!(quiet("keyring"));
        assert!(!quiet("gpui::window"));
        assert!(!quiet("ureqx"));
    }
}
