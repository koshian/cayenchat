// SPDX-License-Identifier: GPL-3.0-only
//! Choosing the range of a wallpaper image (#950, #989), shared by the
//! channel settings and the Appearance tab. A file chosen in the file dialog
//! or dropped (also a Photos file promise, which is read before the platform
//! deletes it) is decoded off the UI thread by the avatar editor's decoder
//! (`media::avatar_edit`, the preview limits and EXIF orientation), and shown
//! in the shared range editor (`crop_editor::ImageCropEditor`, here a free
//! rectangle). "Use" cuts the rectangle out, shrinks it and keeps it as a PNG
//! file of the application's own (`wallpapers` in the data directory). The
//! original file is not needed afterwards, so a temporary drop or an image
//! later moved is no problem.
//!
//! The state lives in the owning view; the functions take the view's
//! accessor to it, so a late answer finds its way back like the wallpaper
//! slots' do.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use cayenchat_media::avatar_edit::{self, AvatarSource};
use cayenchat_model::attachment::{AttachmentError, AttachmentSource};
use gpui::{prelude::*, *};

use crate::{
    avatar_editor::open_error_key,
    crop_editor::{CropConstraint, ImageCropEditor},
    image_upload::{acceptable_attachment, read_in_background},
    localization::Localizer,
    settings_theme,
};

/// Largest size of the range view, in logical pixels.
const VIEW_WIDTH: u32 = 360;
const VIEW_HEIGHT: u32 = 240;
/// The kept image is at most this many pixels a side; the wallpaper loader
/// reduces it further (640 px), this leaves room for a sharper fit later.
const OUTPUT_SIDE: u32 = 1280;
/// Every file the application cut out starts with this. Nothing deletes
/// them: equal cuts share one file, and saved settings may still name it.
const FILE_PREFIX: &str = "wallpaper-";

/// What a view holds of the wallpaper image being chosen.
pub(crate) struct CropState {
    pub(crate) editor: Option<Entity<ImageCropEditor>>,
    /// Repaints the view (the "Whole Image" button) when the range changes.
    _observing: Option<Subscription>,
    /// "Opening the image…", or why the image could not be used.
    pub(crate) status: Option<(String, bool)>,
    /// Reading and decoding; replacing it drops an older request.
    task: Option<Task<()>>,
    /// Where the kept images go instead of the application's own directory
    /// (tests).
    pub(crate) directory: Option<PathBuf>,
}

/// The focus handles of the range editor's three buttons.
pub(crate) fn button_handles(cx: &mut App) -> [FocusHandle; 3] {
    std::array::from_fn(|_| cx.focus_handle().tab_stop(true))
}

impl CropState {
    pub(crate) fn new() -> Self {
        Self {
            editor: None,
            _observing: None,
            status: None,
            task: None,
            directory: None,
        }
    }

    /// Whether the range editor is open.
    pub(crate) fn is_open(&self) -> bool {
        self.editor.is_some()
    }

    /// Closes the editor and drops a request in progress. Returns whether
    /// there was anything to close.
    pub(crate) fn close(&mut self) -> bool {
        let had = self.editor.is_some() || self.task.is_some() || self.status.is_some();
        *self = Self {
            directory: self.directory.take(),
            ..Self::new()
        };
        had
    }

    /// Reads the image named by `paths` (one file; `keep` holds a file
    /// promise until it is read) and opens the range editor on it. `get`
    /// finds this state in the view again.
    pub(crate) fn offer<V: 'static>(
        &mut self,
        get: fn(&mut V) -> &mut CropState,
        i18n: &Localizer,
        paths: Vec<PathBuf>,
        keep: Option<Arc<PromisedFiles>>,
        source: AttachmentSource,
        cx: &mut Context<V>,
    ) {
        self.close();
        self.status = Some((i18n.text("ircv3_avatar_edit_loading"), false));
        let read = read_in_background(paths, keep, source, cx);
        let i18n = i18n.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let outcome: Result<AvatarSource, String> = match read.await {
                Err(error) => Err(error.text(&i18n)),
                // Not the upload's wording: a wallpaper is not uploaded.
                Ok(Err(AttachmentError::NotAnImage)) => Err(i18n.text("wallpaper_not_image")),
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
                let state = get(this);
                state.task = None;
                match outcome {
                    Ok(source) => {
                        state.status = None;
                        let editor = cx.new(|_| {
                            ImageCropEditor::new(
                                source,
                                CropConstraint::Free,
                                (VIEW_WIDTH, VIEW_HEIGHT),
                            )
                        });
                        get(this)._observing = Some(cx.observe(&editor, |_, _, cx| cx.notify()));
                        get(this).editor = Some(editor);
                    }
                    Err(error) => state.status = Some((error, true)),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Cuts the rectangle out and keeps it as the wallpaper file; `kept`
    /// then receives its path.
    pub(crate) fn use_range<V: 'static>(
        &mut self,
        get: fn(&mut V) -> &mut CropState,
        i18n: &Localizer,
        kept: fn(&mut V, String, &mut Context<V>),
        cx: &mut Context<V>,
    ) {
        let Some(editor) = &self.editor else {
            return;
        };
        let (source, region) = {
            let editor = editor.read(cx);
            (editor.source(), editor.region())
        };
        let i18n = i18n.clone();
        let directory = self.directory.clone();
        self.status = Some((i18n.text("channel_look_crop_saving"), false));
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let i18n = i18n.clone();
                    async move {
                        let directory = match directory {
                            Some(directory) => directory,
                            None => cayenchat_storage::wallpaper_directory()?,
                        };
                        let png = source
                            .encode_region(region, OUTPUT_SIDE)
                            .map_err(|_| i18n.text("ircv3_avatar_edit_unreadable"))?;
                        keep(&directory, &png)
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                get(this).task = None;
                match result {
                    Ok(path) => {
                        let path = path.to_string_lossy().into_owned();
                        if path.len() > cayenchat_storage::MAX_WALLPAPER_PATH_BYTES {
                            // The file is kept, but the settings cannot name it.
                            let text =
                                i18n.format("channel_look_crop_path_long", &[("path", &path)]);
                            get(this).status = Some((text, true));
                        } else {
                            get(this).close();
                            kept(this, path, cx);
                        }
                    }
                    Err(error) => {
                        let text = i18n.format("channel_look_crop_failed", &[("error", &error)]);
                        get(this).status = Some((text, true));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The range editor with its buttons, or the status of opening the
    /// image; `None` when there is neither. `focus` are the buttons' handles
    /// (use, whole image, cancel).
    #[allow(clippy::too_many_arguments)] // the view's accessors and focus handles
    pub(crate) fn render<V: 'static>(
        &self,
        get: fn(&mut V) -> &mut CropState,
        use_range: fn(&mut V, &mut Context<V>),
        i18n: &Localizer,
        palette: &crate::theme::Theme,
        ring: Rgba,
        focus: [&FocusHandle; 3],
        cx: &mut Context<V>,
    ) -> Option<Div> {
        let status = self.status.as_ref().map(|(text, failed)| {
            div()
                .when(*failed, |d| d.text_color(palette.warning))
                .when(!*failed, |d| d.text_color(palette.text_secondary))
                .child(text.clone())
        });
        let Some(editor) = self.editor.clone() else {
            return status.map(|status| div().flex().flex_col().gap_1().child(status));
        };
        let whole = editor.read(cx).is_initial();
        let buttons = crate::button_order::row(div().flex().flex_wrap().gap_2())
            .child(
                settings_theme::button("channel-look-crop-use", true, cx)
                    .track_focus(focus[0])
                    .focus(move |style| style.border_color(ring))
                    .child(i18n.text("channel_look_crop_use"))
                    .on_click(cx.listener(move |this, _, _, cx| use_range(this, cx))),
            )
            .child(
                settings_theme::button("channel-look-crop-whole", false, cx)
                    .track_focus(focus[1])
                    .focus(move |style| style.border_color(ring))
                    .child(i18n.text("ircv3_avatar_edit_reset"))
                    .when(whole, |b| b.opacity(0.5))
                    .on_click({
                        let editor = editor.clone();
                        cx.listener(move |_, _, _, cx| {
                            editor.update(cx, |editor, cx| {
                                editor.reset();
                                cx.notify();
                            });
                        })
                    }),
            )
            .child(
                settings_theme::button("channel-look-crop-cancel", false, cx)
                    .track_focus(focus[2])
                    .focus(move |style| style.border_color(ring))
                    .child(i18n.text("cancel"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        get(this).close();
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
                        .child(i18n.text("channel_look_crop_hint")),
                )
                .child(editor)
                .children(status)
                .child(buttons),
        )
    }
}

/// The file name for the kept image: its content decides, so a different
/// cut gets a different path and the wallpaper is loaded again.
fn file_name(png: &[u8]) -> String {
    format!("{FILE_PREFIX}{}.png", content_hex(png))
}

/// The SHA-256 of `bytes` in lower-case hex. Unlike `DefaultHasher` it is the
/// same in every process, so the same content keeps the same file name after
/// a restart, and a collision is not a practical concern.
pub(crate) fn content_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Writes `png` into `directory` and returns its path.
pub(crate) fn keep(directory: &Path, png: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = directory.join(file_name(png));
    if !path.is_file() {
        std::fs::write(&path, png).map_err(|error| error.to_string())?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{content_hex, file_name, keep};

    #[test]
    fn names_follow_a_fixed_sha256_of_the_content() {
        // SHA-256 test vectors: the name must not depend on the process.
        assert_eq!(
            content_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            file_name(b"abc"),
            "wallpaper-ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad.png"
        );
        assert_ne!(file_name(b"abc"), file_name(b"abd"));
    }

    #[test]
    fn kept_files_are_named_by_content_and_never_deleted() {
        let dir = std::env::temp_dir().join(format!("cayenchat-crop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = keep(&dir, b"one").unwrap();
        let second = keep(&dir, b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(keep(&dir, b"one").unwrap(), first, "same cut, same file");
        assert_eq!(first, dir.join(file_name(b"one")));
        // Choosing another range leaves the earlier file: another channel
        // (or the saved settings) may still use it.
        assert!(first.is_file() && second.is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
