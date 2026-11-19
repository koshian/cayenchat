// SPDX-License-Identifier: GPL-3.0-only
//! Choosing a channel's wallpaper image and its range (#950): the file dialog
//! and a drop on the window end in the shared range editor
//! (`crate::wallpaper_crop`); "Use" sets the wallpaper to the kept file.

use std::{path::PathBuf, sync::Arc};

use cayenchat_model::attachment::AttachmentSource;
use gpui::{prelude::*, *};

use super::ChannelSettingsView;
#[cfg(test)]
use cayenchat_media::avatar_edit;

/// File name extensions of the formats the range editor opens.
const WALLPAPER_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"];

impl ChannelSettingsView {
    /// Opens the file chooser for the wallpaper.
    pub(super) fn browse_wallpaper(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.i18n.text("channel_look_wallpaper").into()),
            filter: Some(PathPromptFilter {
                name: self.i18n.text("upload_image_files").into(),
                extensions: WALLPAPER_EXTENSIONS
                    .iter()
                    .map(|extension| SharedString::from(*extension))
                    .collect(),
            }),
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
        self.crop.offer(
            |this: &mut Self| &mut this.crop,
            &self.i18n,
            paths,
            keep,
            source,
            cx,
        );
    }

    /// Cuts the rectangle out, keeps it as the wallpaper file and sets the
    /// field to its path.
    pub(super) fn use_crop(&mut self, cx: &mut Context<Self>) {
        self.crop.use_range(
            |this: &mut Self| &mut this.crop,
            &self.i18n,
            |this, path, cx| {
                this.look
                    .wallpaper
                    .update(cx, |field, cx| field.set_text(&path, cx));
            },
            cx,
        );
    }

    /// The drop handler for the window and the miniature.
    pub(super) fn on_wallpaper_drop(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        self.offer_wallpaper(
            paths.paths().to_vec(),
            paths.keep_alive(),
            AttachmentSource::Drop,
            cx,
        );
    }

    /// The range editor, or the status of opening the image.
    pub(super) fn render_crop(
        &self,
        palette: &crate::theme::Theme,
        ring: Rgba,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let [crop_use, crop_whole, crop_cancel] = [
            &self.controls.crop_use,
            &self.controls.crop_whole,
            &self.controls.crop_cancel,
        ];
        self.crop.render(
            |this: &mut Self| &mut this.crop,
            |this, cx| this.use_crop(cx),
            &self.i18n,
            palette,
            ring,
            [crop_use, crop_whole, crop_cancel],
            cx,
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
