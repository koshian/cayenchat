// SPDX-License-Identifier: GPL-3.0-only
//! The channel settings window (#781, #950): opened from the channel menu,
//! it saves by itself and the chat window shows each change.
use super::ChatWindow;
use crate::channel_settings_dialog::ChannelSettingsView;
use cayenchat_model::names::CaseMapping;
use cayenchat_storage::{
    CharRefMode, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, TextEncoding, WallpaperFit,
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

    // The defaults are stored as nothing; resetting removes the look.
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
            (None, None, None)
        );
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
fn tab_wraps_from_the_last_control_back_to_auto_join(cx: &mut TestAppContext) {
    let (_app, view, _file, cx) = open_window_for_b(cx);
    // Auto-join, decode, encode, four character references, the five log
    // look fields with the wallpaper's Browse button, the two sliders, four
    // fit buttons and reset: twenty stops. Twenty Tabs make a full circle.
    for _ in 0..20 {
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
    // Back at the opened value: the saved one (the default) is shown again.
    assert_eq!(shown(&chat, cx), None);
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
        .read_with(wcx, |view, _| view.crop_region_for_test())
        .unwrap();
    assert_eq!((region.width, region.height), (400.0, 200.0));
    // The range editor is drawn in the window.
    wcx.update(|window, _| window.refresh());
    wcx.run_until_parked();
    assert!(wcx.debug_bounds("channel-look-crop-picture").is_some());
    assert_eq!(
        view.read_with(wcx, |view, cx| view.wallpaper_text_for_test(cx)),
        ""
    );

    view.update(wcx, |view, cx| {
        view.set_crop_region_for_test(Region {
            x: 100.0,
            y: 50.0,
            width: 200.0,
            height: 100.0,
        });
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
        view.read_with(wcx, |view, _| view.crop_region_for_test())
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
        view.set_crop_region_for_test(Region {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        });
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
        view.read_with(wcx, |view, _| view.crop_region_for_test())
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
