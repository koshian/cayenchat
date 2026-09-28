//! The square selection shown before an avatar image is uploaded (IRCv3
//! settings tab). The image is decoded off the UI thread with the preview
//! limits (`media::avatar_edit`). The selection has a handle at each corner
//! to resize it and can be dragged to move it. When a drop leaves it under
//! half of the image, the view shows the selection centered with room
//! around it (twice its size) so it can be adjusted precisely; otherwise
//! the whole image is shown. "Whole Image" starts over. The selected square
//! is uploaded, shrunk, and then sent to the server.

use std::{cell::Cell, rc::Rc, sync::Arc};

use cayenchat_media::{
    LoadError,
    avatar_edit::{self, AvatarSource, Crop},
    decode::{self, Thumbnail},
};
use cayenchat_model::attachment::AttachmentSource;
use gpui::{prelude::*, *};

use crate::{SettingsWindow, settings_theme};

/// Largest size of the editor view, in logical pixels.
const VIEW_SIDE: u32 = 320;
/// Size of the result preview, in logical pixels.
const RESULT_SIDE: f32 = 64.0;
/// Corner handles, drawn and hit, in logical pixels.
const HANDLE_SIDE: f32 = 10.0;
const HANDLE_REACH: f64 = 9.0;
/// A selection below this share of the image's shorter side is shown
/// enlarged.
const CLOSE_UP_BELOW: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// Moving the selection: where the drag started, and the selection
    /// then.
    Move { start: (f64, f64), from: Crop },
    /// Resizing from a corner; the opposite corner stays.
    Resize { anchor: (f64, f64) },
}

pub(crate) struct AvatarEditor {
    /// Server profile whose avatar draft receives the uploaded URL.
    pub profile: String,
    pub source_kind: AttachmentSource,
    source: Arc<AvatarSource>,
    /// The whole image, fitted into the view (twice its logical size).
    whole: Arc<RenderImage>,
    /// Displayed size of the whole image, in logical pixels.
    whole_view: Size<f32>,
    /// What will be uploaded, in working pixels.
    crop: Crop,
    /// The enlarged part shown instead of the whole image, and its sharp
    /// preview once made.
    close_up: Option<(Crop, Option<Arc<RenderImage>>)>,
    drag: Option<Drag>,
    /// Where the view was last painted, to map the mouse into the image.
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl AvatarEditor {
    fn new(profile: String, source_kind: AttachmentSource, source: AvatarSource) -> Self {
        let preview = source.preview(VIEW_SIDE * 2);
        let (width, height) = decode::fit(source.width(), source.height(), VIEW_SIDE, VIEW_SIDE);
        Self {
            profile,
            source_kind,
            whole: render_image(preview),
            whole_view: size(width as f32, height as f32),
            crop: source.initial_crop(),
            source: Arc::new(source),
            close_up: None,
            drag: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    /// The part of the image shown: working-pixel origin, logical pixels
    /// per working pixel, and the view's logical size.
    fn view(&self) -> ((f64, f64), f64, Size<f32>) {
        match &self.close_up {
            Some((area, _)) => (
                (area.x, area.y),
                f64::from(VIEW_SIDE) / area.side,
                size(VIEW_SIDE as f32, VIEW_SIDE as f32),
            ),
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

    /// What will be uploaded.
    pub fn crop(&self) -> Crop {
        self.crop
    }

    pub fn source(&self) -> Arc<AvatarSource> {
        self.source.clone()
    }

    /// The part to show for a selection: the selection centered in twice
    /// its size when it is under half the image, else the whole image.
    fn close_up_for(&self, crop: Crop) -> Option<Crop> {
        let shorter = f64::from(self.source.width().min(self.source.height()));
        (crop.side < shorter * CLOSE_UP_BELOW).then(|| {
            self.source.clamp(Crop {
                x: crop.x - crop.side / 2.0,
                y: crop.y - crop.side / 2.0,
                side: crop.side * 2.0,
            })
        })
    }

    /// Starts a drag at `point` (working pixels): a corner resizes, the
    /// inside moves, elsewhere nothing happens.
    fn press(&mut self, point: (f64, f64)) {
        let (_, scale, _) = self.view();
        let reach = HANDLE_REACH / scale;
        let Crop { x, y, side } = self.crop;
        let corners = [
            ((x, y), (x + side, y + side)),
            ((x + side, y), (x, y + side)),
            ((x, y + side), (x + side, y)),
            ((x + side, y + side), (x, y)),
        ];
        self.drag = corners
            .iter()
            .find(|(corner, _)| {
                (corner.0 - point.0).abs() <= reach && (corner.1 - point.1).abs() <= reach
            })
            .map(|(_, anchor)| Drag::Resize { anchor: *anchor })
            .or_else(|| {
                let inside = (x..=x + side).contains(&point.0) && (y..=y + side).contains(&point.1);
                inside.then_some(Drag::Move {
                    start: point,
                    from: self.crop,
                })
            });
    }

    /// Follows the mouse during a drag.
    fn drag_to(&mut self, point: (f64, f64)) {
        match self.drag {
            Some(Drag::Move { start, from }) => {
                self.crop = self
                    .source
                    .moved(from, point.0 - start.0, point.1 - start.1);
            }
            Some(Drag::Resize { anchor }) => {
                let crop = self.source.square_from_drag(anchor, point);
                if self.source.usable(crop) {
                    self.crop = crop;
                }
            }
            None => {}
        }
    }

    /// Ends a drag; returns the new close-up to prepare, if it changed.
    fn release(&mut self) -> Option<Crop> {
        self.drag.take()?;
        let wanted = self.close_up_for(self.crop);
        if wanted == self.close_up.as_ref().map(|(area, _)| *area) {
            return None;
        }
        self.close_up = wanted.map(|area| (area, None));
        wanted
    }
}

fn render_image(thumbnail: Thumbnail) -> Arc<RenderImage> {
    let buffer = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.bgra)
        .expect("thumbnail buffer matches its size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// The localization key explaining why an image cannot be edited.
pub(crate) fn open_error_key(error: &LoadError) -> &'static str {
    match error {
        LoadError::Unsupported => "ircv3_avatar_edit_unsupported",
        LoadError::TooLarge => "ircv3_avatar_edit_too_large",
        _ => "ircv3_avatar_edit_unreadable",
    }
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

impl SettingsWindow {
    /// Decodes `bytes` in the background and opens the editor for `profile`.
    pub(crate) fn open_avatar_editor(
        &mut self,
        profile: String,
        bytes: Arc<[u8]>,
        source_kind: AttachmentSource,
        cx: &mut Context<Self>,
    ) {
        self.avatar_editor = None;
        self.avatar_opening = true;
        cx.notify();
        let decoding = cx.background_spawn(async move { avatar_edit::open(&bytes) });
        cx.spawn(async move |this, cx| {
            let result = decoding.await;
            let _ = this.update(cx, |this, cx| {
                this.avatar_opening = false;
                match result {
                    Ok(source) => {
                        this.avatar_editor = Some(AvatarEditor::new(profile, source_kind, source));
                    }
                    Err(error) => {
                        this.avatar_feedback = Some(this.i18n.text(open_error_key(&error)));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn avatar_drag_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.avatar_editor
            && editor.drag.is_some()
        {
            let point = editor.to_image(position);
            editor.drag_to(point);
            cx.notify();
        }
    }

    /// Ends a drag, showing a close-up of a small selection; its sharp
    /// preview is made off the UI thread.
    fn avatar_drag_end(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.avatar_editor else {
            return;
        };
        if editor.drag.is_none() {
            return;
        }
        if let Some(area) = editor.release() {
            let source = editor.source.clone();
            let making = cx.background_spawn(async move {
                render_image(source.square_preview(area, VIEW_SIDE * 2))
            });
            cx.spawn(async move |this, cx| {
                let image = making.await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(editor) = &mut this.avatar_editor
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

    pub(crate) fn render_avatar_editor(
        &self,
        provider: &str,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let editor = self.avatar_editor.as_ref()?;
        let theme = settings_theme::palette(cx);
        let (origin, scale, view) = editor.view();
        let full = (
            f64::from(editor.source.width()),
            f64::from(editor.source.height()),
        );
        // The close-up is shown at once from the whole image and sharpened
        // when its own preview is ready.
        let image = match &editor.close_up {
            Some((area, Some(sharp))) => {
                placed(sharp.clone(), (area.side, area.side), (0.0, 0.0), scale)
            }
            _ => placed(editor.whole.clone(), full, origin, scale),
        };
        let crop = editor.crop;
        let (left, top, side) = (
            ((crop.x - origin.0) * scale) as f32,
            ((crop.y - origin.1) * scale) as f32,
            (crop.side * scale) as f32,
        );
        let (width, height) = (view.width, view.height);
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
            .id("avatar-editor-picture")
            .relative()
            .flex_shrink_0()
            .w(px(width))
            .h(px(height))
            .overflow_hidden()
            .child(image)
            .child(shade(0.0, 0.0, width, top))
            .child(shade(0.0, top + side, width, height - top - side))
            .child(shade(0.0, top, left, side))
            .child(shade(left + side, top, width - left - side, side))
            .child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(side))
                    .h(px(side))
                    .cursor(CursorStyle::OpenHand)
                    .border_1()
                    .border_color(gpui::white()),
            )
            .child(handle(left, top, CursorStyle::ResizeUpLeftDownRight))
            .child(handle(left + side, top, CursorStyle::ResizeUpRightDownLeft))
            .child(handle(left, top + side, CursorStyle::ResizeUpRightDownLeft))
            .child(handle(
                left + side,
                top + side,
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
                                        this.avatar_drag_move(event.position, cx);
                                    } else {
                                        this.avatar_drag_end(cx);
                                    }
                                });
                            }
                        });
                        let releasing = view_entity.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                                let _ = releasing.update(cx, |this, cx| this.avatar_drag_end(cx));
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if let Some(editor) = &mut this.avatar_editor {
                        let point = editor.to_image(event.position);
                        editor.press(point);
                        cx.notify();
                    }
                }),
            );
        // What will be uploaded, from the images already on screen.
        let k = f64::from(RESULT_SIDE) / crop.side;
        let result_image = match &editor.close_up {
            Some((area, Some(sharp))) => placed(
                sharp.clone(),
                (area.side, area.side),
                (crop.x - area.x, crop.y - area.y),
                k,
            ),
            _ => placed(editor.whole.clone(), full, (crop.x, crop.y), k),
        };
        let result = div()
            .relative()
            .flex_shrink_0()
            .w(px(RESULT_SIDE))
            .h(px(RESULT_SIDE))
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .child(result_image);
        let whole = editor.close_up.is_none() && crop == editor.source.initial_crop();
        Some(
            div()
                .ml(px(158.))
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .border_1()
                .border_color(theme.border)
                .child(
                    div().flex().gap_3().items_start().child(picture).child(
                        div().flex().flex_col().gap_1().child(result).child(
                            div()
                                .text_color(theme.text_secondary)
                                .child(self.i18n.text("ircv3_avatar_edit_result")),
                        ),
                    ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            settings_theme::button("avatar-editor-upload", true, cx)
                                .child(
                                    self.i18n.format(
                                        "ircv3_avatar_edit_upload",
                                        &[("provider", provider)],
                                    ),
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.upload_edited_avatar(cx)),
                                ),
                        )
                        .child(
                            settings_theme::button("avatar-editor-reset", false, cx)
                                .child(self.i18n.text("ircv3_avatar_edit_reset"))
                                .when(whole, |b| b.opacity(0.5))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(editor) = &mut this.avatar_editor {
                                        editor.crop = editor.source.initial_crop();
                                        editor.close_up = None;
                                        editor.drag = None;
                                        cx.notify();
                                    }
                                })),
                        )
                        .child(
                            settings_theme::button("avatar-editor-cancel", false, cx)
                                .child(self.i18n.text("cancel"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.avatar_editor = None;
                                    cx.notify();
                                })),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttachmentSource, AvatarEditor, AvatarSource, Crop, Drag, LoadError, avatar_edit,
        open_error_key,
    };
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

    fn editor() -> AvatarEditor {
        let editor = AvatarEditor::new("server".into(), AttachmentSource::Drop, photo(1600, 1200));
        editor.bounds.set(Bounds::new(
            point(px(100.), px(50.)),
            size(px(320.), px(240.)),
        ));
        editor
    }

    #[test]
    fn corners_resize_and_the_inside_moves_the_selection() {
        let mut editor = editor();
        let (origin, scale, view) = editor.view();
        assert_eq!((origin, view), ((0.0, 0.0), size(320.0, 240.0)));
        assert!((scale - 0.2).abs() < 1e-9);
        // The centered square to start with: 200..1400 × 0..1200.
        assert_eq!(
            editor.crop(),
            Crop {
                x: 200.0,
                y: 0.0,
                side: 1200.0
            }
        );
        // The top-left handle, 5 logical pixels off: resize with the
        // bottom-right corner fixed.
        editor.press((200.0 + 25.0, 25.0));
        assert_eq!(
            editor.drag,
            Some(Drag::Resize {
                anchor: (1400.0, 1200.0)
            })
        );
        editor.drag_to((800.0, 600.0));
        assert_eq!(
            editor.crop(),
            Crop {
                x: 800.0,
                y: 600.0,
                side: 600.0
            }
        );
        // 600 of 1200 is not under half: still the whole image.
        assert_eq!(editor.release(), None);
        // The inside moves it, kept inside the image.
        editor.press((1000.0, 800.0));
        editor.drag_to((0.0, 800.0));
        assert_eq!(
            editor.crop(),
            Crop {
                x: 0.0,
                y: 600.0,
                side: 600.0
            }
        );
        editor.release();
        // Outside the selection and its handles: nothing.
        editor.press((1500.0, 100.0));
        assert_eq!(editor.drag, None);
    }

    #[test]
    fn a_small_selection_is_shown_centered_with_room_around_it() {
        let mut editor = editor();
        // The top-left handle, dragged in towards the fixed bottom-right.
        editor.press((200.0, 0.0));
        editor.drag_to((1000.0, 800.0));
        assert_eq!(
            editor.crop(),
            Crop {
                x: 1000.0,
                y: 800.0,
                side: 400.0
            }
        );
        // Under half of 1200: twice its size around its center, moved up
        // to stay inside the image (it touches the bottom edge).
        let area = editor.release().unwrap();
        assert_eq!(
            area,
            Crop {
                x: 800.0,
                y: 400.0,
                side: 800.0
            }
        );
        let (origin, scale, view) = editor.view();
        assert_eq!((origin, view), ((800.0, 400.0), size(320.0, 320.0)));
        assert!((scale - 0.4).abs() < 1e-9);
        // The mouse maps into the close-up.
        assert_eq!(editor.to_image(point(px(100.), px(50.))), (800.0, 400.0));
        // Near an edge the close-up stays inside the image.
        editor.press((1210.0, 1010.0));
        editor.drag_to((1600.0, 1200.0));
        assert_eq!(editor.crop().x + editor.crop().side, 1600.0);
        editor.drag = Some(Drag::Move {
            start: (0.0, 0.0),
            from: Crop {
                x: 1500.0,
                y: 1100.0,
                side: 100.0,
            },
        });
        editor.drag_to((0.0, 0.0));
        let area = editor.release().unwrap();
        assert_eq!(
            area,
            Crop {
                x: 1400.0,
                y: 1000.0,
                side: 200.0
            }
        );
        // Growing it again past half shows the whole image.
        editor.close_up = None;
        editor.crop = Crop {
            x: 0.0,
            y: 0.0,
            side: 100.0,
        };
        editor.press((100.0, 100.0));
        editor.drag_to((900.0, 900.0));
        assert_eq!(editor.release(), None);
        assert!(editor.close_up.is_none());
    }

    #[test]
    fn open_errors_have_texts() {
        let catalogs = [
            include_str!("../../../locales/en.json"),
            include_str!("../../../locales/ja.json"),
        ];
        for error in [
            LoadError::Unsupported,
            LoadError::TooLarge,
            LoadError::Malformed,
        ] {
            let key = open_error_key(&error);
            for catalog in catalogs {
                assert!(catalog.contains(&format!("\"{key}\"")), "{key}");
            }
        }
    }
}
