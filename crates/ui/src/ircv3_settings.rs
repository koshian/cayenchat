//! Settings tab for opt-in IRCv3 features, chosen per server.
//!
//! This tab is the current home of IRCv3 preferences that need an explicit
//! opt-in. Each feature is one [`Ircv3Feature`] row reading and writing its
//! own field of [`Ircv3Preferences`]; moving a feature to another tab, or
//! enabling it by default, changes only its row or storage default and never
//! the protocol code.

use std::{path::PathBuf, sync::Arc};

use cayenchat_app::{
    ConnectionStatus,
    attachments::{Completion, Offer, UploadFailure, UploaderReadiness},
    own_avatar::{Action, Blocked, Confirmed, Failure, Outcome, OwnAvatar},
};
use cayenchat_irc_core::{AvatarRequestFailure, MAX_PUBLISHED_AVATAR_BYTES, publishable_avatar};
use cayenchat_media::policy::{PublishProblem, publishable_avatar_url};
use cayenchat_model::NetworkId;
use cayenchat_model::attachment::{Attachment, AttachmentError, AttachmentSource};
use cayenchat_storage::{Ircv3Preferences, ServerProfile, Settings, TextEncoding};
use cayenchat_upload::ExternalUploader;
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, SettingsWindow,
    account_settings::panel,
    image_upload::{
        acceptable_attachment, clipboard_attachment, configured_uploader, file_attachment,
        upload_failure_text, upload_in_background,
    },
    input, settings_theme,
};

/// Server descriptions shown with a rejection are cut to this many
/// characters.
const MAX_SHOWN_DESCRIPTION: usize = 200;

/// One opt-in preference as the settings window shows it.
pub(crate) struct Ircv3Feature {
    pub id: &'static str,
    pub label_key: &'static str,
    pub hint_key: &'static str,
    pub get: fn(&Ircv3Preferences) -> bool,
    pub toggle: fn(&mut Ircv3Preferences),
    /// A warning shown under the row while it is on but cannot work yet,
    /// such as a missing prerequisite. Prerequisites are never switched on
    /// implicitly.
    pub warning: fn(&Ircv3Preferences) -> Option<&'static str>,
}

/// Features shown on the IRCv3 tab, in display order.
pub(crate) const IRCV3_FEATURES: [Ircv3Feature; 4] = [
    Ircv3Feature {
        id: "ircv3-server-time",
        label_key: "ircv3_server_time",
        hint_key: "ircv3_server_time_hint",
        get: |preferences| preferences.server_time,
        toggle: |preferences| preferences.server_time = !preferences.server_time,
        warning: |_| None,
    },
    Ircv3Feature {
        id: "ircv3-message-tags",
        label_key: "ircv3_message_tags",
        hint_key: "ircv3_message_tags_hint",
        get: |preferences| preferences.message_tags,
        toggle: |preferences| preferences.message_tags = !preferences.message_tags,
        warning: |_| None,
    },
    Ircv3Feature {
        id: "ircv3-batch",
        label_key: "ircv3_batch",
        hint_key: "ircv3_batch_hint",
        get: |preferences| preferences.batch,
        toggle: |preferences| preferences.batch = !preferences.batch,
        warning: |_| None,
    },
    // draft/metadata-2 requires batch (the specification says so); the
    // dependency is shown, not resolved behind the user's back.
    Ircv3Feature {
        id: "ircv3-metadata",
        label_key: "ircv3_metadata",
        hint_key: "ircv3_metadata_hint",
        get: |preferences| preferences.metadata,
        toggle: |preferences| preferences.metadata = !preferences.metadata,
        warning: |preferences| {
            (preferences.metadata && !preferences.batch).then_some("ircv3_metadata_needs_batch")
        },
    },
];

impl SettingsWindow {
    pub(crate) fn render_ircv3_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let hint = |key: &str| {
            div()
                .ml(px(26.))
                .text_color(theme.text_secondary)
                .child(self.i18n.text(key))
        };
        let mut panel = panel(cx)
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("ircv3_tab")),
            )
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("ircv3_intro")),
            );
        let Some(profile) = self.settings.values.selected_profile().cloned() else {
            return panel.child(div().child(self.i18n.text("ircv3_no_server")));
        };

        // Which server is being configured, and a way to pick another.
        let mut servers = div().flex().flex_wrap().gap_1();
        for (index, server) in self.settings.values.ordered_servers().enumerate() {
            let id = server.id.clone();
            let selected = id == profile.id;
            servers = servers.child(
                div()
                    .id(("ircv3-server", index))
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.bg(theme.selected).font_weight(FontWeight::BOLD)
                    })
                    .when(!selected, |d| d.hover(|d| d.bg(theme.hover)))
                    .child(server_label(&server.host, server.port, &self.i18n))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_server(id.clone(), cx)),
                    ),
            );
        }
        panel = panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("ircv3_server")),
                    )
                    .child(servers),
            )
            .child(div().font_weight(FontWeight::BOLD).child(self.i18n.format(
                "ircv3_configuring",
                &[(
                    "server",
                    &server_label(&profile.host, profile.port, &self.i18n),
                )],
            )));

        for feature in &IRCV3_FEATURES {
            let toggle = feature.toggle;
            panel = panel
                .child(
                    div()
                        .id(feature.id)
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .child(settings_theme::checkbox(
                            (feature.get)(&profile.ircv3),
                            true,
                            cx,
                        ))
                        .child(self.i18n.text(feature.label_key))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(profile) = this.settings.values.selected_profile_mut() {
                                toggle(&mut profile.ircv3);
                                cx.notify();
                            }
                        })),
                )
                .child(hint(feature.hint_key))
                .when_some((feature.warning)(&profile.ircv3), |panel, key| {
                    panel.child(
                        div()
                            .ml(px(26.))
                            .text_color(theme.warning)
                            .child(self.i18n.text(key)),
                    )
                });
        }
        // Notes about the options above stay with them; our own avatar,
        // which is not an option, comes last.
        if profile.encoding != TextEncoding::Utf8 {
            let encoding = [("encoding", profile.encoding.label())];
            panel = panel.child(
                div()
                    .text_color(theme.warning)
                    .child(self.i18n.format("ircv3_legacy_encoding", &encoding)),
            );
            if profile.ircv3.metadata {
                panel = panel.child(
                    div()
                        .text_color(theme.warning)
                        .child(self.i18n.format("ircv3_metadata_legacy", &encoding)),
                );
            }
        }
        panel
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("ircv3_next_connection")),
            )
            .child(self.render_own_avatar(&profile, cx))
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
    }
}

impl SettingsWindow {
    /// Our own avatar on the selected server, without explanations: the
    /// URL field, "Choose Image…" (and dropping) when an image host is set
    /// up, and "Send to IRC Server" / "Remove from IRC Server" when they
    /// would change something on the connected server.
    fn render_own_avatar(&mut self, profile: &ServerProfile, cx: &mut Context<Self>) -> Div {
        // Experimental and behind the server's metadata opt-in.
        if !profile.ircv3.metadata {
            return div();
        }
        let theme = settings_theme::palette(cx);
        let status = self
            .owner
            .read(cx)
            .map(|chat| chat.own_avatar_status(&profile.id))
            .unwrap_or_default();
        let draft = self.settings.avatar_url.read(cx).text().trim().to_owned();
        let sent = match &status.confirmed {
            Confirmed::Set(url) => Some(url.as_str()),
            _ => None,
        };
        let can_send = status.can_request && !draft.is_empty() && sent != Some(draft.as_str());
        let can_remove = status.can_request && sent.is_some();
        // An uploaded image is sent at once, so images are only taken while
        // this server can receive an avatar.
        let can_upload = status.can_request;
        let provider = self
            .settings
            .values
            .image_upload
            .provider
            .as_deref()
            .and_then(cayenchat_upload::provider)
            .map(|provider| provider.name);
        let uploading = self.avatar_upload.uploading().map(str::to_owned);
        let button = |id: &'static str, key: &str, primary: bool| {
            settings_theme::button(id, primary, cx).child(self.i18n.text(key))
        };
        let buttons = div()
            .ml(px(158.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .when(
                can_upload && provider.is_some() && uploading.is_none(),
                |row| {
                    row.child(
                        button("ircv3-avatar-choose", "ircv3_avatar_choose", false).on_click(
                            cx.listener(|this, _, window, cx| this.choose_avatar_image(window, cx)),
                        ),
                    )
                },
            )
            .when_some(uploading, |row, provider| {
                row.child(
                    self.i18n
                        .format("ircv3_avatar_uploading", &[("provider", &provider)]),
                )
                .child(
                    button("ircv3-avatar-upload-cancel", "cancel", false)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_avatar_upload(cx))),
                )
            })
            .when(can_send, |row| {
                // Sending exposes the URL to the whole network.
                let warning: SharedString = self.i18n.text("ircv3_avatar_exposure").into();
                row.child(
                    button("ircv3-avatar-publish", "ircv3_avatar_publish", true)
                        .tooltip(move |_, cx| {
                            let text = warning.clone();
                            cx.new(|_| TextTooltip(text)).into()
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_own_avatar(Action::Publish, cx)
                        })),
                )
            })
            // Removal is at the far end, in the warning color, and asks
            // first.
            .when(can_remove, |row| {
                row.child(div().flex_1()).child(
                    button("ircv3-avatar-remove", "ircv3_avatar_remove", false)
                        .text_color(theme.warning)
                        .border_color(theme.warning)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_avatar_removal(window, cx)
                        })),
                )
            });
        // Only what needs attention: a request in progress or a failure.
        let outcome = status
            .outcome
            .as_ref()
            .filter(|outcome| !matches!(outcome, Outcome::Published(_) | Outcome::Removed))
            .map(|outcome| outcome_text(&self.i18n, outcome));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .pt_2()
            .on_action(cx.listener(Self::paste_avatar_image))
            .when(can_upload && provider.is_some(), |section| {
                section
                    .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(theme.selected))
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                        this.drop_avatar_image(paths.paths(), window, cx)
                    }))
            })
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("ircv3_avatar_heading")),
            )
            .child(crate::settings_field(
                &self.i18n.text("ircv3_avatar_url"),
                self.settings.avatar_url.clone(),
            ))
            .child(buttons)
            .when(status.unsupported, |section| {
                section.child(
                    div()
                        .ml(px(158.))
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("ircv3_avatar_unsupported")),
                )
            })
            .when_some(outcome, |section, (text, failed)| {
                section.child(
                    div()
                        .ml(px(158.))
                        .when(failed, |d| d.text_color(theme.warning))
                        .child(text),
                )
            })
            .when_some(self.avatar_feedback.clone(), |section, feedback| {
                section.child(div().ml(px(158.)).text_color(theme.warning).child(feedback))
            })
            .when(self.avatar_opening, |section| {
                section.child(
                    div()
                        .ml(px(158.))
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("ircv3_avatar_edit_loading")),
                )
            })
            .children(provider.and_then(|name| self.render_avatar_editor(name, cx)))
    }

    fn confirm_avatar_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &self.i18n.text("ircv3_avatar_remove_title"),
            Some(&self.i18n.text("ircv3_avatar_remove_detail")),
            &[
                PromptButton::ok(self.i18n.text("ircv3_avatar_remove_confirm")),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| this.request_own_avatar(Action::Remove, cx));
            }
        })
        .detach();
    }

    /// Publish or Remove, only when clicked. The draft is checked here;
    /// success is shown only once the server answers.
    pub(crate) fn request_own_avatar(&mut self, action: Action, cx: &mut Context<Self>) {
        self.avatar_feedback = None;
        let Some(profile) = self.settings.values.selected_profile().cloned() else {
            return;
        };
        let url = match action {
            Action::Remove => None,
            Action::Publish => {
                let text = self.settings.avatar_url.read(cx).text().trim().to_owned();
                let utf8 = profile.encoding == TextEncoding::Utf8;
                if let Some(key) = avatar_url_problem(&text, utf8) {
                    let max = MAX_PUBLISHED_AVATAR_BYTES.to_string();
                    self.avatar_feedback = Some(self.i18n.format(key, &[("max", &max)]));
                    cx.notify();
                    return;
                }
                Some(text)
            }
        };
        let result = self
            .owner
            .update(cx, |owner, _, _| owner.request_own_avatar(&profile.id, url));
        self.avatar_feedback = match result {
            Ok(Ok(())) => None,
            Ok(Err(key)) => Some(self.i18n.text(key)),
            Err(error) => Some(
                self.i18n
                    .format("chat_closed", &[("error", &error.to_string())]),
            ),
        };
        cx.notify();
    }
}

/// Avatar images: uploaded through the configured image host into the
/// selected server's avatar URL draft, with the same confirmation and
/// attachment flow as chat drafts. Uploading never publishes.
impl SettingsWindow {
    /// Whether an avatar image can be taken now: the selected server can
    /// receive the avatar it would be sent as.
    fn avatar_images_accepted(&self, cx: &App) -> bool {
        let Some(profile) = self.settings.values.selected_profile() else {
            return false;
        };
        profile.ircv3.metadata
            && self
                .owner
                .read(cx)
                .is_ok_and(|chat| chat.own_avatar_status(&profile.id).can_request)
    }

    fn paste_avatar_image(
        &mut self,
        _: &input::Paste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Otherwise an image paste in the field does nothing, like in any
        // other text field.
        if !self.avatar_images_accepted(cx) {
            return;
        }
        if let Some(attachment) = clipboard_attachment(cx) {
            self.offer_avatar_image(attachment, window, cx);
        }
    }

    fn drop_avatar_image(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.avatar_images_accepted(cx) {
            return;
        }
        match file_attachment(paths, AttachmentSource::Drop, &self.i18n) {
            Ok(attachment) => self.offer_avatar_image(attachment, window, cx),
            Err(error) => {
                self.avatar_feedback = Some(error);
                cx.notify();
            }
        }
    }

    fn choose_avatar_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.i18n.text("ircv3_avatar_choose_prompt").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                match file_attachment(&paths, AttachmentSource::Chooser, &this.i18n) {
                    Ok(attachment) => this.offer_avatar_image(attachment, window, cx),
                    Err(error) => {
                        this.avatar_feedback = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// An image for the avatar: checked, then opened in the square
    /// selection editor. Nothing leaves the computer yet.
    fn offer_avatar_image(
        &mut self,
        attachment: Result<Attachment, AttachmentError>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.avatar_feedback = None;
        let Some(profile) = self
            .settings
            .values
            .selected_profile()
            .map(|p| p.id.clone())
        else {
            return;
        };
        if self.avatar_upload.uploading().is_some() {
            self.avatar_feedback = Some(self.i18n.text("upload_busy"));
            cx.notify();
            return;
        }
        let attachment = match acceptable_attachment(attachment, None, &self.i18n) {
            Ok(attachment) => attachment,
            Err(error) => {
                self.avatar_feedback = Some(error);
                cx.notify();
                return;
            }
        };
        let provider = self.settings.values.image_upload.provider.clone();
        match configured_uploader(provider.as_deref(), None, &self.i18n, cx) {
            Ok((UploaderReadiness::Ready { .. }, Some(_))) => {
                self.open_avatar_editor(profile, attachment.bytes.clone(), attachment.source, cx);
            }
            Ok((UploaderReadiness::NeedsAccount { provider }, _)) => {
                self.avatar_feedback = Some(
                    self.i18n
                        .format("ircv3_avatar_upload_reconnect", &[("provider", &provider)]),
                );
            }
            Ok(_) => {
                self.avatar_feedback = Some(self.i18n.text("ircv3_avatar_upload_needs_setup"));
            }
            Err(error) => self.avatar_feedback = Some(error),
        }
        cx.notify();
    }

    /// The editor's upload button: encode the selected square off the UI
    /// thread, then upload it (the button was the confirmation).
    pub(crate) fn upload_edited_avatar(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.avatar_editor.take() else {
            return;
        };
        let (source, crop) = (editor.source(), editor.crop());
        let (profile, kind) = (editor.profile, editor.source_kind);
        self.avatar_opening = true;
        cx.notify();
        let encoding = cx.background_spawn(async move { source.encode(crop) });
        cx.spawn(async move |this, cx| {
            let encoded = encoding.await;
            let _ = this.update(cx, |this, cx| {
                this.avatar_opening = false;
                match encoded {
                    Ok(encoded) => {
                        let name = format!("avatar.{}", encoded.extension);
                        let attachment = Attachment::image(Some(&name), encoded.bytes, kind);
                        this.upload_avatar_attachment(profile, attachment, cx);
                    }
                    Err(error) => {
                        this.avatar_feedback =
                            Some(this.i18n.text(crate::avatar_editor::open_error_key(&error)));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn upload_avatar_attachment(
        &mut self,
        profile: String,
        attachment: Result<Attachment, AttachmentError>,
        cx: &mut Context<Self>,
    ) {
        let provider = self.settings.values.image_upload.provider.clone();
        let checked = acceptable_attachment(attachment, None, &self.i18n).and_then(|attachment| {
            let (readiness, uploader) =
                configured_uploader(provider.as_deref(), None, &self.i18n, cx)?;
            let attachment = acceptable_attachment(Ok(attachment), uploader.as_ref(), &self.i18n)?;
            Ok((attachment, readiness, uploader))
        });
        let (attachment, readiness, uploader) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                self.avatar_feedback = Some(error);
                return;
            }
        };
        match self.avatar_upload.offer(attachment, profile, readiness) {
            // The editor's upload button, which names the host, was the
            // confirmation.
            Offer::Confirm { .. } => match uploader {
                Some(uploader) => self.start_avatar_upload(uploader, cx),
                None => self.avatar_upload.decline(),
            },
            Offer::Configure => {
                self.avatar_feedback = Some(self.i18n.text("ircv3_avatar_upload_needs_setup"));
            }
            Offer::Reconnect { provider } => {
                self.avatar_feedback = Some(
                    self.i18n
                        .format("ircv3_avatar_upload_reconnect", &[("provider", &provider)]),
                );
            }
            Offer::Busy => self.avatar_feedback = Some(self.i18n.text("upload_busy")),
        }
    }

    fn start_avatar_upload(&mut self, uploader: Arc<dyn ExternalUploader>, cx: &mut Context<Self>) {
        let Some(job) = self.avatar_upload.confirm() else {
            return;
        };
        let id = job.id;
        let upload = upload_in_background(uploader, job, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = upload.await;
            let _ = this.update(cx, |this, cx| {
                let completion = this.avatar_upload.finish(id, result);
                this.complete_avatar_upload(completion, cx);
            });
        })
        .detach();
    }

    fn complete_avatar_upload(&mut self, completion: Completion<String>, cx: &mut Context<Self>) {
        match completion {
            Completion::InsertLink { target, url } => {
                match place_uploaded_avatar(&mut self.settings.values, &target, &url) {
                    Ok(shown) => {
                        if shown {
                            self.settings
                                .avatar_url
                                .update(cx, |field, cx| field.set_text(&url, cx));
                        }
                        // Uploading an image means "use it": send it to
                        // the server now, when it is connected.
                        let sent = self.owner.update(cx, |owner, _, _| {
                            owner.request_own_avatar(&target, Some(url))
                        });
                        self.avatar_feedback = match sent {
                            Ok(Ok(())) => None,
                            Ok(Err("ircv3_avatar_unavailable")) => {
                                Some(self.i18n.text("ircv3_avatar_uploaded_not_sent"))
                            }
                            Ok(Err(key)) => Some(self.i18n.text(key)),
                            Err(error) => Some(
                                self.i18n
                                    .format("chat_closed", &[("error", &error.to_string())]),
                            ),
                        };
                    }
                    Err(key) => self.avatar_feedback = Some(self.i18n.text(key)),
                }
            }
            Completion::Failed {
                provider,
                failure: UploadFailure::Authentication,
            } => {
                self.avatar_feedback = Some(
                    self.i18n
                        .format("ircv3_avatar_upload_reconnect", &[("provider", &provider)]),
                );
            }
            Completion::Failed { provider, failure } => {
                self.avatar_feedback = Some(upload_failure_text(&provider, failure, &self.i18n));
            }
            Completion::Ignored => {}
        }
        cx.notify();
    }

    fn cancel_avatar_upload(&mut self, cx: &mut Context<Self>) {
        if self.avatar_upload.cancel_upload() {
            self.avatar_feedback = Some(self.i18n.text("ircv3_avatar_upload_cancelled"));
        }
        cx.notify();
    }
}

/// Puts an uploaded image's URL into the avatar URL draft of `profile_id`
/// (never publishing it). Returns whether that server is the one shown, so
/// its field must show the URL too; `Err` is a localization key.
pub(crate) fn place_uploaded_avatar(
    values: &mut Settings,
    profile_id: &str,
    url: &str,
) -> Result<bool, &'static str> {
    if publishable_avatar_url(url).is_err() || url.len() > MAX_PUBLISHED_AVATAR_BYTES {
        return Err("ircv3_avatar_upload_bad_url");
    }
    let shown = values.selected_server == profile_id;
    let profile = values
        .servers
        .iter_mut()
        .find(|profile| profile.id == profile_id)
        .ok_or("ircv3_avatar_upload_server_gone")?;
    profile.avatar_url = url.to_owned();
    Ok(shown)
}

/// A plain text tooltip in the platform's tooltip style.
struct TextTooltip(SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Offset from the pointer like platform tooltips.
        div()
            .pl(px(10.))
            .pt(px(18.))
            .child(settings_theme::tooltip(self.0.clone(), cx))
    }
}

/// What the IRCv3 tab shows about our own avatar on one server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OwnAvatarStatus {
    pub can_request: bool,
    /// Registered after asking for avatar metadata, which the server did
    /// not enable.
    pub unsupported: bool,
    pub waiting: bool,
    pub confirmed: Confirmed,
    pub outcome: Option<Outcome>,
}

impl ChatWindow {
    /// Applies a change to a server's own-avatar state. The caller redraws
    /// the settings window ([`ChatWindow::refresh_settings`]).
    pub(crate) fn update_own_avatar(
        &mut self,
        network: NetworkId,
        change: impl FnOnce(&mut OwnAvatar),
    ) {
        if let Some(session) = self.sessions.get_mut(&network) {
            change(&mut session.own_avatar);
        }
    }

    pub(crate) fn refresh_settings(&self, cx: &mut App) {
        if let Some(handle) = self.settings_window {
            let _ = handle.update(cx, |_, _, cx| cx.notify());
        }
    }

    pub(crate) fn own_avatar_status(&self, profile_id: &str) -> OwnAvatarStatus {
        let Some(network) = self.network_of_profile(profile_id) else {
            return OwnAvatarStatus::default();
        };
        let session = &self.sessions[&network];
        let connected = session.irc.is_some()
            && self.state.status(network) == Some(&ConnectionStatus::Registered);
        let own = &session.own_avatar;
        OwnAvatarStatus {
            can_request: connected && own.can_request(),
            unsupported: connected && session.metadata_requested && !own.ready(),
            waiting: own.waiting(),
            confirmed: own.confirmed().clone(),
            outcome: own.outcome().cloned(),
        }
    }

    /// Sends Publish (`Some`) or Remove (`None`) on the server's current
    /// connection. `Err` is a localization key for why nothing was sent.
    pub(crate) fn request_own_avatar(
        &mut self,
        profile_id: &str,
        url: Option<String>,
    ) -> Result<(), &'static str> {
        let network = self
            .network_of_profile(profile_id)
            .ok_or("ircv3_avatar_unavailable")?;
        let registered = self.state.status(network) == Some(&ConnectionStatus::Registered);
        let session = self
            .sessions
            .get_mut(&network)
            .ok_or("ircv3_avatar_unavailable")?;
        let Some(connection) = session.irc.as_ref().filter(|_| registered) else {
            return Err("ircv3_avatar_unavailable");
        };
        let action = if url.is_some() {
            Action::Publish
        } else {
            Action::Remove
        };
        let request = session
            .own_avatar
            .begin(action)
            .map_err(|blocked| match blocked {
                Blocked::NotReady => "ircv3_avatar_unavailable",
                Blocked::Busy => "ircv3_avatar_busy",
            })?;
        if let Err(error) = connection.set_own_avatar(request, url.as_deref()) {
            session.own_avatar.failed(request, Failure::NotSent(error));
        }
        Ok(())
    }
}

/// Why `text` cannot be published, as a localization key (which may use
/// `{max}`), or `None`.
pub(crate) fn avatar_url_problem(text: &str, utf8: bool) -> Option<&'static str> {
    if text.is_empty() {
        return Some("ircv3_avatar_empty");
    }
    if let Err(problem) = publishable_avatar_url(text) {
        return Some(match problem {
            PublishProblem::Invalid => "ircv3_avatar_invalid",
            PublishProblem::Credentials => "ircv3_avatar_credentials",
            PublishProblem::Blocked => "ircv3_avatar_blocked",
        });
    }
    if text.len() > MAX_PUBLISHED_AVATAR_BYTES {
        return Some("ircv3_avatar_too_long");
    }
    if !utf8 && !text.is_ascii() {
        return Some("ircv3_avatar_ascii_only");
    }
    publishable_avatar(text, utf8)
        .is_err()
        .then_some("ircv3_avatar_invalid")
}

pub(crate) fn own_avatar_failure(failure: AvatarRequestFailure) -> Failure {
    match failure {
        AvatarRequestFailure::Unavailable => Failure::Unavailable,
        AvatarRequestFailure::Busy => Failure::Busy,
        AvatarRequestFailure::Rejected { code, description } => Failure::Rejected {
            code,
            description: description.chars().take(MAX_SHOWN_DESCRIPTION).collect(),
        },
        AvatarRequestFailure::RateLimited { retry_after } => Failure::RateLimited { retry_after },
        AvatarRequestFailure::NoReply => Failure::NoReply,
        AvatarRequestFailure::CapabilityLost => Failure::CapabilityLost,
    }
}

/// The line describing `outcome`, and whether it is a failure.
fn outcome_text(i18n: &crate::localization::Localizer, outcome: &Outcome) -> (String, bool) {
    match outcome {
        Outcome::Waiting(Action::Publish) => (i18n.text("ircv3_avatar_waiting_publish"), false),
        Outcome::Waiting(Action::Remove) => (i18n.text("ircv3_avatar_waiting_remove"), false),
        Outcome::Published(url) => (
            i18n.format("ircv3_avatar_published", &[("url", url)]),
            false,
        ),
        Outcome::Removed => (i18n.text("ircv3_avatar_removed"), false),
        Outcome::Failed(_, failure) => {
            let text = match failure {
                Failure::Unavailable => i18n.text("ircv3_avatar_unavailable"),
                Failure::Busy => i18n.text("ircv3_avatar_busy"),
                Failure::Rejected { code, description } => i18n.format(
                    "ircv3_avatar_rejected",
                    &[("code", code), ("description", description)],
                ),
                Failure::RateLimited {
                    retry_after: Some(seconds),
                } => i18n.format(
                    "ircv3_avatar_rate_limited",
                    &[("seconds", &seconds.to_string())],
                ),
                Failure::RateLimited { retry_after: None } => {
                    i18n.text("ircv3_avatar_rate_limited_later")
                }
                Failure::NoReply => i18n.text("ircv3_avatar_no_reply"),
                Failure::CapabilityLost => i18n.text("ircv3_avatar_capability_lost"),
                Failure::ConnectionLost => i18n.text("ircv3_avatar_connection_lost"),
                Failure::NotSent(error) => {
                    i18n.format("ircv3_avatar_not_sent", &[("error", error)])
                }
            };
            (text, true)
        }
    }
}

fn server_label(host: &str, port: u16, i18n: &crate::localization::Localizer) -> String {
    if host.is_empty() {
        i18n.text("new_server")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::{IRCV3_FEATURES, avatar_url_problem, place_uploaded_avatar};
    use cayenchat_storage::{Ircv3Preferences, Settings};

    #[test]
    fn uploaded_images_fill_the_draft_of_the_server_they_were_for() {
        let mut settings = Settings::default();
        let first = settings.add_server("irc.one.example").id.clone();
        let second = settings.add_server("irc.two.example").id.clone();
        settings.selected_server = first.clone();
        let url = "https://i.ibb.co/abc/me.png";
        // The shown server: its field must show the URL too.
        assert_eq!(place_uploaded_avatar(&mut settings, &first, url), Ok(true));
        // The user switched servers while uploading: the draft of the
        // server the image was for changes, the shown one does not.
        assert_eq!(
            place_uploaded_avatar(&mut settings, &second, "https://i.ibb.co/x/b.png"),
            Ok(false)
        );
        assert_eq!(settings.servers[0].avatar_url, url);
        assert_eq!(settings.servers[1].avatar_url, "https://i.ibb.co/x/b.png");
        // Removed meanwhile, or a URL we would refuse to publish.
        assert_eq!(
            place_uploaded_avatar(&mut settings, "gone", url),
            Err("ircv3_avatar_upload_server_gone")
        );
        for bad in [
            "http://localhost/a.png".to_owned(),
            "https://i.ibb.co/a.png?token=x".to_owned(),
            format!("https://i.ibb.co/{}.png", "a".repeat(400)),
        ] {
            assert_eq!(
                place_uploaded_avatar(&mut settings, &first, &bad),
                Err("ircv3_avatar_upload_bad_url"),
                "{bad}"
            );
        }
        assert_eq!(settings.servers[0].avatar_url, url, "unchanged");
        // Only the draft changes: nothing about publishing is stored.
        assert!(settings.servers.iter().all(|server| !server.ircv3.metadata));
    }

    #[test]
    fn avatar_upload_texts_exist_in_both_languages() {
        let catalogs = [
            include_str!("../../../locales/en.json"),
            include_str!("../../../locales/ja.json"),
        ];
        for key in [
            "ircv3_avatar_choose",
            "ircv3_avatar_choose_prompt",
            "ircv3_avatar_edit_loading",
            "ircv3_avatar_edit_result",
            "ircv3_avatar_edit_reset",
            "ircv3_avatar_edit_upload",
            "ircv3_avatar_edit_unsupported",
            "ircv3_avatar_edit_too_large",
            "ircv3_avatar_edit_unreadable",
            "ircv3_avatar_uploading",
            "ircv3_avatar_uploaded_not_sent",
            "ircv3_avatar_upload_bad_url",
            "ircv3_avatar_upload_server_gone",
            "ircv3_avatar_upload_needs_setup",
            "ircv3_avatar_upload_reconnect",
            "ircv3_avatar_upload_cancelled",
            "upload_busy",
            "ircv3_avatar_exposure",
            "ircv3_avatar_unsupported",
            "ircv3_avatar_remove_title",
            "ircv3_avatar_remove_detail",
            "ircv3_avatar_remove_confirm",
        ] {
            for catalog in catalogs {
                assert!(catalog.contains(&format!("\"{key}\"")), "{key}");
            }
        }
    }

    #[test]
    fn drafts_are_checked_before_anything_is_sent() {
        assert_eq!(avatar_url_problem("https://example.com/me.png", true), None);
        assert_eq!(
            avatar_url_problem("https://example.com/{size}/me", true),
            None
        );
        for (url, key) in [
            ("", "ircv3_avatar_empty"),
            ("example.com/me.png", "ircv3_avatar_invalid"),
            ("https://example.com/a\r\nQUIT", "ircv3_avatar_invalid"),
            (
                "https://me:pw@example.com/a.png",
                "ircv3_avatar_credentials",
            ),
            (
                "https://example.com/a.png?access_token=x",
                "ircv3_avatar_credentials",
            ),
            ("https://10.0.0.1/a.png", "ircv3_avatar_blocked"),
            ("https://例え.jp/a.png", "ircv3_avatar_ascii_only"),
        ] {
            let utf8 = !url.contains("例え");
            assert_eq!(avatar_url_problem(url, utf8), Some(key), "{url}");
        }
        let long = format!("https://example.com/{}", "a".repeat(400));
        assert_eq!(
            avatar_url_problem(&long, true),
            Some("ircv3_avatar_too_long")
        );
        let catalogs = [
            include_str!("../../../locales/en.json"),
            include_str!("../../../locales/ja.json"),
        ];
        for key in [
            "ircv3_avatar_empty",
            "ircv3_avatar_invalid",
            "ircv3_avatar_credentials",
            "ircv3_avatar_blocked",
            "ircv3_avatar_too_long",
            "ircv3_avatar_ascii_only",
        ] {
            for catalog in catalogs {
                assert!(catalog.contains(&format!("\"{key}\"")), "{key}");
            }
        }
    }

    #[test]
    fn each_feature_row_toggles_only_its_own_preference() {
        let catalogs = [
            include_str!("../../../locales/en.json"),
            include_str!("../../../locales/ja.json"),
        ];
        for (index, feature) in IRCV3_FEATURES.iter().enumerate() {
            let mut preferences = Ircv3Preferences::default();
            assert!(!(feature.get)(&preferences), "off by default");
            (feature.toggle)(&mut preferences);
            assert!((feature.get)(&preferences));
            for (other_index, other) in IRCV3_FEATURES.iter().enumerate() {
                if other_index != index {
                    assert!(!(other.get)(&preferences), "{} is independent", other.id);
                }
            }
            for catalog in catalogs {
                let warning = (feature.warning)(&preferences);
                for key in [feature.label_key, feature.hint_key]
                    .into_iter()
                    .chain(warning)
                {
                    assert!(catalog.contains(&format!("\"{key}\"")), "{key}");
                }
            }
        }
    }

    #[test]
    fn metadata_shows_its_batch_dependency_without_enabling_batch() {
        let metadata = IRCV3_FEATURES
            .iter()
            .find(|feature| feature.id == "ircv3-metadata")
            .unwrap();
        let mut preferences = Ircv3Preferences::default();
        assert_eq!((metadata.warning)(&preferences), None, "off: no warning");
        (metadata.toggle)(&mut preferences);
        assert!(preferences.metadata && !preferences.batch);
        assert_eq!(
            (metadata.warning)(&preferences),
            Some("ircv3_metadata_needs_batch")
        );
        preferences.batch = true;
        assert_eq!((metadata.warning)(&preferences), None);
        // Turning metadata off leaves batch as the user set it.
        (metadata.toggle)(&mut preferences);
        assert!(!preferences.metadata && preferences.batch);
    }
}

#[cfg(test)]
mod own_avatar_tests {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };

    use cayenchat_app::own_avatar::{Confirmed, Outcome};
    use cayenchat_irc_core::{Connection, ConnectionConfig, Event, Ircv3Options};
    use cayenchat_model::NetworkId;
    use gpui::{Entity, TestAppContext, VisualTestContext};

    use crate::ChatWindow;

    const URL: &str = "https://example.com/me.png";

    fn read_line(reader: &mut BufReader<std::net::TcpStream>) -> String {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if !line.starts_with("PING ") {
                break line.trim_end().to_owned();
            }
        }
    }

    /// Feeds the real connection's events to the window until `until`
    /// has been seen, as the window's event pump would.
    fn pump(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext, until: impl Fn(&Event) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "event did not arrive");
            let event = chat.update(cx, |chat, _| {
                chat.sessions
                    .get_mut(&NetworkId(1))
                    .and_then(|session| session.irc.as_mut())
                    .and_then(Connection::try_recv)
            });
            let Some(event) = event else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            let done = until(&event);
            chat.update(cx, |chat, cx| {
                chat.handle_events(NetworkId(1), vec![event], false, cx);
            });
            if done {
                break;
            }
        }
    }

    fn status(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) -> super::OwnAvatarStatus {
        chat.read_with(cx, |chat, _| {
            let id = chat.saved.servers[0].id.clone();
            chat.own_avatar_status(&id)
        })
    }

    #[gpui::test]
    fn servers_that_never_enable_avatars_are_reported_as_unsupported(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("");
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        // A connection object only; nothing is pumped from it.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec![]);
        config.port = port;
        config.use_tls = false;
        let connection = Connection::connect(config).unwrap();
        let registered = || Event::Registered {
            nickname: "me".into(),
        };
        let unsupported =
            |chat: &Entity<ChatWindow>, cx: &mut VisualTestContext| status(chat, cx).unsupported;
        chat.update(cx, |chat, cx| {
            let session = chat.sessions.get_mut(&NetworkId(1)).unwrap();
            session.irc = Some(connection);
            // Asked for avatars, registered, never enabled.
            session.metadata_requested = true;
            chat.handle_events(NetworkId(1), vec![registered()], false, cx);
        });
        assert!(unsupported(&chat, cx));
        // Enabled: supported.
        chat.update(cx, |chat, cx| {
            chat.handle_events(NetworkId(1), vec![Event::MetadataReady], false, cx);
        });
        let ready = status(&chat, cx);
        assert!(!ready.unsupported && ready.can_request);
        // Not asked for on this connection (options change next time):
        // nothing is claimed about the server.
        chat.update(cx, |chat, cx| {
            let session = chat.sessions.get_mut(&NetworkId(1)).unwrap();
            session.own_avatar.connection_ended();
            session.metadata_requested = false;
            chat.handle_events(NetworkId(1), vec![registered()], false, cx);
        });
        assert!(!unsupported(&chat, cx));
    }

    #[gpui::test]
    fn publish_and_remove_are_explicit_and_wait_for_the_server(cx: &mut TestAppContext) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut send = |text: &str| socket.write_all(text.as_bytes()).unwrap();
            assert_eq!(read_line(&mut lines), "CAP LS 302");
            send(":srv CAP * LS :batch draft/metadata-2\r\n");
            let _: Vec<String> = (0..3).map(|_| read_line(&mut lines)).collect();
            send(":srv CAP * ACK :batch\r\n");
            assert_eq!(read_line(&mut lines), "CAP REQ draft/metadata-2");
            send(":srv CAP * ACK :draft/metadata-2\r\n");
            assert_eq!(read_line(&mut lines), "CAP END");
            send(":srv 001 me :Welcome\r\n:srv 376 me :End\r\n");
            assert_eq!(read_line(&mut lines), "METADATA * SUB avatar");
            assert_eq!(read_line(&mut lines), "METADATA * GET avatar");
            send(":srv 766 me me avatar :not set\r\n");
            // Saving the draft and turning display off send nothing: the
            // next lines are the explicit Publish and Remove.
            assert_eq!(
                read_line(&mut lines),
                format!("METADATA * SET avatar {URL}")
            );
            send(&format!(":srv 761 me me avatar * :{URL}\r\n"));
            assert_eq!(read_line(&mut lines), "METADATA * SET avatar");
            send(":srv 766 me me avatar :Key deleted\r\n");
            assert!(read_line(&mut lines).starts_with("QUIT"));
        });

        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("");
        settings.appearance.user_avatars = true;
        settings.servers[0].nickname = "me".into();
        settings.servers[0].ircv3.batch = true;
        settings.servers[0].ircv3.metadata = true;
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let id = settings.servers[0].id.clone();
        // Nothing to send on before a connection exists.
        chat.update(cx, |chat, _| {
            assert_eq!(
                chat.request_own_avatar(&id, Some(URL.into())),
                Err("ircv3_avatar_unavailable")
            );
        });

        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec![]);
        config.port = port;
        config.use_tls = false;
        config.ircv3 = Ircv3Options {
            batch: true,
            metadata: true,
            ..Ircv3Options::default()
        };
        let connection = Connection::connect(config).unwrap();
        chat.update(cx, |chat, _| {
            chat.sessions.get_mut(&NetworkId(1)).unwrap().irc = Some(connection);
        });
        pump(&chat, cx, |event| {
            matches!(event, Event::OwnAvatar { request: None, .. })
        });
        let ready = status(&chat, cx);
        assert!(ready.can_request, "{ready:?}");
        assert_eq!(ready.confirmed, Confirmed::NotSet);

        // Editing and saving the draft (autosave) publishes nothing.
        chat.update(cx, |chat, cx| {
            let mut next = chat.saved.clone();
            next.servers[0].avatar_url = URL.into();
            chat.apply_servers(next, cx);
        });
        assert_eq!(status(&chat, cx).outcome, None);

        chat.update(cx, |chat, _| {
            assert_eq!(chat.request_own_avatar(&id, Some(URL.into())), Ok(()));
            // Repeated clicks do not queue more.
            assert_eq!(
                chat.request_own_avatar(&id, Some(URL.into())),
                Err("ircv3_avatar_busy")
            );
        });
        let waiting = status(&chat, cx);
        assert_eq!(
            waiting.outcome,
            Some(Outcome::Waiting(cayenchat_app::own_avatar::Action::Publish)),
            "queued is not published"
        );
        assert_eq!(waiting.confirmed, Confirmed::NotSet);
        pump(&chat, cx, |event| {
            matches!(
                event,
                Event::OwnAvatar {
                    request: Some(_),
                    ..
                }
            )
        });
        let published = status(&chat, cx);
        assert_eq!(published.outcome, Some(Outcome::Published(URL.into())));
        assert_eq!(published.confirmed, Confirmed::Set(URL.into()));

        // Hiding avatars does not remove the published one.
        chat.update(cx, |chat, cx| {
            let mut appearance = chat.appearance.clone();
            appearance.user_avatars = false;
            let mode = chat.theme_mode;
            chat.apply_appearance(appearance, mode, cx);
        });
        assert_eq!(status(&chat, cx).confirmed, Confirmed::Set(URL.into()));

        chat.update(cx, |chat, _| {
            assert_eq!(chat.request_own_avatar(&id, None), Ok(()));
        });
        pump(&chat, cx, |event| {
            matches!(
                event,
                Event::OwnAvatar {
                    request: Some(_),
                    ..
                }
            )
        });
        let removed = status(&chat, cx);
        assert_eq!(removed.outcome, Some(Outcome::Removed));
        assert_eq!(removed.confirmed, Confirmed::NotSet);

        // Disconnecting ends what the server confirmed; nothing waits to be
        // republished.
        chat.update(cx, |chat, _| {
            chat.sessions[&NetworkId(1)]
                .irc
                .as_ref()
                .unwrap()
                .disconnect()
                .unwrap();
        });
        pump(&chat, cx, |event| matches!(event, Event::Disconnected(_)));
        let offline = status(&chat, cx);
        assert!(!offline.can_request && !offline.waiting);
        assert_eq!(offline.confirmed, Confirmed::Unknown);
        server.join().unwrap();
    }
}
