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

/// Orders the read-modify-write of the file inside this process. It is held
/// only for one small file operation, never across a store copy.
#[cfg(not(test))]
static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Writes `settings`, except that the credential backend stays what the file
/// holds: only `set_credential_backend` changes it (CRED-3). A snapshot taken
/// before a move of the secrets therefore cannot write the old store back.
#[cfg(not(test))]
pub(crate) fn save(settings: &Settings) -> Result<(), String> {
    let _write = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    match cayenchat_storage::load() {
        Ok(Some(file)) if file.credential_backend != settings.credential_backend => {
            let mut settings = settings.clone();
            settings.credential_backend = file.credential_backend;
            cayenchat_storage::save(&settings)
        }
        _ => cayenchat_storage::save(settings),
    }
}

/// Records `kind` as the credential backend in the file as it is now,
/// changing nothing else; `fallback` is written when there is no file yet.
/// It returns once the file is on disk: the caller deletes the old store's
/// secrets next, and a power loss must not leave the old choice with them
/// gone. Only the store thread calls it, so the wait does not stall the UI.
#[cfg(not(test))]
pub(crate) fn set_credential_backend(
    kind: cayenchat_storage::CredentialBackendKind,
    fallback: &Settings,
) -> Result<(), String> {
    let _write = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut settings = cayenchat_storage::load()?.unwrap_or_else(|| fallback.clone());
    settings.credential_backend = kind;
    cayenchat_storage::save_durable(&settings)
}

/// Replaces the file with `settings` after copying the old one beside it;
/// returns where the copy is (`None` when there was no file). Like `save`, it
/// keeps the credential backend the file holds, so an import cannot write
/// back a store the secrets have been moved out of.
#[cfg(not(test))]
pub(crate) fn replace(settings: &Settings) -> Result<Option<std::path::PathBuf>, String> {
    let _write = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    match cayenchat_storage::load() {
        Ok(Some(file)) if file.credential_backend != settings.credential_backend => {
            let mut settings = settings.clone();
            settings.credential_backend = file.credential_backend;
            cayenchat_storage::transfer::replace_settings(&settings)
        }
        _ => cayenchat_storage::transfer::replace_settings(settings),
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestState {
    /// `None` while no test holds a file; then the file's content.
    file: Option<Settings>,
    enabled: bool,
    failing: bool,
    unreadable: bool,
    /// `set_credential_backend` changes the file, then reports an error, as a
    /// failed flush of the directory after the rename does.
    fail_after_replace: bool,
}

/// What a test's file is made of. Each test thread has its own, so tests do
/// not meet; the credential store thread of a test shares its starter's
/// (`share`, `adopt`), as the real one shares the real file.
#[cfg(test)]
pub(crate) type SharedState = std::sync::Arc<std::sync::Mutex<TestState>>;

#[cfg(test)]
thread_local! {
    static STATE: std::cell::RefCell<SharedState> = std::cell::RefCell::new(SharedState::default());
}

#[cfg(test)]
fn with_state<T>(f: impl FnOnce(&mut TestState) -> T) -> T {
    let state = STATE.with_borrow(Clone::clone);
    let mut state = state.lock().unwrap();
    f(&mut state)
}

/// The test file of this thread, to give to a thread it starts.
#[cfg(test)]
pub(crate) fn share() -> SharedState {
    STATE.with_borrow(Clone::clone)
}

/// Makes this thread use the test file of the thread that started it.
#[cfg(test)]
pub(crate) fn adopt(state: SharedState) {
    STATE.with_borrow_mut(|own| *own = state);
}

/// Makes saving fail (or work again) to test what a failed save leaves.
#[cfg(test)]
pub(crate) fn fail_saves(failing: bool) {
    with_state(|state| state.failing = failing);
}

/// Makes loading fail (or work again), as an unsupported or damaged file does.
#[cfg(test)]
pub(crate) fn fail_loads(failing: bool) {
    with_state(|state| state.unreadable = failing);
}

#[cfg(test)]
pub(crate) fn load() -> Result<Option<Settings>, String> {
    with_state(|state| {
        if state.unreadable {
            return Err("cannot read the settings file".into());
        }
        Ok(state.file.clone())
    })
}

#[cfg(test)]
pub(crate) fn save(settings: &Settings) -> Result<(), String> {
    with_state(|state| {
        if state.failing {
            return Err("cannot write the settings file".into());
        }
        if state.enabled {
            let mut settings = settings.clone();
            if let Some(file) = &state.file {
                settings.credential_backend = file.credential_backend;
            }
            state.file = Some(settings);
        }
        Ok(())
    })
}

#[cfg(test)]
pub(crate) fn set_credential_backend(
    kind: cayenchat_storage::CredentialBackendKind,
    fallback: &Settings,
) -> Result<(), String> {
    with_state(|state| {
        if state.unreadable {
            return Err("cannot read the settings file".into());
        }
        if state.failing {
            return Err("cannot write the settings file".into());
        }
        if state.enabled {
            let mut settings = state.file.clone().unwrap_or_else(|| fallback.clone());
            settings.credential_backend = kind;
            state.file = Some(settings);
        }
        if state.fail_after_replace {
            return Err("cannot flush the settings directory".into());
        }
        Ok(())
    })
}

/// Makes `set_credential_backend` fail after it replaced the file.
#[cfg(test)]
pub(crate) fn fail_set_backend_after_replace(failing: bool) {
    with_state(|state| state.fail_after_replace = failing);
}

#[cfg(test)]
pub(crate) fn replace(settings: &Settings) -> Result<Option<std::path::PathBuf>, String> {
    let backup = with_state(|state| state.file.clone()).map(|_| "settings-backup.json".into());
    save(settings).map(|()| backup)
}

/// The directory of the imported themes (#962).
#[cfg(not(test))]
pub(crate) fn library_directory() -> Result<std::path::PathBuf, String> {
    cayenchat_storage::theme_file::library_directory()
}

/// The directory of the wallpapers that applied themes left (#962).
#[cfg(not(test))]
pub(crate) fn wallpaper_directory() -> Result<std::path::PathBuf, String> {
    cayenchat_storage::theme_file::wallpaper_directory()
}

#[cfg(test)]
thread_local! {
    /// Where a test keeps themes and wallpapers; without one there are none.
    static THEME_ROOT: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Points the theme directories of the running test at `root`.
#[cfg(test)]
pub(crate) fn use_theme_root(root: &std::path::Path) {
    THEME_ROOT.with_borrow_mut(|dir| *dir = Some(root.to_owned()));
}

#[cfg(test)]
pub(crate) fn library_directory() -> Result<std::path::PathBuf, String> {
    THEME_ROOT
        .with_borrow(Clone::clone)
        .map(|root| root.join("themes"))
        .ok_or_else(|| "no theme directory in this test".to_owned())
}

#[cfg(test)]
pub(crate) fn wallpaper_directory() -> Result<std::path::PathBuf, String> {
    THEME_ROOT
        .with_borrow(Clone::clone)
        .map(|root| root.join("wallpapers"))
        .ok_or_else(|| "no theme directory in this test".to_owned())
}

/// Notes in the settings file whether the upload token of `provider` is
/// saved, and returns the note as it is then (#642). Nothing is written when
/// the note already says so; a file that cannot be read is left alone.
pub(crate) fn note_upload_token(
    provider: &str,
    saved: bool,
) -> Result<Option<Vec<String>>, String> {
    let Some(mut settings) = load()? else {
        return Ok(None);
    };
    if settings.image_upload.note_token(provider, saved) {
        save(&settings)?;
    }
    Ok(settings.image_upload.token_saved)
}

/// The in-memory file of the running test, gone again when dropped.
#[cfg(test)]
pub(crate) struct TestFile;

#[cfg(test)]
impl TestFile {
    pub(crate) fn with(settings: &Settings) -> Self {
        with_state(|state| state.enabled = true);
        let _ = save(settings);
        Self
    }
}

#[cfg(test)]
impl Drop for TestFile {
    fn drop(&mut self) {
        with_state(|state| *state = TestState::default());
        THEME_ROOT.with_borrow_mut(|dir| *dir = None);
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
            if !cx.has_global::<secrets::Credentials>() {
                secrets::install_memory(cx);
            }
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
    fn tab_in_the_auto_join_dialog_scrolls_its_list_to_the_focused_row(cx: &mut TestAppContext) {
        let names: Vec<String> = (0..14).map(|i| format!("#c{i}")).collect();
        let settings = settings_with_channels(&names.join(","));
        let file = TestFile::with(&settings);
        // The production Tab bindings, which `open` does not install.
        cx.update(|cx| {
            crate::input::bind_keys(false, cx);
            cx.bind_keys(crate::shortcut_bindings(
                cayenchat_storage::ChannelNumberModifier::Ctrl,
                &Default::default(),
            ));
        });
        let (form, cx) = open(&settings, cx);
        form.update_in(cx, |form, window, cx| form.open_auto_join(window, cx));
        cx.run_until_parked();
        let state = |form: &gpui::Entity<SettingsWindow>, cx: &mut gpui::VisualTestContext| {
            form.update_in(cx, |form, window, cx| {
                let dialog = form.auto_join.as_ref().unwrap();
                let focused = crate::focus_reveal::focused_bounds(window, cx);
                (
                    dialog.scroll.offset().y,
                    dialog.scroll.bounds(),
                    focused,
                    form.pane_scroll.offset().y,
                )
            })
        };
        let (_, _, _, pane_before) = state(&form, cx);
        assert!(form.read_with(cx, |form, _| {
            form.auto_join.as_ref().unwrap().scroll.max_offset().height > gpui::px(0.)
        }));

        let row_focused = |form: &gpui::Entity<SettingsWindow>,
                           cx: &mut gpui::VisualTestContext,
                           index: usize| {
            form.update_in(cx, |form, window, cx| {
                let rows = &form.auto_join.as_ref().unwrap().rows;
                rows[index].name.focus_handle(cx).is_focused(window)
            })
        };
        let visible = |(_, viewport, focused, _): (_, gpui::Bounds<gpui::Pixels>, Option<_>, _)| {
            let focused: gpui::Bounds<gpui::Pixels> = focused.expect("a row has the focus");
            focused.top() >= viewport.top() && focused.bottom() <= viewport.bottom()
        };

        // Tab from the dialog itself leaves the list; start from the first row.
        form.update_in(cx, |form, window, cx| {
            let first = form.auto_join.as_ref().unwrap().rows[0]
                .name
                .focus_handle(cx);
            window.focus(&first);
        });
        cx.run_until_parked();
        // Forward to the last row: it is shown, and the pane behind stays.
        for _ in 0..100 {
            if row_focused(&form, cx, 13) {
                break;
            }
            cx.simulate_keystrokes("tab");
            // The test platform asks for no further frame; the app does.
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        assert!(row_focused(&form, cx, 13));
        cx.run_until_parked();
        let last = state(&form, cx);
        assert!(last.0 < gpui::px(0.), "the list did not follow Tab");
        assert_eq!(last.3, pane_before);
        assert!(visible(last));
        let offset = last.0;

        // Back to the first row: the list returns.
        for _ in 0..100 {
            if row_focused(&form, cx, 0) {
                break;
            }
            cx.simulate_keystrokes("shift-tab");
            // The test platform asks for no further frame; the app does.
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        assert!(row_focused(&form, cx, 0));
        let first = state(&form, cx);
        assert!(first.0 > offset, "the list did not follow Shift+Tab");
        assert_eq!(first.3, pane_before);
        assert!(visible(first));

        // A target already in view leaves the list where it is.
        let at = first.0;
        for _ in 0..100 {
            if row_focused(&form, cx, 1) {
                break;
            }
            cx.simulate_keystrokes("tab");
            // The test platform asks for no further frame; the app does.
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        assert!(row_focused(&form, cx, 1));
        assert_eq!(state(&form, cx).0, at);

        // Leaving a row and coming back to it after the list was scrolled
        // away must not reuse the place the row painted earlier.
        for _ in 0..100 {
            if row_focused(&form, cx, 0) {
                break;
            }
            cx.simulate_keystrokes("shift-tab");
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        assert!(row_focused(&form, cx, 0));
        cx.simulate_keystrokes("shift-tab");
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(!row_focused(&form, cx, 0), "Shift+Tab stayed on the row");
        form.update(cx, |form, _| {
            let scroll = &form.auto_join.as_ref().unwrap().scroll;
            let max = scroll.max_offset().height;
            scroll.set_offset(gpui::point(scroll.offset().x, -max));
        });
        for _ in 0..3 {
            if row_focused(&form, cx, 0) {
                break;
            }
            cx.simulate_keystrokes("tab");
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        assert!(row_focused(&form, cx, 0));
        let back = state(&form, cx);
        assert!(visible(back), "the list kept the returned row out of view");
        assert_eq!(back.3, pane_before);
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

    fn upload_settings(provider: &str, saved: bool) -> Settings {
        let mut settings = settings_with_channels("#a");
        settings.image_upload.provider = Some(provider.into());
        settings.image_upload.note_token(provider, saved);
        settings
    }

    #[gpui::test]
    fn showing_the_settings_does_not_read_the_credential_store(cx: &mut TestAppContext) {
        // The store is empty: a read would show "not connected", the note
        // says it is, and a store that fails changes nothing either.
        let settings = upload_settings("imgbb", true);
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        assert!(form.read_with(cx, |form, _| form.upload_connected));
        cx.update(|_, cx| {
            cx.set_global(secrets::Credentials(CredentialStore::with_backend(
                Arc::new(MemoryBackend::unavailable(CredentialBackendKind::System)),
            )));
        });
        form.update(cx, |form, cx| {
            form.refresh_upload_account(cx);
            assert!(form.upload_connected);
        });
        drop(file);
    }

    #[gpui::test]
    fn a_settings_file_without_the_note_reads_the_provider_as_saved(cx: &mut TestAppContext) {
        let mut settings = upload_settings("imgbb", false);
        settings.image_upload.token_saved = None;
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        assert!(form.read_with(cx, |form, _| form.upload_connected));
        drop(file);
    }

    #[gpui::test]
    fn the_upload_token_note_follows_saving_and_deleting(cx: &mut TestAppContext) {
        let settings = upload_settings("imgbb", false);
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let noted = || {
            super::load()
                .unwrap()
                .unwrap()
                .image_upload
                .token_noted("imgbb")
        };
        assert!(!form.read_with(cx, |form, _| form.upload_connected));

        form.update(cx, |form, cx| {
            form.upload_token
                .update(cx, |field, cx| field.set_text("token", cx));
            form.connect_upload_account(cx);
            // The window is held until the note is made.
            assert!(form.store_busy());
            assert!(!form.save_before_leaving(cx));
        });
        cx.run_until_parked();
        assert!(!form.read_with(cx, |form, _| form.store_busy()));
        assert!(form.read_with(cx, |form, _| form.upload_connected));
        assert!(noted());
        let store = cx.update(|_, cx| secrets::store(cx));
        assert!(
            store
                .contains(&cayenchat_storage::SecretKey::uploader_token("imgbb"))
                .unwrap()
        );

        // A deletion the store refuses leaves the note as it was.
        cx.update(|_, cx| {
            cx.set_global(secrets::Credentials(CredentialStore::with_backend(
                Arc::new(MemoryBackend::unavailable(CredentialBackendKind::System)),
            )));
        });
        form.update(cx, |form, cx| form.disconnect_upload_account(cx));
        cx.run_until_parked();
        assert!(form.read_with(cx, |form, _| form.upload_connected));
        assert!(noted());
        cx.update(|_, cx| cx.set_global(secrets::Credentials(store)));

        form.update(cx, |form, cx| {
            form.disconnect_upload_account(cx);
            assert!(form.store_busy());
        });
        cx.run_until_parked();
        assert!(!form.read_with(cx, |form, _| form.store_busy()));
        assert!(form.read_with(cx, |form, _| !form.upload_connected));
        assert!(!noted());
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

        // A hostname the connection would refuse is not saved either.
        form.update(cx, |form, cx| {
            form.settings
                .custom_host
                .update(cx, |field, cx| field.set_text("bad host", cx));
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
    fn saving_a_draft_does_not_register_other_drafts(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let first = settings.selected_server.clone();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.settings.values.add_server("b.example");
            form.show_selected_server(cx);
            // Back to the registered server, then a second draft.
            form.settings.values.selected_server = first;
            form.show_selected_server(cx);
            form.settings.values.add_server("c.example");
            form.show_selected_server(cx);
            form.save_servers(cx);
            assert!(form.feedback.is_none());
        });
        let hosts: Vec<_> = super::load()
            .unwrap()
            .unwrap()
            .servers
            .into_iter()
            .map(|server| server.host)
            .collect();
        assert_eq!(hosts.len(), 2);
        assert!(hosts.contains(&"c.example".to_owned()));
        assert!(!hosts.contains(&"b.example".to_owned()));
        drop(file);
    }

    #[gpui::test]
    fn leaving_after_a_failed_save_is_refused_until_it_succeeds(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        super::fail_saves(true);
        form.update(cx, |form, cx| {
            form.settings.values.notifications.private_messages = false;
            // Asking again never discards the edits.
            for _ in 0..3 {
                assert!(!form.save_before_leaving(cx));
                assert!(form.autosave_error.is_some());
            }
        });
        super::fail_saves(false);
        form.update(cx, |form, cx| {
            assert!(form.save_before_leaving(cx));
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
        cx.run_until_parked();
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
    fn a_registered_servers_password_is_stored_only_by_its_save_button(cx: &mut TestAppContext) {
        let settings = connectable("#a");
        let key = settings.selected_profile().unwrap().server_password_key();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let store = cx.update(|_, cx| secrets::store(cx));

        // Typing, autosave, Save of the server and leaving do not store it.
        form.update(cx, |form, cx| {
            assert!(form.shown_server_registered());
            form.settings
                .server_password
                .update(cx, |field, cx| field.set_text("typed", cx));
            form.autosave_now(None, cx);
            form.save_servers(cx);
            assert!(!form.servers_unsaved(cx), "a typed password is not an edit");
            assert!(form.save_before_leaving(cx));
        });
        assert!(!store.contains(&key).unwrap());
        // Connect would still use it, for that connection alone.
        form.update(cx, |form, cx| {
            let mut settings = form.settings.snapshot(cx).unwrap();
            let (config, missing) = form
                .settings
                .connection_config(&mut settings, &store, &form.i18n, cx)
                .unwrap();
            assert_eq!(config.server_password.as_deref(), Some("typed"));
            assert!(!missing.any());
        });

        // The Save button writes it, and the note reaches the file.
        form.update_in(cx, |form, window, cx| {
            form.save_credential(Credential::Server, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "typed");
        form.update(cx, |form, cx| {
            assert_eq!(
                form.settings.credential_state(Credential::Server),
                (true, false)
            );
            assert_eq!(form.settings.server_password.read(cx).text(), "");
        });
        assert!(
            super::load()
                .unwrap()
                .unwrap()
                .selected_profile()
                .unwrap()
                .server_password_saved()
        );
        drop(file);
    }

    #[gpui::test]
    fn a_draft_server_gets_only_its_checked_passwords_when_it_is_saved(cx: &mut TestAppContext) {
        let mut settings = connectable("#a");
        settings.selected_profile_mut().unwrap().sasl_enabled = true;
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let store = cx.update(|_, cx| secrets::store(cx));
        let (id, key, sasl_key) = form.update(cx, |form, cx| {
            let server = form.settings.values.add_server("b.example");
            server.nickname = "nick".into();
            server.username = "user".into();
            server.sasl_enabled = true;
            server.sasl_username = "account".into();
            let ids = (
                server.id.clone(),
                server.server_password_key(),
                server.sasl_password_key(),
            );
            form.show_selected_server(cx);
            ids
        });
        form.update_in(cx, |form, window, cx| {
            for which in [Credential::Server, Credential::Sasl] {
                form.settings
                    .credential_field(which)
                    .update(cx, |field, cx| field.set_text("typed", cx));
            }
            // Only the SASL password is checked.
            form.toggle_remember(Credential::Sasl, window, cx);
            // An autosave registers no draft and writes no password.
            form.autosave_now(None, cx);
        });
        assert!(!store.contains(&sasl_key).unwrap());

        // A failing store stops the save and keeps what was typed.
        let working = store.clone();
        cx.update(|_, cx| {
            cx.set_global(secrets::Credentials(CredentialStore::with_backend(
                Arc::new(MemoryBackend::unavailable(CredentialBackendKind::System)),
            )));
        });
        form.update(cx, |form, cx| form.save_servers(cx));
        cx.run_until_parked();
        form.update(cx, |form, cx| {
            assert!(form.feedback.is_some());
            assert!(!form.saved.profile(&id).is_some());
            assert_eq!(
                form.settings
                    .credential_field(Credential::Sasl)
                    .read(cx)
                    .text(),
                "typed"
            );
        });
        cx.update(|_, cx| cx.set_global(secrets::Credentials(working.clone())));
        form.update(cx, |form, cx| form.save_servers(cx));
        cx.run_until_parked();
        form.update(cx, |form, cx| {
            assert!(form.saved.profile(&id).is_some());
            assert!(form.settings.credential_state(Credential::Sasl).0);
            assert!(
                !form.settings.saved_server_password,
                "its box was not checked"
            );
            assert_eq!(form.settings.server_password.read(cx).text(), "typed");
        });
        assert_eq!(working.get(&sasl_key).unwrap().unwrap().expose(), "typed");
        assert!(!working.contains(&key).unwrap());
        let file_settings = super::load().unwrap().unwrap();
        assert!(file_settings.profile(&id).unwrap().sasl_password_saved());
        assert!(!file_settings.profile(&id).unwrap().server_password_saved());
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
            // The settings file notes the saved password.
            form.settings
                .values
                .selected_profile_mut()
                .unwrap()
                .set_server_password_saved(true);
            form.settings.show_selected(&form.i18n, cx);
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
    fn the_window_stays_on_its_server_while_the_store_is_written(cx: &mut TestAppContext) {
        let mut settings = connectable("#a");
        let a_id = settings.selected_server.clone();
        let b_id = settings.add_server("b.example").id.clone();
        settings.selected_server = a_id.clone();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.store_wait(cx);
            // Neither leaving nor showing or removing another server.
            assert!(!form.save_before_leaving(cx));
            form.select_server(b_id.clone(), cx);
            assert_eq!(form.settings.values.selected_server, a_id);
            form.store_done(cx);
            form.select_server(b_id.clone(), cx);
            assert_eq!(form.settings.values.selected_server, b_id);
            assert!(form.save_before_leaving(cx));
        });
        drop(file);
    }

    /// A store that holds a write or a deletion until the test lets it go,
    /// like a system store waiting for the user to allow it.
    struct Gated {
        inner: MemoryBackend,
        gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        /// Whether a call is being held now.
        holding: std::sync::atomic::AtomicBool,
        on_set: bool,
    }

    impl Gated {
        /// Installs it with the store thread of a real run; `release` lets
        /// one held call go.
        fn install(on_set: bool, cx: &mut gpui::App) -> (std::sync::mpsc::Sender<()>, Arc<Self>) {
            let (release, gate) = std::sync::mpsc::channel();
            let backend = Arc::new(Self {
                inner: MemoryBackend::new(CredentialBackendKind::System),
                gate: std::sync::Mutex::new(gate),
                holding: std::sync::atomic::AtomicBool::new(false),
                on_set,
            });
            cx.set_global(secrets::Worker::start(CredentialStore::with_backend(
                backend.clone(),
            )));
            (release, backend)
        }

        fn hold(&self) {
            use std::sync::atomic::Ordering::SeqCst;
            self.holding.store(true, SeqCst);
            let released = self
                .gate
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(20));
            self.holding.store(false, SeqCst);
            released.expect("the test lets the call go");
        }

        /// Waits until a call is held.
        fn wait_until_held(&self) {
            let started = std::time::Instant::now();
            while !self.holding.load(std::sync::atomic::Ordering::SeqCst) {
                assert!(started.elapsed().as_secs() < 20, "a call is held");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    impl cayenchat_storage::credentials::CredentialBackend for Gated {
        fn kind(&self) -> CredentialBackendKind {
            self.inner.kind()
        }

        fn is_memory(&self) -> bool {
            true
        }

        fn get(
            &self,
            key: &cayenchat_storage::SecretKey,
        ) -> Result<Option<Secret>, cayenchat_storage::CredentialError> {
            self.inner.get(key)
        }

        fn set(
            &self,
            key: &cayenchat_storage::SecretKey,
            value: &Secret,
        ) -> Result<(), cayenchat_storage::CredentialError> {
            if self.on_set {
                self.hold();
            }
            self.inner.set(key, value)
        }

        fn delete(
            &self,
            key: &cayenchat_storage::SecretKey,
        ) -> Result<(), cayenchat_storage::CredentialError> {
            if !self.on_set {
                self.hold();
            }
            self.inner.delete(key)
        }
    }

    fn wait_for_store(form: &gpui::Entity<SettingsWindow>, cx: &mut gpui::VisualTestContext) {
        cx.executor().allow_parking();
        let started = std::time::Instant::now();
        while form.read_with(cx, |form, _| form.store_busy()) {
            assert!(started.elapsed().as_secs() < 20, "the store finished");
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn a_store_that_waits_refuses_leaving_and_says_why(cx: &mut TestAppContext) {
        let mut settings = upload_settings("imgbb", false);
        let a_id = settings.selected_server.clone();
        let b_id = settings.add_server("b.example").id.clone();
        settings.selected_server = a_id.clone();
        let file = TestFile::with(&settings);
        let release = cx.update(|cx| Gated::install(true, cx).0);
        let (form, cx) = open(&settings, cx);

        form.update(cx, |form, cx| {
            form.upload_token
                .update(cx, |field, cx| field.set_text("token", cx));
            form.connect_upload_account(cx);
            assert!(form.store_busy());
            assert!(!form.save_before_leaving(cx));
            form.select_server(b_id.clone(), cx);
            assert_eq!(form.settings.values.selected_server, a_id);
        });
        // Quitting goes through the same check and is refused too.
        assert!(cx.update(|_, cx| crate::settings_block_quit(cx)));

        release.send(()).unwrap();
        wait_for_store(&form, cx);
        form.update(cx, |form, cx| {
            assert!(form.save_before_leaving(cx));
            form.select_server(b_id.clone(), cx);
            assert_eq!(form.settings.values.selected_server, b_id);
        });
        drop(file);
    }

    /// The move records the new store in the file before the form learns of
    /// it; a save in between would write the old one back (#965).
    #[gpui::test]
    fn an_edit_during_a_move_waits_instead_of_writing_the_old_store_back(cx: &mut TestAppContext) {
        let settings = upload_settings("imgbb", true);
        let file = TestFile::with(&settings);
        let (release, backend) = cx.update(|cx| Gated::install(false, cx));
        // A secret to move, so that the originals are deleted at the end.
        cayenchat_storage::credentials::CredentialBackend::set(
            &backend.inner,
            &cayenchat_storage::SecretKey::uploader_token("imgbb"),
            &Secret::new("token"),
        )
        .unwrap();
        let (form, cx) = open(&settings, cx);

        form.update(cx, |form, cx| {
            form.switch_credential_backend(CredentialBackendKind::LocalFile, cx);
            assert!(form.store_busy());
        });
        // The move has recorded the new store in the file and is deleting
        // the originals; the form still shows the old one.
        cx.run_until_parked();
        backend.wait_until_held();
        assert_eq!(
            super::load().unwrap().unwrap().credential_backend,
            CredentialBackendKind::LocalFile
        );
        form.update(cx, |form, cx| {
            assert_eq!(
                form.settings.values.credential_backend,
                CredentialBackendKind::System
            );
            form.settings.values.notifications.private_messages = false;
            form.autosave_pause_elapsed(cx);
            form.autosave_now(None, cx);
        });
        let saved = super::load().unwrap().unwrap();
        assert!(saved.notifications.private_messages, "nothing is saved");
        assert_eq!(
            saved.credential_backend,
            CredentialBackendKind::LocalFile,
            "the old store is not written back"
        );

        release.send(()).unwrap();
        wait_for_store(&form, cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        let saved = super::load().unwrap().unwrap();
        assert!(!saved.notifications.private_messages, "saved afterwards");
        assert_eq!(saved.credential_backend, CredentialBackendKind::LocalFile);
        drop(file);
    }

    /// CRED-3: another window's whole-file save of a snapshot taken before
    /// the move cannot write the old store back, during or after the move.
    #[gpui::test]
    fn a_stale_save_from_another_window_keeps_the_new_backend(cx: &mut TestAppContext) {
        let settings = upload_settings("imgbb", true);
        let stale = settings.clone();
        let file = TestFile::with(&settings);
        let (release, backend) = cx.update(|cx| Gated::install(false, cx));
        cayenchat_storage::credentials::CredentialBackend::set(
            &backend.inner,
            &cayenchat_storage::SecretKey::uploader_token("imgbb"),
            &Secret::new("token"),
        )
        .unwrap();
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            form.switch_credential_backend(CredentialBackendKind::LocalFile, cx);
        });
        cx.run_until_parked();
        backend.wait_until_held();
        // Between the file's commit and the deletion of the originals.
        let mut other = stale.clone();
        other.notifications.private_messages = false;
        super::save(&other).unwrap();
        let saved = super::load().unwrap().unwrap();
        assert!(!saved.notifications.private_messages, "their setting lands");
        assert_eq!(saved.credential_backend, CredentialBackendKind::LocalFile);

        release.send(()).unwrap();
        wait_for_store(&form, cx);
        super::save(&stale).unwrap();
        assert_eq!(
            super::load().unwrap().unwrap().credential_backend,
            CredentialBackendKind::LocalFile
        );
        // The file names the store that holds the secret, so the next start
        // reads it from there.
        let store = cx.update(|_, cx| secrets::store(cx));
        assert!(
            store
                .get(&cayenchat_storage::SecretKey::uploader_token("imgbb"))
                .unwrap()
                .is_some(),
            "the moved secret is in the new store"
        );
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
            let profile = form.settings.values.selected_profile_mut().unwrap();
            profile.set_server_password_saved(true);
            profile.set_sasl_password_saved(true);
            form.show_selected_server(cx);
            for which in [Credential::Server, Credential::Sasl] {
                form.change_credential(which, window, cx);
                form.settings
                    .credential_field(which)
                    .update(cx, |field, cx| field.set_text("half", cx));
            }
            // Leaving the server and coming back.
            form.select_server(b_id.clone(), cx);
            assert!(!form.settings.drafts.contains_key(&a_id));
            form.select_server(a_id.clone(), cx);
            assert!(!form.settings.drafts.contains_key(&a_id));
            for which in [Credential::Server, Credential::Sasl] {
                let field = form.settings.credential_field(which);
                assert_eq!(field.read(cx).text(), "");
            }
            assert_eq!(form.settings.values.selected_server, a_id);
            let mut settings = form.settings.snapshot(cx).unwrap();
            let (config, _) = form
                .settings
                .connection_config(&mut settings, &store, &form.i18n, cx)
                .unwrap();
            assert_eq!(config.server_password.as_deref(), Some("old"));
            assert_eq!(
                config.sasl.as_ref().map(|sasl| sasl.password.as_str()),
                Some("old-sasl")
            );
        });
        drop(file);
    }

    /// A store that counts the reads, which is what can make the operating
    /// system ask the user for permission.
    struct CountingBackend {
        inner: MemoryBackend,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl cayenchat_storage::credentials::CredentialBackend for CountingBackend {
        fn kind(&self) -> CredentialBackendKind {
            self.inner.kind()
        }

        fn is_memory(&self) -> bool {
            true
        }

        fn get(
            &self,
            key: &cayenchat_storage::SecretKey,
        ) -> Result<Option<Secret>, cayenchat_storage::CredentialError> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get(key)
        }

        fn set(
            &self,
            key: &cayenchat_storage::SecretKey,
            value: &Secret,
        ) -> Result<(), cayenchat_storage::CredentialError> {
            self.inner.set(key, value)
        }

        fn delete(
            &self,
            key: &cayenchat_storage::SecretKey,
        ) -> Result<(), cayenchat_storage::CredentialError> {
            self.inner.delete(key)
        }
    }

    #[gpui::test]
    fn showing_servers_never_reads_the_credential_store(cx: &mut TestAppContext) {
        let mut settings = settings_with_channels("#a");
        let a_id = settings.selected_server.clone();
        settings
            .selected_profile_mut()
            .unwrap()
            .set_server_password_saved(true);
        let b_id = settings.add_server("b.example").id.clone();
        settings.selected_server = a_id.clone();
        let backend = Arc::new(CountingBackend {
            inner: MemoryBackend::new(CredentialBackendKind::System),
            reads: Default::default(),
        });
        cx.update(|cx| {
            cx.set_global(secrets::Credentials(CredentialStore::with_backend(
                backend.clone(),
            )));
        });
        let reads = || backend.reads.load(std::sync::atomic::Ordering::SeqCst);
        let file = TestFile::with(&settings);
        // Opening the window, picking servers and adding one are no use of
        // a password, so the operating system must not be asked about one.
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            assert!(form.settings.saved_server_password, "shown from the note");
            form.select_server(b_id.clone(), cx);
            assert!(!form.settings.saved_server_password);
            form.select_server(a_id.clone(), cx);
            assert!(form.settings.saved_server_password);
            form.settings.values.add_server("c.example");
            form.show_selected_server(cx);
        });
        cx.run_until_parked();
        assert_eq!(reads(), 0);
        drop(file);
    }

    #[gpui::test]
    fn a_save_keeps_the_note_a_connection_wrote_meanwhile(cx: &mut TestAppContext) {
        let mut settings = connectable("#a");
        let profile = settings.selected_profile_mut().unwrap();
        profile.sasl_enabled = true;
        profile.sasl_username = "account".into();
        let a_id = profile.id.clone();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        form.update(cx, |form, cx| {
            // A draft keeps the window from reloading the file.
            form.settings.values.add_server("b.example");
            form.show_selected_server(cx);
            assert_eq!(
                form.settings.credential_state(Credential::Sasl),
                (false, false)
            );
        });
        // A connection saved both passwords and noted it in the file.
        let mut noted = settings.clone();
        let profile = noted.profile_mut(&a_id).unwrap();
        profile.set_server_password_saved(true);
        profile.set_sasl_password_saved(true);
        super::save(&noted).unwrap();
        form.update(cx, |form, cx| {
            form.settings.values.appearance.alternate_rows ^= true;
            assert!(form.save_before_leaving(cx));
        });
        let written = super::load().unwrap().unwrap();
        let profile = written.profile(&a_id).unwrap();
        assert!(profile.server_password_saved() && profile.sasl_password_saved());
        drop(file);
    }

    #[gpui::test]
    fn a_save_keeps_the_note_this_window_wrote_by_saving_or_deleting(cx: &mut TestAppContext) {
        let mut settings = connectable("#a");
        let profile = settings.selected_profile_mut().unwrap();
        profile.sasl_enabled = true;
        profile.sasl_username = "account".into();
        let a_id = profile.id.clone();
        let file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        for which in [Credential::Server, Credential::Sasl] {
            for saved in [true, false] {
                form.update(cx, |form, cx| {
                    // Saving or deleting a password, then an ordinary change
                    // that autosaves.
                    form.credential_done(which, &a_id, Ok(()), saved, cx);
                    form.settings.values.appearance.alternate_rows ^= true;
                    assert!(form.save_before_leaving(cx));
                    assert_eq!(form.settings.credential_state(which), (saved, false));
                });
                let written = super::load().unwrap().unwrap();
                let profile = written.profile(&a_id).unwrap();
                let noted = match which {
                    Credential::Server => profile.server_password_saved(),
                    Credential::Sasl => profile.sasl_password_saved(),
                };
                assert_eq!(noted, saved);
            }
        }
        drop(file);
    }

    #[gpui::test]
    fn the_settings_window_reads_a_password_only_when_it_is_noted_as_saved(
        cx: &mut TestAppContext,
    ) {
        let mut settings = connectable("#a");
        let profile = settings.selected_profile_mut().unwrap();
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
        let (watch, store) = cx.update(|cx| {
            secrets::install_memory(cx);
            secrets::PasswordWatch::install(cx)
        });
        store.set(&key, &Secret::new("stored")).unwrap();
        store.set(&sasl_key, &Secret::new("stored-sasl")).unwrap();
        let (form, cx) = open(&settings, cx);
        form.update_in(cx, |form, _, cx| {
            // Saving, connecting and removing a server touch nothing that is
            // not noted as saved.
            form.settings.values.appearance.alternate_rows ^= true;
            assert!(form.save_before_leaving(cx));
            let mut snapshot = form.settings.snapshot(cx).unwrap();
            let (config, missing) = form
                .settings
                .connection_config(&mut snapshot, &store, &form.i18n, cx)
                .unwrap();
            assert!(config.server_password.is_none() && missing.sasl);
            form.select_server(b_id.clone(), cx);
            form.settings.values.remove_selected_server();
            form.show_selected_server(cx);
            assert!(form.save_before_leaving(cx));
        });
        assert_eq!(watch.touches(), 0);

        // Noted: read and used, and deleted with the server.
        form.update_in(cx, |form, _, cx| {
            let profile = form.settings.values.profile_mut(&a_id).unwrap();
            profile.set_server_password_saved(true);
            profile.set_sasl_password_saved(true);
            form.settings.show_selected(&form.i18n, cx);
            let mut snapshot = form.settings.snapshot(cx).unwrap();
            let (config, _) = form
                .settings
                .connection_config(&mut snapshot, &store, &form.i18n, cx)
                .unwrap();
            assert_eq!(config.server_password.as_deref(), Some("stored"));
        });
        assert_eq!(watch.touches(), 2);
        drop(file);
    }

    #[gpui::test]
    fn an_unsaved_server_gets_its_checked_password_only_when_it_is_saved(cx: &mut TestAppContext) {
        let saved = settings_with_channels("#a");
        let file = TestFile::with(&saved);
        let (form, cx) = open(&saved, cx);
        let store = cx.update(|_, cx| secrets::store(cx));
        let (_, new_key) = form.update(cx, |form, cx| {
            let server = form.settings.values.add_server("b.example");
            server.nickname = "nick".into();
            server.username = "user".into();
            let ids = (
                server.id.clone(),
                cayenchat_storage::SecretKey::server_password(&server.id),
            );
            form.show_selected_server(cx);
            ids
        });
        form.update_in(cx, |form, window, cx| {
            form.settings
                .server_password
                .update(cx, |field, cx| field.set_text("typed", cx));
            form.toggle_remember(Credential::Server, window, cx);
            // A draft server is not saved by an autosave, nor its password.
            form.autosave_now(None, cx);
        });
        assert!(!store.contains(&new_key).unwrap());
        // Its own Save registers it and stores the checked password.
        form.update(cx, |form, cx| form.save_servers(cx));
        cx.run_until_parked();
        assert_eq!(store.get(&new_key).unwrap().unwrap().expose(), "typed");
        form.update(cx, |form, _| assert!(form.settings.saved_server_password));
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
            form.settings
                .values
                .selected_profile_mut()
                .unwrap()
                .set_server_password_saved(true);
            form.settings.show_selected(&form.i18n, cx)
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

    #[gpui::test]
    fn a_failed_settings_save_keeps_the_backend_and_every_secret(cx: &mut TestAppContext) {
        use cayenchat_storage::SecretKey;

        let mut settings = settings_with_channels("#a");
        let id = settings.selected_server.clone();
        settings.selected_profile_mut().unwrap().keyed_channels =
            Some(vec!["#a".into(), "#b".into()]);
        settings.credential_backend = CredentialBackendKind::System;
        let keys = [
            SecretKey::channel_key(&id, "#a"),
            SecretKey::channel_key(&id, "#b"),
            SecretKey::uploader_token("imgbb"),
        ];
        let file = TestFile::with(&settings);
        let old = cx.update(secrets::install_memory);
        for key in &keys {
            old.set(key, &Secret::new("v")).unwrap();
        }
        let (form, cx) = open(&settings, cx);

        super::fail_saves(true);
        form.update(cx, |form, cx| {
            form.switch_credential_backend(CredentialBackendKind::LocalFile, cx)
        });
        cx.run_until_parked();
        form.update(cx, |form, _| {
            assert!(form.feedback.is_some());
            assert_eq!(
                form.settings.values.credential_backend,
                CredentialBackendKind::System
            );
        });
        cx.update(|_, cx| {
            assert_eq!(secrets::store(cx).kind(), CredentialBackendKind::System);
            for key in &keys {
                assert!(secrets::store(cx).contains(key).unwrap(), "{key:?} kept");
            }
        });
        let saved = super::load().unwrap().unwrap();
        assert_eq!(saved.credential_backend, CredentialBackendKind::System);

        // Once saving works, the same switch moves everything.
        super::fail_saves(false);
        form.update(cx, |form, cx| {
            form.switch_credential_backend(CredentialBackendKind::LocalFile, cx);
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            let new = secrets::store(cx);
            assert_eq!(new.kind(), CredentialBackendKind::LocalFile);
            for key in &keys {
                assert!(new.contains(key).unwrap(), "{key:?} moved");
                assert!(!old.contains(key).unwrap(), "{key:?} left behind");
            }
        });
        assert_eq!(
            super::load().unwrap().unwrap().credential_backend,
            CredentialBackendKind::LocalFile
        );
        drop(file);
    }
}
