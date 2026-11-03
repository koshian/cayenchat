// SPDX-License-Identifier: GPL-3.0-only
//! The look of one channel's log (#928): the channel's own font size, colors
//! and wallpaper over the global appearance, and the wallpaper's loading.

use std::{path::PathBuf, sync::Arc};

use cayenchat_media::{CancelFlag, wallpaper};
use cayenchat_storage::{ChannelLook, DEFAULT_WALLPAPER_OPACITY, color_value};
use gpui::{prelude::*, *};

use crate::{ChatWindow, theme::Theme};

/// A channel's look with the global one filled in.
pub(crate) struct ResolvedLook {
    /// The theme with the channel's colors in place of the pane's.
    pub(crate) theme: Theme,
    /// A text color of the channel's own; `None` keeps the inherited one.
    pub(crate) text: Option<Rgba>,
    /// A font size in px of the channel's own, to be clamped by the caller.
    pub(crate) font_size: u16,
    pub(crate) wallpaper: Option<WallpaperRequest>,
}

/// The wallpaper to show: the file, the log background it is tinted toward
/// and how much of it shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WallpaperRequest {
    pub(crate) path: String,
    pub(crate) tint: u32,
    pub(crate) opacity: f32,
}

/// `look` laid over `theme`. With a wallpaper the alternate rows are drawn
/// half transparent, so the picture shows through them.
pub(crate) fn resolve(look: Option<&ChannelLook>, mut theme: Theme) -> ResolvedLook {
    let Some(look) = look else {
        return ResolvedLook {
            theme,
            text: None,
            font_size: 0,
            wallpaper: None,
        };
    };
    let color = |value: &Option<String>| value.as_deref().and_then(color_value).map(rgb);
    if let Some(background) = color(&look.background) {
        theme.panes.channel_log = background;
    }
    if let Some(alternate) = color(&look.alternate) {
        theme.panes.channel_alternate = alternate;
    }
    let background = theme.panes.channel_log;
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    let tint = channel(background.r) << 16 | channel(background.g) << 8 | channel(background.b);
    let wallpaper = look.wallpaper.as_ref().map(|path| WallpaperRequest {
        path: path.clone(),
        tint,
        opacity: f32::from(
            look.wallpaper_opacity
                .unwrap_or(DEFAULT_WALLPAPER_OPACITY)
                .min(100),
        ) / 100.,
    });
    if wallpaper.is_some() {
        theme.panes.channel_alternate.a = 0.5;
    }
    ResolvedLook {
        text: color(&look.text),
        font_size: look.font_size,
        theme,
        wallpaper,
    }
}

enum State {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

/// The wallpaper of the channel on show. Only that one is kept, keyed by the
/// file and the color it was tinted toward, so a result that arrives after
/// the channel or the colors changed is dropped.
#[derive(Default)]
pub(crate) struct Wallpaper {
    slot: Option<(PathBuf, u32, State)>,
}

impl Wallpaper {
    /// The picture for `wanted`, as the first child of the log; starts its
    /// loading when it is not the one held. Nothing while it loads or when
    /// the file cannot be used.
    pub(crate) fn element(
        &mut self,
        wanted: Option<&WallpaperRequest>,
        window: &mut Window,
        cx: &mut Context<ChatWindow>,
    ) -> Option<Div> {
        let Some(wanted) = wanted else {
            self.release(window);
            return None;
        };
        let path = PathBuf::from(&wanted.path);
        let held = self
            .slot
            .as_ref()
            .is_some_and(|(held, tint, _)| *held == path && *tint == wanted.tint);
        if !held {
            self.release(window);
            self.slot = Some((path.clone(), wanted.tint, State::Loading));
            let tint = wanted.tint;
            let loading = path.clone();
            let loaded = cx.background_spawn(async move {
                wallpaper::load(&loading, tint, &CancelFlag::default())
            });
            cx.spawn(async move |this, cx| {
                let result = loaded.await;
                let _ = this.update(cx, |this, cx| {
                    let Some((held, held_tint, state)) = this.wallpaper.slot.as_mut() else {
                        return;
                    };
                    if *held != path || *held_tint != tint {
                        return;
                    }
                    *state = match result {
                        Ok(thumbnail) => {
                            let buffer = image::RgbaImage::from_raw(
                                thumbnail.width,
                                thumbnail.height,
                                thumbnail.bgra,
                            )
                            .expect("wallpaper buffer matches its size");
                            State::Ready(Arc::new(RenderImage::new(vec![image::Frame::new(
                                buffer,
                            )])))
                        }
                        Err(_) => State::Failed,
                    };
                    cx.notify();
                });
            })
            .detach();
        }
        match self.slot.as_ref() {
            // Cover fit paints past the box, so the box clips it.
            Some((_, _, State::Ready(image))) => Some(
                div()
                    .absolute()
                    .size_full()
                    .overflow_hidden()
                    .opacity(wanted.opacity)
                    .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover)),
            ),
            _ => None,
        }
    }

    fn release(&mut self, window: &mut Window) {
        if let Some((_, _, State::Ready(image))) = self.slot.take() {
            let _ = window.drop_image(image);
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's glob would shadow `#[test]`.
    use super::{ChannelLook, DEFAULT_WALLPAPER_OPACITY, Theme, resolve};
    use cayenchat_storage::{Appearance, ThemeMode};
    use gpui::{WindowAppearance, rgb};

    fn theme(dark: bool) -> Theme {
        let mode = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        Theme::new(mode, WindowAppearance::Light, &Appearance::default())
    }

    #[test]
    fn a_channel_without_a_look_keeps_the_global_theme() {
        for dark in [false, true] {
            let base = theme(dark);
            let resolved = resolve(None, base);
            assert_eq!(resolved.theme, base);
            assert!(resolved.text.is_none() && resolved.wallpaper.is_none());
            assert_eq!(resolved.font_size, 0);
            // A look that sets nothing is the same.
            let empty = ChannelLook::default();
            assert_eq!(resolve(Some(&empty), base).theme, base);
        }
    }

    #[test]
    fn the_channel_look_wins_in_both_themes() {
        for dark in [false, true] {
            let base = theme(dark);
            let look = ChannelLook {
                font_size: 20,
                background: Some("#102030".into()),
                alternate: Some("#405060".into()),
                text: Some("#AABBCC".into()),
                wallpaper: Some("/pic.png".into()),
                wallpaper_opacity: Some(70),
                ..ChannelLook::default()
            };
            let resolved = resolve(Some(&look), base);
            assert_eq!(resolved.theme.panes.channel_log, rgb(0x102030));
            // The picture shows through the alternate rows.
            assert_eq!(resolved.theme.panes.channel_alternate.a, 0.5);
            assert_eq!(resolved.text, Some(rgb(0xAABBCC)));
            assert_eq!(resolved.font_size, 20);
            let wallpaper = resolved.wallpaper.unwrap();
            // Tinted toward the channel's own background.
            assert_eq!((wallpaper.tint, wallpaper.opacity), (0x102030, 0.7));
            // Nothing else of the theme moves.
            assert_eq!(resolved.theme.panes.combined_log, base.panes.combined_log);
            assert_eq!(resolved.theme.text, base.text);
        }
    }

    #[test]
    fn a_wallpaper_alone_tints_toward_the_themes_background_at_the_default_strength() {
        for (dark, tint) in [(false, 0xFFFFFF), (true, 0x1F2124)] {
            let look = ChannelLook {
                wallpaper: Some("/pic.png".into()),
                ..ChannelLook::default()
            };
            let wallpaper = resolve(Some(&look), theme(dark)).wallpaper.unwrap();
            assert_eq!(wallpaper.tint, tint);
            assert_eq!(
                wallpaper.opacity,
                f32::from(DEFAULT_WALLPAPER_OPACITY) / 100.
            );
        }
    }
}
