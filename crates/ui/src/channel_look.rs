// SPDX-License-Identifier: GPL-3.0-only
//! The look of one channel's log (#928): the channel's own font size, colors
//! and wallpaper over the global appearance, and the loading of the
//! wallpapers of the four areas (#962).

use std::{cell::Cell, path::PathBuf, rc::Rc, sync::Arc};

use cayenchat_media::{CancelFlag, wallpaper, wallpaper::Tone};
use cayenchat_storage::{
    ChannelLook, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, MAX_WALLPAPER_BLUR,
    MAX_WALLPAPER_SATURATION, RegionWallpaper, WallpaperFit, WallpaperRegion,
    clamp_wallpaper_brightness, color_value,
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
    /// The picture and the pane background over it, drawn by [`backdrop`] or
    /// by each row. The pane's own opacity is `theme.panes.channel_log.a`.
    pub(crate) background: Layers,
}

/// How a pane shows its wallpaper: the mix of picture and background color
/// (the wallpaper's setting) and the opacity of the pane as a whole (#930,
/// the desktop showing through) are two independent things. The mix is made
/// first and the pane's opacity applies to the result, which painting two
/// layers over the desktop gets exactly with these alphas: the color `c`
/// with `p·b` and the picture with `p·(1−b) / (1−p·b)` under it, for the
/// pane opacity `p` and the color's share `b` (#989). So changing `p` never
/// moves the mix, and changing `b` never moves the total opacity `p`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Layers {
    /// The pane background; its alpha is the layer's.
    pub(crate) color: Rgba,
    /// The opacity of the picture under the color.
    pub(crate) picture: f32,
}

impl Layers {
    /// Without a picture: the pane color as it is.
    pub(crate) fn plain(color: Rgba) -> Self {
        Self { color, picture: 1. }
    }

    /// `pane` (its alpha the pane's opacity `p`) over a picture, with
    /// `percent` of the mix the color.
    pub(crate) fn mixed(pane: Rgba, percent: u8) -> Self {
        let share = f32::from(percent.min(100)) / 100.;
        let color_alpha = pane.a * share;
        let picture = if color_alpha >= 1. {
            0.
        } else {
            pane.a * (1. - share) / (1. - color_alpha)
        };
        Self {
            color: Rgba {
                a: color_alpha,
                ..pane
            },
            picture,
        }
    }

    /// How much of the desktop the two layers cover, `1 − (1 − picture)(1 −
    /// color)`: the pane's opacity `p`.
    #[cfg(test)]
    pub(crate) fn coverage(&self) -> f32 {
        1. - (1. - self.picture) * (1. - self.color.a)
    }
}

/// The wallpaper to show (#950): the file, its blur and tone and how it fits
/// the log. Only the file, the blur and the tone need a new load; the fit is
/// applied when it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WallpaperRequest {
    pub(crate) path: String,
    /// Blur sigma in px of the kept (≤ 640 px) picture; 0 is none.
    pub(crate) blur: u8,
    /// -100 to 100 (#1047).
    pub(crate) brightness: i8,
    /// Percent, 0 to 200.
    pub(crate) saturation: u8,
    pub(crate) fit: WallpaperFit,
}

/// The brightness a look sets for the theme `dark`, capped to the range.
pub(crate) fn own_brightness(look: &ChannelLook, dark: bool) -> Option<i8> {
    look.wallpaper_brightness(dark)
        .map(clamp_wallpaper_brightness)
}

/// The saturation a look sets for the theme `dark`, capped to the range.
pub(crate) fn own_saturation(look: &ChannelLook, dark: bool) -> Option<u8> {
    look.wallpaper_saturation(dark)
        .map(|saturation| saturation.min(MAX_WALLPAPER_SATURATION))
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
/// standard wallpaper `standard` in the theme `dark`: its picture, blur, tone,
/// fit, and its share for the background color. Just the pane color without a
/// picture.
pub(crate) fn region_wallpaper(
    standard: &RegionWallpaper,
    background: Rgba,
    dark: bool,
) -> (Option<WallpaperRequest>, Layers) {
    let Some(path) = &standard.path else {
        return (None, Layers::plain(background));
    };
    let request = WallpaperRequest {
        path: path.clone(),
        blur: standard.blur_tenths(),
        brightness: standard.brightness_value(dark),
        saturation: standard.saturation_percent(),
        fit: standard.fit.unwrap_or_default(),
    };
    (
        Some(request),
        Layers::mixed(background, standard.opacity_percent()),
    )
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
        Some(look) if look.no_wallpaper => (None, Layers::plain(theme.panes.channel_log)),
        Some(look) if look.wallpaper.is_some() => {
            // Unset tone follows the standard picture's, if there is one.
            let none = RegionWallpaper::default();
            let base = if standard.path.is_some() {
                standard
            } else {
                &none
            };
            let request = look.wallpaper.as_ref().map(|path| WallpaperRequest {
                path: path.clone(),
                blur: blur_of(look),
                brightness: own_brightness(look, theme.dark)
                    .unwrap_or_else(|| base.brightness_value(theme.dark)),
                saturation: own_saturation(look, theme.dark)
                    .unwrap_or_else(|| base.saturation_percent()),
                fit: look.wallpaper_fit.unwrap_or_default(),
            });
            let background = Layers::mixed(theme.panes.channel_log, background_opacity_of(look));
            (request, background)
        }
        _ => {
            // The standard picture; the channel's own strength settings
            // still apply to it.
            let (mut request, mut background) =
                region_wallpaper(standard, theme.panes.channel_log, theme.dark);
            if let (Some(look), Some(request)) = (look, request.as_mut()) {
                if let Some(opacity) = look.background_opacity {
                    background = Layers::mixed(theme.panes.channel_log, opacity);
                }
                if look.wallpaper_blur.is_some() {
                    request.blur = blur_of(look);
                }
                if let Some(brightness) = own_brightness(look, theme.dark) {
                    request.brightness = brightness;
                }
                if let Some(saturation) = own_saturation(look, theme.dark) {
                    request.saturation = saturation;
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

/// The parts of `bounds` the picture placed at `placed` does not cover: up to
/// four strips around the overlap, or all of `bounds` without one.
fn gaps(bounds: Bounds<Pixels>, placed: Bounds<Pixels>) -> Vec<Bounds<Pixels>> {
    let inner = bounds.intersect(&placed);
    if inner.size.width <= px(0.) || inner.size.height <= px(0.) {
        return vec![bounds];
    }
    let (b, i) = (bounds, inner);
    [
        Bounds::from_corners(b.origin, point(b.right(), i.origin.y)),
        Bounds::from_corners(point(b.origin.x, i.bottom()), b.bottom_right()),
        Bounds::from_corners(point(b.origin.x, i.origin.y), point(i.origin.x, i.bottom())),
        Bounds::from_corners(point(i.right(), i.origin.y), point(b.right(), i.bottom())),
    ]
    .into_iter()
    .filter(|gap| gap.size.width > px(0.) && gap.size.height > px(0.))
    .collect()
}

/// Paints the picture at `placed` in `bounds`, and `color` where it does not
/// reach (`contain`, `center`). The caller's opacity applies to both alike,
/// so the pane shows its opacity also beside the picture, whatever the mix.
fn paint_picture(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    placed: Bounds<Pixels>,
    image: Arc<RenderImage>,
    color: Rgba,
) {
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let color = Rgba { a: 1., ..color };
        for gap in gaps(bounds, placed) {
            window.paint_quad(fill(gap, color));
        }
        let _ = window.paint_image(placed, Corners::default(), image, 0, false);
    });
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
/// pane color of `layers` over it. It is the first child of a relative pane,
/// which paints no background of its own under it (the picture may be
/// see-through).
pub(crate) fn backdrop(picture: Option<Div>, layers: Layers) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .overflow_hidden()
        .children(picture.map(|picture| picture.opacity(layers.picture)))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(layers.color),
        )
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
    /// The opacities of the picture and of the row color.
    picture_alpha: f32,
    alpha: f32,
}

/// `row` with `color` as its background. With a `slice` it paints the picture
/// and the color over it at the pane's opacities; without one (no wallpaper,
/// or not loaded) the color is as `color` is.
pub(crate) fn alternate_row<T: Styled + ParentElement>(
    row: T,
    color: Rgba,
    slice: Option<&Slice>,
) -> T {
    let Some(slice) = slice else {
        return row.bg(color);
    };
    let (image, placed) = (slice.image.clone(), slice.placed.clone());
    let picture = canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            paint_picture(window, bounds, placed.get(), image, color);
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
    row.child(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .opacity(slice.picture_alpha)
            .child(picture),
    )
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
    brightness: i8,
    saturation: u8,
}

/// The wallpapers on show, one slot per area (#962).
#[derive(Default)]
pub(crate) struct Wallpapers {
    pub(crate) channel_log: Wallpaper,
    pub(crate) combined_log: Wallpaper,
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

    pub(crate) fn get_mut(&mut self, region: WallpaperRegion) -> &mut Wallpaper {
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
        layers: Layers,
        translucent: bool,
        window: &mut Window,
        cx: &mut Context<ChatWindow>,
    ) -> Option<Div> {
        self.get_mut(region).element(
            wanted,
            layers,
            translucent,
            window,
            cx,
            move |this: &mut ChatWindow| this.wallpapers.get_mut(region),
        )
    }
}

/// The wallpaper of one area. Only the one on show is kept, keyed by the
/// file, the blur and the tone, so a result that arrives after the channel or
/// one of those changed is dropped.
#[derive(Default)]
pub(crate) struct Wallpaper {
    slot: Option<(Key, State)>,
    /// Cancels the load of the slot's key. `release` sets it and takes a new
    /// one, so a replaced or removed wallpaper's load stops at its next check
    /// and its result is never applied.
    cancel: CancelFlag,
    placed: Rc<Cell<Bounds<Pixels>>>,
}

impl Wallpaper {
    /// The picture for alternate rows, with the opacities of the pane's
    /// `layers`, when it is shown.
    pub(crate) fn slice(&self, layers: Layers) -> Option<Slice> {
        match self.slot.as_ref() {
            Some((_, State::Ready(image))) => Some(Slice {
                image: image.clone(),
                placed: self.placed.clone(),
                picture_alpha: layers.picture,
                alpha: layers.color.a,
            }),
            _ => None,
        }
    }

    /// The picture for `wanted`, as the first child of the log; starts its
    /// loading when it is not the one held. Nothing while it loads or when
    /// the file cannot be used.
    /// `slot_of` finds this wallpaper again in its owner `T` (the chat window,
    /// or the settings window's miniature) when the load ends.
    pub(crate) fn element<T: 'static>(
        &mut self,
        wanted: Option<&WallpaperRequest>,
        layers: Layers,
        translucent: bool,
        window: &mut Window,
        cx: &mut Context<T>,
        slot_of: impl Fn(&mut T) -> &mut Wallpaper + 'static,
    ) -> Option<Div> {
        let Some(wanted) = wanted else {
            self.release(window);
            return None;
        };
        let key = Key {
            path: PathBuf::from(&wanted.path),
            blur: wanted.blur,
            brightness: wanted.brightness,
            saturation: wanted.saturation,
        };
        let held = self.slot.as_ref().is_some_and(|(held, _)| *held == key);
        if !held {
            self.release(window);
            self.slot = Some((key.clone(), State::Loading));
            let loading = key.clone();
            let cancel = self.cancel.clone();
            let task_cancel = cancel.clone();
            let loaded = cx.background_spawn(async move {
                let tone = Tone {
                    brightness: loading.brightness,
                    saturation: loading.saturation,
                };
                wallpaper::load(&loading.path, loading.blur, tone, &task_cancel)
            });
            cx.spawn(async move |this, cx| {
                let result = loaded.await;
                // Keyed by this load's own flag: a slot released and filled
                // again with the same key must not take this stale result.
                if cancel.is_cancelled() {
                    return;
                }
                let _ = this.update(cx, |this, cx| {
                    let Some((held, state)) = slot_of(this).slot.as_mut() else {
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
                let color = layers.color;
                let picture = div().absolute().top_0().left_0().size_full().child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, (), window, _| {
                            let size = image.size(0);
                            let placed = fit_bounds(fit, bounds, size);
                            shared.set(placed);
                            if !translucent {
                                paint_picture(window, bounds, placed, image, color);
                            }
                        },
                    )
                    .size_full(),
                );
                Some(if translucent {
                    picture
                } else {
                    backdrop(Some(picture), layers)
                })
            }
            _ => None,
        }
    }

    fn release(&mut self, window: &mut Window) {
        // Called on every frame a pane has no wallpaper: only a slot has a
        // load to cancel.
        let Some((_, state)) = self.slot.take() else {
            return;
        };
        self.cancel.cancel();
        self.cancel = CancelFlag::default();
        if let State::Ready(image) = state {
            let _ = window.drop_image(image);
        }
    }
}

impl Drop for Wallpaper {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's glob would shadow `#[test]`.
    use super::{
        ChannelLook, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, Layers, RegionWallpaper,
        Theme, WallpaperFit, region_wallpaper, resolve,
    };
    use cayenchat_storage::{Appearance, ThemeMode};
    use gpui::{Rgba, WindowAppearance, rgb};

    fn standard(path: Option<&str>, opacity: Option<u8>) -> RegionWallpaper {
        RegionWallpaper {
            path: path.map(Into::into),
            opacity,
            blur: None,
            brightness: None,
            saturation: None,
            fit: None,
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
        assert_eq!(resolved.background.color.a, 0.4);
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
            let alpha = resolved.background.color.a;
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
        assert!((inherited.background.color.a - 0.3).abs() < 1e-6);
        let own = ChannelLook {
            background_opacity: Some(DEFAULT_BACKGROUND_OPACITY),
            ..ChannelLook::default()
        };
        let resolved = resolve(Some(&own), &standard, base);
        let want = f32::from(DEFAULT_BACKGROUND_OPACITY) / 100.;
        assert!((resolved.background.color.a - want).abs() < 1e-6);
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
            let cover = resolved.background.color;
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
                    resolved.background.color.r,
                    resolved.background.color.g,
                    resolved.background.color.b
                ),
                (log.r, log.g, log.b)
            );
            assert_eq!(
                resolved.background.color.a,
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
            (
                resolved.wallpaper.map(|w| w.path),
                resolved.background.color.a,
            )
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
        let (request, background) = region_wallpaper(&none(), base.panes.member_list, true);
        assert!(request.is_none());
        assert_eq!(background, Layers::plain(base.panes.member_list));
        let (request, background) = region_wallpaper(
            &standard(Some("/members.png"), None),
            base.panes.member_list,
            true,
        );
        assert_eq!(request.unwrap().path, "/members.png");
        assert_eq!(
            background.color.a,
            f32::from(DEFAULT_BACKGROUND_OPACITY) / 100.
        );
    }

    #[test]
    fn the_gaps_around_a_picture_tile_the_rest_of_the_box() {
        use gpui::{Bounds, point, px, size};
        let b = |x, y, w, h| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let area = |gaps: &[Bounds<gpui::Pixels>]| {
            gaps.iter()
                .map(|g| f32::from(g.size.width) * f32::from(g.size.height))
                .sum::<f32>()
        };
        let row = b(0., 0., 100., 20.);
        // Centered picture: left and right gaps.
        let gaps = super::gaps(row, b(30., -10., 40., 40.));
        assert_eq!(area(&gaps), 100. * 20. - 40. * 20.);
        // Covering picture: no gap.
        assert!(super::gaps(row, b(-10., -10., 200., 200.)).is_empty());
        // Picture elsewhere: all of the box.
        assert_eq!(area(&super::gaps(row, b(0., 50., 10., 10.))), 2000.);
        // Picture inside: four strips.
        assert_eq!(area(&super::gaps(row, b(10., 5., 20., 10.))), 2000. - 200.);
    }

    /// What the desktop shows through `layers`: the picture, then the color,
    /// each drawn "over" what is below, as the GPU does.
    fn painted(layers: Layers, picture: f32, color: f32, desktop: f32) -> f32 {
        let over = |below: f32, above: f32, alpha: f32| below * (1. - alpha) + above * alpha;
        over(
            over(desktop, picture, layers.picture),
            color,
            layers.color.a,
        )
    }

    #[test]
    fn the_wallpaper_mix_and_the_pane_opacity_are_independent() {
        let (picture, color, desktop) = (0.9, 0.1, 0.5);
        for pane_opacity in [1., 0.8, 0.5, 0.2, 0.] {
            for share in [0u8, 1, 30, 50, 80, 99, 100] {
                let pane = Rgba {
                    a: pane_opacity,
                    ..rgb(0x000000)
                };
                let layers = Layers::mixed(pane, share);
                // The picture and the color mixed first, then the pane's
                // opacity applied to that mix.
                let b = f32::from(share) / 100.;
                let mix = picture * (1. - b) + color * b;
                let want = mix * pane_opacity + desktop * (1. - pane_opacity);
                let got = painted(layers, picture, color, desktop);
                assert!(
                    (got - want).abs() < 1e-5,
                    "pane {pane_opacity}, share {share}: {got} != {want}"
                );
                assert!((layers.coverage() - pane_opacity).abs() < 1e-5);
            }
        }
        // The pane's opacity alone never moves the mix: over a desktop of
        // the picture's own color the result is the mix at any opacity.
        let at = |pane_opacity| {
            let pane = Rgba {
                a: pane_opacity,
                ..rgb(0x000000)
            };
            painted(Layers::mixed(pane, 50), 0.8, 0.2, 0.5)
        };
        assert!((at(1.) - 0.5).abs() < 1e-5 && (at(0.4) - 0.5).abs() < 1e-5);
        // An opaque pane is the plain stack: the picture opaque, the color
        // over it at the share.
        let opaque = Layers::mixed(rgb(0x102030), 80);
        assert_eq!((opaque.picture, opaque.color.a), (1., 0.8));
        // A hidden picture draws nothing.
        assert_eq!(Layers::mixed(rgb(0x102030), 100).picture, 0.);
    }

    #[test]
    fn a_standard_wallpaper_carries_its_fit_and_a_channel_may_override_it() {
        let contained = RegionWallpaper {
            fit: Some(WallpaperFit::Contain),
            ..standard(Some("/pic.png"), None)
        };
        let base = theme(false);
        let (request, _) = region_wallpaper(&contained, base.panes.channel_tree, true);
        assert_eq!(request.unwrap().fit, WallpaperFit::Contain);
        assert_eq!(
            resolve(None, &contained, base).wallpaper.unwrap().fit,
            WallpaperFit::Contain
        );
        let own = ChannelLook {
            wallpaper_fit: Some(WallpaperFit::Center),
            ..ChannelLook::default()
        };
        assert_eq!(
            resolve(Some(&own), &contained, base).wallpaper.unwrap().fit,
            WallpaperFit::Center
        );
    }

    #[test]
    fn a_standard_wallpaper_carries_its_blur() {
        let blurred = RegionWallpaper {
            blur: Some(12),
            ..standard(Some("/pic.png"), None)
        };
        let base = theme(false);
        let (request, _) = region_wallpaper(&blurred, base.panes.channel_tree, true);
        assert_eq!(request.unwrap().blur, 12);
        // A channel without its own blur follows it; its own blur wins.
        assert_eq!(resolve(None, &blurred, base).wallpaper.unwrap().blur, 12);
        let own = ChannelLook {
            wallpaper_blur: Some(3),
            ..ChannelLook::default()
        };
        assert_eq!(
            resolve(Some(&own), &blurred, base).wallpaper.unwrap().blur,
            3
        );
        // An explicit 0 is a blur of its own, not "inherit".
        let none = ChannelLook {
            wallpaper_blur: Some(0),
            ..ChannelLook::default()
        };
        assert_eq!(
            resolve(Some(&none), &blurred, base).wallpaper.unwrap().blur,
            0
        );
    }

    #[test]
    fn the_tone_follows_the_standard_then_the_channel_and_the_theme_default_alone() {
        let tone = |request: Option<super::WallpaperRequest>| {
            let request = request.unwrap();
            (request.brightness, request.saturation)
        };
        // Unset: the default of the theme on show.
        for (dark, brightness) in [(true, -40), (false, 40)] {
            let base = theme(dark);
            let plain = standard(Some("/pic.png"), None);
            assert_eq!(
                tone(resolve(None, &plain, base).wallpaper),
                (brightness, 80)
            );
            // A channel's own picture over no standard one: the same.
            let own = ChannelLook {
                wallpaper: Some("/own.png".into()),
                ..ChannelLook::default()
            };
            assert_eq!(
                tone(resolve(Some(&own), &none(), base).wallpaper),
                (brightness, 80)
            );
        }
        // The standard's values are inherited, and the channel's win.
        let toned = RegionWallpaper {
            brightness: Some(-10),
            saturation: Some(150),
            ..standard(Some("/pic.png"), None)
        };
        let base = theme(false);
        assert_eq!(tone(resolve(None, &toned, base).wallpaper), (-10, 150));
        let own = ChannelLook {
            wallpaper_brightness_light: Some(0),
            ..ChannelLook::default()
        };
        assert_eq!(tone(resolve(Some(&own), &toned, base).wallpaper), (0, 150));
        let own = ChannelLook {
            wallpaper_saturation_light: Some(0),
            ..ChannelLook::default()
        };
        assert_eq!(tone(resolve(Some(&own), &toned, base).wallpaper), (-10, 0));
        // A channel's own picture follows the standard picture's tone when
        // it has none of its own set, in both themes.
        for dark in [true, false] {
            let base = theme(dark);
            let own = ChannelLook {
                wallpaper: Some("/own.png".into()),
                ..ChannelLook::default()
            };
            assert_eq!(
                tone(resolve(Some(&own), &toned, base).wallpaper),
                (-10, 150)
            );
            let own = ChannelLook {
                wallpaper_brightness_light: (!dark).then_some(100),
                wallpaper_brightness_dark: dark.then_some(100),
                ..own
            };
            assert_eq!(
                tone(resolve(Some(&own), &toned, base).wallpaper),
                (100, 150)
            );
        }
        // A value is for its own theme: the other theme keeps the standard's
        // (the same picture toned differently in light and dark).
        let own = ChannelLook {
            wallpaper: Some("/own.png".into()),
            wallpaper_brightness_dark: Some(-70),
            wallpaper_saturation_light: Some(10),
            ..ChannelLook::default()
        };
        assert_eq!(
            tone(resolve(Some(&own), &toned, theme(true)).wallpaper),
            (-70, 150)
        );
        assert_eq!(
            tone(resolve(Some(&own), &toned, theme(false)).wallpaper),
            (-10, 10)
        );
        // Out of range values are capped.
        let wild = ChannelLook {
            wallpaper: Some("/own.png".into()),
            wallpaper_brightness_light: Some(i8::MIN),
            wallpaper_saturation_light: Some(255),
            ..ChannelLook::default()
        };
        assert_eq!(
            tone(resolve(Some(&wild), &none(), base).wallpaper),
            (-100, 200)
        );
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
        assert_eq!((first.blur, first_look.background.color.a), (0, 0.));
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
