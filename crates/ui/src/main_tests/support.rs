// SPDX-License-Identifier: GPL-3.0-only
use crate::Settings;

/// Default settings whose selected server auto-joins `channels`.
pub(crate) fn settings_with_channels(channels: &str) -> Settings {
    let mut settings = Settings::default();
    settings
        .add_server(cayenchat_storage::PRESETS[0].host)
        .channels = channels.into();
    settings
}

/// Installs the globals every UI test needs: the in-memory secret store and
/// the Light theme (in this order).
pub(crate) fn install_test_globals(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
}

/// A chat window over `settings` with the default shortcuts and the test
/// globals installed, run until idle.
pub(crate) fn window<'a>(
    settings: &Settings,
    cx: &'a mut gpui::TestAppContext,
) -> (
    gpui::Entity<crate::ChatWindow>,
    &'a mut gpui::VisualTestContext,
) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let (chat, cx) = cx.add_window_view(|window, cx| {
        crate::ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();
    (chat, cx)
}
