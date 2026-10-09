//! Starting CayenChat when the user logs in (#153).
//!
//! The operating system's registration is the only truth: nothing is stored
//! in `settings.json`, and the settings window asks `status` whenever it is
//! shown. Each platform uses its standard mechanism: XDG Autostart on Linux,
//! `SMAppService` on macOS and the HKCU `Run` key on Windows. An entry the
//! user turned off in the system's own settings is reported as
//! `DisabledByUser` and is only changed again by an explicit choice here.
//!
//! Registered launches pass `--autostart`, so they can be told from a launch
//! by hand. On Windows a launch by hand brings its window to the front
//! (#259); a registered one does not take the foreground at login.

/// The argument given to a launch made by the login registration. macOS's
/// login item has no arguments.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub const AUTOSTART_ARG: &str = "--autostart";

/// Whether this process was started by the login registration.
#[cfg(target_os = "windows")]
pub fn launched_by_registration() -> bool {
    std::env::args().skip(1).any(|arg| arg == AUTOSTART_ARG)
}

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

/// These call into the system and may wait on it, so the settings window
/// runs them off the UI thread.
#[cfg(not(test))]
pub fn status() -> Result<AutostartStatus, String> {
    platform::status()
}

#[cfg(not(test))]
pub fn enable() -> Result<(), String> {
    platform::enable()
}

#[cfg(not(test))]
pub fn disable() -> Result<(), String> {
    platform::disable()
}

/// Whether a `DisabledByUser` entry can only be turned back on in the
/// system's own settings, not by choosing it here.
pub fn reenable_in_system_settings() -> bool {
    platform::reenable_in_system_settings()
}

/// Stands in for the system in the UI tests.
#[cfg(test)]
pub mod fake {
    use std::sync::Mutex;

    use super::AutostartStatus;

    pub static STATE: Mutex<Result<AutostartStatus, String>> =
        Mutex::new(Ok(AutostartStatus::Disabled));
    pub static FAIL_CHANGES: Mutex<bool> = Mutex::new(false);
}

#[cfg(test)]
pub fn status() -> Result<AutostartStatus, String> {
    fake::STATE.lock().unwrap().clone()
}

#[cfg(test)]
pub fn enable() -> Result<(), String> {
    change(AutostartStatus::Enabled)
}

#[cfg(test)]
pub fn disable() -> Result<(), String> {
    change(AutostartStatus::Disabled)
}

#[cfg(test)]
fn change(to: AutostartStatus) -> Result<(), String> {
    if *fake::FAIL_CHANGES.lock().unwrap() {
        return Err("refused".into());
    }
    *fake::STATE.lock().unwrap() = Ok(to);
    Ok(())
}

#[cfg_attr(test, allow(dead_code))]
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

    /// Quotes one `Exec` argument as the Desktop Entry specification asks,
    /// then escapes the result as a string value (the spec applies that
    /// first when reading, so backslashes must be doubled again).
    fn quote(argument: &str) -> String {
        let mut escaped = String::new();
        for c in quote_argument(argument).chars() {
            match c {
                '\\' => escaped.push_str("\\\\"),
                '\n' => escaped.push_str("\\n"),
                '\t' => escaped.push_str("\\t"),
                '\r' => escaped.push_str("\\r"),
                _ => escaped.push(c),
            }
        }
        escaped
    }

    fn quote_argument(argument: &str) -> String {
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

    /// GIO refuses an entry whose `Exec` path contains `%` even when it is
    /// written as `%%`, so such a registration would show as on but never
    /// start. A path that is not UTF-8 cannot be written to the entry
    /// without changing it. Refuse both up front instead.
    fn check_executable(executable: &Path) -> Result<(), String> {
        let Some(text) = executable.to_str() else {
            return Err(format!(
                "desktop autostart cannot start {} because its path is not valid UTF-8",
                executable.display()
            ));
        };
        if text.contains('%') {
            return Err(format!(
                "desktop autostart cannot start {} because its path contains '%'",
                executable.display()
            ));
        }
        Ok(())
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
    /// gains `Hidden=true` or `X-GNOME-Autostart-enabled=false`. A file that
    /// is not a usable application entry (empty, cut short, another type)
    /// is not a registration.
    fn parse(text: &str) -> AutostartStatus {
        let mut in_entry = false;
        let mut has_entry = false;
        let mut application = false;
        let mut has_exec = false;
        let mut off = false;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                has_entry |= in_entry;
                continue;
            }
            let Some((key, value)) = in_entry.then(|| line.split_once('=')).flatten() else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "Type" => application = value == "Application",
                "Exec" => has_exec = !value.is_empty(),
                "Hidden" => off |= value.eq_ignore_ascii_case("true"),
                "X-GNOME-Autostart-enabled" => off |= value.eq_ignore_ascii_case("false"),
                _ => {}
            }
        }
        if has_entry && off {
            AutostartStatus::DisabledByUser
        } else if has_entry && application && has_exec {
            AutostartStatus::Enabled
        } else {
            AutostartStatus::Disabled
        }
    }

    /// More than any real entry; a larger file is not read.
    const MAX_ENTRY_BYTES: u64 = 64 * 1024;

    pub fn status() -> Result<AutostartStatus, String> {
        use std::io::Read;

        let file = match std::fs::File::open(entry_path()?) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AutostartStatus::Disabled);
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_ENTRY_BYTES {
            return Ok(AutostartStatus::Disabled);
        }
        Ok(parse(&String::from_utf8_lossy(&bytes)))
    }

    /// Replaces the entry through a temporary file, so a failed write leaves
    /// the old registration as it was.
    fn write_entry(path: &Path, text: &str) -> std::io::Result<()> {
        let temporary = path.with_extension(format!("desktop.tmp{}", std::process::id()));
        let result =
            std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub fn enable() -> Result<(), String> {
        let path = entry_path()?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        check_executable(&executable)?;
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        write_entry(&path, &contents(&executable)).map_err(|error| error.to_string())
    }

    /// Choosing it here rewrites the entry, which clears the desktop's flag.
    pub fn reenable_in_system_settings() -> bool {
        false
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
        fn exec_line_escapes_special_characters_as_a_string_value() {
            let text = contents(Path::new("/opt/Cayen$Chat/a\"b`c\\d\ne"));
            assert!(
                text.contains(
                    "Exec=\"/opt/Cayen\\\\$Chat/a\\\\\"b\\\\`c\\\\\\\\d\\ne\" --autostart\n"
                ),
                "{text}"
            );
            assert_eq!(text.lines().count(), 8);
        }

        #[test]
        fn a_path_with_a_percent_sign_is_refused() {
            assert!(check_executable(Path::new("/opt/a%b/cayenchat")).is_err());
            assert!(check_executable(Path::new("/opt/ab/cayenchat")).is_ok());
        }

        #[test]
        fn a_path_that_is_not_utf8_is_refused() {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;
            let path = PathBuf::from(OsString::from_vec(b"/opt/in\xffvalid/cayenchat".to_vec()));
            assert!(check_executable(&path).is_err());
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

        #[test]
        fn unusable_entries_are_not_a_registration() {
            for text in [
                "",
                "[Desktop Entry]\n",
                "[Desktop Entry]\nType=Application\n",
                "[Desktop Entry]\nType=Application\nExec=\n",
                "[Desktop Entry]\nType=Link\nExec=x\n",
                "Type=Application\nExec=x\n",
                "[Other]\nType=Application\nExec=x\n",
                "[Desktop Entry]\nType=Applic",
            ] {
                assert_eq!(parse(text), AutostartStatus::Disabled, "{text:?}");
            }
        }

        #[test]
        fn a_failed_replacement_keeps_the_old_entry() {
            let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/tmp")
                .join(format!("autostart-{}", std::process::id()));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("cayenchat.desktop");
            write_entry(&path, "old").unwrap();
            write_entry(&path, "new").unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
            // A directory in the way makes the rename fail; nothing is left over.
            let blocked = directory.join("blocked.desktop");
            std::fs::create_dir(&blocked).unwrap();
            std::fs::write(blocked.join("keep"), "x").unwrap();
            assert!(write_entry(&blocked, "text").is_err());
            assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
            std::fs::remove_dir_all(&directory).unwrap();
        }
    }
}

#[cfg_attr(test, allow(dead_code))]
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

    /// An entry waiting for approval is only approved in System Settings.
    pub fn reenable_in_system_settings() -> bool {
        true
    }

    pub fn disable() -> Result<(), String> {
        // SAFETY: as above.
        unsafe { SMAppService::mainAppService().unregisterAndReturnError() }
            .map_err(|error| error.localizedDescription().to_string())
    }
}

#[cfg_attr(test, allow(dead_code))]
#[cfg(target_os = "windows")]
mod platform {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{
        APPMODEL_ERROR_NO_PACKAGE, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS,
    };
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegCreateKeyExW,
        RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    use windows::ApplicationModel::{StartupTask, StartupTaskState};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
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

        /// Opens the key, creating it when it does not exist.
        fn create(path: &str, access: u32) -> Result<Self, String> {
            let mut key: HKEY = std::ptr::null_mut();
            // SAFETY: `path` is NUL-terminated and `key` is a valid out pointer.
            let code = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    wide(path).as_ptr(),
                    0,
                    std::ptr::null(),
                    0,
                    access,
                    std::ptr::null(),
                    &mut key,
                    std::ptr::null_mut(),
                )
            };
            match code {
                ERROR_SUCCESS => Ok(Self(key)),
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

    /// Runs a WinRT call on a thread of its own (an MTA, where blocking on an
    /// async operation is allowed). Callers are already off the UI thread.
    fn winrt<T: Send + 'static>(
        call: impl FnOnce() -> windows::core::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        std::thread::spawn(move || {
            // SAFETY: initializes this fresh thread, undone below.
            unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|error| error.message())?;
            let result = call().map_err(|error| error.message());
            // SAFETY: pairs with the successful `RoInitialize` above.
            unsafe { RoUninitialize() };
            result
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

    /// Writes the value under `path`, creating the key first: a fresh user
    /// profile may not have a `Run` key yet.
    fn register(path: &str, command: &str) -> Result<(), String> {
        let data = wide(command);
        let key = Key::create(path, KEY_SET_VALUE)?;
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
        Ok(())
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
        register(RUN_KEY, &command)?;
        // Choosing it here is an explicit request, so clear an old "disabled".
        if let Some(approved) = Key::open(APPROVED_KEY, KEY_SET_VALUE)? {
            approved.delete()?;
        }
        Ok(())
    }

    /// A disabled `StartupTask` is only turned back on in Windows settings;
    /// the `Run` entry is rewritten by choosing it here.
    pub fn reenable_in_system_settings() -> bool {
        static PACKAGED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *PACKAGED.get_or_init(|| backend() == Backend::StartupTask)
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

    #[cfg(test)]
    mod tests {
        use windows_sys::Win32::System::Registry::RegDeleteKeyW;

        use super::*;

        #[test]
        fn register_creates_a_missing_key() {
            let path = format!(r"Software\CayenChatTest\autostart-{}", std::process::id());
            assert!(Key::open(&path, KEY_READ).unwrap().is_none());

            register(&path, r#""C:\cayenchat.exe" --autostart"#).unwrap();
            let registered = Key::open(&path, KEY_READ).unwrap().unwrap();
            assert!(registered.first_byte().unwrap().is_some());
            drop(registered);

            // SAFETY: the paths are NUL-terminated.
            unsafe {
                RegDeleteKeyW(HKEY_CURRENT_USER, wide(&path).as_ptr());
                RegDeleteKeyW(HKEY_CURRENT_USER, wide(r"Software\CayenChatTest").as_ptr());
            }
        }
    }
}

#[cfg_attr(test, allow(dead_code))]
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    use super::AutostartStatus;

    pub fn status() -> Result<AutostartStatus, String> {
        Ok(AutostartStatus::Unavailable)
    }

    pub fn enable() -> Result<(), String> {
        Err("not supported on this platform".into())
    }

    pub fn reenable_in_system_settings() -> bool {
        false
    }

    pub fn disable() -> Result<(), String> {
        Err("not supported on this platform".into())
    }
}
