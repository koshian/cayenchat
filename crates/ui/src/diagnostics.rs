//! Minimal stderr logger so GPUI's own warnings and errors (renderer, display
//! server, fonts) are visible. Without one, the `log` records are dropped and
//! platform failures, especially on Linux, leave no trace.

use cayenchat_storage::Experimental;
use log::{LevelFilter, Log, Metadata, Record};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::Mutex,
};

// Changing stderr is process-wide. Only configure() changes it; the stderr
// lock also serializes the change with Rust's ordinary stderr writers.
static OUTPUT: Mutex<Output> = Mutex::new(Output {
    redirect: None,
    path: None,
});

struct Output {
    redirect: Option<platform::Redirect>,
    path: Option<PathBuf>,
}

pub fn configuration() -> Experimental {
    let output = OUTPUT.lock().unwrap_or_else(|error| error.into_inner());
    Experimental {
        debug_logging: output.path.is_some(),
        stderr_file: output.path.clone(),
    }
}

/// Redirect actual stderr, including eprintln! and the default panic hook.
/// Open the replacement before changing anything; a failed switch keeps the
/// current destination. Existing logs are appended, never truncated.
pub fn configure(settings: &Experimental) -> Result<(), String> {
    let mut output = OUTPUT.lock().unwrap_or_else(|error| error.into_inner());
    let path = if settings.debug_logging {
        let path = settings
            .stderr_file
            .as_ref()
            .ok_or("No log file selected")?;
        if !path.is_absolute() {
            return Err("The log file path must be absolute".into());
        }
        Some(path)
    } else {
        None
    };
    if output.path.as_ref() == path {
        return Ok(());
    }
    let result = (|| -> io::Result<()> {
        let file = path
            .map(|path| {
                let mut options = OpenOptions::new();
                options.create(true).append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                options.open(path)
            })
            .transpose()?;
        let mut stderr = io::stderr().lock();
        stderr.flush()?;
        match (output.redirect.as_mut(), file) {
            (Some(redirect), Some(file)) => redirect.replace(file)?,
            (None, Some(file)) => output.redirect = Some(platform::Redirect::new(file)?),
            (Some(redirect), None) => {
                redirect.restore()?;
                output.redirect = None;
            }
            (None, None) => {}
        }
        output.path = path.cloned();
        log::set_max_level(if path.is_some() {
            LevelFilter::Debug
        } else {
            environment_level()
        });
        if path.is_some() {
            let _ = writeln!(
                stderr,
                "[CayenChat {}] Debug stderr logging enabled ({})",
                env!("CARGO_PKG_VERSION"),
                chrono::Local::now().to_rfc3339()
            );
        }
        Ok(())
    })();
    result.map_err(|error| match path {
        Some(path) => format!("{}: {error}", path.display()),
        None => error.to_string(),
    })
}

fn environment_level() -> LevelFilter {
    std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(LevelFilter::Warn)
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::fd::{AsFd, AsRawFd, OwnedFd};

    pub(super) struct Redirect {
        original: OwnedFd,
    }

    impl Redirect {
        pub(super) fn new(file: File) -> io::Result<Self> {
            let original = io::stderr().as_fd().try_clone_to_owned()?;
            let mut redirect = Self { original };
            redirect.replace(file)?;
            Ok(redirect)
        }

        pub(super) fn replace(&mut self, file: File) -> io::Result<()> {
            duplicate_to_stderr(file.as_raw_fd())
        }

        pub(super) fn restore(&self) -> io::Result<()> {
            duplicate_to_stderr(self.original.as_raw_fd())
        }
    }

    fn duplicate_to_stderr(fd: std::os::fd::RawFd) -> io::Result<()> {
        // SAFETY: fd is borrowed from a live File/OwnedFd. dup2 atomically
        // replaces descriptor 2; the new descriptor outlives that borrow.
        if unsafe { libc::dup2(fd, libc::STDERR_FILENO) } == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::HANDLE,
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle},
    };

    pub(super) struct Redirect {
        // This is a borrowed process handle, possibly NULL in a GUI build.
        // Store its value without taking ownership or closing it.
        original: isize,
        file: File,
    }

    impl Redirect {
        pub(super) fn new(file: File) -> io::Result<Self> {
            // SAFETY: querying the process handle does not transfer ownership.
            let original = unsafe { GetStdHandle(STD_ERROR_HANDLE) } as isize;
            set(file.as_raw_handle())?;
            Ok(Self { original, file })
        }

        pub(super) fn replace(&mut self, file: File) -> io::Result<()> {
            set(file.as_raw_handle())?;
            self.file = file;
            Ok(())
        }

        pub(super) fn restore(&self) -> io::Result<()> {
            set(self.original as HANDLE)
        }
    }

    fn set(handle: HANDLE) -> io::Result<()> {
        // SAFETY: the replacement File stays alive until stderr is changed
        // again. The original borrowed process handle (including NULL) is
        // preserved. No console is allocated or attached.
        if unsafe { SetStdHandle(STD_ERROR_HANDLE, handle) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

struct StderrLogger;

/// Crates that handle credentials or authenticated requests. Their debug
/// output describes requests, raw IRC commands and credential entries, so it
/// stays off even with `RUST_LOG=trace`; warnings and errors still appear.
const QUIET_TARGETS: [&str; 9] = [
    "irc",
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
            // A full disk must not panic while reporting another error.
            let _ = writeln!(
                io::stderr().lock(),
                "[{} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

/// Installs the logger. `RUST_LOG=error|warn|info|debug|trace|off` sets the
/// level; the default is `warn`.
pub fn init() {
    let level = environment_level();
    if log::set_logger(&StderrLogger).is_ok() {
        log::set_max_level(level);
    }
}

#[cfg(test)]
mod tests {
    use super::quiet;

    // Run redirection in a child so the parallel test harness keeps its stderr.
    #[test]
    fn redirects_stderr_and_restores_it_without_truncating_logs() {
        run_stderr_child(false);
    }

    #[cfg(windows)]
    #[test]
    fn redirects_stderr_without_a_console_handle() {
        run_stderr_child(true);
    }

    fn run_stderr_child(detached: bool) {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("日本語 debug.log");
        std::fs::write(&first, "existing log\n").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "diagnostics::tests::stderr_child", "--nocapture"])
            .env("CAYENCHAT_STDERR_TEST_DIR", directory.path())
            .env(
                "CAYENCHAT_STDERR_TEST_DETACHED",
                if detached { "1" } else { "0" },
            )
            .env("RUST_LOG", "warn")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let first = std::fs::read_to_string(first).unwrap();
        let second = std::fs::read_to_string(directory.path().join("second.log")).unwrap();
        assert!(first.starts_with("existing log\n"));
        for marker in [
            "direct stderr 日本語",
            "debug marker",
            "panic marker",
            "failed switch preserved",
            "reenabled marker",
        ] {
            assert!(first.contains(marker), "missing {marker}: {first}");
        }
        assert!(!first.contains("secret marker"));
        assert!(!first.contains("second file marker"));
        assert!(!first.contains("restored stderr marker"));
        assert!(second.contains("second file marker"));
        assert!(!second.contains("restored stderr marker"));
        if !detached {
            assert!(String::from_utf8_lossy(&output.stderr).contains("restored stderr marker"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(directory.path().join("second.log"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }

    #[test]
    fn stderr_child() {
        use super::*;
        let Some(directory) = std::env::var_os("CAYENCHAT_STDERR_TEST_DIR") else {
            return;
        };
        let directory = PathBuf::from(directory);
        #[cfg(windows)]
        if std::env::var("CAYENCHAT_STDERR_TEST_DETACHED").as_deref() == Ok("1") {
            use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};
            // SAFETY: this isolated child simulates a GUI launch with no stderr.
            assert_ne!(
                unsafe { SetStdHandle(STD_ERROR_HANDLE, std::ptr::null_mut()) },
                0
            );
        }
        init();
        configure(&Experimental::default()).unwrap();
        let first = Experimental {
            debug_logging: true,
            stderr_file: Some(directory.join("日本語 debug.log")),
        };
        configure(&first).unwrap();
        assert_eq!(configuration(), first);
        eprintln!("direct stderr 日本語");
        log::debug!("debug marker");
        log::debug!(target: "irc::client", "secret marker");
        log::debug!(target: "ureq", "secret marker");
        let _ = std::panic::catch_unwind(|| panic!("panic marker"));
        let invalid = Experimental {
            debug_logging: true,
            stderr_file: Some(directory.join("missing").join("log")),
        };
        assert!(configure(&invalid).is_err());
        assert_eq!(configuration(), first);
        eprintln!("failed switch preserved");
        configure(&Experimental {
            debug_logging: true,
            stderr_file: Some(directory.join("second.log")),
        })
        .unwrap();
        eprintln!("second file marker");
        configure(&Experimental::default()).unwrap();
        assert_eq!(log::max_level(), LevelFilter::Warn);
        eprintln!("restored stderr marker");
        configure(&first).unwrap();
        eprintln!("reenabled marker");
        configure(&Experimental::default()).unwrap();
    }

    #[test]
    fn credential_and_http_crates_are_quiet() {
        assert!(quiet("ureq::run"));
        assert!(quiet("keyring_core"));
        assert!(quiet("keyring"));
        assert!(!quiet("gpui::window"));
        assert!(!quiet("ureqx"));
    }

    #[test]
    fn irc_wire_log_targets_are_quiet() {
        // irc logs unredacted PASS and AUTHENTICATE here, independently of
        // the application's redacted connection transcript.
        assert!(quiet("irc"));
        assert!(quiet("irc::client"));
        assert!(quiet("irc::client::transport"));
        assert!(!quiet("cayenchat_irc_core"));
        assert!(!quiet("irc_other"));
    }
}
