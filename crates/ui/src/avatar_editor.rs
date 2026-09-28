//! The square selection shown before an avatar image is uploaded (IRCv3
//! settings tab). The image is decoded off the UI thread with the preview
//! limits (`media::avatar_edit`); the user moves the square by dragging,
//! resizes it with the scroll wheel or the buttons, sees the result, and
//! uploads a small square image. Nothing is uploaded until they click the
//! upload button, and uploading still does not publish.

use std::sync::Arc;

use cayenchat_media::{
    LoadError,
    avatar_edit::{self, AVATAR_OUTPUT_SIDE, AvatarSource, Crop},
    decode::{self, Thumbnail},
};
use cayenchat_model::attachment::AttachmentSource;
use gpui::{prelude::*, *};

use crate::{SettingsWindow, settings_theme};

/// Largest size of the editor image, in logical pixels.
const VIEW_SIDE: u32 = 320;
/// Size of the result preview, in logical pixels.
const RESULT_SIDE: f32 = 64.0;
/// Selection change per scroll step or button press.
const ZOOM_STEP: f64 = 0.9;

pub(crate) struct AvatarEditor {
    /// Server profile whose avatar draft receives the uploaded URL.
    pub profile: String,
    pub source_kind: AttachmentSource,
    source: Arc<AvatarSource>,
    image: Arc<RenderImage>,
    /// Displayed size of the whole image, in logical pixels.
    view: Size<f32>,
    crop: Crop,
    /// Where a drag started, and the selection then.
    drag: Option<(Point<Pixels>, Crop)>,
}

impl AvatarEditor {
    fn new(profile: String, source_kind: AttachmentSource, source: AvatarSource) -> Self {
        let preview = source.preview(VIEW_SIDE * 2);
        let (width, height) = decode::fit(source.width(), source.height(), VIEW_SIDE, VIEW_SIDE);
        let crop = source.initial_crop();
        Self {
            profile,
            source_kind,
            image: render_image(preview),
            view: size(width as f32, height as f32),
            source: Arc::new(source),
            crop,
            drag: None,
        }
    }

    /// Logical pixels per working-image pixel.
    fn scale(&self) -> f64 {
        f64::from(self.view.width) / f64::from(self.source.width())
    }

    pub fn source(&self) -> Arc<AvatarSource> {
        self.source.clone()
    }

    pub fn crop(&self) -> Crop {
        self.crop
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

    fn change_avatar_crop(
        &mut self,
        change: impl FnOnce(&AvatarSource, Crop) -> Crop,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &mut self.avatar_editor {
            editor.crop = change(&editor.source, editor.crop);
            cx.notify();
        }
    }

    pub(crate) fn render_avatar_editor(
        &self,
        provider: &str,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let editor = self.avatar_editor.as_ref()?;
        let theme = settings_theme::palette(cx);
        let scale = editor.scale() as f32;
        let crop = editor.crop;
        let (x, y, side) = (
            crop.x as f32 * scale,
            crop.y as f32 * scale,
            crop.side as f32 * scale,
        );
        // The selection: a bright outline with a dark edge, readable on
        // any photo.
        let selection = div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(side))
            .h(px(side))
            .border_2()
            .border_color(gpui::white())
            .child(div().size_full().border_1().border_color(gpui::black()));
        let picture = div()
            .id("avatar-editor-picture")
            .relative()
            .flex_shrink_0()
            .w(px(editor.view.width))
            .h(px(editor.view.height))
            .overflow_hidden()
            .cursor(CursorStyle::OpenHand)
            .child(img(editor.image.clone()).size_full())
            .child(selection)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if let Some(editor) = &mut this.avatar_editor {
                        editor.drag = Some((event.position, editor.crop));
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some(editor) = &mut this.avatar_editor else {
                    return;
                };
                let Some((start, from)) = editor.drag else {
                    return;
                };
                if event.pressed_button != Some(MouseButton::Left) {
                    editor.drag = None;
                    return;
                }
                let scale = editor.scale();
                let dx = f64::from(f32::from(event.position.x - start.x)) / scale;
                let dy = f64::from(f32::from(event.position.y - start.y)) / scale;
                editor.crop = editor.source.moved(from, dx, dy);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| {
                    if let Some(editor) = &mut this.avatar_editor {
                        editor.drag = None;
                    }
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                let delta = f32::from(event.delta.pixel_delta(px(16.)).y);
                if delta != 0.0 {
                    let factor = if delta > 0.0 {
                        ZOOM_STEP
                    } else {
                        1.0 / ZOOM_STEP
                    };
                    this.change_avatar_crop(|source, crop| source.scaled(crop, factor), cx);
                }
            }));
        // What will be uploaded: the selection clipped from the same image.
        let k = RESULT_SIDE / side.max(1.0);
        let result = div()
            .relative()
            .flex_shrink_0()
            .w(px(RESULT_SIDE))
            .h(px(RESULT_SIDE))
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .child(
                img(editor.image.clone())
                    .absolute()
                    .left(px(-x * k))
                    .top(px(-y * k))
                    .w(px(editor.view.width * k))
                    .h(px(editor.view.height * k)),
            );
        let out = (crop.side.round() as u32)
            .min(AVATAR_OUTPUT_SIDE)
            .to_string();
        let button = |id: &'static str, key: &str| {
            settings_theme::button(id, false, cx).child(self.i18n.text(key))
        };
        Some(
            div()
                .ml(px(158.))
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .border_1()
                .border_color(theme.border)
                .child(self.i18n.format(
                    "ircv3_avatar_edit_hint",
                    &[
                        ("width", &editor.source.width().to_string()),
                        ("height", &editor.source.height().to_string()),
                        ("side", &out),
                    ],
                ))
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
                            button("avatar-editor-smaller", "ircv3_avatar_edit_zoom_in").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.change_avatar_crop(|s, c| s.scaled(c, ZOOM_STEP), cx)
                                }),
                            ),
                        )
                        .child(
                            button("avatar-editor-larger", "ircv3_avatar_edit_zoom_out").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.change_avatar_crop(|s, c| s.scaled(c, 1.0 / ZOOM_STEP), cx)
                                }),
                            ),
                        )
                        .child(
                            button("avatar-editor-reset", "ircv3_avatar_edit_reset").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.change_avatar_crop(|s, _| s.initial_crop(), cx)
                                }),
                            ),
                        ),
                )
                .child(
                    div()
                        .flex()
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
                            button("avatar-editor-cancel", "cancel").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.avatar_editor = None;
                                    cx.notify();
                                },
                            )),
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
    use gpui::size;

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
    fn the_editor_fits_the_photo_and_starts_with_the_centered_square() {
        let editor = AvatarEditor::new("server".into(), AttachmentSource::Drop, photo(1600, 1200));
        assert_eq!(editor.view, size(320.0, 240.0));
        assert!((editor.scale() - 0.2).abs() < 1e-9);
        assert_eq!(editor.crop().side, 1200.0);
        assert_eq!(editor.crop().x, 200.0);
        // Small images are shown at their size.
        let small = AvatarEditor::new("server".into(), AttachmentSource::Drop, photo(100, 60));
        assert_eq!(small.view, size(100.0, 60.0));
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
