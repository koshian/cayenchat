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
