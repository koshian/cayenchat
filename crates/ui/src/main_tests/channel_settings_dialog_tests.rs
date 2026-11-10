// SPDX-License-Identifier: GPL-3.0-only
//! The channel settings window (#781, #950): opened from the channel menu,
//! it saves by itself and the chat window shows each change.
use super::ChatWindow;
use crate::channel_settings_dialog::ChannelSettingsView;
use cayenchat_model::names::CaseMapping;
use cayenchat_storage::{
    CharRefMode, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, TextEncoding, WallpaperFit,
    WallpaperRegion,
};
use gpui::{Entity, TestAppContext, VisualTestContext};

fn install_globals(cx: &mut TestAppContext) {
    crate::support::install_test_globals(cx);
}

fn same(left: &str, right: &str) -> bool {
    CaseMapping::default().same(left, right)
}

/// Opens the window of `channel` and returns its view and a context drawing
/// in it.
pub(super) fn open_channel_window(
    chat: &Entity<ChatWindow>,
    app: &TestAppContext,
    channel: &str,
    cx: &mut VisualTestContext,
) -> (Entity<ChannelSettingsView>, &'static mut VisualTestContext) {
    chat.update_in(cx, |chat, window, cx| {
        let id = chat.saved.selected_profile().unwrap().id.clone();
        chat.open_channel_settings_window(&id, channel, CaseMapping::default(), window, cx);
    });
    let handle = chat.read_with(cx, |chat, _| {
        chat.channel_settings_windows.last().unwrap().handle
    });
    let view = handle.root(cx).unwrap();
    let window_cx = VisualTestContext::from_window(handle.into(), app).into_mut();
    window_cx.run_until_parked();
    (view, window_cx)
}

fn open_windows(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) -> usize {
    chat.update(cx, |chat, cx| {
        chat.channel_settings_windows
            .retain(|open| open.handle.update(cx, |_, _, _| ()).is_ok());
        chat.channel_settings_windows.len()
    })
}

fn new_chat<'a>(
    cx: &'a mut TestAppContext,
    channels: &str,
) -> (
    Entity<ChatWindow>,
    TestAppContext,
    &'a mut VisualTestContext,
    crate::settings_file::TestFile,
) {
    install_globals(cx);
    let settings = crate::settings_with_channels(channels);
    let file = crate::settings_file::TestFile::with(&settings);
    let app = cx.clone();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    (chat, app, cx, file)
}

#[gpui::test]
fn the_window_saves_its_choices_and_leaves_the_settings_window_closed(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a,-#b");
    let (view, wcx) = open_channel_window(&chat, &app, "#b", cx);
    view.update_in(wcx, |view, window, cx| {
        assert!(!view.choices.enabled);
        view.choices.enabled = true;
        view.choices.decode = Some(TextEncoding::Gbk);
        view.choices.encode = Some(TextEncoding::Iso2022Jp);
        view.choices.char_refs = Some(CharRefMode::Iso2022Jp);
        view.save_now(window, cx);
    });
    chat.read_with(cx, |chat, _| {
        assert!(chat.settings_window.is_none());
        let profile = chat.saved.selected_profile().unwrap();
        assert_eq!(profile.channels, "#a,#b");
        assert_eq!(
            profile.channel_charset("#b", same).unwrap().char_refs,
            Some(CharRefMode::Iso2022Jp)
        );
    });
    let on_file = crate::settings_file::load().unwrap().unwrap();
    let profile = on_file.selected_profile().unwrap();
    assert_eq!(profile.channels, "#a,#b");
    let entry = profile.channel_charset("#b", same).unwrap();
    assert_eq!(entry.decode, Some(TextEncoding::Gbk));
    assert_eq!(entry.encode, Some(TextEncoding::Iso2022Jp));
    drop(file);
}

#[gpui::test]
fn an_edit_is_saved_after_a_short_pause(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    view.update(wcx, |view, cx| view.set_blur(9, cx));
    // Nothing is written before the pause is over.
    assert!(
        crate::settings_file::load()
            .unwrap()
            .unwrap()
            .appearance
            .channel_looks
            .is_empty()
    );
    wcx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    wcx.run_until_parked();
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert_eq!(on_file.appearance.channel_looks[0].wallpaper_blur, Some(9));
    drop(file);
}

#[gpui::test]
fn the_chat_window_shows_an_edit_before_the_pause_is_over(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    for opacity in [20_u8, 90] {
        view.update(wcx, |view, cx| view.set_opacity(u32::from(opacity), cx));
        // Shown at once; the file still waits.
        let shown = chat.read_with(cx, |chat, _| {
            chat.appearance
                .channel_look(&chat.saved.servers[0].id, "#a", same)
                .and_then(|look| look.background_opacity)
        });
        assert_eq!(shown, Some(opacity));
    }
    assert!(
        crate::settings_file::load()
            .unwrap()
            .unwrap()
            .appearance
            .channel_looks
            .is_empty()
    );
    drop(file);
}

#[gpui::test]
fn closing_the_window_saves_an_edit_still_waiting(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    view.update(wcx, |view, cx| view.set_opacity(51, cx));
    assert!(wcx.simulate_close());
    // Written once the close event is over, not inside it.
    wcx.run_until_parked();
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert_eq!(
        on_file.appearance.channel_looks[0].background_opacity,
        Some(51)
    );
    let _ = chat;
    drop(file);
}

#[gpui::test]
fn a_look_edit_does_not_rewind_a_charset_changed_after_the_window_opened(cx: &mut TestAppContext) {
    use super::charset_command_tests::{open_chat, run};
    let app = cx.clone();
    let (chat, cx, file) = open_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    // `/charset` runs in the chat window while the window is open.
    run(&chat, cx, "/charset Big5").unwrap();
    assert_eq!(stored_decode("#a"), Some(TextEncoding::Big5));
    view.update_in(wcx, |view, window, cx| {
        view.set_opacity(35, cx);
        view.save_now(window, cx);
    });
    assert_eq!(stored_decode("#a"), Some(TextEncoding::Big5));
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert_eq!(
        on_file.appearance.channel_looks[0].background_opacity,
        Some(35)
    );
    drop(file);
}

#[gpui::test]
fn a_second_request_raises_the_open_window(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (_view, _wcx) = open_channel_window(&chat, &app, "#a", cx);
    chat.update_in(cx, |chat, window, cx| {
        let id = chat.saved.selected_profile().unwrap().id.clone();
        chat.open_channel_settings_window(&id, "#A", CaseMapping::default(), window, cx);
    });
    assert_eq!(open_windows(&chat, cx), 1);
    chat.update_in(cx, |chat, window, cx| {
        let id = chat.saved.selected_profile().unwrap().id.clone();
        chat.open_channel_settings_window(&id, "#b", CaseMapping::default(), window, cx);
    });
    assert_eq!(open_windows(&chat, cx), 2);
    drop(file);
}

#[gpui::test]
fn the_window_saves_the_log_look_and_rejects_a_bad_one(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#A", cx);

    // Fields: size, background, alternate, text, wallpaper.
    view.update_in(wcx, |view, window, cx| {
        view.set_look_field(0, "20", cx);
        view.set_look_field(1, "#102030", cx);
        view.set_look_field(3, "red", cx);
        view.save_now(window, cx);
    });
    // A color that is not #RRGGBB is reported and not written.
    assert!(view.read_with(wcx, |view, _| view.error.is_some()));
    chat.read_with(cx, |chat, _| {
        assert!(chat.saved.appearance.channel_looks.is_empty());
    });
    view.update_in(wcx, |view, window, cx| {
        view.set_look_field(3, "#abcdef", cx);
        // Tenths of a sigma: 1.2.
        view.set_blur(12, cx);
        view.set_opacity(35, cx);
        view.set_fit(WallpaperFit::Contain, cx);
        view.save_now(window, cx);
    });
    assert!(view.read_with(wcx, |view, _| view.error.is_none()));
    chat.read_with(cx, |chat, _| {
        let id = chat.saved.selected_profile().unwrap().id.clone();
        let look = chat.saved.appearance.channel_look(&id, "#a", same).unwrap();
        assert_eq!(look.font_size, 20);
        assert_eq!(look.background.as_deref(), Some("#102030"));
        assert_eq!(look.text.as_deref(), Some("#ABCDEF"));
        assert_eq!(look.wallpaper_blur, Some(12));
        assert_eq!(look.background_opacity, Some(35));
        assert_eq!(look.wallpaper_fit, Some(WallpaperFit::Contain));
        // The live state follows the save.
        assert_eq!(
            chat.appearance.channel_looks,
            chat.saved.appearance.channel_looks
        );
    });

    // Values set by hand are the channel's own, also the defaults; resetting
    // removes the look.
    view.update_in(wcx, |view, window, cx| {
        view.set_blur(DEFAULT_WALLPAPER_BLUR, cx);
        view.set_opacity(DEFAULT_BACKGROUND_OPACITY.into(), cx);
        view.set_fit(WallpaperFit::default(), cx);
        view.save_now(window, cx);
        let look = view.look(cx).unwrap();
        assert_eq!(
            (
                look.wallpaper_blur,
                look.background_opacity,
                look.wallpaper_fit
            ),
            // The blur and the opacity moved by hand are the channel's own,
            // also at the default value: they must not fall back to a
            // standard wallpaper's.
            (
                Some(DEFAULT_WALLPAPER_BLUR),
                Some(DEFAULT_BACKGROUND_OPACITY),
                Some(WallpaperFit::default())
            )
        );
        // An explicit 0 stays 0 under a standard blur.
        view.set_blur(0, cx);
        view.save_now(window, cx);
        assert_eq!(view.look(cx).unwrap().wallpaper_blur, Some(0));
        view.reset(cx);
        view.save_now(window, cx);
    });
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert!(on_file.appearance.channel_looks.is_empty());
    drop(file);
}

#[gpui::test]
fn the_window_opens_on_what_the_file_holds(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a,#b");
    // `/charset` or the settings window wrote the file after this window
    // last saved.
    let mut latest = crate::settings_file::load().unwrap().unwrap();
    latest.servers[0]
        .set_channel_charset("#b", Some(TextEncoding::Gbk), None, None, same)
        .unwrap();
    crate::settings_file::save(&latest).unwrap();
    let (view, _wcx) = open_channel_window(&chat, &app, "#B", cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.choices.decode, Some(TextEncoding::Gbk));
    });
    drop(file);
}

#[gpui::test]
fn saving_from_the_window_does_not_rewind_a_charset_command_change(cx: &mut TestAppContext) {
    use super::charset_command_tests::{open_chat, run};
    let app = cx.clone();
    let (chat, cx, file) = open_chat(cx, "#a,#b");
    // `/charset` changes the selected channel (#a) before the window opens.
    run(&chat, cx, "/charset Big5").unwrap();
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    view.read_with(wcx, |view, _| {
        assert_eq!(view.choices.decode, Some(TextEncoding::Big5));
    });
    // Another writer stores #b while the window is open.
    let mut latest = crate::settings_file::load().unwrap().unwrap();
    latest.servers[0]
        .set_channel_charset("#b", Some(TextEncoding::Gbk), None, None, same)
        .unwrap();
    crate::settings_file::save(&latest).unwrap();

    // Only auto-join is switched.
    view.update_in(wcx, |view, window, cx| {
        view.choices.enabled = false;
        view.save_now(window, cx);
    });
    assert_eq!(stored_decode("#a"), Some(TextEncoding::Big5));
    assert_eq!(stored_decode("#b"), Some(TextEncoding::Gbk));
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert_eq!(on_file.servers[0].channels, "-#a,#b");
    drop(file);
}

fn stored_decode(channel: &str) -> Option<TextEncoding> {
    let file = crate::settings_file::load().unwrap().unwrap();
    file.servers[0]
        .channel_charset(channel, same)
        .and_then(|entry| entry.decode)
}

#[gpui::test]
fn a_failed_save_keeps_the_window_open_with_the_reason(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    crate::settings_file::fail_saves(true);
    view.update_in(wcx, |view, window, cx| {
        view.choices.enabled = false;
        view.choices.decode = Some(TextEncoding::Gbk);
        view.save_now(window, cx);
    });
    crate::settings_file::fail_saves(false);
    assert_eq!(open_windows(&chat, cx), 1);
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.saved.servers[0].channels, "#a");
        assert!(chat.saved.servers[0].channel_charsets.is_empty());
    });
    assert!(view.read_with(wcx, |view, _| view.error.is_some()));
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert_eq!(on_file.servers[0].channels, "#a");
    assert!(on_file.servers[0].channel_charsets.is_empty());
    drop(file);
}

/// A key press as the platform delivers it: key-down, then key-up (which is
/// what GPUI turns into a keyboard click).
fn press(cx: &mut VisualTestContext, key: &str) {
    // The key-up listener exists only on the element focused at paint time.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes(key);
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse(key).unwrap(),
    });
}

/// Tab, as the platform delivers it: the focus change shows after a draw.
fn tab(cx: &mut VisualTestContext) {
    cx.dispatch_action(crate::FocusNextField);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

#[test]
fn char_ref_choices_keep_their_order_and_ids() {
    use crate::channel_settings_dialog::CHAR_REF_CHOICES;
    // The settings window numbers its buttons by position, from 0. The channel
    // window puts "follow the server" (id 0) first and these after it.
    assert_eq!(
        CHAR_REF_CHOICES,
        [
            (CharRefMode::Auto, "char_refs_auto"),
            (CharRefMode::Iso2022Jp, "char_refs_iso2022jp"),
            (CharRefMode::Off, "char_refs_off"),
        ]
    );
}

/// Opens the window for `#b` (auto-join off), drawn so the first control
/// has the focus.
fn open_window_for_b(
    cx: &mut TestAppContext,
) -> (
    TestAppContext,
    Entity<ChannelSettingsView>,
    crate::settings_file::TestFile,
    &'static mut VisualTestContext,
) {
    let (chat, app, cx, file) = new_chat(cx, "#a,-#b");
    let (view, wcx) = open_channel_window(&chat, &app, "#b", cx);
    // The first draw asks for the focus on auto-join; the next shows it.
    for _ in 0..2 {
        wcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    (app, view, file, wcx)
}

#[gpui::test]
fn the_window_opens_with_auto_join_focused(cx: &mut TestAppContext) {
    let (_app, view, _file, cx) = open_window_for_b(cx);
    view.read_with(cx, |view, _| assert!(!view.choices.enabled));
    press(cx, "space");
    view.read_with(cx, |view, _| assert!(view.choices.enabled));
}

#[gpui::test]
fn the_window_tells_inheriting_no_wallpaper_and_its_own_apart(cx: &mut TestAppContext) {
    use crate::channel_settings_dialog::WallpaperChoice;
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let picture = std::env::temp_dir().join(format!("cayen-look-{}.png", std::process::id()));
    std::fs::write(&picture, b"x").unwrap();
    let (view, wcx) = open_channel_window(&chat, &app, "#A", cx);
    let saved_look = |chat: &Entity<ChatWindow>, cx: &mut VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            chat.saved.appearance.channel_looks.first().cloned()
        })
    };

    // Nothing chosen is inheriting: no override is stored at all.
    view.update(wcx, |view, cx| {
        assert_eq!(view.look(cx).unwrap().wallpaper, None);
        assert!(!view.look(cx).unwrap().no_wallpaper);
    });
    // "No wallpaper" is stored apart from "inherit", and drops a typed file.
    view.update_in(wcx, |view, window, cx| {
        view.set_look_field(4, &picture.to_string_lossy(), cx);
        view.set_wallpaper_choice(WallpaperChoice::Off, cx);
        view.save_now(window, cx);
    });
    let look = saved_look(&chat, cx).unwrap();
    assert!(look.no_wallpaper && look.wallpaper.is_none());
    // Its own file replaces it, and needs a file.
    view.update_in(wcx, |view, window, cx| {
        view.set_look_field(4, "", cx);
        view.set_wallpaper_choice(WallpaperChoice::Own, cx);
        assert!(view.look(cx).is_err());
        view.set_look_field(4, &picture.to_string_lossy(), cx);
        view.save_now(window, cx);
    });
    let look = saved_look(&chat, cx).unwrap();
    assert!(!look.no_wallpaper);
    assert_eq!(look.wallpaper.as_deref(), picture.to_str());
    // Back to the standard removes the override.
    view.update_in(wcx, |view, window, cx| {
        view.set_wallpaper_choice(WallpaperChoice::Inherit, cx);
        view.set_look_field(4, "", cx);
        view.save_now(window, cx);
    });
    assert!(saved_look(&chat, cx).is_none());
    std::fs::remove_file(&picture).unwrap();
    drop(file);
}

#[gpui::test]
fn tab_wraps_from_the_last_control_back_to_auto_join(cx: &mut TestAppContext) {
    let (_app, view, _file, cx) = open_window_for_b(cx);
    // Auto-join, decode, encode, four character references, the four log
    // look fields, the wallpaper's three choices, the miniature's choose
    // button, the four sliders, four fit buttons and reset: twenty-four stops.
    // Twenty-four Tabs make a full circle.
    for _ in 0..24 {
        tab(cx);
    }
    press(cx, "space");
    view.read_with(cx, |view, _| assert!(view.choices.enabled));
}

#[gpui::test]
fn enter_and_space_act_once_on_the_focused_control(cx: &mut TestAppContext) {
    let (_app, view, _file, cx) = open_window_for_b(cx);
    // The first control is the auto-join checkbox.
    press(cx, "space");
    view.read_with(cx, |view, _| assert!(view.choices.enabled));
    press(cx, "space");
    view.read_with(cx, |view, _| assert!(!view.choices.enabled));

    // Space opens the receive list, Down and Space choose, and it closes.
    tab(cx);
    press(cx, "space");
    view.read_with(cx, |view, _| assert!(view.open_list.is_some()));
    cx.simulate_keystrokes("down");
    press(cx, "space");
    view.read_with(cx, |view, _| {
        assert!(view.open_list.is_none());
        assert_eq!(view.choices.decode, Some(TextEncoding::ALL[0]));
    });
}

#[gpui::test]
fn tab_closes_an_open_encoding_list_and_moves_on(cx: &mut TestAppContext) {
    use crate::channel_settings_dialog::EncodingSide;
    let (_app, view, _file, cx) = open_window_for_b(cx);
    tab(cx);
    press(cx, "space");
    view.read_with(cx, |view, _| {
        assert_eq!(view.open_list, Some(EncodingSide::Decode));
    });
    tab(cx);
    view.read_with(cx, |view, _| assert!(view.open_list.is_none()));
    // The focus moved on to the encode selector: Space opens its list.
    press(cx, "space");
    view.read_with(cx, |view, _| {
        assert_eq!(view.open_list, Some(EncodingSide::Encode));
    });
}

#[gpui::test]
fn shift_tab_from_auto_join_wraps_to_reset(cx: &mut TestAppContext) {
    let (_app, view, _file, cx) = open_window_for_b(cx);
    view.update(cx, |view, _| view.choices.enabled = true);
    view.update(cx, |view, _| view.choices.decode = Some(TextEncoding::Gbk));
    cx.dispatch_action(crate::FocusPreviousField);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    press(cx, "enter");
    view.read_with(cx, |view, _| {
        // Reset keeps the auto-join choice and clears the rest.
        assert!(view.choices.enabled);
        assert_eq!(view.choices.decode, None);
    });
}

#[gpui::test]
fn escape_closes_the_list_before_the_window(cx: &mut TestAppContext) {
    let (app, view, _file, cx) = open_window_for_b(cx);
    tab(cx);
    press(cx, "space");
    view.read_with(cx, |view, _| assert!(view.open_list.is_some()));
    cx.simulate_keystrokes("escape");
    view.read_with(cx, |view, _| assert!(view.open_list.is_none()));
    assert_eq!(app.update(|app| app.windows().len()), 2);
    cx.simulate_keystrokes("escape");
    assert_eq!(app.update(|app| app.windows().len()), 1);
}

#[gpui::test]
fn an_edit_taken_back_before_the_save_restores_the_chat_window(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    let shown = |chat: &Entity<ChatWindow>, cx: &mut VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            chat.appearance
                .channel_look(&chat.saved.servers[0].id, "#a", same)
                .and_then(|look| look.background_opacity)
        })
    };
    view.update(wcx, |view, cx| view.set_opacity(60, cx));
    assert_eq!(shown(&chat, cx), Some(60));
    view.update(wcx, |view, cx| {
        view.set_opacity(DEFAULT_BACKGROUND_OPACITY.into(), cx)
    });
    // Moved by hand to the default value: it is the channel's own now.
    assert_eq!(shown(&chat, cx), Some(DEFAULT_BACKGROUND_OPACITY));
    drop(file);
}

#[gpui::test]
fn opening_and_editing_a_channel_keeps_the_standard_wallpapers_saved_meanwhile(
    cx: &mut TestAppContext,
) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    // The settings window saved a standard wallpaper after this window
    // started: only the appearance changed.
    chat.update_in(cx, |chat, window, cx| {
        let previous = chat.saved.clone();
        let mut saved = previous.clone();
        saved.appearance.wallpapers.channel_log.path = Some("/standard.png".into());
        saved.appearance.dark.wallpapers.channel_tree.path = Some("/tree.png".into());
        chat.apply_settings_change(&previous, saved, false, window, cx);
    });
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    let wallpapers = |chat: &Entity<ChatWindow>, cx: &mut VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            (
                chat.appearance.wallpapers.channel_log.path.clone(),
                chat.appearance.dark.wallpapers.channel_tree.path.clone(),
            )
        })
    };
    let kept = (
        Some("/standard.png".to_owned()),
        Some("/tree.png".to_owned()),
    );
    assert_eq!(wallpapers(&chat, cx), kept);
    // A preview and taking it back both start from the saved appearance.
    view.update(wcx, |view, cx| view.set_opacity(60, cx));
    assert_eq!(wallpapers(&chat, cx), kept);
    chat.update(cx, |chat, cx| chat.discard_look_preview(cx));
    assert_eq!(wallpapers(&chat, cx), kept);
    drop(file);
}

#[gpui::test]
fn quitting_the_application_saves_an_edit_still_waiting(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    view.update(wcx, |view, cx| view.set_opacity(51, cx));
    // The quit is never held back by the window, and the shutdown that the
    // platform runs after it (no window is being updated then) writes.
    chat.update_in(cx, |chat, window, cx| chat.request_quit(window, cx));
    app.update(|cx| cx.shutdown());
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert_eq!(
        on_file.appearance.channel_looks[0].background_opacity,
        Some(51)
    );
    drop(file);
}

#[gpui::test]
fn a_dropped_image_is_cut_to_the_chosen_range_and_kept_as_the_wallpaper(cx: &mut TestAppContext) {
    use cayenchat_media::avatar_edit::Region;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp")
        .join(format!("wallpaper-drop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("photo.png");
    image::RgbImage::from_pixel(400, 200, image::Rgb([200, 30, 30]))
        .save(&source)
        .unwrap();
    let kept = dir.join("kept");

    let (chat, app, cx, file) = new_chat(cx, "#a");
    let (view, wcx) = open_channel_window(&chat, &app, "#a", cx);
    view.update(wcx, |view, cx| {
        view.drop_wallpaper_for_test(source.clone(), kept.clone(), cx)
    });
    wcx.run_until_parked();
    // The whole image is selected to begin with; nothing is set yet.
    let region = view
        .read_with(wcx, |view, cx| view.crop_region_for_test(cx))
        .unwrap();
    assert_eq!((region.width, region.height), (400.0, 200.0));
    // The range editor is drawn in the window.
    wcx.update(|window, _| window.refresh());
    wcx.run_until_parked();
    assert!(wcx.debug_bounds("image-crop-picture").is_some());
    assert_eq!(
        view.read_with(wcx, |view, cx| view.wallpaper_text_for_test(cx)),
        ""
    );

    view.update(wcx, |view, cx| {
        view.set_crop_region_for_test(
            Region {
                x: 100.0,
                y: 50.0,
                width: 200.0,
                height: 100.0,
            },
            cx,
        );
        view.use_crop_for_test(cx);
    });
    wcx.run_until_parked();
    let path = view.read_with(wcx, |view, cx| view.wallpaper_text_for_test(cx));
    let path = std::path::PathBuf::from(path);
    assert_eq!(path.parent(), Some(kept.as_path()));
    let cut = image::open(&path).unwrap();
    assert_eq!((cut.width(), cut.height()), (200, 100));
    // The editor closed, and the wallpaper is saved with the look.
    assert!(
        view.read_with(wcx, |view, cx| view.crop_region_for_test(cx))
            .is_none()
    );
    wcx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    wcx.run_until_parked();
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert_eq!(
        on_file.appearance.channel_looks[0].wallpaper.as_deref(),
        path.to_str()
    );

    // A second cut leaves the first file: nothing deletes kept wallpapers.
    view.update(wcx, |view, cx| {
        view.drop_wallpaper_for_test(source.clone(), kept.clone(), cx)
    });
    wcx.run_until_parked();
    view.update(wcx, |view, cx| {
        view.set_crop_region_for_test(
            Region {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            cx,
        );
        view.use_crop_for_test(cx);
    });
    wcx.run_until_parked();
    let second = view.read_with(wcx, |view, cx| view.wallpaper_text_for_test(cx));
    assert_ne!(std::path::PathBuf::from(&second), path);
    assert!(path.exists() && std::path::Path::new(&second).is_file());

    // A file that is not an image is reported and nothing changes.
    let text = dir.join("note.png");
    std::fs::write(&text, b"not an image").unwrap();
    view.update(wcx, |view, cx| {
        view.drop_wallpaper_for_test(text.clone(), kept.clone(), cx)
    });
    wcx.run_until_parked();
    assert!(
        view.read_with(wcx, |view, cx| view.crop_region_for_test(cx))
            .is_none()
    );
    assert!(
        view.read_with(wcx, |view, _| view.crop_status_for_test())
            .is_some_and(|(_, failed)| failed)
    );
    assert_eq!(
        view.read_with(wcx, |view, cx| view.wallpaper_text_for_test(cx)),
        second
    );
    let _ = std::fs::remove_dir_all(&dir);
    drop(file);
}

#[gpui::test]
fn an_inherited_blur_shows_the_standard_value_and_zero_can_be_chosen(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    chat.update(cx, |chat, cx| {
        let dark = crate::theme::current(cx).dark;
        let standard = chat
            .appearance
            .wallpapers_mut(dark)
            .get_mut(WallpaperRegion::ChannelLog);
        standard.path = Some("standard.png".into());
        standard.blur = Some(30);
    });
    let (view, wcx) = open_channel_window(&chat, &app, "#A", cx);
    // Inheriting: the slider shows the standard blur, and nothing is the
    // channel's own yet.
    view.read_with(wcx, |view, cx| {
        assert_eq!(view.shown_strengths(cx).0, 30);
        assert_eq!(view.look(cx).unwrap().wallpaper_blur, None);
    });
    view.update_in(wcx, |view, window, cx| {
        view.set_blur_value(0, cx);
        view.save_now(window, cx);
        assert_eq!(view.look(cx).unwrap().wallpaper_blur, Some(0));
        assert_eq!(view.shown_strengths(cx).0, 0);
    });
    drop(file);
}

#[gpui::test]
fn an_inherited_tone_shows_the_standard_value_until_the_channel_moves_it(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    let dark = chat.update(cx, |chat, cx| {
        let dark = crate::theme::current(cx).dark;
        let standard = chat
            .appearance
            .wallpapers_mut(dark)
            .get_mut(WallpaperRegion::ChannelLog);
        standard.path = Some("standard.png".into());
        standard.brightness = Some(-10);
        standard.saturation = Some(150);
        dark
    });
    let (view, wcx) = open_channel_window(&chat, &app, "#A", cx);
    view.read_with(wcx, |view, cx| {
        assert_eq!(view.shown_tone(cx), (-10, 150));
        let look = view.look(cx).unwrap();
        assert_eq!(
            (
                look.wallpaper_brightness(dark),
                look.wallpaper_saturation(dark)
            ),
            (None, None)
        );
    });
    view.update_in(wcx, |view, window, cx| {
        view.set_brightness(0, cx);
        // Only the moved one becomes the channel's own.
        assert_eq!(view.shown_tone(cx).0, 0);
        let look = view.look(cx).unwrap();
        assert_eq!(
            (
                look.wallpaper_brightness(dark),
                look.wallpaper_saturation(dark)
            ),
            (Some(0), None)
        );
        // Only the theme on show gets it.
        assert_eq!(look.wallpaper_brightness(!dark), None);
        view.save_now(window, cx);
    });
    // Resolved for drawing, the channel log gets the same values.
    chat.update(cx, |chat, _| {
        let look = chat.appearance.channel_looks.first().unwrap().clone();
        let standard = cayenchat_storage::RegionWallpaper {
            brightness: Some(-10),
            saturation: Some(150),
            path: Some("standard.png".into()),
            ..cayenchat_storage::RegionWallpaper::default()
        };
        let theme = crate::theme::Theme::new(
            if dark {
                cayenchat_storage::ThemeMode::Dark
            } else {
                cayenchat_storage::ThemeMode::Light
            },
            gpui::WindowAppearance::Light,
            &chat.appearance,
        );
        let request = crate::channel_look::resolve(Some(&look), &standard, theme)
            .wallpaper
            .unwrap();
        assert_eq!((request.brightness, request.saturation), (0, 150));
    });
    drop(file);
}

/// Where the rendered slider `selector` is, asserted to lie inside the
/// window, as `(left, right, y)` of its pointer-sensitive row. The track
/// starts `SLIDER_INSET` px inside the element on both sides. The body is
/// scrolled with the mouse wheel until the slider is in the window.
fn slider_row(cx: &mut VisualTestContext, selector: &'static str) -> (f32, f32, f32) {
    use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TouchPhase, point, px};
    let window = cx.update(|window, _| window.bounds());
    let window = gpui::Bounds::new(gpui::Point::default(), window.size);
    let (bounds, body) = (0..100)
        .find_map(|_| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is not drawn"));
            let body = cx
                .debug_bounds("channel-settings-body")
                .expect("the body is drawn");
            if bounds.top() >= body.top() && bounds.bottom() <= body.bottom() {
                return Some((bounds, body));
            }
            cx.simulate_event(ScrollWheelEvent {
                position: body.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-100.))),
                modifiers: Modifiers::none(),
                touch_phase: TouchPhase::Moved,
            });
            None
        })
        .unwrap_or_else(|| panic!("{selector} cannot be scrolled into the body"));
    // What can be pressed is the body: it lies in the window, and the slider
    // in it.
    assert!(
        window.contains(&body.origin) && window.contains(&body.bottom_right()),
        "the body {body:?} is outside the window {window:?}"
    );
    let (left, right) = (f32::from(bounds.left()), f32::from(bounds.right()));
    assert!(
        left >= f32::from(body.left()) && right <= f32::from(body.right()),
        "{selector} is outside the body: {bounds:?}"
    );
    (left, right, f32::from(bounds.center().y))
}

/// Border (1) plus padding (`KNOB / 2 + 2`) of a slider element, before its
/// track starts.
const SLIDER_INSET: f32 = 10.;

#[gpui::test]
fn inherited_sliders_show_the_standard_values_and_a_real_drag_makes_them_the_channels_own(
    cx: &mut TestAppContext,
) {
    use gpui::{Modifiers, MouseButton, point, px};
    let none = Modifiers::none();
    let (chat, app, cx, file) = new_chat(cx, "#a");
    // Standard blur 3.0 (the right end of the blur slider), standard
    // opacity 80 (a strength of 20 of 100). The saved settings hold it too:
    // the window's preview rebuilds the chat window's look from them.
    chat.update(cx, |chat, cx| {
        let dark = crate::theme::current(cx).dark;
        for appearance in [&mut chat.appearance, &mut chat.saved.appearance] {
            let standard = appearance
                .wallpapers_mut(dark)
                .get_mut(WallpaperRegion::ChannelLog);
            standard.path = Some("standard.png".into());
            standard.blur = Some(30);
            standard.opacity = Some(80);
        }
    });
    let (view, wcx) = open_channel_window(&chat, &app, "#A", cx);
    let own = |view: &Entity<ChannelSettingsView>, wcx: &mut VisualTestContext| {
        view.read_with(wcx, |view, cx| {
            let look = view.look(cx).unwrap();
            (look.wallpaper_blur, look.background_opacity)
        })
    };

    // The blur slider is drawn at 3.0: pressing on its right end is no
    // change, so nothing becomes the channel's own. The opacity slider is
    // drawn at a strength of 20: pressing there is no change either.
    // The opacity slider is below the blur one: scroll to it first, so both
    // are in the window.
    slider_row(wcx, "channel-look-opacity");
    let (left, right, y) = slider_row(wcx, "channel-look-blur");
    let at = point(px(right - 1.), px(y));
    wcx.simulate_mouse_move(at, None, none);
    wcx.simulate_mouse_down(at, MouseButton::Left, none);
    wcx.simulate_mouse_up(at, MouseButton::Left, none);
    let (o_left, o_right, o_y) = slider_row(wcx, "channel-look-opacity");
    let track = o_right - o_left - 2. * SLIDER_INSET;
    let at = point(px(o_left + SLIDER_INSET + track * 0.2), px(o_y));
    wcx.simulate_mouse_move(at, None, none);
    wcx.simulate_mouse_down(at, MouseButton::Left, none);
    wcx.simulate_mouse_up(at, MouseButton::Left, none);
    assert_eq!(own(&view, wcx), (None, None));

    // Dragging the blur knob from 3.0 to the left end saves a blur of 0.
    let start = point(px(right - 1.), px(y));
    wcx.simulate_mouse_move(start, None, none);
    wcx.simulate_mouse_down(start, MouseButton::Left, none);
    for x in [(left + right) / 2., left + 1.] {
        wcx.simulate_mouse_move(point(px(x), px(y)), MouseButton::Left, none);
    }
    wcx.simulate_mouse_up(point(px(left + 1.), px(y)), MouseButton::Left, none);
    // A press on the right end of the opacity slider is a strength of 100,
    // an opacity of 0.
    let (_, o_right, o_y) = slider_row(wcx, "channel-look-opacity");
    let at = point(px(o_right - 1.), px(o_y));
    wcx.simulate_mouse_move(at, None, none);
    wcx.simulate_mouse_down(at, MouseButton::Left, none);
    wcx.simulate_mouse_up(at, MouseButton::Left, none);
    assert_eq!(own(&view, wcx), (Some(0), Some(0)));

    // Saved, and still 0 after reading the file again.
    wcx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    wcx.run_until_parked();
    let on_file = crate::settings_file::load().unwrap().unwrap();
    let saved = on_file.appearance.channel_looks[0].clone();
    assert_eq!(
        (saved.wallpaper_blur, saved.background_opacity),
        (Some(0), Some(0))
    );
    // The wallpaper request drawn from the saved look, over the standard
    // wallpaper (the settings file of this test holds none), has no blur.
    let request = chat.read_with(cx, |_, cx| {
        let standard = cayenchat_storage::RegionWallpaper {
            path: Some("standard.png".into()),
            blur: Some(30),
            opacity: Some(80),
            ..Default::default()
        };
        crate::channel_look::resolve(Some(&saved), &standard, crate::theme::current(cx)).wallpaper
    });
    assert_eq!(request.map(|request| request.blur), Some(0));
    // A reopened window shows 0 for both, not the standard values.
    let (reopened, wcx) = open_channel_window(&chat, &app, "#A", cx);
    reopened.read_with(wcx, |view, cx| assert_eq!(view.shown_strengths(cx), (0, 0)));
    drop(file);
}

#[gpui::test]
fn an_inherited_fit_is_shown_and_an_explicit_cover_overrides_it(cx: &mut TestAppContext) {
    let (chat, app, cx, file) = new_chat(cx, "#a");
    chat.update(cx, |chat, cx| {
        let dark = crate::theme::current(cx).dark;
        let standard = chat
            .appearance
            .wallpapers_mut(dark)
            .get_mut(WallpaperRegion::ChannelLog);
        standard.path = Some("standard.png".into());
        standard.fit = Some(WallpaperFit::Contain);
    });
    let (view, wcx) = open_channel_window(&chat, &app, "#A", cx);
    view.read_with(wcx, |view, cx| {
        assert_eq!(view.shown_fit(cx), WallpaperFit::Contain);
        assert_eq!(view.look(cx).unwrap().wallpaper_fit, None);
    });
    // Choosing Cover is the channel's own fit, kept over the standard one.
    view.update_in(wcx, |view, window, cx| {
        view.set_fit(WallpaperFit::Cover, cx);
        view.save_now(window, cx);
        assert_eq!(
            view.look(cx).unwrap().wallpaper_fit,
            Some(WallpaperFit::Cover)
        );
        assert_eq!(view.shown_fit(cx), WallpaperFit::Cover);
    });
    chat.read_with(cx, |chat, _| {
        let id = chat.saved.selected_profile().unwrap().id.clone();
        let look = chat
            .saved
            .appearance
            .channel_look(&id, "#a", |a: &str, b: &str| a.eq_ignore_ascii_case(b))
            .unwrap();
        assert_eq!(look.wallpaper_fit, Some(WallpaperFit::Cover));
    });
    drop(file);
}
