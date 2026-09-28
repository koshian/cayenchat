//! The square selection shown before an avatar image is uploaded (IRCv3
//! settings tab). The image is decoded off the UI thread with the preview
//! limits (`media::avatar_edit`). Dragging outlines a square; releasing the
//! mouse enlarges that square to fill the view, and dragging again narrows
//! it further. "Whole Image" starts over. What is shown (the largest
//! centered square of the whole image before any selection) is what gets
//! uploaded, shrunk to a small square, after which it is sent to the server.

use std::{cell::Cell, rc::Rc, sync::Arc};

use cayenchat_media::{
    LoadError,
    avatar_edit::{self, AVATAR_OUTPUT_SIDE, AvatarSource, Crop},
    decode::{self, Thumbnail},
};
use cayenchat_model::attachment::AttachmentSource;
use gpui::{prelude::*, *};

use crate::{SettingsWindow, settings_theme};

/// Largest size of the editor view, in logical pixels.
const VIEW_SIDE: u32 = 320;
/// Size of the result preview, in logical pixels.
const RESULT_SIDE: f32 = 64.0;
/// Drags shorter than this (logical pixels) are clicks, not selections.
const MIN_DRAG: f32 = 6.0;

pub(crate) struct AvatarEditor {
    /// Server profile whose avatar draft receives the uploaded URL.
    pub profile: String,
    pub source_kind: AttachmentSource,
    source: Arc<AvatarSource>,
    /// The whole image, fitted into the view (twice its logical size).
    whole: Arc<RenderImage>,
    /// Displayed size of the whole image, in logical pixels.
    whole_view: Size<f32>,
    /// The enlarged selection, if any, and its sharp preview once made.
    zoom: Option<(Crop, Option<Arc<RenderImage>>)>,
    /// Working-pixel anchor and current point of a drag.
    drag: Option<((f64, f64), (f64, f64))>,
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
            source: Arc::new(source),
            zoom: None,
            drag: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    /// The part of the image shown: working-pixel origin, and logical
    /// pixels per working pixel.
    fn view(&self) -> ((f64, f64), f64, Size<f32>) {
        match &self.zoom {
            Some((crop, _)) => (
                (crop.x, crop.y),
                f64::from(VIEW_SIDE) / crop.side,
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
        match &self.zoom {
            Some((crop, _)) => *crop,
            None => self.source.initial_crop(),
        }
    }

    pub fn source(&self) -> Arc<AvatarSource> {
        self.source.clone()
    }

    /// The square being dragged, in working pixels, if it is a selection.
    fn dragged(&self) -> Option<Crop> {
        let (anchor, to) = self.drag?;
        let (_, scale, _) = self.view();
        let crop = self.source.square_from_drag(anchor, to);
        (crop.side * scale >= f64::from(MIN_DRAG) && self.source.usable(crop)).then_some(crop)
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

/// `image` placed so that working pixels `(x, y)` onward fill a view at
/// `scale` logical pixels per working pixel; `full` is the image's size in
/// working pixels.
fn placed(image: Arc<RenderImage>, full: (f64, f64), origin: (f64, f64), scale: f64) -> Img {
    img(image)
        .absolute()
        .left(px((-origin.0 * scale) as f32))
        .top(px((-origin.1 * scale) as f32))
        .w(px((full.0 * scale) as f32))
        .h(px((full.1 * scale) as f32))
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

    /// Ends a drag: a real selection is enlarged to fill the view, and a
    /// sharp preview of it is made off the UI thread.
    fn finish_avatar_drag(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.avatar_editor else {
            return;
        };
        let selected = editor.dragged();
        editor.drag = None;
        if let Some(crop) = selected {
            editor.zoom = Some((crop, None));
            let source = editor.source.clone();
            let making = cx.background_spawn(async move {
                render_image(source.square_preview(crop, VIEW_SIDE * 2))
            });
            cx.spawn(async move |this, cx| {
                let image = making.await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(editor) = &mut this.avatar_editor
                        && let Some((current, sharp)) = &mut editor.zoom
                        && *current == crop
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
        // The whole image enlarged at once; the sharp one replaces it when
        // ready.
        let image = match &editor.zoom {
            Some((crop, Some(sharp))) => {
                placed(sharp.clone(), (crop.side, crop.side), (0.0, 0.0), scale)
            }
            _ => placed(editor.whole.clone(), full, origin, scale),
        };
        // The dragged square, or before any selection the square that
        // would be uploaded.
        let outline = editor
            .dragged()
            .or_else(|| editor.zoom.is_none().then(|| editor.source.initial_crop()))
            .map(|crop| {
                div()
                    .absolute()
                    .left(px(((crop.x - origin.0) * scale) as f32))
                    .top(px(((crop.y - origin.1) * scale) as f32))
                    .w(px((crop.side * scale) as f32))
                    .h(px((crop.side * scale) as f32))
                    .border_2()
                    .border_color(gpui::white())
                    .child(div().size_full().border_1().border_color(gpui::black()))
            });
        let bounds = editor.bounds.clone();
        let picture = div()
            .id("avatar-editor-picture")
            .relative()
            .flex_shrink_0()
            .w(px(view.width))
            .h(px(view.height))
            .overflow_hidden()
            .cursor(CursorStyle::Crosshair)
            .child(image)
            .children(outline)
            .child(
                canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if let Some(editor) = &mut this.avatar_editor {
                        let point = editor.to_image(event.position);
                        editor.drag = Some((point, point));
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some(editor) = &mut this.avatar_editor else {
                    return;
                };
                if editor.drag.is_none() {
                    return;
                }
                if event.pressed_button != Some(MouseButton::Left) {
                    this.finish_avatar_drag(cx);
                    return;
                }
                let point = editor.to_image(event.position);
                if let Some((_, to)) = &mut editor.drag {
                    *to = point;
                }
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_avatar_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_avatar_drag(cx)),
            );
        // What will be uploaded, from the images already on screen.
        let crop = editor.crop();
        let k = f64::from(RESULT_SIDE) / crop.side;
        let result_image = match &editor.zoom {
            Some((crop, Some(sharp))) => {
                placed(sharp.clone(), (crop.side, crop.side), (0.0, 0.0), k)
            }
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
        let out = (crop.side.round() as u32)
            .min(AVATAR_OUTPUT_SIDE)
            .to_string();
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
                    self.i18n
                        .format("ircv3_avatar_edit_hint", &[("side", &out)]),
                )
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
                                .when(editor.zoom.is_none(), |b| b.opacity(0.5))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(editor) = &mut this.avatar_editor {
                                        editor.zoom = None;
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
        AttachmentSource, AvatarEditor, AvatarSource, LoadError, avatar_edit, open_error_key,
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

    #[test]
    fn dragging_selects_and_releasing_enlarges_the_selection() {
        let mut editor =
            AvatarEditor::new("server".into(), AttachmentSource::Drop, photo(1600, 1200));
        editor.bounds.set(Bounds::new(
            point(px(100.), px(50.)),
            size(px(320.), px(240.)),
        ));
        let (origin, scale, view) = editor.view();
        assert_eq!((origin, view), ((0.0, 0.0), size(320.0, 240.0)));
        assert!((scale - 0.2).abs() < 1e-9);
        // Before any selection, the centered square is used.
        assert_eq!((editor.crop().x, editor.crop().side), (200.0, 1200.0));
        // Mouse from (110, 60) to (150, 90) in the window: 40 logical
        // pixels = 200 working pixels, from (50, 50).
        let from = editor.to_image(point(px(110.), px(60.)));
        let to = editor.to_image(point(px(150.), px(90.)));
        assert_eq!(from, (50.0, 50.0));
        editor.drag = Some((from, to));
        let selected = editor.dragged().unwrap();
        assert_eq!((selected.x, selected.y, selected.side), (50.0, 50.0, 200.0));
        // Enlarged: the selection fills the 320-pixel view.
        editor.zoom = Some((selected, None));
        editor.drag = None;
        let (origin, scale, view) = editor.view();
        assert_eq!((origin, view), ((50.0, 50.0), size(320.0, 320.0)));
        assert!((scale - 1.6).abs() < 1e-9);
        assert_eq!(editor.crop(), selected);
        // A click is not a selection.
        let at = editor.to_image(point(px(200.), px(200.)));
        editor.drag = Some((at, (at.0 + 1.0, at.1 + 1.0)));
        assert_eq!(editor.dragged(), None);
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
