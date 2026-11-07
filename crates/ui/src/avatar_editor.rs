// SPDX-License-Identifier: GPL-3.0-only
//! The square selection shown before an avatar image is uploaded (IRCv3
//! settings tab). The image is decoded off the UI thread with the preview
//! limits (`media::avatar_edit`) and the selection is the shared
//! [`ImageCropEditor`] constrained to a square (`crop_editor`: handles,
//! dragging, close-up of a small selection). What is left here is the
//! result preview, the buttons and the upload: the selected square is
//! uploaded, shrunk, and then sent to the server.

use std::sync::Arc;

use cayenchat_media::{
    LoadError,
    avatar_edit::{self, AvatarSource, Crop, Region},
};
use cayenchat_model::attachment::AttachmentSource;
use gpui::{prelude::*, *};

use crate::{
    SettingsWindow,
    crop_editor::{CropConstraint, ImageCropEditor},
    settings_theme,
};

/// Largest size of the editor view, in logical pixels.
const VIEW_SIDE: u32 = 320;
/// Size of the result preview, in logical pixels.
const RESULT_SIDE: f32 = 64.0;

pub(crate) struct AvatarEditor {
    /// Server profile whose avatar draft receives the uploaded URL.
    pub profile: String,
    pub source_kind: AttachmentSource,
    editor: Entity<ImageCropEditor>,
    /// Repaints this view (the result preview) when the selection changes.
    _observing: Subscription,
}

impl AvatarEditor {
    fn new(
        profile: String,
        source_kind: AttachmentSource,
        source: AvatarSource,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let editor = cx
            .new(|_| ImageCropEditor::new(source, CropConstraint::Square, (VIEW_SIDE, VIEW_SIDE)));
        Self {
            profile,
            source_kind,
            _observing: cx.observe(&editor, |_, _, cx| cx.notify()),
            editor,
        }
    }

    /// What will be uploaded.
    pub fn crop(&self, cx: &App) -> Crop {
        square(self.editor.read(cx).region())
    }

    pub fn source(&self, cx: &App) -> Arc<AvatarSource> {
        self.editor.read(cx).source()
    }
}

/// A square selection as the crop to encode.
fn square(region: Region) -> Crop {
    Crop {
        x: region.x,
        y: region.y,
        side: region.width,
    }
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
                        this.avatar_editor =
                            Some(AvatarEditor::new(profile, source_kind, source, cx));
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

    pub(crate) fn render_avatar_editor(
        &self,
        provider: &str,
        registers: bool,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let editor = self.avatar_editor.as_ref()?;
        let theme = settings_theme::palette(cx);
        let crop_editor = editor.editor.clone();
        // What will be uploaded, from the images already on screen.
        let result = crop_editor
            .read(cx)
            .selection_preview(RESULT_SIDE)
            .border_1()
            .border_color(theme.border);
        let whole = crop_editor.read(cx).is_initial();
        Some(
            div()
                .ml(settings_theme::LABEL_INDENT)
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .items_start()
                        .child(crop_editor.clone())
                        .child(
                            div().flex().flex_col().gap_1().child(result).child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(self.i18n.text("ircv3_avatar_edit_result")),
                            ),
                        ),
                )
                .child(
                    crate::button_order::row(div().flex().flex_wrap().gap_2())
                        .child(
                            settings_theme::button("avatar-editor-upload", true, cx)
                                .child(
                                    self.i18n.format(
                                        "ircv3_avatar_edit_upload",
                                        &[("provider", provider)],
                                    ),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.upload_edited_avatar(registers, cx)
                                })),
                        )
                        .child(
                            settings_theme::button("avatar-editor-reset", false, cx)
                                .child(self.i18n.text("ircv3_avatar_edit_reset"))
                                .when(whole, |b| b.opacity(0.5))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    crop_editor.update(cx, |editor, cx| {
                                        editor.reset();
                                        cx.notify();
                                    });
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
    use super::{Crop, LoadError, Region, open_error_key, square};

    #[test]
    fn a_square_region_is_the_crop_to_encode() {
        let region = Region {
            x: 200.0,
            y: 10.0,
            width: 600.0,
            height: 600.0,
        };
        assert_eq!(
            square(region),
            Crop {
                x: 200.0,
                y: 10.0,
                side: 600.0
            }
        );
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
