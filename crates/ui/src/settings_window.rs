//! The settings window and the form it edits.

use crate::{
    ChatWindow, CopyDiagnostics, Disconnect, FocusNextField, FocusPreviousField, OpenSettings,
    Quit, Reconnect, ShortcutPrefs, ToggleDebug, account_settings, apply_shortcuts, autostart,
    avatar_editor, color_picker, connection_config, decorations, diagnostics, field_traversal,
    forget_removed_profiles, i18n_error, input::TextInput, ircv3_settings, localization::Localizer,
    notification_rules, saved_connection_secrets, scrollbar, secrets, settings_file,
    settings_theme, shortcut_settings, theme,
};
use cayenchat_app::{attachments::AttachmentFlow, notifications};
use cayenchat_irc_core::ConnectionConfig;
use cayenchat_storage::{
    Appearance, AutoJoinEntry, ChannelNumberModifier, CredentialBackendKind, CredentialStore,
    DarkColors, Language, Notifications, Secret, SecretKey, ServerProfile, Settings, TextEncoding,
    TextKeyTheme, ThemeMode, color_value,
};
use gpui::{prelude::*, *};
use std::{collections::HashMap, time::Duration};

pub(crate) struct SettingsForm {
    pub(crate) values: Settings,
    server_list_open: bool,
    encoding_list_open: bool,
    pub(crate) language_list_open: bool,
    pub(crate) ircv3_server_list_open: bool,
    pub(crate) custom_host: Entity<TextInput>,
    pub(crate) port: Entity<TextInput>,
    pub(crate) nickname: Entity<TextInput>,
    username: Entity<TextInput>,
    realname: Entity<TextInput>,
    quit_message: Entity<TextInput>,
    display_name: Entity<TextInput>,
    pub(crate) channels: Entity<TextInput>,
    /// Password fields start empty; typing replaces a saved value.
    server_password: Entity<TextInput>,
    sasl_username: Entity<TextInput>,
    sasl_password: Entity<TextInput>,
    saved_server_password: bool,
    saved_sasl_password: bool,
    /// Typed (server, SASL) passwords of servers not being shown, by server
    /// ID. They stay in memory until Save stores them (D021).
    drafts: HashMap<String, (String, String)>,
    pub(crate) member_list_background: Entity<TextInput>,
    pub(crate) main_log_background: Entity<TextInput>,
    pub(crate) main_log_alternate: Entity<TextInput>,
    pub(crate) channel_event_color: Entity<TextInput>,
    pub(crate) notice_color: Entity<TextInput>,
    pub(crate) highlight_color: Entity<TextInput>,
    pub(crate) sub_log_background: Entity<TextInput>,
    pub(crate) sub_log_alternate: Entity<TextInput>,
    pub(crate) dark_member_list_background: Entity<TextInput>,
    pub(crate) dark_main_log_background: Entity<TextInput>,
    pub(crate) dark_main_log_alternate: Entity<TextInput>,
    pub(crate) dark_channel_event_color: Entity<TextInput>,
    pub(crate) dark_notice_color: Entity<TextInput>,
    pub(crate) dark_highlight_color: Entity<TextInput>,
    pub(crate) dark_sub_log_background: Entity<TextInput>,
    pub(crate) dark_sub_log_alternate: Entity<TextInput>,
    pub(crate) url_tooltip_color: Entity<TextInput>,
    pub(crate) dark_url_tooltip_color: Entity<TextInput>,
    /// Percent, as typed.
    pub(crate) url_tooltip_opacity: Entity<TextInput>,
    pub(crate) main_log_font: Entity<TextInput>,
    pub(crate) sub_log_font: Entity<TextInput>,
    pub(crate) member_font: Entity<TextInput>,
    pub(crate) channel_font: Entity<TextInput>,
    pub(crate) input_font: Entity<TextInput>,
    pub(crate) time_font: Entity<TextInput>,
    /// Comma-separated notification keywords.
    pub(crate) keywords: Entity<TextInput>,
    /// Draft URL of our own avatar for the selected server (IRCv3 tab).
    /// Editing it saves the draft; only Publish sends it.
    pub(crate) avatar_url: Entity<TextInput>,
}

impl SettingsForm {
    pub(crate) fn new(
        values: Settings,
        i18n: &Localizer,
        store: &CredentialStore,
        cx: &mut App,
    ) -> Self {
        let (saved_server_password, saved_sasl_password) = values
            .selected_profile()
            .map_or((false, false), |profile| saved_passwords(profile, store));
        let empty = ServerProfile::default();
        let profile = values.selected_profile().unwrap_or(&empty);
        let field = |placeholder: &str, value: &str, secret: bool, cx: &mut App| {
            cx.new(|cx| TextInput::new_settings_field(placeholder, value, secret, cx))
        };
        Self {
            custom_host: field(
                &i18n.text("server_host_placeholder"),
                &profile.host,
                false,
                cx,
            ),
            port: field("6667", &profile.port.to_string(), false, cx),
            nickname: field(&i18n.text("nickname"), &profile.nickname, false, cx),
            username: field(
                &i18n.text("username_placeholder"),
                &profile.username,
                false,
                cx,
            ),
            realname: field(
                &i18n.text("realname_placeholder"),
                &profile.realname,
                false,
                cx,
            ),
            quit_message: field(
                &i18n.text("quit_message_placeholder"),
                &profile.quit_message,
                false,
                cx,
            ),
            display_name: field(
                &i18n.text("display_name_placeholder"),
                &profile.display_name,
                false,
                cx,
            ),
            channels: field("#first,#second", &profile.channels, false, cx),
            server_password: field(
                &i18n.text(if saved_server_password {
                    "password_saved_placeholder"
                } else {
                    "server_password_placeholder"
                }),
                "",
                true,
                cx,
            ),
            sasl_username: field(
                &i18n.text("sasl_account_placeholder"),
                &profile.sasl_username,
                false,
                cx,
            ),
            sasl_password: field(
                &i18n.text(if saved_sasl_password {
                    "password_saved_placeholder"
                } else {
                    "sasl_password"
                }),
                "",
                true,
                cx,
            ),
            saved_server_password,
            saved_sasl_password,
            drafts: HashMap::new(),
            member_list_background: field(
                "#FFFFFF",
                &values.appearance.member_list_background,
                false,
                cx,
            ),
            main_log_background: field(
                "#FFFFFF",
                &values.appearance.main_log_background,
                false,
                cx,
            ),
            main_log_alternate: field("#F2F5FF", &values.appearance.main_log_alternate, false, cx),
            channel_event_color: field(
                "#007D00",
                &values.appearance.channel_event_color,
                false,
                cx,
            ),
            notice_color: field("#7A838C", &values.appearance.notice_color, false, cx),
            highlight_color: field("#D46A8E", &values.appearance.highlight_color, false, cx),
            sub_log_background: field("#F9FAFB", &values.appearance.sub_log_background, false, cx),
            sub_log_alternate: field("#F2F5FF", &values.appearance.sub_log_alternate, false, cx),
            dark_member_list_background: field(
                "#1F2124",
                &values.appearance.dark.member_list_background,
                false,
                cx,
            ),
            dark_main_log_background: field(
                "#1F2124",
                &values.appearance.dark.main_log_background,
                false,
                cx,
            ),
            dark_main_log_alternate: field(
                "#272B31",
                &values.appearance.dark.main_log_alternate,
                false,
                cx,
            ),
            dark_channel_event_color: field(
                "#6CC46C",
                &values.appearance.dark.channel_event_color,
                false,
                cx,
            ),
            dark_notice_color: field("#8C949C", &values.appearance.dark.notice_color, false, cx),
            dark_highlight_color: field(
                "#EFA0BE",
                &values.appearance.dark.highlight_color,
                false,
                cx,
            ),
            dark_sub_log_background: field(
                "#24272B",
                &values.appearance.dark.sub_log_background,
                false,
                cx,
            ),
            dark_sub_log_alternate: field(
                "#2C3036",
                &values.appearance.dark.sub_log_alternate,
                false,
                cx,
            ),
            url_tooltip_color: field("#FFFFFF", &values.appearance.url_tooltip_color, false, cx),
            dark_url_tooltip_color: field(
                "#2A2D32",
                &values.appearance.dark.url_tooltip_color,
                false,
                cx,
            ),
            url_tooltip_opacity: field(
                "80",
                &values.appearance.url_tooltip_opacity.to_string(),
                false,
                cx,
            ),
            main_log_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.main_log_font,
                false,
                cx,
            ),
            sub_log_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.sub_log_font,
                false,
                cx,
            ),
            member_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.member_font,
                false,
                cx,
            ),
            channel_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.channel_font,
                false,
                cx,
            ),
            input_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.input_font,
                false,
                cx,
            ),
            time_font: field(
                &i18n.text("font_monospace_placeholder"),
                &values.appearance.time_font,
                false,
                cx,
            ),
            keywords: field(
                &i18n.text("keywords_placeholder"),
                &values.notifications.keywords.join(", "),
                false,
                cx,
            ),
            avatar_url: {
                let input = field(
                    &i18n.text("ircv3_avatar_url_placeholder"),
                    &profile.avatar_url,
                    false,
                    cx,
                );
                // A pasted image goes to the avatar upload (IRCv3 tab).
                input.update(cx, |input, _| input.accept_pasted_images());
                input
            },
            server_list_open: false,
            encoding_list_open: false,
            language_list_open: false,
            ircv3_server_list_open: false,
            values,
        }
    }

    pub(crate) fn snapshot(&self, cx: &App) -> Result<Settings, String> {
        self.snapshot_with(cx, true)
    }

    /// The form as settings. Without `with_servers`, the shown server's
    /// fields are not read (and not validated), so an unfinished connection
    /// entry does not stop other settings from being saved; the server list
    /// and selection are then the form's earlier values.
    fn snapshot_with(&self, cx: &App, with_servers: bool) -> Result<Settings, String> {
        let mut settings = self.values.clone();
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        if with_servers {
            let host = value(&self.custom_host);
            // Without a server the host and port fields are hidden and unused.
            let port = match self.port.read(cx).text().trim().parse() {
                Ok(port) if port != 0 => port,
                _ if settings.servers.is_empty() => 6667,
                _ => return Err(i18n_error(settings.language, "port_invalid")),
            };
            if let Some(profile) = settings.selected_profile_mut() {
                profile.host = host;
                profile.port = port;
                profile.nickname = value(&self.nickname);
                profile.username = value(&self.username);
                profile.realname = value(&self.realname);
                profile.quit_message = value(&self.quit_message);
                profile.display_name = value(&self.display_name);
                profile.channels = value(&self.channels);
                profile.sasl_username = value(&self.sasl_username);
                profile.avatar_url = value(&self.avatar_url);
            }
        }
        settings.appearance = Appearance {
            member_list_background: value(&self.member_list_background),
            main_log_background: value(&self.main_log_background),
            main_log_alternate: value(&self.main_log_alternate),
            channel_event_color: value(&self.channel_event_color),
            notice_color: value(&self.notice_color),
            highlight_color: value(&self.highlight_color),
            sub_log_background: value(&self.sub_log_background),
            sub_log_alternate: value(&self.sub_log_alternate),
            url_tooltip_color: value(&self.url_tooltip_color),
            url_tooltip_opacity: value(&self.url_tooltip_opacity)
                .parse()
                .ok()
                .filter(|percent| {
                    (cayenchat_storage::MIN_URL_TOOLTIP_OPACITY..=100).contains(percent)
                })
                .ok_or_else(|| i18n_error(settings.language, "url_tooltip_opacity_invalid"))?,
            alternate_rows: self.values.appearance.alternate_rows,
            image_previews: self.values.appearance.image_previews,
            user_avatars: self.values.appearance.user_avatars,
            compact_urls: self.values.appearance.compact_urls,
            reiwa_mode: self.values.appearance.reiwa_mode,
            main_log_font: value(&self.main_log_font),
            sub_log_font: value(&self.sub_log_font),
            member_font: value(&self.member_font),
            channel_font: value(&self.channel_font),
            input_font: value(&self.input_font),
            time_font: value(&self.time_font),
            dark: DarkColors {
                member_list_background: value(&self.dark_member_list_background),
                main_log_background: value(&self.dark_main_log_background),
                main_log_alternate: value(&self.dark_main_log_alternate),
                channel_event_color: value(&self.dark_channel_event_color),
                notice_color: value(&self.dark_notice_color),
                highlight_color: value(&self.dark_highlight_color),
                sub_log_background: value(&self.dark_sub_log_background),
                sub_log_alternate: value(&self.dark_sub_log_alternate),
                url_tooltip_color: value(&self.dark_url_tooltip_color),
            },
            saved_colors: self.values.appearance.saved_colors.clone(),
        };
        settings.appearance.validate()?;
        settings.notifications.keywords =
            notifications::parse_keywords(self.keywords.read(cx).text());
        Ok(settings)
    }

    /// Typed passwords win; otherwise saved ones are used when saving is on.
    fn connection_config(
        &self,
        settings: &Settings,
        store: &CredentialStore,
        i18n: &Localizer,
        cx: &App,
    ) -> Result<ConnectionConfig, String> {
        let typed = |field: &Entity<TextInput>| {
            let text = field.read(cx).text();
            (!text.is_empty()).then(|| Secret::new(text))
        };
        let profile = settings
            .selected_profile()
            .ok_or_else(|| i18n.text("server_required"))?;
        let (saved_server, saved_sasl) = saved_connection_secrets(profile, store, i18n)?;
        connection_config(
            profile,
            settings.language,
            typed(&self.server_password).or(saved_server),
            typed(&self.sasl_password).or(saved_sasl),
        )
    }

    /// Every text field, so edits to any of them can trigger an autosave.
    fn text_fields(&self) -> [&Entity<TextInput>; 38] {
        [
            &self.custom_host,
            &self.port,
            &self.nickname,
            &self.username,
            &self.realname,
            &self.quit_message,
            &self.display_name,
            &self.channels,
            &self.server_password,
            &self.sasl_username,
            &self.sasl_password,
            &self.member_list_background,
            &self.main_log_background,
            &self.main_log_alternate,
            &self.channel_event_color,
            &self.notice_color,
            &self.highlight_color,
            &self.sub_log_background,
            &self.sub_log_alternate,
            &self.dark_member_list_background,
            &self.dark_main_log_background,
            &self.dark_main_log_alternate,
            &self.dark_channel_event_color,
            &self.dark_notice_color,
            &self.dark_highlight_color,
            &self.dark_sub_log_background,
            &self.dark_sub_log_alternate,
            &self.url_tooltip_color,
            &self.dark_url_tooltip_color,
            &self.url_tooltip_opacity,
            &self.main_log_font,
            &self.sub_log_font,
            &self.member_font,
            &self.channel_font,
            &self.input_font,
            &self.time_font,
            &self.keywords,
            &self.avatar_url,
        ]
    }

    /// Moves the typed passwords of the shown server into `drafts`.
    fn stash_typed_passwords(&mut self, cx: &App) {
        let Some(id) = self
            .values
            .selected_profile()
            .map(|profile| profile.id.clone())
        else {
            return;
        };
        let text = |field: &Entity<TextInput>| field.read(cx).text().to_owned();
        let typed = (text(&self.server_password), text(&self.sasl_password));
        if typed.0.is_empty() && typed.1.is_empty() {
            self.drafts.remove(&id);
        } else {
            self.drafts.insert(id, typed);
        }
    }

    /// Whether a typed password of a server that stores its passwords is
    /// waiting for Save.
    fn pending_passwords(&self, cx: &App) -> bool {
        let remembers = |id: &str| {
            self.values
                .servers
                .iter()
                .any(|profile| profile.id == id && profile.remember_passwords)
        };
        let shown = self.values.selected_profile().is_some_and(|profile| {
            profile.remember_passwords
                && [&self.server_password, &self.sasl_password]
                    .into_iter()
                    .any(|field| !field.read(cx).text().is_empty())
        });
        shown
            || self.drafts.iter().any(|(id, (server, sasl))| {
                remembers(id) && !(server.is_empty() && sasl.is_empty())
            })
    }

    /// Stores the typed passwords of the servers in `settings` that keep
    /// them, then empties the shown fields so plaintext does not stay in the
    /// form. Nothing is stored before this (D021).
    fn persist_passwords(
        &mut self,
        settings: &Settings,
        store: &CredentialStore,
        i18n: &Localizer,
        cx: &mut App,
    ) -> Result<(), String> {
        self.stash_typed_passwords(cx);
        let shown = self.values.selected_server.clone();
        for profile in &settings.servers {
            if !profile.remember_passwords || profile.host.is_empty() {
                continue;
            }
            let Some((server, sasl)) = self.drafts.get(&profile.id).cloned() else {
                continue;
            };
            for (text, key, saved) in [
                (
                    server,
                    profile.server_password_key(),
                    &mut self.saved_server_password,
                ),
                (
                    sasl,
                    profile.sasl_password_key(),
                    &mut self.saved_sasl_password,
                ),
            ] {
                if text.is_empty() {
                    continue;
                }
                store
                    .set(&key, &Secret::new(text))
                    .map_err(|error| secrets::error_text(i18n, &error))?;
                if profile.id == shown {
                    *saved = true;
                }
            }
            self.drafts.remove(&profile.id);
            if profile.id == shown {
                let hint = i18n.text("password_saved_placeholder");
                for (field, saved) in [
                    (&self.server_password, self.saved_server_password),
                    (&self.sasl_password, self.saved_sasl_password),
                ] {
                    if saved {
                        field.update(cx, |field, cx| {
                            field.set_text("", cx);
                            field.set_placeholder(&hint, cx);
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

impl SettingsForm {
    /// Shows another server: `change` selects it (or adds it). The shown
    /// server's edits are kept in `values` first and its typed passwords in
    /// `drafts`, so nothing typed for one server can end up in the next.
    /// Nothing is written until Save (D021).
    fn switch_server(
        &mut self,
        change: impl FnOnce(&mut Settings),
        store: &CredentialStore,
        i18n: &Localizer,
        cx: &mut App,
    ) -> Result<(), String> {
        let mut settings = self.snapshot(cx)?;
        self.stash_typed_passwords(cx);
        change(&mut settings);
        self.values = settings;
        self.show_selected(store, i18n, cx);
        Ok(())
    }

    /// Fills the server fields from the selected profile alone; password
    /// fields are emptied and say whether that server has saved passwords.
    pub(crate) fn show_selected(
        &mut self,
        store: &CredentialStore,
        i18n: &Localizer,
        cx: &mut App,
    ) {
        let profile = self.values.selected_profile().cloned().unwrap_or_default();
        self.custom_host
            .update(cx, |field, cx| field.set_text(&profile.host, cx));
        self.port.update(cx, |field, cx| {
            field.set_text(&profile.port.to_string(), cx)
        });
        for (field, value) in [
            (&self.nickname, &profile.nickname),
            (&self.username, &profile.username),
            (&self.realname, &profile.realname),
            (&self.quit_message, &profile.quit_message),
            (&self.display_name, &profile.display_name),
            (&self.channels, &profile.channels),
            (&self.sasl_username, &profile.sasl_username),
            (&self.avatar_url, &profile.avatar_url),
        ] {
            field.update(cx, |field, cx| field.set_text(value, cx));
        }
        let (saved_server, saved_sasl) = saved_passwords(&profile, store);
        self.saved_server_password = saved_server;
        self.saved_sasl_password = saved_sasl;
        let (draft_server, draft_sasl) = self.drafts.get(&profile.id).cloned().unwrap_or_default();
        for (field, saved, key, draft) in [
            (
                &self.server_password,
                saved_server,
                "server_password_placeholder",
                draft_server,
            ),
            (&self.sasl_password, saved_sasl, "sasl_password", draft_sasl),
        ] {
            let placeholder = i18n.text(if saved {
                "password_saved_placeholder"
            } else {
                key
            });
            field.update(cx, |field, cx| {
                field.set_text(&draft, cx);
                field.set_placeholder(&placeholder, cx);
            });
        }
        self.server_list_open = false;
        self.encoding_list_open = false;
        self.language_list_open = false;
        self.ircv3_server_list_open = false;
    }
}

/// Arranges `servers` like `file` where both have a server, so a move made
/// in the chat window (D041) is not undone by a window holding older
/// settings. Servers the file lacks keep their relative order, at the end.
/// Returns whether anything moved.
pub(crate) fn follow_server_order(servers: &mut [ServerProfile], file: &[ServerProfile]) -> bool {
    let position = |id: &str| file.iter().position(|p| p.id == id);
    let ours: Vec<&str> = servers
        .iter()
        .filter(|p| position(&p.id).is_some())
        .map(|p| p.id.as_str())
        .collect();
    let theirs: Vec<&str> = file
        .iter()
        .filter(|p| servers.iter().any(|s| s.id == p.id))
        .map(|p| p.id.as_str())
        .collect();
    if ours == theirs {
        return false;
    }
    servers.sort_by_key(|p| position(&p.id).unwrap_or(usize::MAX));
    true
}

/// Whether the profile has saved server and SASL passwords.
fn saved_passwords(profile: &ServerProfile, store: &CredentialStore) -> (bool, bool) {
    if !profile.remember_passwords {
        return (false, false);
    }
    let has = |key: SecretKey| store.contains(&key).unwrap_or(false);
    (
        has(profile.server_password_key()),
        has(profile.sasl_password_key()),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    Connection,
    Application,
    Appearance,
    Keyboard,
    Shortcuts,
    Notifications,
    Ircv3,
    ImageUpload,
    Credentials,
    Experimental,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FontTarget {
    MainLog,
    SubLog,
    Members,
    Channels,
    Input,
    Time,
}

/// The auto-join dialog of the selected server: one row per entry, in JOIN
/// order. Edits are written back to the form's `channels` field.
pub(crate) struct AutoJoinDialog {
    pub(crate) rows: Vec<AutoJoinRow>,
    /// Holds the keyboard focus while no row is being edited, so typing and
    /// Tab never reach the form behind the dialog.
    pub(crate) focus: FocusHandle,
}

pub(crate) struct AutoJoinRow {
    pub(crate) name: Entity<TextInput>,
    pub(crate) enabled: bool,
    /// Writes the name back while it is edited.
    _subscription: Subscription,
}

/// The color picker open under one color row of the appearance settings.
pub(crate) struct OpenColorPicker {
    /// The `#RRGGBB` field the picker edits.
    pub(crate) field: Entity<TextInput>,
    pub(crate) picker: Entity<color_picker::ColorPicker>,
    /// Keep the picker and the field in step while this exists.
    _subscriptions: Vec<Subscription>,
}

pub(crate) struct SettingsWindow {
    pub(crate) owner: WindowHandle<ChatWindow>,
    pub(crate) settings: SettingsForm,
    pub(crate) feedback: Option<String>,
    pub(crate) tab: SettingsTab,
    /// The category list on the left; Up and Down move through it.
    pub(crate) nav_focus: FocusHandle,
    nav_scroll: ScrollHandle,
    pane_scroll: ScrollHandle,
    pub(crate) font_picker: Option<FontTarget>,
    pub(crate) color_picker: Option<OpenColorPicker>,
    /// A key being recorded for a shortcut (the Shortcuts tab).
    pub(crate) shortcut_recording: Option<shortcut_settings::ShortcutRecording>,
    /// The system's font names, listed when the font list is first opened.
    fonts: Vec<String>,
    pub(crate) i18n: Localizer,
    /// Result of probing the system credential store; `None` while checking.
    pub(crate) system_store: Option<Result<(), String>>,
    /// Access token being entered to connect an image hosting account.
    pub(crate) upload_token: Entity<TextInput>,
    pub(crate) upload_connected: bool,
    pub(crate) upload_token_open: bool,
    /// What the settings file last received from this window; edits that
    /// differ from it are saved after a short pause.
    pub(crate) saved: Settings,
    window: AnyWindowHandle,
    autosave: Option<Task<()>>,
    /// Why the latest edits could not be saved, shown until they can be.
    pub(crate) autosave_error: Option<String>,
    /// Why Publish or Remove could not be started for our own avatar, or
    /// how an avatar image upload went.
    pub(crate) avatar_feedback: Option<String>,
    /// Avatar images on their way to the image host; the target is the
    /// server profile whose avatar URL draft receives the link.
    pub(crate) avatar_upload: AttachmentFlow<String>,
    /// The square selection for an avatar image, before uploading it.
    pub(crate) avatar_editor: Option<avatar_editor::AvatarEditor>,
    /// The auto-join list being edited in its own dialog.
    pub(crate) auto_join: Option<AutoJoinDialog>,
    /// An avatar image is being decoded or encoded.
    pub(crate) avatar_opening: bool,
    /// Observers of `settings`' fields; replaced with the form.
    _field_subscriptions: Vec<Subscription>,
    /// Whether the connection button was last drawn as Disconnect, so the window is
    /// redrawn only when a connection comes up or goes down.
    pub(crate) connected_shown: bool,
    /// The system's login-startup registration as last read; never stored in
    /// the settings file (#153).
    /// `None` until the first read finishes.
    pub(crate) autostart: Option<Result<autostart::AutostartStatus, String>>,
    /// A change is being made; the checkbox ignores clicks until it ends.
    pub(crate) autostart_busy: bool,
    /// Counts reads and changes, so an older answer never replaces a newer one.
    autostart_generation: u64,
    _subscriptions: Vec<Subscription>,
}

/// Pause after the last edit before settings are written.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(500);

pub(crate) fn settings_field(label: &str, input: Entity<TextInput>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(px(150.)).flex_shrink_0().child(label.to_owned()))
        .child(div().flex_1().min_w_0().child(input))
}

impl SettingsWindow {
    /// A labelled row of mutually exclusive choices stored in the settings.
    pub(crate) fn option_row<T: Copy + PartialEq + 'static, const N: usize>(
        &self,
        label_key: &'static str,
        options: [(T, &'static str); N],
        current: T,
        set: fn(&mut Settings, T),
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = theme::current(cx);
        let mut choices = div().flex().gap_1();
        for (index, (value, key)) in options.into_iter().enumerate() {
            choices = choices.child(
                div()
                    .id((label_key, index))
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(current == value, |d| d.bg(theme.selected))
                    .child(self.i18n.text(key))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        set(&mut this.settings.values, value);
                        this.feedback = None;
                        cx.notify();
                    })),
            );
        }
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(150.))
                    .flex_shrink_0()
                    .child(self.i18n.text(label_key)),
            )
            .child(choices)
    }

    pub(crate) fn select_language(
        &mut self,
        language: Language,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.values.language = language;
        self.i18n = Localizer::new(language);
        window.set_window_title(&self.i18n.text("settings_title"));
        for (field, key) in [
            (&self.settings.custom_host, "server_host_placeholder"),
            (&self.settings.nickname, "nickname"),
            (&self.settings.username, "username_placeholder"),
            (&self.settings.realname, "realname_placeholder"),
            (&self.settings.quit_message, "quit_message_placeholder"),
            (&self.settings.display_name, "display_name_placeholder"),
            (
                &self.settings.server_password,
                if self.settings.saved_server_password {
                    "password_saved_placeholder"
                } else {
                    "server_password_placeholder"
                },
            ),
            (&self.settings.sasl_username, "sasl_account_placeholder"),
            (
                &self.settings.sasl_password,
                if self.settings.saved_sasl_password {
                    "password_saved_placeholder"
                } else {
                    "sasl_password"
                },
            ),
            (&self.settings.main_log_font, "font_system_placeholder"),
            (&self.settings.sub_log_font, "font_system_placeholder"),
            (&self.settings.member_font, "font_system_placeholder"),
            (&self.settings.channel_font, "font_system_placeholder"),
            (&self.settings.input_font, "font_system_placeholder"),
            (&self.settings.time_font, "font_monospace_placeholder"),
        ] {
            let placeholder = self.i18n.text(key);
            field.update(cx, |field, cx| field.set_placeholder(&placeholder, cx));
        }
        self.feedback = None;
        cx.notify();
    }

    pub(crate) fn new(
        owner: WindowHandle<ChatWindow>,
        values: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The title given at window creation reaches X11 only as Latin-1 `WM_NAME`;
        // setting it again also writes the UTF-8 `_NET_WM_NAME` (issue #143).
        let i18n = Localizer::new(values.language);
        window.set_window_title(&i18n.text("settings_title"));
        settings_theme::refresh(cx);
        let settings = SettingsForm::new(values, &i18n, &secrets::store(cx), cx);
        window.focus(&settings.nickname.focus_handle(cx));
        let upload_token = cx.new(|cx| {
            TextInput::new_settings_field(&i18n.text("image_token_placeholder"), "", true, cx)
        });
        let mut subscriptions = vec![cx.observe_self(|this, cx| this.schedule_autosave(cx))];
        let field_subscriptions = Self::observe_fields(&settings, window, cx);
        // The connection button follows the chat window's connections. The
        // chat window opens this window from its own update, when its window
        // cannot be read yet, so subscribe once that update has returned.
        cx.defer_in(window, |this, _, cx| {
            if let Ok(chat) = this.owner.entity(cx) {
                this._subscriptions.push(cx.observe(&chat, |this, _, cx| {
                    if this.selected_connected(cx) != this.connected_shown {
                        cx.notify();
                    }
                }));
            }
        });
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            let active = window.is_window_active();
            this.window_activation_changed(active, cx);
            // Another process may write the settings while this window is in
            // the background, so leaving saves what is pending and coming
            // back reads what changed (#147).
            if active {
                this.reload_changed_settings(window, cx);
            } else {
                this.autosave_now(None, cx);
            }
        }));
        // Leaving saves everything, including a password field that still
        // has focus.
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = view.update(cx, |this, cx| this.autosave_now(None, cx));
            true
        });
        subscriptions.push(cx.on_app_quit(|this, cx| {
            this.autosave_now(None, cx);
            async {}
        }));
        let saved = settings.values.clone();
        let mut this = Self {
            owner,
            settings,
            feedback: None,
            tab: SettingsTab::Connection,
            nav_focus: cx.focus_handle().tab_stop(true),
            nav_scroll: ScrollHandle::new(),
            pane_scroll: ScrollHandle::new(),
            font_picker: None,
            color_picker: None,
            shortcut_recording: None,
            fonts: Vec::new(),
            i18n,
            system_store: None,
            upload_token,
            upload_connected: false,
            upload_token_open: false,
            saved,
            window: window.window_handle(),
            autosave: None,
            autosave_error: None,
            avatar_feedback: None,
            avatar_upload: AttachmentFlow::default(),
            avatar_editor: None,
            auto_join: None,
            avatar_opening: false,
            _field_subscriptions: field_subscriptions,
            connected_shown: false,
            autostart: None,
            autostart_busy: false,
            autostart_generation: 0,
            _subscriptions: subscriptions,
        };
        this.refresh_autostart(cx);
        this.probe_system_store(cx);
        this.refresh_upload_account(cx);
        this
    }

    /// Schedules the autosave after an edit to any text field (it leaves the
    /// server fields to Save) and refreshes the unsaved-changes note.
    fn observe_fields(
        settings: &SettingsForm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        let mut subscriptions = Vec::new();
        for field in settings.text_fields() {
            subscriptions.push(cx.observe(field, |this, _, cx| {
                this.schedule_autosave(cx);
                // Shows or hides the unsaved-changes note.
                cx.notify();
            }));
        }
        for field in [&settings.server_password, &settings.sasl_password] {
            subscriptions.push(cx.observe(field, |_, _, cx| cx.notify()));
        }
        subscriptions
    }

    /// Shows what another process wrote to the settings file while this
    /// window was in the background, so a later save does not put the older
    /// values back. Edits this window could not save yet are kept.
    /// Takes over the server order the file has, keeping every other edit
    /// in the form, so that saving cannot put back an older order.
    fn follow_saved_server_order(&mut self) {
        let Ok(Some(file)) = settings_file::load() else {
            return;
        };
        if follow_server_order(&mut self.saved.servers, &file.servers) {
            follow_server_order(&mut self.settings.values.servers, &file.servers);
        }
    }

    fn reload_changed_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_saved_server_order();
        if self.autosave.is_some() || self.autosave_error.is_some() || self.servers_unsaved(cx) {
            return;
        }
        let Ok(Some(mut loaded)) = settings_file::load() else {
            return;
        };
        if loaded == self.saved {
            return;
        }
        let previous = self.saved.clone();
        if loaded
            .profile(&self.settings.values.selected_server)
            .is_some()
        {
            loaded.selected_server = self.settings.values.selected_server.clone();
        }
        if loaded.language != self.settings.values.language {
            self.i18n = Localizer::new(loaded.language);
            window.set_window_title(&self.i18n.text("settings_title"));
        }
        self.settings = SettingsForm::new(loaded.clone(), &self.i18n, &secrets::store(cx), cx);
        self._field_subscriptions = Self::observe_fields(&self.settings, window, cx);
        let servers_changed = previous.servers != loaded.servers
            || previous.selected_server != loaded.selected_server;
        self.saved = loaded.clone();
        // The chat window follows too, so that a later save, which compares
        // with `saved`, does not find nothing to apply (#147).
        self.apply_to_chat_window(&previous, loaded, servers_changed, cx);
        self.font_picker = None;
        self.color_picker = None;
        self.auto_join = None;
        self.shortcut_recording = None;
        self.refresh_upload_account(cx);
        cx.notify();
    }

    /// Saves typed passwords to the credential store, forgets the passwords
    /// of profiles removed since `previous` (what this window last saved, not
    /// the file, which another process may have added servers to), and
    /// writes the settings file.
    fn commit_settings(
        &mut self,
        previous: &Settings,
        mut settings: Settings,
        with_servers: bool,
        cx: &mut Context<Self>,
    ) -> Result<Settings, String> {
        let store = secrets::store(cx);
        settings.servers.retain(|server| !server.host.is_empty());
        if with_servers {
            self.settings
                .persist_passwords(&settings, &store, &self.i18n, cx)?;
        }
        forget_removed_profiles(previous, &settings, &store)
            .map_err(|error| secrets::error_text(&self.i18n, &error))?;
        let previous_logging = diagnostics::configuration();
        diagnostics::configure(&settings.experimental)
            .map_err(|error| self.i18n.format("debug_log_error", &[("error", &error)]))?;
        if let Err(error) = settings_file::save(&settings) {
            let _ = diagnostics::configure(&previous_logging);
            return Err(error);
        }
        Ok(settings)
    }

    pub(crate) fn connect_from_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_saved_server_order();
        let result = (|| {
            let settings = self.settings.snapshot(cx)?;
            let config =
                self.settings
                    .connection_config(&settings, &secrets::store(cx), &self.i18n, cx)?;
            self.autosave = None;
            // `saved` follows only once the file is written, so a failed
            // Connect leaves the draft unsaved.
            let previous = self.saved.clone();
            let settings = self.commit_settings(&previous, settings, true, cx)?;
            self.saved = settings.clone();
            self.settings.values = settings.clone();
            Ok::<_, String>((config, settings))
        })();
        self.feedback = match result {
            Ok((config, settings)) => {
                let (appearance, mode, language) = (
                    settings.appearance.clone(),
                    settings.theme,
                    settings.language,
                );
                let profile = settings.selected_server.clone();
                match self.owner.update(cx, |owner, chat_window, cx| {
                    owner.apply_appearance(appearance, mode, cx);
                    owner.apply_language(language, chat_window, cx);
                    owner.apply_servers(settings, cx);
                    owner.connect_profile(&profile, config, chat_window, cx)
                }) {
                    Ok(Ok(())) => {
                        window.remove_window();
                        return;
                    }
                    Ok(Err(error)) => Some(error),
                    Err(error) => Some(
                        self.i18n
                            .format("chat_closed", &[("error", &error.to_string())]),
                    ),
                }
            }
            Err(error) => Some(error),
        };
        cx.notify();
    }

    /// The latest action's feedback, or why edits are not being saved.
    pub(crate) fn status_message(&self) -> Option<String> {
        self.feedback
            .clone()
            .or_else(|| self.autosave_error.clone())
    }

    /// Restarts the pause before edited settings are saved.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        let window = self.window;
        self.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_DELAY).await;
            let _ = cx.update_window(window, |_, window, cx| {
                this.update(cx, |this, cx| this.autosave_now(Some(window), cx))
            });
        }));
    }

    /// Writes everything except the server list when the form differs from
    /// what was last saved and applies the changes to the chat window. The
    /// servers (and typed passwords) wait for Save (D021).
    pub(crate) fn autosave_now(&mut self, _window: Option<&Window>, cx: &mut Context<Self>) {
        self.autosave = None;
        match self.save_settings(false, cx) {
            Ok(()) => {
                if self.autosave_error.take().is_some() {
                    cx.notify();
                }
            }
            Err(error) => {
                if self.autosave_error.as_ref() != Some(&error) {
                    self.autosave_error = Some(error);
                    cx.notify();
                }
            }
        }
    }

    /// Whether the server list or a typed password has edits that Save has
    /// not written yet.
    pub(crate) fn servers_unsaved(&self, cx: &App) -> bool {
        let Ok(settings) = self.settings.snapshot(cx) else {
            return true;
        };
        settings.servers != self.saved.servers
            || settings.selected_server != self.saved.selected_server
            || self.settings.pending_passwords(cx)
    }

    /// The Save button: writes the servers along with everything else.
    pub(crate) fn save_servers(&mut self, cx: &mut Context<Self>) {
        self.autosave = None;
        match self.save_settings(true, cx) {
            Ok(()) => {
                self.autosave_error = None;
                self.feedback = None;
            }
            Err(error) => self.feedback = Some(error),
        }
        cx.notify();
    }

    /// Writes the form, with the servers only when `with_servers`; without,
    /// the saved server list stays as it is. A server without a host cannot
    /// be saved.
    fn save_settings(&mut self, with_servers: bool, cx: &mut Context<Self>) -> Result<(), String> {
        self.follow_saved_server_order();
        let mut settings = self.settings.snapshot_with(cx, with_servers)?;
        if with_servers {
            if settings
                .selected_profile()
                .is_some_and(|profile| profile.host.is_empty())
            {
                return Err(self.i18n.text("server_required"));
            }
        } else {
            settings.servers = self.saved.servers.clone();
            settings.selected_server = self.saved.selected_server.clone();
        }
        if settings == self.saved && !(with_servers && self.servers_unsaved(cx)) {
            return Ok(());
        }
        let previous = std::mem::replace(&mut self.saved, settings.clone());
        let saved = match self.commit_settings(&previous, settings, with_servers, cx) {
            Ok(saved) => saved,
            Err(error) => {
                // Try again on the next edit.
                self.saved = previous;
                return Err(error);
            }
        };
        if with_servers {
            self.saved = saved.clone();
        }
        let servers_changed = previous.servers != self.saved.servers
            || previous.selected_server != self.saved.selected_server;
        self.apply_to_chat_window(&previous, saved, servers_changed, cx);
        Ok(())
    }

    /// Applies what differs between `previous` and `saved` to the chat window:
    /// shortcuts, appearance, language, layout and servers. Used for this
    /// window's own saves and for settings another process wrote.
    fn apply_to_chat_window(
        &self,
        previous: &Settings,
        saved: Settings,
        servers_changed: bool,
        cx: &mut Context<Self>,
    ) {
        let appearance_changed =
            previous.appearance != saved.appearance || previous.theme != saved.theme;
        let language_changed = previous.language != saved.language;
        let layout_changed = previous.restore_window_layout != saved.restore_window_layout;
        let restore_layout = saved.restore_window_layout;
        let shortcuts = ShortcutPrefs::from(&saved);
        let shortcuts_changed = ShortcutPrefs::from(previous) != shortcuts;
        let menu_bar_changed = previous.menu_bar_auto_hide != saved.menu_bar_auto_hide;
        let menu_bar_always = !saved.menu_bar_auto_hide;
        let provider = saved.image_upload.provider.clone();
        let rules = notification_rules(&saved.notifications);
        let _ = self.owner.update(cx, |owner, window, cx| {
            owner.image_provider = provider;
            owner.notification_rules = rules;
            if shortcuts_changed {
                apply_shortcuts(shortcuts, cx);
            }
            if menu_bar_changed {
                owner.menu_bar.set_always(menu_bar_always);
                cx.notify();
            }
            if layout_changed {
                owner.restore_layout = restore_layout;
                // Turning it on remembers where the window is shortly, in the
                // background like any other layout change.
                owner.note_window_bounds(window);
                owner.schedule_layout_save(cx);
            }
            if appearance_changed {
                owner.apply_appearance(saved.appearance.clone(), saved.theme, cx);
            }
            if language_changed {
                owner.apply_language(saved.language, window, cx);
            }
            if servers_changed {
                owner.apply_servers(saved, cx);
            }
        });
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.autosave_now(None, cx);
        window.remove_window();
    }

    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.activate_window();
    }

    fn disconnect_action(&mut self, _: &Disconnect, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.owner.update(cx, |owner, _, cx| {
            if let Some(network) = owner.selected_network_id() {
                owner.disconnect(network, cx)
            }
        });
        cx.notify();
    }

    fn reconnect_action(&mut self, _: &Reconnect, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.owner.update(cx, |owner, window, cx| {
            if let Some(network) = owner.selected_network_id() {
                owner.reconnect(network, window, cx)
            }
        });
        cx.notify();
    }

    fn toggle_debug_action(&mut self, _: &ToggleDebug, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.owner.update(cx, |owner, window, cx| {
            owner.toggle_debug(&ToggleDebug, window, cx)
        });
    }

    fn copy_diagnostics_action(
        &mut self,
        _: &CopyDiagnostics,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.owner.update(cx, |owner, window, cx| {
            owner.copy_diagnostics(&CopyDiagnostics, window, cx)
        });
    }

    pub(crate) fn toggle_remember_passwords(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = secrets::store(cx);
        let Some(profile) = self.settings.values.selected_profile().cloned() else {
            return;
        };
        if profile.remember_passwords {
            // The stored secrets are deleted when the change is saved. Take
            // the typed connection fields into `values` first so rebuilding
            // the fields does not bring back older text.
            match self.settings.snapshot(cx) {
                Ok(settings) => self.settings.values = settings,
                Err(error) => {
                    self.feedback = Some(error);
                    cx.notify();
                    return;
                }
            }
            if let Some(profile) = self.settings.values.selected_profile_mut() {
                profile.remember_passwords = false;
            }
            self.settings.drafts.remove(&profile.id);
            self.show_selected_server(cx);
            self.feedback = Some(self.i18n.text("passwords_removed"));
            cx.notify();
            return;
        }
        if store.kind() == CredentialBackendKind::System {
            // Secure storage needs no plaintext warning, but it must work.
            match secrets::system_probe(cx)() {
                Ok(()) => {
                    if let Some(profile) = self.settings.values.selected_profile_mut() {
                        profile.remember_passwords = true;
                    }
                    self.feedback = None;
                }
                Err(error) => self.feedback = Some(secrets::error_text(&self.i18n, &error)),
            }
            cx.notify();
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &self.i18n.text("password_warning_title"),
            Some(&self.i18n.format(
                "password_warning_detail",
                &[("path", &secrets::local_path_text())],
            )),
            &[
                PromptButton::ok(self.i18n.text("save_passwords")),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    if let Some(profile) = this.settings.values.selected_profile_mut() {
                        profile.remember_passwords = true;
                    }
                    this.feedback = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Sending PASS without TLS is never turned on without a confirmation.
    fn toggle_plaintext_pass(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self.settings.values.selected_profile_mut() else {
            return;
        };
        if profile.allow_plaintext_pass {
            profile.allow_plaintext_pass = false;
            cx.notify();
            return;
        }
        let id = profile.id.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            &self.i18n.text("plaintext_pass_title"),
            Some(&self.i18n.text("plaintext_pass_detail")),
            &[
                PromptButton::ok(self.i18n.text("plaintext_pass_confirm")),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    if let Some(profile) = this.settings.values.selected_profile_mut()
                        && profile.id == id
                    {
                        profile.allow_plaintext_pass = true;
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn toggle_tls(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.settings.values.selected_profile_mut() else {
            return;
        };
        if profile.use_tls && profile.sasl_enabled {
            self.feedback = Some(self.i18n.text("disable_sasl_first"));
            cx.notify();
            return;
        }
        let use_tls = !profile.use_tls;
        profile.use_tls = use_tls;
        if !use_tls {
            profile.verify_tls_certificates = true;
        }
        self.feedback = None;
        let port = self.settings.port.read(cx).text().to_owned();
        if port == "6667" && use_tls {
            self.settings
                .port
                .update(cx, |field, cx| field.set_text("6697", cx));
        } else if port == "6697" && !use_tls {
            self.settings
                .port
                .update(cx, |field, cx| field.set_text("6667", cx));
        }
        cx.notify();
    }

    fn toggle_certificate_verification(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = self.settings.values.selected_profile_mut()
            && profile.use_tls
        {
            profile.verify_tls_certificates = !profile.verify_tls_certificates;
            cx.notify();
        }
    }

    fn toggle_sasl(&mut self, cx: &mut Context<Self>) {
        self.feedback = None;
        let Some(profile) = self.settings.values.selected_profile_mut() else {
            return;
        };
        profile.sasl_enabled = !profile.sasl_enabled;
        if profile.sasl_enabled && !profile.use_tls {
            self.toggle_tls(cx);
        }
        cx.notify();
    }

    pub(crate) fn show_selected_server(&mut self, cx: &mut Context<Self>) {
        self.settings
            .show_selected(&secrets::store(cx), &self.i18n, cx);
        self.feedback = None;
        cx.notify();
    }

    pub(crate) fn select_server(&mut self, id: String, cx: &mut Context<Self>) {
        self.switch_server(move |settings| settings.selected_server = id, cx);
    }

    fn auto_join_row(&self, name: &str, enabled: bool, cx: &mut Context<Self>) -> AutoJoinRow {
        let input = cx.new(|cx| TextInput::new_settings_field("#channel", name, false, cx));
        let _subscription = cx.observe(&input, |this, _, cx| this.sync_auto_join(cx));
        AutoJoinRow {
            name: input,
            enabled,
            _subscription,
        }
    }

    /// The enabled channels in JOIN order, for the Connection tab.
    fn auto_join_summary(&self, cx: &App) -> String {
        let names: Vec<String> =
            cayenchat_storage::parse_auto_join(self.settings.channels.read(cx).text())
                .into_iter()
                .filter(|entry| entry.enabled)
                .map(|entry| entry.name)
                .collect();
        const SHOWN: usize = 5;
        match names.len() {
            0 => self.i18n.text("auto_join_none"),
            n if n > SHOWN => format!("{}, …", names[..SHOWN].join(", ")),
            _ => names.join(", "),
        }
    }

    fn render_auto_join_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.auto_join.as_ref()?;
        let theme = settings_theme::palette(cx);
        let last = dialog.rows.len().saturating_sub(1);
        let mut list = div().flex().flex_col().gap_1();
        for (index, row) in dialog.rows.iter().enumerate() {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("auto-join-enabled", index))
                            .cursor_pointer()
                            .child(settings_theme::checkbox(row.enabled, true, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_auto_join(
                                    |rows| rows[index].enabled = !rows[index].enabled,
                                    cx,
                                );
                            })),
                    )
                    .child(div().flex_1().min_w_0().child(row.name.clone()))
                    .child(
                        settings_theme::button(("auto-join-up", index), false, cx)
                            .child(self.i18n.text("auto_join_move_up"))
                            .when(index == 0, |d| d.opacity(0.4))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if index > 0 {
                                    this.edit_auto_join(|rows| rows.swap(index - 1, index), cx);
                                }
                            })),
                    )
                    .child(
                        settings_theme::button(("auto-join-down", index), false, cx)
                            .child(self.i18n.text("auto_join_move_down"))
                            .when(index == last, |d| d.opacity(0.4))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_auto_join(
                                    |rows| {
                                        if index + 1 < rows.len() {
                                            rows.swap(index, index + 1);
                                        }
                                    },
                                    cx,
                                );
                            })),
                    )
                    .child(
                        settings_theme::button(("auto-join-delete", index), false, cx)
                            .child(self.i18n.text("auto_join_delete"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_auto_join(
                                    |rows| {
                                        rows.remove(index);
                                    },
                                    cx,
                                );
                                if let Some(dialog) = &this.auto_join {
                                    window.focus(&dialog.focus);
                                }
                            })),
                    ),
            );
        }
        let panel = div()
            .id("auto-join-dialog")
            .track_focus(&dialog.focus)
            .on_action(cx.listener(|this, _: &FocusNextField, window, cx| {
                this.auto_join_traverse(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousField, window, cx| {
                this.auto_join_traverse(false, window, cx)
            }))
            .occlude()
            .w(px(560.))
            .max_w_full()
            .max_h_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("auto_join_title")),
            )
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("auto_join_hint")),
            )
            .child(
                div()
                    .id("auto-join-list")
                    .flex_1()
                    .min_h_0()
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .child(list),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(
                        settings_theme::button("auto-join-add", false, cx)
                            .child(self.i18n.text("auto_join_add"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_auto_join_row(window, cx)
                            })),
                    )
                    .child(
                        settings_theme::button("auto-join-done", true, cx)
                            .child(self.i18n.text("auto_join_done"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_auto_join(window, cx)),
                            ),
                    ),
            );
        Some(
            div()
                .id("auto-join-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .bg(gpui::black().opacity(0.4))
                .child(panel)
                .into_any_element(),
        )
    }

    pub(crate) fn open_auto_join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = cayenchat_storage::parse_auto_join(self.settings.channels.read(cx).text());
        let rows = entries
            .iter()
            .map(|entry| self.auto_join_row(&entry.name, entry.enabled, cx))
            .collect();
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.auto_join = Some(AutoJoinDialog { rows, focus });
        cx.notify();
    }

    fn close_auto_join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.auto_join = None;
        window.focus(&self.nav_focus);
        cx.notify();
    }

    /// Tab / Shift+Tab inside the dialog: moves between its fields and wraps
    /// back to the dialog instead of leaving it.
    fn auto_join_traverse(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.auto_join else {
            return;
        };
        if forward {
            window.focus_next();
        } else {
            window.focus_prev();
        }
        if !dialog.focus.contains_focused(window, cx) {
            window.focus(&dialog.focus);
        }
    }

    /// Stores the dialog's rows in the `channels` field, which is saved with Save.
    fn sync_auto_join(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &self.auto_join else {
            return;
        };
        let entries: Vec<AutoJoinEntry> = dialog
            .rows
            .iter()
            .map(|row| AutoJoinEntry {
                name: row.name.read(cx).text().to_owned(),
                enabled: row.enabled,
            })
            .collect();
        let text = cayenchat_storage::format_auto_join(&entries);
        self.settings
            .channels
            .update(cx, |field, cx| field.set_text(&text, cx));
        cx.notify();
    }

    pub(crate) fn edit_auto_join(
        &mut self,
        change: impl FnOnce(&mut Vec<AutoJoinRow>),
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.auto_join {
            change(&mut dialog.rows);
        }
        self.sync_auto_join(cx);
    }

    fn add_auto_join_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.auto_join_row("", true, cx);
        let input = row.name.clone();
        self.edit_auto_join(|rows| rows.push(row), cx);
        window.focus(&input.read(cx).focus_handle(cx));
    }

    /// Adds a server, blank or filled in from a preset's `host`.
    fn choose_preset(&mut self, host: &str, cx: &mut Context<Self>) {
        self.settings
            .custom_host
            .update(cx, |field, cx| field.set_text(host, cx));
        self.settings
            .port
            .update(cx, |field, cx| field.set_text("6667", cx));
        self.settings.server_list_open = false;
        cx.notify();
    }

    fn add_server(&mut self, host: &str, cx: &mut Context<Self>) {
        let host = host.to_owned();
        self.switch_server(
            move |settings| {
                settings.add_server(&host);
            },
            cx,
        );
    }

    fn switch_server(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        self.avatar_feedback = None;
        self.avatar_editor = None;
        self.auto_join = None;
        let store = secrets::store(cx);
        match self.settings.switch_server(change, &store, &self.i18n, cx) {
            Ok(()) => self.feedback = None,
            Err(error) => self.feedback = Some(error),
        }
        cx.notify();
    }

    /// Removes the selected server. Saving the removal disconnects it and
    /// forgets its passwords, so a saved server asks first.
    fn remove_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.settings.values.selected_server.clone();
        let Some(profile) = self.saved.profile(&id).filter(|p| !p.host.is_empty()) else {
            self.settings.values.remove_selected_server();
            self.show_selected_server(cx);
            return;
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &self
                .i18n
                .format("remove_server_title", &[("host", &profile.host)]),
            Some(&self.i18n.text("remove_server_detail")),
            &[
                PromptButton::ok(self.i18n.text("remove_server_confirm")),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    if this.settings.values.selected_server == id {
                        this.settings.values.remove_selected_server();
                        this.show_selected_server(cx);
                    }
                });
            }
        })
        .detach();
    }

    /// Whether the server being edited is connected, being connected or
    /// waiting to retry.
    fn selected_connected(&self, cx: &App) -> bool {
        let profile = &self.settings.values.selected_server;
        self.owner.read(cx).is_ok_and(|chat| {
            chat.network_of_profile(profile)
                .is_some_and(|network| chat.can_disconnect(network))
        })
    }

    fn render_connection_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let connected = self.selected_connected(cx);
        self.connected_shown = connected;
        let profile = self.settings.values.selected_profile().cloned();
        let no_server = profile.is_none();
        // A server not in the saved list is being added; it is saved with Save.
        let adding = profile
            .as_ref()
            .is_some_and(|profile| self.saved.profile(&profile.id).is_none());
        let unsaved = self.servers_unsaved(cx);
        let selected_id = profile.as_ref().map(|profile| profile.id.clone());
        let server_fields = profile
            .as_ref()
            .map(|profile| self.render_server_fields(profile, cx));
        let current_host = self.settings.custom_host.read(cx).text().trim().to_owned();
        let current_port = self.settings.port.read(cx).text().trim().to_owned();
        let selected_label = if no_server {
            self.i18n.text("no_servers")
        } else if current_host.is_empty() {
            self.i18n.text("new_server")
        } else {
            format!("{current_host}:{current_port}")
        };
        let mut server_selector = div().flex().flex_col().child(
            div()
                .id("server-select")
                .px_2()
                .py_1()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(format!("{selected_label}  ▾"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings.server_list_open = !this.settings.server_list_open;
                    this.settings.encoding_list_open = false;
                    this.settings.language_list_open = false;
                    cx.notify();
                })),
        );
        if self.settings.server_list_open {
            let mut menu = div()
                .id("server-menu")
                .max_h(px(260.))
                .overflow_y_scroll()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface);
            if adding {
                // Adding offers only suggestions; existing servers stay out
                // of the way so the list cannot be mistaken for editing one.
                for (index, preset) in cayenchat_storage::PRESETS.iter().enumerate() {
                    menu = menu.child(
                        div()
                            .id(("preset-option", index))
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.hover))
                            .when(preset.host == current_host, |d| d.bg(theme.selected))
                            .child(self.i18n.format(
                                "preset_option",
                                &[("name", preset.name), ("host", preset.host)],
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_preset(preset.host, cx)
                            })),
                    );
                }
            } else {
                for (index, server) in self.saved.ordered_servers().enumerate() {
                    let id = server.id.clone();
                    let label = format!("{}:{}", server.host, server.port);
                    menu = menu.child(
                        div()
                            .id(("server-option", index))
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.hover))
                            .when(Some(&id) == selected_id.as_ref(), |d| d.bg(theme.selected))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_server(id.clone(), cx)
                            })),
                    );
                }
                menu = menu.child(
                    div()
                        .id("add-server-option")
                        .px_2()
                        .py_1()
                        .when(!no_server, |d| d.border_t_1().border_color(theme.border))
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .child(self.i18n.text("add_server"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.add_server(cayenchat_storage::PRESETS[0].host, cx)
                        })),
                );
            }
            server_selector = server_selector.child(menu);
        }
        account_settings::panel(cx)
            .child(self.tab_heading("connection"))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text(if adding {
                        "adding_server_intro"
                    } else {
                        "connection_intro"
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("destination")),
                    )
                    .child(server_selector.flex_1().min_w_0()),
            )
            .when_some(server_fields, |d, fields| d.child(fields))
            .when(no_server, |d| {
                d.child(
                    div()
                        .ml(px(158.))
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("no_servers_hint")),
                )
            })
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .when(unsaved, |d| {
                d.child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("unsaved_server_changes")),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .child(
                        settings_theme::button("back-button", false, cx)
                            .debug_selector(|| "back-button".into())
                            .child(self.i18n.text("back"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    // Removing the last server needs Save to take effect.
                    .when(!no_server || unsaved, |d| {
                        d.child(
                            settings_theme::button("save-button", false, cx)
                                .debug_selector(|| "save-button".into())
                                .child(self.i18n.text("save"))
                                .on_click(cx.listener(|this, _, _, cx| this.save_servers(cx))),
                        )
                    })
                    .when(!no_server, |d| {
                        // One button in one place: Disconnect while the server
                        // being edited is connected, being connected or
                        // waiting to retry, Connect otherwise.
                        d.child(
                            settings_theme::button("connection-button", !connected, cx)
                                .debug_selector(|| "connection-button".into())
                                .child(self.i18n.text(if connected {
                                    "disconnect"
                                } else {
                                    "connect"
                                }))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if !connected {
                                        this.connect_from_settings(window, cx);
                                        return;
                                    }
                                    // Disconnects the server being edited.
                                    let profile = this.settings.values.selected_server.clone();
                                    let _ = this.owner.update(cx, |owner, _, cx| {
                                        if let Some(network) = owner.network_of_profile(&profile) {
                                            owner.disconnect(network, cx);
                                        }
                                    });
                                    cx.notify();
                                })),
                        )
                    }),
            )
    }

    /// Everything on the Connection tab that belongs to `profile`.
    fn render_server_fields(&mut self, profile: &ServerProfile, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let tls = profile.use_tls;
        let sasl = profile.sasl_enabled;
        let mut encoding_selector = div().flex().flex_col().child(
            div()
                .id("encoding-select")
                .px_2()
                .py_1()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(format!("{}  ▾", profile.encoding.label()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings.encoding_list_open = !this.settings.encoding_list_open;
                    this.settings.server_list_open = false;
                    this.settings.language_list_open = false;
                    cx.notify();
                })),
        );
        if self.settings.encoding_list_open {
            let mut menu = div()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface);
            for (index, encoding) in TextEncoding::ALL.into_iter().enumerate() {
                menu = menu.child(
                    div()
                        .id(("encoding-option", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .when(profile.encoding == encoding, |d| d.bg(theme.selected))
                        .child(encoding.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(profile) = this.settings.values.selected_profile_mut() {
                                profile.encoding = encoding;
                            }
                            this.settings.encoding_list_open = false;
                            cx.notify();
                        })),
                );
            }
            encoding_selector = encoding_selector.child(menu);
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(settings_field(
                &self.i18n.text("display_name"),
                self.settings.display_name.clone(),
            ))
            .child(settings_field(
                &self.i18n.text("host"),
                self.settings.custom_host.clone(),
            ))
            .child(
                div()
                    .id("remove-server")
                    .ml(px(158.))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .text_color(theme.warning)
                    .child(
                        self.i18n
                            .text(if self.saved.profile(&profile.id).is_none() {
                                "cancel_add_server"
                            } else {
                                "remove_server"
                            }),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.remove_server(window, cx))),
            )
            .child(settings_field(
                &self.i18n.text("port"),
                self.settings.port.clone(),
            ))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("encoding")),
                    )
                    .child(encoding_selector.flex_1().min_w_0()),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("encoding_hint")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(150.)).child("TLS/SSL"))
                    .child(
                        div()
                            .id("tls-toggle")
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .child(settings_theme::checkbox(tls, true, cx))
                            .child(self.i18n.text(if tls { "on" } else { "off" }))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_tls(cx))),
                    ),
            )
            .when(tls, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(150.))
                                .child(self.i18n.text("verify_certificates")),
                        )
                        .child(
                            div()
                                .id("certificate-verification-toggle")
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .child(settings_theme::checkbox(
                                    profile.verify_tls_certificates,
                                    true,
                                    cx,
                                ))
                                .child(self.i18n.text(if profile.verify_tls_certificates {
                                    "on"
                                } else {
                                    "off"
                                }))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_certificate_verification(cx)
                                })),
                        ),
                )
                .when(!profile.verify_tls_certificates, |d| {
                    d.child(
                        div()
                            .ml(px(158.))
                            .text_color(theme.warning)
                            .child(self.i18n.text("certificate_warning")),
                    )
                })
            })
            .child(settings_field(
                &self.i18n.text("nickname"),
                self.settings.nickname.clone(),
            ))
            .child(settings_field(
                &self.i18n.text("username"),
                self.settings.username.clone(),
            ))
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("username_hint")),
            )
            .child(settings_field(
                &self.i18n.text("realname"),
                self.settings.realname.clone(),
            ))
            .child(settings_field(
                &self.i18n.text("quit_message"),
                self.settings.quit_message.clone(),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("auto_join_channels")),
                    )
                    .child(
                        div()
                            .id("auto-join-summary")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(self.auto_join_summary(cx)),
                    )
                    .child(
                        settings_theme::button("auto-join-edit", false, cx)
                            .child(self.i18n.text("auto_join_edit"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_auto_join(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .id("connect-on-startup")
                    .ml(px(158.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        profile.connect_on_startup,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("connect_on_startup"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(profile) = this.settings.values.selected_profile_mut() {
                            profile.connect_on_startup = !profile.connect_on_startup;
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("server_auth_heading")),
            )
            .child(settings_field(
                &self.i18n.text("server_password"),
                self.settings.server_password.clone(),
            ))
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("server_password_hint")),
            )
            .when(!tls, |d| {
                d.child(
                    div()
                        .id("plaintext-pass")
                        .ml(px(158.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .child(settings_theme::checkbox(
                            profile.allow_plaintext_pass,
                            true,
                            cx,
                        ))
                        .child(self.i18n.text("plaintext_pass"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_plaintext_pass(window, cx)
                        })),
                )
                .when(profile.allow_plaintext_pass, |d| {
                    d.child(
                        div()
                            .ml(px(158.))
                            .text_color(theme.warning)
                            .child(self.i18n.text("plaintext_pass_warning")),
                    )
                })
            })
            .child(
                div()
                    .id("remember-passwords")
                    .ml(px(158.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        profile.remember_passwords,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("remember_passwords"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_remember_passwords(window, cx)
                    })),
            )
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("sasl_heading")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(150.)).child("SASL PLAIN"))
                    .child(
                        div()
                            .id("sasl-toggle")
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .child(settings_theme::checkbox(sasl, true, cx))
                            .child(self.i18n.text(if sasl { "on" } else { "off" }))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sasl(cx))),
                    ),
            )
            .when(sasl, |d| {
                d.child(settings_field(
                    &self.i18n.text("sasl_account"),
                    self.settings.sasl_username.clone(),
                ))
                .child(settings_field(
                    &self.i18n.text("sasl_password"),
                    self.settings.sasl_password.clone(),
                ))
            })
    }

    fn font_input(&self, target: FontTarget) -> Entity<TextInput> {
        match target {
            FontTarget::MainLog => self.settings.main_log_font.clone(),
            FontTarget::SubLog => self.settings.sub_log_font.clone(),
            FontTarget::Members => self.settings.member_font.clone(),
            FontTarget::Channels => self.settings.channel_font.clone(),
            FontTarget::Input => self.settings.input_font.clone(),
            FontTarget::Time => self.settings.time_font.clone(),
        }
    }

    /// One `#RRGGBB` field with a swatch that opens the color picker for it.
    fn color_input(&self, input: &Entity<TextInput>, cx: &mut Context<Self>) -> Div {
        let theme = theme::current(cx);
        let swatch = color_value(input.read(cx).text()).unwrap_or(0xffffff);
        let open = self
            .color_picker
            .as_ref()
            .is_some_and(|open| open.field == *input);
        let target = input.clone();
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w_0().child(input.clone()))
            .child(
                div()
                    .id(("color-swatch", input.entity_id().as_u64() as usize))
                    .w(px(24.))
                    .h(px(24.))
                    .flex_shrink_0()
                    .border_1()
                    .border_color(if open { theme.text } else { theme.border })
                    .bg(rgb(swatch))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_color_picker(&target, cx);
                    })),
            )
    }

    /// A color setting with its light-theme and dark-theme values side by
    /// side; the picker for either opens below the row.
    fn color_pair(
        &self,
        label: &str,
        light: &Entity<TextInput>,
        dark: &Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> Div {
        let picker_here = self
            .color_picker
            .as_ref()
            .is_some_and(|open| open.field == *light || open.field == *dark);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(150.)).flex_shrink_0().child(label.to_owned()))
                    .child(self.color_input(light, cx))
                    .child(self.color_input(dark, cx)),
            )
            .when(picker_here, |d| d.child(self.render_color_panel(cx)))
    }

    /// Opens the color picker for `field`, or closes it when it is open for
    /// that field already.
    pub(crate) fn toggle_color_picker(
        &mut self,
        field: &Entity<TextInput>,
        cx: &mut Context<Self>,
    ) {
        if self
            .color_picker
            .as_ref()
            .is_some_and(|open| open.field == *field)
        {
            self.color_picker = None;
            cx.notify();
            return;
        }
        let initial = color_value(field.read(cx).text()).unwrap_or(0xFFFFFF);
        // The row's own field shows the color as text, so the picker's is
        // not drawn a second time.
        let picker = cx.new(|cx| color_picker::ColorPicker::new(initial, cx).without_hex_field());
        // What is picked goes into the field; what is typed in the field
        // moves the picker.
        let into_field = cx.subscribe(
            &picker,
            |this, _, event: &color_picker::ColorChanged, cx| {
                if let Some(open) = &this.color_picker {
                    let text = color_picker::format_hex(event.0);
                    open.field.update(cx, |field, cx| field.set_text(&text, cx));
                }
            },
        );
        let into_picker = cx.observe(field, |this, field, cx| {
            if let Some(open) = &this.color_picker
                && let Some(color) = color_picker::parse_hex(field.read(cx).text())
                && open.field == field
                && open.picker.read(cx).color() != color
            {
                open.picker
                    .update(cx, |picker, cx| picker.set_color(color, cx));
            }
        });
        self.color_picker = Some(OpenColorPicker {
            field: field.clone(),
            picker,
            _subscriptions: vec![into_field, into_picker],
        });
        cx.notify();
    }

    /// Puts a palette color into the open field and picker.
    pub(crate) fn apply_palette_color(&mut self, color: &str, cx: &mut Context<Self>) {
        let (Some(open), Some(rgb)) = (&self.color_picker, color_value(color)) else {
            return;
        };
        open.picker
            .update(cx, |picker, cx| picker.set_color(rgb, cx));
        open.field.update(cx, |field, cx| field.set_text(color, cx));
    }

    /// The picker and the saved palette, under the row being edited.
    fn render_color_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let Some(open) = &self.color_picker else {
            return div();
        };
        let current = color_picker::format_hex(open.picker.read(cx).color());
        let saved = &self.settings.values.appearance.saved_colors;
        let can_save =
            saved.len() < cayenchat_storage::MAX_SAVED_COLORS && !saved.contains(&current);
        let mut palette = div().flex().flex_wrap().gap_1().w(px(220.));
        for (index, color) in saved.iter().enumerate() {
            let Some(rgb) = color_value(color) else {
                continue;
            };
            let (apply, remove) = (color.clone(), color.clone());
            let hint: SharedString = self.i18n.text("palette_remove_hint").into();
            palette = palette.child(
                div()
                    .id(("palette-color", index))
                    .size(px(22.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(gpui::rgb(rgb))
                    .cursor_pointer()
                    .tooltip(move |_, cx| {
                        let text = hint.clone();
                        cx.new(|_| ircv3_settings::TextTooltip(text)).into()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.apply_palette_color(&apply, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.settings.values.appearance.remove_saved_color(&remove);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            );
        }
        div()
            .ml(px(158.))
            .flex()
            .gap_4()
            .child(open.picker.clone())
            .child(
                div().flex().flex_col().gap_2().child(palette).child(
                    settings_theme::button("palette-save", false, cx)
                        .when(!can_save, |button| button.opacity(0.5).cursor_default())
                        .child(self.i18n.text("palette_save"))
                        .when(can_save, |button| {
                            button.on_click(cx.listener(move |this, _, _, cx| {
                                let color = this.color_picker.as_ref().map(|open| {
                                    color_picker::format_hex(open.picker.read(cx).color())
                                });
                                if let Some(color) = color {
                                    let _ = this.settings.values.appearance.save_color(&color);
                                }
                                cx.notify();
                            }))
                        }),
                ),
            )
    }

    fn font_field(&self, target: FontTarget, label: &str, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let input = self.font_input(target);
        let mut field = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(150.)).flex_shrink_0().child(label.to_owned()))
                .child(div().flex_1().min_w_0().child(input.clone()))
                .child(
                    div()
                        .id(("font-picker", target as u32))
                        .px_2()
                        .py_1()
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .child(self.i18n.text("choose_font"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.font_picker = if this.font_picker == Some(target) {
                                None
                            } else {
                                Some(target)
                            };
                            // Listing the system's fonts takes a while (many
                            // seconds in a debug build), so it waits until a
                            // list is wanted instead of slowing every opening
                            // of the settings window.
                            if this.font_picker.is_some() && this.fonts.is_empty() {
                                let mut fonts = window.text_system().all_font_names();
                                fonts.sort_unstable();
                                fonts.dedup();
                                this.fonts = fonts;
                            }
                            cx.notify();
                        })),
                ),
        );
        if self.font_picker == Some(target) {
            let query = input.read(cx).text().trim().to_lowercase();
            let mut choices = div()
                .id(("font-choices", target as u32))
                .ml(px(158.))
                .max_h(px(170.))
                .overflow_y_scroll()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface);
            choices = choices.child(
                div()
                    .id(("font-system", target as u32))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .child(self.i18n.text(if target == FontTarget::Time {
                        "default_monospace"
                    } else {
                        "system_default"
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.font_input(target)
                            .update(cx, |field, cx| field.set_text("", cx));
                        this.font_picker = None;
                        cx.notify();
                    })),
            );
            for (index, name) in self
                .fonts
                .iter()
                .filter(|name| name.to_lowercase().contains(&query))
                .enumerate()
            {
                let name = name.clone();
                choices = choices.child(
                    div()
                        .id(("font-choice", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .child(name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.font_input(target)
                                .update(cx, |field, cx| field.set_text(&name, cx));
                            this.font_picker = None;
                            cx.notify();
                        })),
                );
            }
            field = field.child(choices);
        }
        field
    }

    fn render_appearance_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        account_settings::panel(cx)
            .child(self.tab_heading("appearance"))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("appearance_intro")),
            )
            .child(self.option_row(
                "theme",
                [
                    (ThemeMode::System, "theme_system"),
                    (ThemeMode::Light, "theme_light"),
                    (ThemeMode::Dark, "theme_dark"),
                ],
                self.settings.values.theme,
                |settings, mode| settings.theme = mode,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().w(px(150.)).flex_shrink_0())
                    .child(div().flex_1().child(self.i18n.text("colors_light")))
                    .child(div().flex_1().child(self.i18n.text("colors_dark"))),
            )
            .child(self.color_pair(
                &self.i18n.text("member_list_background"),
                &self.settings.member_list_background,
                &self.settings.dark_member_list_background,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("channel_log"),
                &self.settings.main_log_background,
                &self.settings.dark_main_log_background,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("channel_log_alternate"),
                &self.settings.main_log_alternate,
                &self.settings.dark_main_log_alternate,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("channel_event_color"),
                &self.settings.channel_event_color,
                &self.settings.dark_channel_event_color,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("notice_color"),
                &self.settings.notice_color,
                &self.settings.dark_notice_color,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("highlight_color"),
                &self.settings.highlight_color,
                &self.settings.dark_highlight_color,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("combined_log"),
                &self.settings.sub_log_background,
                &self.settings.dark_sub_log_background,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("combined_log_alternate"),
                &self.settings.sub_log_alternate,
                &self.settings.dark_sub_log_alternate,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("url_tooltip_color"),
                &self.settings.url_tooltip_color,
                &self.settings.dark_url_tooltip_color,
                cx,
            ))
            .child(settings_field(
                &self.i18n.text("url_tooltip_opacity"),
                self.settings.url_tooltip_opacity.clone(),
            ))
            .child(
                div()
                    .id("alternate-rows")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.appearance.alternate_rows,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("alternate_rows"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.alternate_rows;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(self.font_field(FontTarget::MainLog, &self.i18n.text("channel_log"), cx))
            .child(self.font_field(FontTarget::SubLog, &self.i18n.text("combined_log"), cx))
            .child(self.font_field(FontTarget::Members, &self.i18n.text("member_list"), cx))
            .child(self.font_field(FontTarget::Channels, &self.i18n.text("channel_list"), cx))
            .child(self.font_field(FontTarget::Input, &self.i18n.text("draft_input"), cx))
            .child(self.font_field(FontTarget::Time, &self.i18n.text("timestamp_monospace"), cx))
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(self.reset_footer(SettingsTab::Appearance, cx))
    }

    /// Channel-number and draft-editing keys; only Windows and Linux have
    /// choices here, so macOS hides this tab.
    fn render_keyboard_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let hint = |key: &str| {
            div()
                .ml(px(158.))
                .text_color(theme.text_secondary)
                .child(self.i18n.text(key))
        };
        account_settings::panel(cx)
            .child(self.tab_heading("keyboard"))
            .child(self.option_row(
                "channel_number_modifier",
                [
                    (ChannelNumberModifier::Ctrl, "channel_number_modifier_ctrl"),
                    (ChannelNumberModifier::Alt, "channel_number_modifier_alt"),
                    (
                        ChannelNumberModifier::Super,
                        "channel_number_modifier_super",
                    ),
                ],
                self.settings.values.channel_number_modifier,
                |settings, modifier| settings.channel_number_modifier = modifier,
                cx,
            ))
            .child(hint("channel_number_modifier_hint"))
            .when(cfg!(target_os = "linux"), |d| {
                d.child(self.option_row(
                    "text_key_theme",
                    [
                        (TextKeyTheme::Auto, "text_key_theme_auto"),
                        (TextKeyTheme::Standard, "text_key_theme_standard"),
                        (TextKeyTheme::Emacs, "text_key_theme_emacs"),
                    ],
                    self.settings.values.text_key_theme,
                    |settings, keys| settings.text_key_theme = keys,
                    cx,
                ))
                .child(hint("text_key_theme_hint"))
            })
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(self.reset_footer(SettingsTab::Keyboard, cx))
    }

    fn notification_toggle(
        &self,
        id: &'static str,
        label_key: &str,
        get: fn(&Notifications) -> bool,
        toggle: fn(&mut Notifications),
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = settings_theme::palette(cx);
        div()
            .id(id)
            .ml(px(158.))
            .flex()
            .items_center()
            .gap_2()
            .when(!enabled, |d| d.text_color(theme.text_secondary))
            .when(enabled, |d| d.cursor_pointer())
            .child(settings_theme::checkbox(
                get(&self.settings.values.notifications),
                enabled,
                cx,
            ))
            .child(self.i18n.text(label_key))
            .when(enabled, |d| {
                d.on_click(cx.listener(move |this, _, _, cx| {
                    toggle(&mut this.settings.values.notifications);
                    cx.notify();
                }))
            })
    }

    /// Desktop notification preferences, shown through the operating
    /// system's notification service.
    fn render_notification_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let enabled = self.settings.values.notifications.enabled;
        let hint = |key: &str| {
            div()
                .ml(px(158.))
                .text_color(theme.text_secondary)
                .child(self.i18n.text(key))
        };
        account_settings::panel(cx)
            .child(self.tab_heading("notifications_tab"))
            .child(self.notification_toggle(
                "notifications-enabled",
                "notifications_enabled",
                |n| n.enabled,
                |n| n.enabled = !n.enabled,
                true,
                cx,
            ))
            .child(self.notification_toggle(
                "notify-mentions",
                "notify_mentions",
                |n| n.mentions,
                |n| n.mentions = !n.mentions,
                enabled,
                cx,
            ))
            .child(self.notification_toggle(
                "notify-private-messages",
                "notify_private_messages",
                |n| n.private_messages,
                |n| n.private_messages = !n.private_messages,
                enabled,
                cx,
            ))
            .child(self.notification_toggle(
                "notify-keywords",
                "notify_keywords",
                |n| n.keyword_alerts,
                |n| n.keyword_alerts = !n.keyword_alerts,
                enabled,
                cx,
            ))
            .child(settings_field(
                &self.i18n.text("keywords"),
                self.settings.keywords.clone(),
            ))
            .child(hint("keywords_hint"))
            .when(cfg!(target_os = "windows"), |d| {
                d.child(self.notification_toggle(
                    "notify-sound",
                    "notify_sound",
                    |n| n.sound,
                    |n| n.sound = !n.sound,
                    enabled,
                    cx,
                ))
            })
            .child(hint(if cfg!(target_os = "macos") {
                "notifications_hint_macos"
            } else {
                "notifications_hint"
            }))
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(self.reset_footer(SettingsTab::Notifications, cx))
    }

    fn settings_tab(
        &self,
        tab: SettingsTab,
        id: &'static str,
        label_key: &str,
        nav_focused: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = settings_theme::palette(cx);
        let selected = self.tab == tab;
        div()
            .id(id)
            .debug_selector(move || id.into())
            .w_full()
            .px_3()
            .py_2()
            .border_l_2()
            // The indicator takes the link color while Up/Down act on the list.
            .border_color(if selected && nav_focused {
                theme.link.into()
            } else if selected {
                theme.text.into()
            } else {
                gpui::transparent_black()
            })
            .whitespace_nowrap()
            .cursor_pointer()
            .when(selected, |d| {
                d.bg(theme.selected).font_weight(FontWeight::BOLD)
            })
            .when(!selected, |d| d.hover(|d| d.bg(theme.hover)))
            .child(self.i18n.text(label_key))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_tab(tab, window, cx);
            }))
    }

    /// Shows `tab` from the category list, which keeps the focus so Up and
    /// Down continue from it.
    fn select_tab(&mut self, tab: SettingsTab, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tab(tab, cx);
        self.font_picker = None;
        window.focus(&self.nav_focus);
        cx.notify();
    }

    /// The categories in list order; the last, Experimental, stands apart.
    fn settings_tabs() -> Vec<(SettingsTab, &'static str, &'static str)> {
        let mut tabs = vec![
            (SettingsTab::Connection, "connection-tab", "connection"),
            (
                SettingsTab::Application,
                "application-tab",
                "application_tab",
            ),
            (SettingsTab::Appearance, "appearance-tab", "appearance"),
        ];
        if !cfg!(target_os = "macos") {
            tabs.push((SettingsTab::Keyboard, "keyboard-tab", "keyboard"));
        }
        tabs.extend([
            (SettingsTab::Shortcuts, "shortcuts-tab", "shortcuts_tab"),
            (
                SettingsTab::Notifications,
                "notifications-tab",
                "notifications_tab",
            ),
            (SettingsTab::Ircv3, "ircv3-tab", "ircv3_tab"),
            (
                SettingsTab::ImageUpload,
                "image-upload-tab",
                "image_upload_tab",
            ),
            (
                SettingsTab::Credentials,
                "credentials-tab",
                "credentials_tab",
            ),
            (
                SettingsTab::Experimental,
                "experimental-tab",
                "experimental_tab",
            ),
        ]);
        tabs
    }

    /// Up and Down move through the categories while the list has focus.
    fn nav_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A key being recorded for a shortcut belongs to the recording.
        if self.shortcut_recording.is_some() || event.keystroke.modifiers.modified() {
            return;
        }
        let step: isize = match event.keystroke.key.as_str() {
            "up" => -1,
            "down" => 1,
            _ => return,
        };
        let tabs = Self::settings_tabs();
        let Some(current) = tabs.iter().position(|(tab, ..)| *tab == self.tab) else {
            return;
        };
        if let Some((tab, ..)) = tabs.get(current.wrapping_add_signed(step)) {
            self.select_tab(*tab, window, cx);
            cx.stop_propagation();
        }
    }

    /// Switches tabs. A message about something done on one tab (such as
    /// connecting an image upload account) is not shown on the others;
    /// an autosave failure still is.
    pub(crate) fn show_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.feedback = None;
            self.avatar_feedback = None;
            // A key being recorded does not wait on another tab.
            self.shortcut_recording = None;
            // A picker open under a color row does not wait for a return.
            self.color_picker = None;
        }
        // The system may have changed the registration since it was read.
        self.refresh_autostart(cx);
        self.tab = tab;
    }

    /// Reads the system's registration without blocking the window. Skipped
    /// while a change is under way: its own answer follows.
    pub(crate) fn refresh_autostart(&mut self, cx: &mut Context<Self>) {
        if self.autostart_busy {
            return;
        }
        self.autostart_generation += 1;
        let generation = self.autostart_generation;
        let read = cx
            .background_executor()
            .spawn(async { autostart::status() });
        cx.spawn(async move |this, cx| {
            let status = read.await;
            let _ = this.update(cx, |this, cx| {
                if this.autostart_generation == generation {
                    this.autostart = Some(status);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Registers or removes the login startup entry. The checkbox only
    /// follows what the system reports afterwards, so a failure leaves it
    /// matching the real state.
    pub(crate) fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        if self.autostart_busy {
            return;
        }
        let turn_on = !self
            .autostart
            .as_ref()
            .is_some_and(|status| status.as_ref().is_ok_and(|status| status.is_on()));
        self.autostart_busy = true;
        self.autostart_generation += 1;
        let generation = self.autostart_generation;
        let change = cx.background_executor().spawn(async move {
            let result = if turn_on {
                autostart::enable()
            } else {
                autostart::disable()
            };
            (result, autostart::status())
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (result, status) = change.await;
            let _ = this.update(cx, |this, cx| {
                this.autostart_busy = false;
                if this.autostart_generation == generation {
                    this.autostart = Some(status);
                }
                this.feedback = result
                    .err()
                    .map(|error| this.i18n.format("autostart_error", &[("error", &error)]));
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_autostart_toggle(&self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let status = self
            .autostart
            .as_ref()
            .and_then(|status| status.as_ref().ok())
            .copied();
        let usable = !self.autostart_busy
            && status.is_some_and(|status| status != autostart::AutostartStatus::Unavailable);
        let note = match &self.autostart {
            Some(Ok(autostart::AutostartStatus::DisabledByUser)) => {
                Some(if autostart::reenable_in_system_settings() {
                    "autostart_disabled_in_system"
                } else {
                    "autostart_disabled_by_user"
                })
            }
            Some(Ok(autostart::AutostartStatus::Unavailable)) => Some("autostart_unavailable"),
            _ => None,
        };
        let read_error = self
            .autostart
            .as_ref()
            .and_then(|status| status.as_ref().err());
        let mut toggle = div()
            .id("autostart")
            .debug_selector(|| "autostart".into())
            .flex()
            .items_center()
            .gap_2()
            .child(settings_theme::checkbox(
                status.is_some_and(autostart::AutostartStatus::is_on),
                usable,
                cx,
            ))
            .child(self.i18n.text("autostart"));
        if usable {
            toggle = toggle
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_autostart(cx)));
        }
        div()
            .ml(px(158.))
            .flex()
            .flex_col()
            .child(toggle)
            .when_some(note, |d, key| {
                d.child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text(key)),
                )
            })
            .when_some(read_error, |d, error| {
                d.child(
                    div()
                        .text_color(theme.warning)
                        .child(self.i18n.format("autostart_error", &[("error", error)])),
                )
            })
    }

    fn render_settings(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = settings_theme::palette(cx);
        let nav_focused = self.nav_focus.is_focused(window);
        let mut tabs = Self::settings_tabs();
        let experimental = tabs.pop();
        let mut nav = div()
            .id("settings-nav")
            .track_scroll(&self.nav_scroll)
            .track_focus(&self.nav_focus)
            .on_key_down(
                cx.listener(|this, event, window, cx| this.nav_key_down(event, window, cx)),
            )
            .w(px(190.))
            .flex_shrink_0()
            .h_full()
            .py_2()
            .flex()
            .flex_col()
            .gap_px()
            .border_r_1()
            .border_color(theme.border)
            .overflow_y_scroll();
        for (tab, id, label_key) in tabs {
            nav = nav.child(self.settings_tab(tab, id, label_key, nav_focused, cx));
        }
        // Experimental stands apart from the ordinary settings.
        nav = nav.child(div().flex_1().min_h_4());
        if let Some((tab, id, label_key)) = experimental {
            nav = nav.child(self.settings_tab(tab, id, label_key, nav_focused, cx));
        }
        let panel = match self.tab {
            SettingsTab::Connection => self.render_connection_settings(cx).into_any_element(),
            SettingsTab::Application => self.render_application_settings(cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_settings(cx).into_any_element(),
            SettingsTab::Keyboard => self.render_keyboard_settings(cx).into_any_element(),
            SettingsTab::Shortcuts => self.render_shortcut_settings(cx).into_any_element(),
            SettingsTab::Notifications => self.render_notification_settings(cx).into_any_element(),
            SettingsTab::Ircv3 => self.render_ircv3_settings(cx).into_any_element(),
            SettingsTab::ImageUpload => self.render_image_upload_settings(cx).into_any_element(),
            SettingsTab::Credentials => self.render_credential_settings(cx).into_any_element(),
            SettingsTab::Experimental => self.render_experimental_settings(cx).into_any_element(),
        };
        let auto_join = self.render_auto_join_dialog(cx);
        field_traversal(div().id("settings-screen"))
            .key_context("SettingsWindow")
            .relative()
            .size_full()
            .flex()
            .bg(theme.window)
            .text_size(px(13.))
            .when_some(settings_theme::current(cx), |d, native| {
                d.font_family(native.defaults.font.family.clone())
                    .text_size(px(native.defaults.font.size))
            })
            .text_color(theme.text)
            .child(div().relative().flex_shrink_0().h_full().child(nav).child(
                scrollbar::scrollbar("scrollbar-settings-nav", &self.nav_scroll, theme.text_muted),
            ))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .id("settings-pane")
                            .track_scroll(&self.pane_scroll)
                            .size_full()
                            .overflow_y_scroll()
                            .child(div().w_full().max_w(px(720.)).p_4().child(panel)),
                    )
                    .child(scrollbar::scrollbar(
                        "scrollbar-settings-pane",
                        &self.pane_scroll,
                        theme.text_muted,
                    )),
            )
            .children(auto_join)
            .on_action(cx.listener(Self::open_settings_action))
            .when(
                self.owner
                    .read(cx)
                    .is_ok_and(ChatWindow::can_disconnect_selected),
                |d| d.on_action(cx.listener(Self::disconnect_action)),
            )
            .on_action(cx.listener(Self::reconnect_action))
            .on_action(cx.listener(Self::toggle_debug_action))
            .on_action(cx.listener(Self::copy_diagnostics_action))
            .on_action(|_: &Quit, _, cx| cx.quit())
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.i18n.text("settings_title");
        let content = self.render_settings(window, cx).into_any_element();
        decorations::window_frame(window, cx, title, content)
    }
}

#[cfg(test)]
mod server_settings_tests {
    use super::{Localizer, SettingsForm};
    use crate::saved_connection_config;
    use cayenchat_irc_core::Connection;
    use cayenchat_storage::{
        CredentialBackendKind, CredentialStore, Ircv3Preferences, Language, Secret, Settings,
        credentials::MemoryBackend,
    };
    use gpui::{Entity, TestAppContext};
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::Arc,
        thread,
        time::Duration,
    };

    use crate::input::TextInput;

    fn memory_store() -> CredentialStore {
        CredentialStore::with_backend(Arc::new(MemoryBackend::new(CredentialBackendKind::System)))
    }

    /// Server A, fully configured, the only server so far.
    fn configured_a() -> Settings {
        let mut settings = Settings::default();
        settings.language = Language::English;
        let a = settings.add_server("a.example");
        a.port = 6697;
        a.use_tls = true;
        a.nickname = "alice".into();
        a.username = "ident-a".into();
        a.channels = "#a1,#a2".into();
        a.sasl_enabled = true;
        a.sasl_username = "account-a".into();
        a.remember_passwords = true;
        a.connect_on_startup = true;
        a.ircv3 = Ircv3Preferences {
            message_tags: true,
            server_time: true,
            batch: true,
            peer_avatars: true,
            chathistory: true,
            confirmed_sending: false,
            accounts: false,
        };
        settings
    }

    fn text(field: &Entity<TextInput>, cx: &TestAppContext) -> String {
        cx.read(|cx| field.read(cx).text().to_owned())
    }

    fn type_into(field: &Entity<TextInput>, value: &str, cx: &mut TestAppContext) {
        cx.update(|cx| field.update(cx, |field, cx| field.set_text(value, cx)));
    }

    fn form(settings: Settings, store: &CredentialStore, cx: &mut TestAppContext) -> SettingsForm {
        let i18n = Localizer::new(Language::English);
        cx.update(|cx| SettingsForm::new(settings, &i18n, store, cx))
    }

    fn switch(
        form: &mut SettingsForm,
        store: &CredentialStore,
        cx: &mut TestAppContext,
        change: impl FnOnce(&mut Settings),
    ) {
        let i18n = Localizer::new(Language::English);
        cx.update(|cx| form.switch_server(change, store, &i18n, cx))
            .unwrap();
    }

    fn server_fields(form: &SettingsForm, cx: &TestAppContext) -> [String; 7] {
        [
            text(&form.custom_host, cx),
            text(&form.port, cx),
            text(&form.nickname, cx),
            text(&form.username, cx),
            text(&form.channels, cx),
            text(&form.sasl_username, cx),
            text(&form.server_password, cx) + &text(&form.sasl_password, cx),
        ]
    }

    #[gpui::test]
    fn a_server_added_after_configuring_another_starts_blank(cx: &mut TestAppContext) {
        let store = memory_store();
        let settings = configured_a();
        let a_id = settings.selected_server.clone();
        store
            .set(
                &settings.selected_profile().unwrap().sasl_password_key(),
                &Secret::new("a-secret"),
            )
            .unwrap();
        let mut form = form(settings, &store, cx);
        // An edit to A that autosave has not written yet.
        type_into(&form.channels, "#a1,#a2,#a3", cx);

        switch(&mut form, &store, cx, |settings| {
            settings.add_server("");
        });
        assert_eq!(
            server_fields(&form, cx),
            ["", "6667", "", "", "", "", ""].map(str::to_owned)
        );
        assert!(!form.saved_server_password && !form.saved_sasl_password);
        let settings = cx.read(|cx| form.snapshot(cx)).unwrap();
        let b = settings.selected_profile().unwrap();
        assert_ne!(b.id, a_id);
        assert!(b.nickname.is_empty() && b.username.is_empty() && b.channels.is_empty());
        assert!(b.sasl_username.is_empty() && !b.sasl_enabled);
        assert!(!b.remember_passwords && !b.connect_on_startup && !b.use_tls);
        assert!(b.verify_tls_certificates, "transport defaults are kept");
        assert_eq!(b.ircv3, Ircv3Preferences::default());
        assert!(store.get(&b.sasl_password_key()).unwrap().is_none());
        assert_eq!(settings.profile(&a_id).unwrap().channels, "#a1,#a2,#a3");

        // A preset fills only its documented host.
        let mut form = self::form(configured_a(), &store, cx);
        switch(&mut form, &store, cx, |settings| {
            settings.add_server(cayenchat_storage::PRESETS[0].host);
        });
        assert_eq!(
            server_fields(&form, cx),
            ["irc.ircnet.com", "6667", "", "", "", "", ""].map(str::to_owned)
        );
        let settings = cx.read(|cx| form.snapshot(cx)).unwrap();
        let preset = settings.selected_profile().unwrap();
        assert!(preset.nickname.is_empty() && preset.channels.is_empty());
        assert_eq!(preset.ircv3, Ircv3Preferences::default());
    }

    #[gpui::test]
    fn an_unfinished_connection_entry_does_not_block_other_settings(cx: &mut TestAppContext) {
        let store = memory_store();
        let form = form(configured_a(), &store, cx);
        type_into(&form.port, "bad", cx);
        assert!(cx.read(|cx| form.snapshot(cx)).is_err());
        let settings = cx.read(|cx| form.snapshot_with(cx, false)).unwrap();
        assert_eq!(settings.servers, form.values.servers);
    }

    #[gpui::test]
    fn typed_passwords_wait_for_save_and_are_kept_per_server(cx: &mut TestAppContext) {
        let store = memory_store();
        let mut settings = configured_a();
        settings.selected_profile_mut().unwrap().remember_passwords = true;
        let a_id = settings.selected_server.clone();
        let b_id = settings.add_server("b.example").id.clone();
        settings.selected_server = a_id.clone();
        let mut form = form(settings, &store, cx);
        assert!(!cx.read(|cx| form.pending_passwords(cx)));
        type_into(&form.server_password, "a-typed", cx);
        assert!(cx.read(|cx| form.pending_passwords(cx)));
        switch(&mut form, &store, cx, |settings| {
            settings.selected_server = b_id.clone()
        });
        // Not shown, but still waiting, and nothing was written.
        assert_eq!(text(&form.server_password, cx), "");
        assert!(cx.read(|cx| form.pending_passwords(cx)));
        let key = form.values.profile(&a_id).unwrap().server_password_key();
        assert!(store.get(&key).unwrap().is_none());
        switch(&mut form, &store, cx, |settings| {
            settings.selected_server = a_id.clone()
        });
        assert_eq!(text(&form.server_password, cx), "a-typed");
    }

    #[gpui::test]
    fn switching_with_autosave_pending_keeps_each_servers_values(cx: &mut TestAppContext) {
        let store = memory_store();
        let mut settings = configured_a();
        let a_id = settings.selected_server.clone();
        let b = settings.add_server("b.example");
        b.use_tls = true;
        b.nickname = "bob".into();
        b.username = "ident-b".into();
        b.channels = "#b1".into();
        b.remember_passwords = true;
        let b_id = b.id.clone();
        settings.selected_server = a_id.clone();
        let (a_key, b_key) = (
            settings.profile(&a_id).unwrap().server_password_key(),
            settings.profile(&b_id).unwrap().server_password_key(),
        );
        let mut form = form(settings, &store, cx);

        // Edits and a typed password for A, then B is picked before the
        // delayed autosave runs.
        type_into(&form.channels, "#a-edited", cx);
        type_into(&form.server_password, "a-typed", cx);
        switch(&mut form, &store, cx, |settings| {
            settings.selected_server = b_id.clone()
        });
        assert_eq!(
            server_fields(&form, cx),
            ["b.example", "6667", "bob", "ident-b", "#b1", "", ""].map(str::to_owned)
        );
        assert!(
            store.get(&a_key).unwrap().is_none(),
            "switching does not store anything"
        );
        assert!(store.get(&b_key).unwrap().is_none());
        assert!(!form.saved_server_password, "B has no saved password");

        // Save stores the draft under the server it was typed for.
        let settings = cx.read(|cx| form.snapshot(cx)).unwrap();
        cx.update(|cx| {
            form.persist_passwords(&settings, &store, &Localizer::new(Language::English), cx)
        })
        .unwrap();
        assert_eq!(
            store.get(&a_key).unwrap().map(|s| s.expose().to_owned()),
            Some("a-typed".to_owned()),
            "the password went to the server it was typed for"
        );
        assert!(store.get(&b_key).unwrap().is_none());

        // The pending autosave now snapshots the form showing B.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let pending = cx.read(|cx| form.snapshot(cx)).unwrap();
        cayenchat_storage::save_to(&path, &pending).unwrap();
        let reloaded = cayenchat_storage::load_from(&path).unwrap().unwrap();
        let (a, b) = (
            reloaded.profile(&a_id).unwrap(),
            reloaded.profile(&b_id).unwrap(),
        );
        assert_eq!(
            (a.nickname.as_str(), a.channels.as_str()),
            ("alice", "#a-edited")
        );
        assert_eq!((b.nickname.as_str(), b.channels.as_str()), ("bob", "#b1"));
        assert_eq!(
            (a.username.as_str(), b.username.as_str()),
            ("ident-a", "ident-b")
        );
        assert_eq!(
            (a.sasl_username.as_str(), b.sasl_username.as_str()),
            ("account-a", "")
        );
        assert!(a.connect_on_startup && !b.connect_on_startup);
        assert!(a.ircv3.batch && !b.ircv3.batch);

        // Edit B, switch back: A shows its own values and saved password.
        type_into(&form.channels, "#b-edited", cx);
        switch(&mut form, &store, cx, |settings| {
            settings.selected_server = a_id.clone()
        });
        assert_eq!(
            server_fields(&form, cx),
            [
                "a.example",
                "6697",
                "alice",
                "ident-a",
                "#a-edited",
                "account-a",
                ""
            ]
            .map(str::to_owned)
        );
        assert!(form.saved_server_password);
        let settings = cx.read(|cx| form.snapshot(cx)).unwrap();
        assert_eq!(settings.profile(&b_id).unwrap().channels, "#b-edited");
        assert_eq!(settings.profile(&a_id).unwrap().channels, "#a-edited");
    }

    /// Accepts one client, registers it and returns every line it sent up
    /// to and including its JOIN lines.
    fn fixture(listener: TcpListener, joins: usize) -> thread::JoinHandle<Vec<String>> {
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut lines = Vec::new();
            let mut read = |lines: &mut Vec<String>| {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                lines.push(line.trim_end().to_owned());
                lines.last().unwrap().clone()
            };
            while !read(&mut lines).starts_with("USER ") {}
            let nick = lines
                .iter()
                .find_map(|line| line.strip_prefix("NICK "))
                .unwrap()
                .to_owned();
            socket
                .write_all(
                    format!(":srv 001 {nick} :Welcome\r\n:srv 376 {nick} :End\r\n").as_bytes(),
                )
                .unwrap();
            let mut seen = 0;
            while seen < joins {
                if read(&mut lines).starts_with("JOIN ") {
                    seen += 1;
                }
            }
            lines
        })
    }

    #[gpui::test]
    fn two_servers_connect_with_their_own_identity_and_joins(cx: &mut TestAppContext) {
        let store = memory_store();
        let (listener_a, listener_b) = (
            TcpListener::bind("127.0.0.1:0").unwrap(),
            TcpListener::bind("127.0.0.1:0").unwrap(),
        );
        let (port_a, port_b) = (
            listener_a.local_addr().unwrap().port(),
            listener_b.local_addr().unwrap().port(),
        );
        // Configure A, add B through the form, fill B, and save.
        let mut settings = Settings::default();
        settings.language = Language::English;
        let a = settings.add_server("127.0.0.1");
        a.port = port_a;
        a.nickname = "alice".into();
        a.username = "ident-a".into();
        a.channels = "#a1,#a2".into();
        let a_id = a.id.clone();
        let mut form = form(settings, &store, cx);
        switch(&mut form, &store, cx, |settings| {
            settings.add_server("");
        });
        assert_eq!(text(&form.channels, cx), "");
        type_into(&form.custom_host, "127.0.0.1", cx);
        type_into(&form.port, &port_b.to_string(), cx);
        type_into(&form.nickname, "bob", cx);
        type_into(&form.username, "ident-b", cx);
        type_into(&form.channels, "#b1", cx);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let saved = cx.read(|cx| form.snapshot(cx)).unwrap();
        cayenchat_storage::save_to(&path, &saved).unwrap();
        let reloaded = cayenchat_storage::load_from(&path).unwrap().unwrap();
        let b_id = saved.selected_server.clone();

        let config = |id: &str| {
            saved_connection_config(reloaded.profile(id).unwrap(), Language::English, &store)
                .unwrap()
        };
        let (config_a, config_b) = (config(&a_id), config(&b_id));
        assert_eq!(config_a.channels, ["#a1", "#a2"]);
        assert_eq!(config_b.channels, ["#b1"]);

        let (server_a, server_b) = (fixture(listener_a, 2), fixture(listener_b, 1));
        let (connection_a, connection_b) = (
            Connection::connect(config_a).unwrap(),
            Connection::connect(config_b).unwrap(),
        );
        let (lines_a, lines_b) = (server_a.join().unwrap(), server_b.join().unwrap());
        let _ = connection_a.disconnect();
        let _ = connection_b.disconnect();
        let pick = |lines: &[String], prefix: &str| -> Vec<String> {
            lines
                .iter()
                .filter(|line| line.starts_with(prefix))
                .cloned()
                .collect()
        };
        assert_eq!(pick(&lines_a, "NICK "), ["NICK alice"]);
        assert_eq!(pick(&lines_b, "NICK "), ["NICK bob"]);
        assert!(pick(&lines_a, "USER ")[0].starts_with("USER ident-a "));
        assert!(pick(&lines_b, "USER ")[0].starts_with("USER ident-b "));
        assert_eq!(pick(&lines_a, "JOIN "), ["JOIN #a1", "JOIN #a2"]);
        assert_eq!(pick(&lines_b, "JOIN "), ["JOIN #b1"]);
        for lines in [&lines_a, &lines_b] {
            assert!(pick(lines, "PASS").is_empty() && pick(lines, "AUTHENTICATE").is_empty());
        }
    }
}
