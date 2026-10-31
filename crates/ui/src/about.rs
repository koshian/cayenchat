// SPDX-License-Identifier: GPL-3.0-only
use crate::{localization::Localizer, os_text::WindowOsText};
use gpui::{prelude::*, *};

const REPOSITORY_URL: &str = "https://github.com/koshian/cayenchat";

/// Version, commit and build kind of the running binary, all fixed when it was
/// built. `commit` is absent when the build had no repository to ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: &'static str,
    pub commit: Option<&'static str>,
    pub kind: BuildKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildKind {
    Release,
    Debug,
    Test,
}

impl BuildInfo {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            commit: option_env!("CAYENCHAT_COMMIT"),
            kind: if cfg!(feature = "test-build") {
                BuildKind::Test
            } else if cfg!(debug_assertions) {
                BuildKind::Debug
            } else {
                BuildKind::Release
            },
        }
    }
}

impl BuildKind {
    fn locale_key(self) -> &'static str {
        match self {
            Self::Release => "about_build_release",
            Self::Debug => "about_build_debug",
            Self::Test => "about_build_test",
        }
    }
}

/// The labelled rows of the window, in order; the commit row is left out when
/// the build has none instead of showing a made-up value.
pub fn rows(info: &BuildInfo, i18n: &Localizer) -> Vec<(String, String)> {
    let mut rows = vec![(i18n.text("about_version"), info.version.to_owned())];
    if let Some(commit) = info.commit {
        rows.push((i18n.text("about_commit"), commit.to_owned()));
    }
    rows.push((i18n.text("about_build"), i18n.text(info.kind.locale_key())));
    rows.push((i18n.text("about_license"), i18n.text("about_license_value")));
    rows
}

/// Height of the macOS title bar. GPUI's macOS backend takes the opening
/// bounds as the frame, title bar included (`vendor/gpui/PATCHES.md`, #801),
/// so a window sized for its content adds this to keep that content (#809).
const MACOS_TITLEBAR_HEIGHT: f32 = 28.;

/// `content` grown by the title bar on macOS; unchanged elsewhere.
pub(crate) fn frame_size(content: Size<Pixels>) -> Size<Pixels> {
    if cfg!(target_os = "macos") {
        size(content.width, content.height + px(MACOS_TITLEBAR_HEIGHT))
    } else {
        content
    }
}

/// The window currently open, so a second request raises it.
#[derive(Default)]
struct OpenAbout(Option<WindowHandle<AboutWindow>>);

impl Global for OpenAbout {}

/// Opens the About window, or raises the one already open.
pub fn open(i18n: &Localizer, cx: &mut App) {
    if let Some(handle) = cx.try_global::<OpenAbout>().and_then(|open| open.0)
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let i18n = i18n.clone();
    let bounds = Bounds::centered(None, frame_size(size(px(420.), px(320.))), cx);
    let title = i18n.text("about_title");
    let opened = cx.open_window(
        WindowOptions {
            app_id: Some(crate::APP_ID.into()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(360.), px(280.))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone().into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| {
            // Gives X11 the UTF-8 title too (issue #143).
            window.os_set_title(&title);
            cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus);
                AboutWindow {
                    info: BuildInfo::current(),
                    i18n,
                    focus,
                }
            })
        },
    );
    match opened {
        Ok(handle) => cx.set_global(OpenAbout(Some(handle))),
        Err(error) => log::warn!("Could not open the About window: {error}"),
    }
}

/// Re-reads the language of an open About window.
pub fn set_localizer(i18n: &Localizer, cx: &mut App) {
    let Some(handle) = cx.try_global::<OpenAbout>().and_then(|open| open.0) else {
        return;
    };
    let _ = handle.update(cx, |view, window, cx| {
        window.os_set_title(&i18n.text("about_title"));
        view.i18n = i18n.clone();
        cx.notify();
    });
}

pub struct AboutWindow {
    info: BuildInfo,
    i18n: Localizer,
    focus: FocusHandle,
}

impl AboutWindow {
    fn escape_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut App) {
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            window.remove_window();
            cx.stop_propagation();
        }
    }
}

impl Render for AboutWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::current(cx);
        let mut details = div().flex().flex_col().gap_1();
        for (label, value) in rows(&self.info, &self.i18n) {
            details = details.child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .w(px(110.))
                            .flex_shrink_0()
                            .text_color(theme.text_secondary)
                            .child(label),
                    )
                    .child(div().flex_1().min_w_0().child(value)),
            );
        }
        let content = div()
            .id("about-screen")
            .key_context("AboutWindow")
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, event, window, cx| this.escape_key_down(event, window, cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(theme.window)
            .text_color(theme.text)
            .child(div().text_xl().child("CayenChat"))
            .child(details)
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("about_copyright")),
            )
            .child(
                div()
                    .id("about-repository")
                    .text_color(theme.link)
                    .cursor_pointer()
                    .child(REPOSITORY_URL)
                    .on_click(|_, _, cx| cx.open_url(REPOSITORY_URL)),
            )
            .child(div().flex_1())
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("about-close")
                        .px_3()
                        .py_1()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface)
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover_strong))
                        .child(self.i18n.text("about_close"))
                        .on_click(|_, window, _| window.remove_window()),
                ),
            );
        crate::decorations::window_frame(window, cx, self.i18n.text("about_title"), content)
    }
}

#[cfg(test)]
mod tests {
    // Named imports: `super::*` would bring in `gpui::test`, which shadows
    // the `#[test]` attribute.
    use super::{BuildInfo, BuildKind, rows};
    use crate::localization::Localizer;
    use cayenchat_storage::Language;

    fn info(commit: Option<&'static str>, kind: BuildKind) -> BuildInfo {
        BuildInfo {
            version: "1.2.3",
            commit,
            kind,
        }
    }

    #[test]
    fn rows_show_the_version_and_commit_in_both_languages() {
        for language in [Language::English, Language::Japanese] {
            let i18n = Localizer::new(language);
            let rows = rows(&info(Some("abc123def456"), BuildKind::Release), &i18n);
            assert_eq!(rows[0], (i18n.text("about_version"), "1.2.3".to_owned()));
            assert_eq!(
                rows[1],
                (i18n.text("about_commit"), "abc123def456".to_owned())
            );
            assert_eq!(rows[2].1, i18n.text("about_build_release"));
            assert!(rows.iter().all(|(label, value)| {
                !label.starts_with("about_") && !value.starts_with("about_")
            }));
        }
    }

    #[test]
    fn a_build_without_a_commit_shows_no_commit_row() {
        let i18n = Localizer::new(Language::English);
        let rows = rows(&info(None, BuildKind::Debug), &i18n);
        assert!(
            rows.iter()
                .all(|(label, _)| *label != i18n.text("about_commit"))
        );
        assert_eq!(rows[1].1, i18n.text("about_build_debug"));
    }

    #[test]
    fn the_running_build_reports_the_crate_version() {
        assert_eq!(BuildInfo::current().version, env!("CARGO_PKG_VERSION"));
    }
}
