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
    use crate::{SettingsWindow, secrets, settings_window::Credential, settings_with_channels};

    /// A server whose identity is complete, so Connect gets past validation.
    fn connectable(channels: &str) -> Settings {
        let mut settings = settings_with_channels(channels);
        let profile = settings.selected_profile_mut().unwrap();
        profile.nickname = "nick".into();
        profile.username = "user".into();
        profile.use_tls = true;
        settings
    }

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
            form.save_servers(cx);
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
    fn server_edits_wait_for_save_while_other_settings_autosave(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);

        form.update(cx, |form, cx| {
            form.settings
                .channels
                .update(cx, |field, cx| field.set_text("#b", cx));
            form.settings.values.notifications.private_messages = false;
            form.autosave_now(None, cx);
            assert!(form.servers_unsaved(cx));
        });
        let saved = super::load().unwrap().unwrap();
        assert!(!saved.notifications.private_messages);
        assert_eq!(saved.selected_profile().unwrap().channels, "#a");

        form.update(cx, |form, cx| {
            form.save_servers(cx);
            assert!(!form.servers_unsaved(cx));
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_profile().unwrap().channels, "#b");
        drop(file);
    }

    #[gpui::test]
    fn an_unfinished_port_does_not_stop_other_settings_from_autosaving(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.settings
                .port
                .update(cx, |field, cx| field.set_text("bad", cx));
            form.settings.values.notifications.private_messages = false;
            form.autosave_now(None, cx);
            assert!(form.autosave_error.is_none());
        });
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
    fn an_unsaved_selection_is_not_saved_by_a_readback(cx: &mut TestAppContext) {
        let mut settings = settings_with_channels("#a");
        let a_id = settings.selected_server.clone();
        let b_id = settings.add_server("b.example").id.clone();
        settings.selected_server = a_id.clone();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.select_server(b_id.clone(), cx);
            assert!(form.servers_unsaved(cx));
            form.settings.values.notifications.private_messages = false;
            form.autosave_now(None, cx);
            assert!(form.servers_unsaved(cx));
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.selected_server, a_id);
        assert!(!saved.notifications.private_messages);
        drop(file);
    }

    #[gpui::test]
    fn removing_the_last_server_is_saved_explicitly(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.settings.values.remove_selected_server();
            form.settings
                .show_selected(&crate::secrets::store(cx), &form.i18n, cx);
            assert!(form.servers_unsaved(cx));
            form.save_servers(cx);
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

            // Another setting autosaves without the draft.
            super::fail_saves(false);
            form.settings.values.notifications.private_messages = false;
            form.autosave_now(None, cx);
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
    fn a_typed_password_is_stored_only_by_its_own_save(cx: &mut TestAppContext) {
        let settings = connectable("#a");
        let key = settings.selected_profile().unwrap().server_password_key();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let store = cx.update(|_, cx| secrets::store(cx));

        // Typing, autosave and Save of the server never store it.
        form.update(cx, |form, cx| {
            form.settings
                .server_password
                .update(cx, |field, cx| field.set_text("typed", cx));
            form.autosave_now(None, cx);
            form.save_servers(cx);
            assert!(!form.servers_unsaved(cx), "a typed password is not an edit");
        });
        assert!(!store.contains(&key).unwrap());
        // Connect would still use it, for that connection alone.
        form.update(cx, |form, cx| {
            let settings = form.settings.snapshot(cx).unwrap();
            let (config, missing) = form
                .settings
                .connection_config(&settings, &store, &form.i18n, cx)
                .unwrap();
            assert_eq!(config.server_password.as_deref(), Some("typed"));
            assert!(!missing.any());
        });

        form.update_in(cx, |form, window, cx| {
            form.save_credential(Credential::Server, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "typed");
        form.update(cx, |form, cx| {
            assert!(form.settings.saved_server_password);
            assert_eq!(form.settings.server_password.read(cx).text(), "");
            assert_eq!(
                form.settings.credential_state(Credential::Server),
                (true, false)
            );
        });
        drop(file);
    }

    #[gpui::test]
    fn changing_a_saved_password_keeps_it_until_the_new_one_is_saved(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let key = settings.selected_profile().unwrap().server_password_key();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let store = cx.update(|_, cx| secrets::store(cx));
        store.set(&key, &Secret::new("old")).unwrap();
        form.update(cx, |form, cx| {
            form.settings.show_selected(&store, &form.i18n, cx);
            assert_eq!(
                form.settings.credential_state(Credential::Server),
                (true, false)
            );
        });

        // Change, then Cancel: nothing happened to the saved value.
        form.update_in(cx, |form, window, cx| {
            form.change_credential(Credential::Server, window, cx)
        });
        form.update(cx, |form, cx| {
            assert_eq!(
                form.settings.credential_state(Credential::Server),
                (true, true)
            );
            form.settings
                .server_password
                .update(cx, |field, cx| field.set_text("half", cx));
            form.cancel_credential_change(Credential::Server, cx);
            assert_eq!(form.settings.server_password.read(cx).text(), "");
        });
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "old");

        // Change and Save replaces it.
        form.update_in(cx, |form, window, cx| {
            form.change_credential(Credential::Server, window, cx);
            form.settings
                .server_password
                .update(cx, |field, cx| field.set_text("new", cx));
            form.save_credential(Credential::Server, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "new");

        // A failed Save keeps what was saved and says so.
        form.update_in(cx, |form, window, cx| {
            form.change_credential(Credential::Server, window, cx);
            form.settings
                .server_password
                .update(cx, |field, cx| field.set_text("lost", cx));
        });
        cx.update(|_, cx| {
            cx.set_global(secrets::Credentials(CredentialStore::with_backend(
                Arc::new(MemoryBackend::unavailable(CredentialBackendKind::System)),
            )));
        });
        form.update_in(cx, |form, window, cx| {
            form.save_credential(Credential::Server, window, cx);
        });
        cx.run_until_parked();
        form.update(cx, |form, _| {
            assert!(form.feedback.is_some());
            assert_eq!(
                form.settings.credential_state(Credential::Server),
                (true, true),
                "still being changed"
            );
        });
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "new");
        drop(file);
    }

    #[gpui::test]
    fn an_unfinished_change_is_dropped_when_the_server_is_left(cx: &mut TestAppContext) {
        let mut settings = connectable("#a");
        let profile = settings.selected_profile_mut().unwrap();
        profile.use_tls = true;
        profile.sasl_enabled = true;
        profile.sasl_username = "account".into();
        let (a_id, key, sasl_key) = (
            profile.id.clone(),
            profile.server_password_key(),
            profile.sasl_password_key(),
        );
        let b_id = settings.add_server("b.example").id.clone();
        settings.selected_server = a_id.clone();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let store = cx.update(|_, cx| secrets::store(cx));
        store.set(&key, &Secret::new("old")).unwrap();
        store.set(&sasl_key, &Secret::new("old-sasl")).unwrap();
        form.update_in(cx, |form, window, cx| {
            form.show_selected_server(cx);
            for which in [Credential::Server, Credential::Sasl] {
                form.change_credential(which, window, cx);
                form.settings
                    .credential_field(which)
                    .update(cx, |field, cx| field.set_text("half", cx));
            }
            // Leaving the server and coming back.
            form.select_server(b_id.clone(), cx);
            form.select_server(a_id.clone(), cx);
            assert_eq!(form.settings.values.selected_server, a_id);
            let settings = form.settings.snapshot(cx).unwrap();
            let (config, _) = form
                .settings
                .connection_config(&settings, &store, &form.i18n, cx)
                .unwrap();
            assert_eq!(config.server_password.as_deref(), Some("old"));
            assert_eq!(
                config.sasl.as_ref().map(|sasl| sasl.password.as_str()),
                Some("old-sasl")
            );
        });
        drop(file);
    }

    #[gpui::test]
    fn an_unsaved_server_cannot_save_a_password_yet(cx: &mut TestAppContext) {
        let saved = settings_with_channels("#a");
        let file = TestFile::with(&saved);
        let (form, cx) = open(&saved, cx);
        let store = cx.update(|_, cx| secrets::store(cx));
        let new_id = form.update(cx, |form, cx| {
            let id = form.settings.values.add_server("b.example").id.clone();
            form.show_selected_server(cx);
            id
        });
        form.update_in(cx, |form, window, cx| {
            form.settings
                .server_password
                .update(cx, |field, cx| field.set_text("typed", cx));
            form.save_credential(Credential::Server, window, cx);
            assert!(form.feedback.is_some());
        });
        cx.run_until_parked();
        assert!(
            !store
                .contains(&cayenchat_storage::SecretKey::server_password(&new_id))
                .unwrap()
        );
        drop(file);
    }

    #[gpui::test]
    fn delete_removes_a_saved_password_and_a_failure_keeps_it(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let key = settings.selected_profile().unwrap().server_password_key();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let working = cx.update(|_, cx| secrets::store(cx));
        working.set(&key, &Secret::new("secret")).unwrap();
        form.update(cx, |form, cx| {
            form.settings.show_selected(&working, &form.i18n, cx)
        });

        cx.update(|_, cx| {
            cx.set_global(secrets::Credentials(CredentialStore::with_backend(
                Arc::new(MemoryBackend::unavailable(CredentialBackendKind::System)),
            )));
        });
        form.update(cx, |form, cx| {
            form.delete_credential(Credential::Server, cx);
        });
        cx.run_until_parked();
        form.update(cx, |form, _| {
            assert!(form.feedback.is_some(), "the failed deletion is shown");
            assert!(form.settings.saved_server_password, "still shown as saved");
        });
        assert!(working.contains(&key).unwrap());

        cx.update(|_, cx| cx.set_global(secrets::Credentials(working.clone())));
        form.update(cx, |form, cx| {
            form.delete_credential(Credential::Server, cx);
        });
        cx.run_until_parked();
        form.update(cx, |form, cx| {
            let _ = cx;
            assert!(!form.settings.saved_server_password);
            assert_eq!(
                form.settings.credential_state(Credential::Server),
                (false, false)
            );
        });
        assert!(!working.contains(&key).unwrap());
        drop(file);
    }
}
