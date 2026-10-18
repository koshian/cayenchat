// SPDX-License-Identifier: GPL-3.0-only
//! IRC image sharing through the configured external host. Clipboard
//! images, dropped files and (for avatars) files picked in the system dialog
//! enter one attachment flow (`cayenchat_app::attachments`), which asks for
//! confirmation, uploads and inserts the link: into the chat draft in the
//! chat window, or into a server's avatar URL draft in the settings window
//! (`ircv3_settings`). Nothing here sends an IRC message or publishes an
//! avatar.

use std::{path::PathBuf, sync::Arc};

use cayenchat_app::Selection;
use cayenchat_app::attachments::{Completion, Offer, UploadFailure, UploadJob, UploaderReadiness};
use cayenchat_model::attachment::{
    Attachment, AttachmentError, AttachmentSource, ImageFormat, MAX_ATTACHMENT_BYTES, format_size,
};
use cayenchat_storage::SecretKey;
use cayenchat_upload::{ExternalUploader, UploadError};
use gpui::{prelude::*, *};

use crate::{ChatWindow, SettingsTab, input, localization::Localizer, secrets};

/// The uploader for `provider` (a provider ID from settings) and what the
/// attachment flow should know about it; `replacement` (tests) wins.
/// Credential failures become user-visible text.
pub(crate) fn configured_uploader(
    provider: Option<&str>,
    replacement: Option<&Arc<dyn ExternalUploader>>,
    i18n: &Localizer,
    cx: &App,
) -> Result<(UploaderReadiness, Option<Arc<dyn ExternalUploader>>), String> {
    if let Some(uploader) = replacement {
        let provider = uploader.provider().name.to_owned();
        return Ok((
            UploaderReadiness::Ready { provider },
            Some(uploader.clone()),
        ));
    }
    let Some(info) = provider.and_then(cayenchat_upload::provider) else {
        return Ok((UploaderReadiness::NotConfigured, None));
    };
    let provider = info.name.to_owned();
    match secrets::store(cx).get(&SecretKey::uploader_token(info.id)) {
        Ok(Some(token)) => Ok((
            UploaderReadiness::Ready { provider },
            cayenchat_upload::connect(info.id, token),
        )),
        Ok(None) => Ok((UploaderReadiness::NeedsAccount { provider }, None)),
        Err(error) => Err(secrets::error_text(i18n, &error)),
    }
}

/// What a file dialog offers when an image is wanted: the extensions of the
/// formats the attachment check accepts. Where a dialog cannot filter, that
/// check refuses the other files.
pub(crate) fn image_file_filter(i18n: &Localizer) -> PathPromptFilter {
    PathPromptFilter {
        name: i18n.text("upload_image_files").into(),
        extensions: ImageFormat::FILE_EXTENSIONS
            .iter()
            .map(|extension| SharedString::from(*extension))
            .collect(),
    }
}

/// An image from a paste or a single file, or the text explaining why not.
pub(crate) fn clipboard_attachment(cx: &App) -> Option<Result<Attachment, AttachmentError>> {
    let item = cx.read_from_clipboard()?;
    if item.text().is_some() {
        return None;
    }
    let image = input::clipboard_image(&item)?;
    Some(Attachment::image(
        None,
        image.bytes().to_vec(),
        AttachmentSource::Clipboard,
    ))
}

/// Why the file(s) offered could not be read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileReadError {
    /// A file promise (Photos, Mail) was not fulfilled.
    NoFile,
    TooMany,
    Io(String),
}

impl FileReadError {
    pub(crate) fn text(&self, i18n: &Localizer) -> String {
        match self {
            Self::NoFile => i18n.format("upload_read_failed", &[("error", "no file was received")]),
            Self::TooMany => i18n.text("upload_one_file"),
            Self::Io(error) => i18n.format("upload_read_failed", &[("error", error)]),
        }
    }
}

/// Reads the one file in `paths` as an image attachment. This blocks on
/// the file system, so run it off the UI thread.
pub(crate) fn file_attachment(
    paths: &[PathBuf],
    source: AttachmentSource,
) -> Result<Result<Attachment, AttachmentError>, FileReadError> {
    let [path] = paths else {
        return Err(if paths.is_empty() {
            FileReadError::NoFile
        } else {
            FileReadError::TooMany
        });
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    read_limited(path)
        .map(|bytes| Attachment::image(name.as_deref(), bytes, source))
        .map_err(FileReadError::Io)
}

/// What a background file read produces.
pub(crate) type FileRead = Result<Result<Attachment, AttachmentError>, FileReadError>;

/// Reads the file off the UI thread. Dropping the returned task discards
/// the result, so a later drop supersedes an earlier one. `keep` holds
/// files the platform deletes once it is dropped (macOS file promises)
/// until they are read.
pub(crate) fn read_in_background(
    paths: Vec<PathBuf>,
    keep: Option<Arc<PromisedFiles>>,
    source: AttachmentSource,
    cx: &App,
) -> Task<FileRead> {
    cx.background_spawn(async move {
        let read = file_attachment(&paths, source);
        drop(keep);
        read
    })
}

/// The attachment if it is an image the uploader accepts, or why not.
pub(crate) fn acceptable_attachment(
    attachment: Result<Attachment, AttachmentError>,
    uploader: Option<&Arc<dyn ExternalUploader>>,
    i18n: &Localizer,
) -> Result<Attachment, String> {
    let attachment = attachment.map_err(|error| match error {
        AttachmentError::NotAnImage => i18n.text("upload_not_image"),
        AttachmentError::TooLarge { limit } => {
            i18n.format("upload_failed_too_large", &[("limit", &format_size(limit))])
        }
    })?;
    if let Some(uploader) = uploader
        && attachment.bytes.len() > uploader.provider().max_bytes
    {
        let limit = format_size(uploader.provider().max_bytes);
        return Err(i18n.format("upload_failed_too_large", &[("limit", &limit)]));
    }
    Ok(attachment)
}

/// Runs a confirmed upload off the UI thread.
pub(crate) fn upload_in_background(
    uploader: Arc<dyn ExternalUploader>,
    job: UploadJob,
    cx: &App,
) -> Task<Result<String, UploadFailure>> {
    cx.background_spawn(async move {
        uploader
            .upload(&job.attachment)
            .map(|image| image.url)
            .map_err(|error| match error {
                UploadError::Authentication => UploadFailure::Authentication,
                UploadError::Rejected(reason) => UploadFailure::Rejected(reason),
                UploadError::Network(reason) => UploadFailure::Network(reason),
            })
    })
}

/// Text for a failed upload other than a rejected credential.
pub(crate) fn upload_failure_text(
    provider: &str,
    failure: UploadFailure,
    i18n: &Localizer,
) -> String {
    let (key, error) = match failure {
        UploadFailure::Network(error) => ("upload_failed_network", error),
        UploadFailure::Rejected(error) => ("upload_failed_rejected", error),
        UploadFailure::Authentication => ("upload_reconnect_title", String::new()),
    };
    i18n.format(key, &[("provider", provider), ("error", &error)])
}

impl ChatWindow {
    fn uploader(
        &self,
        cx: &App,
    ) -> Result<(UploaderReadiness, Option<Arc<dyn ExternalUploader>>), String> {
        configured_uploader(
            self.image_provider.as_deref(),
            self.uploader_override.as_ref(),
            &self.i18n,
            cx,
        )
    }

    /// Paste in a draft whose clipboard has an image and no text.
    pub(crate) fn paste_image(
        &mut self,
        _: &input::Paste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(attachment) = clipboard_attachment(cx) {
            let target = self.state.selection();
            self.accept_attachment(attachment, target, window, cx);
        }
    }

    /// Files dropped on the draft row. The draft they were dropped on is
    /// fixed now, so switching conversations while the file is read does not
    /// move the link into another draft.
    pub(crate) fn drop_paths(
        &mut self,
        paths: &[PathBuf],
        keep: Option<Arc<PromisedFiles>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.state.selection();
        let read = read_in_background(paths.to_vec(), keep, AttachmentSource::Drop, cx);
        // Replacing the task drops an earlier read that is still running.
        self.file_read = Some(cx.spawn_in(window, async move |this, cx| {
            let read = read.await;
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok(attachment) => this.accept_attachment(attachment, target, window, cx),
                Err(error) => {
                    this.feedback = Some(error.text(&this.i18n));
                    cx.notify();
                }
            });
        }));
    }

    fn accept_attachment(
        &mut self,
        attachment: Result<Attachment, AttachmentError>,
        target: Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let checked = acceptable_attachment(attachment, None, &self.i18n).and_then(|attachment| {
            let (readiness, uploader) = self.uploader(cx)?;
            let attachment = acceptable_attachment(Ok(attachment), uploader.as_ref(), &self.i18n)?;
            Ok((attachment, readiness, uploader))
        });
        let (attachment, readiness, uploader) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
                return;
            }
        };
        match self.attachments.offer(attachment, target, readiness) {
            Offer::Confirm {
                provider,
                name,
                size,
            } => {
                let answer = window.prompt(
                    PromptLevel::Info,
                    &self
                        .i18n
                        .format("upload_confirm_title", &[("provider", &provider)]),
                    Some(&self.i18n.format(
                        "upload_confirm_detail",
                        &[("provider", &provider), ("name", &name), ("size", &size)],
                    )),
                    &[
                        PromptButton::ok(self.i18n.text("upload_confirm")),
                        PromptButton::cancel(self.i18n.text("cancel")),
                    ],
                    cx,
                );
                cx.spawn_in(window, async move |this, cx| {
                    let accepted = answer.await == Ok(0);
                    let _ = this.update_in(cx, |this, window, cx| {
                        if accepted && let Some(uploader) = uploader {
                            this.start_upload(uploader, window, cx);
                        } else {
                            // Cancel uploads nothing and leaves the draft alone.
                            this.attachments.decline();
                        }
                    });
                })
                .detach();
            }
            Offer::Configure => self.prompt_configure_upload(window, cx),
            Offer::Reconnect { provider } => self.prompt_reconnect_upload(&provider, window, cx),
            Offer::Busy => {
                self.feedback = Some(self.i18n.text("upload_busy"));
                cx.notify();
            }
        }
    }

    /// The toolbar's upload button: an image is asked for only once the
    /// upload service is ready; otherwise the user is told first, with the way
    /// to the settings. The chosen file then goes the way a dropped one does.
    pub(crate) fn toolbar_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.uploader(cx) {
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
                return;
            }
            Ok((UploaderReadiness::NotConfigured, _)) => {
                return self.prompt_configure_upload(window, cx);
            }
            Ok((UploaderReadiness::NeedsAccount { provider }, _)) => {
                return self.prompt_reconnect_upload(&provider, window, cx);
            }
            Ok((UploaderReadiness::Ready { .. }, _)) => {}
        }
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
            filter: Some(image_file_filter(&self.i18n)),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.drop_paths(&paths, None, window, cx)
                });
            }
        })
        .detach();
    }

    fn prompt_configure_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_upload_setup(
            self.i18n.text("upload_not_configured_title"),
            self.i18n.text("upload_not_configured_detail"),
            self.i18n.text("upload_configure"),
            window,
            cx,
        );
    }

    fn prompt_reconnect_upload(
        &mut self,
        provider: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_upload_setup(
            self.i18n
                .format("upload_reconnect_title", &[("provider", provider)]),
            self.i18n
                .format("upload_reconnect_detail", &[("provider", provider)]),
            self.i18n.text("upload_reconnect"),
            window,
            cx,
        );
    }

    /// Asks whether to open image upload settings.
    fn prompt_upload_setup(
        &mut self,
        title: String,
        detail: String,
        action: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(
            PromptLevel::Info,
            &title,
            Some(&detail),
            &[
                PromptButton::ok(action),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.open_settings_tab(SettingsTab::ImageUpload, None, window, cx)
                });
            }
        })
        .detach();
    }

    fn start_upload(
        &mut self,
        uploader: Arc<dyn ExternalUploader>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = self.attachments.confirm() else {
            return;
        };
        self.feedback = None;
        cx.notify();
        let id = job.id;
        let upload = upload_in_background(uploader, job, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = upload.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let completion = this.attachments.finish(id, result);
                this.complete_upload(completion, window, cx);
            });
        })
        .detach();
    }

    fn complete_upload(
        &mut self,
        completion: Completion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match completion {
            Completion::InsertLink { target, url } => {
                // The draft may have been edited meanwhile; insert at its cursor.
                if let Some(input) = self.inputs.get(&target) {
                    input.update(cx, |input, cx| input.insert_link(&url, cx));
                }
                // The inserted link is the feedback; clear the progress note.
                self.feedback = None;
            }
            Completion::Failed {
                provider,
                failure: UploadFailure::Authentication,
            } => {
                // An expired or revoked credential: offer to reconnect.
                self.feedback = None;
                self.prompt_upload_setup(
                    self.i18n
                        .format("upload_reconnect_title", &[("provider", &provider)]),
                    self.i18n
                        .format("upload_reconnect_detail", &[("provider", &provider)]),
                    self.i18n.text("upload_reconnect"),
                    window,
                    cx,
                );
            }
            Completion::Failed { provider, failure } => {
                // A dialog stays until the user closes it, so the reason is
                // not lost, and it takes no room from the draft row.
                self.feedback = None;
                let detail = upload_failure_text(&provider, failure, &self.i18n);
                let answer = window.prompt(
                    PromptLevel::Warning,
                    &self.i18n.text("upload_failed_title"),
                    Some(&detail),
                    &[PromptButton::ok(self.i18n.text("upload_failed_close"))],
                    cx,
                );
                // Closing it only dismisses the dialog.
                cx.spawn(async move |_, _| {
                    let _ = answer.await;
                })
                .detach();
            }
            Completion::Ignored => {}
        }
        cx.notify();
    }

    pub(crate) fn cancel_upload(&mut self, cx: &mut Context<Self>) {
        if let Some(provider) = self.attachments.uploading().map(str::to_owned)
            && self.attachments.cancel_upload()
        {
            self.feedback = Some(
                self.i18n
                    .format("upload_cancelled", &[("provider", &provider)]),
            );
            cx.notify();
        }
    }

    /// Progress text while an upload runs.
    pub(crate) fn upload_status(&self) -> Option<String> {
        self.attachments.uploading().map(|provider| {
            self.i18n
                .format("upload_uploading", &[("provider", provider)])
        })
    }
}

/// Reads at most the attachment size guard, so a huge or endless file (a
/// FIFO or device) cannot exhaust memory.
fn read_limited(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    // Opening a FIFO blocks until a writer appears, so refuse anything but
    // a regular file before opening it.
    if !std::fs::metadata(path)
        .map_err(|error| error.kind().to_string())?
        .is_file()
    {
        return Err(std::io::ErrorKind::InvalidInput.to_string());
    }
    let file = std::fs::File::open(path).map_err(|error| error.kind().to_string())?;
    // The path may have been replaced since the check above.
    if !file
        .metadata()
        .map_err(|error| error.kind().to_string())?
        .is_file()
    {
        return Err(std::io::ErrorKind::InvalidInput.to_string());
    }
    let mut bytes = Vec::new();
    file.take(MAX_ATTACHMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.kind().to_string())?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cayenchat_app::Selection;
    use cayenchat_storage::{
        CredentialBackendKind, CredentialStore, Secret, SecretKey, credentials::MemoryBackend,
    };
    use cayenchat_upload::{UploadError, testing::FakeUploader};
    use gpui::{
        ClipboardItem, Entity, Focusable, Image, ImageFormat, TestAppContext, VisualTestContext,
    };

    use crate::{ChatWindow, input::TextInput, secrets};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-test-image-";
    const URL: &str = "https://img.example/abc.png";

    fn open_chat<'a>(
        cx: &'a mut TestAppContext,
        uploader: Option<Arc<FakeUploader>>,
        provider: Option<&str>,
    ) -> (
        Entity<ChatWindow>,
        Entity<TextInput>,
        CredentialStore,
        &'a mut VisualTestContext,
    ) {
        let store = CredentialStore::with_backend(Arc::new(MemoryBackend::new(
            CredentialBackendKind::System,
        )));
        let global = store.clone();
        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            cx.set_global(secrets::Credentials(global));
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ))
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.language = cayenchat_storage::Language::English;
        settings.image_upload.provider = provider.map(str::to_owned);
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let input = chat.update(cx, |chat, _| {
            if let Some(uploader) = uploader {
                chat.uploader_override = Some(uploader);
            }
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            chat.inputs[&Selection::Channel(channel)].clone()
        });
        cx.update(|window, cx| {
            window.focus(&input.focus_handle(cx));
            window.refresh();
        });
        cx.run_until_parked();
        (chat, input, store, cx)
    }

    fn image_clipboard() -> ClipboardItem {
        ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, PNG.to_vec()))
    }

    fn draft(input: &Entity<TextInput>, cx: &mut VisualTestContext) -> String {
        input.read_with(cx, |input, _| input.text().to_owned())
    }

    #[gpui::test]
    fn text_paste_stays_a_normal_paste(cx: &mut TestAppContext) {
        let fake = Arc::new(FakeUploader::succeeding(URL));
        let (_, input, _, cx) = open_chat(cx, Some(fake.clone()), None);
        cx.write_to_clipboard(ClipboardItem::new_string("hello\nthere".into()));
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        assert_eq!(draft(&input, cx), "hello there");
        assert!(!cx.has_pending_prompt());
        assert_eq!(fake.calls(), 0);
    }

    #[gpui::test]
    fn confirmed_image_paste_inserts_link_without_sending(cx: &mut TestAppContext) {
        let fake = Arc::new(FakeUploader::succeeding(URL));
        let (chat, input, _, cx) = open_chat(cx, Some(fake.clone()), None);
        cx.simulate_input("look:");
        cx.write_to_clipboard(image_clipboard());
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        let (title, detail) = cx.pending_prompt().expect("confirmation first");
        assert!(title.contains("Fake Host"), "{title}");
        assert!(detail.contains("Fake Host"));
        assert_eq!(fake.calls(), 0, "nothing leaves before confirmation");
        cx.simulate_prompt_answer("Upload");
        cx.run_until_parked();
        assert_eq!(fake.calls(), 1);
        assert_eq!(draft(&input, cx), format!("look: {URL}"));
        // The draft is only edited; it is not sent or cleared.
        let messages = chat.read_with(cx, |chat, _| chat.state.conversations()[0].messages.len());
        assert_eq!(messages, 0);
        assert!(!cx.has_pending_prompt());
    }

    #[gpui::test]
    fn cancelled_confirmation_uploads_nothing(cx: &mut TestAppContext) {
        let fake = Arc::new(FakeUploader::succeeding(URL));
        let (_, input, _, cx) = open_chat(cx, Some(fake.clone()), None);
        cx.simulate_input("keep me");
        cx.write_to_clipboard(image_clipboard());
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(fake.calls(), 0);
        assert_eq!(draft(&input, cx), "keep me");
    }

    #[gpui::test]
    fn failed_upload_preserves_the_draft(cx: &mut TestAppContext) {
        let fake = Arc::new(FakeUploader::failing(UploadError::Network(
            "offline".into(),
        )));
        let (chat, input, _, cx) = open_chat(cx, Some(fake.clone()), None);
        cx.simulate_input("draft");
        cx.write_to_clipboard(image_clipboard());
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Upload");
        cx.run_until_parked();
        assert_eq!(fake.calls(), 1);
        assert_eq!(draft(&input, cx), "draft");
        let (title, detail) = cx.pending_prompt().expect("failure dialog");
        assert!(title.contains("failed"), "{title}");
        assert!(detail.contains("offline"), "{detail}");
        cx.simulate_prompt_answer("Close");
        cx.run_until_parked();
        assert!(cx.pending_prompt().is_none());
        assert_eq!(draft(&input, cx), "draft");
        let feedback = chat.read_with(cx, |chat, _| chat.feedback.clone());
        assert!(feedback.is_none(), "{feedback:?}");
    }

    #[gpui::test]
    fn expired_credential_offers_to_reconnect(cx: &mut TestAppContext) {
        let fake = Arc::new(FakeUploader::failing(UploadError::Authentication));
        let (_, input, _, cx) = open_chat(cx, Some(fake.clone()), None);
        cx.write_to_clipboard(image_clipboard());
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Upload");
        cx.run_until_parked();
        let (title, _) = cx.pending_prompt().expect("reconnect guidance");
        assert!(title.contains("Reconnect"), "{title}");
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(draft(&input, cx), "");
    }

    #[gpui::test]
    fn missing_provider_guides_to_configuration(cx: &mut TestAppContext) {
        let (_, _, _, cx) = open_chat(cx, None, None);
        cx.write_to_clipboard(image_clipboard());
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        let (title, _) = cx.pending_prompt().expect("configuration guidance");
        assert!(title.contains("not configured"), "{title}");
        cx.simulate_prompt_answer("Cancel");
    }

    #[gpui::test]
    fn provider_without_account_guides_to_reconnect(cx: &mut TestAppContext) {
        let (chat, _, store, cx) = open_chat(cx, None, Some("imgbb"));
        cx.write_to_clipboard(image_clipboard());
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        let (title, _) = cx.pending_prompt().expect("account guidance");
        assert!(title.contains("ImgBB"), "{title}");
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        // With a saved token the same paste asks for upload confirmation.
        store
            .set(&SecretKey::uploader_token("imgbb"), &Secret::new("token"))
            .unwrap();
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        let (title, detail) = cx.pending_prompt().expect("confirmation");
        assert!(title.starts_with("Upload this image to ImgBB"), "{title}");
        assert!(detail.contains("outside this channel"));
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(chat.read_with(cx, |chat, _| chat.attachments.uploading().is_none()));
    }

    #[gpui::test]
    fn images_over_the_provider_limit_are_refused_before_confirmation(cx: &mut TestAppContext) {
        let fake = Arc::new(FakeUploader::succeeding(URL));
        let (chat, _, _, cx) = open_chat(cx, Some(fake.clone()), None);
        let mut big = PNG.to_vec();
        big.resize(100, 0);
        cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
            ImageFormat::Png,
            big,
        )));
        cx.dispatch_action(crate::input::Paste);
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
        assert_eq!(fake.calls(), 0);
        let feedback = chat.read_with(cx, |chat, _| chat.feedback.clone()).unwrap();
        assert!(feedback.contains("64 B"), "{feedback}");
    }

    #[test]
    fn directories_are_refused_and_images_are_read() {
        use super::{FileReadError, file_attachment};
        use cayenchat_model::attachment::AttachmentSource;
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("a.png");
        std::fs::write(&image, PNG).unwrap();
        let read = file_attachment(&[image], AttachmentSource::Drop).unwrap();
        assert!(read.is_ok());
        let error = file_attachment(&[directory.path().to_owned()], AttachmentSource::Drop);
        assert!(matches!(error, Err(FileReadError::Io(_))));
    }

    /// Opening a FIFO without a writer would block forever.
    #[cfg(unix)]
    #[test]
    fn fifos_are_refused_without_being_opened() {
        use super::{FileReadError, file_attachment};
        use cayenchat_model::attachment::AttachmentSource;
        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(file_attachment(&[fifo], AttachmentSource::Drop));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the FIFO must not be opened");
        assert!(matches!(result, Err(FileReadError::Io(_))));
    }

    #[gpui::test]
    fn dropped_image_goes_to_the_draft_it_was_dropped_on(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("slow.png");
        std::fs::write(&image, PNG).unwrap();
        let fake = Arc::new(FakeUploader::succeeding(URL));
        let (chat, input, _, cx) = open_chat(cx, Some(fake), None);
        chat.update_in(cx, |chat, window, cx| {
            chat.drop_paths(std::slice::from_ref(&image), None, window, cx);
            // The read has not finished; the user moves to the server.
            chat.state
                .dispatch(cayenchat_app::Command::SelectServerAt(0));
        });
        cx.run_until_parked();
        cx.simulate_prompt_answer("Upload");
        cx.run_until_parked();
        assert_eq!(draft(&input, cx), URL.to_owned());
        let server_inputs = chat.read_with(cx, |chat, _| {
            chat.inputs
                .iter()
                .filter(|(key, _)| matches!(key, Selection::Server(_)))
                .map(|(_, input)| input.clone())
                .collect::<Vec<_>>()
        });
        for server_input in server_inputs {
            assert_eq!(draft(&server_input, cx), "", "the server draft stays empty");
        }
    }

    #[gpui::test]
    fn promised_files_stay_until_the_read_ends(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let promised = directory.path().join("promised");
        std::fs::create_dir(&promised).unwrap();
        let image = promised.join("shot.png");
        std::fs::write(&image, PNG).unwrap();
        let keep = Arc::new(gpui::PromisedFiles::new(promised.clone()));
        let fake = Arc::new(FakeUploader::succeeding(URL));
        let (chat, _, _, cx) = open_chat(cx, Some(fake), None);
        chat.update_in(cx, |chat, window, cx| {
            chat.drop_paths(std::slice::from_ref(&image), Some(keep), window, cx)
        });
        // The drop handler returned and the platform holds no handle: the
        // file is still there for the background read.
        assert!(image.exists());
        cx.run_until_parked();
        assert!(!promised.exists(), "removed once the read is done");
        let (_, detail) = cx.pending_prompt().unwrap();
        assert!(detail.contains("shot.png"), "{detail}");
    }

    #[gpui::test]
    fn dropped_image_uses_the_same_flow(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("dropped.png");
        let text = directory.path().join("notes.txt");
        std::fs::write(&image, PNG).unwrap();
        std::fs::write(&text, "plain").unwrap();
        let fake = Arc::new(FakeUploader::succeeding(URL));
        let (chat, input, _, cx) = open_chat(cx, Some(fake.clone()), None);
        chat.update_in(cx, |chat, window, cx| {
            chat.drop_paths(std::slice::from_ref(&text), None, window, cx)
        });
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt(), "non-images are refused");
        chat.update_in(cx, |chat, window, cx| {
            chat.drop_paths(std::slice::from_ref(&image), None, window, cx)
        });
        cx.run_until_parked();
        let (_, detail) = cx.pending_prompt().unwrap();
        assert!(detail.contains("dropped.png"), "{detail}");
        cx.simulate_prompt_answer("Upload");
        cx.run_until_parked();
        assert_eq!(fake.uploaded_names(), ["dropped.png"]);
        assert_eq!(draft(&input, cx), URL.to_owned());
    }
}
