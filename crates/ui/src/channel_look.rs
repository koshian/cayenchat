// SPDX-License-Identifier: GPL-3.0-only
//! The look of one channel's log (#928): the channel's own font size, colors
//! and wallpaper over the global appearance, and the wallpaper's loading.

use std::{path::PathBuf, sync::Arc};

use cayenchat_media::{CancelFlag, wallpaper};
use cayenchat_storage::{
    ChannelLook, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, MAX_WALLPAPER_BLUR,
    WallpaperFit, color_value,
};
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

/// The wallpaper to show (#950): the file, its blur, how it fits the log and
/// the log background color laid over it. Only the file and the blur need a
/// new load; the fit and the color are applied when it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WallpaperRequest {
    pub(crate) path: String,
    /// Blur sigma in px of the kept (≤ 640 px) picture; 0 is none.
    pub(crate) blur: u8,
    pub(crate) fit: WallpaperFit,
    /// The log background with the channel's opacity as its alpha.
    pub(crate) cover: Rgba,
}

/// The blur a look holds, capped to the range.
pub(crate) fn blur_of(look: &ChannelLook) -> u8 {
    look.wallpaper_blur
        .unwrap_or(DEFAULT_WALLPAPER_BLUR)
        .min(MAX_WALLPAPER_BLUR)
}

/// The percent of the log background's opacity a look holds, capped.
pub(crate) fn background_opacity_of(look: &ChannelLook) -> u8 {
    look.background_opacity
        .unwrap_or(DEFAULT_BACKGROUND_OPACITY)
        .min(100)
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
    let wallpaper = look.wallpaper.as_ref().map(|path| WallpaperRequest {
        path: path.clone(),
        blur: blur_of(look),
        fit: look.wallpaper_fit.unwrap_or_default(),
        cover: Rgba {
            a: f32::from(background_opacity_of(look)) / 100.,
            ..theme.panes.channel_log
        },
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

fn object_fit(fit: WallpaperFit) -> ObjectFit {
    match fit {
        WallpaperFit::Stretch => ObjectFit::Fill,
        WallpaperFit::Cover => ObjectFit::Cover,
        WallpaperFit::Contain => ObjectFit::Contain,
        WallpaperFit::Center => ObjectFit::None,
    }
}

enum State {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

/// What a loaded picture was made from. The fit and the background laid
/// over it are not part of it: they only change how the held picture is
/// drawn, so changing them never loads again.
#[derive(Clone, PartialEq)]
struct Key {
    path: PathBuf,
    blur: u8,
}

/// The wallpaper of the channel on show. Only that one is kept, keyed by the
/// file and the blur, so a result that arrives after the channel or the blur
/// changed is dropped.
#[derive(Default)]
pub(crate) struct Wallpaper {
    slot: Option<(Key, State)>,
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
        let key = Key {
            path: PathBuf::from(&wanted.path),
            blur: wanted.blur,
        };
        let held = self.slot.as_ref().is_some_and(|(held, _)| *held == key);
        if !held {
            self.release(window);
            self.slot = Some((key.clone(), State::Loading));
            let loading = key.clone();
            let loaded = cx.background_spawn(async move {
                wallpaper::load(&loading.path, loading.blur, &CancelFlag::default())
            });
            cx.spawn(async move |this, cx| {
                let result = loaded.await;
                let _ = this.update(cx, |this, cx| {
                    let Some((held, state)) = this.wallpaper.slot.as_mut() else {
                        return;
                    };
                    if *held != key {
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
            // The picture is the backmost layer, the log background (with
            // its opacity) lies over it, and the rows over both. Cover and
            // center paint past the box, so the box clips them.
            Some((_, State::Ready(image))) => Some(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .overflow_hidden()
                    .child(
                        img(image.clone())
                            .size_full()
                            .object_fit(object_fit(wanted.fit)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(wanted.cover),
                    ),
            ),
            _ => None,
        }
    }

    fn release(&mut self, window: &mut Window) {
        if let Some((_, State::Ready(image))) = self.slot.take() {
            let _ = window.drop_image(image);
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's glob would shadow `#[test]`.
    use super::{
        ChannelLook, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, Theme, WallpaperFit,
        resolve,
    };
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
                background_opacity: Some(70),
                ..ChannelLook::default()
            };
            let resolved = resolve(Some(&look), base);
            assert_eq!(resolved.theme.panes.channel_log, rgb(0x102030));
            // The picture shows through the alternate rows.
            assert_eq!(resolved.theme.panes.channel_alternate.a, 0.5);
            assert_eq!(resolved.text, Some(rgb(0xAABBCC)));
            assert_eq!(resolved.font_size, 20);
            let wallpaper = resolved.wallpaper.unwrap();
            // The channel's own background lies over the picture, 70 % opaque.
            let cover = wallpaper.cover;
            assert_eq!((cover.r, cover.g, cover.b), {
                let c = rgb(0x102030);
                (c.r, c.g, c.b)
            });
            assert_eq!(cover.a, 0.7);
            // Nothing else of the theme moves.
            assert_eq!(resolved.theme.panes.combined_log, base.panes.combined_log);
            assert_eq!(resolved.theme.text, base.text);
        }
    }

    #[test]
    fn a_wallpaper_alone_gets_the_defaults() {
        for dark in [false, true] {
            let base = theme(dark);
            let look = ChannelLook {
                wallpaper: Some("/pic.png".into()),
                ..ChannelLook::default()
            };
            let wallpaper = resolve(Some(&look), base).wallpaper.unwrap();
            let log = base.panes.channel_log;
            assert_eq!(
                (wallpaper.cover.r, wallpaper.cover.g, wallpaper.cover.b),
                (log.r, log.g, log.b)
            );
            assert_eq!(
                wallpaper.cover.a,
                f32::from(DEFAULT_BACKGROUND_OPACITY) / 100.
            );
            assert_eq!(wallpaper.blur, DEFAULT_WALLPAPER_BLUR);
            assert_eq!(wallpaper.fit, WallpaperFit::Stretch);
        }
    }

    #[test]
    fn zero_is_kept_and_only_the_blur_decides_what_is_loaded_again() {
        let look = |blur, opacity, fit| ChannelLook {
            wallpaper: Some("/pic.png".into()),
            wallpaper_blur: Some(blur),
            background_opacity: Some(opacity),
            wallpaper_fit: Some(fit),
            ..ChannelLook::default()
        };
        let base = theme(false);
        let first = resolve(Some(&look(0, 0, WallpaperFit::Contain)), base)
            .wallpaper
            .unwrap();
        assert_eq!((first.blur, first.cover.a), (0, 0.));
        assert_eq!(first.fit, WallpaperFit::Contain);
        // A color, an opacity and a fit differ without touching the blur.
        let second = resolve(Some(&look(0, 100, WallpaperFit::Cover)), base)
            .wallpaper
            .unwrap();
        assert_eq!(second.blur, first.blur);
        assert_ne!((second.cover, second.fit), (first.cover, first.fit));
    }
}
