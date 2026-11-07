// SPDX-License-Identifier: GPL-3.0-only
//! The look of one channel's log (#928): the channel's own font size, colors
//! and wallpaper over the global appearance, and the loading of the
//! wallpapers of the four areas (#962).

use std::{path::PathBuf, sync::Arc};

use cayenchat_media::{CancelFlag, wallpaper};
use cayenchat_storage::{ChannelLook, RegionWallpaper, WallpaperRegion, color_value};
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
}

/// The wallpaper to show: the file, the log background it is tinted toward
/// and how much of it shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WallpaperRequest {
    pub(crate) path: String,
    pub(crate) tint: u32,
    pub(crate) opacity: f32,
}

/// `look` laid over `theme`, with `standard` the channel log's wallpaper
/// for the theme. With a wallpaper the alternate rows are drawn half
/// transparent, so the picture shows through them.
pub(crate) fn resolve(
    look: Option<&ChannelLook>,
    standard: &RegionWallpaper,
    mut theme: Theme,
) -> ResolvedLook {
    let color = |value: &Option<String>| value.as_deref().and_then(color_value).map(rgb);
    if let Some(look) = look {
        if let Some(background) = color(&look.background) {
            theme.panes.channel_log = background;
        }
        if let Some(alternate) = color(&look.alternate) {
            theme.panes.channel_alternate = alternate;
        }
    }
    let has_wallpaper = match look {
        Some(look) if look.no_wallpaper => false,
        Some(look) if look.wallpaper.is_some() => true,
        _ => standard.path.is_some(),
    };
    if has_wallpaper {
        theme.panes.channel_alternate.a = 0.5;
    }
    ResolvedLook {
        text: look.and_then(|look| color(&look.text)),
        font_size: look.map_or(0, |look| look.font_size),
        theme,
    }
}

/// The channel log's wallpaper to load: the channel's own picture, none when
/// the channel switched it off, or else the standard one. The channel's
/// strength setting applies to whichever shows. `background` is the log's
/// background after the channel's colors, which the picture is tinted toward.
pub(crate) fn channel_wallpaper(
    look: Option<&ChannelLook>,
    standard: &RegionWallpaper,
    background: Rgba,
) -> Option<WallpaperRequest> {
    let Some(look) = look else {
        return region_wallpaper(standard, background);
    };
    if look.no_wallpaper {
        return None;
    }
    let (path, standard_opacity) = match &look.wallpaper {
        Some(path) => (path, None),
        None => (standard.path.as_ref()?, standard.opacity),
    };
    Some(request(
        path,
        look.wallpaper_opacity.or(standard_opacity),
        background,
    ))
}

/// The standard wallpaper of one of the other areas.
pub(crate) fn region_wallpaper(
    standard: &RegionWallpaper,
    background: Rgba,
) -> Option<WallpaperRequest> {
    Some(request(
        standard.path.as_ref()?,
        standard.opacity,
        background,
    ))
}

fn request(path: &str, opacity: Option<u8>, background: Rgba) -> WallpaperRequest {
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    WallpaperRequest {
        path: path.to_owned(),
        tint: channel(background.r) << 16 | channel(background.g) << 8 | channel(background.b),
        opacity: f32::from(
            opacity
                .unwrap_or(cayenchat_storage::DEFAULT_WALLPAPER_OPACITY)
                .min(100),
        ) / 100.,
    }
}

/// Lets the picture show through the alternate rows of the combined log.
pub(crate) fn translucent_combined(theme: &mut Theme, standard: &RegionWallpaper) {
    if standard.path.is_some() {
        theme.panes.combined_alternate.a = 0.5;
    }
}

enum State {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

/// The wallpapers on show, one slot per area.
#[derive(Default)]
pub(crate) struct Wallpapers {
    channel_log: Wallpaper,
    combined_log: Wallpaper,
    member_list: Wallpaper,
    channel_tree: Wallpaper,
}

impl Wallpapers {
    fn get_mut(&mut self, region: WallpaperRegion) -> &mut Wallpaper {
        match region {
            WallpaperRegion::ChannelLog => &mut self.channel_log,
            WallpaperRegion::CombinedLog => &mut self.combined_log,
            WallpaperRegion::MemberList => &mut self.member_list,
            WallpaperRegion::ChannelTree => &mut self.channel_tree,
        }
    }

    /// The picture for `wanted` in `region`, as the first child of its pane.
    pub(crate) fn element(
        &mut self,
        region: WallpaperRegion,
        wanted: Option<&WallpaperRequest>,
        window: &mut Window,
        cx: &mut Context<ChatWindow>,
    ) -> Option<Div> {
        self.get_mut(region).element(region, wanted, window, cx)
    }
}

/// The wallpaper of one area. Only the one on show is kept, keyed by the
/// file and the color it was tinted toward, so a result that arrives after
/// the channel or the colors changed is dropped.
#[derive(Default)]
struct Wallpaper {
    slot: Option<(PathBuf, u32, State)>,
}

impl Wallpaper {
    /// The picture for `wanted`, as the first child of the log; starts its
    /// loading when it is not the one held. Nothing while it loads or when
    /// the file cannot be used.
    fn element(
        &mut self,
        region: WallpaperRegion,
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
                    let slot = &mut this.wallpapers.get_mut(region).slot;
                    let Some((held, held_tint, state)) = slot.as_mut() else {
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
                    .top_0()
                    .left_0()
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
    use super::{
        ChannelLook, RegionWallpaper, Theme, channel_wallpaper, region_wallpaper, resolve,
        translucent_combined,
    };
    use cayenchat_storage::{Appearance, DEFAULT_WALLPAPER_OPACITY, ThemeMode};
    use gpui::{WindowAppearance, rgb};

    fn standard(path: Option<&str>, opacity: Option<u8>) -> RegionWallpaper {
        RegionWallpaper {
            path: path.map(Into::into),
            opacity,
        }
    }

    fn none() -> RegionWallpaper {
        RegionWallpaper::default()
    }

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
            let resolved = resolve(None, &none(), base);
            assert_eq!(resolved.theme, base);
            assert!(resolved.text.is_none());
            assert_eq!(resolved.font_size, 0);
            assert!(channel_wallpaper(None, &none(), base.panes.channel_log).is_none());
            // A look that sets nothing is the same.
            let empty = ChannelLook::default();
            assert_eq!(resolve(Some(&empty), &none(), base).theme, base);
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
            let resolved = resolve(Some(&look), &none(), base);
            assert_eq!(resolved.theme.panes.channel_log, rgb(0x102030));
            // The picture shows through the alternate rows.
            assert_eq!(resolved.theme.panes.channel_alternate.a, 0.5);
            assert_eq!(resolved.text, Some(rgb(0xAABBCC)));
            assert_eq!(resolved.font_size, 20);

            let wallpaper =
                channel_wallpaper(Some(&look), &none(), resolved.theme.panes.channel_log).unwrap();
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
            let resolved = resolve(Some(&look), &none(), theme(dark));
            let wallpaper =
                channel_wallpaper(Some(&look), &none(), resolved.theme.panes.channel_log).unwrap();
            assert_eq!(wallpaper.tint, tint);
            assert_eq!(
                wallpaper.opacity,
                f32::from(DEFAULT_WALLPAPER_OPACITY) / 100.
            );
        }
    }

    #[test]
    fn a_channel_inherits_the_standard_wallpaper_unless_it_sets_or_switches_off_its_own() {
        let base = theme(false);
        let background = base.panes.channel_log;
        let standard = standard(Some("/standard.png"), Some(30));
        let path = |look: Option<&ChannelLook>| {
            channel_wallpaper(look, &standard, background).map(|w| (w.path, w.opacity))
        };

        // No look, or a look without a wallpaper choice, inherits.
        let inherit = ("/standard.png".to_owned(), 0.3);
        assert_eq!(path(None), Some(inherit.clone()));
        let recolored = ChannelLook {
            background: Some("#102030".into()),
            ..ChannelLook::default()
        };
        assert_eq!(path(Some(&recolored)), Some(inherit));
        assert_eq!(
            resolve(Some(&recolored), &standard, base)
                .theme
                .panes
                .channel_alternate
                .a,
            0.5
        );
        // The channel's own strength applies to the inherited picture.
        let stronger = ChannelLook {
            wallpaper_opacity: Some(80),
            ..ChannelLook::default()
        };
        assert_eq!(
            path(Some(&stronger)),
            Some(("/standard.png".to_owned(), 0.8))
        );
        // Its own picture replaces the standard one.
        let own = ChannelLook {
            wallpaper: Some("/own.png".into()),
            ..ChannelLook::default()
        };
        // At the default strength: the standard's setting is not its own.
        assert_eq!(path(Some(&own)), Some(("/own.png".to_owned(), 0.4)));
        // No wallpaper beats the standard one, and the rows turn opaque again.
        let off = ChannelLook {
            no_wallpaper: true,
            ..ChannelLook::default()
        };
        assert_eq!(path(Some(&off)), None);
        let resolved = resolve(Some(&off), &standard, base);

        assert_eq!(resolved.theme.panes.channel_alternate.a, 1.);
        assert_eq!(
            resolve(None, &standard, base)
                .theme
                .panes
                .channel_alternate
                .a,
            0.5
        );
    }

    #[test]
    fn the_other_areas_use_their_standard_wallpaper_tinted_toward_their_own_background() {
        let base = theme(true);
        assert!(region_wallpaper(&none(), base.panes.member_list).is_none());
        let wallpaper = region_wallpaper(
            &standard(Some("/members.png"), None),
            base.panes.member_list,
        )
        .unwrap();
        assert_eq!(wallpaper.tint, 0x1F2124);
        assert_eq!(
            wallpaper.opacity,
            f32::from(DEFAULT_WALLPAPER_OPACITY) / 100.
        );
        let tree = region_wallpaper(
            &standard(Some("/t.png"), Some(100)),
            base.panes.channel_tree,
        )
        .unwrap();
        assert_eq!((tree.tint, tree.opacity), (0x1D2630, 1.));

        let mut combined = base;
        translucent_combined(&mut combined, &none());
        assert_eq!(combined, base);
        translucent_combined(&mut combined, &standard(Some("/c.png"), None));
        assert_eq!(combined.panes.combined_alternate.a, 0.5);
    }
}
