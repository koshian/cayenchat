//! The settings file as the settings window reads and writes it. Outside
//! tests this is `cayenchat_storage`'s file. A test never touches the user's
//! own file: unless it holds a `TestFile`, there is no file and saving does
//! nothing.

use cayenchat_storage::Settings;

#[cfg(not(test))]
pub(crate) fn load() -> Result<Option<Settings>, String> {
    cayenchat_storage::load()
}

#[cfg(not(test))]
pub(crate) fn save(settings: &Settings) -> Result<(), String> {
    cayenchat_storage::save(settings)
}

#[cfg(test)]
thread_local! {
    /// `None` while no test holds a file; then the file's content.
    static FILE: std::cell::RefCell<Option<Settings>> = const { std::cell::RefCell::new(None) };
    static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn load() -> Result<Option<Settings>, String> {
    Ok(FILE.with_borrow(Clone::clone))
}

#[cfg(test)]
pub(crate) fn save(settings: &Settings) -> Result<(), String> {
    if ENABLED.get() {
        FILE.with_borrow_mut(|file| *file = Some(settings.clone()));
    }
    Ok(())
}

/// The in-memory file of the running test, gone again when dropped.
#[cfg(test)]
pub(crate) struct TestFile;

#[cfg(test)]
impl TestFile {
    pub(crate) fn with(settings: &Settings) -> Self {
        ENABLED.set(true);
        let _ = save(settings);
        Self
    }
}

#[cfg(test)]
impl Drop for TestFile {
    fn drop(&mut self) {
        ENABLED.set(false);
        FILE.with_borrow_mut(|file| *file = None);
    }
}

#[cfg(test)]
mod tests {
    use cayenchat_storage::{Appearance, Secret, Settings, ThemeMode};
    use gpui::{Focusable, TestAppContext};

    use super::TestFile;
    use crate::{SettingsWindow, secrets, settings_with_channels};

    fn open<'a>(
        settings: &Settings,
        cx: &'a mut TestAppContext,
    ) -> (
        gpui::Entity<SettingsWindow>,
        &'a mut gpui::VisualTestContext,
    ) {
        cx.update(|cx| {
            secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &Appearance::default(),
            ));
        });
        let owner = cx.add_window(|window, cx| {
            crate::ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let (form, cx) = cx
            .add_window_view(|window, cx| SettingsWindow::new(owner, settings.clone(), window, cx));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (form, cx)
    }

    #[gpui::test]
    fn the_auto_join_dialog_edits_toggles_reorders_and_deletes(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a,-#b,#c");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let channels = |form: &SettingsWindow, cx: &gpui::App| {
            form.settings.channels.read(cx).text().to_owned()
        };

        form.update_in(cx, |form, window, cx| form.open_auto_join(window, cx));
        form.update(cx, |form, cx| {
            form.edit_auto_join(|rows| rows[1].enabled = true, cx);
            form.edit_auto_join(|rows| rows[0].enabled = false, cx);
            form.edit_auto_join(|rows| rows.swap(0, 2), cx);
            assert_eq!(channels(form, cx), "#c,#b,-#a");
            form.edit_auto_join(
                |rows| {
                    rows.remove(1);
                },
                cx,
            );
            assert_eq!(channels(form, cx), "#c,-#a");
            let name = form.auto_join.as_ref().unwrap().rows[0].name.clone();
            name.update(cx, |name, cx| name.set_text("#renamed", cx));
        });
        cx.run_until_parked();
        form.update(cx, |form, cx| {
            assert_eq!(channels(form, cx), "#renamed,-#a");
            form.autosave_now(None, cx);
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_profile().unwrap().channels, "#renamed,-#a");
        drop(file);
    }

    #[gpui::test]
    fn the_auto_join_dialog_keeps_the_focus_away_from_the_form(cx: &mut TestAppContext) {
        let settings = settings_with_channels("");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let nickname = form.read_with(cx, |form, _| form.settings.nickname.clone());
        let text = |cx: &gpui::App| nickname.read(cx).text().to_owned();
        let before = cx.update(|_, cx| text(cx));
        cx.update(|window, cx| window.focus(&nickname.read(cx).focus_handle(cx)));
        form.update_in(cx, |form, window, cx| form.open_auto_join(window, cx));
        cx.simulate_input("X");
        assert_eq!(cx.update(|_, cx| text(cx)), before);

        // Tab and Shift+Tab stay inside the dialog, even with no rows.
        for keys in ["tab", "shift-tab", "tab", "tab"] {
            cx.simulate_keystrokes(keys);
            cx.simulate_input("X");
            let inside = form.update_in(cx, |form, window, cx| {
                form.auto_join
                    .as_ref()
                    .is_some_and(|dialog| dialog.focus.contains_focused(window, cx))
            });
            assert!(inside, "{keys} left the dialog");
        }
        assert_eq!(cx.update(|_, cx| text(cx)), before);
        drop(file);
    }

    #[gpui::test]
    fn another_processs_change_is_read_back_instead_of_overwritten(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);

        // Another process turns a notification off while this window is in
        // the background.
        cx.deactivate_window();
        let mut elsewhere = settings.clone();
        elsewhere.notifications.mentions = false;
        super::save(&elsewhere).unwrap();
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(form.read_with(cx, |form, _| !form.settings.values.notifications.mentions));

        // An edit here is saved on top of it, not of the old copy.
        form.update(cx, |form, cx| {
            form.settings.values.appearance.alternate_rows = true;
            form.autosave_now(None, cx);
        });
        let saved = super::load().unwrap().unwrap();
        assert!(!saved.notifications.mentions);
        assert!(saved.appearance.alternate_rows);
        drop(file);
    }

    #[gpui::test]
    fn another_processs_theme_and_provider_reach_the_chat_window(cx: &mut TestAppContext) {
        let mut settings = settings_with_channels("#a");
        settings.theme = ThemeMode::Light;
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let owner = form.read_with(cx, |form, _| form.owner);

        // Another process changes the theme and the image host while this
        // window is in the background.
        cx.deactivate_window();
        let mut elsewhere = settings.clone();
        elsewhere.theme = ThemeMode::Dark;
        elsewhere.image_upload.provider = Some("elsewhere".into());
        super::save(&elsewhere).unwrap();
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let (mode, provider) = cx.update(|_, app| {
            let chat = owner.read(app).unwrap();
            (chat.theme_mode, chat.image_provider.clone())
        });
        assert_eq!(mode, ThemeMode::Dark);
        assert_eq!(provider.as_deref(), Some("elsewhere"));

        // An edit here must not leave the chat window on the older values:
        // the next save compares with what was read back.
        form.update(cx, |form, cx| {
            form.settings.values.appearance.alternate_rows = true;
            form.autosave_now(None, cx);
        });
        let (mode, provider) = cx.update(|_, app| {
            let chat = owner.read(app).unwrap();
            (chat.theme_mode, chat.image_provider.clone())
        });
        assert_eq!(mode, ThemeMode::Dark);
        assert_eq!(provider.as_deref(), Some("elsewhere"));
        drop(file);
    }

    #[gpui::test]
    fn leaving_the_window_saves_what_is_pending(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);

        form.update(cx, |form, cx| {
            form.settings.values.notifications.private_messages = false;
            cx.notify();
        });
        assert!(
            super::load()
                .unwrap()
                .unwrap()
                .notifications
                .private_messages
        );
        cx.deactivate_window();
        assert!(
            !super::load()
                .unwrap()
                .unwrap()
                .notifications
                .private_messages
        );
        drop(file);
    }

    #[gpui::test]
    fn a_server_another_process_added_keeps_its_password(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);

        // Another process adds a server and stores its password; this window
        // never saw either.
        let mut elsewhere = settings.clone();
        let added = elsewhere.add_server("irc.example.org").clone();
        elsewhere.selected_server = settings.selected_server.clone();
        super::save(&elsewhere).unwrap();
        let store = cx.update(|_, cx| secrets::store(cx));
        store
            .set(&added.server_password_key(), &Secret::new("secret"))
            .unwrap();

        form.update(cx, |form, cx| {
            form.settings.values.appearance.alternate_rows = true;
            form.autosave_now(None, cx);
        });
        assert!(store.contains(&added.server_password_key()).unwrap());
        drop(file);
    }
}
