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
