//! Chat-window side of IRC image sharing: clipboard images and dropped files
//! enter one attachment flow (`cayenchat_app::attachments`), which asks for
//! confirmation, uploads through the configured external host and inserts
//! the link into the draft. Nothing here sends an IRC message.

use std::{path::PathBuf, sync::Arc};

use cayenchat_app::attachments::{Completion, Offer, UploadFailure, UploaderReadiness};
use cayenchat_model::attachment::{
    Attachment, AttachmentError, AttachmentSource, MAX_ATTACHMENT_BYTES, format_size,
};
use cayenchat_storage::SecretKey;
use cayenchat_upload::{ExternalUploader, UploadError};
use gpui::{prelude::*, *};

use crate::{ChatWindow, SettingsTab, input, secrets};

impl ChatWindow {
    /// The uploader to use now and what the attachment flow should know
    /// about it. Credential failures become user-visible text.
    fn uploader(
        &self,
        cx: &App,
    ) -> Result<(UploaderReadiness, Option<Arc<dyn ExternalUploader>>), String> {
        if let Some(uploader) = &self.uploader_override {
            let provider = uploader.provider().name.to_owned();
            return Ok((
                UploaderReadiness::Ready { provider },
                Some(uploader.clone()),
            ));
        }
        let Some(info) = self
            .image_provider
            .as_deref()
            .and_then(cayenchat_upload::provider)
        else {
            return Ok((UploaderReadiness::NotConfigured, None));
        };
        let provider = info.name.to_owned();
        match secrets::store(cx).get(&SecretKey::uploader_token(info.id)) {
            Ok(Some(token)) => Ok((
                UploaderReadiness::Ready { provider },
                cayenchat_upload::connect(info.id, token),
            )),
            Ok(None) => Ok((UploaderReadiness::NeedsAccount { provider }, None)),
            Err(error) => Err(secrets::error_text(&self.i18n, &error)),
        }
    }

    /// Paste in a draft whose clipboard has an image and no text.
    pub(crate) fn paste_image(
        &mut self,
        _: &input::Paste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if item.text().is_some() {
            return;
        }
        let Some(image) = input::clipboard_image(&item) else {
            return;
        };
        let attachment =
            Attachment::image(None, image.bytes().to_vec(), AttachmentSource::Clipboard);
        self.accept_attachment(attachment, window, cx);
    }

    /// Files dropped on the draft row.
    pub(crate) fn drop_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let [path] = paths else {
            self.feedback = Some(self.i18n.text("upload_one_file"));
            cx.notify();
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let result = read_limited(path)
            .map(|bytes| Attachment::image(name.as_deref(), bytes, AttachmentSource::Drop));
        match result {
            Ok(attachment) => self.accept_attachment(attachment, window, cx),
            Err(error) => {
                self.feedback = Some(self.i18n.format("upload_read_failed", &[("error", &error)]));
                cx.notify();
            }
        }
    }

    fn accept_attachment(
        &mut self,
        attachment: Result<Attachment, AttachmentError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let attachment = match attachment {
            Ok(attachment) => attachment,
            Err(AttachmentError::NotAnImage) => {
                self.feedback = Some(self.i18n.text("upload_not_image"));
                cx.notify();
                return;
            }
            Err(AttachmentError::TooLarge { limit }) => {
                self.feedback = Some(
                    self.i18n
                        .format("upload_failed_too_large", &[("limit", &format_size(limit))]),
                );
                cx.notify();
                return;
            }
        };
        let (readiness, uploader) = match self.uploader(cx) {
            Ok(found) => found,
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
                return;
            }
        };
        if let Some(uploader) = &uploader
            && attachment.bytes.len() > uploader.provider().max_bytes
        {
            let limit = format_size(uploader.provider().max_bytes);
            self.feedback = Some(
                self.i18n
                    .format("upload_failed_too_large", &[("limit", &limit)]),
            );
            cx.notify();
            return;
        }
        let target = self.state.selection();
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
            Offer::Configure => self.prompt_upload_setup(
                self.i18n.text("upload_not_configured_title"),
                self.i18n.text("upload_not_configured_detail"),
                self.i18n.text("upload_configure"),
                window,
                cx,
            ),
            Offer::Reconnect { provider } => self.prompt_upload_setup(
                self.i18n
                    .format("upload_reconnect_title", &[("provider", &provider)]),
                self.i18n
                    .format("upload_reconnect_detail", &[("provider", &provider)]),
                self.i18n.text("upload_reconnect"),
                window,
                cx,
            ),
            Offer::Busy => {
                self.feedback = Some(self.i18n.text("upload_busy"));
                cx.notify();
            }
        }
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
                    this.open_settings_tab(SettingsTab::ImageUpload, window, cx)
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
        let upload = cx.background_spawn(async move {
            uploader
                .upload(&job.attachment)
                .map(|image| image.url)
                .map_err(|error| match error {
                    UploadError::Authentication => UploadFailure::Authentication,
                    UploadError::Rejected(reason) => UploadFailure::Rejected(reason),
                    UploadError::Network(reason) => UploadFailure::Network(reason),
                })
        });
        let id = job.id;
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
                self.feedback = Some(self.i18n.text("upload_done"));
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
                let (key, error) = match failure {
                    UploadFailure::Network(error) => ("upload_failed_network", error),
                    UploadFailure::Rejected(error) => ("upload_failed_rejected", error),
                    UploadFailure::Authentication => unreachable!(),
                };
                self.feedback = Some(
                    self.i18n
                        .format(key, &[("provider", &provider), ("error", &error)]),
                );
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
    let file = std::fs::File::open(path).map_err(|error| error.kind().to_string())?;
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
        CredentialBackendKind, CredentialStore, Secret, SecretKey, Settings,
        credentials::MemoryBackend,
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
        let mut settings = Settings {
            channels: "#a".into(),
            language: cayenchat_storage::Language::English,
            ..Settings::default()
        };
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
        assert_eq!(draft(&input, cx), format!("look: {URL} "));
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
        let feedback = chat.read_with(cx, |chat, _| chat.feedback.clone()).unwrap();
        assert!(feedback.contains("offline"), "{feedback}");
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
            chat.drop_paths(std::slice::from_ref(&text), window, cx)
        });
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt(), "non-images are refused");
        chat.update_in(cx, |chat, window, cx| {
            chat.drop_paths(std::slice::from_ref(&image), window, cx)
        });
        cx.run_until_parked();
        let (_, detail) = cx.pending_prompt().unwrap();
        assert!(detail.contains("dropped.png"), "{detail}");
        cx.simulate_prompt_answer("Upload");
        cx.run_until_parked();
        assert_eq!(fake.uploaded_names(), ["dropped.png"]);
        assert_eq!(draft(&input, cx), format!("{URL} "));
    }
}
