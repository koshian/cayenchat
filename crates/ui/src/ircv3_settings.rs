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
    input,
    session::PeerAvatarConnection,
    settings_theme,
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
pub(crate) const IRCV3_FEATURES: [Ircv3Feature; 7] = [
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
    Ircv3Feature {
        id: "ircv3-chathistory",
        label_key: "ircv3_chathistory",
        hint_key: "ircv3_chathistory_hint",
        get: |preferences| preferences.chathistory,
        toggle: |preferences| preferences.chathistory = !preferences.chathistory,
        warning: |_| None,
    },
    Ircv3Feature {
        id: "ircv3-confirmed-sending",
        label_key: "ircv3_confirmed_sending",
        hint_key: "ircv3_confirmed_sending_hint",
        get: |preferences| preferences.confirmed_sending,
        toggle: |preferences| preferences.confirmed_sending = !preferences.confirmed_sending,
        warning: |_| None,
    },
    Ircv3Feature {
        id: "ircv3-accounts",
        label_key: "ircv3_accounts",
        hint_key: "ircv3_accounts_hint",
        get: |preferences| preferences.accounts,
        toggle: |preferences| preferences.accounts = !preferences.accounts,
        warning: |_| None,
    },
    Ircv3Feature {
        id: "ircv3-peer-avatars",
        label_key: "ircv3_peer_avatars",
        hint_key: "ircv3_peer_avatars_hint",
        get: |preferences| preferences.peer_avatars,
        toggle: |preferences| preferences.peer_avatars = !preferences.peer_avatars,
        warning: |_| None,
    },
];

/// Toggles one feature of `profile`. Turning peer avatars off also stops
/// sharing, so turning them on again shares nothing until the user chooses
/// to.
pub(crate) fn toggle_feature(profile: &mut ServerProfile, toggle: fn(&mut Ircv3Preferences)) {
    toggle(&mut profile.ircv3);
    if !profile.ircv3.peer_avatars {
        profile.peer_avatar_url.clear();
    }
}

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
                                toggle_feature(profile, toggle);
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
            .when(provider.is_some() && uploading.is_none(), |row| {
                row.child(
                    button("ircv3-avatar-choose", "ircv3_avatar_choose", false).on_click(
                        cx.listener(|this, _, window, cx| this.choose_avatar_image(window, cx)),
                    ),
                )
            })
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
            .when(provider.is_some(), |section| {
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
            .when(profile.ircv3.peer_avatars, |section| {
                section.child(self.render_peer_sharing(profile, &draft, &status, cx))
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

    /// Sharing with other clients through CTCP AVATAR, separate from the
    /// server: which URL is shared, "Share with Peers" when the draft
    /// differs from it, "Stop Sharing", and a note when the realname mark of
    /// the current connection no longer matches.
    fn render_peer_sharing(
        &self,
        profile: &ServerProfile,
        draft: &str,
        status: &OwnAvatarStatus,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = settings_theme::palette(cx);
        let shared = profile.peer_avatar_url.as_str();
        let can_share = !draft.is_empty() && draft != shared;
        let warning: SharedString = self.i18n.text("ircv3_peer_share_exposure").into();
        let reconnect = peer_reconnect_needed(status, profile);
        div()
            .ml(px(158.))
            .flex()
            .flex_col()
            .gap_2()
            .when(!shared.is_empty(), |section| {
                section.child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(self.i18n.format("ircv3_peer_sharing", &[("url", shared)])),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .when(can_share, |row| {
                        row.child(
                            settings_theme::button("ircv3-peer-share", false, cx)
                                .child(self.i18n.text("ircv3_peer_share"))
                                .tooltip(move |_, cx| {
                                    let text = warning.clone();
                                    cx.new(|_| TextTooltip(text)).into()
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.share_with_peers(cx))),
                        )
                    })
                    .when(!shared.is_empty(), |row| {
                        row.child(
                            settings_theme::button("ircv3-peer-stop", false, cx)
                                .child(self.i18n.text("ircv3_peer_stop"))
                                .on_click(cx.listener(|this, _, _, cx| this.stop_sharing(cx))),
                        )
                    }),
            )
            .when(reconnect, |section| {
                section.child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("ircv3_peer_reconnect")),
                )
            })
    }

    /// "Share with Peers": the draft, checked, becomes the shared URL. It is
    /// the only way a URL reaches other clients; editing the draft never
    /// does.
    fn share_with_peers(&mut self, cx: &mut Context<Self>) {
        self.avatar_feedback = None;
        let draft = self.settings.avatar_url.read(cx).text().trim().to_owned();
        let Some(profile) = self.settings.values.selected_profile_mut() else {
            return;
        };
        if let Err(key) = share_draft_with_peers(profile, &draft) {
            let max = MAX_PUBLISHED_AVATAR_BYTES.to_string();
            self.avatar_feedback = Some(self.i18n.format(key, &[("max", &max)]));
        }
        cx.notify();
    }

    fn stop_sharing(&mut self, cx: &mut Context<Self>) {
        self.avatar_feedback = None;
        if let Some(profile) = self.settings.values.selected_profile_mut() {
            profile.peer_avatar_url.clear();
        }
        cx.notify();
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
    /// Whether an avatar image can be taken now: any server can have one,
    /// connected, without avatar support or not.
    fn avatar_images_accepted(&self) -> bool {
        self.settings.values.selected_profile().is_some()
    }

    fn paste_avatar_image(
        &mut self,
        _: &input::Paste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Otherwise an image paste in the field does nothing, like in any
        // other text field.
        if !self.avatar_images_accepted() {
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
        if !self.avatar_images_accepted() {
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
    /// Registered on the server's current connection.
    pub connected: bool,
    /// How that connection exchanges avatars with other clients.
    pub peer: PeerAvatarConnection,
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
            connected,
            peer: session.peer_avatars.clone(),
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

/// Why `text` cannot be shared with other clients, as a localization key
/// (which may use `{max}`), or `None`: the rules for publishing, and no
/// `{size}`, which other clients would request literally.
pub(crate) fn peer_avatar_url_problem(text: &str, utf8: bool) -> Option<&'static str> {
    avatar_url_problem(text, utf8)
        .or_else(|| {
            text.contains(cayenchat_media::policy::AVATAR_SIZE_PLACEHOLDER)
                .then_some("ircv3_peer_size_placeholder")
        })
        // The connection refuses anything else, so it is never saved.
        .or_else(|| {
            cayenchat_irc_core::shareable_avatar(text, utf8)
                .is_err()
                .then_some("ircv3_avatar_invalid")
        })
}

/// Whether the connected server's realname mark (or peer exchange itself)
/// no longer matches the settings, so a reconnect is needed to apply them.
pub(crate) fn peer_reconnect_needed(status: &OwnAvatarStatus, profile: &ServerProfile) -> bool {
    status.connected
        && (status.peer.enabled != profile.ircv3.peer_avatars
            || status.peer.advertised != crate::shared_peer_avatar(profile).is_some())
}

/// Makes `draft` the URL `profile` shares with other clients. `Err` is a
/// localization key.
pub(crate) fn share_draft_with_peers(
    profile: &mut ServerProfile,
    draft: &str,
) -> Result<(), &'static str> {
    let utf8 = profile.encoding == TextEncoding::Utf8;
    if let Some(key) = peer_avatar_url_problem(draft, utf8) {
        return Err(key);
    }
    profile.peer_avatar_url = draft.to_owned();
    Ok(())
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
    use super::{
        IRCV3_FEATURES, OwnAvatarStatus, avatar_url_problem, peer_avatar_url_problem,
        peer_reconnect_needed, place_uploaded_avatar, share_draft_with_peers, toggle_feature,
    };
    use crate::session::PeerAvatarConnection;
    use cayenchat_storage::{Ircv3Preferences, Settings};

    #[test]
    fn peers_get_only_a_url_shared_explicitly_while_the_option_is_on() {
        let mut settings = Settings::default();
        let id = settings.add_server("irc.example.org").id.clone();
        let peer = IRCV3_FEATURES
            .iter()
            .find(|feature| feature.id == "ircv3-peer-avatars")
            .unwrap();
        let profile = settings.servers.iter_mut().find(|p| p.id == id).unwrap();
        // Off by default; a draft alone shares nothing.
        profile.avatar_url = "https://example.com/me.png".into();
        assert_eq!(crate::shared_peer_avatar(profile), None);
        toggle_feature(profile, peer.toggle);
        assert!(profile.ircv3.peer_avatars);
        assert_eq!(crate::shared_peer_avatar(profile), None, "not shared yet");
        // Share with Peers takes the draft; later edits and uploads of the
        // draft do not change what peers get.
        assert_eq!(
            share_draft_with_peers(profile, "https://example.com/me.png"),
            Ok(())
        );
        assert_eq!(
            crate::shared_peer_avatar(profile).as_deref(),
            Some("https://example.com/me.png")
        );
        profile.avatar_url = "https://example.com/other.png".into();
        settings.selected_server = id.clone();
        place_uploaded_avatar(&mut settings, &id, "https://i.ibb.co/x/new.png").unwrap();
        let profile = settings.servers.iter_mut().find(|p| p.id == id).unwrap();
        assert_eq!(profile.peer_avatar_url, "https://example.com/me.png");
        // Unacceptable drafts are refused and keep the shared URL.
        for (draft, key) in [
            ("", "ircv3_avatar_empty"),
            ("avatar.png", "ircv3_avatar_invalid"),
            (
                "https://example.com/{size}.png",
                "ircv3_peer_size_placeholder",
            ),
            (
                "https://me:pw@example.com/a.png",
                "ircv3_avatar_credentials",
            ),
            ("http://192.168.1.2/a.png", "ircv3_avatar_blocked"),
        ] {
            assert_eq!(share_draft_with_peers(profile, draft), Err(key), "{draft}");
        }
        assert_eq!(profile.peer_avatar_url, "https://example.com/me.png");
        // A saved value that is no longer acceptable is not shared.
        profile.peer_avatar_url = "https://localhost/a.png".into();
        assert_eq!(crate::shared_peer_avatar(profile), None);
        profile.peer_avatar_url = "https://example.com/me.png".into();
        // Turning the option off stops sharing for good: turning it on
        // again shares nothing until chosen again.
        toggle_feature(profile, peer.toggle);
        assert!(profile.peer_avatar_url.is_empty());
        toggle_feature(profile, peer.toggle);
        assert_eq!(crate::shared_peer_avatar(profile), None);
        // Other options leave sharing alone.
        share_draft_with_peers(profile, "https://example.com/me.png").unwrap();
        toggle_feature(profile, IRCV3_FEATURES[0].toggle);
        assert_eq!(profile.peer_avatar_url, "https://example.com/me.png");
        assert_eq!(
            peer_avatar_url_problem("https://example.com/me.png", true),
            None
        );
    }

    #[test]
    fn a_reconnect_is_asked_for_when_the_realname_mark_would_change() {
        let mut settings = Settings::default();
        let profile = settings.add_server("irc.example.org");
        let connected = |enabled: bool, advertised: bool| OwnAvatarStatus {
            connected: true,
            peer: PeerAvatarConnection {
                enabled,
                advertised,
                answering: None,
            },
            ..OwnAvatarStatus::default()
        };
        // Disconnected: nothing to reconnect.
        profile.ircv3.peer_avatars = true;
        assert!(!peer_reconnect_needed(&OwnAvatarStatus::default(), profile));
        // Connected without the option, then turned on.
        assert!(peer_reconnect_needed(&connected(false, false), profile));
        assert!(!peer_reconnect_needed(&connected(true, false), profile));
        // Shared after connecting: the mark is missing until reconnecting.
        profile.peer_avatar_url = "https://example.com/me.png".into();
        assert!(peer_reconnect_needed(&connected(true, false), profile));
        assert!(!peer_reconnect_needed(&connected(true, true), profile));
        // Stopped: the mark stays until reconnecting.
        profile.peer_avatar_url.clear();
        assert!(peer_reconnect_needed(&connected(true, true), profile));
    }

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
            "ircv3_peer_share",
            "ircv3_peer_share_exposure",
            "ircv3_peer_stop",
            "ircv3_peer_sharing",
            "ircv3_peer_reconnect",
            "ircv3_peer_size_placeholder",
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

    /// Queues `/msg <peer> x` behind what was sent to the worker so far;
    /// the test server waits for that line before it goes on.
    fn sync_after_changes(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext, peer: &str) {
        chat.update(cx, |chat, _| {
            let connection = chat.sessions[&NetworkId(1)].irc.as_ref().unwrap();
            connection
                .send_command(&format!("/msg {peer} x"), None)
                .unwrap();
        });
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
    fn peers_are_answered_only_with_an_explicitly_shared_url(cx: &mut TestAppContext) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (step_tx, step_rx) = std::sync::mpsc::channel::<()>();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let mut send = |text: &str| socket.write_all(text.as_bytes()).unwrap();
            assert_eq!(read_line(&mut lines), "CAP END");
            assert_eq!(read_line(&mut lines), "NICK me");
            // Connected with nothing shared: no mark.
            assert!(read_line(&mut lines).ends_with(" CayenChat"));
            send(":srv.example 001 me :Welcome\r\n:srv.example 376 me :End\r\n");
            // A query before sharing, then a line the test waits for.
            send(":early!u@h PRIVMSG me :\x01AVATAR\x01\r\n:early!u@h PRIVMSG me :hi\r\n");
            // After an autosaved draft and an explicit Share. The worker
            // takes commands and server lines in no fixed order, so the
            // query waits for a line queued after the change: once it
            // arrives, the change has been applied.
            step_rx.recv().unwrap();
            while read_line(&mut lines) != "PRIVMSG sync1 x" {}
            send(":kv!u@h PRIVMSG me :\x01AVATAR\x01\r\n");
            assert_eq!(
                read_line(&mut lines),
                format!("NOTICE kv :\x01AVATAR {URL}\x01"),
                "the shared URL, not an answer to the early query"
            );
            // After turning the option off: not answered.
            step_rx.recv().unwrap();
            while read_line(&mut lines) != "PRIVMSG sync2 x" {}
            send(":kv2!u@h PRIVMSG me :\x01AVATAR\x01\r\n:kv2!u@h PRIVMSG me :still there?\r\n");
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
        settings.servers[0].nickname = "me".into();
        settings.servers[0].ircv3.peer_avatars = true;
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec![]);
        config.port = port;
        config.use_tls = false;
        config.ircv3 = Ircv3Options {
            peer_avatars: true,
            ..Ircv3Options::default()
        };
        config.shared_avatar = crate::shared_peer_avatar(&settings.servers[0]);
        assert_eq!(config.shared_avatar, None);
        let connection = Connection::connect(config.clone()).unwrap();
        chat.update(cx, |chat, _| {
            let session = chat.sessions.get_mut(&NetworkId(1)).unwrap();
            session.connection_starting(&config);
            session.active_config = Some(config);
            session.irc = Some(connection);
        });
        pump(&chat, cx, |event| {
            matches!(event, Event::PrivateMessage { .. })
        });
        let answering = |chat: &Entity<ChatWindow>, cx: &mut VisualTestContext| {
            chat.read_with(cx, |chat, _| {
                chat.sessions[&NetworkId(1)].peer_avatars.answering.clone()
            })
        };
        // Autosaving a typed draft shares nothing.
        chat.update(cx, |chat, cx| {
            let mut next = chat.saved.clone();
            next.servers[0].avatar_url = URL.into();
            chat.apply_servers(next, cx);
        });
        assert_eq!(answering(&chat, cx), None);
        // Share with Peers: answered from now on; the mark waits for the
        // next connection, which will carry it.
        chat.update(cx, |chat, cx| {
            let mut next = chat.saved.clone();
            super::share_draft_with_peers(&mut next.servers[0], URL).unwrap();
            chat.apply_servers(next, cx);
        });
        assert_eq!(answering(&chat, cx).as_deref(), Some(URL));
        chat.read_with(cx, |chat, _| {
            let session = &chat.sessions[&NetworkId(1)];
            assert!(!session.peer_avatars.advertised);
            assert!(session.active_config.as_ref().unwrap().advertises_avatar());
        });
        sync_after_changes(&chat, cx, "sync1");
        step_tx.send(()).unwrap();
        // The answer goes out before anything else changes.
        pump(
            &chat,
            cx,
            |event| matches!(event, Event::Wire { line, .. } if line.starts_with("NOTICE kv ")),
        );
        // Turning the option off stops answering at once.
        chat.update(cx, |chat, cx| {
            let mut next = chat.saved.clone();
            let peer = super::IRCV3_FEATURES
                .iter()
                .find(|feature| feature.id == "ircv3-peer-avatars")
                .unwrap();
            super::toggle_feature(&mut next.servers[0], peer.toggle);
            chat.apply_servers(next, cx);
        });
        assert_eq!(answering(&chat, cx), None);
        chat.read_with(cx, |chat, _| {
            let config = chat.sessions[&NetworkId(1)].active_config.clone().unwrap();
            assert!(!config.ircv3.peer_avatars && config.shared_avatar.is_none());
        });
        sync_after_changes(&chat, cx, "sync2");
        step_tx.send(()).unwrap();
        pump(
            &chat,
            cx,
            |event| matches!(event, Event::PrivateMessage { text, .. } if text == "still there?"),
        );
        chat.update(cx, |chat, _| {
            chat.sessions.get_mut(&NetworkId(1)).unwrap().close();
        });
        server.join().unwrap();
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
