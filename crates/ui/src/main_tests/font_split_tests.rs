// SPDX-License-Identifier: GPL-3.0-only
//! The fonts of the settings form: real sizes with steppers, and the
//! separate light and dark fonts.
use super::{ChatWindow, SettingsTab, SettingsWindow};
use crate::settings_window::FontTarget;
use cayenchat_storage::{FontSet, MAX_FONT_SIZE, MIN_FONT_SIZE, Settings};
use gpui::{Entity, TestAppContext, VisualTestContext};

fn open(cx: &mut TestAppContext) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    crate::support::install_test_globals(cx);
    let owner = cx.add_window(|window, cx| {
        ChatWindow::with_settings(crate::settings_with_channels("#a"), None, window, cx)
    });
    let (form, cx) = cx.add_window_view(|window, cx| {
        let mut form = SettingsWindow::new(owner, Settings::default(), window, cx);
        form.tab = SettingsTab::Appearance;
        form
    });
    cx.run_until_parked();
    (form, cx)
}

fn fonts(
    form: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> cayenchat_storage::Appearance {
    form.read_with(cx, |form, cx| {
        form.settings.snapshot(cx).unwrap().appearance
    })
}

#[gpui::test]
fn the_size_fields_show_the_real_standard_sizes(cx: &mut TestAppContext) {
    let (form, cx) = open(cx);
    let texts = form.read_with(cx, |form, cx| {
        form.settings
            .font_sizes
            .clone()
            .map(|f| f.read(cx).text().to_owned())
    });
    assert_eq!(
        texts,
        ["13", "13", "12", "12", "13", "13"].map(String::from)
    );
}

#[gpui::test]
fn the_steppers_move_by_one_pixel_and_stop_at_the_ends(cx: &mut TestAppContext) {
    let (form, cx) = open(cx);
    let size = |form: &Entity<SettingsWindow>, cx: &mut VisualTestContext| {
        fonts(form, cx).fonts.input_font_size
    };
    form.update(cx, |form, cx| {
        form.settings.step_font_size(FontTarget::Input, 1, cx)
    });
    assert_eq!(size(&form, cx), 14);
    form.update(cx, |form, cx| {
        form.settings.step_font_size(FontTarget::Input, -1, cx);
        form.settings.step_font_size(FontTarget::Input, -1, cx);
    });
    assert_eq!(size(&form, cx), 12);
    // Typed by hand, then stepped from the typed value, up to the limit.
    form.update(cx, |form, cx| {
        form.settings.font_sizes[FontTarget::Input as usize]
            .update(cx, |field, cx| field.set_text("39", cx));
        for _ in 0..5 {
            form.settings.step_font_size(FontTarget::Input, 1, cx);
        }
    });
    assert_eq!(size(&form, cx), MAX_FONT_SIZE);
    form.update(cx, |form, cx| {
        form.settings.font_sizes[FontTarget::Input as usize]
            .update(cx, |field, cx| field.set_text("9", cx));
        for _ in 0..5 {
            form.settings.step_font_size(FontTarget::Input, -1, cx);
        }
    });
    assert_eq!(size(&form, cx), MIN_FONT_SIZE);
}

#[gpui::test]
fn separate_fonts_keep_each_side_and_come_back(cx: &mut TestAppContext) {
    let (form, cx) = open(cx);
    form.update(cx, |form, cx| {
        form.settings.step_font_size(FontTarget::ChannelLog, 2, cx); // 15 shared
        form.settings.set_split_fonts(true, cx);
    });
    let a = fonts(&form, cx);
    // Both sides start as the shared fonts.
    assert!(a.split_fonts);
    assert_eq!(a.fonts(false).channel_log_font_size, 15);
    assert_eq!(a.fonts(true).channel_log_font_size, 15);

    // The light side is shown; edit the dark one after switching.
    form.update(cx, |form, cx| {
        form.settings.select_font_side(true, cx);
        form.settings.step_font_size(FontTarget::ChannelLog, 3, cx);
        form.settings.select_font_side(false, cx);
    });
    let a = fonts(&form, cx);
    assert_eq!(a.fonts(true).channel_log_font_size, 18);
    assert_eq!(a.fonts(false).channel_log_font_size, 15);
    let shown = form.read_with(cx, |form, cx| {
        form.settings.font_sizes[0].read(cx).text().to_owned()
    });
    assert_eq!(shown, "15");

    // Back to shared: the dark value is kept for the next time.
    form.update(cx, |form, cx| form.settings.set_split_fonts(false, cx));
    let a = fonts(&form, cx);
    assert!(!a.split_fonts);
    assert_eq!(a.fonts(true).channel_log_font_size, 15);
    assert_eq!(a.dark_fonts.as_ref().unwrap().channel_log_font_size, 18);
    form.update(cx, |form, cx| form.settings.set_split_fonts(true, cx));
    assert_eq!(fonts(&form, cx).fonts(true).channel_log_font_size, 18);
    assert_eq!(
        *fonts(&form, cx).fonts(false),
        FontSet {
            channel_log_font_size: 15,
            ..FontSet::default()
        }
    );
}
