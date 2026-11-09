// SPDX-License-Identifier: GPL-3.0-only
use super::ChatWindow;
use gpui::TestAppContext;

#[gpui::test]
fn the_combined_log_color_is_painted_once_under_a_wallpaper(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.appearance.wallpapers.combined_log.path = Some("/wallpaper.png".into());
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.read_with(cx, |chat, cx| {
        let mut theme = crate::theme::current(cx);
        theme.panes.combined_log.a = 0.4;
        // The backdrop carries the opacity, so no second layer is drawn.
        assert_eq!(chat.combined_log_base(theme).a, 1.);
    });
    chat.update(cx, |chat, _| {
        chat.appearance.wallpapers.combined_log.path = None
    });
    chat.read_with(cx, |chat, cx| {
        let mut theme = crate::theme::current(cx);
        theme.panes.combined_log.a = 0.4;
        assert_eq!(chat.combined_log_base(theme).a, 0.4);
    });
}
