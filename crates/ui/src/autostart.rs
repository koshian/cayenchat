//! Starting CayenChat when the user logs in (#153).
//!
//! The operating system's registration is the only truth: nothing is stored
//! in `settings.json`, and the settings window asks `status` whenever it is
//! shown. Each platform uses its standard mechanism: XDG Autostart on Linux,
//! `SMAppService` on macOS and the HKCU `Run` key on Windows. An entry the
//! user turned off in the system's own settings is reported as
//! `DisabledByUser` and is only changed again by an explicit choice here.
//!
//! Registered launches pass `--autostart`, so a later change can tell them
//! from a launch by hand. The flag does nothing yet.

/// The argument given to a launch made by the login registration.
pub const AUTOSTART_ARG: &str = "--autostart";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutostartStatus {
    Enabled,
    Disabled,
    /// Registered, but switched off in the system's own settings.
    DisabledByUser,
    /// This installation cannot register itself.
    Unavailable,
}

impl AutostartStatus {
    /// What the checkbox shows. A user-disabled entry counts as off.
    pub fn is_on(self) -> bool {
        self == Self::Enabled
    }
}

pub fn status() -> Result<AutostartStatus, String> {
    platform::status()
}

pub fn enable() -> Result<(), String> {
    platform::enable()
}

pub fn disable() -> Result<(), String> {
    platform::disable()
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::{Path, PathBuf};

    use super::{AUTOSTART_ARG, AutostartStatus};

    fn entry_path() -> Result<PathBuf, String> {
        entry_path_in(
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    }

    /// `$XDG_CONFIG_HOME` when it is an absolute path, otherwise `~/.config`.
    fn entry_path_in(
        config_home: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Result<PathBuf, String> {
        let base = match config_home.filter(|path| path.is_absolute()) {
            Some(path) => path,
            None => home.ok_or("the home directory is unknown")?.join(".config"),
        };
        Ok(base.join("autostart").join("cayenchat.desktop"))
    }

    /// Quotes one `Exec` argument as the Desktop Entry specification asks.
    fn quote(argument: &str) -> String {
        let special = |c: char| " \t\n\"'\\><~|&;$*?#()`".contains(c);
        if !argument.chars().any(special) {
            return argument.replace('%', "%%");
        }
        let mut quoted = String::from("\"");
        for c in argument.chars() {
            match c {
                '"' | '`' | '$' | '\\' => {
                    quoted.push('\\');
                    quoted.push(c);
                }
                '%' => quoted.push_str("%%"),
                _ => quoted.push(c),
            }
        }
        quoted.push('"');
        quoted
    }

    fn contents(executable: &Path) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=CayenChat\n\
             Comment=Modern IRC client with IRCv3 support\n\
             Exec={} {AUTOSTART_ARG}\nIcon=cayenchat\nTerminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            quote(&executable.to_string_lossy())
        )
    }

    /// An entry the desktop's own settings switched off keeps its file but
    /// gains `Hidden=true` or `X-GNOME-Autostart-enabled=false`.
    fn parse(text: &str) -> AutostartStatus {
        let off = text.lines().any(|line| {
            let line = line.trim();
            line.eq_ignore_ascii_case("Hidden=true")
                || line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
        });
        if off {
            AutostartStatus::DisabledByUser
        } else {
            AutostartStatus::Enabled
        }
    }

    pub fn status() -> Result<AutostartStatus, String> {
        match std::fs::read_to_string(entry_path()?) {
            Ok(text) => Ok(parse(&text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(AutostartStatus::Disabled)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn enable() -> Result<(), String> {
        let path = entry_path()?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        std::fs::write(path, contents(&executable)).map_err(|error| error.to_string())
    }

    pub fn disable() -> Result<(), String> {
        match std::fs::remove_file(entry_path()?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn config_home_is_honored_only_when_absolute() {
            let home = Some(PathBuf::from("/home/u"));
            assert_eq!(
                entry_path_in(Some("/cfg".into()), home.clone()).unwrap(),
                PathBuf::from("/cfg/autostart/cayenchat.desktop")
            );
            for unusable in [None, Some(PathBuf::from("")), Some(PathBuf::from("rel"))] {
                assert_eq!(
                    entry_path_in(unusable, home.clone()).unwrap(),
                    PathBuf::from("/home/u/.config/autostart/cayenchat.desktop")
                );
            }
            assert!(entry_path_in(None, None).is_err());
        }

        #[test]
        fn exec_line_quotes_the_path_and_passes_the_flag() {
            let text = contents(Path::new("/opt/Cayen Chat/cayenchat"));
            assert!(text.contains("Exec=\"/opt/Cayen Chat/cayenchat\" --autostart\n"));
            let plain = contents(Path::new("/usr/bin/cayenchat"));
            assert!(plain.contains("Exec=/usr/bin/cayenchat --autostart\n"));
            assert_eq!(parse(&plain), AutostartStatus::Enabled);
        }

        #[test]
        fn entries_switched_off_by_the_desktop_are_reported() {
            assert_eq!(
                parse("[Desktop Entry]\nHidden=true\n"),
                AutostartStatus::DisabledByUser
            );
            assert_eq!(
                parse("[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"),
                AutostartStatus::DisabledByUser
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    use super::AutostartStatus;

    pub fn status() -> Result<AutostartStatus, String> {
        // SAFETY: plain message sends to the main-app service object.
        let status = unsafe { SMAppService::mainAppService().status() };
        Ok(match status {
            SMAppServiceStatus::Enabled => AutostartStatus::Enabled,
            SMAppServiceStatus::RequiresApproval => AutostartStatus::DisabledByUser,
            // Not registered. A bare binary outside an app bundle cannot
            // register, and the call then fails with an error shown to the user.
            _ => AutostartStatus::Disabled,
        })
    }

    pub fn enable() -> Result<(), String> {
        // SAFETY: as above.
        unsafe { SMAppService::mainAppService().registerAndReturnError() }
            .map_err(|error| error.localizedDescription().to_string())
    }

    pub fn disable() -> Result<(), String> {
        // SAFETY: as above.
        unsafe { SMAppService::mainAppService().unregisterAndReturnError() }
            .map_err(|error| error.localizedDescription().to_string())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{
        APPMODEL_ERROR_NO_PACKAGE, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS,
    };
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
        RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    use windows::ApplicationModel::{StartupTask, StartupTaskState};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::core::HSTRING;

    use super::{AUTOSTART_ARG, AutostartStatus};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// Task Manager and Settings record their on/off choice for `Run` entries
    /// here: the first byte is even when enabled and odd when disabled.
    const APPROVED_KEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const VALUE: &str = "CayenChat";

    /// How this process registers itself, chosen at run time so one
    /// executable works both packaged and unpackaged.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Backend {
        /// MSIX / Store: the manifest's WinRT `StartupTask`.
        StartupTask,
        RegistryRun,
    }

    fn backend() -> Backend {
        let mut length = 0u32;
        // SAFETY: a null buffer with length 0 only asks for the needed size.
        let code = unsafe { GetCurrentPackageFullName(&mut length, std::ptr::null_mut()) };
        if code == APPMODEL_ERROR_NO_PACKAGE {
            Backend::RegistryRun
        } else {
            Backend::StartupTask
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(Some(0))
            .collect()
    }

    struct Key(HKEY);

    impl Key {
        fn open(path: &str, access: u32) -> Result<Option<Self>, String> {
            let mut key: HKEY = std::ptr::null_mut();
            // SAFETY: `path` is NUL-terminated and `key` is a valid out pointer.
            let code = unsafe {
                RegOpenKeyExW(HKEY_CURRENT_USER, wide(path).as_ptr(), 0, access, &mut key)
            };
            match code {
                ERROR_SUCCESS => Ok(Some(Self(key))),
                ERROR_FILE_NOT_FOUND => Ok(None),
                code => Err(format!("registry error {code}")),
            }
        }

        fn first_byte(&self) -> Result<Option<u8>, String> {
            let mut buffer = [0u8; 16];
            let mut size = buffer.len() as u32;
            // SAFETY: the buffer and its size describe the same memory.
            let code = unsafe {
                RegQueryValueExW(
                    self.0,
                    wide(VALUE).as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    buffer.as_mut_ptr(),
                    &mut size,
                )
            };
            match code {
                ERROR_SUCCESS | 234 /* ERROR_MORE_DATA */ => Ok(Some(buffer[0])),
                ERROR_FILE_NOT_FOUND => Ok(None),
                code => Err(format!("registry error {code}")),
            }
        }

        fn delete(&self) -> Result<(), String> {
            // SAFETY: the value name is NUL-terminated.
            match unsafe { RegDeleteValueW(self.0, wide(VALUE).as_ptr()) } {
                ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
                code => Err(format!("registry error {code}")),
            }
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by `open` and is closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    /// The `StartupTask` the package manifest declares, with
    /// `Parameters="--autostart"` so the launch carries the flag.
    const TASK_ID: &str = "CayenChat";

    /// Runs a WinRT call on a thread of its own: blocking on an async
    /// operation from the UI thread's apartment can deadlock.
    fn winrt<T: Send + 'static>(
        call: impl FnOnce() -> windows::core::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        std::thread::spawn(move || {
            // SAFETY: initializes this fresh thread; a repeat or a mode
            // clash only means COM is already usable here.
            let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
            call().map_err(|error| error.message())
        })
        .join()
        .map_err(|_| "the startup task call panicked".to_string())?
    }

    fn task_status(state: StartupTaskState) -> AutostartStatus {
        match state {
            StartupTaskState::Enabled | StartupTaskState::EnabledByPolicy => {
                AutostartStatus::Enabled
            }
            StartupTaskState::Disabled => AutostartStatus::Disabled,
            StartupTaskState::DisabledByUser => AutostartStatus::DisabledByUser,
            // Group policy forbids it (or the state is one we do not know).
            _ => AutostartStatus::Unavailable,
        }
    }

    fn task_state() -> Result<StartupTaskState, String> {
        winrt(|| {
            StartupTask::GetAsync(&HSTRING::from(TASK_ID))?
                .get()?
                .State()
        })
    }

    pub fn status() -> Result<AutostartStatus, String> {
        if backend() == Backend::StartupTask {
            return task_state().map(task_status);
        }
        let registered = match Key::open(RUN_KEY, KEY_READ)? {
            Some(key) => key.first_byte()?.is_some(),
            None => false,
        };
        if !registered {
            return Ok(AutostartStatus::Disabled);
        }
        let approval = match Key::open(APPROVED_KEY, KEY_READ)? {
            Some(key) => key.first_byte()?,
            None => None,
        };
        Ok(match approval {
            Some(byte) if byte & 1 == 1 => AutostartStatus::DisabledByUser,
            _ => AutostartStatus::Enabled,
        })
    }

    pub fn enable() -> Result<(), String> {
        if backend() == Backend::StartupTask {
            // Never re-enables a task the user turned off in Windows settings:
            // the request then returns `DisabledByUser` and we report it.
            let state = winrt(|| {
                StartupTask::GetAsync(&HSTRING::from(TASK_ID))?
                    .get()?
                    .RequestEnableAsync()?
                    .get()
            })?;
            return match task_status(state) {
                AutostartStatus::Enabled => Ok(()),
                AutostartStatus::DisabledByUser => Err(
                    "it is turned off in Windows settings (Apps > Startup); turn it on there"
                        .into(),
                ),
                _ => Err("Windows did not enable the startup task".into()),
            };
        }
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let command = format!("\"{}\" {AUTOSTART_ARG}", executable.display());
        let data = wide(&command);
        let key = Key::open(RUN_KEY, KEY_SET_VALUE)?.ok_or("the Run key is missing")?;
        // SAFETY: `data` is NUL-terminated UTF-16 and the size counts its bytes.
        let code = unsafe {
            RegSetValueExW(
                key.0,
                wide(VALUE).as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if code != ERROR_SUCCESS {
            return Err(format!("registry error {code}"));
        }
        // Choosing it here is an explicit request, so clear an old "disabled".
        if let Some(approved) = Key::open(APPROVED_KEY, KEY_SET_VALUE)? {
            approved.delete()?;
        }
        Ok(())
    }

    pub fn disable() -> Result<(), String> {
        if backend() == Backend::StartupTask {
            return winrt(|| {
                StartupTask::GetAsync(&HSTRING::from(TASK_ID))?
                    .get()?
                    .Disable()
            });
        }
        if let Some(key) = Key::open(RUN_KEY, KEY_SET_VALUE)? {
            key.delete()?;
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    use super::AutostartStatus;

    pub fn status() -> Result<AutostartStatus, String> {
        Ok(AutostartStatus::Unavailable)
    }

    pub fn enable() -> Result<(), String> {
        Err("not supported on this platform".into())
    }

    pub fn disable() -> Result<(), String> {
        Err("not supported on this platform".into())
    }
}
