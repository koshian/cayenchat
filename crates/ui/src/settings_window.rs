// SPDX-License-Identifier: GPL-3.0-only
//! The settings window and the form it edits.

use crate::channel_settings_dialog::CHAR_REF_CHOICES;
use crate::os_text::WindowOsText;
use crate::{
    ChatWindow, Connect, Disconnect, FocusNextField, FocusPreviousField, OpenSettings, Reconnect,
    ShortcutPrefs, ToggleDebug, account_settings, apply_shortcuts, autostart, avatar_editor,
    color_picker, decorations, delete_secrets, diagnostics, focus_reveal, i18n_error,
    input::TextInput, ircv3_settings, localization::Localizer, notification_rules,
    removed_secret_keys, scrollbar, secrets, settings_file, settings_theme, shortcut_settings,
    theme,
};
use cayenchat_app::{attachments::AttachmentFlow, notifications};
use cayenchat_irc_core::{ConnectionConfig, KeyError};
use cayenchat_model::display;
use cayenchat_storage::{
    Appearance, AutoJoinEntry, ChannelNumberModifier, CredentialBackendKind, DarkColors, FontSet,
    Language, NoticeKey, Notifications, SaslMechanism, Secret, SecretKey, SendKey, ServerProfile,
    Settings, TextEncoding, TextKeyTheme, ThemeMode, color_value,
};
use gpui::{prelude::*, *};
use std::{cell::Cell, collections::HashMap, rc::Rc, time::Duration};

/// What was typed for a server that is not shown: its unsaved passwords and
/// whether each is to be saved with the settings.
#[derive(Clone, Default)]
pub(crate) struct Draft {
    server: String,
    sasl: String,
    save_server: bool,
    save_sasl: bool,
}

impl Draft {
    fn text_mut(&mut self, which: Credential) -> &mut String {
        match which {
            Credential::Server => &mut self.server,
            Credential::Sasl => &mut self.sasl,
        }
    }

    fn save_mut(&mut self, which: Credential) -> &mut bool {
        match which {
            Credential::Server => &mut self.save_server,
            Credential::Sasl => &mut self.save_sasl,
        }
    }
}

pub(crate) struct SettingsForm {
    pub(crate) values: Settings,
    server_list_open: bool,
    encoding_list_open: bool,
    sasl_list_open: bool,
    pub(crate) language_list_open: bool,
    pub(crate) ircv3_server_list_open: bool,
    pub(crate) custom_host: Entity<TextInput>,
    pub(crate) port: Entity<TextInput>,
    pub(crate) nickname: Entity<TextInput>,
    username: Entity<TextInput>,
    realname: Entity<TextInput>,
    quit_message: Entity<TextInput>,
    sleep_quit_message: Entity<TextInput>,
    part_message: Entity<TextInput>,
    display_name: Entity<TextInput>,
    pub(crate) channels: Entity<TextInput>,
    /// Password fields start empty and are shown while the password is not
    /// saved or is being changed; a saved value is never shown again (#635).
    pub(crate) server_password: Entity<TextInput>,
    sasl_username: Entity<TextInput>,
    sasl_password: Entity<TextInput>,
    pub(crate) saved_server_password: bool,
    saved_sasl_password: bool,
    /// "Change" was pressed: the field takes the new value of a saved password.
    changing_server_password: bool,
    changing_sasl_password: bool,
    /// The "save password" boxes of the shown server: a typed password is
    /// written to the credential store when the settings are saved, only if
    /// its box is checked (#635).
    remember_server_password: bool,
    remember_sasl_password: bool,
    /// The public key of the shown server's SASL key, once the user made,
    /// loaded or asked for it. It is no secret; the private key is never in
    /// the form.
    sasl_public_key: Option<String>,
    /// Typed passwords of servers not being shown, by server ID. They are
    /// used by Connect, and stored at a save only where the box is checked;
    /// closing the window drops them.
    pub(crate) drafts: HashMap<String, Draft>,
    pub(crate) member_list_background: Entity<TextInput>,
    pub(crate) channel_tree_background: Entity<TextInput>,
    pub(crate) channel_log_background: Entity<TextInput>,
    pub(crate) channel_log_alternate: Entity<TextInput>,
    pub(crate) channel_event_color: Entity<TextInput>,
    pub(crate) notice_color: Entity<TextInput>,
    pub(crate) highlight_color: Entity<TextInput>,
    pub(crate) combined_log_background: Entity<TextInput>,
    pub(crate) combined_log_alternate: Entity<TextInput>,
    pub(crate) dark_member_list_background: Entity<TextInput>,
    pub(crate) dark_channel_tree_background: Entity<TextInput>,
    pub(crate) dark_channel_log_background: Entity<TextInput>,
    pub(crate) dark_channel_log_alternate: Entity<TextInput>,
    pub(crate) dark_channel_event_color: Entity<TextInput>,
    pub(crate) dark_notice_color: Entity<TextInput>,
    pub(crate) dark_highlight_color: Entity<TextInput>,
    pub(crate) dark_combined_log_background: Entity<TextInput>,
    pub(crate) dark_combined_log_alternate: Entity<TextInput>,
    pub(crate) url_tooltip_color: Entity<TextInput>,
    pub(crate) dark_url_tooltip_color: Entity<TextInput>,
    pub(crate) link_color: Entity<TextInput>,
    pub(crate) dark_link_color: Entity<TextInput>,
    /// Empty is automatic.
    pub(crate) link_background: Entity<TextInput>,
    pub(crate) dark_link_background: Entity<TextInput>,
    pub(crate) url_tooltip_link_color: Entity<TextInput>,
    pub(crate) dark_url_tooltip_link_color: Entity<TextInput>,
    pub(crate) url_tooltip_text_color: Entity<TextInput>,
    pub(crate) dark_url_tooltip_text_color: Entity<TextInput>,
    pub(crate) channel_log_font: Entity<TextInput>,
    pub(crate) combined_log_font: Entity<TextInput>,
    pub(crate) member_list_font: Entity<TextInput>,
    pub(crate) channel_tree_font: Entity<TextInput>,
    pub(crate) input_font: Entity<TextInput>,
    pub(crate) time_font: Entity<TextInput>,
    /// Pixel sizes as typed, one per [`FontTarget`] in its order; empty is
    /// the area's standard size.
    pub(crate) font_sizes: [Entity<TextInput>; 6],
    /// The theme whose fonts the inputs above show while the light and dark
    /// fonts are separate (`values.appearance.split_fonts`).
    pub(crate) font_side_dark: bool,
    /// Comma-separated notification keywords.
    pub(crate) keywords: Entity<TextInput>,
    /// Comma-separated text that cancels the matches overlapping it.
    pub(crate) exclude_keywords: Entity<TextInput>,
    /// Draft URL of our own avatar for the selected server (IRCv3 tab).
    /// Editing it saves the draft; only Publish sends it.
    pub(crate) avatar_url: Entity<TextInput>,
}

impl SettingsForm {
    pub(crate) fn new(values: Settings, i18n: &Localizer, cx: &mut App) -> Self {
        let (saved_server_password, saved_sasl_password) = values
            .selected_profile()
            .map_or((false, false), saved_passwords);
        let empty = ServerProfile::default();
        let profile = values.selected_profile().unwrap_or(&empty);
        let field = |placeholder: &str, value: &str, secret: bool, cx: &mut App| {
            cx.new(|cx| TextInput::new_settings_field(placeholder, value, secret, cx))
        };
        let form = Self {
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
            sleep_quit_message: field(
                &i18n.text("sleep_quit_message_placeholder"),
                &profile.sleep_quit_message,
                false,
                cx,
            ),
            part_message: field(
                &i18n.text("part_message_placeholder"),
                &profile.part_message,
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
                    "credential_new_placeholder"
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
                    "credential_new_placeholder"
                } else {
                    "sasl_password"
                }),
                "",
                true,
                cx,
            ),
            saved_server_password,
            saved_sasl_password,
            changing_server_password: false,
            changing_sasl_password: false,
            remember_server_password: false,
            remember_sasl_password: false,
            sasl_public_key: None,
            drafts: HashMap::new(),
            member_list_background: field(
                "#FFFFFF",
                &values.appearance.member_list_background,
                false,
                cx,
            ),
            channel_tree_background: field(
                "#EAF3FF",
                &values.appearance.channel_tree_background,
                false,
                cx,
            ),
            channel_log_background: field(
                "#FFFFFF",
                &values.appearance.channel_log_background,
                false,
                cx,
            ),
            channel_log_alternate: field(
                "#F2F5FF",
                &values.appearance.channel_log_alternate,
                false,
                cx,
            ),
            channel_event_color: field(
                "#007D00",
                &values.appearance.channel_event_color,
                false,
                cx,
            ),
            notice_color: field("#7A838C", &values.appearance.notice_color, false, cx),
            highlight_color: field("#D46A8E", &values.appearance.highlight_color, false, cx),
            combined_log_background: field(
                "#F9FAFB",
                &values.appearance.combined_log_background,
                false,
                cx,
            ),
            combined_log_alternate: field(
                "#F2F5FF",
                &values.appearance.combined_log_alternate,
                false,
                cx,
            ),
            dark_member_list_background: field(
                "#1F2124",
                &values.appearance.dark.member_list_background,
                false,
                cx,
            ),
            dark_channel_tree_background: field(
                "#1D2630",
                &values.appearance.dark.channel_tree_background,
                false,
                cx,
            ),
            dark_channel_log_background: field(
                "#1F2124",
                &values.appearance.dark.channel_log_background,
                false,
                cx,
            ),
            dark_channel_log_alternate: field(
                "#272B31",
                &values.appearance.dark.channel_log_alternate,
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
            dark_combined_log_background: field(
                "#24272B",
                &values.appearance.dark.combined_log_background,
                false,
                cx,
            ),
            dark_combined_log_alternate: field(
                "#2C3036",
                &values.appearance.dark.combined_log_alternate,
                false,
                cx,
            ),
            url_tooltip_color: field("#1B1D20", &values.appearance.url_tooltip_color, false, cx),
            dark_url_tooltip_color: field(
                "#1B1D20",
                &values.appearance.dark.url_tooltip_color,
                false,
                cx,
            ),
            link_color: field("#0645AD", &values.appearance.link_color, false, cx),
            dark_link_color: field("#78AAFF", &values.appearance.dark.link_color, false, cx),
            link_background: field(
                &i18n.text("link_background_auto"),
                &values.appearance.link_background,
                false,
                cx,
            ),
            dark_link_background: field(
                &i18n.text("link_background_auto"),
                &values.appearance.dark.link_background,
                false,
                cx,
            ),
            url_tooltip_link_color: field(
                "#78AAFF",
                &values.appearance.url_tooltip_link_color,
                false,
                cx,
            ),
            dark_url_tooltip_link_color: field(
                "#78AAFF",
                &values.appearance.dark.url_tooltip_link_color,
                false,
                cx,
            ),
            url_tooltip_text_color: field(
                "#E3E6EA",
                &values.appearance.url_tooltip_text_color,
                false,
                cx,
            ),
            dark_url_tooltip_text_color: field(
                "#E3E6EA",
                &values.appearance.dark.url_tooltip_text_color,
                false,
                cx,
            ),
            channel_log_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.fonts(false).channel_log_font,
                false,
                cx,
            ),
            combined_log_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.fonts(false).combined_log_font,
                false,
                cx,
            ),
            member_list_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.fonts(false).member_list_font,
                false,
                cx,
            ),
            channel_tree_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.fonts(false).channel_tree_font,
                false,
                cx,
            ),
            input_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.fonts(false).input_font,
                false,
                cx,
            ),
            time_font: field(
                &i18n.text("font_monospace_placeholder"),
                &values.appearance.fonts(false).time_font,
                false,
                cx,
            ),
            font_sizes: font_size_texts(values.appearance.fonts(false))
                .map(|text| field(&i18n.text("font_size_placeholder"), &text, false, cx)),
            font_side_dark: false,
            keywords: field(
                &i18n.text("keywords_placeholder"),
                &values.notifications.keywords.join(", "),
                false,
                cx,
            ),
            exclude_keywords: field(
                &i18n.text("exclude_keywords_placeholder"),
                &values.notifications.exclude_keywords.join(", "),
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
            sasl_list_open: false,
            language_list_open: false,
            ircv3_server_list_open: false,
            values,
        };
        for input in form.color_inputs() {
            input.update(cx, |input, _| input.select_all_on_double_click());
        }
        for input in form.direct_input_fields() {
            input.update(cx, |input, _| input.prefer_direct_input());
        }
        form
    }

    /// Fields that mostly take ASCII, so the IME is detached while they have
    /// focus. Real name, messages, display name and passwords stay out.
    fn direct_input_fields(&self) -> impl Iterator<Item = &Entity<TextInput>> {
        [
            &self.nickname,
            &self.username,
            &self.custom_host,
            &self.port,
            &self.channels,
            &self.sasl_username,
        ]
        .into_iter()
        .chain(self.color_inputs())
    }

    /// The `#RRGGBB` code fields shown in the colors tab.
    fn color_inputs(&self) -> [&Entity<TextInput>; 28] {
        [
            &self.member_list_background,
            &self.dark_member_list_background,
            &self.channel_tree_background,
            &self.dark_channel_tree_background,
            &self.channel_log_background,
            &self.dark_channel_log_background,
            &self.channel_log_alternate,
            &self.dark_channel_log_alternate,
            &self.channel_event_color,
            &self.dark_channel_event_color,
            &self.notice_color,
            &self.dark_notice_color,
            &self.highlight_color,
            &self.dark_highlight_color,
            &self.combined_log_background,
            &self.dark_combined_log_background,
            &self.combined_log_alternate,
            &self.dark_combined_log_alternate,
            &self.link_color,
            &self.dark_link_color,
            &self.link_background,
            &self.dark_link_background,
            &self.url_tooltip_color,
            &self.dark_url_tooltip_color,
            &self.url_tooltip_link_color,
            &self.dark_url_tooltip_link_color,
            &self.url_tooltip_text_color,
            &self.dark_url_tooltip_text_color,
        ]
    }

    /// Writes the shown server's host, port and display name back to the
    /// values, so tabs that list servers show what was typed before saving.
    /// Nothing else is read, and an unusable port is left as it was.
    pub(crate) fn sync_shown_server(&mut self, cx: &App) {
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        let port = self.port.read(cx).text().trim().parse().ok();
        let (host, display_name) = (value(&self.custom_host), value(&self.display_name));
        if let Some(profile) = self.values.selected_profile_mut() {
            profile.host = host;
            profile.display_name = display_name;
            if let Some(port) = port.filter(|port| *port != 0) {
                profile.port = port;
            }
        }
    }

    pub(crate) fn snapshot(&self, cx: &App) -> Result<Settings, String> {
        self.snapshot_with(cx, true)
    }

    /// The form as settings. Without `with_servers`, the shown server's
    /// fields are not read (and not validated), so an unfinished connection
    /// entry does not stop other settings from being saved; the server list
    /// and selection are then the form's earlier values.
    pub(crate) fn snapshot_with(&self, cx: &App, with_servers: bool) -> Result<Settings, String> {
        let mut settings = self.values.clone();
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        if with_servers {
            self.read_server_text(&mut settings, cx)?;
        }
        let (fonts, light_fonts, dark_fonts) = self.font_sets_with(self.typed_fonts(cx, true)?);
        settings.appearance = Appearance {
            member_list_background: value(&self.member_list_background),
            channel_tree_background: value(&self.channel_tree_background),
            channel_log_background: value(&self.channel_log_background),
            channel_log_alternate: value(&self.channel_log_alternate),
            channel_event_color: value(&self.channel_event_color),
            notice_color: value(&self.notice_color),
            highlight_color: value(&self.highlight_color),
            combined_log_background: value(&self.combined_log_background),
            combined_log_alternate: value(&self.combined_log_alternate),
            url_tooltip_color: value(&self.url_tooltip_color),
            link_color: value(&self.link_color),
            link_background: value(&self.link_background),
            url_tooltip_link_color: value(&self.url_tooltip_link_color),
            url_tooltip_text_color: value(&self.url_tooltip_text_color),
            url_tooltip_opacity: self.values.appearance.url_tooltip_opacity,
            member_list_opacity: self.values.appearance.member_list_opacity,
            channel_tree_opacity: self.values.appearance.channel_tree_opacity,
            channel_log_opacity: self.values.appearance.channel_log_opacity,
            combined_log_opacity: self.values.appearance.combined_log_opacity,
            alternate_rows: self.values.appearance.alternate_rows,
            image_previews: self.values.appearance.image_previews,
            compact_urls: self.values.appearance.compact_urls,
            decode_char_refs: self.values.appearance.decode_char_refs,
            reiwa_mode: self.values.appearance.reiwa_mode,
            fonts,
            split_fonts: self.values.appearance.split_fonts,
            light_fonts,
            dark_fonts,
            channel_log_line_height: self.values.appearance.channel_log_line_height,
            combined_log_line_height: self.values.appearance.combined_log_line_height,
            dark: DarkColors {
                member_list_background: value(&self.dark_member_list_background),
                channel_tree_background: value(&self.dark_channel_tree_background),
                channel_log_background: value(&self.dark_channel_log_background),
                channel_log_alternate: value(&self.dark_channel_log_alternate),
                channel_event_color: value(&self.dark_channel_event_color),
                notice_color: value(&self.dark_notice_color),
                highlight_color: value(&self.dark_highlight_color),
                combined_log_background: value(&self.dark_combined_log_background),
                combined_log_alternate: value(&self.dark_combined_log_alternate),
                url_tooltip_color: value(&self.dark_url_tooltip_color),
                link_color: value(&self.dark_link_color),
                link_background: value(&self.dark_link_background),
                url_tooltip_link_color: value(&self.dark_url_tooltip_link_color),
                url_tooltip_text_color: value(&self.dark_url_tooltip_text_color),
                // Not text fields: changed by the wallpaper and theme buttons.
                wallpapers: self.values.appearance.dark.wallpapers.clone(),
                origin: self.values.appearance.dark.origin.clone(),
            },
            wallpapers: self.values.appearance.wallpapers.clone(),
            origin: self.values.appearance.origin.clone(),
            saved_colors: self.values.appearance.saved_colors.clone(),
            channel_looks: self.values.appearance.channel_looks.clone(),
        };
        settings.appearance.validate()?;
        settings.notifications.keywords =
            notifications::parse_keywords(self.keywords.read(cx).text());
        settings.notifications.exclude_keywords =
            notifications::parse_keywords(self.exclude_keywords.read(cx).text());
        Ok(settings)
    }

    /// Reads the shown server's text fields into `settings`. A port that is
    /// not a number leaves `settings` untouched.
    fn read_server_text(&self, settings: &mut Settings, cx: &App) -> Result<(), String> {
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        let host = value(&self.custom_host);
        // An empty host is left to `commit_server_text`, which knows whether
        // the server is a draft.
        if !host.is_empty() && !ConnectionConfig::host_is_valid(&host) {
            return Err(i18n_error(settings.language, "host_invalid"));
        }
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
            profile.sleep_quit_message = value(&self.sleep_quit_message);
            profile.part_message = value(&self.part_message);
            profile.display_name = value(&self.display_name);
            profile.channels = value(&self.channels);
            profile.sasl_username = value(&self.sasl_username);
            let avatar = value(&self.avatar_url);
            crate::ircv3_settings::stop_sharing_if_avatar_differs(profile, &avatar);
            profile.avatar_url = avatar;
        }
        Ok(())
    }

    /// Takes the shown server's text fields into `values` when they are
    /// valid; otherwise `values` (and so the saved server) stays as it was
    /// and the fields keep what was typed. With `require_host` an empty
    /// hostname is invalid too, since saving it would drop the server.
    pub(crate) fn commit_server_text(
        &mut self,
        require_host: bool,
        cx: &App,
    ) -> Result<(), String> {
        let mut settings = self.values.clone();
        self.read_server_text(&mut settings, cx)?;
        if require_host
            && settings
                .selected_profile()
                .is_some_and(|profile| profile.host.is_empty())
        {
            return Err(i18n_error(settings.language, "server_required"));
        }
        self.values.servers = settings.servers;
        Ok(())
    }

    /// The text fields of a server profile. Edits to them are committed when
    /// the field loses focus, not while typing.
    fn server_text_fields(&self) -> [&Entity<TextInput>; 12] {
        [
            &self.custom_host,
            &self.port,
            &self.nickname,
            &self.username,
            &self.realname,
            &self.quit_message,
            &self.sleep_quit_message,
            &self.part_message,
            &self.display_name,
            &self.channels,
            &self.sasl_username,
            &self.avatar_url,
        ]
    }

    /// Typed passwords win; otherwise the saved ones are used.
    pub(crate) fn connection_config(
        &self,
        settings: &mut Settings,
        stored: impl crate::StoredSource,
        i18n: &Localizer,
        cx: &App,
    ) -> Result<(ConnectionConfig, crate::connect_auth::Missing), String> {
        // A saved password shown as such is used as saved: what the hidden
        // field holds counts only while it is being changed.
        let typed = |field: &Entity<TextInput>, which: Credential| {
            let (saved, changing) = self.credential_state(which);
            let text = field.read(cx).text();
            (!text.is_empty() && (!saved || changing)).then(|| Secret::new(text))
        };
        let profile = settings
            .selected_profile()
            .ok_or_else(|| i18n.text("server_required"))?;
        let stored = stored.stored(profile);
        let (saved_server, saved_sasl) = stored
            .secrets
            .map_err(|error| secrets::error_text(i18n, &error))?;
        let sasl_found = profile.sasl_enabled.then(|| saved_sasl.is_some());
        let found = (saved_server.is_some(), sasl_found);
        let mut config = crate::connection_config_without_secrets(profile, settings.language)?;
        let missing = crate::connect_auth::fill(
            &mut config,
            typed(&self.server_password, Credential::Server)
                .or(saved_server)
                .as_ref(),
            // A key is never typed; what the password field holds is not it.
            typed(&self.sasl_password, Credential::Sasl)
                .filter(|_| !profile.sasl_mechanism.uses_key())
                .or(saved_sasl)
                .as_ref(),
            false,
        );
        crate::connect_auth::while_incomplete(&mut config, |config| {
            crate::channel_keys::use_keys(config, stored.keys);
            config.validate()
        })?;
        // What the read found corrects the note of stored passwords; the
        // settings are written along with Connect.
        if let Some(profile) = settings.selected_profile_mut() {
            crate::connect_auth::note_found(profile, Some(found.0), found.1);
        }
        Ok((config, missing))
    }

    /// Every text field, so edits to any of them can trigger an autosave.
    fn text_fields(&self) -> [&Entity<TextInput>; 56] {
        [
            &self.custom_host,
            &self.port,
            &self.nickname,
            &self.username,
            &self.realname,
            &self.quit_message,
            &self.sleep_quit_message,
            &self.part_message,
            &self.display_name,
            &self.channels,
            &self.server_password,
            &self.sasl_username,
            &self.sasl_password,
            &self.member_list_background,
            &self.channel_tree_background,
            &self.channel_log_background,
            &self.channel_log_alternate,
            &self.channel_event_color,
            &self.notice_color,
            &self.highlight_color,
            &self.combined_log_background,
            &self.combined_log_alternate,
            &self.dark_member_list_background,
            &self.dark_channel_tree_background,
            &self.dark_channel_log_background,
            &self.dark_channel_log_alternate,
            &self.dark_channel_event_color,
            &self.dark_notice_color,
            &self.dark_highlight_color,
            &self.dark_combined_log_background,
            &self.dark_combined_log_alternate,
            &self.url_tooltip_color,
            &self.dark_url_tooltip_color,
            &self.link_color,
            &self.dark_link_color,
            &self.link_background,
            &self.dark_link_background,
            &self.url_tooltip_link_color,
            &self.dark_url_tooltip_link_color,
            &self.url_tooltip_text_color,
            &self.dark_url_tooltip_text_color,
            &self.channel_log_font,
            &self.combined_log_font,
            &self.member_list_font,
            &self.channel_tree_font,
            &self.input_font,
            &self.time_font,
            &self.font_sizes[0],
            &self.font_sizes[1],
            &self.font_sizes[2],
            &self.font_sizes[3],
            &self.font_sizes[4],
            &self.font_sizes[5],
            &self.keywords,
            &self.exclude_keywords,
            &self.avatar_url,
        ]
    }

    /// Moves the typed passwords of the shown server into `drafts`. A value
    /// typed to change a saved password is dropped instead of kept.
    pub(crate) fn stash_typed_passwords(&mut self, cx: &App) {
        let Some(id) = self
            .values
            .selected_profile()
            .map(|profile| profile.id.clone())
        else {
            return;
        };
        let mut draft = Draft::default();
        for which in [Credential::Server, Credential::Sasl] {
            let (saved, _) = self.credential_state(which);
            if !saved {
                *draft.text_mut(which) = self.credential_field(which).read(cx).text().to_owned();
                *draft.save_mut(which) = self.remember(which);
            }
        }
        if draft.server.is_empty() && draft.sasl.is_empty() {
            self.drafts.remove(&id);
        } else {
            self.drafts.insert(id, draft);
        }
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
        i18n: &Localizer,
        cx: &mut App,
    ) -> Result<(), String> {
        let mut settings = self.snapshot(cx)?;
        self.stash_typed_passwords(cx);
        change(&mut settings);
        self.values = settings;
        self.show_selected(i18n, cx);
        Ok(())
    }

    /// Fills the server fields from the selected profile alone; password
    /// fields are emptied and say whether that server has saved passwords.
    pub(crate) fn show_selected(&mut self, i18n: &Localizer, cx: &mut App) {
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
            (&self.sleep_quit_message, &profile.sleep_quit_message),
            (&self.part_message, &profile.part_message),
            (&self.display_name, &profile.display_name),
            (&self.channels, &profile.channels),
            (&self.sasl_username, &profile.sasl_username),
            (&self.avatar_url, &profile.avatar_url),
        ] {
            field.update(cx, |field, cx| field.set_text(value, cx));
        }
        let (saved_server, saved_sasl) = saved_passwords(&profile);
        self.saved_server_password = saved_server;
        self.saved_sasl_password = saved_sasl;
        self.changing_server_password = false;
        self.changing_sasl_password = false;
        self.sasl_public_key = None;
        let draft = self.drafts.get(&profile.id).cloned().unwrap_or_default();
        // An unfinished change of a saved password is dropped on a switch.
        let (draft_server, draft_sasl) = (
            if saved_server {
                String::new()
            } else {
                draft.server
            },
            if saved_sasl {
                String::new()
            } else {
                draft.sasl
            },
        );
        self.remember_server_password = !saved_server && draft.save_server;
        self.remember_sasl_password = !saved_sasl && draft.save_sasl;
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
                "credential_new_placeholder"
            } else {
                key
            });
            field.update(cx, |field, cx| {
                field.set_text(&draft, cx);
                field.set_placeholder(&placeholder, cx);
            });
        }
        self.server_list_open = false;
        self.sasl_list_open = false;
        self.encoding_list_open = false;
        self.language_list_open = false;
        self.ircv3_server_list_open = false;
    }
}

/// Why a key file could not be used.
enum KeyFileError {
    /// The file was read but is not an unencrypted P-256 PEM key.
    Key(KeyError),
    /// The file could not be read (the system's wording).
    Io(String),
}

/// The most of a key file that is read; a P-256 PEM key is a few hundred
/// bytes, so anything near this size is not one.
const MAX_KEY_FILE_BYTES: u64 = 16 * 1024;

const LINE_HEIGHT_TRACK_WIDTH: f32 = 220.;
const LINE_HEIGHT_THUMB: f32 = 14.;

/// Where along the slider (`0..=1`) a saved line height sits; the ends are
/// the smallest and the largest allowed height.
fn line_height_fraction(percent: u16) -> f32 {
    let percent = cayenchat_storage::clamp_line_height_percent(percent);
    f32::from(percent - cayenchat_storage::MIN_LINE_HEIGHT_PERCENT)
        / f32::from(
            cayenchat_storage::MAX_LINE_HEIGHT_PERCENT - cayenchat_storage::MIN_LINE_HEIGHT_PERCENT,
        )
}

/// The line height at a slider fraction (`0..=1`), in whole percents.
fn line_height_at(fraction: f32) -> u16 {
    let span =
        cayenchat_storage::MAX_LINE_HEIGHT_PERCENT - cayenchat_storage::MIN_LINE_HEIGHT_PERCENT;
    cayenchat_storage::MIN_LINE_HEIGHT_PERCENT
        + (fraction.clamp(0., 1.) * f32::from(span)).round() as u16
}

/// Marks a drag as the line height slider's own.
struct LineHeightDrag(EntityId, usize);

/// The pane font sizes as shown in their fields, in [`FontTarget`] order.
pub(crate) fn font_size_texts(fonts: &FontSet) -> [String; 6] {
    [
        fonts.channel_log_font_size,
        fonts.combined_log_font_size,
        fonts.member_list_font_size,
        fonts.channel_tree_font_size,
        fonts.input_font_size,
        fonts.time_font_size,
    ]
    .map(|size| size.to_string())
}

impl SettingsForm {
    /// The fonts the inputs hold. An empty or invalid size is the standard
    /// one, or an error when `strict`, which snapshots use; unfinished text
    /// while switching sides is not worth refusing.
    pub(crate) fn typed_fonts(&self, cx: &App, strict: bool) -> Result<FontSet, String> {
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        let standard = FontSet::default();
        let size = |index: usize, standard: u16| -> Result<u16, String> {
            let text = value(&self.font_sizes[index]);
            match font_size_text_value(&text) {
                Some(size) => Ok(size),
                None if text.is_empty() || !strict => Ok(standard),
                None => Err(i18n_error(self.values.language, "font_size_invalid")),
            }
        };
        Ok(FontSet {
            channel_log_font: value(&self.channel_log_font),
            combined_log_font: value(&self.combined_log_font),
            member_list_font: value(&self.member_list_font),
            channel_tree_font: value(&self.channel_tree_font),
            input_font: value(&self.input_font),
            time_font: value(&self.time_font),
            channel_log_font_size: size(0, standard.channel_log_font_size)?,
            combined_log_font_size: size(1, standard.combined_log_font_size)?,
            member_list_font_size: size(2, standard.member_list_font_size)?,
            channel_tree_font_size: size(3, standard.channel_tree_font_size)?,
            input_font_size: size(4, standard.input_font_size)?,
            time_font_size: size(5, standard.time_font_size)?,
        })
    }

    /// The shared, light and dark font sets with `typed` in the place the
    /// inputs show: the shared ones, or the shown theme's own when they are
    /// separate.
    pub(crate) fn font_sets_with(
        &self,
        typed: FontSet,
    ) -> (FontSet, Option<FontSet>, Option<FontSet>) {
        let appearance = &self.values.appearance;
        let mut sets = (
            appearance.fonts.clone(),
            appearance.light_fonts.clone(),
            appearance.dark_fonts.clone(),
        );
        if !appearance.split_fonts {
            sets.0 = typed;
        } else if self.font_side_dark {
            sets.2 = Some(typed);
        } else {
            sets.1 = Some(typed);
        }
        sets
    }

    /// Puts the typed fonts into `values`, so what the inputs show next is
    /// the stored state.
    pub(crate) fn stash_fonts(&mut self, cx: &App) {
        if let Ok(typed) = self.typed_fonts(cx, false) {
            let (fonts, light, dark) = self.font_sets_with(typed);
            let appearance = &mut self.values.appearance;
            (
                appearance.fonts,
                appearance.light_fonts,
                appearance.dark_fonts,
            ) = (fonts, light, dark);
        }
    }

    /// Shows `fonts` in the inputs.
    pub(crate) fn show_fonts(&self, fonts: &FontSet, cx: &mut App) {
        for (field, text) in [
            (&self.channel_log_font, &fonts.channel_log_font),
            (&self.combined_log_font, &fonts.combined_log_font),
            (&self.member_list_font, &fonts.member_list_font),
            (&self.channel_tree_font, &fonts.channel_tree_font),
            (&self.input_font, &fonts.input_font),
            (&self.time_font, &fonts.time_font),
        ] {
            field.update(cx, |field, cx| field.set_text(text, cx));
        }
        for (field, text) in self.font_sizes.iter().zip(font_size_texts(fonts)) {
            field.update(cx, |field, cx| field.set_text(&text, cx));
        }
    }

    /// Switches between shared and separate light and dark fonts. The first
    /// time on, both start as the shared fonts; turning it off keeps the
    /// separate ones for the next time.
    pub(crate) fn set_split_fonts(&mut self, on: bool, cx: &mut App) {
        self.stash_fonts(cx);
        self.values.appearance.set_split_fonts(on);
        let fonts = self.values.appearance.fonts(self.font_side_dark).clone();
        self.show_fonts(&fonts, cx);
    }

    /// Shows the fonts of the light or dark theme.
    pub(crate) fn select_font_side(&mut self, dark: bool, cx: &mut App) {
        self.stash_fonts(cx);
        self.font_side_dark = dark;
        let fonts = self.values.appearance.fonts(dark).clone();
        self.show_fonts(&fonts, cx);
    }

    /// Changes a size by `delta` px, within the allowed range.
    pub(crate) fn step_font_size(&self, target: FontTarget, delta: i32, cx: &mut App) {
        let field = &self.font_sizes[target as usize];
        let standard = FontSet::default();
        let current = font_size_text_value(field.read(cx).text()).unwrap_or(match target {
            FontTarget::ChannelLog => standard.channel_log_font_size,
            FontTarget::CombinedLog => standard.combined_log_font_size,
            FontTarget::MemberList => standard.member_list_font_size,
            FontTarget::ChannelTree => standard.channel_tree_font_size,
            FontTarget::Input => standard.input_font_size,
            FontTarget::Time => standard.time_font_size,
        });
        let next = (i32::from(current) + delta).clamp(
            i32::from(cayenchat_storage::MIN_FONT_SIZE),
            i32::from(cayenchat_storage::MAX_FONT_SIZE),
        );
        field.update(cx, |field, cx| field.set_text(&next.to_string(), cx));
    }
}

/// A font size field's text as a size, when it is a whole number of px within
/// the allowed range.
fn font_size_text_value(text: &str) -> Option<u16> {
    text.trim().parse().ok().filter(|size| {
        (cayenchat_storage::MIN_FONT_SIZE..=cayenchat_storage::MAX_FONT_SIZE).contains(size)
    })
}

/// Reads `path` as an unencrypted PKCS#8 or SEC1 PEM key. Only the first
/// [`MAX_KEY_FILE_BYTES`] + 1 bytes are read, so a huge file is refused
/// without being loaded.
fn read_key_file(path: &std::path::Path) -> Result<cayenchat_irc_core::SecretText, KeyFileError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| KeyFileError::Io(error.to_string()))?;
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.take(MAX_KEY_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| KeyFileError::Io(error.to_string()))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| KeyFileError::Key(KeyError::NotPem))?;
    cayenchat_irc_core::import_sasl_key_pem(text).map_err(KeyFileError::Key)
}

/// Takes over a flag the settings file holds, unless the form value differs
/// from the last saved one: that is an edit of this window, which a save
/// right after must not lose.
fn follow_file_flag(form: &mut bool, saved: &mut bool, file: bool) {
    if *form == *saved {
        *form = file;
    }
    *saved = file;
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

/// Takes over the notes of saved passwords the file holds for the servers
/// both have. Only an explicit save or a connection changes them; the file
/// is the newer one, with the chat window's unwritten corrections laid over it.
fn follow_password_notes(servers: &mut [ServerProfile], file: &[ServerProfile]) {
    for server in servers {
        if let Some(theirs) = file.iter().find(|p| p.id == server.id) {
            server.server_saved = theirs.server_saved;
            server.sasl_saved = theirs.sasl_saved;
            server.sasl_key_saved = theirs.sasl_key_saved;
            server.remember_passwords = theirs.remember_passwords;
        }
    }
}

/// A saved server whose host the settings change (#973).
struct HostChange {
    id: String,
    old: String,
    new: String,
    /// The store entries sent to the server as they are.
    keys: Vec<SecretKey>,
    /// Some of them are noted as saved, so the change is asked about.
    ask: bool,
}

/// What follows once a host change's old passwords are deleted (#973).
#[derive(Clone, Copy)]
enum AfterHostChange {
    /// Save the new host in the background.
    Autosave,
    /// Save the servers as shown (the Save button).
    Save,
    /// Connect with the new host.
    Connect,
}

/// Which of a server's two passwords.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Credential {
    Server,
    Sasl,
}

impl Credential {
    /// Notes in the profile that this password is saved or gone.
    pub(crate) fn note(self, profile: &mut ServerProfile, saved: bool) {
        match self {
            Self::Server => profile.set_server_password_saved(saved),
            Self::Sasl => profile.set_sasl_password_saved(saved),
        }
    }

    fn key(self, profile: &str) -> SecretKey {
        match self {
            Self::Server => SecretKey::server_password(profile),
            Self::Sasl => SecretKey::sasl_password(profile),
        }
    }
}

impl SettingsForm {
    pub(crate) fn credential_field(&self, which: Credential) -> &Entity<TextInput> {
        match which {
            Credential::Server => &self.server_password,
            Credential::Sasl => &self.sasl_password,
        }
    }

    fn changing_mut(&mut self, which: Credential) -> &mut bool {
        match which {
            Credential::Server => &mut self.changing_server_password,
            Credential::Sasl => &mut self.changing_sasl_password,
        }
    }

    /// Whether the password is shown as saved (Change, Delete), as being
    /// changed (a field, Save and Cancel), or neither (a field and the
    /// "save password" box).
    pub(crate) fn credential_state(&self, which: Credential) -> (bool, bool) {
        match which {
            Credential::Server => (self.saved_server_password, self.changing_server_password),
            Credential::Sasl => (self.saved_sasl_password, self.changing_sasl_password),
        }
    }

    /// Whether the unsaved password is to be saved with the settings.
    pub(crate) fn remember(&self, which: Credential) -> bool {
        match which {
            Credential::Server => self.remember_server_password,
            Credential::Sasl => self.remember_sasl_password,
        }
    }

    pub(crate) fn set_remember(&mut self, which: Credential, remember: bool) {
        match which {
            Credential::Server => self.remember_server_password = remember,
            Credential::Sasl => self.remember_sasl_password = remember,
        }
    }

    /// Records that the password is now saved or gone, and empties its field
    /// so what was typed does not stay in the form.
    fn set_saved(&mut self, which: Credential, saved: bool, i18n: &Localizer, cx: &mut App) {
        match which {
            Credential::Server => self.saved_server_password = saved,
            Credential::Sasl => self.saved_sasl_password = saved,
        }
        self.set_remember(which, false);
        *self.changing_mut(which) = false;
        let placeholder = i18n.text(match (which, saved) {
            (_, true) => "credential_new_placeholder",
            (Credential::Server, false) => "server_password_placeholder",
            (Credential::Sasl, false) => "sasl_password",
        });
        self.credential_field(which).update(cx, |field, cx| {
            field.set_text("", cx);
            field.set_placeholder(&placeholder, cx);
        });
    }
}

/// Whether the profile is noted as having saved server and SASL passwords.
/// Showing a server never reads the credential store, which can ask the user
/// for permission; the note is corrected when a connection reads the store.
fn saved_passwords(profile: &ServerProfile) -> (bool, bool) {
    (
        profile.server_password_saved(),
        profile.sasl_password_saved(),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    Connection,
    Application,
    Appearance,
    Keyboard,
    Notifications,
    Ircv3,
    ImageUpload,
    Credentials,
    Experimental,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FontTarget {
    ChannelLog,
    CombinedLog,
    MemberList,
    ChannelTree,
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
    /// Scroll position of the row list, which follows Tab like the pane does.
    pub(crate) scroll: ScrollHandle,
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
    pub(crate) nav_scroll: ScrollHandle,
    pub(crate) pane_scroll: ScrollHandle,
    /// Frames still allowed to scroll to the field Tab moved to: the first
    /// renders before the field is painted at its new place, the second after.
    reveal_focus: u8,
    pub(crate) font_picker: Option<FontTarget>,
    /// Imported themes and the state of the theme actions (#962).
    pub(crate) themes: crate::theme_settings::ThemeUi,
    /// Where the log line height slider's track was last painted, to turn
    /// the pointer position into a value (#953).
    line_height_tracks: [Rc<Cell<Bounds<Pixels>>>; 2],
    /// The opacity sliders (member list, channel tree, channel log,
    /// combined log, hover popup): their painted tracks and keyboard focus.
    opacity_tracks: [crate::slider::TrackBounds; 5],
    opacity_focus: [FocusHandle; 5],
    pub(crate) color_picker: Option<OpenColorPicker>,
    /// A key being recorded for a shortcut (the Keyboard tab).
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
    pub(crate) window: AnyWindowHandle,
    autosave: Option<Task<()>>,
    /// Why the latest edits could not be saved, shown until they can be.
    pub(crate) autosave_error: Option<String>,
    /// Secrets of removed servers that could not be deleted yet; every save
    /// tries them again.
    forget_pending: Vec<SecretKey>,
    /// A host change waits for its confirmation or for the deletion of the
    /// passwords that belong to the old host (#973).
    host_change_open: bool,
    /// Connect was pressed and the store thread is still reading.
    connecting: bool,
    /// Credential writes, deletions and moves of the store thread that this
    /// window waits for (passwords, SASL keys, a switch of the store). While
    /// any is pending the window neither closes nor shows another server:
    /// what finishes them is bound to this window and the server shown (D087).
    store_waits: usize,
    /// Why the shown server's typed text was not taken over (a bad port, no
    /// hostname). The text stays in its fields and the saved server is left
    /// as it was, until the text is valid.
    pub(crate) field_error: Option<String>,
    /// Why Publish or Remove could not be started for our own avatar, or
    /// how an avatar image upload went.
    pub(crate) avatar_feedback: Option<String>,
    /// Servers whose icon was registered or updated and not yet saved; once
    /// the servers are saved, the chat window fetches those icons again.
    pub(crate) avatar_refresh: Vec<String>,
    /// The avatar image file being read off the UI thread; a newer drop or
    /// choice replaces it.
    pub(crate) avatar_file_read: Option<Task<()>>,
    /// Avatar images on their way to the image host; the target is the
    /// server profile whose avatar URL draft receives the link.
    pub(crate) avatar_upload: AttachmentFlow<String>,
    /// The square selection for an avatar image, before uploading it.
    pub(crate) avatar_editor: Option<avatar_editor::AvatarEditor>,
    /// The auto-join list being edited in its own dialog.
    pub(crate) auto_join: Option<AutoJoinDialog>,
    /// An avatar image is being decoded or encoded.
    pub(crate) avatar_opening: bool,
    /// The quick icon section at the top of the Connection tab is expanded.
    /// Only a view state: it is never saved and sends nothing.
    pub(crate) avatar_quick_open: bool,
    /// Keyboard focus of that section's disclosure header.
    pub(crate) avatar_toggle_focus: FocusHandle,
    /// Id of the running upload that was started in the quick
    /// section, so finishing it registers the icon.
    pub(crate) avatar_upload_registers: Option<u64>,
    /// The latest feedback is about the image host, so the quick section
    /// offers the way to the Image Upload tab.
    pub(crate) avatar_setup_link: bool,
    /// The server whose quick icon section sent the user to the Image
    /// Upload tab, to return to.
    pub(crate) avatar_return: Option<String>,
    /// Observers of `settings`' fields; replaced with the form.
    _field_subscriptions: Vec<Subscription>,
    /// Whether the connection button was last drawn as Disconnect, so the window is
    /// redrawn only when a connection comes up or goes down.
    pub(crate) connected_shown: bool,
    /// The conversation log status as last drawn, so a failure that comes
    /// by itself redraws the window.
    pub(crate) log_status_shown: Option<cayenchat_storage::chatlog::Status>,
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

/// What a save writes of the server list (#634).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SaveMode {
    /// The registered servers as committed so far. A draft and text still
    /// being typed are left out, and no password is stored.
    Committed,
    /// The servers as shown, including a draft, and the typed passwords.
    Explicit,
}

/// See `SettingsWindow::reveal_focus`.
const REVEAL_FRAMES: u8 = 2;

/// Pause after the last edit before settings are written.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(500);

pub(crate) fn settings_field(label: &str, input: Entity<TextInput>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(settings_theme::label_column().child(label.to_owned()))
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
                    .w(settings_theme::LABEL_WIDTH)
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
        window.os_set_title(&self.i18n.text("settings_title"));
        for (field, key) in [
            (&self.settings.custom_host, "server_host_placeholder"),
            (&self.settings.nickname, "nickname"),
            (&self.settings.username, "username_placeholder"),
            (&self.settings.realname, "realname_placeholder"),
            (&self.settings.quit_message, "quit_message_placeholder"),
            (
                &self.settings.sleep_quit_message,
                "sleep_quit_message_placeholder",
            ),
            (&self.settings.part_message, "part_message_placeholder"),
            (&self.settings.display_name, "display_name_placeholder"),
            (
                &self.settings.server_password,
                if self.settings.saved_server_password {
                    "credential_new_placeholder"
                } else {
                    "server_password_placeholder"
                },
            ),
            (&self.settings.sasl_username, "sasl_account_placeholder"),
            (
                &self.settings.sasl_password,
                if self.settings.saved_sasl_password {
                    "credential_new_placeholder"
                } else {
                    "sasl_password"
                },
            ),
            (&self.settings.channel_log_font, "font_system_placeholder"),
            (&self.settings.combined_log_font, "font_system_placeholder"),
            (&self.settings.member_list_font, "font_system_placeholder"),
            (&self.settings.channel_tree_font, "font_system_placeholder"),
            (&self.settings.input_font, "font_system_placeholder"),
            (&self.settings.time_font, "font_monospace_placeholder"),
        ] {
            let placeholder = self.i18n.text(key);
            field.update(cx, |field, cx| field.set_placeholder(&placeholder, cx));
        }
        let placeholder = self.i18n.text("font_size_placeholder");
        for field in &self.settings.font_sizes {
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
        window.os_set_title(&i18n.text("settings_title"));
        settings_theme::refresh(cx);
        let settings = SettingsForm::new(values, &i18n, cx);
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
                this._subscriptions
                    .push(cx.observe(&chat, |this, chat, cx| {
                        if this.selected_connected(cx) != this.connected_shown
                            || chat.read(cx).conversation_logs.status() != this.log_status_shown
                        {
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
            view.update(cx, |this, cx| this.save_before_leaving(cx))
                .unwrap_or(true)
        });
        subscriptions.push(cx.on_app_quit(|this, cx| {
            this.autosave_now(None, cx);
            secrets::flush(cx, std::time::Duration::from_secs(2));
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
            reveal_focus: 0,
            font_picker: None,
            themes: crate::theme_settings::ThemeUi::new(crate::theme::current(cx).dark),
            line_height_tracks: Default::default(),
            opacity_tracks: Default::default(),
            opacity_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
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
            forget_pending: Vec::new(),
            host_change_open: false,
            connecting: false,
            store_waits: 0,
            field_error: None,
            avatar_feedback: None,
            avatar_refresh: Vec::new(),
            avatar_file_read: None,
            avatar_upload: AttachmentFlow::default(),
            avatar_editor: None,
            auto_join: None,
            avatar_opening: false,
            avatar_quick_open: false,
            avatar_toggle_focus: cx.focus_handle().tab_stop(true),
            avatar_upload_registers: None,
            avatar_setup_link: false,
            avatar_return: None,
            _field_subscriptions: field_subscriptions,
            connected_shown: false,
            log_status_shown: None,
            autostart: None,
            autostart_busy: false,
            autostart_generation: 0,
            _subscriptions: subscriptions,
        };
        let avatar_url = this.settings.avatar_url.clone();
        this._subscriptions.push(cx.subscribe_in(
            &avatar_url,
            window,
            |this, _, paste: &crate::input::ClipboardImagePaste, window, cx| {
                this.paste_avatar_image(&paste.0, window, cx);
            },
        ));
        // Without a server the form shows no fields, so the focus waits on
        // the page itself and Escape still reaches the window.
        if this.settings.values.servers.is_empty() {
            window.focus(&this.nav_focus);
        }
        this.refresh_autostart(cx);
        this.probe_system_store(cx);
        this.refresh_upload_account(cx);
        this
    }

    /// Schedules the autosave after an edit to any text field and refreshes
    /// the pending-changes notes. A server field is only saved once it loses
    /// focus (or the window is left or the server is connected), so a value
    /// still being typed is never applied.
    fn observe_fields(
        settings: &SettingsForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        let mut subscriptions = Vec::new();
        for field in settings.server_text_fields() {
            let handle = field.focus_handle(cx);
            subscriptions.push(cx.on_blur(&handle, window, |this, window, cx| {
                this.autosave_now(Some(window), cx)
            }));
        }
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
    /// The toolbar's and the menu bar's visibility, which the chat window's
    /// View menu changes without this window, are taken over the same way.
    /// The notes of saved passwords are taken over too: a connection that
    /// saved one (D068) wrote the file, and a save from this window must not
    /// write the older note back.
    fn follow_saved_server_order(&mut self, cx: &mut Context<Self>) {
        let Ok(Some(mut file)) = settings_file::load() else {
            return;
        };
        // Notes the chat window corrected and has not written are newer.
        let _ = self
            .owner
            .update(cx, |owner, _, _| owner.overlay_pending_notes(&mut file));
        follow_password_notes(&mut self.saved.servers, &file.servers);
        follow_password_notes(&mut self.settings.values.servers, &file.servers);
        // A SASL account corrected in the connection dialog (D068) is taken
        // over unless this window edited it too.
        let shown = self
            .settings
            .values
            .selected_profile()
            .map(|p| p.id.clone());
        for theirs in &file.servers {
            let Some(old) = self
                .saved
                .profile(&theirs.id)
                .map(|profile| profile.sasl_username.clone())
                .filter(|old| *old != theirs.sasl_username)
            else {
                continue;
            };
            if let Some(profile) = self.saved.profile_mut(&theirs.id) {
                profile.sasl_username.clone_from(&theirs.sasl_username);
            }
            if let Some(profile) = self.settings.values.profile_mut(&theirs.id)
                && profile.sasl_username == old
            {
                profile.sasl_username.clone_from(&theirs.sasl_username);
                if shown.as_deref() == Some(theirs.id.as_str()) {
                    self.settings
                        .sasl_username
                        .update(cx, |field, cx| field.set_text(&theirs.sasl_username, cx));
                }
            }
        }
        self.follow_channel_values(&file.servers, cx);
        // The channel dialog writes the channels' log looks (#928); this
        // window never edits them, but its save writes the whole file back.
        for appearance in [
            &mut self.saved.appearance,
            &mut self.settings.values.appearance,
        ] {
            appearance
                .channel_looks
                .clone_from(&file.appearance.channel_looks);
        }
        if let Some(profile) = self.settings.values.selected_profile() {
            let (server, sasl) = saved_passwords(profile);
            for (which, saved) in [(Credential::Server, server), (Credential::Sasl, sasl)] {
                let (shown, changing) = self.settings.credential_state(which);
                if shown != saved && !changing {
                    self.settings.set_saved(which, saved, &self.i18n, cx);
                }
            }
        }
        follow_file_flag(
            &mut self.settings.values.show_toolbar,
            &mut self.saved.show_toolbar,
            file.show_toolbar,
        );
        // The View menu switches it; this window never edits it, but its
        // save writes the whole file back.
        follow_file_flag(
            &mut self.settings.values.connection_diagnostics,
            &mut self.saved.connection_diagnostics,
            file.connection_diagnostics,
        );
        follow_file_flag(
            &mut self.settings.values.menu_bar_auto_hide,
            &mut self.saved.menu_bar_auto_hide,
            file.menu_bar_auto_hide,
        );
        if follow_server_order(&mut self.saved.servers, &file.servers) {
            follow_server_order(&mut self.settings.values.servers, &file.servers);
        }
    }

    /// Takes over the per-channel encodings and the auto-join list the file
    /// holds, which `/charset` and the chat window's channel dialog write
    /// without this window. Each of the two is taken whole, unless the form
    /// holds a change of it that is not saved yet; that change then wins over
    /// the file's value as a whole.
    fn follow_channel_values(&mut self, file: &[ServerProfile], cx: &mut Context<Self>) {
        let shown = self
            .settings
            .values
            .selected_profile()
            .map(|profile| profile.id.clone());
        let mut shown_list = None;
        for theirs in file {
            let Some(saved) = self.saved.profile_mut(&theirs.id) else {
                continue;
            };
            let old = std::mem::replace(&mut saved.channels, theirs.channels.clone());
            let old_charsets =
                std::mem::replace(&mut saved.channel_charsets, theirs.channel_charsets.clone());
            let is_shown = shown.as_deref() == Some(theirs.id.as_str());
            let edited = if is_shown {
                self.settings.channels.read(cx).text().trim() != old
            } else {
                self.settings
                    .values
                    .profile(&theirs.id)
                    .is_some_and(|profile| profile.channels != old)
            };
            if let Some(profile) = self.settings.values.profile_mut(&theirs.id) {
                if profile.channel_charsets == old_charsets {
                    profile
                        .channel_charsets
                        .clone_from(&theirs.channel_charsets);
                }
                if !edited {
                    profile.channels.clone_from(&theirs.channels);
                }
            }
            if is_shown && !edited && theirs.channels != old {
                shown_list = Some(theirs.channels.clone());
            }
        }
        if let Some(list) = shown_list {
            self.settings
                .channels
                .update(cx, |field, cx| field.set_text(&list, cx));
            if self.auto_join.is_some() {
                let rows = cayenchat_storage::parse_auto_join(&list)
                    .iter()
                    .map(|entry| self.auto_join_row(&entry.name, entry.enabled, cx))
                    .collect();
                if let Some(dialog) = &mut self.auto_join {
                    dialog.rows = rows;
                }
            }
            cx.notify();
        }
    }

    /// Shows the "keep on top" value the toolbar's button just saved, so the
    /// checkbox follows it and a later save does not put the old one back.
    /// Other edits in the form are kept.
    #[cfg(target_os = "windows")]
    pub(crate) fn follow_saved_file(&mut self, cx: &mut Context<Self>) {
        if let Ok(Some(file)) = settings_file::load() {
            self.saved.always_on_top = file.always_on_top;
            self.settings.values.always_on_top = file.always_on_top;
            cx.notify();
        }
    }

    fn reload_changed_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_saved_server_order(cx);
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
            window.os_set_title(&self.i18n.text("settings_title"));
        }
        self.settings = SettingsForm::new(loaded.clone(), &self.i18n, cx);
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

    /// Forgets the passwords of profiles removed since `previous` (what this
    /// window last saved, not the file, which another process may have added
    /// servers to), and writes the settings file. Passwords are stored only
    /// by their own Save (#635), never here.
    fn commit_settings(
        &mut self,
        previous: &Settings,
        mut settings: Settings,
        cx: &mut Context<Self>,
    ) -> Result<Settings, String> {
        settings.servers.retain(|server| !server.host.is_empty());
        let previous_logging = diagnostics::configuration();
        diagnostics::configure(&settings.experimental)
            .map_err(|error| self.i18n.format("debug_log_error", &[("error", &error)]))?;
        // The notes the chat window corrected without writing ride along.
        let _ = self
            .owner
            .update(cx, |owner, _, _| owner.overlay_pending_notes(&mut settings));
        if let Err(error) = settings_file::save(&settings) {
            let _ = diagnostics::configure(&previous_logging);
            return Err(error);
        }
        let _ = self.owner.update(cx, |owner, _, _| owner.notes_written());
        // After the save: a failed save must not cost the secrets of profiles
        // the file still holds (#760). The deletion is done by the store
        // thread; one that fails is tried again by the next Save.
        self.forget_removed(removed_secret_keys(previous, &settings), cx);
        Ok(settings)
    }

    /// The saved servers whose host `settings` changes while passwords sent
    /// as they are may still be stored for the old one (#973).
    fn host_changes(&self, settings: &Settings) -> Vec<HostChange> {
        settings
            .servers
            .iter()
            .filter_map(|server| {
                let saved = self.saved.profile(&server.id)?;
                (saved.host_differs(&server.host)
                    && (saved.has_host_bound_secrets() || saved.channel_key_scan_pending()))
                .then(|| HostChange {
                    id: saved.id.clone(),
                    old: saved.host.clone(),
                    new: server.host.clone(),
                    keys: saved.host_bound_secret_keys(),
                    ask: saved.has_host_bound_secrets(),
                })
            })
            .collect()
    }

    /// Asks (when passwords are saved), deletes the old host's passwords and
    /// then saves again, which writes the new host. Cancelling, or a password
    /// that cannot be deleted, keeps the old host.
    fn start_host_change(
        &mut self,
        changes: Vec<HostChange>,
        then: AfterHostChange,
        cx: &mut Context<Self>,
    ) {
        if std::mem::replace(&mut self.host_change_open, true) {
            return;
        }
        let window = self.window;
        let title = self.i18n.text("host_change_title");
        let confirm = self.i18n.text("host_change_confirm");
        let cancel = self.i18n.text("cancel");
        // A change whose notes do not tell yet (first channel key scan
        // pending) is asked about when the store holds one of its entries.
        let probe: Vec<Vec<SecretKey>> = changes
            .iter()
            .filter(|change| !change.ask)
            .map(|change| change.keys.clone())
            .collect();
        let detail_of = |i18n: &Localizer, change: &HostChange| {
            i18n.format(
                "host_change_detail",
                &[("old", &change.old), ("new", &change.new)],
            )
        };
        let asked = changes
            .iter()
            .find(|change| change.ask)
            .map(|change| detail_of(&self.i18n, change));
        let probed_detail: Vec<String> = changes
            .iter()
            .filter(|change| !change.ask)
            .map(|change| detail_of(&self.i18n, change))
            .collect();
        self.store_wait(cx);
        cx.spawn(async move |this, cx| {
            let mut detail = asked;
            if detail.is_none() && !probe.is_empty() {
                let found = this
                    .update(cx, |_, cx| {
                        secrets::run(cx, move |store| {
                            probe.into_iter().position(|keys| {
                                // A key that cannot be read counts as saved.
                                keys.iter().any(|key| !matches!(store.get(key), Ok(None)))
                            })
                        })
                    })
                    .ok();
                if let Some(found) = found {
                    // The store thread stopped: nothing is deleted, so ask.
                    let at = found.await.unwrap_or(Some(0));
                    detail = at.and_then(|at| probed_detail.get(at).cloned());
                }
            }
            let confirmed = match detail {
                None => true,
                Some(detail) => {
                    let answer = cx
                        .update_window(window, |_, window, cx| {
                            window.os_prompt(
                                PromptLevel::Warning,
                                &title,
                                Some(&detail),
                                &[PromptButton::ok(confirm), PromptButton::cancel(cancel)],
                                cx,
                            )
                        })
                        .ok();
                    match answer {
                        Some(answer) => answer.await == Ok(0),
                        None => false,
                    }
                }
            };
            let keys: Vec<SecretKey> = changes
                .iter()
                .flat_map(|change| change.keys.clone())
                .collect();
            let deleting = if confirmed {
                this.update(cx, |_, cx| {
                    secrets::run(cx, move |store| delete_secrets(keys, store))
                })
                .ok()
            } else {
                None
            };
            let failed =
                match deleting {
                    Some(deleting) => Some(deleting.await.unwrap_or_else(|| {
                        vec![(SecretKey::server_password(""), secrets::stopped())]
                    })),
                    None => None,
                };
            let _ = cx.update_window(window, |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.store_done(cx);
                    this.host_change_done(&changes, confirmed, failed, then, window, cx);
                })
            });
        })
        .detach();
    }

    /// The end of [`Self::start_host_change`].
    fn host_change_done(
        &mut self,
        changes: &[HostChange],
        confirmed: bool,
        failed: Option<Vec<(SecretKey, cayenchat_storage::CredentialError)>>,
        then: AfterHostChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.host_change_open = false;
        let failure = failed.and_then(|failed| failed.into_iter().next());
        if confirmed && failure.is_none() {
            for change in changes {
                let forget = |profile: &mut ServerProfile| profile.forget_host_bound_secrets();
                if let Some(profile) = self.settings.values.profile_mut(&change.id) {
                    forget(profile);
                }
                if let Some(profile) = self.saved.profile_mut(&change.id) {
                    forget(profile);
                }
                let _ = self.owner.update(cx, |owner, _, _| {
                    owner.update_profile_note(&change.id, |profile| {
                        forget(profile);
                        true
                    });
                });
            }
            // Written at once: a restart must not find notes of passwords
            // that are gone.
            let _ = self
                .owner
                .update(cx, |owner, _, _| owner.write_pending_notes());
            self.autosave_error = None;
            match then {
                AfterHostChange::Save => self.save_servers(cx),
                AfterHostChange::Autosave => {
                    self.autosave_committed(cx);
                }
                // The passwords are gone, so this finds no change left.
                AfterHostChange::Connect => self.connect_from_settings(window, cx),
            }
            return;
        }
        // The old host stays, in the form too.
        for change in changes {
            if let Some(profile) = self.settings.values.profile_mut(&change.id) {
                profile.host.clone_from(&change.old);
            }
            if self.settings.values.selected_server == change.id {
                self.settings
                    .custom_host
                    .update(cx, |field, cx| field.set_text(&change.old, cx));
            }
        }
        self.autosave_error = None;
        self.feedback = Some(match failure {
            Some((_, error)) => self.i18n.format(
                "host_change_failed",
                &[("error", &secrets::error_text(&self.i18n, &error))],
            ),
            None => self.i18n.text("host_change_cancelled"),
        });
        cx.notify();
    }

    /// Queues the deletion of the secrets of removed servers (and those a
    /// earlier deletion failed for) on the store thread.
    fn forget_removed(&mut self, keys: Vec<SecretKey>, cx: &mut Context<Self>) {
        self.forget_pending.extend(keys);
        self.forget_pending.sort();
        self.forget_pending.dedup();
        if self.forget_pending.is_empty() {
            return;
        }
        let keys = std::mem::take(&mut self.forget_pending);
        let retry = keys.clone();
        let deleting = secrets::run(cx, move |store| delete_secrets(keys, store));
        cx.spawn(async move |this, cx| {
            let failed = deleting.await;
            let _ = this.update(cx, |this, cx| match failed {
                Some(failed) => {
                    if let Some((_, error)) = failed.first() {
                        this.autosave_error = Some(this.i18n.format(
                            "credential_delete_failed",
                            &[("error", &secrets::error_text(&this.i18n, error))],
                        ));
                        cx.notify();
                    }
                    this.forget_pending
                        .extend(failed.into_iter().map(|(key, _)| key));
                }
                None => this.forget_pending.extend(retry),
            });
        })
        .detach();
    }

    pub(crate) fn connect_from_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // A second click waits for the first: the store thread is reading.
        if std::mem::replace(&mut self.connecting, true) {
            return;
        }
        self.follow_saved_server_order(cx);
        // The passwords marked on a draft are written first, then what is
        // saved for the server is read, both on the store thread; the
        // connection starts when they are in.
        let marked = self.marked_passwords(cx);
        let clicked = self.settings.values.selected_server.clone();
        self.write_marked_passwords(marked, cx, move |this, written, cx| {
            // Another server shown meanwhile: the click is dropped.
            if this.settings.values.selected_server != clicked {
                this.connecting = false;
                cx.notify();
                return;
            }
            let mut host_changes = Vec::new();
            let read = written.and_then(|()| {
                let mut settings = this.settings.snapshot(cx)?;
                this.drop_other_drafts(&mut settings);
                host_changes = this.host_changes(&settings);
                let profile = settings
                    .selected_profile()
                    .cloned()
                    .ok_or_else(|| this.i18n.text("server_required"))?;
                Ok(profile)
            });
            let profile = match read {
                Ok(profile) => profile,
                Err(error) => {
                    this.connecting = false;
                    this.feedback = Some(error);
                    cx.notify();
                    return;
                }
            };
            if !host_changes.is_empty() {
                // The old host's passwords are deleted before anything is
                // read for the connection; it goes on afterwards (#973).
                this.connecting = false;
                this.start_host_change(host_changes, AfterHostChange::Connect, cx);
                return;
            }
            this.read_for_connect(profile, cx);
        });
    }

    /// Reads on the store thread what is saved for `profile`, then goes on
    /// with Connect.
    fn read_for_connect(&mut self, profile: ServerProfile, cx: &mut Context<Self>) {
        let reading = crate::read_stored_connections(cx, vec![profile.clone()]);
        let handle = self.window;
        cx.spawn(async move |this, cx| {
            let stored = reading.await.pop();
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.connect_with_stored(&profile.id, stored, window, cx)
                })
            });
        })
        .detach();
    }

    /// The rest of `connect_from_settings` once what is saved for the server
    /// has been read. The form is taken again: edits made meanwhile count,
    /// but another server shown now means the click is dropped.
    fn connect_with_stored(
        &mut self,
        profile: &str,
        stored: Option<crate::StoredConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connecting = false;
        if self.settings.values.selected_server != profile {
            return;
        }
        // The profile was edited while the read was queued (another
        // authentication mechanism, say): what was read does not fit it, so
        // it is read again instead of being applied.
        if let Some(stored) = &stored {
            let current = self.settings.snapshot(cx).ok().and_then(|mut settings| {
                self.drop_other_drafts(&mut settings);
                settings.selected_profile().cloned()
            });
            if let Some(current) = current
                && !stored.fits(&current)
            {
                self.connecting = true;
                self.read_for_connect(current, cx);
                return;
            }
        }
        let result = (|| {
            let stored =
                stored.ok_or_else(|| secrets::error_text(&self.i18n, &secrets::stopped()))?;
            let mut settings = self.settings.snapshot(cx)?;
            self.drop_other_drafts(&mut settings);
            // The host was edited while the read was queued: what was read
            // belongs to the old one. Connect again to be asked.
            if !self.host_changes(&settings).is_empty() {
                return Err(self.i18n.text("host_change_title"));
            }
            let (config, missing) =
                self.settings
                    .connection_config(&mut settings, stored, &self.i18n, cx)?;
            self.autosave = None;
            // `saved` follows only once the file is written, so a failed
            // Connect leaves the draft unsaved.
            let previous = self.saved.clone();
            let settings = self.commit_settings(&previous, settings, cx)?;
            self.saved = settings.clone();
            self.settings.values = settings.clone();
            Ok::<_, String>((config, missing, settings, previous))
        })();
        self.feedback = match result {
            Ok((config, missing, settings, previous)) => {
                // Connecting may have taken over a pending autosave, so what
                // it saved, such as always-on-top, is applied here whether or
                // not the connection succeeds.
                self.apply_to_chat_window(&previous, settings.clone(), false, cx);
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
                    owner.connect_profile(&profile, config, missing, chat_window, cx)
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
            .or_else(|| self.field_error.clone())
            .or_else(|| self.autosave_error.clone())
    }

    /// Restarts the pause before edited settings are saved.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        let window = self.window;
        self.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_DELAY).await;
            let _ = cx.update_window(window, |_, _, cx| {
                this.update(cx, |this, cx| this.autosave_pause_elapsed(cx))
            });
        }));
    }

    /// Whether the shown server is in the saved list. One that is not is a
    /// draft: it is saved by the Save button only and cannot be connected.
    pub(crate) fn shown_server_registered(&self) -> bool {
        self.saved
            .profile(&self.settings.values.selected_server)
            .is_some()
    }

    /// Takes the shown server's typed text over once it is valid. While it
    /// is not, the fields keep what was typed, the saved server stays as it
    /// was and the reason is shown.
    fn commit_shown_server(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let result = self
            .settings
            .commit_server_text(self.shown_server_registered(), cx);
        let error = result.clone().err();
        if self.field_error != error {
            self.field_error = error;
            cx.notify();
        }
        result
    }

    /// The pause after the last edit is over: saves the settings, with the
    /// servers as already committed. Server text still being typed waits
    /// until its field loses focus.
    pub(crate) fn autosave_pause_elapsed(&mut self, cx: &mut Context<Self>) {
        self.autosave = None;
        self.autosave_committed(cx);
    }

    /// A text field was left, the window loses activation or is closed, or
    /// the app quits: commits the shown server's text and saves everything
    /// that is pending.
    pub(crate) fn autosave_now(&mut self, _window: Option<&Window>, cx: &mut Context<Self>) {
        self.autosave = None;
        let _ = self.commit_shown_server(cx);
        self.autosave_committed(cx);
    }

    /// Saves before the window closes or the app quits. Returns whether
    /// that may go on: when saving fails, the form and the error stay so the
    /// edits can be retried; nothing is discarded without a successful save.
    ///
    /// Every way out passes here (the title bar, Escape, Back, a quit): a
    /// Save or a credential change that waits for the store thread ends in
    /// this window, so leaving before it is in would lose its result (a
    /// server registration, the note that a password or key is saved). The
    /// attempt is refused and can be made again a moment later.
    pub(crate) fn save_before_leaving(&mut self, cx: &mut Context<Self>) -> bool {
        if self.store_busy() {
            return false;
        }
        self.autosave = None;
        let _ = self.commit_shown_server(cx);
        self.autosave_committed(cx)
    }

    /// Whether the store thread still owes this window a result.
    pub(crate) fn store_busy(&self) -> bool {
        self.store_waits > 0
    }

    /// Starts waiting for a store job; `store_done` ends it.
    pub(crate) fn store_wait(&mut self, cx: &mut Context<Self>) {
        self.store_waits += 1;
        cx.notify();
    }

    pub(crate) fn store_done(&mut self, cx: &mut Context<Self>) {
        self.store_waits = self.store_waits.saturating_sub(1);
        cx.notify();
    }

    fn autosave_committed(&mut self, cx: &mut Context<Self>) -> bool {
        // A job of the store thread may still change what the form holds
        // (the credential backend a move records in the file before the form
        // learns of it). Saving now could write the old value back, so the
        // save waits until the store thread is done.
        if self.store_busy() {
            self.schedule_autosave(cx);
            return false;
        }
        match self.save_settings(SaveMode::Committed, cx) {
            Ok(()) => {
                if self.autosave_error.take().is_some() {
                    cx.notify();
                }
                true
            }
            Err(error) => {
                if self.autosave_error.as_ref() != Some(&error) {
                    self.autosave_error = Some(error);
                    cx.notify();
                }
                false
            }
        }
    }

    /// Whether a draft server has not been written yet. Edits to a
    /// registered server are saved as they are made, so they are never
    /// pending for long.
    pub(crate) fn servers_unsaved(&self, cx: &App) -> bool {
        let Ok(settings) = self.settings.snapshot(cx) else {
            return true;
        };
        settings.servers != self.saved.servers
            || settings.selected_server != self.saved.selected_server
    }

    /// The Save button of a new server: writes the servers along with everything else. A draft
    /// becomes a registered server once this succeeds; if it fails, it stays
    /// a draft.
    pub(crate) fn save_servers(&mut self, cx: &mut Context<Self>) {
        // A second click waits for the first.
        if self.store_busy() {
            return;
        }
        self.autosave = None;
        // The passwords marked on a draft are written first, on the store
        // thread; the settings follow when they are in. The shown server
        // cannot change meanwhile (`switch_server`).
        let marked = self.marked_passwords(cx);
        self.write_marked_passwords(marked, cx, move |this, written, cx| {
            match written.and_then(|()| this.save_settings(SaveMode::Explicit, cx)) {
                Ok(()) => {
                    this.autosave_error = None;
                    this.field_error = None;
                    this.feedback = None;
                }
                Err(error) => this.feedback = Some(error),
            }
            cx.notify();
        });
    }

    /// Removes the drafts other than the shown server from `settings`: the
    /// Save button registers the server it belongs to and nothing else.
    fn drop_other_drafts(&self, settings: &mut Settings) {
        let shown = settings.selected_server.clone();
        settings
            .servers
            .retain(|server| server.id == shown || self.saved.profile(&server.id).is_some());
    }

    /// Writes the form. `Committed` leaves out the shown draft server and
    /// whatever is only typed into the server fields, and stores no
    /// passwords; `Explicit` writes the servers as shown.
    fn save_settings(&mut self, mode: SaveMode, cx: &mut Context<Self>) -> Result<(), String> {
        self.follow_saved_server_order(cx);
        let explicit = mode == SaveMode::Explicit;
        let mut settings = self.settings.snapshot_with(cx, explicit)?;
        if explicit {
            self.drop_other_drafts(&mut settings);
            if settings
                .selected_profile()
                .is_some_and(|profile| profile.host.is_empty())
            {
                return Err(self.i18n.text("server_required"));
            }
        } else {
            // Registered servers as committed, with the removals and
            // choices made since; a draft is not among them. A hostless
            // server would be dropped from the file along with its
            // passwords, so it is never written.
            settings
                .servers
                .retain(|server| self.saved.profile(&server.id).is_some());
            if settings.servers.iter().any(|server| server.host.is_empty()) {
                return Err(self.i18n.text("server_required"));
            }
            if settings.profile(&settings.selected_server).is_none() {
                let kept = &self.saved.selected_server;
                settings.selected_server = settings
                    .profile(kept)
                    .or(settings.servers.first())
                    .map(|server| server.id.clone())
                    .unwrap_or_default();
            }
        }
        let host_changes = self.host_changes(&settings);
        if !host_changes.is_empty() {
            // The old host's passwords go first; the host is written after.
            let then = if explicit {
                AfterHostChange::Save
            } else {
                AfterHostChange::Autosave
            };
            self.start_host_change(host_changes, then, cx);
            return Err(self.i18n.text("host_change_title"));
        }
        if settings == self.saved && !(explicit && self.servers_unsaved(cx)) {
            // Nothing to write: registering the same URL again still asks
            // for its image to be fetched again.
            self.refresh_registered_avatars(cx);
            return Ok(());
        }
        let previous = std::mem::replace(&mut self.saved, settings.clone());
        let saved = match self.commit_settings(&previous, settings, cx) {
            Ok(saved) => saved,
            Err(error) => {
                // Try again on the next edit.
                self.saved = previous;
                return Err(error);
            }
        };
        if explicit {
            self.saved = saved.clone();
        }
        let servers_changed = previous.servers != self.saved.servers
            || previous.selected_server != self.saved.selected_server;
        self.apply_to_chat_window(&previous, saved, servers_changed, cx);
        self.refresh_registered_avatars(cx);
        Ok(())
    }

    /// After the servers were saved, has the chat window fetch again the
    /// icons registered or updated since the last save. A failed save, or a
    /// server that is still a draft, keeps them pending.
    fn refresh_registered_avatars(&mut self, cx: &mut Context<Self>) {
        let (ready, pending): (Vec<_>, Vec<_>) = std::mem::take(&mut self.avatar_refresh)
            .into_iter()
            .partition(|id| self.saved.profile(id).is_some());
        self.avatar_refresh = pending;
        for id in ready {
            let _ = self
                .owner
                .update(cx, |owner, _, cx| owner.refresh_own_avatar(&id, cx));
        }
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
        let _ = self.owner.update(cx, |owner, window, cx| {
            owner.apply_settings_change(previous, saved, servers_changed, window, cx)
        });
    }
}

impl ChatWindow {
    /// Applies what differs between `previous` and `saved` to this window;
    /// the settings window's saves and the import of a settings file both end
    /// here.
    pub(crate) fn apply_settings_change(
        &mut self,
        previous: &Settings,
        saved: Settings,
        servers_changed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = self;
        let appearance_changed =
            previous.appearance != saved.appearance || previous.theme != saved.theme;
        let language_changed = previous.language != saved.language;
        let layout_changed = previous.restore_window_layout != saved.restore_window_layout;
        #[cfg(target_os = "windows")]
        let topmost_changed = previous.always_on_top != saved.always_on_top;
        let restore_layout = saved.restore_window_layout;
        let confirm_quit = saved.confirm_quit;
        let shortcuts = ShortcutPrefs::from(&saved);
        let shortcuts_changed = ShortcutPrefs::from(previous) != shortcuts;
        let menu_bar_changed = previous.menu_bar_auto_hide != saved.menu_bar_auto_hide;
        let menu_bar_always = !saved.menu_bar_auto_hide;
        let toolbar_changed = previous.show_toolbar != saved.show_toolbar;
        let show_toolbar = saved.show_toolbar;
        let provider = saved.image_upload.provider.clone();
        let shorten_amazon_urls = saved.shorten_amazon_urls;
        let confirm_multiline_paste = saved.confirm_multiline_paste;
        let rules = notification_rules(&saved.notifications);
        {
            owner.image_provider = provider;
            owner.confirm_quit = confirm_quit;
            owner.confirm_server_dblclick_disconnect = saved.confirm_server_dblclick_disconnect;
            owner.confirm_channel_dblclick_part = saved.confirm_channel_dblclick_part;
            owner.set_send_key(saved.send_key, cx);
            owner.unread_log_position = saved.unread_log_position;
            owner.apply_conversation_log(&saved.conversation_log);
            cx.set_global(crate::input::ShortenPastedAmazon(shorten_amazon_urls));
            cx.set_global(crate::input::ConfirmMultilinePaste(confirm_multiline_paste));
            owner.notification_rules = rules;
            #[cfg(target_os = "windows")]
            if topmost_changed {
                owner.apply_always_on_top(saved.always_on_top, window, cx);
            }
            if shortcuts_changed {
                apply_shortcuts(shortcuts, cx);
            }
            if menu_bar_changed {
                owner.set_menu_bar_auto_hide(!menu_bar_always, cx);
            }
            if toolbar_changed {
                owner.set_toolbar_shown(show_toolbar, cx);
            }
            if layout_changed {
                owner.restore_layout = restore_layout;
                // Turning it on remembers where the window is shortly, in the
                // background like any other layout change.
                owner.note_window_bounds(window, cx);
                owner.schedule_layout_save(cx);
            }
            if owner.auto_phrase != saved.experimental.auto_phrase {
                // Shown rows are laid out again on the next draw.
                owner.auto_phrase = saved.experimental.auto_phrase;
                cx.notify();
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
        }
    }
}

impl SettingsWindow {
    /// Closes the window once everything is saved; returns whether it did.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let saved = self.save_before_leaving(cx);
        if saved {
            window.remove_window();
        }
        saved
    }

    /// Whether any field holds uncommitted IME text. GPUI never marks an
    /// Escape keystroke as IME input, so the fields are asked directly.
    fn is_composing(&self, cx: &App) -> bool {
        self.settings
            .text_fields()
            .into_iter()
            .chain([&self.upload_token])
            .chain(
                self.auto_join
                    .iter()
                    .flat_map(|d| d.rows.iter().map(|r| &r.name)),
            )
            .any(|field| field.read(cx).is_composing())
    }

    /// Escape closes what is open inside the window, one layer per press
    /// (a dialog, the avatar editor, a picker, a server list), and only
    /// then the window itself, saved like the close button. A key that is
    /// part of an IME composition, or a recording of a shortcut, is theirs.
    fn escape_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        if keystroke.key != "escape"
            || keystroke.modifiers.modified()
            || keystroke.is_ime_in_progress()
            || self.shortcut_recording.is_some()
            || self.is_composing(cx)
        {
            return;
        }
        cx.stop_propagation();
        if self.auto_join.is_some() {
            self.close_auto_join(window, cx);
        } else if self.avatar_editor.is_some() {
            self.avatar_editor = None;
            cx.notify();
        } else if self.color_picker.is_some() {
            self.color_picker = None;
            cx.notify();
        } else if self.font_picker.is_some() {
            self.font_picker = None;
            cx.notify();
        } else if self.settings.server_list_open
            || self.settings.ircv3_server_list_open
            || self.settings.encoding_list_open
            || self.settings.sasl_list_open
            || self.settings.language_list_open
        {
            self.settings.server_list_open = false;
            self.settings.ircv3_server_list_open = false;
            self.settings.sasl_list_open = false;
            self.settings.encoding_list_open = false;
            self.settings.language_list_open = false;
            cx.notify();
        } else {
            // When saving failed the window stays, with its error in front.
            if self.close_settings(window, cx) {
                // The chat window takes the keyboard back.
                let _ = self
                    .owner
                    .update(cx, |_, chat_window, _| chat_window.activate_window());
            }
        }
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
                owner.disconnect(network, cx);
            }
        });
        cx.notify();
    }

    fn connect_action(&mut self, _: &Connect, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.owner.update(cx, |owner, window, cx| {
            if let Some(network) = owner.selected_network_id() {
                owner.reconnect(network, window, cx)
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

    /// The Save button of a password: the only way (besides the same choice
    /// in the dialog that asks for it at connect time) a password reaches
    /// the credential store. Autosave and Save of the server never do.
    pub(crate) fn save_credential(
        &mut self,
        which: Credential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self
            .settings
            .credential_field(which)
            .read(cx)
            .text()
            .to_owned();
        if text.is_empty() {
            self.feedback = Some(self.i18n.text("credential_enter"));
            cx.notify();
            return;
        }
        // The password belongs to a saved server: a server that is not
        // saved yet may never be, which would leave the password behind.
        let Some(profile) = self
            .settings
            .values
            .selected_profile()
            .filter(|profile| self.saved.profile(&profile.id).is_some())
            .cloned()
        else {
            self.feedback = Some(self.i18n.text("credential_server_unsaved"));
            cx.notify();
            return;
        };
        if secrets::store(cx).kind() == CredentialBackendKind::System {
            self.store_credential(which, &profile.id, Secret::new(text), cx);
            return;
        }
        let answer = self.ask_plaintext_storage(window, cx);
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    // What is typed now is what was agreed to only if the
                    // same server is still shown.
                    if this.settings.values.selected_server == profile.id {
                        let text = this
                            .settings
                            .credential_field(which)
                            .read(cx)
                            .text()
                            .to_owned();
                        if !text.is_empty() {
                            this.store_credential(which, &profile.id, Secret::new(text), cx);
                        }
                    }
                });
            }
        })
        .detach();
    }

    /// The local file holds a password unencrypted; the choice of that
    /// storage was the user's, but each password asks once more.
    fn ask_plaintext_storage(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> futures_channel::oneshot::Receiver<usize> {
        window.os_prompt(
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
        )
    }

    /// The "save password" box of an unsaved password. Checking it only
    /// marks the password: the settings' save writes it (#635). With the
    /// local file as the storage, checking asks for confirmation first.
    pub(crate) fn toggle_remember(
        &mut self,
        which: Credential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.remember(which) {
            self.settings.set_remember(which, false);
        } else if secrets::store(cx).kind() == CredentialBackendKind::System {
            self.settings.set_remember(which, true);
            self.schedule_autosave(cx);
        } else {
            let profile = self.settings.values.selected_server.clone();
            let answer = self.ask_plaintext_storage(window, cx);
            cx.spawn(async move |this, cx| {
                if answer.await == Ok(0) {
                    let _ = this.update(cx, |this, cx| {
                        if this.settings.values.selected_server == profile {
                            this.settings.set_remember(which, true);
                            this.schedule_autosave(cx);
                            cx.notify();
                        }
                    });
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// The passwords whose box is checked on the shown server when it is a
    /// draft that its own Save or Connect is about to register. A registered
    /// server has no box: its passwords are written by the Save button only,
    /// never by an autosave.
    fn marked_passwords(&self, cx: &App) -> Vec<(Credential, Secret)> {
        if self.shown_server_registered() {
            return Vec::new();
        }
        let Some(profile) = self
            .settings
            .values
            .profile(&self.settings.values.selected_server)
        else {
            return Vec::new();
        };
        [Credential::Server, Credential::Sasl]
            .into_iter()
            .filter(|which| *which != Credential::Sasl || profile.sasl_enabled)
            .filter_map(|which| {
                let (saved, changing) = self.settings.credential_state(which);
                let text = self.settings.credential_field(which).read(cx).text();
                (!(saved || changing || text.is_empty() || !self.settings.remember(which)))
                    .then(|| (which, Secret::new(text)))
            })
            .collect()
    }

    /// Writes `marked` on the store thread, in order, up to the first
    /// failure. `then` gets the outcome, noted as `marked_passwords_written`.
    fn write_marked_passwords(
        &mut self,
        marked: Vec<(Credential, Secret)>,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Result<(), String>, &mut Context<Self>) + 'static,
    ) {
        if marked.is_empty() {
            then(self, Ok(()), cx);
            return;
        }
        let shown = self.settings.values.selected_server.clone();
        self.store_wait(cx);
        let keys: Vec<_> = marked.iter().map(|(which, _)| which.key(&shown)).collect();
        let writing = secrets::run(cx, move |store| {
            let mut written = Vec::new();
            for ((which, secret), key) in marked.iter().zip(&keys) {
                let result = store.set(key, secret);
                let failed = result.is_err();
                written.push((*which, result));
                if failed {
                    break;
                }
            }
            written
        });
        cx.spawn(async move |this, cx| {
            let written = writing.await;
            let _ = this.update(cx, |this, cx| {
                this.store_done(cx);
                let outcome = this.marked_passwords_written(&shown, written, cx);
                then(this, outcome, cx);
            });
        })
        .detach();
    }

    /// Notes the passwords the store thread wrote as saved. A failure stops
    /// the save and keeps what was typed.
    fn marked_passwords_written(
        &mut self,
        shown: &str,
        written: Option<Vec<(Credential, Result<(), cayenchat_storage::CredentialError>)>>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let failed = |this: &Self, error: cayenchat_storage::CredentialError| {
            this.i18n.format(
                "credential_save_failed",
                &[("error", &secrets::error_text(&this.i18n, &error))],
            )
        };
        let written = written.ok_or_else(|| failed(self, secrets::stopped()))?;
        for (which, result) in written {
            result.map_err(|error| failed(self, error))?;
            if let Some(profile) = self.settings.values.profile_mut(shown) {
                which.note(profile, true);
            }
            // Only while the form still shows the server it was typed for.
            if self.settings.values.selected_server == shown {
                self.settings.set_saved(which, true, &self.i18n, cx);
            }
        }
        Ok(())
    }

    /// Writes the password. A failure is reported and leaves a saved
    /// password as it was.
    fn store_credential(
        &mut self,
        which: Credential,
        profile: &str,
        secret: Secret,
        cx: &mut Context<Self>,
    ) {
        let key = which.key(profile);
        let writing = secrets::run(cx, move |store| store.set(&key, &secret));
        let profile = profile.to_owned();
        self.store_wait(cx);
        cx.spawn(async move |this, cx| {
            let result = writing.await.unwrap_or_else(|| Err(secrets::stopped()));
            let _ = this.update(cx, |this, cx| {
                this.store_done(cx);
                this.credential_done(which, &profile, result, true, cx)
            });
        })
        .detach();
    }

    /// Shows the outcome of a store write.
    pub(crate) fn credential_done(
        &mut self,
        which: Credential,
        profile: &str,
        result: Result<(), cayenchat_storage::CredentialError>,
        saved: bool,
        cx: &mut Context<Self>,
    ) {
        let (done, failed) = if saved {
            ("credential_saved_done", "credential_save_failed")
        } else {
            ("credential_deleted_done", "credential_delete_failed")
        };
        match result {
            Ok(()) => {
                if self.settings.values.selected_server == profile {
                    self.settings.set_saved(which, saved, &self.i18n, cx);
                }
                // The settings file notes it, so showing the server later
                // needs no read of the store.
                if let Some(server) = self.settings.values.profile_mut(profile) {
                    which.note(server, saved);
                }
                if let Some(server) = self.saved.profile_mut(profile) {
                    which.note(server, saved);
                }
                // Written at once, like a connection's note, so that the
                // file is the newer one when a save follows its notes.
                if let Ok(Some(mut file)) = settings_file::load()
                    && let Some(server) = file.profile_mut(profile)
                {
                    which.note(server, saved);
                    let _ = settings_file::save(&file);
                }
                let _ = self.owner.update(cx, |owner, _, _| {
                    owner.follow_credential_note(profile, which, saved)
                });
                self.schedule_autosave(cx);
                self.feedback = Some(self.i18n.text(done));
            }
            Err(error) => {
                self.feedback = Some(self.i18n.format(
                    failed,
                    &[("error", &secrets::error_text(&self.i18n, &error))],
                ));
            }
        }
        cx.notify();
    }

    /// Change: the field takes the new password; the saved one is kept
    /// until Save succeeds and is never shown.
    pub(crate) fn change_credential(
        &mut self,
        which: Credential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        *self.settings.changing_mut(which) = true;
        window.focus(&self.settings.credential_field(which).focus_handle(cx));
        self.feedback = None;
        cx.notify();
    }

    pub(crate) fn cancel_credential_change(&mut self, which: Credential, cx: &mut Context<Self>) {
        self.settings.set_saved(which, true, &self.i18n, cx);
        self.feedback = None;
        cx.notify();
    }

    /// Delete: removes the saved password. A failure keeps it.
    pub(crate) fn delete_credential(&mut self, which: Credential, cx: &mut Context<Self>) {
        let Some(id) = self
            .settings
            .values
            .selected_profile()
            .map(|profile| profile.id.clone())
        else {
            return;
        };
        let key = which.key(&id);
        let deleting = secrets::run(cx, move |store| store.delete(&key));
        self.store_wait(cx);
        cx.spawn(async move |this, cx| {
            let result = deleting.await.unwrap_or_else(|| Err(secrets::stopped()));
            let _ = this.update(cx, |this, cx| {
                this.store_done(cx);
                this.credential_done(which, &id, result, false, cx)
            });
        })
        .detach();
    }

    /// The saved server the key buttons act on. A server that is not saved
    /// yet may never be, which would leave its key behind.
    fn key_profile(&mut self, cx: &mut Context<Self>) -> Option<ServerProfile> {
        let profile = self
            .settings
            .values
            .selected_profile()
            .filter(|profile| self.saved.profile(&profile.id).is_some())
            .cloned();
        if profile.is_none() {
            self.feedback = Some(self.i18n.text("sasl_key_server_unsaved"));
            cx.notify();
        }
        profile
    }

    /// The text for a key that could not be made or read.
    fn key_error_text(&self, error: KeyError) -> String {
        self.i18n.text(match error {
            KeyError::Random => "sasl_key_error_random",
            KeyError::NotPem => "sasl_key_error_not_pem",
            KeyError::Encrypted => "sasl_key_error_encrypted",
            KeyError::OpenSsh => "sasl_key_error_openssh",
            KeyError::Rsa => "sasl_key_error_rsa",
            KeyError::PublicOnly => "sasl_key_error_public_only",
            KeyError::UnsupportedBlock => "sasl_key_error_unsupported",
            KeyError::NotP256 => "sasl_key_error_not_p256",
            KeyError::Stored => "sasl_key_error_stored",
        })
    }

    /// Generate: a new key from the operating system's random numbers.
    fn generate_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback = None;
        let Some(target) = self.key_profile(cx).map(|profile| profile.id) else {
            return;
        };
        match cayenchat_irc_core::generate_sasl_key() {
            Ok(secret) => self.offer_key(&target, secret, window, cx),
            Err(error) => {
                self.feedback = Some(self.key_error_text(error));
                cx.notify();
            }
        }
    }

    /// Import: an unencrypted PEM file chosen by the user. The file is read
    /// off the UI thread and only its first 16 KiB; what it holds is never
    /// shown, only why it was refused.
    fn import_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback = None;
        // The server the import was started for: the selection may change
        // while the dialog is open or the file is read.
        let Some(target) = self.key_profile(cx).map(|profile| profile.id) else {
            return;
        };
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.i18n.text("sasl_key_import_prompt").into()),
            filter: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let result = cx
                .background_spawn(async move { read_key_file(&path) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.imported_key(&target, result, window, cx)
            });
        })
        .detach();
    }

    fn imported_key(
        &mut self,
        target: &str,
        result: Result<cayenchat_irc_core::SecretText, KeyFileError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(secret) => self.offer_key(target, secret, window, cx),
            Err(KeyFileError::Key(error)) => {
                self.feedback = Some(self.key_error_text(error));
                cx.notify();
            }
            Err(KeyFileError::Io(error)) => {
                self.feedback = Some(
                    self.i18n
                        .format("sasl_key_file_failed", &[("error", &error)]),
                );
                cx.notify();
            }
        }
    }

    /// Stores a new key for the server `target` the import was started for,
    /// not the one shown now (a removed server stores nothing). Replacing a
    /// key asks first (the old one cannot be got back, and its public key is
    /// registered with the network); so does the local file as storage, as
    /// for a password.
    fn offer_key(
        &mut self,
        target: &str,
        secret: cayenchat_irc_core::SecretText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.saved.profile(target).cloned() else {
            self.feedback = Some(self.i18n.text("sasl_key_server_unsaved"));
            cx.notify();
            return;
        };
        let answer = if profile.sasl_key_saved {
            self.confirm(
                window,
                "sasl_key_replace_title",
                self.i18n.text("sasl_key_replace_detail"),
                "sasl_key_replace_confirm",
                cx,
            )
        } else if secrets::store(cx).kind() != CredentialBackendKind::System {
            self.confirm(
                window,
                "sasl_key_plaintext_title",
                self.i18n.format(
                    "sasl_key_plaintext_detail",
                    &[("path", &secrets::local_path_text())],
                ),
                "save_passwords",
                cx,
            )
        } else {
            self.store_key(&profile.id, &secret, cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| this.store_key(&profile.id, &secret, cx));
            }
        })
        .detach();
    }

    fn confirm(
        &self,
        window: &mut Window,
        title: &str,
        detail: String,
        confirm: &str,
        cx: &mut Context<Self>,
    ) -> futures_channel::oneshot::Receiver<usize> {
        window.os_prompt(
            PromptLevel::Warning,
            &self.i18n.text(title),
            Some(&detail),
            &[
                PromptButton::ok(self.i18n.text(confirm)),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        )
    }

    pub(crate) fn store_key(
        &mut self,
        profile: &str,
        secret: &cayenchat_irc_core::SecretText,
        cx: &mut Context<Self>,
    ) {
        let key = SecretKey::sasl_key(profile);
        let secret_text = Secret::new(secret.as_str());
        let writing = secrets::run(cx, move |store| store.set(&key, &secret_text));
        let public = cayenchat_irc_core::sasl_public_key(secret).ok();
        let profile = profile.to_owned();
        self.store_wait(cx);
        cx.spawn(async move |this, cx| {
            let result = writing.await.unwrap_or_else(|| Err(secrets::stopped()));
            let _ = this.update(cx, |this, cx| {
                this.store_done(cx);
                if this.key_done(&profile, result, true, cx)
                    && this.settings.values.selected_server == profile
                {
                    this.settings.sasl_public_key = public;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Delete: removes the key from the store, after a confirmation.
    fn delete_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback = None;
        let Some(profile) = self.key_profile(cx) else {
            return;
        };
        let answer = self.confirm(
            window,
            "sasl_key_delete_title",
            self.i18n.text("sasl_key_delete_detail"),
            "sasl_key_delete_confirm",
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let key = SecretKey::sasl_key(&profile.id);
                let deleting = this
                    .update(cx, |this, cx| {
                        this.store_wait(cx);
                        secrets::run(cx, move |store| store.delete(&key))
                    })
                    .ok();
                let Some(deleting) = deleting else {
                    return;
                };
                let result = deleting.await.unwrap_or_else(|| Err(secrets::stopped()));
                let _ = this.update(cx, |this, cx| {
                    this.store_done(cx);
                    this.key_done(&profile.id, result, false, cx);
                    if this.settings.values.selected_server == profile.id {
                        this.settings.sasl_public_key = None;
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Notes the outcome of a store write or delete of a key like a
    /// password's: in the form, the saved settings and the file, so showing
    /// the server later needs no read of the store.
    fn key_done(
        &mut self,
        profile: &str,
        result: Result<(), cayenchat_storage::CredentialError>,
        saved: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let succeeded = result.is_ok();
        match result {
            Ok(()) => {
                if let Some(server) = self.settings.values.profile_mut(profile) {
                    server.sasl_key_saved = saved;
                }
                if let Some(server) = self.saved.profile_mut(profile) {
                    server.sasl_key_saved = saved;
                }
                if let Ok(Some(mut file)) = settings_file::load()
                    && let Some(server) = file.profile_mut(profile)
                {
                    server.sasl_key_saved = saved;
                    let _ = settings_file::save(&file);
                }
                let _ = self
                    .owner
                    .update(cx, |owner, _, _| owner.follow_key_note(profile, saved));
                self.schedule_autosave(cx);
                self.feedback = Some(self.i18n.text(if saved {
                    "sasl_key_saved_done"
                } else {
                    "sasl_key_deleted_done"
                }));
            }
            Err(error) => {
                self.feedback = Some(self.i18n.format(
                    if saved {
                        "sasl_key_save_failed"
                    } else {
                        "sasl_key_delete_failed"
                    },
                    &[("error", &secrets::error_text(&self.i18n, &error))],
                ));
            }
        }
        succeeded
    }

    /// Reads the stored key (the user asked) and shows its public key.
    fn show_public_key(&mut self, cx: &mut Context<Self>) {
        self.feedback = None;
        let Some(profile) = self.key_profile(cx) else {
            return;
        };
        let key = SecretKey::sasl_key(&profile.id);
        let reading = secrets::run(cx, move |store| store.get(&key));
        cx.spawn(async move |this, cx| {
            let stored = reading.await.unwrap_or_else(|| Err(secrets::stopped()));
            let _ = this.update(cx, |this, cx| {
                let public = match stored {
                    Ok(Some(secret)) => cayenchat_irc_core::sasl_public_key(secret.expose())
                        .map_err(|error| this.key_error_text(error)),
                    Ok(None) => Err(this.i18n.text("sasl_key_missing")),
                    Err(error) => Err(this.i18n.format(
                        "sasl_key_read_failed",
                        &[("error", &secrets::error_text(&this.i18n, &error))],
                    )),
                };
                // Only for the server it was asked for.
                if this.settings.values.selected_server == profile.id {
                    match public {
                        Ok(public) => this.settings.sasl_public_key = Some(public),
                        Err(text) => this.feedback = Some(text),
                    }
                } else if let Err(text) = public {
                    this.feedback = Some(text);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Copy: the public key only.
    fn copy_public_key(&mut self, cx: &mut Context<Self>) {
        if let Some(public) = self.settings.sasl_public_key.clone() {
            cx.write_to_clipboard(ClipboardItem::new_string(public));
            self.feedback = Some(self.i18n.text("sasl_key_copied"));
            cx.notify();
        }
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
        let answer = window.os_prompt(
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
        let swapped = match (port.as_str(), use_tls) {
            ("6667", true) => Some(6697),
            ("6697", false) => Some(6667),
            _ => None,
        };
        if let Some(swapped) = swapped {
            // The port follows the choice, so it is committed with it.
            if let Some(profile) = self.settings.values.selected_profile_mut() {
                profile.port = swapped;
            }
            self.settings
                .port
                .update(cx, |field, cx| field.set_text(&swapped.to_string(), cx));
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

    /// Whether SASL is used is independent of the mechanism. Every mechanism
    /// needs TLS, so turning SASL on turns TLS on, and TLS stays on while
    /// SASL is.
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

    /// Only picks the mechanism; it leaves SASL on or off as it was.
    fn choose_sasl_mechanism(&mut self, mechanism: SaslMechanism, cx: &mut Context<Self>) {
        self.feedback = None;
        self.settings.sasl_list_open = false;
        if let Some(profile) = self.settings.values.selected_profile_mut() {
            profile.sasl_mechanism = mechanism;
        }
        cx.notify();
    }

    pub(crate) fn show_selected_server(&mut self, cx: &mut Context<Self>) {
        self.settings.show_selected(&self.i18n, cx);
        self.feedback = None;
        self.field_error = None;
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
        join_summary(&names, self.i18n.text("auto_join_none"))
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
                        settings_theme::button(("auto-join-settings", index), false, cx)
                            .child(self.i18n.text("channel_settings_button"))
                            .when(
                                !cayenchat_storage::valid_charset_channel(row.name.read(cx).text()),
                                |d| d.opacity(0.4),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_auto_join_channel_settings(index, cx);
                            })),
                    )
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
                    .track_scroll(&dialog.scroll)
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
        self.auto_join = Some(AutoJoinDialog {
            rows,
            focus,
            scroll: ScrollHandle::new(),
        });
        cx.notify();
    }

    /// Done: the channels are saved now, with no Save button of the
    /// Connection tab needed.
    pub(crate) fn close_auto_join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.auto_join = None;
        window.focus(&self.nav_focus);
        self.autosave_now(None, cx);
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
        // Only the dialog's own list follows; the panes behind stay put.
        focus_reveal::clear(cx);
        self.reveal_focus = REVEAL_FRAMES;
        cx.notify();
    }

    /// Tab / Shift+Tab on the settings screen: moves the focus, then scrolls
    /// whichever pane holds the new target by the least amount that shows it.
    fn traverse(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if forward {
            window.focus_next();
        } else {
            window.focus_prev();
        }
        // The dialog covers the panes; nothing behind it needs to follow.
        if self.auto_join.is_none() {
            if self.nav_focus.is_focused(window) {
                self.reveal_selected_tab();
            }
            focus_reveal::clear(cx);
            self.reveal_focus = REVEAL_FRAMES;
            cx.notify();
        }
    }

    /// Scrolls the category list just enough to show the selected category.
    fn reveal_selected_tab(&self) {
        let tabs = Self::settings_tabs();
        let Some(index) = tabs.iter().position(|(tab, ..)| *tab == self.tab) else {
            return;
        };
        // The last category (Experimental) follows a spacer child.
        let spacer = usize::from(index + 1 == tabs.len());
        self.nav_scroll.scroll_to_item(index + spacer);
    }

    /// While a Tab is being followed, scrolls the pane (or the dialog's list)
    /// to the field that has the focus unless it is already in view. The
    /// bounds are those the field painted at its new place, so the frame
    /// right after the Tab finds none and asks for one more.
    fn reveal_focused(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.reveal_focus == 0 {
            return;
        }
        self.reveal_focus -= 1;
        let Some(target) = focus_reveal::focused_bounds(window, cx) else {
            if self.reveal_focus > 0 {
                // Notifying while drawing does not schedule another frame.
                let this = cx.entity();
                window.on_next_frame(move |_, cx| cx.notify(this.entity_id()));
            }
            return;
        };
        self.reveal_focus = 0;
        // With the dialog open, the scrolling region is its list.
        let scroll = match &self.auto_join {
            Some(dialog) => &dialog.scroll,
            None => &self.pane_scroll,
        };
        let offset = scroll.offset();
        let y = focus_reveal::offset_to_reveal(
            scroll.bounds(),
            target,
            offset.y,
            scroll.max_offset().height,
        );
        if y != offset.y {
            scroll.set_offset(point(offset.x, y));
        }
    }

    /// Stores the dialog's rows in the `channels` field and in the shown
    /// server, so the autosave of a registered server writes them without
    /// the field having to lose focus; a draft keeps them until Save.
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
        if let Some(profile) = self.settings.values.selected_profile_mut() {
            profile.channels = text.trim().to_owned();
        }
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

    fn open_auto_join_channel_settings(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(name) = self
            .auto_join
            .as_ref()
            .and_then(|dialog| dialog.rows.get(index))
            .map(|row| row.name.read(cx).text().to_owned())
            .filter(|name| cayenchat_storage::valid_charset_channel(name))
        else {
            return;
        };
        let Some(profile) = self.settings.values.selected_profile() else {
            return;
        };
        let profile_id = profile.id.clone();
        // A server still being added is not known to the chat window.
        if self.saved.profile(&profile_id).is_none() {
            self.feedback = Some(self.i18n.text("channel_settings_server_unsaved"));
            cx.notify();
            return;
        }
        // The channel gets a window of its own, opened by the chat window
        // (which also shows its changes). It saves by itself.
        let _ = self.owner.update(cx, |owner, window, cx| {
            let (mapping, _) = owner.channel_settings_context(&profile_id);
            owner.open_channel_settings_window(&profile_id, &name, mapping, window, cx);
        });
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
        // What the store thread still owes belongs to the server shown.
        if self.store_busy() {
            return;
        }
        // Leaving a registered server takes its typed text over first; text
        // that is not valid keeps the form where it is.
        if let Err(error) = self.commit_shown_server(cx) {
            self.feedback = Some(error);
            cx.notify();
            return;
        }
        self.avatar_feedback = None;
        self.avatar_setup_link = false;
        self.avatar_editor = None;
        self.auto_join = None;
        match self.settings.switch_server(change, &self.i18n, cx) {
            Ok(()) => self.feedback = None,
            Err(error) => self.feedback = Some(error),
        }
        cx.notify();
    }

    /// Removes the selected server. Saving the removal disconnects it and
    /// forgets its passwords, so a saved server asks first.
    fn remove_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.store_busy() {
            return;
        }
        let id = self.settings.values.selected_server.clone();
        let Some(profile) = self.saved.profile(&id).filter(|p| !p.host.is_empty()) else {
            self.settings.values.remove_selected_server();
            self.show_selected_server(cx);
            return;
        };
        let answer = window.os_prompt(
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
                    if !this.store_busy() && this.settings.values.selected_server == id {
                        this.settings.values.remove_selected_server();
                        this.show_selected_server(cx);
                        // Confirmed, so it is saved at once like any
                        // other change to a registered server.
                        this.autosave_now(None, cx);
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
        // A server not in the saved list is a draft being added: only Save
        // registers it, and only a registered server can be connected. A
        // registered server's changes are saved as they are made (#634).
        let adding = profile
            .as_ref()
            .is_some_and(|profile| self.saved.profile(&profile.id).is_none());
        let selected_id = profile.as_ref().map(|profile| profile.id.clone());
        let server_fields = profile
            .as_ref()
            .map(|profile| self.render_server_fields(profile, cx));
        let quick_avatar = profile
            .as_ref()
            .map(|profile| self.render_quick_avatar(profile, cx));
        let reiwa_mode = self.reiwa_mode_row("connection-reiwa-mode", cx);
        let current_host = self.settings.custom_host.read(cx).text().trim().to_owned();
        let current_port = self.settings.port.read(cx).text().trim().to_owned();
        let saved_profile = selected_id.as_ref().and_then(|id| self.saved.profile(id));
        let selected_label = if no_server {
            self.i18n.text("no_servers")
        } else if let Some(saved_profile) = saved_profile {
            // A registered server reads the same as its entry in the list.
            ircv3_settings::server_label(saved_profile, &self.i18n)
        } else if current_host.is_empty() {
            self.i18n.text("new_server")
        } else {
            format!("{current_host}:{current_port}")
        };
        let selected_label = display::neutralize_bidi(&selected_label).into_owned();
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
                    this.settings.sasl_list_open = false;
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
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
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
                    let label = ircv3_settings::server_label(server, &self.i18n);
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
                            .w(settings_theme::LABEL_WIDTH)
                            .flex_shrink_0()
                            .child(self.i18n.text("destination")),
                    )
                    .child(server_selector.flex_1().min_w_0()),
            )
            .when_some(quick_avatar, |d, quick| d.child(quick))
            // App-wide, so outside the icon section and its fold.
            .child(reiwa_mode)
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("reiwa_mode_hint")),
            )
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("reiwa_mode_global_hint")),
            )
            .when_some(server_fields, |d, fields| d.child(fields))
            .when(no_server, |d| {
                d.child(
                    div()
                        .ml(settings_theme::LABEL_INDENT)
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("no_servers_hint")),
                )
            })
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .when(connected && !adding, |d| {
                d.child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("ircv3_next_connection")),
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
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_settings(window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .when(adding, |d| {
                        d.child(
                            settings_theme::button("save-button", false, cx)
                                .debug_selector(|| "save-button".into())
                                .child(self.i18n.text("save"))
                                .on_click(cx.listener(|this, _, _, cx| this.save_servers(cx))),
                        )
                    })
                    .when(!no_server && !adding, |d| {
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
                    this.settings.sasl_list_open = false;
                    this.settings.server_list_open = false;
                    this.settings.language_list_open = false;
                    cx.notify();
                })),
        );
        if self.settings.encoding_list_open {
            let mut menu = div()
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
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
        let sasl_label = profile.sasl_mechanism.name();
        let mut sasl_selector = div().flex().flex_col().child(
            div()
                .id("sasl-select")
                .px_2()
                .py_1()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(format!("{sasl_label}  ▾"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings.sasl_list_open = !this.settings.sasl_list_open;
                    this.settings.server_list_open = false;
                    this.settings.encoding_list_open = false;
                    this.settings.language_list_open = false;
                    cx.notify();
                })),
        );
        if self.settings.sasl_list_open {
            let mut menu = div()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface);
            for (index, choice) in SaslMechanism::ALL.into_iter().enumerate() {
                let selected = choice == profile.sasl_mechanism;
                let label = choice.name();
                menu = menu.child(
                    div()
                        .id(("sasl-option", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .when(selected, |d| d.bg(theme.selected))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose_sasl_mechanism(choice, cx)
                        })),
                );
            }
            sasl_selector = sasl_selector.child(menu);
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
                    .ml(settings_theme::LABEL_INDENT)
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
                    .items_center()
                    .gap_2()
                    .pl(settings_theme::OPTION_INDENT)
                    .child(
                        div()
                            .w(settings_theme::LABEL_WIDTH - settings_theme::OPTION_INDENT)
                            .flex_shrink_0()
                            .child("TLS/SSL"),
                    )
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
                        .pl(settings_theme::OPTION_INDENT * 2.)
                        .child(
                            div()
                                .w(
                                    settings_theme::LABEL_WIDTH
                                        - settings_theme::OPTION_INDENT * 2.,
                                )
                                .flex_shrink_0()
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
                            .ml(settings_theme::LABEL_INDENT)
                            .text_color(theme.warning)
                            .child(self.i18n.text("certificate_warning")),
                    )
                })
            })
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("server_auth_heading")),
            )
            .child(self.credential_row(Credential::Server, "server_password", cx))
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("server_password_hint")),
            )
            .when(!tls, |d| {
                d.child(
                    div()
                        .id("plaintext-pass")
                        .ml(settings_theme::LABEL_INDENT)
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
                            .ml(settings_theme::LABEL_INDENT)
                            .text_color(theme.warning)
                            .child(self.i18n.text("plaintext_pass_warning")),
                    )
                })
            })
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
                    .child(div().w(settings_theme::LABEL_WIDTH).child("SASL"))
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .w(settings_theme::LABEL_WIDTH)
                            .flex_shrink_0()
                            .child(self.i18n.text("sasl_mechanism")),
                    )
                    .child(sasl_selector.flex_1().min_w_0()),
            )
            .when(sasl, |d| {
                d.child(
                    div()
                        .ml(settings_theme::LABEL_INDENT)
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("sasl_tls_note")),
                )
            })
            .when(sasl, |d| {
                d.child(settings_field(
                    &self.i18n.text("sasl_account"),
                    self.settings.sasl_username.clone(),
                ))
                .child(if profile.sasl_mechanism.uses_key() {
                    self.sasl_key_section(profile.sasl_key_saved, cx)
                } else {
                    self.credential_row(Credential::Sasl, "sasl_password", cx)
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
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("username_hint")),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(settings_theme::LABEL_WIDTH)
                            .flex_shrink_0()
                            .child(self.i18n.text("encoding")),
                    )
                    .child(encoding_selector.flex_1().min_w_0()),
            )
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("encoding_hint")),
            )
            .child(
                div()
                    .id("iso2022jp-char-refs")
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(settings_theme::LABEL_WIDTH)
                            .flex_shrink_0()
                            .child(self.i18n.text("iso2022jp_char_refs")),
                    )
                    .child(
                        div().flex().flex_wrap().gap_1().children(
                            CHAR_REF_CHOICES
                                .into_iter()
                                .enumerate()
                                .map(|(index, (mode, key))| {
                                    settings_theme::button(
                                        ("char-refs-mode", index),
                                        profile.char_refs == mode,
                                        cx,
                                    )
                                    .child(self.i18n.text(key))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            if let Some(profile) =
                                                this.settings.values.selected_profile_mut()
                                            {
                                                profile.char_refs = mode;
                                            }
                                            cx.notify();
                                        },
                                    ))
                                }),
                        ),
                    ),
            )
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("iso2022jp_char_refs_hint")),
            )
            .child(settings_field(
                &self.i18n.text("realname"),
                self.settings.realname.clone(),
            ))
            .child(settings_field(
                &self.i18n.text("quit_message"),
                self.settings.quit_message.clone(),
            ))
            .child(settings_field(
                &self.i18n.text("sleep_quit_message"),
                self.settings.sleep_quit_message.clone(),
            ))
            .child(settings_field(
                &self.i18n.text("part_message"),
                self.settings.part_message.clone(),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(settings_theme::LABEL_WIDTH)
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
                    .ml(settings_theme::LABEL_INDENT)
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
                    .id("user-avatars")
                    .ml(settings_theme::LABEL_INDENT)
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(profile.user_avatars, true, cx))
                    .child(self.i18n.text("user_avatars"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(profile) = this.settings.values.selected_profile_mut() {
                            profile.user_avatars = !profile.user_avatars;
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("user_avatars_hint")),
            )
            .child(
                div()
                    .id("regain-ops")
                    .ml(settings_theme::LABEL_INDENT)
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(profile.regain_ops, true, cx))
                    .child(self.i18n.text("regain_ops"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(profile) = this.settings.values.selected_profile_mut() {
                            profile.regain_ops = !profile.regain_ops;
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("regain_ops_hint")),
            )
    }

    /// A password of a draft server: a "save password" box beside the field
    /// (its own Save writes it). Of a registered server: Save beside an empty
    /// field while none is saved; Change and Delete beside "Saved" once one
    /// is; Save and Cancel beside a field while it is being changed. A saved
    /// value is never shown.
    fn credential_row(
        &self,
        which: Credential,
        label_key: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let (saved, changing) = self.settings.credential_state(which);
        let slot = which as usize;
        let row = div()
            .flex()
            .items_center()
            .gap_2()
            .child(settings_theme::label_column().child(self.i18n.text(label_key)));
        if saved && !changing {
            return row
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(self.i18n.text("credential_saved")),
                )
                .child(
                    settings_theme::button(("credential-change", slot), false, cx)
                        .child(self.i18n.text("credential_change"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.change_credential(which, window, cx)
                        })),
                )
                .child(
                    settings_theme::button(("credential-delete", slot), false, cx)
                        .child(self.i18n.text("credential_delete"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.delete_credential(which, cx)),
                        ),
                );
        }
        let row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .child(self.settings.credential_field(which).clone()),
        );
        if !changing && !self.shown_server_registered() {
            // A draft server: the box makes its own Save write the password.
            return row.child(
                div()
                    .id(("credential-remember", slot))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.remember(which),
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("credential_remember"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_remember(which, window, cx)
                    })),
            );
        }
        row.child(
            settings_theme::button(("credential-save", slot), true, cx)
                .child(self.i18n.text("credential_save"))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.save_credential(which, window, cx)),
                ),
        )
        .when(changing, |row| {
            row.child(
                settings_theme::button(("credential-cancel", slot), false, cx)
                    .child(self.i18n.text("cancel"))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.cancel_credential_change(which, cx)),
                    ),
            )
        })
    }

    /// The key management that takes the place of the password row for
    /// ECDSA-NIST256P-CHALLENGE: make, load, show and delete. The private key
    /// is never shown, copied or exported.
    fn sasl_key_section(&self, saved: bool, cx: &mut Context<Self>) -> Div {
        let button = |id: &'static str, label: &'static str, cx: &mut Context<Self>| {
            settings_theme::button(id, false, cx).child(self.i18n.text(label))
        };
        let mut actions = div().flex().flex_wrap().items_center().gap_2().child(
            button("sasl-key-generate", "sasl_key_generate", cx)
                .on_click(cx.listener(|this, _, window, cx| this.generate_key(window, cx))),
        );
        actions = actions.child(
            button("sasl-key-import", "sasl_key_import", cx)
                .on_click(cx.listener(|this, _, window, cx| this.import_key(window, cx))),
        );
        if saved {
            actions = actions
                .child(
                    button("sasl-key-show", "sasl_key_show_public", cx)
                        .on_click(cx.listener(|this, _, _, cx| this.show_public_key(cx))),
                )
                .child(
                    button("sasl-key-delete", "credential_delete", cx)
                        .on_click(cx.listener(|this, _, window, cx| this.delete_key(window, cx))),
                );
        }
        let theme = settings_theme::palette(cx);
        let mut section = div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(settings_theme::label_column().child(self.i18n.text("sasl_key")))
                .child(div().child(self.i18n.text(if saved {
                    "credential_saved"
                } else {
                    "sasl_key_none"
                })))
                .child(actions),
        );
        if let Some(public) = self.settings.sasl_public_key.clone() {
            section =
                section.child(
                    div()
                        .ml(settings_theme::LABEL_INDENT)
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_color(theme.text_secondary)
                                .child(self.i18n.text("sasl_key_public")),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().min_w_0().child(public))
                                .child(button("sasl-key-copy", "sasl_key_copy", cx).on_click(
                                    cx.listener(|this, _, _, cx| this.copy_public_key(cx)),
                                )),
                        ),
                );
        }
        section.child(
            div()
                .ml(settings_theme::LABEL_INDENT)
                .text_color(theme.text_secondary)
                .child(self.i18n.text("sasl_key_note")),
        )
    }

    fn font_input(&self, target: FontTarget) -> Entity<TextInput> {
        match target {
            FontTarget::ChannelLog => self.settings.channel_log_font.clone(),
            FontTarget::CombinedLog => self.settings.combined_log_font.clone(),
            FontTarget::MemberList => self.settings.member_list_font.clone(),
            FontTarget::ChannelTree => self.settings.channel_tree_font.clone(),
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
                    .child(settings_theme::label_column().child(label.to_owned()))
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
                        cx.new(|_| crate::tooltip::TextTooltip(text)).into()
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
            .ml(settings_theme::LABEL_INDENT)
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

    /// The saved line height of the channel log (0) or combined log (1).
    fn line_height_mut(&mut self, log: usize) -> &mut u16 {
        let appearance = &mut self.settings.values.appearance;
        if log == 0 {
            &mut appearance.channel_log_line_height
        } else {
            &mut appearance.combined_log_line_height
        }
    }

    /// Sets a log's line height from a pointer position over its slider.
    fn pick_line_height(&mut self, log: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let track = self.line_height_tracks[log].get();
        // The thumb's center travels from half a thumb in to half a thumb
        // before the end, so the value follows the thumb under the pointer.
        let travel = f32::from(track.size.width) - LINE_HEIGHT_THUMB;
        if travel <= 0. {
            return;
        }
        let fraction = (f32::from(position.x - track.origin.x) - LINE_HEIGHT_THUMB / 2.) / travel;
        let percent = line_height_at(fraction);
        let current = self.line_height_mut(log);
        if *current != percent {
            *current = percent;
            cx.notify();
        }
    }

    /// A labelled slider for a background opacity, `index` in the order
    /// member list, channel tree, channel log, combined log, hover popup.
    fn opacity_slider(&self, index: usize, label_key: &str, cx: &mut Context<Self>) -> Div {
        use cayenchat_storage::{MIN_PANE_OPACITY, MIN_URL_TOOLTIP_OPACITY};
        // The hover popup has its own lower limit.
        let min = if index == 4 {
            MIN_URL_TOOLTIP_OPACITY
        } else {
            MIN_PANE_OPACITY
        };
        let theme = settings_theme::palette(cx);
        let primary = settings_theme::primary_color(cx);
        let appearance = &self.settings.values.appearance;
        let percent = [
            appearance.member_list_opacity,
            appearance.channel_tree_opacity,
            appearance.channel_log_opacity,
            appearance.combined_log_opacity,
            appearance.url_tooltip_opacity,
        ][index]
            .clamp(min, 100);
        let colors = crate::slider::SliderColors {
            track: theme.border,
            fill: primary.into(),
            knob: theme.surface,
            ring: primary.into(),
        };
        let slider = crate::slider::slider(
            ("pane-opacity", index),
            u32::from(percent - min),
            u32::from(100 - min),
            &self.opacity_tracks[index],
            &self.opacity_focus[index],
            colors,
            cx,
            move |this, value, _, _| {
                let appearance = &mut this.settings.values.appearance;
                let value = min + value as u8;
                match index {
                    0 => appearance.member_list_opacity = value,
                    1 => appearance.channel_tree_opacity = value,
                    2 => appearance.channel_log_opacity = value,
                    3 => appearance.combined_log_opacity = value,
                    _ => appearance.url_tooltip_opacity = value,
                }
            },
        );
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(settings_theme::label_column().child(self.i18n.text(label_key)))
            .child(
                div()
                    .w(px(LINE_HEIGHT_TRACK_WIDTH))
                    .flex_shrink_0()
                    .child(slider),
            )
            .child(
                div()
                    .w(px(48.))
                    .flex_shrink_0()
                    .child(format!("{percent}%")),
            )
    }

    /// The line height slider of the channel log (0) or combined log (1)
    /// (#953), with a button back to the default.
    fn line_height_slider(&self, log: usize, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let primary = settings_theme::primary_color(cx);
        let appearance = &self.settings.values.appearance;
        let percent = if log == 0 {
            appearance.channel_log_line_height
        } else {
            appearance.combined_log_line_height
        };
        let fraction = line_height_fraction(percent);
        let owner = cx.entity_id();
        let track = self.line_height_tracks[log].clone();
        let travel = LINE_HEIGHT_TRACK_WIDTH - LINE_HEIGHT_THUMB;
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(settings_theme::label_column().child(self.i18n.text("log_line_height")))
            .child(
                div()
                    .id(("log-line-height", log))
                    .relative()
                    .w(px(LINE_HEIGHT_TRACK_WIDTH))
                    .h(px(20.))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .child(
                        canvas(move |bounds, _, _| track.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(9.))
                            .w_full()
                            .h(px(2.))
                            .bg(theme.border),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(fraction * travel))
                            .top(px(3.))
                            .size(px(LINE_HEIGHT_THUMB))
                            .rounded_full()
                            .border_1()
                            .border_color(primary)
                            .bg(primary),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.pick_line_height(log, event.position, cx)
                        }),
                    )
                    .on_drag(LineHeightDrag(owner, log), |_, _, _, cx| {
                        cx.new(|_| crate::splitter::NoGhost)
                    })
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<LineHeightDrag>, _, cx| {
                            let drag = event.drag(cx);
                            if drag.0 == owner && drag.1 == log {
                                this.pick_line_height(log, event.event.position, cx);
                            }
                        },
                    )),
            )
            .child(
                div()
                    .w(px(48.))
                    .flex_shrink_0()
                    .child(format!("{percent}%")),
            )
            .child(
                div()
                    .id(("log-line-height-reset", log))
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .child(self.i18n.text("line_height_reset"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        *this.line_height_mut(log) = cayenchat_storage::DEFAULT_LINE_HEIGHT_PERCENT;
                        cx.notify();
                    })),
            )
    }

    /// The size input with a button on each side that steps it by 1 px; a
    /// button is disabled (dimmed) at the end of the allowed range.
    fn font_size_stepper(&self, target: FontTarget, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let field = self.settings.font_sizes[target as usize].clone();
        let size = field
            .read(cx)
            .text()
            .trim()
            .parse::<u16>()
            .unwrap_or(cayenchat_storage::MIN_FONT_SIZE);
        let button = |id: &'static str, label: &'static str, delta: i32, enabled: bool| {
            let button = div()
                .id((id, target as u32))
                .w(px(24.))
                .py_1()
                .flex()
                .justify_center()
                .border_1()
                .border_color(theme.border)
                .child(label);
            if enabled {
                button
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings.step_font_size(target, delta, cx);
                        cx.notify();
                    }))
            } else {
                button.text_color(theme.text_secondary).opacity(0.4)
            }
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .flex_shrink_0()
            .child(button(
                "font-size-down",
                "\u{2212}",
                -1,
                size > cayenchat_storage::MIN_FONT_SIZE,
            ))
            .child(div().w(px(56.)).child(field))
            .child(button(
                "font-size-up",
                "+",
                1,
                size < cayenchat_storage::MAX_FONT_SIZE,
            ))
    }

    /// The switch for separate light and dark fonts and, while it is on,
    /// the choice of the theme whose fonts the rows below show.
    fn render_font_mode(&self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let split = self.settings.values.appearance.split_fonts;
        let side = self.settings.font_side_dark;
        let mut block = div().flex().flex_col().gap_1().child(
            div()
                .id("split-fonts")
                .ml(settings_theme::LABEL_INDENT)
                .flex()
                .gap_2()
                .cursor_pointer()
                .child(settings_theme::checkbox(split, true, cx))
                .child(self.i18n.text("split_fonts"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let on = !this.settings.values.appearance.split_fonts;
                    this.settings.set_split_fonts(on, cx);
                    this.font_picker = None;
                    cx.notify();
                })),
        );
        if split {
            let tab = |id: &'static str, key: &str, dark: bool| {
                div()
                    .id(id)
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(side == dark, |d| d.bg(theme.hover))
                    .child(self.i18n.text(key))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings.select_font_side(dark, cx);
                        this.font_picker = None;
                        cx.notify();
                    }))
            };
            block = block.child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .flex()
                    .gap_2()
                    .child(tab("font-side-light", "theme_kind_light", false))
                    .child(tab("font-side-dark", "theme_kind_dark", true)),
            );
        }
        block
    }

    fn font_field(&self, target: FontTarget, label: &str, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let input = self.font_input(target);
        let mut field = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(settings_theme::label_column().child(label.to_owned()))
                .child(div().flex_1().min_w_0().child(input.clone()))
                .child(self.font_size_stepper(target, cx))
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
                .ml(settings_theme::LABEL_INDENT)
                .max_h(px(170.))
                .overflow_y_scroll()
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
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

    fn render_appearance_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
            .child(self.render_theme_settings(cx))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(settings_theme::label_column())
                    .child(div().flex_1().child(self.i18n.text("colors_light")))
                    .child(div().flex_1().child(self.i18n.text("colors_dark"))),
            )
            .child(self.color_pair(
                &self.i18n.text("member_list_background"),
                &self.settings.member_list_background,
                &self.settings.dark_member_list_background,
                cx,
            ))
            .child(self.opacity_slider(0, "member_list_opacity", cx))
            .child(self.color_pair(
                &self.i18n.text("channel_tree_background"),
                &self.settings.channel_tree_background,
                &self.settings.dark_channel_tree_background,
                cx,
            ))
            .child(self.opacity_slider(1, "channel_tree_opacity", cx))
            .child(self.color_pair(
                &self.i18n.text("channel_log"),
                &self.settings.channel_log_background,
                &self.settings.dark_channel_log_background,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("channel_log_alternate"),
                &self.settings.channel_log_alternate,
                &self.settings.dark_channel_log_alternate,
                cx,
            ))
            .child(self.opacity_slider(2, "channel_log_opacity", cx))
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
                &self.settings.combined_log_background,
                &self.settings.dark_combined_log_background,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("combined_log_alternate"),
                &self.settings.combined_log_alternate,
                &self.settings.dark_combined_log_alternate,
                cx,
            ))
            .child(self.opacity_slider(3, "combined_log_opacity", cx))
            .child(self.color_pair(
                &self.i18n.text("link_color"),
                &self.settings.link_color,
                &self.settings.dark_link_color,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("link_background"),
                &self.settings.link_background,
                &self.settings.dark_link_background,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("url_tooltip_color"),
                &self.settings.url_tooltip_color,
                &self.settings.dark_url_tooltip_color,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("url_tooltip_link_color"),
                &self.settings.url_tooltip_link_color,
                &self.settings.dark_url_tooltip_link_color,
                cx,
            ))
            .child(self.color_pair(
                &self.i18n.text("url_tooltip_text_color"),
                &self.settings.url_tooltip_text_color,
                &self.settings.dark_url_tooltip_text_color,
                cx,
            ))
            .child(self.opacity_slider(4, "url_tooltip_opacity", cx))
            .child(
                div()
                    .id("alternate-rows")
                    .ml(settings_theme::LABEL_INDENT)
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
            .child(self.render_wallpaper_settings(window, cx))
            .child(self.render_font_mode(cx))
            .child(self.font_field(FontTarget::ChannelLog, &self.i18n.text("channel_log"), cx))
            .child(self.line_height_slider(0, cx))
            .child(self.font_field(FontTarget::CombinedLog, &self.i18n.text("combined_log"), cx))
            .child(self.line_height_slider(1, cx))
            .child(self.font_field(FontTarget::MemberList, &self.i18n.text("member_list"), cx))
            .child(self.font_field(FontTarget::ChannelTree, &self.i18n.text("channel_tree"), cx))
            .child(self.font_field(FontTarget::Input, &self.i18n.text("draft_input"), cx))
            .child(self.font_field(FontTarget::Time, &self.i18n.text("timestamp_monospace"), cx))
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(self.reset_footer(SettingsTab::Appearance, cx))
    }

    /// The send and NOTICE keys, the channel-number and draft-editing keys
    /// (only Windows and Linux have choices there), then the shortcut list.
    fn render_keyboard_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let hint = |key: &str| {
            div()
                .ml(settings_theme::LABEL_INDENT)
                .text_color(theme.text_secondary)
                .child(self.i18n.text(key))
        };
        account_settings::panel(cx)
            .child(self.tab_heading("keyboard"))
            .child(self.option_row(
                "send_key",
                [
                    (SendKey::Enter, "send_key_enter"),
                    (SendKey::CtrlEnter, "send_key_ctrl_enter"),
                    (SendKey::DoubleEnter, "send_key_double_enter"),
                ],
                self.settings.values.send_key,
                |settings, key| settings.send_key = key,
                cx,
            ))
            .child(hint("send_key_hint"))
            .child(self.option_row(
                "notice_key",
                [
                    (NoticeKey::Off, "notice_key_off"),
                    (NoticeKey::CtrlEnter, "notice_key_ctrl_enter"),
                    (NoticeKey::AltEnter, "notice_key_alt_enter"),
                ],
                self.settings.values.notice_key,
                |settings, key| settings.notice_key = key,
                cx,
            ))
            .child(hint("notice_key_hint"))
            .when(
                self.settings
                    .values
                    .notice_key
                    .clashes_with(self.settings.values.send_key),
                |d| {
                    d.child(
                        div()
                            .ml(settings_theme::LABEL_INDENT)
                            .text_color(theme.warning)
                            .child(self.i18n.text("notice_key_clash")),
                    )
                },
            )
            .when(!cfg!(target_os = "macos"), |d| {
                d.child(self.option_row(
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
            })
            .when(cfg!(unix_desktop), |d| {
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
            .children(self.shortcut_rows(cx))
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
            .ml(settings_theme::LABEL_INDENT)
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
                .ml(settings_theme::LABEL_INDENT)
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
            .child(settings_field(
                &self.i18n.text("exclude_keywords"),
                self.settings.exclude_keywords.clone(),
            ))
            .child(hint("exclude_keywords_hint"))
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
            (SettingsTab::Keyboard, "keyboard-tab", "keyboard"),
        ];
        tabs.extend([
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
            self.avatar_setup_link = false;
            // A key being recorded does not wait on another tab.
            self.shortcut_recording = None;
            // A picker open under a color row does not wait for a return.
            self.color_picker = None;
            // The new tab starts at its top; the nav list keeps its place.
            let offset = self.pane_scroll.offset();
            self.pane_scroll.set_offset(point(offset.x, px(0.)));
        }
        // Server names on other tabs follow the connection tab's fields: a
        // registered server's text is committed (valid values only), a
        // draft's is only shown.
        if self.shown_server_registered() {
            self.autosave_now(None, cx);
        } else {
            self.settings.sync_shown_server(cx);
        }
        // The system may have changed the registration since it was read.
        self.refresh_autostart(cx);
        if tab == SettingsTab::Connection {
            self.avatar_return = None;
        }
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
            .ml(settings_theme::LABEL_INDENT)
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

    fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            SettingsTab::Appearance => self
                .render_appearance_settings(window, cx)
                .into_any_element(),
            SettingsTab::Keyboard => self.render_keyboard_settings(cx).into_any_element(),
            SettingsTab::Notifications => self.render_notification_settings(cx).into_any_element(),
            SettingsTab::Ircv3 => self.render_ircv3_settings(cx).into_any_element(),
            SettingsTab::ImageUpload => self.render_image_upload_settings(cx).into_any_element(),
            SettingsTab::Credentials => self.render_credential_settings(cx).into_any_element(),
            SettingsTab::Experimental => self.render_experimental_settings(cx).into_any_element(),
        };
        let auto_join = self.render_auto_join_dialog(cx);
        self.reveal_focused(window, cx);
        div()
            .id("settings-screen")
            .on_action(
                cx.listener(|this, _: &FocusNextField, window, cx| this.traverse(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &FocusPreviousField, window, cx| {
                this.traverse(false, window, cx)
            }))
            .key_context("SettingsWindow")
            .on_key_down(
                cx.listener(|this, event, window, cx| this.escape_key_down(event, window, cx)),
            )
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
                    .flex()
                    .flex_col()
                    // Outside the scrolling pane, so the reason a refused
                    // leave gives is readable wherever the pane is scrolled.
                    .when(self.store_busy(), |d| {
                        d.child(
                            div()
                                .id("credential-store-waiting")
                                .flex_shrink_0()
                                .w_full()
                                .px_4()
                                .py_2()
                                .border_b_1()
                                .border_color(theme.border)
                                .text_color(theme.warning)
                                .child(self.i18n.text("credential_store_waiting")),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .w_full()
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
                    ),
            )
            .children(auto_join)
            .on_action(cx.listener(Self::open_settings_action))
            .when(
                self.owner
                    .read(cx)
                    .is_ok_and(ChatWindow::can_disconnect_selected),
                |d| d.on_action(cx.listener(Self::disconnect_action)),
            )
            .when(
                self.owner
                    .read(cx)
                    .is_ok_and(ChatWindow::can_connect_selected),
                |d| d.on_action(cx.listener(Self::connect_action)),
            )
            .when(
                self.owner
                    .read(cx)
                    .is_ok_and(ChatWindow::can_reconnect_selected),
                |d| d.on_action(cx.listener(Self::reconnect_action)),
            )
            .on_action(cx.listener(Self::toggle_debug_action))
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.i18n.text("settings_title");
        let content = self.render_settings(window, cx).into_any_element();
        decorations::window_frame(window, cx, title, content)
    }
}

/// The auto-join line: up to five names, then an ellipsis. Names can come
/// from an imported settings file, so each is neutralized on its own to keep
/// the boundaries between names readable.
fn join_summary(names: &[String], none: String) -> String {
    const SHOWN: usize = 5;
    let shown = names
        .iter()
        .take(SHOWN)
        .map(|name| display::neutralize_bidi(name))
        .collect::<Vec<_>>()
        .join(", ");
    match names.len() {
        0 => none,
        n if n > SHOWN => format!("{shown}, …"),
        _ => shown,
    }
}

#[cfg(test)]
mod join_summary_tests {
    use super::join_summary;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn bidi_controls_in_names_are_neutralized() {
        let summary = join_summary(&names(&["#a\u{202E}b", "#日本語"]), String::new());
        assert_eq!(summary, "#a\u{200B}b, #日本語");
    }

    #[test]
    fn plain_summaries_are_unchanged() {
        assert_eq!(join_summary(&[], "(none)".into()), "(none)");
        assert_eq!(join_summary(&names(&["#a", "#b"]), String::new()), "#a, #b");
        let many = names(&["#1", "#2", "#3", "#4", "#5", "#6"]);
        assert_eq!(join_summary(&many, String::new()), "#1, #2, #3, #4, #5, …");
    }
}

#[cfg(test)]
mod line_height_slider_tests {
    use super::{line_height_at, line_height_fraction};

    #[test]
    fn the_ends_are_the_minimum_and_the_maximum() {
        assert_eq!(line_height_at(0.), 100);
        assert_eq!(line_height_at(1.), 200);
        assert_eq!(line_height_at(-3.), 100);
        assert_eq!(line_height_at(9.), 200);
        assert_eq!(line_height_fraction(100), 0.);
        assert_eq!(line_height_fraction(200), 1.);
    }

    #[test]
    fn every_percent_round_trips_including_the_default() {
        for percent in 100..=200 {
            assert_eq!(line_height_at(line_height_fraction(percent)), percent);
        }
        assert_eq!(
            line_height_at(line_height_fraction(
                cayenchat_storage::DEFAULT_LINE_HEIGHT_PERCENT
            )),
            cayenchat_storage::DEFAULT_LINE_HEIGHT_PERCENT
        );
    }
}

#[cfg(test)]
mod follow_file_flag_tests {
    use super::follow_file_flag;

    #[test]
    fn unedited_form_follows_the_file() {
        let (mut form, mut saved) = (false, false);
        follow_file_flag(&mut form, &mut saved, true);
        assert!(form && saved);
    }

    #[test]
    fn edit_in_the_form_survives_an_older_file_value() {
        // The user ticked the box (form true, saved false); the file still
        // holds false.
        let (mut form, mut saved) = (true, false);
        follow_file_flag(&mut form, &mut saved, false);
        assert!(form);
        assert!(!saved);
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

    fn form(settings: Settings, cx: &mut TestAppContext) -> SettingsForm {
        let i18n = Localizer::new(Language::English);
        cx.update(|cx| SettingsForm::new(settings, &i18n, cx))
    }

    fn click(field: &Entity<TextInput>, click_count: usize, cx: &mut gpui::VisualTestContext) {
        let event = gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(gpui::px(0.), gpui::px(0.)),
            modifiers: gpui::Modifiers::default(),
            click_count,
            first_mouse: false,
        };
        field.update_in(cx, |field, window, cx| {
            field.on_mouse_down(&event, window, cx)
        });
    }

    #[gpui::test]
    fn double_click_selects_a_whole_color_code_only(cx: &mut TestAppContext) {
        let form = form(configured_a(), cx);
        let (color, other) = (form.dark_link_color.clone(), form.nickname.clone());
        let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
        for field in [&color, &other] {
            field.update(cx, |field, cx| field.set_text("#AABBCC", cx));
        }
        click(&color, 1, cx);
        assert!(color.read_with(cx, |f, _| f.selection().is_empty()));
        click(&color, 2, cx);
        assert_eq!(color.read_with(cx, |f, _| f.selection()), 0..7);
        // Other fields select the unit under the pointer, not everything.
        click(&other, 2, cx);
        let selected = other.read_with(cx, |f, _| f.selection());
        assert!(!selected.is_empty() && selected != (0..7), "{selected:?}");
    }

    #[gpui::test]
    fn every_color_field_selects_on_double_click(cx: &mut TestAppContext) {
        let form = form(configured_a(), cx);
        let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
        for field in form.color_inputs() {
            field.update(cx, |field, cx| field.set_text("#12", cx));
            click(field, 2, cx);
            assert_eq!(field.read_with(cx, |f, _| f.selection()), 0..3);
        }
    }

    #[gpui::test]
    fn only_ascii_oriented_fields_prefer_direct_input(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler as _;
        let form = form(configured_a(), cx);
        let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
        let direct = |field: &Entity<TextInput>, cx: &mut gpui::VisualTestContext| {
            field.update_in(cx, |field, window, cx| {
                field.prefers_direct_input(window, cx)
            })
        };
        let wanted = [
            &form.nickname,
            &form.username,
            &form.custom_host,
            &form.port,
            &form.channels,
            &form.sasl_username,
        ];
        for field in wanted.into_iter().chain(form.color_inputs()) {
            assert!(direct(field, cx));
        }
        let others = [
            &form.realname,
            &form.display_name,
            &form.quit_message,
            &form.sleep_quit_message,
            &form.part_message,
            &form.server_password,
            &form.sasl_password,
        ];
        for field in others {
            assert!(!direct(field, cx));
        }
    }

    fn switch(
        form: &mut SettingsForm,
        cx: &mut TestAppContext,
        change: impl FnOnce(&mut Settings),
    ) {
        let i18n = Localizer::new(Language::English);
        cx.update(|cx| form.switch_server(change, &i18n, cx))
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
        let mut form = form(settings, cx);
        // An edit to A that autosave has not written yet.
        type_into(&form.channels, "#a1,#a2,#a3", cx);

        switch(&mut form, cx, |settings| {
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
        let mut form = self::form(configured_a(), cx);
        switch(&mut form, cx, |settings| {
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
    fn typed_host_and_display_name_reach_the_values_before_saving(cx: &mut TestAppContext) {
        let mut form = form(Settings::default(), cx);
        switch(&mut form, cx, |settings| {
            settings.add_server(cayenchat_storage::PRESETS[0].host);
        });
        type_into(&form.custom_host, "tiarra.example", cx);
        type_into(&form.display_name, "Tiarra", cx);
        type_into(&form.port, "bad", cx);
        cx.read(|cx| form.sync_shown_server(cx));
        let profile = form.values.selected_profile().unwrap();
        assert_eq!(profile.host, "tiarra.example");
        assert_eq!(profile.display_name, "Tiarra");
        // An unfinished port keeps the earlier one.
        assert_eq!(profile.port, 6667);
    }

    #[gpui::test]
    fn an_unfinished_connection_entry_does_not_block_other_settings(cx: &mut TestAppContext) {
        let form = form(configured_a(), cx);
        type_into(&form.port, "bad", cx);
        assert!(cx.read(|cx| form.snapshot(cx)).is_err());
        let settings = cx.read(|cx| form.snapshot_with(cx, false)).unwrap();
        assert_eq!(settings.servers, form.values.servers);
    }

    #[gpui::test]
    fn typed_passwords_are_not_stored_and_are_kept_per_server(cx: &mut TestAppContext) {
        let store = memory_store();
        let mut settings = configured_a();
        let a_id = settings.selected_server.clone();
        let b_id = settings.add_server("b.example").id.clone();
        settings.selected_server = a_id.clone();
        let mut form = form(settings, cx);
        type_into(&form.server_password, "a-typed", cx);
        switch(&mut form, cx, |settings| {
            settings.selected_server = b_id.clone()
        });
        // Not shown for the other server, and nothing was written.
        assert_eq!(text(&form.server_password, cx), "");
        let key = form.values.profile(&a_id).unwrap().server_password_key();
        assert!(store.get(&key).unwrap().is_none());
        switch(&mut form, cx, |settings| {
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
        let b_id = b.id.clone();
        settings.selected_server = a_id.clone();
        let (a_key, b_key) = (
            settings.profile(&a_id).unwrap().server_password_key(),
            settings.profile(&b_id).unwrap().server_password_key(),
        );
        let mut form = form(settings, cx);

        // Edits and a typed password for A, then B is picked before the
        // delayed autosave runs.
        type_into(&form.channels, "#a-edited", cx);
        type_into(&form.server_password, "a-typed", cx);
        switch(&mut form, cx, |settings| {
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

        // Edit B, switch back: A shows its own values and the password
        // typed for it, which was never stored.
        type_into(&form.channels, "#b-edited", cx);
        switch(&mut form, cx, |settings| {
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
                "a-typed"
            ]
            .map(str::to_owned)
        );
        assert!(!form.saved_server_password);
        assert!(store.get(&a_key).unwrap().is_none());
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
        let mut form = form(settings, cx);
        switch(&mut form, cx, |settings| {
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
                .0
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

#[cfg(test)]
mod escape_tests {
    use super::{KeyError, KeyFileError, SecretKey, SettingsWindow, read_key_file};
    use crate::channel_settings_dialog::channel_encodings_allowed;
    use cayenchat_model::names::CaseMapping;
    use cayenchat_storage::TextEncoding;
    use gpui::TestAppContext;

    #[gpui::test]
    fn escape_closes_layers_first_then_the_window(cx: &mut TestAppContext) {
        crate::support::install_test_globals(cx);
        let settings = crate::settings_with_channels("#a");
        let owner = cx.add_window(|window, cx| {
            crate::ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let mut app = cx.clone();
        let (form, cx) = cx
            .add_window_view(|window, cx| SettingsWindow::new(owner, settings.clone(), window, cx));
        cx.run_until_parked();

        // An open server list takes the first Escape; the window stays.
        form.update(cx, |form, cx| {
            form.settings.server_list_open = true;
            cx.notify();
        });
        cx.simulate_keystrokes("escape");
        assert!(form.read_with(cx, |form, _| !form.settings.server_list_open));
        let windows = |app: &mut TestAppContext| app.update(|app| app.windows().len());
        assert_eq!(windows(&mut app), 2);

        // A shortcut being recorded keeps Escape for itself.
        form.update(cx, |form, cx| {
            form.toggle_shortcut_recording("next_channel", cx)
        });
        cx.simulate_keystrokes("escape");
        assert!(form.read_with(cx, |form, _| form.shortcut_recording.is_none()));
        assert_eq!(windows(&mut app), 2);

        // Nothing open: Escape closes the settings window only.
        cx.simulate_keystrokes("escape");
        assert_eq!(windows(&mut app), 1);
        assert!(
            owner.read_with(&app, |_, _| true).is_ok(),
            "the chat remains"
        );
    }

    pub(super) fn open(
        cx: &mut TestAppContext,
        settings: cayenchat_storage::Settings,
    ) -> (
        gpui::Entity<SettingsWindow>,
        &mut gpui::VisualTestContext,
        TestAppContext,
    ) {
        crate::support::install_test_globals(cx);
        let owner = cx.add_window({
            let settings = settings.clone();
            move |window, cx| crate::ChatWindow::with_settings(settings, None, window, cx)
        });
        let app = cx.clone();
        let (form, vcx) = cx
            .add_window_view(|window, cx| SettingsWindow::new(owner, settings.clone(), window, cx));
        vcx.run_until_parked();
        (form, vcx, app)
    }

    #[gpui::test]
    fn escape_closes_language_and_encoding_lists_first(cx: &mut TestAppContext) {
        let (form, cx, mut app) = open(cx, crate::settings_with_channels("#a"));
        let windows = |app: &mut TestAppContext| app.update(|app| app.windows().len());
        form.update(cx, |form, cx| {
            form.settings.language_list_open = true;
            cx.notify();
        });
        cx.simulate_keystrokes("escape");
        assert!(form.read_with(cx, |form, _| !form.settings.language_list_open));
        assert_eq!(windows(&mut app), 2);
        form.update(cx, |form, cx| {
            form.settings.encoding_list_open = true;
            cx.notify();
        });
        cx.simulate_keystrokes("escape");
        assert!(form.read_with(cx, |form, _| !form.settings.encoding_list_open));
        assert_eq!(windows(&mut app), 2);
    }

    #[gpui::test]
    fn sasl_checkbox_is_independent_of_the_mechanism_and_keeps_tls_on(cx: &mut TestAppContext) {
        let (form, cx, _app) = open(cx, crate::settings_with_channels("#a"));
        let state = |cx: &mut gpui::VisualTestContext| {
            form.read_with(cx, |form, _| {
                let profile = form.settings.values.selected_profile().unwrap();
                (
                    profile.sasl_enabled,
                    profile.sasl_mechanism,
                    profile.use_tls,
                )
            })
        };
        form.update(cx, |form, cx| {
            form.settings.values.selected_profile_mut().unwrap().use_tls = false;
            form.settings.sasl_list_open = true;
            form.choose_sasl_mechanism(cayenchat_storage::SaslMechanism::ScramSha512, cx);
        });
        // Choosing a mechanism neither turns SASL on nor touches TLS.
        assert_eq!(
            state(cx),
            (false, cayenchat_storage::SaslMechanism::ScramSha512, false)
        );
        assert!(form.read_with(cx, |form, _| !form.settings.sasl_list_open));
        form.update(cx, |form, cx| form.toggle_sasl(cx));
        assert_eq!(
            state(cx),
            (true, cayenchat_storage::SaslMechanism::ScramSha512, true)
        );
        form.update(cx, |form, cx| form.toggle_sasl(cx));
        // The mechanism is remembered for the next time SASL is turned on.
        assert_eq!(
            state(cx),
            (false, cayenchat_storage::SaslMechanism::ScramSha512, true)
        );
    }

    #[gpui::test]
    fn the_sasl_key_is_made_shown_copied_and_deleted(cx: &mut TestAppContext) {
        let (form, cx, _app) = open(cx, crate::settings_with_channels("#a"));
        let id = form.read_with(cx, |form, _| form.settings.values.selected_server.clone());
        let store = cx.update(|_, cx| crate::secrets::store(cx));
        let note = |cx: &mut gpui::VisualTestContext| {
            form.read_with(cx, |form, _| {
                let in_form = form
                    .settings
                    .values
                    .selected_profile()
                    .unwrap()
                    .sasl_key_saved;
                let in_saved = form.saved.profile(&form.settings.values.selected_server);
                (in_form, in_saved.unwrap().sasl_key_saved)
            })
        };
        assert_eq!(note(cx), (false, false));

        cx.update(|window, cx| form.update(cx, |form, cx| form.generate_key(window, cx)));
        cx.run_until_parked();
        let stored = store.get(&SecretKey::sasl_key(&id)).unwrap().unwrap();
        assert!(cayenchat_irc_core::sasl_key_is_valid(stored.expose()));
        assert_eq!(note(cx), (true, true));
        let public = form
            .read_with(cx, |form, _| form.settings.sasl_public_key.clone())
            .expect("the public key is shown");
        // 33 bytes (compressed) in base64 are 44 characters.
        assert_eq!(public.len(), 44);

        // Copy puts the public key on the clipboard, never the private one.
        form.update(cx, |form, cx| form.copy_public_key(cx));
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some(public.as_str()));
        assert_ne!(copied.as_deref(), Some(stored.expose()));

        // Showing again reads the store, as the user asked.
        form.update(cx, |form, _| form.settings.sasl_public_key = None);
        form.update(cx, |form, cx| form.show_public_key(cx));
        cx.run_until_parked();
        assert_eq!(
            form.read_with(cx, |form, _| form.settings.sasl_public_key.clone()),
            Some(public)
        );

        // Delete clears the store, the notes and the shown key.
        form.update(cx, |form, cx| {
            let result = crate::secrets::store(cx).delete(&SecretKey::sasl_key(&id));
            form.key_done(&id, result, false, cx);
            form.settings.sasl_public_key = None;
        });
        assert!(store.get(&SecretKey::sasl_key(&id)).unwrap().is_none());
        assert_eq!(note(cx), (false, false));
        form.update(cx, |form, cx| form.show_public_key(cx));
        cx.run_until_parked();
        assert!(form.read_with(cx, |form, _| form.settings.sasl_public_key.is_none()));
    }

    #[gpui::test]
    fn an_imported_key_goes_to_the_server_the_import_started_for(cx: &mut TestAppContext) {
        let mut settings = crate::settings_with_channels("#a");
        let a = settings.selected_server.clone();
        let b = settings.add_server("b.example").id.clone();
        settings.selected_server = a.clone();
        let (form, cx, _app) = open(cx, settings);
        let store = cx.update(|_, cx| crate::secrets::store(cx));
        let existing = cayenchat_irc_core::generate_sasl_key().unwrap();
        store
            .set(
                &SecretKey::sasl_key(&b),
                &cayenchat_storage::Secret::new(existing.as_str()),
            )
            .unwrap();

        // The import is started for A; the selection moves to B before the
        // file has been read.
        form.update(cx, |form, cx| form.select_server(b.clone(), cx));
        cx.run_until_parked();
        let imported = cayenchat_irc_core::generate_sasl_key().unwrap();
        cx.update(|window, cx| {
            form.update(cx, |form, cx| {
                form.imported_key(&a, Ok(imported.clone()), window, cx)
            })
        });
        cx.run_until_parked();

        let stored = |id: &str| store.get(&SecretKey::sasl_key(id)).unwrap();
        assert_eq!(stored(&a).unwrap().expose(), imported.as_str());
        assert_eq!(stored(&b).unwrap().expose(), existing.as_str());
        // A server that is gone stores nothing.
        let gone = cayenchat_irc_core::generate_sasl_key().unwrap();
        cx.update(|window, cx| {
            form.update(cx, |form, cx| {
                form.imported_key("removed", Ok(gone.clone()), window, cx)
            })
        });
        assert!(stored("removed").is_none());
    }

    #[test]
    fn key_files_are_read_with_the_reason_they_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let path = directory.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let ok = write(
            "ok.pem",
            "-----BEGIN PRIVATE KEY-----\n\
             MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg7bua35rLTqSZSqVG\n\
             eqVCn9nr9neWhNoQSdfIliAszo6hRANCAAQt7dJhtTnt+F+o8tGZEmiXpAv19D+/\n\
             wxsyotYgHOeMqHvQsPqboMB/wIAv6O15TDlnG6Cuyk9MQ/G3FRgwtTg8\n\
             -----END PRIVATE KEY-----\n",
        );
        assert!(read_key_file(&ok).is_ok());
        let encrypted = write(
            "enc.pem",
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n",
        );
        assert!(matches!(
            read_key_file(&encrypted),
            Err(KeyFileError::Key(KeyError::Encrypted))
        ));
        let huge = write("huge.pem", &"A".repeat(100_000));
        assert!(matches!(
            read_key_file(&huge),
            Err(KeyFileError::Key(KeyError::NotPem))
        ));
        assert!(matches!(
            read_key_file(&directory.path().join("missing.pem")),
            Err(KeyFileError::Io(_))
        ));
        let binary = directory.path().join("binary.pem");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(
            read_key_file(&binary),
            Err(KeyFileError::Key(KeyError::NotPem))
        ));
    }

    #[gpui::test]
    fn escape_during_composition_keeps_the_window(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler;
        let (form, cx, mut app) = open(cx, crate::settings_with_channels("#a"));
        let nickname = form.read_with(cx, |form, _| form.settings.nickname.clone());
        cx.update(|window, cx| {
            nickname.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "あ", None, window, cx)
            })
        });
        cx.simulate_keystrokes("escape");
        let windows = |app: &mut TestAppContext| app.update(|app| app.windows().len());
        assert_eq!(windows(&mut app), 2);
        cx.update(|window, cx| nickname.update(cx, |input, cx| input.unmark_text(window, cx)));
        cx.simulate_keystrokes("escape");
        assert_eq!(windows(&mut app), 1);
    }

    #[gpui::test]
    fn escape_closes_settings_without_servers(cx: &mut TestAppContext) {
        let mut settings = crate::settings_with_channels("#a");
        settings.servers.clear();
        let (_form, cx, app) = open(cx, settings);
        cx.simulate_keystrokes("escape");
        assert_eq!(app.clone().update(|app| app.windows().len()), 1);
    }

    /// Saves `channel` of the first server the way the channel settings
    /// window does, after `change` edited what the file holds for it.
    fn save_channel(
        owner: gpui::WindowHandle<crate::ChatWindow>,
        channel: &str,
        cx: &mut gpui::VisualTestContext,
        change: impl FnOnce(&mut crate::channel_settings_dialog::Choices),
    ) {
        let mapping = CaseMapping::default();
        owner
            .update(cx, |chat, window, cx| {
                let profile = chat.saved.selected_profile().unwrap();
                let mut choices =
                    crate::channel_settings_dialog::Choices::of(profile, channel, mapping);
                let base = (choices, cayenchat_storage::ChannelLook::default());
                change(&mut choices);
                let id = profile.id.clone();
                let look = cayenchat_storage::ChannelLook::default();
                chat.apply_channel_settings(
                    &id, channel, mapping, &base, choices, look, window, cx,
                )
                .unwrap();
            })
            .unwrap();
    }

    /// Registers the chat window's connection to `NetworkId(1)` with
    /// `channels` joined and the first of them selected, so that `/charset`
    /// can run.
    fn register(
        owner: gpui::WindowHandle<crate::ChatWindow>,
        channels: &[&str],
        cx: &mut gpui::VisualTestContext,
    ) {
        owner
            .update(cx, |chat, _, cx| {
                let mut events = vec![cayenchat_irc_core::Event::Registered {
                    nickname: "me".into(),
                }];
                events.extend(
                    channels
                        .iter()
                        .map(|channel| cayenchat_irc_core::Event::Joined {
                            channel: (*channel).to_owned(),
                        }),
                );
                chat.handle_events(
                    cayenchat_model::NetworkId(1),
                    events,
                    crate::Worker::Running,
                    cx,
                );
                let first = chat.state.conversations()[0].id;
                chat.state
                    .dispatch(cayenchat_app::Command::SelectChannel(first));
            })
            .unwrap();
    }

    fn file_profile() -> cayenchat_storage::ServerProfile {
        crate::settings_file::load()
            .unwrap()
            .unwrap()
            .selected_profile()
            .unwrap()
            .clone()
    }

    #[gpui::test]
    fn saving_channel_settings_sends_no_join_or_part(cx: &mut TestAppContext) {
        use std::io::{BufRead, BufReader};
        use std::time::{Duration, Instant};

        let settings = crate::settings_with_channels("#a,-#b");
        let file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = open(cx, settings);
        let owner = form.read_with(cx, |form, _| form.owner);
        register(owner, &["#a", "#b"], cx);

        // A server that records every line it receives until the end of the
        // connection.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (lines_tx, lines_rx) = std::sync::mpsc::channel();
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut reader = BufReader::new(stream);
            let mut lines = Vec::new();
            let mut line = String::new();
            while Instant::now() < deadline {
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        lines.push(std::mem::take(&mut line));
                        let _ = first_tx.send(());
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => break,
                }
            }
            let _ = lines_tx.send(lines);
        });
        let mut config =
            cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
        config.port = port;
        config.use_tls = false;
        let connection = cayenchat_irc_core::Connection::connect(config.clone()).unwrap();
        owner
            .update(cx, |chat, _, _| {
                let session = chat
                    .sessions
                    .get_mut(&cayenchat_model::NetworkId(1))
                    .unwrap();
                session.active_config = Some(config);
                session.irc = Some(connection);
            })
            .unwrap();

        // Channel settings turn auto-join on for #b and off for #a.
        save_channel(owner, "#b", cx, |choices| {
            choices.enabled = true;
            choices.decode = Some(TextEncoding::Gbk);
        });
        save_channel(owner, "#a", cx, |choices| choices.enabled = false);
        assert_eq!(file_profile().channels, "-#a,#b");

        // Closing the connection ends the recording. Wait for the first line
        // so a slow connection thread is not closed before it wrote anything.
        first_rx
            .recv_timeout(Duration::from_secs(8))
            .expect("the connection sent its first line");
        owner
            .update(cx, |chat, _, _| {
                chat.sessions
                    .get_mut(&cayenchat_model::NetworkId(1))
                    .unwrap()
                    .irc = None;
            })
            .unwrap();
        let lines = lines_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert!(!lines.is_empty(), "the connection was recorded");
        for line in &lines {
            let command = line.split_whitespace().next().unwrap_or("");
            assert!(
                !command.eq_ignore_ascii_case("JOIN") && !command.eq_ignore_ascii_case("PART"),
                "unexpected {line}"
            );
        }
        drop(file);
    }

    #[gpui::test]
    fn an_open_settings_window_does_not_write_back_the_channel_menus_save(cx: &mut TestAppContext) {
        let settings = crate::settings_with_channels("#a,-#b");
        let file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = open(cx, settings);
        let owner = form.read_with(cx, |form, _| form.owner);
        save_channel(owner, "#b", cx, |choices| {
            choices.enabled = true;
            choices.decode = Some(TextEncoding::Gbk);
        });
        // An unrelated edit of the window saves the form.
        let nickname = form.read_with(cx, |form, _| form.settings.nickname.clone());
        nickname.update(cx, |field, cx| field.set_text("someone-else", cx));
        form.update(cx, |form, cx| form.autosave_now(None, cx));
        let profile = file_profile();
        assert_eq!(profile.channels, "#a,#b");
        assert_eq!(profile.channel_charsets.len(), 1);
        assert_eq!(profile.nickname, "someone-else");
        form.read_with(cx, |form, cx| {
            assert_eq!(form.settings.channels.read(cx).text(), "#a,#b");
        });
        drop(file);
    }

    #[test]
    fn utf8only_channel_settings_allow_only_utf8_overrides() {
        assert!(channel_encodings_allowed(true, None, None));
        assert!(channel_encodings_allowed(
            true,
            Some(TextEncoding::Utf8),
            Some(TextEncoding::Utf8)
        ));
        assert!(!channel_encodings_allowed(
            true,
            Some(TextEncoding::ShiftJis),
            None
        ));
        assert!(channel_encodings_allowed(
            false,
            Some(TextEncoding::ShiftJis),
            None
        ));
    }

    #[gpui::test]
    fn a_settings_save_writes_the_notes_the_chat_window_corrected(cx: &mut TestAppContext) {
        let mut settings = crate::settings_with_channels("#a");
        settings.selected_profile_mut().unwrap().keyed_channels = Some(Vec::new());
        let id = settings.selected_profile().unwrap().id.clone();
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = open(cx, settings.clone());
        let owner = form.read_with(cx, |form, _| form.owner);
        let _ = owner.update(cx, |owner, _, _| {
            owner.update_profile_note(&id, |p| p.note_channel_key("#a", true));
        });
        let noted =
            |file: &cayenchat_storage::Settings| file.profile(&id).unwrap().keyed_channels.clone();
        assert_eq!(
            noted(&crate::settings_file::load().unwrap().unwrap()),
            Some(Vec::new())
        );
        let mut next = settings.clone();
        next.selected_profile_mut().unwrap().quit_message = "bye".into();
        let previous = settings.clone();
        let saved = form
            .update(cx, |form, cx| form.commit_settings(&previous, next, cx))
            .unwrap();
        assert_eq!(noted(&saved), Some(vec!["#a".to_owned()]));
        assert_eq!(
            noted(&crate::settings_file::load().unwrap().unwrap()),
            Some(vec!["#a".to_owned()])
        );
        let pending = owner
            .read_with(cx, |owner, _| owner.pending_notes.is_empty())
            .unwrap();
        assert!(pending, "nothing is left to write");
    }

    #[gpui::test]
    fn failed_save_keeps_the_secrets_of_removed_profiles(cx: &mut TestAppContext) {
        use cayenchat_storage::Secret;
        let (form, cx, _app) = open(cx, crate::settings_with_channels("#a"));
        let mut previous = cayenchat_storage::Settings::default();
        previous.add_server("");
        previous.selected_profile_mut().unwrap().host = "irc.example.org".into();
        previous
            .selected_profile_mut()
            .unwrap()
            .set_server_password_saved(true);
        let removed = previous.selected_profile().unwrap().clone();
        let mut next = previous.clone();
        next.remove_selected_server();
        let store = form.update(cx, |_, cx| crate::secrets::store(cx));
        store
            .set(&removed.server_password_key(), &Secret::new("x"))
            .unwrap();

        let _file = crate::settings_file::TestFile::with(&previous);
        crate::settings_file::fail_saves(true);
        let result = form.update(cx, |form, cx| {
            form.commit_settings(&previous, next.clone(), cx)
        });
        assert!(result.is_err());
        assert!(store.contains(&removed.server_password_key()).unwrap());

        // A later successful save forgets it.
        crate::settings_file::fail_saves(false);
        let result = form.update(cx, |form, cx| form.commit_settings(&previous, next, cx));
        assert!(result.is_ok());
        assert!(!store.contains(&removed.server_password_key()).unwrap());
    }
}

#[cfg(test)]
mod host_change_tests {
    use super::{Settings, SettingsWindow};
    use cayenchat_storage::{Secret, SecretKey};
    use gpui::TestAppContext;

    /// A saved server with every kind of secret stored, and its settings.
    fn saved_secrets(cx: &mut TestAppContext) -> (Settings, String) {
        let mut settings = crate::settings_with_channels("#a");
        let profile = settings.selected_profile_mut().unwrap();
        profile.host = "irc.old.example".into();
        profile.set_server_password_saved(true);
        profile.set_sasl_password_saved(true);
        profile.sasl_key_saved = true;
        profile.note_channel_key("#a", true);
        let id = profile.id.clone();
        let _ = cx;
        (settings, id)
    }

    fn fill_store(
        cx: &mut gpui::VisualTestContext,
        id: &str,
    ) -> cayenchat_storage::CredentialStore {
        let store = cx.update(|_, cx| crate::secrets::store(cx));
        for key in [
            SecretKey::server_password(id),
            SecretKey::sasl_password(id),
            SecretKey::sasl_key(id),
            SecretKey::channel_key(id, "#a"),
        ] {
            store.set(&key, &Secret::new("secret")).unwrap();
        }
        store
    }

    fn edit_host(
        form: &gpui::Entity<SettingsWindow>,
        cx: &mut gpui::VisualTestContext,
        host: &str,
    ) {
        form.update(cx, |form, cx| {
            form.settings
                .custom_host
                .update(cx, |field, cx| field.set_text(host, cx));
            form.autosave_now(None, cx);
        });
        cx.run_until_parked();
    }

    fn saved_host(form: &gpui::Entity<SettingsWindow>, cx: &mut gpui::VisualTestContext) -> String {
        form.read_with(cx, |form, _| {
            form.saved.selected_profile().unwrap().host.clone()
        })
    }

    #[gpui::test]
    fn changing_the_host_asks_then_deletes_what_is_sent_as_is(cx: &mut TestAppContext) {
        let (settings, id) = saved_secrets(cx);
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = super::escape_tests::open(cx, settings);
        let store = fill_store(cx, &id);

        // Cancelling changes neither the host nor the passwords.
        edit_host(&form, cx, "irc.new.example");
        assert!(cx.has_pending_prompt());
        let cancel = form.read_with(cx, |form, _| form.i18n.text("cancel"));
        cx.simulate_prompt_answer(&cancel);
        cx.run_until_parked();
        assert_eq!(saved_host(&form, cx), "irc.old.example");
        assert_eq!(
            form.read_with(cx, |form, cx| form
                .settings
                .custom_host
                .read(cx)
                .text()
                .to_owned()),
            "irc.old.example"
        );
        assert!(store.contains(&SecretKey::server_password(&id)).unwrap());
        assert!(store.contains(&SecretKey::channel_key(&id, "#a")).unwrap());

        // Confirming deletes them, keeps the SASL key and notes it all.
        edit_host(&form, cx, "irc.new.example");
        let confirm = form.read_with(cx, |form, _| form.i18n.text("host_change_confirm"));
        cx.simulate_prompt_answer(&confirm);
        cx.run_until_parked();
        assert_eq!(saved_host(&form, cx), "irc.new.example");
        for key in [
            SecretKey::server_password(&id),
            SecretKey::sasl_password(&id),
            SecretKey::channel_key(&id, "#a"),
        ] {
            assert!(!store.contains(&key).unwrap());
        }
        assert!(store.contains(&SecretKey::sasl_key(&id)).unwrap());
        let file = crate::settings_file::load().unwrap().unwrap();
        let profile = file.profile(&id).unwrap();
        assert_eq!(profile.host, "irc.new.example");
        assert!(!profile.has_host_bound_secrets());
        assert!(profile.sasl_key_saved);
        form.read_with(cx, |form, _| {
            assert!(!form.saved.profile(&id).unwrap().has_host_bound_secrets());
            assert!(
                !form
                    .settings
                    .values
                    .profile(&id)
                    .unwrap()
                    .has_host_bound_secrets()
            );
        });
    }

    #[gpui::test]
    fn a_password_that_cannot_be_deleted_keeps_the_old_host(cx: &mut TestAppContext) {
        use cayenchat_storage::{
            CredentialBackendKind, CredentialStore, credentials::MemoryBackend,
        };
        let (settings, id) = saved_secrets(cx);
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = super::escape_tests::open(cx, settings);
        cx.update(|_, cx| {
            cx.set_global(crate::secrets::Credentials(CredentialStore::with_backend(
                std::sync::Arc::new(MemoryBackend::unavailable(CredentialBackendKind::System)),
            )));
        });

        edit_host(&form, cx, "irc.new.example");
        let confirm = form.read_with(cx, |form, _| form.i18n.text("host_change_confirm"));
        cx.simulate_prompt_answer(&confirm);
        cx.run_until_parked();
        assert_eq!(saved_host(&form, cx), "irc.old.example");
        form.read_with(cx, |form, cx| {
            assert_eq!(form.settings.custom_host.read(cx).text(), "irc.old.example");
            assert!(form.saved.profile(&id).unwrap().has_host_bound_secrets());
            assert!(form.feedback.is_some());
        });
    }

    #[gpui::test]
    fn spaces_case_and_a_port_change_keep_the_passwords(cx: &mut TestAppContext) {
        let (settings, id) = saved_secrets(cx);
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = super::escape_tests::open(cx, settings);
        let store = fill_store(cx, &id);

        edit_host(&form, cx, "  IRC.Old.Example ");
        assert!(!cx.has_pending_prompt());
        form.update(cx, |form, cx| {
            form.settings
                .port
                .update(cx, |field, cx| field.set_text("6670", cx));
            form.autosave_now(None, cx);
        });
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
        assert!(store.contains(&SecretKey::server_password(&id)).unwrap());
        assert!(store.contains(&SecretKey::sasl_password(&id)).unwrap());
        assert!(store.contains(&SecretKey::channel_key(&id, "#a")).unwrap());
        assert_eq!(
            form.read_with(cx, |form, _| form.saved.selected_profile().unwrap().port),
            6670
        );
    }

    fn type_host(
        form: &gpui::Entity<SettingsWindow>,
        cx: &mut gpui::VisualTestContext,
        host: &str,
    ) {
        form.update(cx, |form, cx| {
            form.settings
                .custom_host
                .update(cx, |field, cx| field.set_text(host, cx));
        });
    }

    #[gpui::test]
    fn connecting_with_a_changed_host_asks_first_and_cancelling_does_not_connect(
        cx: &mut TestAppContext,
    ) {
        let (mut settings, id) = saved_secrets(cx);
        let profile = settings.selected_profile_mut().unwrap();
        profile.nickname = "nick".into();
        profile.username = "user".into();
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = super::escape_tests::open(cx, settings);
        let store = fill_store(cx, &id);

        // No autosave in between: Connect itself must ask.
        type_host(&form, cx, "127.0.0.1");
        form.update_in(cx, |form, window, cx| {
            form.connect_from_settings(window, cx)
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        let cancel = form.read_with(cx, |form, _| form.i18n.text("cancel"));
        cx.simulate_prompt_answer(&cancel);
        cx.run_until_parked();
        assert_eq!(saved_host(&form, cx), "irc.old.example");
        assert_eq!(
            crate::settings_file::load()
                .unwrap()
                .unwrap()
                .profile(&id)
                .unwrap()
                .host,
            "irc.old.example"
        );
        assert!(store.contains(&SecretKey::server_password(&id)).unwrap());
        assert!(store.contains(&SecretKey::channel_key(&id, "#a")).unwrap());

        // Confirming deletes them before the connection is made.
        type_host(&form, cx, "127.0.0.1");
        form.update_in(cx, |form, window, cx| {
            form.connect_from_settings(window, cx)
        });
        cx.run_until_parked();
        let confirm = form.read_with(cx, |form, _| form.i18n.text("host_change_confirm"));
        cx.simulate_prompt_answer(&confirm);
        cx.run_until_parked();
        for key in [
            SecretKey::server_password(&id),
            SecretKey::sasl_password(&id),
            SecretKey::channel_key(&id, "#a"),
        ] {
            assert!(!store.contains(&key).unwrap());
        }
        assert!(store.contains(&SecretKey::sasl_key(&id)).unwrap());
    }

    #[gpui::test]
    fn an_unscanned_profile_asks_when_a_channel_key_is_stored(cx: &mut TestAppContext) {
        let (mut settings, id) = saved_secrets(cx);
        let profile = settings.selected_profile_mut().unwrap();
        profile.set_server_password_saved(false);
        profile.set_sasl_password_saved(false);
        profile.keyed_channels = None;
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = super::escape_tests::open(cx, settings);
        let store = fill_store(cx, &id);

        edit_host(&form, cx, "irc.new.example");
        assert!(cx.has_pending_prompt());
        let cancel = form.read_with(cx, |form, _| form.i18n.text("cancel"));
        cx.simulate_prompt_answer(&cancel);
        cx.run_until_parked();
        assert_eq!(saved_host(&form, cx), "irc.old.example");
        assert!(store.contains(&SecretKey::channel_key(&id, "#a")).unwrap());

        edit_host(&form, cx, "irc.new.example");
        let confirm = form.read_with(cx, |form, _| form.i18n.text("host_change_confirm"));
        cx.simulate_prompt_answer(&confirm);
        cx.run_until_parked();
        assert_eq!(saved_host(&form, cx), "irc.new.example");
        assert!(!store.contains(&SecretKey::channel_key(&id, "#a")).unwrap());
    }

    #[gpui::test]
    fn an_unscanned_profile_covers_a_disabled_auto_join_channel(cx: &mut TestAppContext) {
        let (mut settings, id) = saved_secrets(cx);
        let profile = settings.selected_profile_mut().unwrap();
        profile.set_server_password_saved(false);
        profile.set_sasl_password_saved(false);
        profile.channels = "-#b".into();
        profile.keyed_channels = None;
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = super::escape_tests::open(cx, settings);
        let store = cx.update(|_, cx| crate::secrets::store(cx));
        let key = SecretKey::channel_key(&id, "#b");
        store.set(&key, &Secret::new("secret")).unwrap();

        edit_host(&form, cx, "irc.new.example");
        assert!(cx.has_pending_prompt());
        let cancel = form.read_with(cx, |form, _| form.i18n.text("cancel"));
        cx.simulate_prompt_answer(&cancel);
        cx.run_until_parked();
        assert_eq!(saved_host(&form, cx), "irc.old.example");
        assert!(store.contains(&key).unwrap());

        edit_host(&form, cx, "irc.new.example");
        let confirm = form.read_with(cx, |form, _| form.i18n.text("host_change_confirm"));
        cx.simulate_prompt_answer(&confirm);
        cx.run_until_parked();
        assert_eq!(saved_host(&form, cx), "irc.new.example");
        assert!(!store.contains(&key).unwrap());
    }

    #[gpui::test]
    fn an_unscanned_profile_with_nothing_stored_asks_nothing(cx: &mut TestAppContext) {
        let (mut settings, _id) = saved_secrets(cx);
        let profile = settings.selected_profile_mut().unwrap();
        profile.set_server_password_saved(false);
        profile.set_sasl_password_saved(false);
        profile.keyed_channels = None;
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = super::escape_tests::open(cx, settings);

        edit_host(&form, cx, "irc.new.example");
        assert!(!cx.has_pending_prompt());
        assert_eq!(saved_host(&form, cx), "irc.new.example");
    }

    #[gpui::test]
    fn a_host_change_without_saved_passwords_asks_nothing(cx: &mut TestAppContext) {
        let mut settings = crate::settings_with_channels("#a");
        settings.selected_profile_mut().unwrap().host = "irc.old.example".into();
        let _file = crate::settings_file::TestFile::with(&settings);
        let (form, cx, _app) = super::escape_tests::open(cx, settings);

        edit_host(&form, cx, "irc.new.example");
        assert!(!cx.has_pending_prompt());
        assert_eq!(saved_host(&form, cx), "irc.new.example");
    }
}
