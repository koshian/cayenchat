// SPDX-License-Identifier: GPL-3.0-only
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

/// Replaces the file with `settings` after copying the old one beside it;
/// returns where the copy is (`None` when there was no file).
#[cfg(not(test))]
pub(crate) fn replace(settings: &Settings) -> Result<Option<std::path::PathBuf>, String> {
    cayenchat_storage::transfer::replace_settings(settings)
}

#[cfg(test)]
thread_local! {
    /// `None` while no test holds a file; then the file's content.
    static FILE: std::cell::RefCell<Option<Settings>> = const { std::cell::RefCell::new(None) };
    static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAILING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static UNREADABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes saving fail (or work again) to test what a failed save leaves.
#[cfg(test)]
pub(crate) fn fail_saves(failing: bool) {
    FAILING.set(failing);
}

/// Makes loading fail (or work again), as an unsupported or damaged file does.
#[cfg(test)]
pub(crate) fn fail_loads(failing: bool) {
    UNREADABLE.set(failing);
}

#[cfg(test)]
pub(crate) fn load() -> Result<Option<Settings>, String> {
    if UNREADABLE.get() {
        return Err("cannot read the settings file".into());
    }
    Ok(FILE.with_borrow(Clone::clone))
}

#[cfg(test)]
pub(crate) fn save(settings: &Settings) -> Result<(), String> {
    if FAILING.get() {
        return Err("cannot write the settings file".into());
    }
    if ENABLED.get() {
        FILE.with_borrow_mut(|file| *file = Some(settings.clone()));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn replace(settings: &Settings) -> Result<Option<std::path::PathBuf>, String> {
    let backup = FILE
        .with_borrow(Clone::clone)
        .map(|_| "settings-backup.json".into());
    save(settings).map(|()| backup)
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
        FAILING.set(false);
        UNREADABLE.set(false);
        FILE.with_borrow_mut(|file| *file = None);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cayenchat_storage::credentials::MemoryBackend;
    use cayenchat_storage::{
        Appearance, CredentialBackendKind, CredentialStore, Secret, Settings, ThemeMode,
    };
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
        // Done saves the list without the Connection tab's Save button.
        form.update_in(cx, |form, window, cx| {
            assert_eq!(channels(form, cx), "#renamed,-#a");
            form.close_auto_join(window, cx);
            assert!(!form.servers_unsaved(cx));
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
    fn a_registered_servers_text_is_saved_when_committed_not_while_typing(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);

        form.update(cx, |form, cx| {
            form.settings
                .channels
                .update(cx, |field, cx| field.set_text("#b", cx));
            // The pause after typing leaves the field alone.
            form.autosave_pause_elapsed(cx);
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_profile().unwrap().channels, "#a");

        // Leaving the field (or the window) commits it, with no Save.
        form.update(cx, |form, cx| {
            form.autosave_now(None, cx);
            assert!(!form.servers_unsaved(cx));
            assert!(form.status_message().is_none());
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_profile().unwrap().channels, "#b");
        drop(file);
    }

    #[gpui::test]
    fn a_choice_on_a_registered_server_is_saved_without_save(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            let profile = form.settings.values.selected_profile_mut().unwrap();
            profile.connect_on_startup = !profile.connect_on_startup;
            form.autosave_pause_elapsed(cx);
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(
            saved.selected_profile().unwrap().connect_on_startup,
            !settings.selected_profile().unwrap().connect_on_startup
        );
        drop(file);
    }

    #[gpui::test]
    fn invalid_server_text_keeps_the_saved_server_and_the_typed_text(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let host = settings.selected_profile().unwrap().host.clone();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.settings
                .port
                .update(cx, |field, cx| field.set_text("bad", cx));
            form.settings
                .channels
                .update(cx, |field, cx| field.set_text("#b", cx));
            form.settings.values.notifications.private_messages = false;
            form.autosave_now(None, cx);
            // The bad port is reported, the text stays, nothing of the
            // server was written; other settings still were.
            assert!(form.field_error.is_some());
            assert!(form.status_message().is_some());
            assert_eq!(form.settings.port.read(cx).text(), "bad");
            assert!(form.autosave_error.is_none());
        });
        let saved = super::load().unwrap().unwrap();
        assert!(!saved.notifications.private_messages);
        assert_eq!(saved.selected_profile().unwrap().channels, "#a");

        // An empty hostname is invalid too and never drops the server.
        form.update(cx, |form, cx| {
            form.settings
                .port
                .update(cx, |field, cx| field.set_text("6667", cx));
            form.settings
                .custom_host
                .update(cx, |field, cx| field.set_text("", cx));
            form.autosave_now(None, cx);
            assert!(form.field_error.is_some());
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_profile().unwrap().host, host);

        // Fixing the text saves it and clears the error.
        form.update(cx, |form, cx| {
            form.settings
                .custom_host
                .update(cx, |field, cx| field.set_text("fixed.example", cx));
            form.autosave_now(None, cx);
            assert!(form.field_error.is_none());
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_profile().unwrap().host, "fixed.example");
        assert_eq!(saved.selected_profile().unwrap().channels, "#b");
        drop(file);
    }

    #[gpui::test]
    fn a_new_server_is_a_draft_until_save_succeeds(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.settings.values.add_server("new.example");
            form.show_selected_server(cx);
            assert!(!form.shown_server_registered());
            // Leaving the window or any autosave does not register it.
            form.settings.values.notifications.private_messages = false;
            form.autosave_now(None, cx);
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.servers.len(), 1);
        assert!(!saved.notifications.private_messages);

        // A failed save leaves it a draft.
        super::fail_saves(true);
        form.update(cx, |form, cx| {
            form.save_servers(cx);
            assert!(form.feedback.is_some());
            assert!(!form.shown_server_registered());
        });
        super::fail_saves(false);
        assert_eq!(super::load().unwrap().unwrap().servers.len(), 1);

        form.update(cx, |form, cx| {
            form.save_servers(cx);
            assert!(form.feedback.is_none());
            assert!(form.shown_server_registered());
            assert!(!form.servers_unsaved(cx));
        });
        assert_eq!(super::load().unwrap().unwrap().servers.len(), 2);
        drop(file);
    }

    #[gpui::test]
    fn choosing_a_registered_server_is_saved_but_a_draft_selection_is_not(cx: &mut TestAppContext) {
        let mut settings = settings_with_channels("#a");
        let a_id = settings.selected_server.clone();
        let b_id = settings.add_server("b.example").id.clone();
        settings.selected_server = a_id.clone();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.select_server(b_id.clone(), cx);
            form.autosave_now(None, cx);
        });
        assert_eq!(super::load().unwrap().unwrap().selected_server, b_id);

        form.update(cx, |form, cx| {
            form.settings.values.add_server("draft.example");
            form.show_selected_server(cx);
            form.autosave_now(None, cx);
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_server, b_id);
        assert_eq!(saved.servers.len(), 2);
        drop(file);
    }

    #[gpui::test]
    fn removing_the_last_server_is_saved(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.settings.values.remove_selected_server();
            form.show_selected_server(cx);
            form.autosave_now(None, cx);
            assert!(!form.servers_unsaved(cx));
        });
        assert!(super::load().unwrap().unwrap().servers.is_empty());
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

    #[gpui::test]
    fn a_failed_connect_save_leaves_the_draft_unsaved(cx: &mut TestAppContext) {
        // Valid identity, so Connect gets as far as saving.
        let mut settings = settings_with_channels("#a");
        let profile = settings.selected_profile_mut().unwrap();
        profile.nickname = "nick".into();
        profile.username = "user".into();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.settings
                .channels
                .update(cx, |field, cx| field.set_text("#draft", cx));
        });
        super::fail_saves(true);
        form.update_in(cx, |form, window, cx| {
            form.connect_from_settings(window, cx)
        });
        form.update(cx, |form, cx| {
            assert!(
                form.feedback
                    .as_deref()
                    .is_some_and(|text| text.contains("cannot write the settings file")),
                "the save failure is shown: {:?}",
                form.feedback
            );
            assert!(form.servers_unsaved(cx));
            assert_eq!(form.saved.selected_profile().unwrap().channels, "#a");

            // Another setting autosaves without the text still pending.
            super::fail_saves(false);
            form.settings.values.notifications.private_messages = false;
            form.autosave_pause_elapsed(cx);
        });
        let saved = super::load().unwrap().unwrap();
        assert!(!saved.notifications.private_messages);
        assert_eq!(saved.selected_profile().unwrap().channels, "#a");

        // Save tries the draft again.
        form.update(cx, |form, cx| {
            form.save_servers(cx);
            assert!(form.feedback.is_none());
            assert!(!form.servers_unsaved(cx));
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_profile().unwrap().channels, "#draft");
        drop(file);
    }

    #[gpui::test]
    fn turning_password_saving_off_keeps_the_typed_connection_fields(cx: &mut TestAppContext) {
        let mut settings = settings_with_channels("#a");
        settings.selected_profile_mut().unwrap().remember_passwords = true;
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let host = |form: &SettingsWindow, cx: &gpui::App| {
            form.settings.custom_host.read(cx).text().to_owned()
        };

        // A saved edit, then an unsaved one, survive the switch.
        form.update(cx, |form, cx| {
            form.settings
                .custom_host
                .update(cx, |field, cx| field.set_text("changed.example", cx));
            form.save_servers(cx);
            form.settings
                .channels
                .update(cx, |field, cx| field.set_text("#draft", cx));
        });
        form.update_in(cx, |form, window, cx| {
            form.toggle_remember_passwords(window, cx)
        });
        form.update(cx, |form, cx| {
            assert_eq!(host(form, cx), "changed.example");
            assert_eq!(form.settings.channels.read(cx).text(), "#draft");
            assert!(form.servers_unsaved(cx));
            form.save_servers(cx);
        });
        let saved = super::load().unwrap().unwrap();
        let profile = saved.selected_profile().unwrap();
        assert_eq!(profile.host, "changed.example");
        assert_eq!(profile.channels, "#draft");
        assert!(!profile.remember_passwords);
        drop(file);
    }

    #[gpui::test]
    fn a_failed_password_deletion_stays_unsaved_until_a_save_succeeds(cx: &mut TestAppContext) {
        let mut settings = settings_with_channels("#a");
        settings.selected_profile_mut().unwrap().remember_passwords = true;
        let key = settings.selected_profile().unwrap().server_password_key();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let working = cx.update(|_, cx| secrets::store(cx));
        working.set(&key, &Secret::new("secret")).unwrap();

        form.update_in(cx, |form, window, cx| {
            form.toggle_remember_passwords(window, cx)
        });
        cx.update(|_, cx| {
            cx.set_global(secrets::Credentials(CredentialStore::with_backend(
                Arc::new(MemoryBackend::unavailable(CredentialBackendKind::System)),
            )));
        });
        form.update(cx, |form, cx| {
            form.save_servers(cx);
            assert!(form.feedback.is_some(), "the failed deletion is shown");
            assert!(form.servers_unsaved(cx));
        });
        assert!(
            super::load()
                .unwrap()
                .unwrap()
                .selected_profile()
                .unwrap()
                .remember_passwords
        );
        assert!(working.contains(&key).unwrap());

        // Once the store works again, Save deletes the secret.
        cx.update(|_, cx| cx.set_global(secrets::Credentials(working.clone())));
        form.update(cx, |form, cx| {
            form.save_servers(cx);
            assert!(form.feedback.is_none());
            assert!(!form.servers_unsaved(cx));
        });
        assert!(!working.contains(&key).unwrap());
        assert!(
            !super::load()
                .unwrap()
                .unwrap()
                .selected_profile()
                .unwrap()
                .remember_passwords
        );
        drop(file);
    }
}
