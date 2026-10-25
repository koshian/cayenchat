// SPDX-License-Identifier: GPL-3.0-only
//! The quick icon section at the top of the Connection tab (#593).
//!
//! One place to register an icon: it reuses the IRCv3 tab's URL draft, image
//! chooser, square selection and upload, and "Register" applies the existing
//! switches that make an icon visible to others (Metadata, CTCP AVATAR)
//! instead of adding a setting of its own. Expanding the section is only a
//! view state: nothing is saved or sent until "Register / Update" is pressed.

use cayenchat_app::own_avatar::Confirmed;
use cayenchat_irc_core::MAX_PUBLISHED_AVATAR_BYTES;
use cayenchat_storage::{ServerProfile, TextEncoding};
use gpui::{prelude::*, *};

use crate::{
    SettingsTab, SettingsWindow,
    ircv3_settings::{
        OwnAvatarStatus, outcome_text, peer_avatar_url_problem, peer_reconnect_needed,
    },
    settings_theme,
};

/// What registering `url` as the icon of `profile` changes in its settings:
/// the icon itself, showing icons on this server, and the CTCP AVATAR
/// sharing. `Err` is a localization key (which may use `{max}`) and leaves
/// `profile` as it was. Metadata is sent separately, on the connection.
pub(crate) fn register_in_profile(
    profile: &mut ServerProfile,
    url: &str,
) -> Result<(), &'static str> {
    let utf8 = profile.encoding == TextEncoding::Utf8;
    if let Some(key) = peer_avatar_url_problem(url, utf8) {
        return Err(key);
    }
    profile.avatar_url = url.to_owned();
    profile.user_avatars = true;
    profile.ircv3.peer_avatars = true;
    profile.peer_avatar_url = url.to_owned();
    Ok(())
}

/// The localization key of the CTCP AVATAR line, from what the connection
/// answers now (not only from what was saved): `None` when not shared.
fn ctcp_state_key(
    status: &OwnAvatarStatus,
    profile: &ServerProfile,
    saved: bool,
) -> Option<&'static str> {
    let shared = crate::shared_peer_avatar(profile)?;
    Some(if !saved {
        "avatar_quick_ctcp_unsaved"
    } else if peer_reconnect_needed(status, profile) {
        "avatar_quick_ctcp_reconnect"
    } else if status.connected && status.peer.answering.as_deref() != Some(&shared) {
        // The connection could not take the new URL (for example its
        // queue was full) and still answers with the old one.
        "avatar_quick_ctcp_stale"
    } else if status.connected {
        "avatar_quick_ctcp_active"
    } else {
        "avatar_quick_ctcp_ready"
    })
}

impl SettingsWindow {
    /// The explicit register / update step: puts `url` into the settings of
    /// server `profile_id` and tries to publish it on its connection. Which
    /// of the ways worked is shown by the status lines, not assumed here.
    pub(crate) fn register_avatar(&mut self, profile_id: &str, url: &str, cx: &mut Context<Self>) {
        self.avatar_feedback = None;
        self.avatar_setup_link = false;
        let shown = self.settings.values.selected_server == profile_id;
        let Some(profile) = self
            .settings
            .values
            .servers
            .iter_mut()
            .find(|profile| profile.id == profile_id)
        else {
            self.avatar_feedback = Some(self.i18n.text("ircv3_avatar_upload_server_gone"));
            cx.notify();
            return;
        };
        if let Err(key) = register_in_profile(profile, url) {
            let max = MAX_PUBLISHED_AVATAR_BYTES.to_string();
            self.avatar_feedback = Some(self.i18n.format(key, &[("max", &max)]));
            cx.notify();
            return;
        }
        if shown {
            self.settings
                .avatar_url
                .update(cx, |field, cx| field.set_text(url, cx));
        }
        if !self.avatar_refresh.iter().any(|id| id == profile_id) {
            self.avatar_refresh.push(profile_id.to_owned());
        }
        let sent = self.owner.update(cx, |owner, _, _| {
            owner.request_own_avatar(profile_id, Some(url.to_owned()))
        });
        self.avatar_feedback = match sent {
            Ok(Ok(())) => None,
            // Not connected or without Metadata: the status lines say so.
            Ok(Err("ircv3_avatar_unavailable")) => None,
            Ok(Err(key)) => Some(self.i18n.text(key)),
            Err(error) => Some(
                self.i18n
                    .format("chat_closed", &[("error", &error.to_string())]),
            ),
        };
        cx.notify();
    }

    /// One line for the collapsed section: whether an icon is registered.
    fn quick_avatar_summary(&self, profile: &ServerProfile) -> String {
        if profile.avatar_url.is_empty() {
            self.i18n.text("avatar_quick_summary_none")
        } else {
            self.i18n.format(
                if profile.user_avatars {
                    "avatar_quick_summary_shown"
                } else {
                    "avatar_quick_summary_hidden"
                },
                &[("url", &profile.avatar_url)],
            )
        }
    }

    /// The separate states of the two ways of publishing, for the section's
    /// footer: `(text, is a failure)`.
    fn quick_avatar_status(
        &self,
        profile: &ServerProfile,
        status: &OwnAvatarStatus,
    ) -> Vec<(String, bool)> {
        if profile.avatar_url.is_empty() {
            return Vec::new();
        }
        let mut lines = Vec::new();
        let published =
            matches!(&status.confirmed, Confirmed::Set(url) if *url == profile.avatar_url);
        let metadata = if let Some(outcome) = status.outcome.as_ref().filter(|_| !published) {
            let (text, failed) = outcome_text(&self.i18n, outcome);
            Some((
                self.i18n
                    .format("avatar_quick_metadata", &[("state", &text)]),
                failed,
            ))
        } else {
            let key = if published {
                "avatar_quick_metadata_published"
            } else if !status.connected {
                "avatar_quick_metadata_offline"
            } else if status.unsupported {
                "avatar_quick_metadata_unsupported"
            } else {
                "avatar_quick_metadata_unsent"
            };
            Some((self.i18n.text(key), false))
        };
        lines.extend(metadata);
        let saved = self.saved.profile(&profile.id);
        let applied = saved.is_some_and(|saved| {
            saved.ircv3.peer_avatars == profile.ircv3.peer_avatars
                && saved.peer_avatar_url == profile.peer_avatar_url
        });
        if let Some(key) = ctcp_state_key(status, profile, applied) {
            lines.push((self.i18n.text(key), key == "avatar_quick_ctcp_stale"));
        }
        lines
    }

    pub(crate) fn render_quick_avatar(
        &mut self,
        profile: &ServerProfile,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = settings_theme::palette(cx);
        let open = self.avatar_quick_open;
        let registered = !profile.avatar_url.is_empty();
        // A flat disclosure item like the other settings: the whole header
        // row toggles, the chevron shows the fold and the state word says what
        // is registered. The label sits in the label column like any other
        // setting; the fold mark, state and all content use the right column.
        let text = theme.text;
        let toggle = |this: &mut Self, cx: &mut Context<Self>| {
            this.avatar_quick_open = !this.avatar_quick_open;
            cx.notify();
        };
        let section = div().flex().flex_col().gap_2().child(
            div()
                .id("avatar-quick-toggle")
                .track_focus(&self.avatar_toggle_focus)
                .flex()
                .items_center()
                .gap_2()
                // As tall as a row with an input, so the item spacing matches.
                .min_h(px(35.))
                .cursor_pointer()
                .hover(|s| s.bg(theme.hover))
                .focus(move |s| s.border_b_1().border_color(text))
                .child(settings_theme::label_column().child(self.i18n.text("avatar_quick_toggle")))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .w_4()
                                .flex_shrink_0()
                                .child(if open { "▾" } else { "▸" }),
                        )
                        .child(div().text_color(theme.text_secondary).child(self.i18n.text(
                            if registered {
                                "avatar_quick_state_set"
                            } else {
                                "avatar_quick_state_unset"
                            },
                        ))),
                )
                .on_click(cx.listener(move |this, _, _, cx| toggle(this, cx))),
        );
        if !open {
            return section;
        }
        // Everything under the header starts in the right column.
        let right = || {
            div()
                .ml(settings_theme::LABEL_INDENT)
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
        };
        let section = section.child(
            div()
                .id("avatar-quick-summary")
                .ml(settings_theme::LABEL_INDENT)
                .min_w_0()
                .text_color(theme.text_secondary)
                .child(self.quick_avatar_summary(profile)),
        );

        let status = self
            .owner
            .read(cx)
            .map(|chat| chat.own_avatar_status(&profile.id))
            .unwrap_or_default();
        let draft = self.settings.avatar_url.read(cx).text().trim().to_owned();
        let uploading = self.avatar_upload.uploading().map(str::to_owned);
        let lines = self.quick_avatar_status(profile, &status);
        let provider = self
            .settings
            .values
            .image_upload
            .provider
            .as_deref()
            .and_then(cayenchat_upload::provider)
            .map(|provider| provider.name);
        let button = |id: &'static str, key: &str, primary: bool| {
            settings_theme::button(id, primary, cx).child(self.i18n.text(key))
        };
        let buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            // Choosing an image works without an image host; the missing
            // setup is explained when an image is chosen.
            .when(uploading.is_none(), |row| {
                row.child(
                    button("avatar-quick-choose", "ircv3_avatar_choose", false).on_click(
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
                    button("avatar-quick-upload-cancel", "cancel", false)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_avatar_upload(cx))),
                )
            })
            .when(!draft.is_empty(), |row| {
                let id = profile.id.clone();
                let url = draft.clone();
                row.child(
                    button("avatar-quick-register", "avatar_quick_register", true).on_click(
                        cx.listener(move |this, _, _, cx| this.register_avatar(&id, &url, cx)),
                    ),
                )
            })
            .when(self.avatar_setup_link, |row| {
                let id = profile.id.clone();
                row.child(
                    button("avatar-quick-setup", "avatar_quick_open_upload", false).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.avatar_return = Some(id.clone());
                            this.show_tab(SettingsTab::ImageUpload, cx);
                            cx.notify();
                        }),
                    ),
                )
            });
        section.child(
            div()
                .id("avatar-quick-body")
                .flex()
                .flex_col()
                .gap_2()
                .on_action(cx.listener(Self::paste_avatar_image))
                .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(theme.selected))
                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    this.drop_avatar_image(paths.paths(), paths.keep_alive(), window, cx)
                }))
                .child(
                    right()
                        .child(
                            div()
                                .text_color(theme.warning)
                                .child(self.i18n.text("avatar_quick_notice")),
                        )
                        .child(
                            div()
                                .text_color(theme.text_secondary)
                                .child(self.i18n.text("avatar_quick_local_note")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_color(theme.text_secondary)
                                        .child(self.i18n.text("ircv3_avatar_url")),
                                )
                                .child(self.settings.avatar_url.clone()),
                        )
                        .child(buttons)
                        .children(lines.into_iter().map(|(text, failed)| {
                            div()
                                .when(failed, |d| d.text_color(theme.warning))
                                .when(!failed, |d| d.text_color(theme.text_secondary))
                                .child(text)
                        }))
                        .when_some(self.avatar_feedback.clone(), |d, feedback| {
                            d.child(div().text_color(theme.warning).child(feedback))
                        })
                        .when(self.avatar_opening, |d| {
                            d.child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(self.i18n.text("ircv3_avatar_edit_loading")),
                            )
                        }),
                )
                // The editor sets its own right-column margin.
                .children(provider.and_then(|name| self.render_avatar_editor(name, true, cx))),
        )
    }

    /// On the Image Upload tab: back to the icon section of the server that
    /// sent the user here.
    pub(crate) fn render_avatar_return(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let id = self.avatar_return.clone()?;
        let profile = self.settings.values.servers.iter().find(|p| p.id == id)?;
        let label = crate::ircv3_settings::server_label(profile, &self.i18n);
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    settings_theme::button("avatar-return", false, cx)
                        .child(self.i18n.text("avatar_quick_return"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.settings.values.selected_server != id {
                                this.select_server(id.clone(), cx);
                            }
                            this.avatar_quick_open = true;
                            this.show_tab(SettingsTab::Connection, cx);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .text_color(settings_theme::palette(cx).text_secondary)
                        .child(label),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ctcp_state_key, register_in_profile};
    use crate::ircv3_settings::OwnAvatarStatus;
    use crate::session::PeerAvatarConnection;
    use crate::{SettingsTab, SettingsWindow};
    use cayenchat_storage::{ServerProfile, Settings, TextEncoding};
    use gpui::{Entity, TestAppContext, VisualTestContext};

    const URL: &str = "https://img.example/me.png";

    #[test]
    fn registering_turns_on_display_and_both_publishing_ways() {
        let mut profile = ServerProfile::default();
        profile.user_avatars = false;
        profile.ircv3.peer_avatars = false;
        register_in_profile(&mut profile, URL).unwrap();
        assert_eq!(profile.avatar_url, URL);
        assert_eq!(profile.peer_avatar_url, URL);
        assert!(profile.user_avatars && profile.ircv3.peer_avatars);
    }

    #[test]
    fn a_rejected_url_changes_nothing() {
        let mut profile = ServerProfile::default();
        profile.user_avatars = false;
        let before = profile.clone();
        for bad in ["", "not a url", "https://user:pw@img.example/a.png"] {
            assert!(register_in_profile(&mut profile, bad).is_err(), "{bad}");
            assert_eq!(profile, before);
        }
        // A legacy encoding cannot carry a non-ASCII URL.
        profile.encoding = TextEncoding::Iso2022Jp;
        let before = profile.clone();
        assert!(register_in_profile(&mut profile, "https://img.example/ア.png").is_err());
        assert_eq!(profile, before);
    }

    #[test]
    fn ctcp_state_follows_what_the_connection_answers() {
        let mut profile = ServerProfile::default();
        register_in_profile(&mut profile, URL).unwrap();
        let connected = |answering: Option<&str>| OwnAvatarStatus {
            connected: true,
            peer: PeerAvatarConnection {
                enabled: true,
                advertised: true,
                answering: answering.map(str::to_owned),
            },
            ..OwnAvatarStatus::default()
        };
        let p = profile.clone();
        let key = |status: &OwnAvatarStatus| ctcp_state_key(status, &p, true);
        // Updated normally.
        assert_eq!(key(&connected(Some(URL))), Some("avatar_quick_ctcp_active"));
        // The update failed: the connection still answers with the old URL.
        let old = Some("https://img.example/old.png");
        assert_eq!(key(&connected(old)), Some("avatar_quick_ctcp_stale"));
        assert_eq!(key(&connected(None)), Some("avatar_quick_ctcp_stale"));
        // Not connected, not saved, not shared.
        assert_eq!(
            key(&OwnAvatarStatus::default()),
            Some("avatar_quick_ctcp_ready")
        );
        assert_eq!(
            ctcp_state_key(&connected(Some(URL)), &profile, false),
            Some("avatar_quick_ctcp_unsaved")
        );
        profile.ircv3.peer_avatars = false;
        assert_eq!(ctcp_state_key(&connected(None), &profile, true), None);
    }

    fn open(
        cx: &mut TestAppContext,
        settings: Settings,
    ) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let owner = cx.add_window({
            let settings = settings.clone();
            move |window, cx| crate::ChatWindow::with_settings(settings, None, window, cx)
        });
        let (form, vcx) = cx
            .add_window_view(|window, cx| SettingsWindow::new(owner, settings.clone(), window, cx));
        vcx.run_until_parked();
        (form, vcx)
    }

    fn two_servers() -> Settings {
        let mut settings = crate::settings_with_channels("");
        let mut other = settings.servers[0].clone();
        other.id = "other".into();
        other.host = "other.example".into();
        other.user_avatars = false;
        settings.servers[0].user_avatars = false;
        settings.servers.push(other);
        settings
    }

    #[gpui::test]
    fn the_section_starts_folded_and_folding_changes_no_value(cx: &mut TestAppContext) {
        let (form, cx) = open(cx, two_servers());
        let before = form.read_with(cx, |form, _| form.settings.values.clone());
        assert!(form.read_with(cx, |form, _| !form.avatar_quick_open));
        form.update(cx, |form, cx| {
            form.avatar_quick_open = true;
            cx.notify();
        });
        cx.run_until_parked();
        form.update(cx, |form, cx| {
            form.avatar_quick_open = false;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            form.read_with(cx, |form, _| form.settings.values.clone()),
            before
        );
    }

    #[gpui::test]
    fn registering_affects_only_the_target_server_and_not_reiwa_mode(cx: &mut TestAppContext) {
        let (form, cx) = open(cx, two_servers());
        let reiwa = form.read_with(cx, |form, _| form.settings.values.appearance.reiwa_mode);
        let selected = form.read_with(cx, |form, _| form.settings.values.selected_server.clone());
        form.update(cx, |form, cx| form.register_avatar(&selected, URL, cx));
        form.read_with(cx, |form, cx| {
            let values = &form.settings.values;
            let target = values.profile(&selected).unwrap();
            assert!(target.user_avatars && target.ircv3.peer_avatars);
            assert_eq!(
                (&target.avatar_url[..], &target.peer_avatar_url[..]),
                (URL, URL)
            );
            assert_eq!(form.settings.avatar_url.read(cx).text(), URL);
            let other = values.profile("other").unwrap();
            assert!(!other.user_avatars && other.avatar_url.is_empty());
            assert_eq!(values.appearance.reiwa_mode, reiwa);
            // Not connected: nothing was sent, and it is not shown as done.
            assert!(form.avatar_feedback.is_none());
        });

        // A later switch-off stays off until the next registration.
        form.update(cx, |form, cx| {
            form.settings
                .values
                .selected_profile_mut()
                .unwrap()
                .user_avatars = false;
            form.avatar_quick_open = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(form.read_with(cx, |form, _| {
            !form
                .settings
                .values
                .profile(&selected)
                .unwrap()
                .user_avatars
        }));
        form.update(cx, |form, cx| form.register_avatar(&selected, URL, cx));
        assert!(form.read_with(cx, |form, _| {
            form.settings
                .values
                .profile(&selected)
                .unwrap()
                .user_avatars
        }));
    }

    #[gpui::test]
    fn a_bad_url_is_reported_and_nothing_is_registered(cx: &mut TestAppContext) {
        let (form, cx) = open(cx, two_servers());
        let selected = form.read_with(cx, |form, _| form.settings.values.selected_server.clone());
        let before = form.read_with(cx, |form, _| form.settings.values.clone());
        form.update(cx, |form, cx| form.register_avatar(&selected, "nope", cx));
        form.read_with(cx, |form, _| {
            assert!(form.avatar_feedback.is_some());
            assert_eq!(form.settings.values, before);
        });
    }

    #[gpui::test]
    fn the_way_back_from_the_upload_tab_lasts_until_the_connection_tab(cx: &mut TestAppContext) {
        let (form, cx) = open(cx, two_servers());
        let selected = form.read_with(cx, |form, _| form.settings.values.selected_server.clone());
        form.update(cx, |form, cx| {
            form.avatar_return = Some(selected.clone());
            form.show_tab(SettingsTab::ImageUpload, cx);
        });
        assert_eq!(
            form.read_with(cx, |form, _| form.avatar_return.clone()),
            Some(selected)
        );
        form.update(cx, |form, cx| form.show_tab(SettingsTab::Connection, cx));
        assert_eq!(
            form.read_with(cx, |form, _| form.avatar_return.clone()),
            None
        );
    }
}
