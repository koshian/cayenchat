// SPDX-License-Identifier: GPL-3.0-only
//! The look of one channel's log (#928): the channel's own font size, colors
//! and wallpaper over the global appearance, and the loading of the
//! wallpapers of the four areas (#962).

use std::{cell::Cell, path::PathBuf, rc::Rc, sync::Arc};

use cayenchat_media::{CancelFlag, wallpaper};
use cayenchat_storage::{
    ChannelLook, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, MAX_WALLPAPER_BLUR,
    RegionWallpaper, WallpaperFit, WallpaperRegion, color_value,
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
    /// The pane background with the one effective opacity as its alpha (the
    /// pane's times the wallpaper's), drawn over the wallpaper by [`backdrop`]
    /// or by each row. The pane's own opacity is `theme.panes.channel_log.a`.
    pub(crate) background: Rgba,
}

/// The wallpaper to show (#950): the file, its blur and how it fits the log.
/// Only the file and the blur need a new load; the fit is applied when it is
/// drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WallpaperRequest {
    pub(crate) path: String,
    /// Blur sigma in px of the kept (≤ 640 px) picture; 0 is none.
    pub(crate) blur: u8,
    pub(crate) fit: WallpaperFit,
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

/// The wallpaper and the pane background over it for one area, from its
/// standard wallpaper `standard`: its picture, the default blur and fit, and
/// its opacity for the background color. Opaque without a picture.
pub(crate) fn region_wallpaper(
    standard: &RegionWallpaper,
    background: Rgba,
) -> (Option<WallpaperRequest>, Rgba) {
    let Some(path) = &standard.path else {
        return (None, background);
    };
    let request = WallpaperRequest {
        path: path.clone(),
        blur: DEFAULT_WALLPAPER_BLUR,
        fit: WallpaperFit::default(),
    };
    // The pane's own opacity (#930) times the wallpaper's (#950).
    let background = Rgba {
        a: background.a * f32::from(standard.opacity_percent()) / 100.,
        ..background
    };
    (Some(request), background)
}

/// `look` laid over `theme`, with `standard` the channel log's wallpaper for
/// the theme. A channel shows its own picture, none when it switched the
/// wallpaper off, or else the standard one (#962). With a wallpaper the
/// background gets the opacity as its alpha, so the picture shows through.
pub(crate) fn resolve(
    look: Option<&ChannelLook>,
    standard: &RegionWallpaper,
    mut theme: Theme,
) -> ResolvedLook {
    let color = |value: &Option<String>| value.as_deref().and_then(color_value).map(rgb);
    if let Some(look) = look {
        // A channel's color keeps the opacity of the log pane (#930), so the
        // pane stays see-through with its own color.
        if let Some(background) = color(&look.background) {
            theme.panes.channel_log = Rgba {
                a: theme.panes.channel_log.a,
                ..background
            };
        }
        if let Some(alternate) = color(&look.alternate) {
            theme.panes.channel_alternate = Rgba {
                a: theme.panes.channel_alternate.a,
                ..alternate
            };
        }
    }
    let (wallpaper, background) = match look {
        Some(look) if look.no_wallpaper => (None, theme.panes.channel_log),
        Some(look) if look.wallpaper.is_some() => {
            let request = look.wallpaper.as_ref().map(|path| WallpaperRequest {
                path: path.clone(),
                blur: blur_of(look),
                fit: look.wallpaper_fit.unwrap_or_default(),
            });
            let background = Rgba {
                a: theme.panes.channel_log.a * f32::from(background_opacity_of(look)) / 100.,
                ..theme.panes.channel_log
            };
            (request, background)
        }
        _ => {
            // The standard picture; the channel's own strength settings
            // still apply to it.
            let (mut request, mut background) = region_wallpaper(standard, theme.panes.channel_log);
            if let (Some(look), Some(request)) = (look, request.as_mut()) {
                if let Some(opacity) = look.background_opacity {
                    background.a = theme.panes.channel_log.a * f32::from(opacity.min(100)) / 100.;
                }
                if look.wallpaper_blur.is_some() {
                    request.blur = blur_of(look);
                }
                request.fit = look.wallpaper_fit.unwrap_or(request.fit);
            }
            (request, background)
        }
    };
    ResolvedLook {
        background,
        wallpaper,
        text: look.and_then(|look| color(&look.text)),
        font_size: look.map_or(0, |look| look.font_size),
        theme,
    }
}

/// Lets the picture show through the alternate rows of the combined log. The
/// pane is opaque under the picture's backdrop, which carries its opacity.
pub(crate) fn translucent_combined(theme: &mut Theme, standard: &RegionWallpaper) {
    if standard.path.is_some() {
        theme.panes.combined_log.a = 1.;
        theme.panes.combined_alternate.a = 0.5;
    }
}

/// Where a picture of `image` pixels lies in `bounds` for `fit`. Cover and
/// center paint past `bounds`; the caller clips. The box is laid out by the
/// caller, not by an image element, which would force the picture's shape on
/// it.
fn fit_bounds(
    fit: WallpaperFit,
    bounds: Bounds<Pixels>,
    image: Size<DevicePixels>,
) -> Bounds<Pixels> {
    let object_fit = match fit {
        WallpaperFit::Stretch => ObjectFit::Fill,
        WallpaperFit::Cover => ObjectFit::Cover,
        WallpaperFit::Contain => ObjectFit::Contain,
        WallpaperFit::Center => {
            let size = image.map(|side| Pixels::from(u32::from(side)));
            return Bounds {
                origin: point(
                    bounds.origin.x + (bounds.size.width - size.width) / 2.,
                    bounds.origin.y + (bounds.size.height - size.height) / 2.,
                ),
                size,
            };
        }
    };
    object_fit.get_bounds(bounds, image)
}

/// The layer behind a pane's content: the picture, if any, and over it the
/// pane `background` (with its alpha). It is the first child of a relative
/// pane.
pub(crate) fn backdrop(picture: Option<Div>, background: Rgba) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .overflow_hidden()
        .children(picture)
        .child(div().absolute().top_0().left_0().size_full().bg(background))
}

/// The picture as a row needs it on a translucent pane, which has no backdrop
/// layer (it would add its opacity under the rows): each row, and the filler
/// below a short log, paints the picture over its own area and its color over
/// that, so the color's opacity is applied once, also where the picture does
/// not reach.
#[derive(Clone)]
pub(crate) struct Slice {
    image: Arc<RenderImage>,
    /// Where the pane placed the picture, set while it paints.
    placed: Rc<Cell<Bounds<Pixels>>>,
    /// The opacity of the row color.
    alpha: f32,
}

/// `row` with `color` as its background. With a `slice` it paints the picture
/// and the color over it at the pane's opacity; without one (no wallpaper, or
/// not loaded) the color is as `color` is.
pub(crate) fn alternate_row(row: Div, color: Rgba, slice: Option<&Slice>) -> Div {
    let Some(slice) = slice else {
        return row.bg(color);
    };
    let (image, placed) = (slice.image.clone(), slice.placed.clone());
    let picture = canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                let _ = window.paint_image(placed.get(), Corners::default(), image, 0, false);
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let color = Rgba {
        a: slice.alpha,
        ..color
    };
    row.child(picture)
        .child(div().absolute().top_0().left_0().size_full().bg(color))
}

/// The four strips of the padding around a translucent log's rows
/// (`FRAME_X` left and right, `FRAME_Y` above and below). Each paints like a
/// row does (`alternate_row`), so the frame shows the picture and the color
/// at the same opacity as the rows. The side strips stop short of the top and
/// bottom ones, so no pixel is painted twice.
pub(crate) const FRAME_X: f32 = 8.;
pub(crate) const FRAME_Y: f32 = 4.;

pub(crate) fn frame(color: Rgba, slice: Option<&Slice>) -> [Div; 4] {
    let strip = || div().absolute();
    [
        strip().top_0().left_0().right_0().h(px(FRAME_Y)),
        strip().bottom_0().left_0().right_0().h(px(FRAME_Y)),
        strip()
            .top(px(FRAME_Y))
            .bottom(px(FRAME_Y))
            .left_0()
            .w(px(FRAME_X)),
        strip()
            .top(px(FRAME_Y))
            .bottom(px(FRAME_Y))
            .right_0()
            .w(px(FRAME_X)),
    ]
    .map(|strip| alternate_row(strip, color, slice))
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

/// The wallpapers on show, one slot per area (#962).
#[derive(Default)]
pub(crate) struct Wallpapers {
    pub(crate) channel_log: Wallpaper,
    combined_log: Wallpaper,
    member_list: Wallpaper,
    channel_tree: Wallpaper,
}

impl Wallpapers {
    /// Whether no area holds or loads a file: `wallpaper::load` is the only
    /// reader of a wallpaper path and runs only after a slot is set.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        [
            &self.channel_log,
            &self.combined_log,
            &self.member_list,
            &self.channel_tree,
        ]
        .iter()
        .all(|wallpaper| wallpaper.slot.is_none())
    }

    fn get_mut(&mut self, region: WallpaperRegion) -> &mut Wallpaper {
        match region {
            WallpaperRegion::ChannelLog => &mut self.channel_log,
            WallpaperRegion::CombinedLog => &mut self.combined_log,
            WallpaperRegion::MemberList => &mut self.member_list,
            WallpaperRegion::ChannelTree => &mut self.channel_tree,
        }
    }

    /// The picture for `wanted` in `region` with `background` over it, as
    /// the first child of its pane.
    pub(crate) fn element(
        &mut self,
        region: WallpaperRegion,
        wanted: Option<&WallpaperRequest>,
        background: Rgba,
        translucent: bool,
        window: &mut Window,
        cx: &mut Context<ChatWindow>,
    ) -> Option<Div> {
        self.get_mut(region)
            .element(region, wanted, background, translucent, window, cx)
    }
}

/// The wallpaper of one area. Only the one on show is kept, keyed by the
/// file and the blur, so a result that arrives after the channel or the blur
/// changed is dropped.
#[derive(Default)]
pub(crate) struct Wallpaper {
    slot: Option<(Key, State)>,
    placed: Rc<Cell<Bounds<Pixels>>>,
}

impl Wallpaper {
    /// The picture for alternate rows, with the opacity of the pane
    /// `background` as the row color's alpha, when it is shown.
    pub(crate) fn slice(&self, background: Rgba) -> Option<Slice> {
        match self.slot.as_ref() {
            Some((_, State::Ready(image))) => Some(Slice {
                image: image.clone(),
                placed: self.placed.clone(),
                alpha: background.a,
            }),
            _ => None,
        }
    }

    /// The picture for `wanted`, as the first child of the log; starts its
    /// loading when it is not the one held. Nothing while it loads or when
    /// the file cannot be used.
    fn element(
        &mut self,
        region: WallpaperRegion,
        wanted: Option<&WallpaperRequest>,
        background: Rgba,
        translucent: bool,
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
                    let Some((held, state)) = this.wallpapers.get_mut(region).slot.as_mut() else {
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
            // On an opaque pane the picture is the backmost layer, the pane
            // background lies over it, and the rows over both. On a
            // translucent one there is no such layer: it would put its
            // opacity under every row's. This only places the picture;
            // rows and filler paint it (`Slice`).
            Some((_, State::Ready(image))) => {
                let (image, fit, shared) = (image.clone(), wanted.fit, self.placed.clone());
                let picture = div().absolute().top_0().left_0().size_full().child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, (), window, _| {
                            let size = image.size(0);
                            let placed = fit_bounds(fit, bounds, size);
                            shared.set(placed);
                            if !translucent {
                                let _ =
                                    window.paint_image(placed, Corners::default(), image, 0, false);
                            }
                        },
                    )
                    .size_full(),
                );
                Some(if translucent {
                    picture
                } else {
                    backdrop(Some(picture), background)
                })
            }
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
        ChannelLook, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, RegionWallpaper, Theme,
        WallpaperFit, region_wallpaper, resolve, translucent_combined,
    };
    use cayenchat_storage::{Appearance, ThemeMode};
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
            assert!(resolved.wallpaper.is_none());
            // A look that sets nothing is the same.
            let empty = ChannelLook::default();
            assert_eq!(resolve(Some(&empty), &none(), base).theme, base);
        }
    }

    #[test]
    fn a_channel_color_keeps_the_pane_opacity() {
        let appearance = Appearance {
            channel_log_opacity: 40,
            ..Appearance::default()
        };
        let base = Theme::new(ThemeMode::Dark, WindowAppearance::Light, &appearance);
        let look = ChannelLook {
            background: Some("#22aa44".into()),
            alternate: Some("#aa2244".into()),
            ..ChannelLook::default()
        };
        let resolved = resolve(Some(&look), &none(), base);
        assert_eq!(resolved.theme.panes.channel_log.a, 0.4);
        assert_eq!(resolved.theme.panes.channel_alternate.a, 0.4);
        assert_eq!(resolved.theme.panes.channel_log.g, rgb(0x22aa44).g);
        // Without a wallpaper the background is the pane color itself.
        assert_eq!(resolved.background.a, 0.4);
    }

    #[test]
    fn a_wallpaper_and_a_pane_opacity_resolve_to_one_opacity() {
        let look = ChannelLook {
            wallpaper: Some("/pic.png".into()),
            background_opacity: Some(80),
            ..ChannelLook::default()
        };
        for (pane, want) in [(100, 0.8), (50, 0.4)] {
            let appearance = Appearance {
                channel_log_opacity: pane,
                ..Appearance::default()
            };
            let base = Theme::new(ThemeMode::Dark, WindowAppearance::Light, &appearance);
            let resolved = resolve(Some(&look), &none(), base);
            let alpha = resolved.background.a;
            assert!((alpha - want).abs() < 1e-6, "{pane} % gave {alpha}");
            // The pane's own opacity stays what decides its base.
            let own = f32::from(pane) / 100.;
            assert!((resolved.theme.panes.channel_log.a - own).abs() < 1e-6);
        }
    }

    #[test]
    fn an_own_default_opacity_beats_a_weaker_standard_one() {
        let base = theme(false);
        let standard = standard(Some("/std.png"), Some(30));
        let inherited = resolve(None, &standard, base);
        assert!((inherited.background.a - 0.3).abs() < 1e-6);
        let own = ChannelLook {
            background_opacity: Some(DEFAULT_BACKGROUND_OPACITY),
            ..ChannelLook::default()
        };
        let resolved = resolve(Some(&own), &standard, base);
        let want = f32::from(DEFAULT_BACKGROUND_OPACITY) / 100.;
        assert!((resolved.background.a - want).abs() < 1e-6);
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
            let resolved = resolve(Some(&look), &none(), base);
            let log = resolved.theme.panes.channel_log;
            assert_eq!((log.r, log.g, log.b), {
                let c = rgb(0x102030);
                (c.r, c.g, c.b)
            });
            // The theme keeps the pane's own opacity (100 %): the wallpaper's
            // is in `background`.
            assert_eq!(resolved.theme.panes.channel_log.a, 1.);
            assert_eq!(resolved.theme.panes.channel_alternate.a, 1.);
            assert_eq!(resolved.text, Some(rgb(0xAABBCC)));
            assert_eq!(resolved.font_size, 20);
            let wallpaper = resolved.wallpaper.unwrap();
            // The channel's own background lies over the picture, 70 % opaque.
            let _ = wallpaper;
            let cover = resolved.background;
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
            let resolved = resolve(Some(&look), &none(), base);
            let wallpaper = resolved.wallpaper.unwrap();
            let log = base.panes.channel_log;
            assert_eq!(
                (
                    resolved.background.r,
                    resolved.background.g,
                    resolved.background.b
                ),
                (log.r, log.g, log.b)
            );
            assert_eq!(
                resolved.background.a,
                f32::from(DEFAULT_BACKGROUND_OPACITY) / 100.
            );
            assert_eq!(wallpaper.blur, DEFAULT_WALLPAPER_BLUR);
            assert_eq!(wallpaper.fit, WallpaperFit::default());
            assert_eq!(wallpaper.fit, WallpaperFit::Cover);
            assert_eq!(wallpaper.blur, 0);
        }
    }

    #[test]
    fn a_channel_inherits_the_standard_wallpaper_unless_it_sets_or_switches_off_its_own() {
        let base = theme(false);
        let standard = standard(Some("/standard.png"), Some(30));
        let shown = |look: Option<&ChannelLook>| {
            let resolved = resolve(look, &standard, base);
            (resolved.wallpaper.map(|w| w.path), resolved.background.a)
        };
        // No look, or a look without a wallpaper choice, inherits.
        let inherit = (Some("/standard.png".to_owned()), 0.3);
        assert_eq!(shown(None), inherit);
        let recolored = ChannelLook {
            background: Some("#102030".into()),
            ..ChannelLook::default()
        };
        assert_eq!(shown(Some(&recolored)), inherit);
        // The channel's own opacity applies to the inherited picture.
        let stronger = ChannelLook {
            background_opacity: Some(80),
            ..ChannelLook::default()
        };
        assert_eq!(
            shown(Some(&stronger)),
            (Some("/standard.png".to_owned()), 0.8)
        );
        // Its own picture replaces the standard one, at the default opacity.
        let own = ChannelLook {
            wallpaper: Some("/own.png".into()),
            ..ChannelLook::default()
        };
        assert_eq!(
            shown(Some(&own)),
            (
                Some("/own.png".to_owned()),
                f32::from(DEFAULT_BACKGROUND_OPACITY) / 100.
            )
        );
        // No wallpaper beats the standard one, and the background is opaque.
        let off = ChannelLook {
            no_wallpaper: true,
            ..ChannelLook::default()
        };
        assert_eq!(shown(Some(&off)), (None, 1.));
    }

    #[test]
    fn the_other_areas_use_their_standard_wallpaper_under_their_own_background() {
        let base = theme(true);
        let (request, background) = region_wallpaper(&none(), base.panes.member_list);
        assert!(request.is_none());
        assert_eq!(background, base.panes.member_list);
        let (request, background) = region_wallpaper(
            &standard(Some("/members.png"), None),
            base.panes.member_list,
        );
        assert_eq!(request.unwrap().path, "/members.png");
        assert_eq!(background.a, f32::from(DEFAULT_BACKGROUND_OPACITY) / 100.);
        let mut combined = base;
        translucent_combined(&mut combined, &none());
        assert_eq!(combined, base);
        translucent_combined(&mut combined, &standard(Some("/c.png"), None));
        assert_eq!(combined.panes.combined_alternate.a, 0.5);
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
        let first_look = resolve(Some(&look(0, 0, WallpaperFit::Contain)), &none(), base);
        let first = first_look.wallpaper.unwrap();
        assert_eq!((first.blur, first_look.background.a), (0, 0.));
        assert_eq!(first.fit, WallpaperFit::Contain);
        // A color, an opacity and a fit differ without touching the blur.
        let second_look = resolve(Some(&look(0, 100, WallpaperFit::Cover)), &none(), base);
        let second = second_look.wallpaper.unwrap();
        assert_eq!(second.blur, first.blur);
        assert_ne!(second_look.background, first_look.background);
        assert_ne!(second.fit, first.fit);
    }

    #[test]
    fn each_fit_places_the_picture_in_the_log_box() {
        use super::fit_bounds;
        use gpui::{Bounds, DevicePixels, point, px, size};
        let log = Bounds::new(point(px(10.), px(20.)), size(px(756.), px(351.)));
        let picture = size(DevicePixels(600), DevicePixels(360));
        assert_eq!(fit_bounds(WallpaperFit::Stretch, log, picture), log);
        // Cover: the width fills the box, the height runs over it equally.
        let cover = fit_bounds(WallpaperFit::Cover, log, picture);
        assert_eq!(cover.size.width, px(756.));
        assert!(cover.size.height > log.size.height);
        assert_eq!(cover.origin.x, log.origin.x);
        assert!(cover.origin.y < log.origin.y);
        // Contain: the height fills the box, the sides are left over.
        let contain = fit_bounds(WallpaperFit::Contain, log, picture);
        assert_eq!(contain.size.height, px(351.));
        assert!(contain.size.width < log.size.width);
        assert!(contain.origin.x > log.origin.x);
        // Center: the picture's own size, in the middle.
        let center = fit_bounds(WallpaperFit::Center, log, picture);
        assert_eq!(center.size, size(px(600.), px(360.)));
        assert_eq!(center.origin, point(px(88.), px(15.5)));
    }
}
