// SPDX-License-Identifier: GPL-3.0-only
use super::ChatWindow;
use cayenchat_model::{NetworkId, names::CaseMapping};
use cayenchat_storage::{CharRefMode, TextEncoding};
use gpui::TestAppContext;

fn install_globals(cx: &mut TestAppContext) {
    crate::support::install_test_globals(cx);
}

#[gpui::test]
fn the_channel_menu_dialog_saves_without_opening_the_settings_window(cx: &mut TestAppContext) {
    install_globals(cx);
    let settings = crate::settings_with_channels("#a,-#b");
    let file = crate::settings_file::TestFile::with(&settings);
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let same = |left: &str, right: &str| CaseMapping::default().same(left, right);

    // Cancel (and Escape) save nothing.
    chat.update_in(cx, |chat, window, cx| {
        chat.show_channel_settings(NetworkId(1), "#b", window, cx);
        let view = chat.channel_settings.as_ref().unwrap().view().clone();
        view.update(cx, |view, _| {
            assert!(!view.choices.enabled);
            view.choices.decode = Some(TextEncoding::Gbk);
        });
        // A second request while one is open changes nothing.
        chat.show_channel_settings(NetworkId(1), "#a", window, cx);
        assert_eq!(
            chat.channel_settings
                .as_ref()
                .unwrap()
                .view()
                .read(cx)
                .channel(),
            "#b"
        );
    });
    cx.simulate_keystrokes("escape");
    chat.read_with(cx, |chat, _| assert!(chat.channel_settings.is_none()));
    let on_file = crate::settings_file::load().unwrap().unwrap();
    assert!(
        on_file
            .selected_profile()
            .unwrap()
            .channel_charsets
            .is_empty()
    );

    // Save writes all four values, to the file and to the live state, and
    // never opens the settings window.
    chat.update_in(cx, |chat, window, cx| {
        chat.show_channel_settings(NetworkId(1), "#b", window, cx);
        let view = chat.channel_settings.as_ref().unwrap().view().clone();
        view.update(cx, |view, _| {
            view.choices.enabled = true;
            view.choices.decode = Some(TextEncoding::Gbk);
            view.choices.encode = Some(TextEncoding::Iso2022Jp);
            view.choices.char_refs = Some(CharRefMode::Iso2022Jp);
        });
        view.update(cx, |view, cx| view.save(cx));
    });
    chat.read_with(cx, |chat, _| {
        assert!(chat.channel_settings.is_none());
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
fn enter_and_space_act_once_on_the_focused_control(cx: &mut TestAppContext) {
    install_globals(cx);
    let settings = crate::settings_with_channels("-#a");
    let file = crate::settings_file::TestFile::with(&settings);
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update_in(cx, |chat, window, cx| {
        chat.show_channel_settings(NetworkId(1), "#a", window, cx);
    });
    let view = chat.read_with(cx, |chat, _| {
        chat.channel_settings.as_ref().unwrap().view().clone()
    });
    cx.run_until_parked();

    // The first control is the auto-join checkbox.
    press(cx, "space");
    cx.update(|window, _| window.refresh());
    press(cx, "space");
    view.read_with(cx, |view, _| assert!(!view.choices.enabled));

    // Space opens the receive list, Down and Space choose, and it closes.
    cx.dispatch_action(crate::FocusNextField);
    press(cx, "space");
    view.read_with(cx, |view, _| assert!(view.open_list.is_some()));
    cx.simulate_keystrokes("down");
    press(cx, "space");
    view.read_with(cx, |view, _| {
        assert!(view.open_list.is_none());
        assert_eq!(view.choices.decode, Some(TextEncoding::ALL[0]));
    });
    drop(file);
}

/// A key press as the platform delivers it: key-down, then key-up (which is
/// what GPUI turns into a keyboard click).
fn press(cx: &mut gpui::VisualTestContext, key: &str) {
    // The key-up listener exists only on the element focused at paint time.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes(key);
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse(key).unwrap(),
    });
}

#[test]
fn char_ref_choices_keep_their_order_and_ids() {
    use crate::channel_settings_dialog::CHAR_REF_CHOICES;
    // The settings window numbers its buttons by position, from 0. The channel
    // dialog puts "follow the server" (id 0) first and these after it.
    assert_eq!(
        CHAR_REF_CHOICES,
        [
            (CharRefMode::Auto, "char_refs_auto"),
            (CharRefMode::Iso2022Jp, "char_refs_iso2022jp"),
            (CharRefMode::Off, "char_refs_off"),
        ]
    );
}
