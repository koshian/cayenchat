// SPDX-License-Identifier: GPL-3.0-only
use super::ChatWindow;
use cayenchat_model::{NetworkId, names::CaseMapping};
use cayenchat_storage::{CharRefMode, TextEncoding};
use gpui::TestAppContext;

fn install_globals(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
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
