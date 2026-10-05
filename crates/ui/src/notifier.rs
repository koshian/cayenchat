//! Desktop notifications through the operating system.
//!
//! `notify-rust` calls `org.freedesktop.Notifications` over D-Bus (zbus) on
//! Linux and BSD, `NSUserNotificationCenter` on macOS and WinRT toasts on
//! Windows. Showing a notification can block (D-Bus round trip, macOS
//! delivery confirmation), so a worker thread does it off the UI thread.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopNotification {
    pub summary: String,
    pub body: String,
    /// Also beep and flash the taskbar button (Windows only).
    pub sound: bool,
}

pub struct Notifier {
    #[cfg(not(test))]
    sender: Option<std::sync::mpsc::Sender<DesktopNotification>>,
    /// Tests record notifications instead of showing them on the desktop.
    #[cfg(test)]
    pub shown: Vec<DesktopNotification>,
}

impl Notifier {
    #[cfg(not(test))]
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<DesktopNotification>();
        let sender = std::thread::Builder::new()
            .name("notifications".into())
            .spawn(move || {
                let Some(()) = platform::prepare() else {
                    return;
                };
                for notification in receiver {
                    if let Err(error) = platform::show(&notification) {
                        log::warn!("Could not show a desktop notification: {error}");
                    }
                    if notification.sound {
                        platform::alert();
                    }
                }
            })
            .map_err(|error| log::warn!("Could not start the notification thread: {error}"))
            .ok()
            .map(|_| sender);
        Self { sender }
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Self { shown: Vec::new() }
    }

    #[cfg(not(test))]
    pub fn show(&mut self, notification: DesktopNotification) {
        if let Some(sender) = &self.sender {
            // The worker exits only when the platform cannot notify at all.
            let _ = sender.send(notification);
        }
    }

    #[cfg(test)]
    pub fn show(&mut self, notification: DesktopNotification) {
        self.shown.push(notification);
    }
}

#[cfg(not(test))]
mod platform {
    use super::DesktopNotification;

    const APP_NAME: &str = "CayenChat";

    /// Registers the sending application once. Returns `None` when this
    /// process cannot post notifications.
    #[cfg(target_os = "macos")]
    pub fn prepare() -> Option<()> {
        // Without an explicit identifier, mac-notification-sys asks AppleScript
        // for an application named "use_default" and falls back to Finder.
        // Only a bundled app has its own identifier; `cargo run` does not.
        let Some(identifier) = objc2_foundation::NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|identifier| identifier.to_string())
        else {
            log::info!(
                "Desktop notifications need the app bundle on macOS (scripts/bundle-macos.sh)."
            );
            return None;
        };
        match notify_rust::set_application(&identifier) {
            Ok(()) => Some(()),
            Err(error) => {
                log::warn!("Could not register {identifier} for notifications: {error}");
                None
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn prepare() -> Option<()> {
        Some(())
    }

    pub fn show(notification: &DesktopNotification) -> Result<(), notify_rust::error::Error> {
        let mut builder = notify_rust::Notification::new();
        builder
            .appname(APP_NAME)
            .summary(&notification.summary)
            .body(&body(&notification.body));
        #[cfg(all(unix, not(target_os = "macos")))]
        builder.hint(notify_rust::Hint::Category("im.received".into()));
        builder.show().map(|_| ())
    }

    /// The system notification sound and a taskbar flash that lasts until
    /// the window comes to the foreground.
    #[cfg(target_os = "windows")]
    pub fn alert() {
        use windows_sys::Win32::{
            Foundation::{HWND, LPARAM},
            System::{Diagnostics::Debug::MessageBeep, Threading::GetCurrentProcessId},
            UI::WindowsAndMessaging::{
                EnumWindows, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GW_OWNER,
                GetWindow, GetWindowThreadProcessId, IsWindowVisible, MB_OK,
            },
        };

        unsafe extern "system" fn flash(hwnd: HWND, process: LPARAM) -> i32 {
            // SAFETY: plain Win32 queries on a handle EnumWindows just gave us.
            unsafe {
                let mut owner = 0;
                GetWindowThreadProcessId(hwnd, &mut owner);
                // Top-level, visible windows of this process: owned windows
                // have no taskbar button of their own.
                if owner as LPARAM == process
                    && IsWindowVisible(hwnd) != 0
                    && GetWindow(hwnd, GW_OWNER).is_null()
                {
                    let info = FLASHWINFO {
                        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                        hwnd,
                        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                        uCount: 0,
                        dwTimeout: 0,
                    };
                    FlashWindowEx(&info);
                }
            }
            1
        }

        // SAFETY: both calls take no pointers owned by us beyond the callback.
        unsafe {
            MessageBeep(MB_OK);
            EnumWindows(Some(flash), GetCurrentProcessId() as LPARAM);
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn alert() {}

    /// Servers that advertise `body-markup` parse the body as a small
    /// HTML subset, so IRC text must be escaped.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn body(text: &str) -> String {
        super::escape_markup(text)
    }

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    fn body(text: &str) -> String {
        text.to_owned()
    }
}

#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    #[test]
    fn markup_is_escaped_for_freedesktop_servers() {
        assert_eq!(
            super::escape_markup("<b>a & b</b>"),
            "&lt;b&gt;a &amp; b&lt;/b&gt;"
        );
    }
}
