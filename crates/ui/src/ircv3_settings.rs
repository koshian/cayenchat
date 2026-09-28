//! Settings tab for opt-in IRCv3 features, chosen per server.
//!
//! This tab is the current home of IRCv3 preferences that need an explicit
//! opt-in. Each feature is one [`Ircv3Feature`] row reading and writing its
//! own field of [`Ircv3Preferences`]; moving a feature to another tab, or
//! enabling it by default, changes only its row or storage default and never
//! the protocol code.

use cayenchat_app::{
    ConnectionStatus,
    own_avatar::{Action, Blocked, Confirmed, Failure, Outcome, OwnAvatar},
};
use cayenchat_irc_core::{AvatarRequestFailure, MAX_PUBLISHED_AVATAR_BYTES, publishable_avatar};
use cayenchat_media::policy::{PublishProblem, publishable_avatar_url};
use cayenchat_model::NetworkId;
use cayenchat_storage::{Ircv3Preferences, ServerProfile, TextEncoding};
use gpui::{prelude::*, *};

use crate::{ChatWindow, SettingsWindow, account_settings::panel, settings_theme};

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
        panel = panel.child(self.render_own_avatar(&profile, cx));
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
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
    }
}

impl SettingsWindow {
    /// Our own avatar on the selected server: the saved draft URL and
    /// explicit Publish / Remove, with what the server confirmed.
    fn render_own_avatar(&mut self, profile: &ServerProfile, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let note = |text: String| {
            div()
                .ml(px(158.))
                .text_color(theme.text_secondary)
                .child(text)
        };
        let section = div().flex().flex_col().gap_2().pt_2().child(
            div()
                .font_weight(FontWeight::BOLD)
                .child(self.i18n.text("ircv3_avatar_heading")),
        );
        // Experimental and behind the server's metadata opt-in.
        if !profile.ircv3.metadata {
            return section.child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("ircv3_avatar_needs_metadata")),
            );
        }
        let status = self
            .owner
            .read(cx)
            .map(|chat| chat.own_avatar_status(&profile.id))
            .unwrap_or_default();
        let has_draft = !self.settings.avatar_url.read(cx).text().trim().is_empty();
        let server = match &status.confirmed {
            Confirmed::Unknown => self.i18n.text("ircv3_avatar_server_unknown"),
            Confirmed::NotSet => self.i18n.text("ircv3_avatar_server_none"),
            Confirmed::Set(url) => self.i18n.format("ircv3_avatar_server_set", &[("url", url)]),
        };
        let action_button = |id: &'static str, key: &str, primary: bool, enabled: bool| {
            let button = settings_theme::button(id, primary, cx).child(self.i18n.text(key));
            if enabled {
                button
            } else {
                button.opacity(0.5).cursor_default()
            }
        };
        let publish = action_button(
            "ircv3-avatar-publish",
            "ircv3_avatar_publish",
            true,
            status.can_request && has_draft,
        )
        .when(status.can_request && has_draft, |button| {
            button.on_click(
                cx.listener(|this, _, _, cx| this.request_own_avatar(Action::Publish, cx)),
            )
        });
        let remove = action_button(
            "ircv3-avatar-remove",
            "ircv3_avatar_remove",
            false,
            status.can_request,
        )
        .when(status.can_request, |button| {
            button
                .on_click(cx.listener(|this, _, _, cx| this.request_own_avatar(Action::Remove, cx)))
        });
        let outcome = status
            .outcome
            .as_ref()
            .map(|outcome| outcome_text(&self.i18n, outcome));
        section
            .child(crate::settings_field(
                &self.i18n.text("ircv3_avatar_url"),
                self.settings.avatar_url.clone(),
            ))
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.warning)
                    .child(self.i18n.text("ircv3_avatar_exposure")),
            )
            .child(note(self.i18n.text("ircv3_avatar_publish_hint")))
            .child(
                div()
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .child(publish)
                    .child(remove),
            )
            .child(div().ml(px(158.)).child(server))
            .when_some(outcome, |section, (text, failed)| {
                section.child(
                    div()
                        .ml(px(158.))
                        .when(failed, |d| d.text_color(theme.warning))
                        .child(text),
                )
            })
            .when(!status.can_request && !status.waiting, |section| {
                section.child(note(self.i18n.text("ircv3_avatar_not_ready")))
            })
            .when_some(self.avatar_feedback.clone(), |section, feedback| {
                section.child(div().ml(px(158.)).text_color(theme.warning).child(feedback))
            })
            .child(note(self.i18n.text("ircv3_avatar_display_note")))
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

/// What the IRCv3 tab shows about our own avatar on one server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OwnAvatarStatus {
    pub can_request: bool,
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
    use super::{IRCV3_FEATURES, avatar_url_problem};
    use cayenchat_storage::Ircv3Preferences;

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
