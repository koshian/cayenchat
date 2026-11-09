// SPDX-License-Identifier: GPL-3.0-only
//! The range selection on an image, shared by the avatar editor
//! (`avatar_editor`, a square) and the wallpaper chooser
//! (`channel_settings_dialog::wallpaper_crop`, any rectangle). It owns the
//! selection, the view onto the image, the drag (moving by the inside,
//! resizing by corners, and for a free rectangle by sides, also from just
//! outside), the close-up of a small selection and its drawing. The image
//! comes from `media::avatar_edit` (limits, EXIF orientation). The users
//! keep only what is theirs: where the image comes from, what is cut out of
//! the selection, and the buttons.
//!
//! When a drop leaves the selection under half of the image on both sides,
//! the view shows it centered with room around it (twice its size) so it can
//! be adjusted precisely, at once from the whole image and sharpened when its
//! own preview is made off the UI thread; otherwise the whole image is shown.

use std::{cell::Cell, rc::Rc, sync::Arc};

use cayenchat_media::{
    avatar_edit::{AvatarSource, Region},
    decode::{self, Thumbnail},
};
use gpui::{prelude::*, *};

/// Corner handles, drawn and hit, in logical pixels.
const HANDLE_SIDE: f32 = 10.0;
/// How far from a corner or side a press still grabs it, in logical pixels.
/// The margin around the picture is as wide, so an edge of the image can be
/// grabbed from just outside it.
const HANDLE_REACH: f64 = 9.0;
/// A selection under this share of the image on both sides is shown enlarged
/// once released.
const CLOSE_UP_BELOW: f64 = 0.5;

/// What shape the selection may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CropConstraint {
    /// Always a square, resized by its corners only; starts as the largest
    /// centered square.
    Square,
    /// Any rectangle, resized by its corners and sides; starts as the whole
    /// image.
    Free,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// Moving the selection: where the drag started, and the selection then.
    Move { start: (f64, f64), from: Region },
    /// Resizing from a corner or a side; the opposite corner `anchor` stays.
    /// `far` is the dragged corner as it was, and `follows` tells which of
    /// its axes the mouse moves (a side moves only one).
    Resize {
        anchor: (f64, f64),
        far: (f64, f64),
        follows: (bool, bool),
    },
}

pub(crate) struct ImageCropEditor {
    source: Arc<AvatarSource>,
    constraint: CropConstraint,
    /// The largest size of the view, in logical pixels.
    view_box: (u32, u32),
    /// The whole image, fitted into the view (twice its logical size).
    whole: Arc<RenderImage>,
    /// Displayed size of the whole image, in logical pixels.
    whole_view: Size<f32>,
    /// What will be cut out, in working pixels.
    region: Region,
    /// The enlarged part shown instead of the whole image, and its sharp
    /// preview once made.
    close_up: Option<(Region, Option<Arc<RenderImage>>)>,
    drag: Option<Drag>,
    /// Where the picture was last painted, to map the mouse into the image.
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl ImageCropEditor {
    pub(crate) fn new(
        source: AvatarSource,
        constraint: CropConstraint,
        view_box: (u32, u32),
    ) -> Self {
        let (width, height) = decode::fit(source.width(), source.height(), view_box.0, view_box.1);
        Self {
            whole: render_image(source.preview(view_box.0.max(view_box.1) * 2)),
            whole_view: size(width as f32, height as f32),
            region: initial_region(&source, constraint),
            source: Arc::new(source),
            constraint,
            view_box,
            close_up: None,
            drag: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    pub(crate) fn source(&self) -> Arc<AvatarSource> {
        self.source.clone()
    }

    /// What will be cut out.
    pub(crate) fn region(&self) -> Region {
        self.region
    }

    /// Whether the selection is where it starts, with the whole image shown.
    pub(crate) fn is_initial(&self) -> bool {
        self.close_up.is_none() && self.region == initial_region(&self.source, self.constraint)
    }

    /// Starts over: the initial selection on the whole image.
    pub(crate) fn reset(&mut self) {
        self.region = initial_region(&self.source, self.constraint);
        self.close_up = None;
        self.drag = None;
    }

    /// Replaces the selection by `region` kept inside the image (tests).
    #[cfg(test)]
    pub(crate) fn set_region(&mut self, region: Region) {
        self.region = self.source.clamp_region(region);
    }

    /// The part of the image shown: working-pixel origin, logical pixels per
    /// working pixel, and the view's logical size.
    fn view(&self) -> ((f64, f64), f64, Size<f32>) {
        match &self.close_up {
            Some((area, _)) => {
                let scale = (f64::from(self.view_box.0) / area.width)
                    .min(f64::from(self.view_box.1) / area.height);
                (
                    (area.x, area.y),
                    scale,
                    size((area.width * scale) as f32, (area.height * scale) as f32),
                )
            }
            None => (
                (0.0, 0.0),
                f64::from(self.whole_view.width) / f64::from(self.source.width()),
                self.whole_view,
            ),
        }
    }

    /// A mouse position as working-image pixels.
    fn to_image(&self, position: Point<Pixels>) -> (f64, f64) {
        let ((x, y), scale, _) = self.view();
        let origin = self.bounds.get().origin;
        (
            x + f64::from(f32::from(position.x - origin.x)) / scale,
            y + f64::from(f32::from(position.y - origin.y)) / scale,
        )
    }

    /// The part to show for a selection: the selection centered in twice its
    /// size when it is under half the image on both sides, else the whole
    /// image.
    fn close_up_for(&self, region: Region) -> Option<Region> {
        let small = region.width < f64::from(self.source.width()) * CLOSE_UP_BELOW
            && region.height < f64::from(self.source.height()) * CLOSE_UP_BELOW;
        small.then(|| {
            self.source.clamp_region(Region {
                x: region.x - region.width / 2.0,
                y: region.y - region.height / 2.0,
                width: region.width * 2.0,
                height: region.height * 2.0,
            })
        })
    }

    /// Starts a drag at `point` (working pixels): a corner resizes, for a
    /// free rectangle so does a side, the inside moves, elsewhere nothing
    /// happens.
    fn press(&mut self, point: (f64, f64)) {
        let reach = HANDLE_REACH / self.view().1;
        let Region {
            x,
            y,
            width,
            height,
        } = self.region;
        let (right, bottom) = (x + width, y + height);
        let near = |a: f64, b: f64, reach: f64| (a - b).abs() <= reach;
        // Corners first: the dragged corner, the opposite corner that stays,
        // and which axes follow.
        let corners = [
            ((x, y), (right, bottom)),
            ((right, y), (x, bottom)),
            ((x, bottom), (right, y)),
            ((right, bottom), (x, y)),
        ];
        let corner = corners
            .iter()
            .find(|(c, _)| near(c.0, point.0, reach) && near(c.1, point.1, reach))
            .map(|(c, anchor)| Drag::Resize {
                anchor: *anchor,
                far: *c,
                follows: (true, true),
            });
        // Then the four sides, which change one dimension. The reach is kept
        // below a third of the other side so the middle still moves.
        let across_x = reach.min(width / 3.0);
        let across_y = reach.min(height / 3.0);
        let within_x = (x - reach..=right + reach).contains(&point.0);
        let within_y = (y - reach..=bottom + reach).contains(&point.1);
        let side = |near_side: bool, far: (f64, f64), anchor: (f64, f64), follows| {
            (self.constraint == CropConstraint::Free && near_side).then_some(Drag::Resize {
                anchor,
                far,
                follows,
            })
        };
        let sides = [
            side(
                within_x && near(point.1, y, across_y),
                (x, y),
                (right, bottom),
                (false, true),
            ),
            side(
                within_x && near(point.1, bottom, across_y),
                (right, bottom),
                (x, y),
                (false, true),
            ),
            side(
                within_y && near(point.0, x, across_x),
                (x, y),
                (right, bottom),
                (true, false),
            ),
            side(
                within_y && near(point.0, right, across_x),
                (right, bottom),
                (x, y),
                (true, false),
            ),
        ];
        self.drag = corner
            .or_else(|| sides.into_iter().flatten().next())
            .or_else(|| {
                let inside = (x..=right).contains(&point.0) && (y..=bottom).contains(&point.1);
                inside.then_some(Drag::Move {
                    start: point,
                    from: self.region,
                })
            });
    }

    /// Follows the mouse during a drag.
    fn drag_to(&mut self, point: (f64, f64)) {
        match self.drag {
            Some(Drag::Move { start, from }) => {
                self.region = self
                    .source
                    .moved_region(from, point.0 - start.0, point.1 - start.1);
            }
            Some(Drag::Resize {
                anchor,
                far,
                follows,
            }) => match self.constraint {
                CropConstraint::Square => {
                    let crop = self.source.square_from_drag(anchor, point);
                    if self.source.usable(crop) {
                        self.region = Region {
                            x: crop.x,
                            y: crop.y,
                            width: crop.side,
                            height: crop.side,
                        };
                    }
                }
                CropConstraint::Free => {
                    let target = (
                        if follows.0 { point.0 } else { far.0 },
                        if follows.1 { point.1 } else { far.1 },
                    );
                    let region = self.source.region_from_drag(anchor, target);
                    if self.source.region_usable(region) {
                        self.region = region;
                    }
                }
            },
            None => {}
        }
    }

    /// Ends a drag; returns the new close-up to prepare, if it changed.
    fn release(&mut self) -> Option<Region> {
        self.drag.take()?;
        let wanted = self.close_up_for(self.region);
        if wanted == self.close_up.as_ref().map(|(area, _)| *area) {
            return None;
        }
        self.close_up = wanted.map(|area| (area, None));
        wanted
    }

    fn drag_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            let point = self.to_image(position);
            self.drag_to(point);
            cx.notify();
        }
    }

    /// Ends a drag, showing a close-up of a small selection; its sharp
    /// preview is made off the UI thread and kept only if the close-up is
    /// still the one it was made for.
    fn drag_end(&mut self, cx: &mut Context<Self>) {
        if self.drag.is_none() {
            return;
        }
        if let Some(area) = self.release() {
            let source = self.source.clone();
            let max_side = self.view_box.0.max(self.view_box.1) * 2;
            let making =
                cx.background_spawn(
                    async move { render_image(source.region_preview(area, max_side)) },
                );
            cx.spawn(async move |this, cx| {
                let image = making.await;
                let _ = this.update(cx, |this, cx| {
                    if let Some((current, sharp)) = &mut this.close_up
                        && *current == area
                    {
                        *sharp = Some(image);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// The selection alone, `side` logical pixels wide (a square selection's
    /// result preview), from the images already on screen.
    pub(crate) fn selection_preview(&self, side: f32) -> Div {
        let region = self.region;
        let k = f64::from(side) / region.width;
        let image = match &self.close_up {
            Some((area, Some(sharp))) => placed(
                sharp.clone(),
                (area.width, area.height),
                (region.x - area.x, region.y - area.y),
                k,
            ),
            _ => placed(self.whole.clone(), self.full(), (region.x, region.y), k),
        };
        div()
            .relative()
            .flex_shrink_0()
            .w(px(side))
            .h(px((region.height * k) as f32))
            .overflow_hidden()
            .child(image)
    }

    fn full(&self) -> (f64, f64) {
        (
            f64::from(self.source.width()),
            f64::from(self.source.height()),
        )
    }
}

impl Render for ImageCropEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (origin, scale, view) = self.view();
        // The close-up is shown at once from the whole image and sharpened
        // when its own preview is ready.
        let image = match &self.close_up {
            Some((area, Some(sharp))) => {
                placed(sharp.clone(), (area.width, area.height), (0.0, 0.0), scale)
            }
            _ => placed(self.whole.clone(), self.full(), origin, scale),
        };
        let region = self.region;
        let (left, top, width, height) = (
            ((region.x - origin.0) * scale) as f32,
            ((region.y - origin.1) * scale) as f32,
            (region.width * scale) as f32,
            (region.height * scale) as f32,
        );
        let handle = |x: f32, y: f32, cursor: CursorStyle| {
            div()
                .absolute()
                .left(px(x - HANDLE_SIDE / 2.0))
                .top(px(y - HANDLE_SIDE / 2.0))
                .w(px(HANDLE_SIDE))
                .h(px(HANDLE_SIDE))
                .bg(gpui::white())
                .border_1()
                .border_color(gpui::black())
                .cursor(cursor)
        };
        let view_entity = cx.entity().downgrade();
        let bounds = self.bounds.clone();
        let picture = div()
            .id("image-crop-picture")
            .debug_selector(|| "image-crop-picture".into())
            .relative()
            .flex_shrink_0()
            .w(px(view.width))
            .h(px(view.height))
            .child(div().absolute().size_full().overflow_hidden().child(image))
            .child(shade(0.0, 0.0, view.width, top))
            .child(shade(
                0.0,
                top + height,
                view.width,
                view.height - top - height,
            ))
            .child(shade(0.0, top, left, height))
            .child(shade(left + width, top, view.width - left - width, height))
            .child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .cursor(CursorStyle::OpenHand)
                    .border_1()
                    .border_color(gpui::white()),
            )
            .child(handle(left, top, CursorStyle::ResizeUpLeftDownRight))
            .child(handle(
                left + width,
                top,
                CursorStyle::ResizeUpRightDownLeft,
            ))
            .child(handle(
                left,
                top + height,
                CursorStyle::ResizeUpRightDownLeft,
            ))
            .child(handle(
                left + width,
                top + height,
                CursorStyle::ResizeUpLeftDownRight,
            ))
            // Follows the mouse anywhere in the window while dragging.
            .child(
                canvas(
                    move |area, _, _| bounds.set(area),
                    move |_, _, window, _| {
                        let moving = view_entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                let _ = moving.update(cx, |this, cx| {
                                    if event.pressed_button == Some(MouseButton::Left) {
                                        this.drag_move(event.position, cx);
                                    } else {
                                        this.drag_end(cx);
                                    }
                                });
                            }
                        });
                        let releasing = view_entity.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                                let _ = releasing.update(cx, |this, cx| this.drag_end(cx));
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            );
        // The margin around the picture takes presses too, so a corner or
        // side at the image's edge can be grabbed from just outside it.
        div()
            .id("image-crop-grab")
            .p(px(HANDLE_REACH as f32))
            .child(picture)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    let point = this.to_image(event.position);
                    this.press(point);
                    cx.notify();
                }),
            )
    }
}

fn initial_region(source: &AvatarSource, constraint: CropConstraint) -> Region {
    match constraint {
        CropConstraint::Square => {
            let crop = source.initial_crop();
            Region {
                x: crop.x,
                y: crop.y,
                width: crop.side,
                height: crop.side,
            }
        }
        CropConstraint::Free => source.full_region(),
    }
}

fn render_image(thumbnail: Thumbnail) -> Arc<RenderImage> {
    let buffer = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.bgra)
        .expect("thumbnail buffer matches its size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// `image`, covering `full` working pixels, placed so that working pixels
/// from `origin` onward fill a view at `scale` logical pixels per working
/// pixel.
fn placed(image: Arc<RenderImage>, full: (f64, f64), origin: (f64, f64), scale: f64) -> Img {
    img(image)
        .absolute()
        .left(px((-origin.0 * scale) as f32))
        .top(px((-origin.1 * scale) as f32))
        .w(px((full.0 * scale) as f32))
        .h(px((full.1 * scale) as f32))
}

/// A translucent rectangle darkening the image outside the selection.
fn shade(left: f32, top: f32, width: f32, height: f32) -> Div {
    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width.max(0.0)))
        .h(px(height.max(0.0)))
        .bg(gpui::black().opacity(0.45))
}

#[cfg(test)]
mod tests {
    use super::{CropConstraint, Drag, ImageCropEditor};
    use cayenchat_media::avatar_edit::{self, AvatarSource, Region};
    use gpui::{Bounds, point, px, size};

    fn photo(width: u32, height: u32) -> AvatarSource {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(width, height, image::Rgb([10, 20, 30]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        avatar_edit::open(&bytes).unwrap()
    }

    fn square() -> ImageCropEditor {
        let editor = ImageCropEditor::new(photo(1600, 1200), CropConstraint::Square, (320, 320));
        editor.bounds.set(Bounds::new(
            point(px(100.), px(50.)),
            size(px(320.), px(240.)),
        ));
        editor
    }

    fn free() -> ImageCropEditor {
        let editor = ImageCropEditor::new(photo(1600, 800), CropConstraint::Free, (360, 240));
        editor.bounds.set(Bounds::new(
            point(px(100.), px(50.)),
            size(px(360.), px(180.)),
        ));
        editor
    }

    fn region(x: f64, y: f64, width: f64, height: f64) -> Region {
        Region {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_square_starts_centered_and_resizes_by_corners_only() {
        let mut editor = square();
        let (origin, scale, view) = editor.view();
        assert_eq!((origin, view), ((0.0, 0.0), size(320.0, 240.0)));
        assert!((scale - 0.2).abs() < 1e-9);
        assert_eq!(editor.region(), region(200.0, 0.0, 1200.0, 1200.0));
        assert!(editor.is_initial());
        // The top-left handle, 5 logical pixels off: resize with the
        // bottom-right corner fixed.
        editor.press((200.0 + 25.0, 25.0));
        assert!(matches!(
            editor.drag,
            Some(Drag::Resize { anchor, .. }) if anchor == (1400.0, 1200.0)
        ));
        editor.drag_to((800.0, 600.0));
        assert_eq!(editor.region(), region(800.0, 600.0, 600.0, 600.0));
        // 600 of 1200 is not under half: still the whole image.
        assert_eq!(editor.release(), None);
        assert!(!editor.is_initial());
        // The inside moves it, kept inside the image.
        editor.press((1000.0, 800.0));
        editor.drag_to((0.0, 800.0));
        assert_eq!(editor.region(), region(0.0, 600.0, 600.0, 600.0));
        editor.release();
        // Outside the selection and its handles: nothing.
        editor.press((1500.0, 100.0));
        assert_eq!(editor.drag, None);
        // A square has no side handles: just outside a side grabs nothing,
        // just inside moves.
        editor.press((300.0, 600.0 - 5.0));
        assert_eq!(editor.drag, None);
        editor.press((300.0, 600.0 + 5.0));
        assert!(matches!(editor.drag, Some(Drag::Move { .. })));
        editor.reset();
        assert!(editor.is_initial());
    }

    #[test]
    fn a_small_square_is_shown_centered_with_room_around_it() {
        let mut editor = square();
        editor.press((200.0, 0.0));
        editor.drag_to((1000.0, 800.0));
        assert_eq!(editor.region(), region(1000.0, 800.0, 400.0, 400.0));
        // Under half of 1200: twice its size around its center, moved up to
        // stay inside the image (it touches the bottom edge).
        let area = editor.release().unwrap();
        assert_eq!(area, region(800.0, 400.0, 800.0, 800.0));
        let (origin, scale, view) = editor.view();
        assert_eq!((origin, view), ((800.0, 400.0), size(320.0, 320.0)));
        assert!((scale - 0.4).abs() < 1e-9);
        // The mouse maps into the close-up.
        assert_eq!(editor.to_image(point(px(100.), px(50.))), (800.0, 400.0));
        // Near an edge the close-up stays inside the image.
        editor.press((1210.0, 1010.0));
        editor.drag_to((1600.0, 1200.0));
        assert_eq!(editor.region().x + editor.region().width, 1600.0);
        editor.drag = Some(Drag::Move {
            start: (0.0, 0.0),
            from: region(1500.0, 1100.0, 100.0, 100.0),
        });
        editor.drag_to((0.0, 0.0));
        assert_eq!(
            editor.release().unwrap(),
            region(1400.0, 1000.0, 200.0, 200.0)
        );
        // Growing it again past half shows the whole image.
        editor.close_up = None;
        editor.region = region(0.0, 0.0, 100.0, 100.0);
        editor.press((100.0, 100.0));
        editor.drag_to((900.0, 900.0));
        assert_eq!(editor.release(), None);
        assert!(editor.close_up.is_none());
    }

    #[test]
    fn a_free_rectangle_resizes_by_corners_and_sides_and_moves() {
        let mut editor = free();
        assert_eq!(editor.view().2, size(360.0, 180.0));
        assert!((editor.view().1 - 0.225).abs() < 1e-9);
        assert_eq!(editor.region(), editor.source.full_region());
        assert!(editor.is_initial());
        // The mouse maps into the image.
        assert_eq!(editor.to_image(point(px(100.), px(50.))), (0.0, 0.0));
        // The top-left corner, a few pixels in: resize, the opposite fixed.
        editor.press((10.0, 10.0));
        assert_eq!(
            editor.drag,
            Some(Drag::Resize {
                anchor: (1600.0, 800.0),
                far: (0.0, 0.0),
                follows: (true, true),
            })
        );
        editor.drag_to((800.0, 200.0));
        assert_eq!(editor.region(), region(800.0, 200.0, 800.0, 600.0));
        // Too small to use: the last usable rectangle stays.
        editor.drag_to((1590.0, 790.0));
        assert_eq!(editor.region().width, 800.0);
        editor.drag = None;
        // The inside moves it, kept inside the image.
        editor.press((1000.0, 500.0));
        editor.drag_to((0.0, 500.0));
        assert_eq!(editor.region(), region(0.0, 200.0, 800.0, 600.0));
        editor.drag = None;
        // A side changes one dimension only, and can be grabbed from just
        // outside the rectangle.
        editor.press((400.0, 190.0));
        editor.drag_to((700.0, 400.0));
        assert_eq!(editor.region(), region(0.0, 400.0, 800.0, 400.0));
        editor.drag = None;
        editor.press((820.0, 600.0));
        editor.drag_to((500.0, 100.0));
        assert_eq!(editor.region(), region(0.0, 400.0, 500.0, 400.0));
        editor.drag = None;
        // Outside the rectangle and its handles: nothing.
        editor.press((1500.0, 100.0));
        assert_eq!(editor.drag, None);
    }

    #[test]
    fn a_small_rectangle_is_shown_enlarged_with_room_around_it() {
        let mut editor = free();
        // Dragging the top-left corner to leave a quarter of each side.
        editor.press((10.0, 10.0));
        editor.drag_to((1200.0, 600.0));
        let wanted = editor.release().expect("small: a close-up");
        // Twice its size around its center, kept inside the image.
        assert_eq!(wanted, region(800.0, 400.0, 800.0, 400.0));
        let (origin, scale, view) = editor.view();
        assert_eq!((origin, view), ((800.0, 400.0), size(360.0, 180.0)));
        assert!((scale - 0.45).abs() < 1e-9);
        // Mouse positions now map through the enlarged view.
        assert_eq!(editor.to_image(point(px(100.), px(50.))), (800.0, 400.0));
        // The same release again changes nothing.
        editor.drag = Some(Drag::Move {
            start: (0.0, 0.0),
            from: editor.region,
        });
        assert_eq!(editor.release(), None);
        // Growing it past half of the image goes back to the whole image.
        editor.drag = Some(Drag::Resize {
            anchor: (800.0, 400.0),
            far: (1600.0, 800.0),
            follows: (true, true),
        });
        editor.drag_to((1600.0, 800.0));
        assert_eq!(editor.release(), None);
        assert!(editor.close_up.is_none());
    }
}
