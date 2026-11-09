// SPDX-License-Identifier: GPL-3.0-only
//! Choosing a wallpaper image and its range (#950). A file chosen in the
//! file dialog or dropped on the window (also a Photos file promise, which
//! is read before the platform deletes it) is decoded off the UI thread by
//! the avatar editor's decoder (`media::avatar_edit`, the preview limits and
//! EXIF orientation), and shown in the shared range editor
//! (`crop_editor::ImageCropEditor`, here a free rectangle that can be moved
//! and resized by its corners and sides). "Use" cuts the rectangle out,
//! shrinks it and keeps it as a PNG file of the application's own
//! (`wallpapers` in the data directory), whose path becomes the wallpaper.
//! The original file is not needed afterwards, so a temporary drop or an
//! image later moved is no problem. A path typed into the field is still
//! used as it is.

use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
};

use cayenchat_media::avatar_edit::{self, AvatarSource};
use cayenchat_model::attachment::AttachmentSource;
use gpui::{prelude::*, *};

use super::ChannelSettingsView;
use crate::{
    avatar_editor::open_error_key,
    crop_editor::{CropConstraint, ImageCropEditor},
    image_upload::{acceptable_attachment, image_file_filter, read_in_background},
    settings_theme,
};

/// Largest size of the range view, in logical pixels.
const VIEW_WIDTH: u32 = 360;
const VIEW_HEIGHT: u32 = 240;
/// The kept image is at most this many pixels a side; the wallpaper loader
/// reduces it further (640 px), this leaves room for a sharper fit later.
const OUTPUT_SIDE: u32 = 1280;
/// Every file the application cut out starts with this. Nothing deletes
/// them: equal cuts share one file, and a channel's saved settings may still
/// name it.
const FILE_PREFIX: &str = "wallpaper-";

/// What the window holds of the wallpaper image being chosen.
pub(super) struct CropState {
    pub(super) editor: Option<Entity<ImageCropEditor>>,
    /// Repaints the window (the "Whole Image" button) when the range
    /// changes.
    _observing: Option<Subscription>,
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
            _observing: None,
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
                        let editor = cx.new(|_| {
                            ImageCropEditor::new(
                                source,
                                CropConstraint::Free,
                                (VIEW_WIDTH, VIEW_HEIGHT),
                            )
                        });
                        this.crop._observing = Some(cx.observe(&editor, |_, _, cx| cx.notify()));
                        this.crop.editor = Some(editor);
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
        let (source, region) = {
            let editor = editor.read(cx);
            (editor.source(), editor.region())
        };
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
        let Some(editor) = self.crop.editor.clone() else {
            return status.map(|status| div().flex().flex_col().gap_1().child(status));
        };
        let whole = editor.read(cx).is_initial();
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
                .child(editor)
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

    /// The range editor's rectangle, when it is open.
    pub(crate) fn crop_region_for_test(&self, cx: &App) -> Option<avatar_edit::Region> {
        self.crop
            .editor
            .as_ref()
            .map(|editor| editor.read(cx).region())
    }

    /// The status line (text, failed) under the wallpaper row.
    pub(crate) fn crop_status_for_test(&self) -> Option<(String, bool)> {
        self.crop.status.clone()
    }

    pub(crate) fn set_crop_region_for_test(
        &mut self,
        region: avatar_edit::Region,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &self.crop.editor {
            editor.update(cx, |editor, _| editor.set_region(region));
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
    use super::keep;

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
