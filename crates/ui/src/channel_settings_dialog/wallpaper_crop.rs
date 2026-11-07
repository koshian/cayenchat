// SPDX-License-Identifier: GPL-3.0-only
//! Choosing a wallpaper image and its range (#950). A file chosen in the
//! file dialog or dropped on the window (also a Photos file promise, which
//! is read before the platform deletes it) is decoded off the UI thread by
//! the avatar editor's decoder (`media::avatar_edit`, the preview limits and
//! EXIF orientation), and shown with a rectangle that can be moved and
//! resized by its corners. "Use" cuts the rectangle out, shrinks it and
//! keeps it as a PNG file of the application's own (`wallpapers` in the data
//! directory), whose path becomes the wallpaper. The original file is not
//! needed afterwards, so a temporary drop or an image later moved is no
//! problem. A path typed into the field is still used as it is.

use std::{
    cell::Cell,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use cayenchat_media::avatar_edit::{self, AvatarSource, Region};
use cayenchat_media::decode;
use cayenchat_model::attachment::AttachmentSource;
use gpui::{prelude::*, *};

use super::ChannelSettingsView;
use crate::{
    avatar_editor::{open_error_key, placed, render_image, shade},
    image_upload::{acceptable_attachment, image_file_filter, read_in_background},
    settings_theme,
};

/// Largest size of the range view, in logical pixels.
const VIEW_WIDTH: u32 = 360;
const VIEW_HEIGHT: u32 = 240;
/// The kept image is at most this many pixels a side; the wallpaper loader
/// reduces it further (640 px), this leaves room for a sharper fit later.
const OUTPUT_SIDE: u32 = 1280;
/// Corner handles, drawn and hit, in logical pixels.
const HANDLE_SIDE: f32 = 10.0;
const HANDLE_REACH: f64 = 9.0;
/// A rectangle under this share of the image on both sides is shown
/// enlarged once released.
const CLOSE_UP_BELOW: f64 = 0.5;
/// Every file the application cut out starts with this. Nothing deletes
/// them: equal cuts share one file, and a channel's saved settings may still
/// name it.
const FILE_PREFIX: &str = "wallpaper-";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// Moving the rectangle: where the drag started, and the rectangle then.
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

/// The image being cut and the rectangle on it.
pub(super) struct WallpaperCrop {
    source: Arc<AvatarSource>,
    /// The whole image, fitted into the view.
    image: Arc<RenderImage>,
    /// Displayed size of the whole image, in logical pixels.
    view: Size<f32>,
    region: Region,
    /// The enlarged part shown instead of the whole image, and its sharp
    /// preview once made.
    close_up: Option<(Region, Option<Arc<RenderImage>>)>,
    drag: Option<Drag>,
    /// Where the view was last painted, to map the mouse into the image.
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl WallpaperCrop {
    fn new(source: AvatarSource) -> Self {
        let (width, height) = decode::fit(source.width(), source.height(), VIEW_WIDTH, VIEW_HEIGHT);
        Self {
            image: render_image(source.preview(VIEW_WIDTH.max(VIEW_HEIGHT) * 2)),
            view: size(width as f32, height as f32),
            region: source.full_region(),
            source: Arc::new(source),
            close_up: None,
            drag: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    /// The part of the image shown: working-pixel origin, logical pixels
    /// per working pixel, and the view's logical size.
    fn shown(&self) -> ((f64, f64), f64, Size<f32>) {
        match &self.close_up {
            Some((area, _)) => {
                let scale =
                    (f64::from(VIEW_WIDTH) / area.width).min(f64::from(VIEW_HEIGHT) / area.height);
                (
                    (area.x, area.y),
                    scale,
                    size((area.width * scale) as f32, (area.height * scale) as f32),
                )
            }
            None => (
                (0.0, 0.0),
                f64::from(self.view.width) / f64::from(self.source.width()),
                self.view,
            ),
        }
    }

    /// Logical pixels per working pixel.
    fn scale(&self) -> f64 {
        self.shown().1
    }

    /// A mouse position as working-image pixels.
    fn to_image(&self, position: Point<Pixels>) -> (f64, f64) {
        let ((x, y), scale, _) = self.shown();
        let origin = self.bounds.get().origin;
        (
            x + f64::from(f32::from(position.x - origin.x)) / scale,
            y + f64::from(f32::from(position.y - origin.y)) / scale,
        )
    }

    /// The part to show for a rectangle: the rectangle centered in twice
    /// its size when it is under half the image on both sides, else the
    /// whole image (as the avatar editor does).
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

    /// Starts a drag at `point` (working pixels): a corner resizes, the
    /// inside moves, elsewhere nothing happens.
    fn press(&mut self, point: (f64, f64)) {
        let reach = HANDLE_REACH / self.scale();
        let Region {
            x,
            y,
            width,
            height,
        } = self.region;
        let (right, bottom) = (x + width, y + height);
        let near = |a: f64, b: f64, reach: f64| (a - b).abs() <= reach;
        // Corners first, from the far corner's point of view: the dragged
        // corner, the opposite corner that stays, and which axes follow.
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
        // Then the four sides, which change one dimension. The reach is
        // kept below a third of the other side so the middle still moves.
        let across_x = reach.min(width / 3.0);
        let across_y = reach.min(height / 3.0);
        let within_x = (x - reach..=right + reach).contains(&point.0);
        let within_y = (y - reach..=bottom + reach).contains(&point.1);
        let side = |near_side: bool, far: (f64, f64), anchor: (f64, f64), follows| {
            near_side.then_some(Drag::Resize {
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
            }) => {
                let target = (
                    if follows.0 { point.0 } else { far.0 },
                    if follows.1 { point.1 } else { far.1 },
                );
                let region = self.source.region_from_drag(anchor, target);
                if self.source.region_usable(region) {
                    self.region = region;
                }
            }
            None => {}
        }
    }
}

/// What the window holds of the wallpaper image being chosen.
pub(super) struct CropState {
    pub(super) editor: Option<WallpaperCrop>,
    /// "Opening the image…", or why the image could not be used.
    status: Option<(String, bool)>,
    /// Reading and decoding; replacing it drops an older request.
    task: Option<Task<()>>,
    /// Where the kept images go instead of the application's own directory
    /// (tests).
    directory: Option<PathBuf>,
}

impl CropState {
    pub(super) fn new() -> Self {
        Self {
            editor: None,
            status: None,
            task: None,
            directory: None,
        }
    }

    /// Whether the range editor is open.
    pub(super) fn is_open(&self) -> bool {
        self.editor.is_some()
    }

    /// Closes the editor and drops a request in progress. Returns whether
    /// there was anything to close.
    pub(super) fn close(&mut self) -> bool {
        let had = self.editor.is_some() || self.task.is_some() || self.status.is_some();
        *self = Self {
            directory: self.directory.take(),
            ..Self::new()
        };
        had
    }
}

/// The file name for the kept image: its content decides, so a different
/// cut gets a different path and the wallpaper is loaded again.
fn file_name(png: &[u8]) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    png.hash(&mut hasher);
    format!("{FILE_PREFIX}{:016x}.png", hasher.finish())
}

/// Writes `png` into `directory` and returns its path.
fn keep(directory: &Path, png: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = directory.join(file_name(png));
    if !path.is_file() {
        std::fs::write(&path, png).map_err(|error| error.to_string())?;
    }
    Ok(path)
}

impl ChannelSettingsView {
    /// Opens the file chooser for the wallpaper.
    pub(super) fn browse_wallpaper(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.i18n.text("channel_look_wallpaper").into()),
            filter: Some(image_file_filter(&self.i18n)),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.offer_wallpaper(paths, None, AttachmentSource::Chooser, cx)
            });
        })
        .detach();
    }

    /// Reads the image named by `paths` (one file) and opens the range
    /// editor on it. `keep` holds a file promise until the file is read.
    pub(super) fn offer_wallpaper(
        &mut self,
        paths: Vec<PathBuf>,
        keep: Option<Arc<PromisedFiles>>,
        source: AttachmentSource,
        cx: &mut Context<Self>,
    ) {
        self.crop.close();
        self.crop.status = Some((self.i18n.text("ircv3_avatar_edit_loading"), false));
        let read = read_in_background(paths, keep, source, cx);
        let i18n = self.i18n.clone();
        self.crop.task = Some(cx.spawn(async move |this, cx| {
            let outcome: Result<AvatarSource, String> = match read.await {
                Err(error) => Err(error.text(&i18n)),
                Ok(attachment) => match acceptable_attachment(attachment, None, &i18n) {
                    Err(error) => Err(error),
                    Ok(attachment) => {
                        let bytes = attachment.bytes.clone();
                        cx.background_executor()
                            .spawn(async move { avatar_edit::open(&bytes) })
                            .await
                            .map_err(|error| i18n.text(open_error_key(&error)))
                    }
                },
            };
            let _ = this.update(cx, |this, cx| {
                this.crop.task = None;
                match outcome {
                    Ok(source) => {
                        this.crop.status = None;
                        this.crop.editor = Some(WallpaperCrop::new(source));
                    }
                    Err(error) => this.crop.status = Some((error, true)),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Cuts the rectangle out, keeps it as the wallpaper file and sets the
    /// field to its path.
    fn use_crop(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.crop.editor else {
            return;
        };
        let (source, region) = (editor.source.clone(), editor.region);
        let i18n = self.i18n.clone();
        let directory = self.crop.directory.clone();
        self.crop.status = Some((i18n.text("channel_look_crop_saving"), false));
        self.crop.task = Some(cx.spawn(async move |this, cx| {
            let kept = cx
                .background_executor()
                .spawn(async move {
                    let directory = match directory {
                        Some(directory) => directory,
                        None => cayenchat_storage::wallpaper_directory()?,
                    };
                    let png = source
                        .encode_region(region, OUTPUT_SIDE)
                        .map_err(|_| i18n.text("ircv3_avatar_edit_unreadable"))?;
                    let path = keep(&directory, &png)?;
                    Ok::<_, String>(path)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.crop.task = None;
                match kept {
                    Ok(path) => {
                        this.crop.close();
                        let path = path.to_string_lossy().into_owned();
                        this.look
                            .wallpaper
                            .update(cx, |field, cx| field.set_text(&path, cx));
                    }
                    Err(error) => {
                        let text = this
                            .i18n
                            .format("channel_look_crop_failed", &[("error", &error)]);
                        this.crop.status = Some((text, true));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn crop_drag_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.crop.editor
            && editor.drag.is_some()
        {
            let point = editor.to_image(position);
            editor.drag_to(point);
            cx.notify();
        }
    }

    /// Ends a drag, showing a close-up of a small rectangle; its sharp
    /// preview is made off the UI thread.
    fn crop_drag_end(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.crop.editor else {
            return;
        };
        if editor.drag.is_none() {
            return;
        }
        if let Some(area) = editor.release() {
            let source = editor.source.clone();
            let making = cx.background_spawn(async move {
                render_image(source.region_preview(area, VIEW_WIDTH.max(VIEW_HEIGHT) * 2))
            });
            cx.spawn(async move |this, cx| {
                let image = making.await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(editor) = &mut this.crop.editor
                        && let Some((current, sharp)) = &mut editor.close_up
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

    /// The drop target highlight and handler for the whole window.
    pub(super) fn on_wallpaper_drop(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        self.offer_wallpaper(
            paths.paths().to_vec(),
            paths.keep_alive(),
            AttachmentSource::Drop,
            cx,
        );
    }

    /// The range editor, or the status of opening the image; `None` when
    /// there is neither.
    pub(super) fn render_crop(
        &self,
        palette: &crate::theme::Theme,
        ring: Rgba,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let status = self.crop.status.as_ref().map(|(text, failed)| {
            div()
                .when(*failed, |d| d.text_color(palette.warning))
                .when(!*failed, |d| d.text_color(palette.text_secondary))
                .child(text.clone())
        });
        let Some(editor) = &self.crop.editor else {
            return status.map(|status| div().flex().flex_col().gap_1().child(status));
        };
        let (origin, scale, view) = editor.shown();
        let full = (
            f64::from(editor.source.width()),
            f64::from(editor.source.height()),
        );
        // The close-up is shown at once from the whole image and sharpened
        // when its own preview is ready.
        let image = match &editor.close_up {
            Some((area, Some(sharp))) => {
                placed(sharp.clone(), (area.width, area.height), (0.0, 0.0), scale)
            }
            _ => placed(editor.image.clone(), full, origin, scale),
        };
        let region = editor.region;
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
        let bounds = editor.bounds.clone();
        let picture = div()
            .id("channel-look-crop-picture")
            .debug_selector(|| "channel-look-crop-picture".into())
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
                                        this.crop_drag_move(event.position, cx);
                                    } else {
                                        this.crop_drag_end(cx);
                                    }
                                });
                            }
                        });
                        let releasing = view_entity.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                                let _ = releasing.update(cx, |this, cx| this.crop_drag_end(cx));
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            );
        // The margin around the picture takes presses too, so a corner or
        // side at the image's edge can be grabbed from just outside it.
        let picture = div()
            .id("channel-look-crop-grab")
            .p(px(HANDLE_REACH as f32))
            .child(picture)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if let Some(editor) = &mut this.crop.editor {
                        let point = editor.to_image(event.position);
                        editor.press(point);
                        cx.notify();
                    }
                }),
            );
        let whole = region == editor.source.full_region();
        let buttons = crate::button_order::row(div().flex().flex_wrap().gap_2())
            .child(
                settings_theme::button("channel-look-crop-use", true, cx)
                    .track_focus(&self.controls.crop_use)
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text("channel_look_crop_use"))
                    .on_click(cx.listener(|this, _, _, cx| this.use_crop(cx))),
            )
            .child(
                settings_theme::button("channel-look-crop-whole", false, cx)
                    .track_focus(&self.controls.crop_whole)
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text("ircv3_avatar_edit_reset"))
                    .when(whole, |b| b.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(editor) = &mut this.crop.editor {
                            editor.region = editor.source.full_region();
                            editor.close_up = None;
                            editor.drag = None;
                            cx.notify();
                        }
                    })),
            )
            .child(
                settings_theme::button("channel-look-crop-cancel", false, cx)
                    .track_focus(&self.controls.crop_cancel)
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text("cancel"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.crop.close();
                        cx.notify();
                    })),
            );
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(palette.border)
                .child(
                    div()
                        .text_sm()
                        .text_color(palette.text_secondary)
                        .child(self.i18n.text("channel_look_crop_hint")),
                )
                .child(picture)
                .children(status)
                .child(buttons),
        )
    }
}

/// What the window's tests drive.
#[cfg(test)]
impl ChannelSettingsView {
    /// Drops `path` on the window, keeping the cut image in `directory`.
    pub(crate) fn drop_wallpaper_for_test(
        &mut self,
        path: PathBuf,
        directory: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.crop.directory = Some(directory);
        self.offer_wallpaper(vec![path], None, AttachmentSource::Drop, cx);
    }

    /// The range editor's image size and rectangle, when it is open.
    pub(crate) fn crop_region_for_test(&self) -> Option<Region> {
        self.crop.editor.as_ref().map(|editor| editor.region)
    }

    /// The status line (text, failed) under the wallpaper row.
    pub(crate) fn crop_status_for_test(&self) -> Option<(String, bool)> {
        self.crop.status.clone()
    }

    pub(crate) fn set_crop_region_for_test(&mut self, region: Region) {
        if let Some(editor) = &mut self.crop.editor {
            editor.region = editor.source.clamp_region(region);
        }
    }

    pub(crate) fn use_crop_for_test(&mut self, cx: &mut Context<Self>) {
        self.use_crop(cx);
    }

    pub(crate) fn wallpaper_text_for_test(&self, cx: &App) -> String {
        self.look.wallpaper.read(cx).text().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{Drag, WallpaperCrop, keep};
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

    fn editor() -> WallpaperCrop {
        let editor = WallpaperCrop::new(photo(1600, 800));
        editor.bounds.set(Bounds::new(
            point(px(100.), px(50.)),
            size(px(360.), px(180.)),
        ));
        editor
    }

    #[test]
    fn corners_resize_freely_and_the_inside_moves_the_rectangle() {
        let mut editor = editor();
        assert_eq!(editor.view, size(360.0, 180.0));
        assert!((editor.scale() - 0.225).abs() < 1e-9);
        assert_eq!(editor.region, editor.source.full_region());
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
        assert_eq!(
            editor.region,
            Region {
                x: 800.0,
                y: 200.0,
                width: 800.0,
                height: 600.0
            }
        );
        // Too small to use: the last usable rectangle stays.
        editor.drag_to((1590.0, 790.0));
        assert_eq!(editor.region.width, 800.0);
        editor.drag = None;
        // The inside moves it, kept inside the image.
        editor.press((1000.0, 500.0));
        editor.drag_to((0.0, 500.0));
        assert_eq!(
            editor.region,
            Region {
                x: 0.0,
                y: 200.0,
                width: 800.0,
                height: 600.0
            }
        );
        editor.drag = None;
        // A side changes one dimension only, and can be grabbed from just
        // outside the rectangle.
        editor.press((400.0, 190.0));
        editor.drag_to((700.0, 400.0));
        assert_eq!(
            editor.region,
            Region {
                x: 0.0,
                y: 400.0,
                width: 800.0,
                height: 400.0
            }
        );
        editor.drag = None;
        editor.press((820.0, 600.0));
        editor.drag_to((500.0, 100.0));
        assert_eq!(
            editor.region,
            Region {
                x: 0.0,
                y: 400.0,
                width: 500.0,
                height: 400.0
            }
        );
        editor.drag = None;
        // Outside the rectangle and its handles: nothing.
        editor.press((1500.0, 100.0));
        assert_eq!(editor.drag, None);
    }

    #[test]
    fn a_small_rectangle_is_shown_enlarged_with_room_around_it() {
        let mut editor = editor();
        // Dragging the top-left corner to leave a quarter of each side.
        editor.press((10.0, 10.0));
        editor.drag_to((1200.0, 600.0));
        let wanted = editor.release().expect("small: a close-up");
        // Twice its size around its center, kept inside the image.
        assert_eq!(
            wanted,
            Region {
                x: 800.0,
                y: 400.0,
                width: 800.0,
                height: 400.0
            }
        );
        let (origin, scale, view) = editor.shown();
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
        editor.press((1000.0, 500.0));
        editor.drag = Some(Drag::Resize {
            anchor: (800.0, 400.0),
            far: (1600.0, 800.0),
            follows: (true, true),
        });
        editor.drag_to((1600.0, 800.0));
        assert_eq!(editor.release(), None);
        assert!(editor.close_up.is_none());
    }

    #[test]
    fn kept_files_are_named_by_content_and_never_deleted() {
        let dir = std::env::temp_dir().join(format!("cayenchat-crop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = keep(&dir, b"one").unwrap();
        let second = keep(&dir, b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(keep(&dir, b"one").unwrap(), first, "same cut, same file");
        // Choosing another range leaves the earlier file: another channel
        // (or the saved settings) may still use it.
        assert!(first.is_file() && second.is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
