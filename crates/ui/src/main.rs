#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account_settings;
mod avatar_editor;
mod avatars;
mod color_picker;
mod decorations;
mod default_avatar;
mod desktop;
mod diagnostics;
mod experimental_settings;
mod image_upload;
mod input;
mod input_history;
mod ircv3_settings;
mod key_recorder;
mod localization;
mod log_list;
mod member_selection;
mod menu_bar;
mod notifier;
#[cfg(test)]
mod perf_baseline;
mod previews;
mod secrets;
mod session;
mod settings_reset;
mod settings_theme;
mod shortcut_settings;
mod shortcuts;
mod splitter;
mod theme;
mod whois;
mod window_layout;

use cayenchat_app::{
    AppState, Command, ConnectionStatus, MessageMeta, NetworkConfig, Selection,
    attachments::AttachmentFlow,
    notifications::{self, BurstLimiter, IncomingMessage, NotificationRules, Trigger},
    own_avatar::{Confirmed, OwnAvatar},
    timeline::TimelineLine,
};
use cayenchat_irc_core::{
    ChannelActivityKind, Connection, ConnectionConfig, Event, HistoryMessage, HistoryResume,
    Ircv3Options, MemberCommand, MessageReference, OlderHistoryStatus, RealNameFailure,
    SaslCredentials, WhoisInfo, WireDirection, valid_channel,
};
use cayenchat_model::{ConversationId, NetworkId, TimeOfDay, Timestamp};
use cayenchat_storage::{
    Appearance, ChannelNumberModifier, CredentialBackendKind, CredentialStore,
    DEFAULT_SUB_LOG_NAME_WIDTH, DarkColors, Ircv3Preferences, Language, LinuxDisplay,
    Notifications, SUB_LOG_NAME_WIDTHS, Secret, SecretKey, ServerProfile, Settings, TextEncoding,
    TextKeyTheme, ThemeMode, color_value,
};
use gpui::{prelude::*, *};
use input::TextInput;
use ircv3_settings::own_avatar_failure;
use localization::Localizer;
use log_list::LogList;
use notifier::{DesktopNotification, Notifier};
use session::ServerSession;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use theme::Theme;
use whois::WhoisWindow;

/// The channel log's share of the log height when nothing was dragged, and
/// the bounds of what dragging may leave to either log.
const DEFAULT_LOG_SPLIT: f32 = 0.5;
/// Pause after the last move or resize before the window layout is written.
const LAYOUT_SAVE_DELAY: Duration = Duration::from_millis(500);
/// Width of the right column (members and channel tree), its limits, and the
/// width the left column keeps.
const DEFAULT_RIGHT_WIDTH: f32 = 240.;
const RIGHT_WIDTH_MIN: f32 = 160.;
const LEFT_MIN_WIDTH: f32 = 360.;
/// Least height of the member list or the channel tree.
const RIGHT_PANE_MIN_HEIGHT: f32 = 80.;
const LOG_SPLIT_LIMITS: (f32, f32) = (0.1, 0.9);
/// Height of the draft row including its borders.
const DRAFT_ROW_HEIGHT: f32 = 38.;

const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(3),
    Duration::from_secs(6),
    Duration::from_secs(12),
    Duration::from_secs(24),
    Duration::from_secs(30),
];
/// Most lines the combined other-channel log keeps on screen.
const SUB_LOG_LIMIT: usize = 1_000;
/// Worker events applied per update before yielding to input and redraws.
const EVENT_BATCH_LIMIT: usize = 256;

fn channel_activity_text(actor: &str, kind: ChannelActivityKind) -> String {
    fn with_detail(actor: &str, action: &str, detail: Option<String>) -> String {
        match detail.filter(|detail| !detail.is_empty()) {
            Some(detail) => format!("{actor} {action} ({detail})"),
            None => format!("{actor} {action}"),
        }
    }

    match kind {
        ChannelActivityKind::Joined { mask } => with_detail(actor, "has joined", mask),
        ChannelActivityKind::Left { reason } => with_detail(actor, "has left", reason),
        ChannelActivityKind::Quit { reason } => with_detail(actor, "has quit", reason),
        ChannelActivityKind::NickChanged { to } => format!("{actor} is now known as {to}"),
        ChannelActivityKind::ModeChanged { modes } => {
            format!("{actor} has changed mode: {modes}")
        }
    }
}

actions!(
    cayenchat,
    [
        CompleteNickname,
        SendMessage,
        Notice,
        HistoryPrevious,
        HistoryNext,
        OpenSettings,
        Disconnect,
        Reconnect,
        ToggleDebug,
        CopyDiagnostics,
        CopyLogSelection,
        FocusNextField,
        FocusPreviousField,
        Quit
    ]
);

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = cayenchat, no_json)]
struct Navigate {
    command: Command,
}

struct SettingsForm {
    values: Settings,
    server_list_open: bool,
    encoding_list_open: bool,
    language_list_open: bool,
    ircv3_server_list_open: bool,
    custom_host: Entity<TextInput>,
    port: Entity<TextInput>,
    nickname: Entity<TextInput>,
    username: Entity<TextInput>,
    realname: Entity<TextInput>,
    quit_message: Entity<TextInput>,
    channels: Entity<TextInput>,
    /// Password fields start empty; typing replaces a saved value.
    server_password: Entity<TextInput>,
    sasl_username: Entity<TextInput>,
    sasl_password: Entity<TextInput>,
    saved_server_password: bool,
    saved_sasl_password: bool,
    member_list_background: Entity<TextInput>,
    main_log_background: Entity<TextInput>,
    main_log_alternate: Entity<TextInput>,
    channel_event_color: Entity<TextInput>,
    highlight_color: Entity<TextInput>,
    sub_log_background: Entity<TextInput>,
    sub_log_alternate: Entity<TextInput>,
    dark_member_list_background: Entity<TextInput>,
    dark_main_log_background: Entity<TextInput>,
    dark_main_log_alternate: Entity<TextInput>,
    dark_channel_event_color: Entity<TextInput>,
    dark_highlight_color: Entity<TextInput>,
    dark_sub_log_background: Entity<TextInput>,
    dark_sub_log_alternate: Entity<TextInput>,
    main_log_font: Entity<TextInput>,
    sub_log_font: Entity<TextInput>,
    sub_log_name_width: Entity<TextInput>,
    member_font: Entity<TextInput>,
    channel_font: Entity<TextInput>,
    input_font: Entity<TextInput>,
    time_font: Entity<TextInput>,
    /// Comma-separated notification keywords.
    keywords: Entity<TextInput>,
    /// Draft URL of our own avatar for the selected server (IRCv3 tab).
    /// Editing it saves the draft; only Publish sends it.
    avatar_url: Entity<TextInput>,
}

impl SettingsForm {
    fn new(values: Settings, i18n: &Localizer, store: &CredentialStore, cx: &mut App) -> Self {
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
            sub_log_name_width: field(
                &DEFAULT_SUB_LOG_NAME_WIDTH.to_string(),
                &values.appearance.sub_log_name_width.to_string(),
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

    fn snapshot(&self, cx: &App) -> Result<Settings, String> {
        let mut settings = self.values.clone();
        let host = self.custom_host.read(cx).text().trim().to_owned();
        // Without a server the host and port fields are hidden and unused.
        let port = match self.port.read(cx).text().trim().parse() {
            Ok(port) if port != 0 => port,
            _ if settings.servers.is_empty() => 6667,
            _ => return Err(i18n_error(settings.language, "port_invalid")),
        };
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        if let Some(profile) = settings.selected_profile_mut() {
            profile.host = host;
            profile.port = port;
            profile.nickname = value(&self.nickname);
            profile.username = value(&self.username);
            profile.realname = value(&self.realname);
            profile.quit_message = value(&self.quit_message);
            profile.channels = value(&self.channels);
            profile.sasl_username = value(&self.sasl_username);
            profile.avatar_url = value(&self.avatar_url);
        }
        settings.appearance = Appearance {
            member_list_background: value(&self.member_list_background),
            main_log_background: value(&self.main_log_background),
            main_log_alternate: value(&self.main_log_alternate),
            channel_event_color: value(&self.channel_event_color),
            highlight_color: value(&self.highlight_color),
            sub_log_background: value(&self.sub_log_background),
            sub_log_alternate: value(&self.sub_log_alternate),
            alternate_rows: self.values.appearance.alternate_rows,
            image_previews: self.values.appearance.image_previews,
            user_avatars: self.values.appearance.user_avatars,
            wrap_long_nicknames: self.values.appearance.wrap_long_nicknames,
            sub_log_name_width: value(&self.sub_log_name_width)
                .parse()
                .map_err(|_| "Combined log channel name width must be a number.".to_owned())?,
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
                highlight_color: value(&self.dark_highlight_color),
                sub_log_background: value(&self.dark_sub_log_background),
                sub_log_alternate: value(&self.dark_sub_log_alternate),
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
    fn text_fields(&self) -> [&Entity<TextInput>; 33] {
        [
            &self.custom_host,
            &self.port,
            &self.nickname,
            &self.username,
            &self.realname,
            &self.quit_message,
            &self.channels,
            &self.server_password,
            &self.sasl_username,
            &self.sasl_password,
            &self.member_list_background,
            &self.main_log_background,
            &self.main_log_alternate,
            &self.channel_event_color,
            &self.highlight_color,
            &self.sub_log_background,
            &self.sub_log_alternate,
            &self.dark_member_list_background,
            &self.dark_main_log_background,
            &self.dark_main_log_alternate,
            &self.dark_channel_event_color,
            &self.dark_highlight_color,
            &self.dark_sub_log_background,
            &self.dark_sub_log_alternate,
            &self.main_log_font,
            &self.sub_log_font,
            &self.sub_log_name_width,
            &self.member_font,
            &self.channel_font,
            &self.input_font,
            &self.time_font,
            &self.keywords,
            &self.avatar_url,
        ]
    }

    /// Whether a typed password is waiting to be stored. With `window`, a
    /// field that still has focus is left alone so it is not stored mid-typing.
    fn pending_passwords(&self, window: Option<&Window>, cx: &App) -> bool {
        self.values
            .selected_profile()
            .is_some_and(|profile| profile.remember_passwords)
            && [&self.server_password, &self.sasl_password]
                .into_iter()
                .any(|field| password_ready(field, window, cx))
    }

    /// Stores typed passwords when saving is on, then empties the fields so
    /// plaintext does not stay in the form. With `window`, a focused field is
    /// skipped until the user leaves it.
    fn persist_passwords(
        &mut self,
        store: &CredentialStore,
        i18n: &Localizer,
        window: Option<&Window>,
        cx: &mut App,
    ) -> Result<(), String> {
        let Some(profile) = self.values.selected_profile().cloned() else {
            return Ok(());
        };
        if !profile.remember_passwords {
            return Ok(());
        }
        for (field, key, saved) in [
            (
                self.server_password.clone(),
                profile.server_password_key(),
                &mut self.saved_server_password,
            ),
            (
                self.sasl_password.clone(),
                profile.sasl_password_key(),
                &mut self.saved_sasl_password,
            ),
        ] {
            if !password_ready(&field, window, cx) {
                continue;
            }
            let text = field.read(cx).text().to_owned();
            store
                .set(&key, &Secret::new(text))
                .map_err(|error| secrets::error_text(i18n, &error))?;
            *saved = true;
            let hint = i18n.text("password_saved_placeholder");
            field.update(cx, |field, cx| {
                field.set_text("", cx);
                field.set_placeholder(&hint, cx);
            });
        }
        Ok(())
    }
}

impl SettingsForm {
    /// Shows another server: `change` selects it (or adds it). The shown
    /// server's edits are kept in `values` first, and its typed passwords
    /// are stored under its own keys (switching leaves the field), so
    /// nothing typed for one server can be saved into the next.
    fn switch_server(
        &mut self,
        change: impl FnOnce(&mut Settings),
        store: &CredentialStore,
        i18n: &Localizer,
        cx: &mut App,
    ) -> Result<(), String> {
        let mut settings = self.snapshot(cx)?;
        // A server without a host is not saved, so neither are its passwords.
        if settings
            .selected_profile()
            .is_some_and(|profile| !profile.host.is_empty())
        {
            self.persist_passwords(store, i18n, None, cx)?;
        }
        change(&mut settings);
        self.values = settings;
        self.show_selected(store, i18n, cx);
        Ok(())
    }

    /// Fills the server fields from the selected profile alone; password
    /// fields are emptied and say whether that server has saved passwords.
    fn show_selected(&mut self, store: &CredentialStore, i18n: &Localizer, cx: &mut App) {
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
            (&self.channels, &profile.channels),
            (&self.sasl_username, &profile.sasl_username),
            (&self.avatar_url, &profile.avatar_url),
        ] {
            field.update(cx, |field, cx| field.set_text(value, cx));
        }
        let (saved_server, saved_sasl) = saved_passwords(&profile, store);
        self.saved_server_password = saved_server;
        self.saved_sasl_password = saved_sasl;
        for (field, saved, key) in [
            (
                &self.server_password,
                saved_server,
                "server_password_placeholder",
            ),
            (&self.sasl_password, saved_sasl, "sasl_password"),
        ] {
            let placeholder = i18n.text(if saved {
                "password_saved_placeholder"
            } else {
                key
            });
            field.update(cx, |field, cx| {
                field.set_text("", cx);
                field.set_placeholder(&placeholder, cx);
            });
        }
        self.server_list_open = false;
        self.encoding_list_open = false;
        self.language_list_open = false;
        self.ircv3_server_list_open = false;
    }
}

/// A password field holds text and, with `window`, is not being typed in.
fn password_ready(field: &Entity<TextInput>, window: Option<&Window>, cx: &App) -> bool {
    !field.read(cx).text().is_empty()
        && window.is_none_or(|window| !field.read(cx).focus_handle(cx).is_focused(window))
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

/// Saved passwords for the profile, when password saving is on.
fn saved_connection_secrets(
    profile: &ServerProfile,
    store: &CredentialStore,
    i18n: &Localizer,
) -> Result<(Option<Secret>, Option<Secret>), String> {
    if !profile.remember_passwords {
        return Ok((None, None));
    }
    let get = |key: SecretKey| {
        store
            .get(&key)
            .map_err(|error| secrets::error_text(i18n, &error))
    };
    let server = get(profile.server_password_key())?;
    let sasl = if profile.sasl_enabled {
        get(profile.sasl_password_key())?
    } else {
        None
    };
    Ok((server, sasl))
}

fn connection_config(
    profile: &ServerProfile,
    language: Language,
    server_password: Option<Secret>,
    sasl_password: Option<Secret>,
) -> Result<ConnectionConfig, String> {
    let mut config = ConnectionConfig::tls(
        profile.host.clone(),
        profile.nickname.clone(),
        profile.channels(),
    );
    if profile.username.is_empty() {
        return Err(i18n_error(language, "username_required"));
    }
    config.username = profile.username.clone();
    config.realname = profile.realname.clone();
    config.quit_message = profile.quit_message.clone();
    config.port = profile.port;
    config.use_tls = profile.use_tls;
    config.verify_tls_certificates = profile.verify_tls_certificates;
    config.allow_plaintext_pass = profile.allow_plaintext_pass;
    config.encoding = profile.encoding.label().into();
    config.ircv3 = ircv3_options(profile.ircv3);
    config.shared_avatar = shared_peer_avatar(profile);
    if let Some(password) = server_password.filter(|value| !value.is_empty()) {
        config.server_password = Some(password.expose().to_owned());
    }
    if profile.sasl_enabled {
        config.sasl = Some(SaslCredentials {
            username: profile.sasl_username.clone(),
            password: sasl_password
                .map(|value| value.expose().to_owned())
                .unwrap_or_default(),
        });
    }
    config.validate()?;
    Ok(config)
}

/// The IRCv3 extensions a connection asks for, from the server's opt-ins.
fn ircv3_options(preferences: Ircv3Preferences) -> Ircv3Options {
    Ircv3Options {
        message_tags: preferences.message_tags,
        server_time: preferences.server_time,
        batch: preferences.batch,
        // Avatar metadata is always asked for when a server offers it:
        // references only, nothing is downloaded unless avatars are shown.
        metadata: true,
        peer_avatars: preferences.peer_avatars,
        chathistory: preferences.chathistory,
        confirmed_sending: preferences.confirmed_sending,
        accounts: preferences.accounts,
    }
}

/// The URL this server shares with other clients through CTCP AVATAR: the
/// one the user chose with "Share with Peers", while peer avatars are on
/// and it is still acceptable.
pub(crate) fn shared_peer_avatar(profile: &ServerProfile) -> Option<String> {
    let url = profile.peer_avatar_url.trim();
    let utf8 = profile.encoding == cayenchat_storage::TextEncoding::Utf8;
    (profile.ircv3.peer_avatars && ircv3_settings::peer_avatar_url_problem(url, utf8).is_none())
        .then(|| url.to_owned())
}

/// A connection from saved settings alone (at startup or from the channel
/// tree) uses only passwords saved in the credential store.
fn saved_connection_config(
    profile: &ServerProfile,
    language: Language,
    store: &CredentialStore,
) -> Result<ConnectionConfig, String> {
    let i18n = Localizer::new(language);
    let (server_password, sasl_password) = saved_connection_secrets(profile, store, &i18n)?;
    connection_config(profile, language, server_password, sasl_password)
}

/// Servers marked to connect at startup, in tree order, with their configs.
fn startup_connections(
    settings: &Settings,
    store: &CredentialStore,
) -> Vec<(String, Result<ConnectionConfig, String>)> {
    settings
        .ordered_servers()
        .filter(|profile| profile.connect_on_startup)
        .map(|profile| {
            (
                profile.id.clone(),
                saved_connection_config(profile, settings.language, store),
            )
        })
        .collect()
}

/// Deletes the saved passwords of profiles that no longer exist. New profiles
/// have fresh IDs even if cleanup fails or a removal has not been saved yet.
fn forget_removed_profiles(previous: &Settings, next: &Settings, store: &CredentialStore) {
    for server in &previous.servers {
        if !next.servers.iter().any(|kept| kept.id == server.id) {
            let _ = store.delete(&server.server_password_key());
            let _ = store.delete(&server.sasl_password_key());
        }
    }
}

/// Fills the account and real name a WHOIS reply lacks from what the
/// connection tracks; what the server said wins.
fn complete_whois(
    info: &mut WhoisInfo,
    tracked: &std::collections::HashMap<String, (Option<String>, Option<String>)>,
) {
    let key = cayenchat_irc_core::text::nickname_key(&info.nickname);
    let Some((account, realname)) = tracked.get(&key) else {
        return;
    };
    if info.account.is_none() {
        info.account = account.clone();
    }
    if info.realname.is_none() {
        info.realname = realname.clone();
    }
}

/// What the IRC adapter knows about an incoming message besides its text:
/// server-time, a usable `msgid`, and whether it is replayed history.
fn irc_message_meta(
    server_time: Option<std::time::SystemTime>,
    msgid: Option<&str>,
    account: Option<&str>,
    replayed: bool,
) -> MessageMeta {
    MessageMeta {
        server_time,
        native_id: msgid.and_then(cayenchat_model::NativeMessageId::new),
        account: account.and_then(cayenchat_model::ServicesAccount::new),
        ..MessageMeta::replayed(replayed)
    }
}

/// Requested history lines as timeline items: history, never news.
fn history_lines(messages: Vec<HistoryMessage>) -> Vec<TimelineLine> {
    messages
        .into_iter()
        .map(|message| TimelineLine {
            sender: message.sender,
            text: if message.notice {
                format!("[NOTICE] {}", message.text)
            } else {
                message.text
            },
            meta: irc_message_meta(
                message.server_time,
                message.msgid.as_deref(),
                message.account.as_deref(),
                true,
            ),
        })
        .collect()
}

/// Shown where a reconnect recovered only the most recent missed lines of a
/// channel; like other channel activity, in English.
const HISTORY_GAP_NOTE: &str = "Some messages sent while disconnected are not shown.";

/// Message rows between the top of the main log and the first visible row
/// within which scrolling asks for older history.
const OLDER_HISTORY_TRIGGER_ROWS: usize = 5;

/// The newest `limit` conversation lines (not channel activity or requested
/// history, which is context rather than news) across every conversation
/// except `excluded`, oldest first, as (sequence, conversation,
/// message index). Merges the conversation tails newest-first, so the cost
/// grows with `limit` and the number of conversations, not with every
/// retained line (with several servers there are many conversations).
fn newest_lines(
    conversations: &[cayenchat_model::Conversation],
    excluded: Option<ConversationId>,
    limit: usize,
) -> Vec<(u64, ConversationId, usize)> {
    use std::collections::BinaryHeap;

    fn previous_line(messages: &[cayenchat_model::Message], before: usize) -> Option<usize> {
        messages[..before].iter().rposition(|message| {
            !message.activity && message.provenance != cayenchat_model::Provenance::Requested
        })
    }

    // (sequence, conversation position, message index), newest on top.
    let mut heads: BinaryHeap<(u64, usize, usize)> = conversations
        .iter()
        .enumerate()
        .filter(|(_, conversation)| Some(conversation.id) != excluded)
        .filter_map(|(position, conversation)| {
            let index = previous_line(&conversation.messages, conversation.messages.len())?;
            Some((conversation.messages[index].sequence, position, index))
        })
        .collect();
    let mut rows = Vec::with_capacity(limit.min(1024));
    while rows.len() < limit
        && let Some((sequence, position, index)) = heads.pop()
    {
        let conversation = &conversations[position];
        rows.push((sequence, conversation.id, index));
        if let Some(previous) = previous_line(&conversation.messages, index) {
            heads.push((conversation.messages[previous].sequence, position, previous));
        }
    }
    rows.reverse();
    rows
}

/// Returns to the executor once, so other queued work runs before continuing.
async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await
}

fn i18n_error(language: Language, key: &str) -> String {
    Localizer::new(language).text(key)
}

struct ChatWindow {
    state: AppState,
    // Virtualized logs keep a separate scroll position per server or channel.
    main_lists: HashMap<Selection, LogList>,
    sub_list: LogList,
    // Created on first render, when this window's entity exists.
    panes: Option<ChatPanes>,
    tree_list: LogList,
    tree_rows: Vec<TreeRow>,
    // Selection and newest message sequence the combined log was built for.
    sub_source: Option<(Option<ConversationId>, u64)>,
    sub_rows: Vec<(ConversationId, usize)>,
    // Editing and IME state belong to each server or channel.
    inputs: HashMap<Selection, Entity<TextInput>>,
    feedback: Option<String>,
    /// Connection state of every configured server, keyed like the tree.
    sessions: HashMap<NetworkId, ServerSession>,
    next_network_id: u32,
    /// Saved settings; their server profiles define the networks.
    saved: Settings,
    server_menu: Option<ServerMenu>,
    member_menu: Option<MemberMenu>,
    channel_menu: Option<ChannelMenu>,
    member_prompt: Option<MemberPrompt>,
    /// Members chosen in the member list of the selected channel.
    member_selection: member_selection::MemberSelection,
    /// Sent drafts recalled with Up/Down in the draft input.
    input_history: input_history::InputHistory,
    /// One row per server whose nickname was rejected during registration,
    /// in the order the rejections arrived.
    nick_prompts: Vec<NickPrompt>,
    startup_connections: Vec<(NetworkId, Result<ConnectionConfig, String>)>,
    settings_window: Option<WindowHandle<SettingsWindow>>,
    window_handle: Option<WindowHandle<ChatWindow>>,
    /// Remember where the window and its panes were left (a setting), and
    /// the file that is written to. `None` writes nothing: tests, and the
    /// moment before startup supplies the path.
    restore_layout: bool,
    layout_file: Option<std::path::PathBuf>,
    layout_save: Option<Task<()>>,
    /// Where the window was last seen, for writing the layout without it.
    window_bounds: Option<WindowBounds>,
    /// The native window title last set from `render`.
    shown_title: String,
    /// Open WHOIS windows by network and lowercase nickname.
    whois_windows: HashMap<(NetworkId, String), WindowHandle<WhoisWindow>>,
    whois_replies: Vec<(NetworkId, WhoisInfo, bool)>,
    debug_enabled: bool,
    menu_bar: menu_bar::MenuBar,
    appearance: Appearance,
    theme_mode: ThemeMode,
    i18n: Localizer,
    log_focus: FocusHandle,
    log_selection: Option<LogSelection>,
    log_dragging: bool,
    /// The main log row whose URL the pointer is over; it shows a hand.
    url_hover: Option<(ConversationId, usize)>,
    /// Share of the two logs' height taken by the channel log; changed by
    /// dragging the bottom edge of the draft input, kept for this run only.
    log_split: f32,
    log_split_dragging: bool,
    /// Where the left column was last laid out, to turn a pointer position
    /// into `log_split`.
    left_column_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Width of the right column (members over channel tree), set by its
    /// splitter.
    right_width: f32,
    /// Height of the member list; `None` splits the right column evenly.
    members_height: Option<f32>,
    right_column_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Pasted or dropped images on their way to the external uploader.
    attachments: AttachmentFlow,
    /// Configured image hosting provider ID (IRC external uploads).
    image_provider: Option<String>,
    /// Replaces the configured uploader; tests use a fake one.
    uploader_override: Option<Arc<dyn cayenchat_upload::ExternalUploader>>,
    notifier: Notifier,
    notification_rules: NotificationRules,
    notification_burst: BurstLimiter,
    /// Inline image previews in the main channel log (application-wide).
    previews: previews::Previews,
    /// User avatar images for the main log and member list (application-wide).
    avatars: avatars::Avatars,
    /// Whether the chat window has keyboard focus; messages in the selected
    /// conversation of a focused window are already visible.
    window_active: bool,
    #[cfg(test)]
    pane_renders: usize,
}

/// An incoming channel or private message, as `notify_message` needs it.
struct ReceivedMessage<'a> {
    /// `None` for a private message.
    channel: Option<&'a str>,
    /// Where it was added; `None` for the server log.
    conversation: Option<ConversationId>,
    sender: &'a str,
    text: &'a str,
    notice: bool,
    mentioned: bool,
    replayed: bool,
}

struct ServerMenu {
    position: Point<Pixels>,
    network: NetworkId,
}

struct ChannelMenu {
    position: Point<Pixels>,
    network: NetworkId,
    conversation: ConversationId,
    channel: String,
    joined: bool,
    /// A private conversation offers Close instead of Join and Part.
    private: bool,
}

struct MemberMenu {
    position: Point<Pixels>,
    network: NetworkId,
    nickname: String,
    channel: String,
    /// The chosen members when the menu was opened on one of two or more
    /// chosen members: it then acts on all of them (and `nickname` is the one
    /// clicked). Empty for a menu about one member.
    group: Vec<String>,
}

#[derive(Clone, Copy)]
enum MemberPromptKind {
    PrivateMessage,
    Invite,
    /// Join a channel; the prompt is about the server, not a member.
    Join,
    /// Change our own nickname on the server.
    Nick,
}

/// Another nickname for a server that rejected `rejected` (432/433) during
/// registration. Several servers can wait for one at the same time.
struct NickPrompt {
    network: NetworkId,
    rejected: String,
    input: Entity<TextInput>,
    error: Option<String>,
    /// Set when shown from an event; render focuses the input if no other
    /// nickname field has focus.
    focus_pending: bool,
}

#[derive(Clone, Copy)]
enum MemberMenuChoice {
    Whois,
    PrivateMessage,
    Invite,
    GiveOp,
    Deop,
}

struct MemberPrompt {
    /// `None` centers the prompt in the window.
    position: Option<Point<Pixels>>,
    network: NetworkId,
    nickname: String,
    kind: MemberPromptKind,
    input: Entity<TextInput>,
    /// Set when opened without a window; render focuses the input.
    focus_pending: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LogPosition {
    row: usize,
    byte: usize,
}

#[derive(Clone, Copy)]
struct LogSelection {
    channel: ConversationId,
    anchor: LogPosition,
    cursor: LogPosition,
}

impl LogSelection {
    fn bounds(self) -> (LogPosition, LogPosition) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    fn range(self, row: usize, len: usize) -> Option<std::ops::Range<usize>> {
        let (start, end) = self.bounds();
        if row < start.row || row > end.row {
            return None;
        }
        let from = if row == start.row {
            start.byte.min(len)
        } else {
            0
        };
        let to = if row == end.row {
            end.byte.min(len)
        } else {
            len
        };
        (from < to).then_some(from..to)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Connection,
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
enum FontTarget {
    MainLog,
    SubLog,
    Members,
    Channels,
    Input,
    Time,
}

/// The color picker open under one color row of the appearance settings.
struct OpenColorPicker {
    /// The `#RRGGBB` field the picker edits.
    field: Entity<TextInput>,
    picker: Entity<color_picker::ColorPicker>,
    /// Keep the picker and the field in step while this exists.
    _subscriptions: Vec<Subscription>,
}

struct SettingsWindow {
    owner: WindowHandle<ChatWindow>,
    settings: SettingsForm,
    feedback: Option<String>,
    tab: SettingsTab,
    /// The category list on the left; Up and Down move through it.
    nav_focus: FocusHandle,
    font_picker: Option<FontTarget>,
    color_picker: Option<OpenColorPicker>,
    /// A key being recorded for a shortcut (the Shortcuts tab).
    shortcut_recording: Option<shortcut_settings::ShortcutRecording>,
    /// The system's font names, listed when the font list is first opened.
    fonts: Vec<String>,
    i18n: Localizer,
    /// Result of probing the system credential store; `None` while checking.
    system_store: Option<Result<(), String>>,
    /// Access token being entered to connect an image hosting account.
    upload_token: Entity<TextInput>,
    upload_connected: bool,
    upload_token_open: bool,
    /// What the settings file last received from this window; edits that
    /// differ from it are saved after a short pause.
    saved: Settings,
    window: AnyWindowHandle,
    autosave: Option<Task<()>>,
    /// Why the latest edits could not be saved, shown until they can be.
    autosave_error: Option<String>,
    /// Why Publish or Remove could not be started for our own avatar, or
    /// how an avatar image upload went.
    avatar_feedback: Option<String>,
    /// Avatar images on their way to the image host; the target is the
    /// server profile whose avatar URL draft receives the link.
    avatar_upload: AttachmentFlow<String>,
    /// The square selection for an avatar image, before uploading it.
    avatar_editor: Option<avatar_editor::AvatarEditor>,
    /// An avatar image is being decoded or encoded.
    avatar_opening: bool,
    /// Whether the connection switch was last drawn on, so the window is
    /// redrawn only when a connection comes up or goes down.
    connected_shown: bool,
    _subscriptions: Vec<Subscription>,
}

/// Pause after the last edit before settings are written.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(500);

impl ChatWindow {
    fn apply_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        self.i18n = Localizer::new(language);
        let placeholder = self.i18n.text("draft_placeholder");
        for input in self.inputs.values() {
            input.update(cx, |input, cx| input.set_placeholder(&placeholder, cx));
        }
        cx.set_menus(app_menus(self.debug_enabled, &self.i18n));
        for handle in self.whois_windows.values() {
            let i18n = self.i18n.clone();
            let _ = handle.update(cx, |view, _, cx| view.set_localizer(i18n, cx));
        }
        cx.notify();
        window.refresh();
    }

    fn status_text(&self, status: Option<&ConnectionStatus>) -> String {
        match status {
            Some(ConnectionStatus::OfflineMock) => self.i18n.text("status_offline"),
            Some(ConnectionStatus::Connecting) => self.i18n.text("status_connecting"),
            Some(ConnectionStatus::TransportConnected) => self.i18n.text("status_registering"),
            Some(ConnectionStatus::Registered) => self.i18n.text("status_connected"),
            Some(ConnectionStatus::Disconnected(reason)) => self
                .i18n
                .format("status_disconnected", &[("reason", reason)]),
            None => self.i18n.text("status_unknown"),
        }
    }

    fn window_title(&self) -> String {
        let app = if cfg!(feature = "test-build") {
            "CayenChat [test build]"
        } else {
            "CayenChat"
        };
        let Some(network) = self.state.selected_network() else {
            return app.into();
        };
        match self.state.selected_channel() {
            Some(channel) => {
                // A channel shows how many members it has, once the roster is known.
                let name = if channel.is_private() || channel.members.is_empty() {
                    channel.name.clone()
                } else {
                    format!("{} ({})", channel.name, channel.members.len())
                };
                let topic = cayenchat_irc_core::text::strip_formatting(&channel.topic);
                let topic = topic.split_whitespace().collect::<Vec<_>>().join(" ");
                if topic.is_empty() {
                    format!("{name} @ {} — {app}", network.name)
                } else {
                    format!("{name} @ {}: {topic} — {app}", network.name)
                }
            }
            None => format!("{} — {app}", network.name),
        }
    }

    /// The selected server; `None` only while no server is configured.
    fn selected_network_id(&self) -> Option<NetworkId> {
        self.state.selected_network().map(|network| network.id)
    }

    fn update_title(&self, window: &mut Window) {
        window.set_window_title(&self.window_title());
    }

    fn with_settings(
        saved: Settings,
        feedback: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let i18n = Localizer::new(saved.language);
        // Every configured server is a network in the tree, connected or not.
        let mut sessions = HashMap::new();
        let mut networks = Vec::new();
        for (index, profile) in saved.ordered_servers().enumerate() {
            let id = NetworkId(index as u32 + 1);
            sessions.insert(id, ServerSession::new(profile.id.clone()));
            networks.push(NetworkConfig {
                id,
                name: profile.host.clone(),
                channels: profile.channels(),
            });
        }
        let next_network_id = networks.len() as u32 + 1;
        let network_of = |profile_id: &str| {
            sessions
                .iter()
                .find(|(_, session)| session.profile_id == profile_id)
                .map(|(id, _)| *id)
        };
        let startup_connections = startup_connections(&saved, &secrets::store(cx))
            .into_iter()
            .filter_map(|(profile, config)| Some((network_of(&profile)?, config)))
            .collect();
        let state = AppState::with_networks(networks);
        let mut inputs = HashMap::new();
        let placeholder = i18n.text("draft_placeholder");
        for key in std::iter::once(Selection::None)
            .chain(
                state
                    .networks()
                    .iter()
                    .map(|server| Selection::Server(server.id)),
            )
            .chain(
                state
                    .conversations()
                    .iter()
                    .map(|channel| Selection::Channel(channel.id)),
            )
        {
            inputs.insert(key, cx.new(|cx| TextInput::new_live(&placeholder, cx)));
        }
        window.focus(&inputs[&state.selection()].focus_handle(cx));
        let mut this = Self {
            state,
            main_lists: HashMap::new(),
            sub_list: LogList::new(),
            panes: None,
            tree_list: LogList::new_top(),
            tree_rows: Vec::new(),
            sub_source: None,
            sub_rows: Vec::new(),
            inputs,
            feedback,
            sessions,
            next_network_id,
            server_menu: None,
            member_menu: None,
            channel_menu: None,
            member_prompt: None,
            member_selection: Default::default(),
            input_history: Default::default(),
            nick_prompts: Vec::new(),
            startup_connections,
            settings_window: None,
            window_handle: window.window_handle().downcast::<ChatWindow>(),
            restore_layout: saved.restore_window_layout,
            layout_file: None,
            layout_save: None,
            window_bounds: None,
            shown_title: String::new(),
            whois_windows: HashMap::new(),
            whois_replies: Vec::new(),
            debug_enabled: false,
            menu_bar: {
                let mut bar = menu_bar::MenuBar::new(window, cx, |this| &mut this.menu_bar);
                bar.set_always(!saved.menu_bar_auto_hide);
                bar
            },
            appearance: saved.appearance.clone(),
            theme_mode: saved.theme,
            image_provider: saved.image_upload.provider.clone(),
            notification_rules: notification_rules(&saved.notifications),
            previews: previews::Previews::new(saved.appearance.image_previews),
            avatars: avatars::Avatars::new(saved.appearance.user_avatars),
            saved,
            i18n,
            log_focus: cx.focus_handle(),
            log_selection: None,
            log_split: DEFAULT_LOG_SPLIT,
            log_split_dragging: false,
            left_column_bounds: Rc::new(Cell::new(None)),
            right_width: DEFAULT_RIGHT_WIDTH,
            members_height: None,
            right_column_bounds: Rc::new(Cell::new(None)),
            log_dragging: false,
            url_hover: None,
            attachments: AttachmentFlow::default(),
            uploader_override: None,
            notifier: Notifier::new(),
            notification_burst: BurstLimiter::default(),
            window_active: window.is_window_active(),
            #[cfg(test)]
            pane_renders: 0,
        };
        this.update_title(window);
        // GPUI reports the macOS/Windows appearance and, on Linux, the XDG
        // desktop portal color scheme.
        cx.observe_window_appearance(window, |this, _, cx| {
            theme::apply(this.theme_mode, &this.appearance, cx);
        })
        .detach();
        cx.observe_window_activation(window, |this, window, _| {
            this.window_active = window.is_window_active();
        })
        .detach();
        // Moving or resizing the window, and closing it, keep the layout.
        // The window's bounds are noted as they change so that the layout can
        // also be written where no window is at hand: when the application
        // quits (Cmd+Q) without the window being asked to close.
        cx.observe_window_bounds(window, |this, window, cx| {
            this.note_window_bounds(window);
            this.schedule_layout_save(cx);
        })
        .detach();
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = view.update(cx, |this, _| {
                this.note_window_bounds(window);
                this.save_layout_now();
            });
            true
        });
        cx.on_app_quit(|this, _| {
            this.save_layout_now();
            async {}
        })
        .detach();
        this.note_window_bounds(window);
        this
    }

    /// Remembers where the window is, for the next layout write.
    fn note_window_bounds(&mut self, window: &Window) {
        self.window_bounds = Some(window.window_bounds());
    }

    /// Takes over the pane sizes of a saved layout. They are clamped when
    /// drawn, so values from a larger window or another display are safe.
    fn apply_layout(&mut self, layout: &cayenchat_storage::layout::Layout) {
        if let Some(width) = layout.right_width {
            self.right_width = width;
        }
        self.members_height = layout.members_height.or(self.members_height);
        if let Some(split) = layout.log_split {
            self.log_split = split.clamp(LOG_SPLIT_LIMITS.0, LOG_SPLIT_LIMITS.1);
        }
    }

    /// Writes the layout shortly after the last change; a drag changes it
    /// many times a second.
    fn schedule_layout_save(&mut self, cx: &mut Context<Self>) {
        if !self.restore_layout || self.layout_file.is_none() {
            return;
        }
        self.layout_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LAYOUT_SAVE_DELAY).await;
            let _ = this.update(cx, |this, _| this.save_layout_now());
        }));
    }

    /// Writes the window's position and size (as last noted) and the pane
    /// sizes now. A failure only means the layout is not remembered, so it
    /// is not shown.
    fn save_layout_now(&mut self) {
        self.layout_save = None;
        let (true, Some(path), Some(window_bounds)) = (
            self.restore_layout,
            self.layout_file.as_ref(),
            self.window_bounds,
        ) else {
            return;
        };
        let (bounds, maximized) = match window_bounds {
            WindowBounds::Windowed(bounds) => (bounds, false),
            WindowBounds::Maximized(bounds) => (bounds, true),
            // Leaving full screen returns to the normal rectangle.
            WindowBounds::Fullscreen(bounds) => (bounds, false),
        };
        let layout = cayenchat_storage::layout::Layout {
            window: Some(cayenchat_storage::layout::WindowRect {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
            }),
            maximized,
            right_width: Some(self.right_width),
            members_height: self.members_height,
            log_split: Some(self.log_split),
            ..Default::default()
        };
        if let Err(error) = cayenchat_storage::layout::save_layout_to(path, &layout) {
            eprintln!("CayenChat: {error}");
        }
    }

    /// Our current nickname on `network`.
    fn own_nickname(&self, network: NetworkId) -> Option<&str> {
        self.sessions
            .get(&network)
            .and_then(|session| session.own_nickname.as_deref())
    }

    fn is_own_nickname(&self, network: NetworkId, nickname: &str) -> bool {
        self.own_nickname(network)
            .is_some_and(|own| cayenchat_irc_core::text::same_nickname(own, nickname))
    }

    /// Our own avatar on `network`, shown for our nickname instead of asking
    /// anyone for it: the one the server confirmed, else the one shared with
    /// peers. Read at draw time, so every row follows a change at once.
    fn own_avatar_for(&self, network: NetworkId, nickname: &str) -> Option<Arc<str>> {
        if !self.is_own_nickname(network, nickname) {
            return None;
        }
        let session = self.sessions.get(&network)?;
        if let Confirmed::Set(url) = session.own_avatar.confirmed() {
            return Some(url.as_str().into());
        }
        let profile = self.saved.profile(&session.profile_id)?;
        shared_peer_avatar(profile).map(|url| url.as_str().into())
    }

    /// Byte ranges of mentions of our nickname and of keywords in a channel
    /// message, drawn in the highlight color. Own lines, activity and
    /// replayed history are not highlighted.
    fn highlight_ranges(
        &self,
        network: NetworkId,
        message: &cayenchat_model::Message,
    ) -> Vec<std::ops::Range<usize>> {
        if message.activity
            || message.is_history()
            || self.is_own_nickname(network, &message.sender)
        {
            return Vec::new();
        }
        let mut ranges = self
            .own_nickname(network)
            .map(|own| cayenchat_irc_core::text::mention_ranges(&message.text, own))
            .unwrap_or_default();
        ranges.extend(notifications::keyword_ranges(
            &message.text,
            &self.notification_rules.keywords,
        ));
        ranges
    }

    /// Shows a desktop notification for an incoming IRC message when the
    /// rules ask for one and the message is not already in front of the user.
    fn notify_message(&mut self, network: NetworkId, message: ReceivedMessage) {
        use cayenchat_irc_core::text::{action_text, strip_formatting};

        let ReceivedMessage {
            channel,
            conversation,
            sender,
            text,
            notice,
            mentioned,
            replayed,
        } = message;

        let plain = match action_text(text) {
            Some(action) => format!("* {sender} {}", strip_formatting(action)),
            None => strip_formatting(text),
        };
        let Some(trigger) = self.notification_rules.trigger(IncomingMessage {
            text: &plain,
            channel: channel.is_some(),
            notice,
            from_self: self.is_own_nickname(network, sender),
            mentioned,
            replayed,
        }) else {
            return;
        };
        let visible = self.window_active
            && self.state.selection()
                == conversation.map_or(Selection::Server(network), Selection::Channel);
        if visible || !self.notification_burst.allow(Instant::now()) {
            return;
        }
        let summary = match (trigger, channel) {
            (Trigger::Mention | Trigger::Keyword, Some(channel)) => self.i18n.format(
                "notification_channel_title",
                &[("channel", channel), ("sender", sender)],
            ),
            _ => self
                .i18n
                .format("notification_private_title", &[("sender", sender)]),
        };
        self.notifier.show(DesktopNotification {
            summary,
            body: notifications::body_text(&plain),
        });
    }

    fn apply_appearance(
        &mut self,
        appearance: Appearance,
        mode: ThemeMode,
        cx: &mut Context<Self>,
    ) {
        theme::apply(mode, &appearance, cx);
        // Off: no more requests, pending loads are cancelled or ignored and
        // decoded images are released on the next draw.
        self.previews.set_enabled(appearance.image_previews);
        // Independent of previews; off also removes the avatar column.
        self.avatars.set_enabled(appearance.user_avatars);
        self.appearance = appearance;
        self.theme_mode = mode;
        // Fonts and row styles are drawn by the cached panes.
        cx.notify();
    }

    /// Handles one server's worker events as they arrive. The task sleeps
    /// while the connection is idle instead of waking on a timer, and incoming
    /// lines are shown without polling delay. Each server has its own task and
    /// yields after every batch, so a busy server cannot starve the others.
    fn spawn_event_pump(&mut self, network: NetworkId, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        let Some(mut events) = session.irc.as_mut().and_then(Connection::take_events) else {
            return;
        };
        let generation = session.generation;
        cx.spawn(async move |this, cx| {
            loop {
                let first = events.recv().await;
                let closed = first.is_none();
                let mut batch: Vec<Event> = first.into_iter().collect();
                while batch.len() < EVENT_BATCH_LIMIT
                    && let Some(event) = events.try_recv()
                {
                    batch.push(event);
                }
                let keep_going = this
                    .update(cx, |this, cx| {
                        this.is_current(network, generation)
                            && this.handle_events(network, batch, closed, cx)
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
                // Let input, redraws and other servers run between batches.
                yield_now().await;
            }
        })
        .detach();
        self.spawn_watchdog(network, generation, cx);
    }

    fn is_current(&self, network: NetworkId, generation: u64) -> bool {
        self.sessions
            .get(&network)
            .is_some_and(|session| session.generation == generation)
    }

    /// Reports a slow transport worker while the connection is still opening.
    fn spawn_watchdog(&self, network: NetworkId, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for (stage, after) in [(1, Duration::from_secs(5)), (2, Duration::from_secs(15))] {
                let Some(started) = this
                    .update(cx, |this, _| {
                        this.sessions
                            .get(&network)
                            .and_then(|session| session.connection_started)
                    })
                    .ok()
                    .flatten()
                else {
                    return;
                };
                Timer::after(after.saturating_sub(started.elapsed())).await;
                let waiting = this
                    .update(cx, |this, cx| {
                        let connecting =
                            this.state.status(network) == Some(&ConnectionStatus::Connecting);
                        let Some(session) = this.sessions.get_mut(&network) else {
                            return false;
                        };
                        let waiting = session.generation == generation
                            && session.connection_started.is_some()
                            && connecting;
                        if waiting && stage > session.watchdog_stage {
                            session.watchdog_stage = stage;
                            let elapsed = started.elapsed();
                            session.push_diagnostic(format!("[{:.1}s] UI is still waiting for the transport worker; inspect the latest diagnostic stage.", elapsed.as_secs_f32()));
                            cx.notify();
                        }
                        waiting
                    })
                    .unwrap_or(false);
                if !waiting {
                    return;
                }
            }
        })
        .detach();
    }

    fn network_of_profile(&self, profile_id: &str) -> Option<NetworkId> {
        self.sessions
            .iter()
            .find(|(_, session)| session.profile_id == profile_id)
            .map(|(id, _)| *id)
    }

    /// Creates draft inputs for servers and conversations that lack one.
    fn ensure_inputs(&mut self, cx: &mut Context<Self>) {
        let placeholder = self.i18n.text("draft_placeholder");
        // Selection::None has a draft too, shown while no server exists.
        let keys: Vec<Selection> = std::iter::once(Selection::None)
            .chain(
                self.state
                    .networks()
                    .iter()
                    .map(|server| Selection::Server(server.id)),
            )
            .chain(
                self.state
                    .conversations()
                    .iter()
                    .map(|channel| Selection::Channel(channel.id)),
            )
            .collect();
        for key in keys {
            self.inputs
                .entry(key)
                .or_insert_with(|| cx.new(|cx| TextInput::new_live(&placeholder, cx)));
        }
    }

    /// Drops UI state kept for conversations that no longer exist.
    fn forget_conversations(&mut self, removed: &[ConversationId]) {
        for id in removed {
            self.inputs.remove(&Selection::Channel(*id));
            self.main_lists.remove(&Selection::Channel(*id));
        }
        self.previews.retain_rows(|row| match row.selection {
            Selection::Channel(id) => !removed.contains(&id),
            _ => true,
        });
        if self
            .log_selection
            .is_some_and(|selection| removed.contains(&selection.channel))
        {
            self.log_selection = None;
        }
        self.sub_source = None;
    }

    /// Makes the networks follow the saved server profiles: new servers
    /// appear disconnected, removed ones are disconnected and dropped, and a
    /// changed host renames the server.
    fn apply_servers(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let mut networks = Vec::new();
        let mut realname_errors = Vec::new();
        for profile in settings.ordered_servers() {
            let id = match self.network_of_profile(&profile.id) {
                Some(id) => id,
                None => {
                    let id = NetworkId(self.next_network_id);
                    self.next_network_id += 1;
                    self.sessions
                        .insert(id, ServerSession::new(profile.id.clone()));
                    id
                }
            };
            // IRCv3 choices apply from the next connection, reconnects and
            // retries included; the current connection is left alone, except
            // that the URL it answers CTCP AVATAR with follows the explicit
            // Share / Stop Sharing at once.
            let shared = shared_peer_avatar(profile);
            if let Some(session) = self.sessions.get_mut(&id) {
                if let Some(config) = session.active_config.as_mut() {
                    config.ircv3 = ircv3_options(profile.ircv3);
                    config.shared_avatar = shared.clone();
                    // A new realname is sent at once with SETNAME when the
                    // connection has it; the server's answer says otherwise.
                    if config.realname != profile.realname {
                        config.realname = profile.realname.clone();
                        if let Some(connection) = &session.irc
                            && let Err(error) = connection.set_real_name(&profile.realname)
                        {
                            realname_errors.push((id, error));
                        }
                    }
                }
                if session.peer_avatars.enabled
                    && session.peer_avatars.answering != shared
                    && let Some(connection) = &session.irc
                    && connection.share_avatar(shared.as_deref()).is_ok()
                {
                    session.peer_avatars.answering = shared;
                }
            }
            networks.push(NetworkConfig {
                id,
                name: profile.host.clone(),
                channels: profile.channels(),
            });
        }
        for (id, error) in realname_errors {
            self.state.append_server_message(id, error);
        }
        let stale: Vec<NetworkId> = self
            .sessions
            .keys()
            .copied()
            .filter(|id| !networks.iter().any(|kept| kept.id == *id))
            .collect();
        for id in stale {
            if let Some(mut session) = self.sessions.remove(&id) {
                session.close();
            }
            self.inputs.remove(&Selection::Server(id));
            self.main_lists.remove(&Selection::Server(id));
            self.whois_windows.retain(|(network, _), _| *network != id);
            self.nick_prompts.retain(|prompt| prompt.network != id);
            if self
                .member_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.network == id)
            {
                self.member_prompt = None;
            }
        }
        let removed = self.state.sync_networks(&networks);
        self.forget_conversations(&removed);
        self.ensure_inputs(cx);
        self.saved = settings;
        cx.notify();
    }

    /// Connects the server of a saved profile with `config`, replacing any
    /// connection it had, and selects it.
    fn connect_profile(
        &mut self,
        profile_id: &str,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let network = self
            .network_of_profile(profile_id)
            .ok_or_else(|| self.i18n.text("not_connected"))?;
        let outcome = self.apply_connection(network, config, window, cx);
        let target = self
            .state
            .conversations()
            .iter()
            .find(|channel| channel.network == network)
            .map_or(Command::SelectServer(network), |channel| {
                Command::SelectChannel(channel.id)
            });
        self.dispatch(target, window, cx);
        outcome
    }

    /// Starts a fresh session on `network`: its channel logs restart from the
    /// configured channels while other servers keep theirs.
    fn apply_connection(
        &mut self,
        network: NetworkId,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(session) = self.sessions.get_mut(&network) else {
            return Err(self.i18n.text("not_connected"));
        };
        session.close();
        session.manual_disconnect = false;
        session.retry_attempt = 0;
        session.active_config = Some(config.clone());
        session.connection_starting(&config);
        session.diagnostics.clear();
        session.pending_whois.clear();
        session.connection_started = Some(Instant::now());
        session.watchdog_stage = 0;
        session.own_nickname = Some(config.nickname.clone());
        if self.menus_for(network) {
            self.member_menu = None;
            self.channel_menu = None;
        }
        if self
            .member_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.network == network)
        {
            self.member_prompt = None;
        }
        self.nick_prompts.retain(|prompt| prompt.network != network);
        let removed = self.state.reset_network(network, config.channels.clone());
        self.forget_conversations(&removed);
        self.main_lists.remove(&Selection::Server(network));
        self.ensure_inputs(cx);
        let outcome = match Connection::connect(config) {
            Ok(connection) => {
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.irc = Some(connection);
                }
                self.feedback = None;
                self.spawn_event_pump(network, cx);
                window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
                Ok(())
            }
            Err(error) => {
                self.record_disconnect(network, error.clone());
                self.schedule_retry(network, cx);
                Err(error)
            }
        };
        self.update_title(window);
        cx.notify();
        window.refresh();
        outcome
    }

    /// Whether the open member or channel menu belongs to `network`.
    fn menus_for(&self, network: NetworkId) -> bool {
        self.member_menu
            .as_ref()
            .is_some_and(|menu| menu.network == network)
            || self
                .channel_menu
                .as_ref()
                .is_some_and(|menu| menu.network == network)
    }

    /// Whether Disconnect has something to stop on `network`: a connection
    /// (including one still opening) or a scheduled reconnect.
    fn can_disconnect(&self, network: NetworkId) -> bool {
        self.sessions
            .get(&network)
            .is_some_and(|session| session.irc.is_some() || session.retry_pending)
    }

    fn can_disconnect_selected(&self) -> bool {
        self.selected_network_id()
            .is_some_and(|network| self.can_disconnect(network))
    }

    fn disconnect(&mut self, network: NetworkId, cx: &mut Context<Self>) {
        self.server_menu = None;
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        let cancelled_retry = session.retry_pending;
        session.manual_disconnect = true;
        session.retry_pending = false;
        session.retry_token += 1;
        if let Some(connection) = &session.irc {
            self.feedback = connection.disconnect().err();
        } else if cancelled_retry {
            let message = self.i18n.text("event_retry_cancelled");
            self.state.append_server_message(network, message);
        }
        cx.notify();
    }

    fn disconnect_action(&mut self, _: &Disconnect, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(network) = self.selected_network_id() {
            self.disconnect(network, cx);
        }
    }

    fn reconnect_action(&mut self, _: &Reconnect, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected_network_id() {
            Some(network) => self.reconnect(network, window, cx),
            None => self.open_settings_for(None, window, cx),
        }
    }

    /// Reconnects with the last configuration, or connects a server that has
    /// not been connected in this run from its saved settings.
    fn reconnect(&mut self, network: NetworkId, window: &mut Window, cx: &mut Context<Self>) {
        self.server_menu = None;
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        if session.active_config.is_none() {
            let profile_id = session.profile_id.clone();
            let config = self
                .saved
                .profile(&profile_id)
                .ok_or_else(|| self.i18n.text("not_connected"))
                .and_then(|profile| {
                    saved_connection_config(profile, self.saved.language, &secrets::store(cx))
                });
            match config {
                Ok(config) => {
                    let _ = self.apply_connection(network, config, window, cx);
                }
                Err(error) => {
                    self.feedback = Some(error);
                    self.open_settings_for(Some(profile_id), window, cx);
                }
            }
            return;
        }
        session.manual_disconnect = false;
        session.retry_pending = false;
        session.retry_attempt = 0;
        session.retry_token += 1;
        self.start_reconnect(network, cx);
    }

    fn start_reconnect(&mut self, network: NetworkId, cx: &mut Context<Self>) {
        let Some(config) = self.reconnect_config(network) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        session.retry_pending = false;
        if let Some(connection) = session.irc.take() {
            let _ = connection.disconnect();
        }
        session.generation += 1;
        session.own_avatar.connection_ended();
        session.connection_starting(&config);
        session.connection_started = Some(Instant::now());
        session.watchdog_stage = 0;
        self.state.set_status(network, ConnectionStatus::Connecting);
        self.state
            .append_server_message(network, self.i18n.text("event_reconnecting"));
        self.feedback = None;
        match Connection::connect(config) {
            Ok(connection) => {
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.irc = Some(connection);
                }
                self.spawn_event_pump(network, cx);
            }
            Err(error) => {
                self.record_disconnect(network, error);
                self.schedule_retry(network, cx);
            }
        }
        cx.notify();
    }

    /// The configuration of `network`'s last connection, for connecting
    /// again with the same server and settings. Channels the last connection
    /// was cut off in recover what they missed, if the server offers
    /// history.
    fn reconnect_config(&self, network: NetworkId) -> Option<ConnectionConfig> {
        let session = self.sessions.get(&network)?;
        let mut config = session.active_config.clone()?;
        config.history_targets_since = session.disconnected_at;
        config.resume_history = self
            .state
            .history_resume(network)
            .into_iter()
            .map(|resume| HistoryResume {
                channel: resume.name,
                after: MessageReference {
                    msgid: resume.native_id.map(|id| id.as_str().to_owned()),
                    time: resume.timestamp.map(Timestamp::to_system_time),
                },
            })
            .collect();
        Some(config)
    }

    fn schedule_retry(&mut self, network: NetworkId, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        if session.manual_disconnect || session.active_config.is_none() {
            return;
        }
        let delay = RETRY_DELAYS[session.retry_attempt.min(RETRY_DELAYS.len() - 1)];
        session.retry_pending = true;
        session.retry_attempt = session.retry_attempt.saturating_add(1);
        session.retry_token += 1;
        let token = session.retry_token;
        let seconds = delay.as_secs().to_string();
        self.state.append_server_message(
            network,
            self.i18n
                .format("event_retry_scheduled", &[("seconds", &seconds)]),
        );
        cx.spawn(async move |this, cx| {
            Timer::after(delay).await;
            let _ = this.update(cx, |this, cx| {
                let due = this.sessions.get(&network).is_some_and(|session| {
                    session.retry_token == token
                        && !session.manual_disconnect
                        && session.irc.is_none()
                });
                if due {
                    this.start_reconnect(network, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn record_disconnect(&mut self, network: NetworkId, reason: String) {
        if let Some(session) = self.sessions.get_mut(&network) {
            session.connection_started = None;
            session
                .disconnected_at
                .get_or_insert_with(std::time::SystemTime::now);
            session.push_diagnostic(format!("Disconnected: {reason}"));
            session.pending_whois.clear();
        }
        self.state
            .set_status(network, ConnectionStatus::Disconnected(reason.clone()));
        let message = self
            .i18n
            .format("status_disconnected", &[("reason", &reason)]);
        self.state.append_server_message(network, message.clone());
        // Another server's disconnect is shown in its tree row and log only.
        if self.selected_network_id() == Some(network) {
            self.feedback = Some(message);
        }
    }

    fn member_command(&mut self, command: MemberCommand, cx: &mut Context<Self>) {
        let Some(menu) = self.member_menu.take() else {
            return;
        };
        if command == MemberCommand::Whois {
            self.feedback = self.request_whois(menu.network, &menu.nickname, cx).err();
            cx.notify();
            return;
        }
        self.feedback = self
            .registered_connection(menu.network)
            .and_then(|connection| connection.send_member_command(&menu.nickname, command))
            .err();
        cx.notify();
    }

    /// The members of a group menu who are still chosen, now. The menu keeps
    /// the nicknames it was opened with, but while it is open someone may have
    /// left and someone else taken the nickname; the choice is trimmed with
    /// every roster, so a newcomer is not among the chosen.
    fn menu_group_now(&self, menu: &MemberMenu) -> Vec<String> {
        let Some(id) = self.state.channel_id(menu.network, &menu.channel) else {
            return Vec::new();
        };
        let Some(conversation) = self.state.conversations().iter().find(|c| c.id == id) else {
            return Vec::new();
        };
        let chosen = self
            .member_selection
            .nicknames(id, &conversation.members)
            .iter()
            .map(|nickname| cayenchat_irc_core::text::nickname_key(nickname))
            .collect::<Vec<_>>();
        menu.group
            .iter()
            .filter(|nickname| {
                chosen.contains(&cayenchat_irc_core::text::nickname_key(nickname.as_str()))
            })
            .cloned()
            .collect()
    }

    /// Gives or takes op or voice for every member the open menu acts on, as
    /// chosen at the moment of the click.
    fn member_modes(&mut self, mode: cayenchat_irc_core::MemberMode, cx: &mut Context<Self>) {
        let Some(menu) = self.member_menu.take() else {
            return;
        };
        let group = self.menu_group_now(&menu);
        // Nobody left to act on: nothing is sent.
        self.feedback = if group.is_empty() {
            None
        } else {
            self.registered_connection(menu.network)
                .and_then(|connection| connection.send_member_modes(&menu.channel, mode, &group))
                .err()
        };
        cx.notify();
    }

    fn registered_connection(&self, network: NetworkId) -> Result<&Connection, String> {
        match self
            .sessions
            .get(&network)
            .and_then(|session| session.irc.as_ref())
        {
            Some(connection)
                if self.state.status(network) == Some(&ConnectionStatus::Registered) =>
            {
                Ok(connection)
            }
            _ => Err(self.i18n.text("not_connected")),
        }
    }

    fn request_whois(
        &mut self,
        network: NetworkId,
        nickname: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.registered_connection(network)?
            .send_member_command(nickname, MemberCommand::Whois)?;
        if let Some(session) = self.sessions.get_mut(&network) {
            session.pending_whois.insert(nickname.to_lowercase());
        }
        cx.notify();
        Ok(())
    }

    fn joined_channels(&self, network: NetworkId) -> HashSet<String> {
        self.state
            .conversations()
            .iter()
            .filter(|conversation| {
                conversation.network == network
                    && !conversation.is_private()
                    && self.state.is_active_channel(conversation.id)
            })
            .map(|conversation| conversation.name.to_lowercase())
            .collect()
    }

    fn dismiss_menus(&mut self) -> bool {
        let server = self.server_menu.take().is_some();
        let member = self.member_menu.take().is_some();
        let channel = self.channel_menu.take().is_some();
        server || member || channel
    }

    fn channel_menu_command(&mut self, join: bool, cx: &mut Context<Self>) {
        let Some(menu) = self.channel_menu.take() else {
            return;
        };
        self.feedback = if join {
            self.join_channel(menu.network, &menu.channel, cx).err()
        } else {
            self.registered_connection(menu.network)
                .and_then(|connection| {
                    connection.send_command(&format!("/part {}", menu.channel), None)
                })
                .err()
        };
        cx.notify();
    }

    fn close_private_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.channel_menu.take() else {
            return;
        };
        if self.state.close_private(menu.conversation) {
            self.forget_conversations(&[menu.conversation]);
        }
        cx.notify();
    }

    fn join_channel(
        &mut self,
        network: NetworkId,
        channel: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.registered_connection(network)?
            .send_command(&format!("/join {channel}"), None)?;
        cx.notify();
        Ok(())
    }

    fn show_whois(
        &mut self,
        network: NetworkId,
        info: WhoisInfo,
        requested: bool,
        cx: &mut Context<Self>,
    ) {
        let key = (network, info.nickname.to_lowercase());
        if let Some(handle) = self.whois_windows.get(&key).copied() {
            let shown = handle.update(cx, |view, window, cx| {
                view.set_info(info.clone(), window, cx);
                window.activate_window();
            });
            if shown.is_ok() {
                cx.activate(true);
                return;
            }
            self.whois_windows.remove(&key);
        }
        if !requested || !info.found() {
            return;
        }
        let Some(owner) = self.window_handle else {
            return;
        };
        let joined = self.joined_channels(network);
        match WhoisWindow::open(owner, network, info, joined, self.i18n.clone(), cx) {
            Ok(handle) => {
                self.whois_windows.insert(key, handle);
                cx.activate(true);
            }
            Err(error) => {
                self.feedback = Some(self.i18n.format("whois_open_failed", &[("error", &error)]))
            }
        }
    }

    fn show_private_message_prompt(
        &mut self,
        network: NetworkId,
        nickname: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = window.viewport_size();
        let position = point(
            ((viewport.width - px(300.)) / 2.).max(px(0.)),
            ((viewport.height - px(140.)) / 2.).max(px(0.)),
        );
        self.member_menu = None;
        self.server_menu = None;
        self.show_member_prompt(
            network,
            nickname,
            MemberPromptKind::PrivateMessage,
            position,
            window,
            cx,
        );
    }

    fn open_member_prompt(
        &mut self,
        kind: MemberPromptKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.member_menu.take() else {
            return;
        };
        self.show_member_prompt(menu.network, menu.nickname, kind, menu.position, window, cx);
    }

    /// Opens a server-level prompt (join a channel, change nickname) where the
    /// server menu was.
    fn open_server_prompt(
        &mut self,
        kind: MemberPromptKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.server_menu.take() else {
            return;
        };
        // The nickname in use, else the one the profile would connect with.
        let nickname = self
            .own_nickname(menu.network)
            .map(str::to_owned)
            .or_else(|| {
                let session = self.sessions.get(&menu.network)?;
                Some(self.saved.profile(&session.profile_id)?.nickname.clone())
            })
            .unwrap_or_default();
        self.show_member_prompt(menu.network, nickname, kind, menu.position, window, cx);
    }

    fn show_member_prompt(
        &mut self,
        network: NetworkId,
        nickname: String,
        kind: MemberPromptKind,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = self.i18n.text(match kind {
            MemberPromptKind::PrivateMessage => "member_message_placeholder",
            MemberPromptKind::Invite | MemberPromptKind::Join => "member_channel_placeholder",
            MemberPromptKind::Nick => "nickname",
        });
        // A new nickname starts from the current one, all selected, so a
        // small change is a small edit and typing replaces it.
        let initial = match kind {
            MemberPromptKind::Nick => nickname.as_str(),
            _ => "",
        };
        let input = cx.new(|cx| TextInput::new_field(&placeholder, initial, false, cx));
        if matches!(kind, MemberPromptKind::Nick) {
            input.update(cx, |input, cx| input.select_everything(cx));
        }
        let viewport = window.viewport_size();
        self.feedback = None;
        self.member_prompt = Some(MemberPrompt {
            position: Some(point(
                position.x.min((viewport.width - px(300.)).max(px(0.))),
                position.y.min((viewport.height - px(140.)).max(px(0.))),
            )),
            network,
            nickname,
            kind,
            input: input.clone(),
            focus_pending: false,
        });
        window.focus(&input.focus_handle(cx));
        cx.notify();
    }

    /// Asks for another nickname after `network` rejected `rejected` during
    /// registration. Each server gets its own row; a repeated rejection on
    /// the same server updates that row.
    fn show_nick_prompt(&mut self, network: NetworkId, rejected: String, cx: &mut Context<Self>) {
        let suggestion = format!("{rejected}_");
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        if let Some(prompt) = self
            .nick_prompts
            .iter_mut()
            .find(|prompt| prompt.network == network)
        {
            prompt.rejected = rejected;
            prompt.error = None;
            prompt.focus_pending = true;
            prompt
                .input
                .update(cx, |input, cx| input.set_text(&suggestion, cx));
            return;
        }
        let placeholder = self.i18n.text("nick_prompt_placeholder");
        let input = cx.new(|cx| TextInput::new_field(&placeholder, &suggestion, false, cx));
        self.nick_prompts.push(NickPrompt {
            network,
            rejected,
            input,
            error: None,
            focus_pending: true,
        });
    }

    /// Retries registration with the nickname from the prompt. The nickname
    /// replaces the configured one for this session's reconnects only.
    fn retry_nickname(
        &mut self,
        network: NetworkId,
        nickname: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let nickname = nickname.trim().to_owned();
        if nickname.is_empty() {
            return Err(self.i18n.text("nick_prompt_required"));
        }
        let Some(session) = self.sessions.get_mut(&network) else {
            return Err(self.i18n.text("not_connected"));
        };
        if let Some(connection) = &session.irc {
            connection.change_nickname(&nickname)?;
        }
        if let Some(config) = session.active_config.as_mut() {
            config.nickname = nickname.clone();
        }
        session.own_nickname = Some(nickname.clone());
        let closed = session.irc.is_none();
        if closed {
            // The server closed the link while the prompt was open.
            session.manual_disconnect = false;
            session.retry_attempt = 0;
            session.retry_token += 1;
        }
        self.state.append_server_message(
            network,
            self.i18n
                .format("event_nick_retry", &[("nickname", &nickname)]),
        );
        if closed {
            self.start_reconnect(network, cx);
        }
        Ok(())
    }

    fn submit_nick_prompt(
        &mut self,
        network: NetworkId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self
            .nick_prompts
            .iter()
            .find(|prompt| prompt.network == network)
        else {
            return;
        };
        if prompt.input.read(cx).is_composing() {
            return;
        }
        let value = prompt.input.read(cx).text().to_owned();
        match self.retry_nickname(network, value, cx) {
            Ok(()) => self.close_nick_prompt(network, window, cx),
            Err(error) => {
                if let Some(prompt) = self
                    .nick_prompts
                    .iter_mut()
                    .find(|prompt| prompt.network == network)
                {
                    prompt.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    /// Gives up on a new nickname for `network` and disconnects it.
    fn cancel_nick_prompt(
        &mut self,
        network: NetworkId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_nick_prompt(network, window, cx);
        if self
            .sessions
            .get(&network)
            .is_some_and(|session| session.irc.is_some())
        {
            self.disconnect(network, cx);
        }
        cx.notify();
    }

    /// Removes the row and moves focus to the next waiting server, or back
    /// to the draft when none is left.
    fn close_nick_prompt(
        &mut self,
        network: NetworkId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.nick_prompts.retain(|prompt| prompt.network != network);
        match self.nick_prompts.first() {
            Some(next) => window.focus(&next.input.focus_handle(cx)),
            None => window.focus(&self.inputs[&self.state.selection()].focus_handle(cx)),
        }
    }

    /// The server whose nickname field has focus, if any.
    fn focused_nick_prompt(&self, window: &Window, cx: &App) -> Option<NetworkId> {
        self.nick_prompts
            .iter()
            .find(|prompt| prompt.input.read(cx).focus_handle(cx).is_focused(window))
            .map(|prompt| prompt.network)
    }

    fn cancel_member_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.member_prompt = None;
        self.feedback = None;
        window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
        cx.notify();
    }

    fn submit_member_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.member_prompt.as_ref() else {
            return;
        };
        if prompt.input.read(cx).is_composing() {
            return;
        }
        let value = prompt.input.read(cx).text().to_owned();
        let network = prompt.network;
        let result = match self.registered_connection(network) {
            Ok(connection) => match prompt.kind {
                MemberPromptKind::PrivateMessage if value.trim().is_empty() => {
                    Err(self.i18n.text("member_message_required"))
                }
                MemberPromptKind::PrivateMessage => {
                    connection.send_private_message(&prompt.nickname, &value, false)
                }
                MemberPromptKind::Invite => connection.send_member_command(
                    &prompt.nickname,
                    MemberCommand::Invite {
                        channel: value.trim().to_owned(),
                    },
                ),
                MemberPromptKind::Join if value.trim().is_empty() => {
                    Err(self.i18n.text("channel_join_required"))
                }
                MemberPromptKind::Join => {
                    connection.send_command(&format!("/join {}", value.trim()), None)
                }
                MemberPromptKind::Nick if value.trim().is_empty() => {
                    Err(self.i18n.text("nickname_change_required"))
                }
                MemberPromptKind::Nick => connection.change_nickname(value.trim()),
            },
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                self.member_prompt = None;
                self.feedback = None;
                window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
            }
            Err(error) => self.feedback = Some(error),
        }
        cx.notify();
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        let profile = self.selected_profile_id();
        self.open_settings_for(profile, window, cx);
    }

    /// Profile of the selected server, which the settings window edits first.
    fn selected_profile_id(&self) -> Option<String> {
        self.sessions
            .get(&self.selected_network_id()?)
            .map(|session| session.profile_id.clone())
    }

    /// Opens (or raises) the settings window on the Connection tab showing
    /// `profile`, when given.
    fn open_settings_for(
        &mut self,
        profile: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_tab(SettingsTab::Connection, profile, window, cx);
    }

    /// Opens (or raises) the settings window. `tab` is selected when the
    /// window is new or when it is not the Connection tab; `profile`, when
    /// given, is shown on the Connection tab.
    fn open_settings_tab(
        &mut self,
        tab: SettingsTab,
        profile: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = self.settings_window
            && handle
                .update(cx, |settings, window, cx| {
                    if tab != SettingsTab::Connection {
                        settings.show_tab(tab);
                        cx.notify();
                    } else if let Some(profile) = profile.clone() {
                        settings.show_tab(tab);
                        settings.select_server(profile, cx);
                    }
                    window.activate_window()
                })
                .is_ok()
        {
            return;
        }
        let mut settings = match cayenchat_storage::load() {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
                return;
            }
        };
        if let Some(profile) = profile
            && settings.profile(&profile).is_some()
        {
            settings.selected_server = profile;
        }
        let owner = window.window_handle().downcast::<ChatWindow>().unwrap();
        let bounds = Bounds::centered(None, size(px(900.), px(750.)), cx);
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.), px(540.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(self.i18n.text("settings_title").into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                cx.new(|cx| {
                    let mut view = SettingsWindow::new(owner, settings, window, cx);
                    view.tab = tab;
                    view
                })
            },
        ) {
            Ok(handle) => self.settings_window = Some(handle),
            Err(error) => {
                self.feedback = Some(
                    self.i18n
                        .format("settings_open_failed", &[("error", &error.to_string())]),
                )
            }
        }
        cx.notify();
    }

    fn toggle_debug(&mut self, _: &ToggleDebug, window: &mut Window, cx: &mut Context<Self>) {
        if !self.debug_enabled {
            self.show_diagnostics(window, cx);
            return;
        }
        self.debug_enabled = false;
        cx.set_menus(app_menus(self.debug_enabled, &self.i18n));
        cx.notify();
    }

    /// Copies the selected server's transcript.
    fn copy_diagnostics(&mut self, _: &CopyDiagnostics, _: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .selected_session()
            .map(|session| {
                session
                    .diagnostics
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn selected_session(&self) -> Option<&ServerSession> {
        self.sessions.get(&self.selected_network_id()?)
    }

    fn show_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.debug_enabled = true;
        cx.set_menus(app_menus(true, &self.i18n));
        if let Some(network) = self.selected_network_id() {
            self.dispatch(Command::SelectServer(network), window, cx);
        }
        self.sync_log_lists();
        self.main_lists[&self.state.selection()]
            .state
            .scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
    }

    /// The user scrolled conversation `id`'s main log so that row
    /// `first_visible` is at its top. Close to the oldest line, one older
    /// page is asked for; nothing is asked while the log is only open, and
    /// nothing while a page is on its way or history has run out.
    fn scrolled_main_log(&mut self, id: ConversationId, first_visible: usize) {
        let near_top = self
            .main_lists
            .get(&Selection::Channel(id))
            .is_some_and(|list| {
                list.message_rows_above(first_visible) <= OLDER_HISTORY_TRIGGER_ROWS
            });
        if near_top && self.state.selection() == Selection::Channel(id) {
            self.load_older_history(id);
        }
    }

    /// Asks the conversation's connection for one older page, if one may
    /// be asked for now.
    fn load_older_history(&mut self, id: ConversationId) {
        let Some(conversation) = self.state.conversations().iter().find(|c| c.id == id) else {
            return;
        };
        let (network, channel) = (conversation.network, conversation.name.clone());
        let Some(connection) = self.sessions.get(&network).and_then(|s| s.irc.as_ref()) else {
            return;
        };
        let Some(page) = self.state.request_older_history(id) else {
            return;
        };
        let reference = MessageReference {
            msgid: page.native_id.map(|id| id.as_str().to_owned()),
            time: page.timestamp.map(Timestamp::to_system_time),
        };
        if let Err(error) =
            connection.request_older_history(&channel, page.request, reference, page.limit)
        {
            self.state.older_history_failed(id, page.request);
            self.push_diagnostic(network, format!("Older history for {channel}: {error}"));
        }
    }

    fn start_log_selection(
        &mut self,
        channel: ConversationId,
        row: usize,
        byte: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = LogPosition { row, byte };
        self.log_selection = Some(LogSelection {
            channel,
            anchor: position,
            cursor: position,
        });
        self.log_dragging = true;
        window.focus(&self.log_focus);
        cx.notify();
    }

    fn extend_log_selection(
        &mut self,
        channel: ConversationId,
        row: usize,
        byte: usize,
        cx: &mut Context<Self>,
    ) {
        if self.log_dragging
            && let Some(selection) = self.log_selection.as_mut()
            && selection.channel == channel
        {
            selection.cursor = LogPosition { row, byte };
            cx.notify();
        }
    }

    fn finish_log_selection(&mut self) {
        self.log_dragging = false;
    }

    fn copy_selected_log(&mut self, cx: &mut Context<Self>) {
        let (Some(selection), Some(channel)) = (self.log_selection, self.state.selected_channel())
        else {
            return;
        };
        if selection.channel != channel.id {
            return;
        }
        let (start, end) = selection.bounds();
        if start == end {
            return;
        }
        let pieces: Vec<_> = (start.row..=end.row)
            .filter_map(|row| {
                channel.messages.get(row).map(|message| {
                    let range = selection.range(row, message.text.len()).unwrap_or(0..0);
                    message.text[range].to_owned()
                })
            })
            .collect();
        if !pieces.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(pieces.join("\n")));
        }
    }

    fn copy_log_selection(&mut self, _: &CopyLogSelection, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected_log(cx);
    }

    fn copy_log_selection_menu(&mut self, _: &input::Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected_log(cx);
    }

    /// Moves the split so the draft row's bottom edge follows the pointer.
    fn drag_log_split(&mut self, pointer_y: Pixels, cx: &mut Context<Self>) {
        let Some(bounds) = self.left_column_bounds.get() else {
            return;
        };
        let flexible = f32::from(bounds.size.height) - DRAFT_ROW_HEIGHT;
        if flexible <= 0. {
            return;
        }
        let main_height = f32::from(pointer_y - bounds.top()) - DRAFT_ROW_HEIGHT;
        self.log_split = (main_height / flexible).clamp(LOG_SPLIT_LIMITS.0, LOG_SPLIT_LIMITS.1);
        self.schedule_layout_save(cx);
        cx.notify();
    }

    /// Applies a click on row `index` of the selected channel's member list.
    fn click_member(&mut self, index: usize, click: member_selection::Click) {
        let Some(channel) = self.state.selected_channel() else {
            return;
        };
        let (conversation, members) = (channel.id, channel.members.clone());
        self.member_selection
            .click(conversation, &members, index, click);
    }

    fn push_diagnostic(&mut self, network: NetworkId, line: String) {
        if let Some(session) = self.sessions.get_mut(&network) {
            session.push_diagnostic(line);
        }
    }

    fn dispatch(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.state.dispatch(command);
        self.log_selection = None;
        self.feedback = None;
        self.update_title(window);
        window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
        cx.notify();
        // Redraw the pane contents together with the native title change.
        window.refresh();
    }

    fn navigate(&mut self, action: &Navigate, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(action.command, window, cx);
    }

    fn complete_nickname(
        &mut self,
        _: &CompleteNickname,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .member_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.input.read(cx).focus_handle(cx).is_focused(window))
            || self.focused_nick_prompt(window, cx).is_some()
        {
            return;
        }
        let Some(channel) = self.state.selected_channel() else {
            return;
        };
        let members = channel.members.clone();
        let input = self.inputs[&self.state.selection()].clone();
        if input.read(cx).is_composing() {
            return;
        }
        input.update(cx, |input, cx| {
            input.complete_nickname(&members, window, cx)
        });
    }

    /// Applies a batch of worker events. `worker_closed` means the worker's
    /// event stream ended. Returns whether to keep listening.
    fn handle_events(
        &mut self,
        network: NetworkId,
        batch: Vec<Event>,
        worker_closed: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = !batch.is_empty();
        let mut disconnected = false;
        let mut refused = false;
        // The IRCv3 settings tab shows our own avatar and whether it can
        // be published; it is redrawn only when that may have changed.
        let own_avatar_changed = batch.iter().any(|event| {
            matches!(
                event,
                Event::Registered { .. }
                    | Event::Disconnected(_)
                    | Event::Refused(_)
                    | Event::AvatarsReset
                    | Event::MetadataReady
                    | Event::OwnAvatar { .. }
                    | Event::OwnAvatarFailed { .. }
            )
        });
        for event in batch {
            match &event {
                Event::Disconnected(_) => disconnected = true,
                Event::Refused(_) => {
                    disconnected = true;
                    refused = true;
                }
                Event::NicknameRejected { nickname } => {
                    self.show_nick_prompt(network, nickname.clone(), cx)
                }
                Event::Registered { .. } => {
                    self.nick_prompts.retain(|prompt| prompt.network != network);
                }
                _ => {}
            }
            self.handle_event(network, event);
        }
        for (network, info, requested) in std::mem::take(&mut self.whois_replies) {
            self.show_whois(network, info, requested, cx);
        }
        if own_avatar_changed || worker_closed {
            self.refresh_settings(cx);
        }
        if changed {
            let joined = self.joined_channels(network);
            self.whois_windows.retain(|(owner, _), handle| {
                *owner != network
                    || handle
                        .update(cx, |view, _, cx| view.set_joined(joined.clone(), cx))
                        .is_ok()
            });
            self.ensure_inputs(cx);
            cx.notify();
        }
        if worker_closed && !disconnected {
            self.push_diagnostic(
                network,
                "IRC worker ended without a disconnect event.".into(),
            );
            self.handle_event(
                network,
                Event::Disconnected("IRC worker stopped unexpectedly.".into()),
            );
            cx.notify();
        }
        if disconnected || worker_closed {
            if let Some(session) = self.sessions.get_mut(&network) {
                session.irc = None;
            }
            if refused {
                // Retrying rejected credentials risks account lockout or a ban.
                self.state
                    .append_server_message(network, self.i18n.text("event_retry_refused"));
            } else {
                self.schedule_retry(network, cx);
            }
            return false;
        }
        true
    }

    fn handle_event(&mut self, network: NetworkId, event: Event) {
        match event {
            Event::Diagnostic { elapsed, message } => {
                self.push_diagnostic(
                    network,
                    format!("[{:.1}s] {message}", elapsed.as_secs_f32()),
                );
            }
            Event::Wire {
                elapsed,
                direction,
                line,
            } => {
                let arrow = match direction {
                    WireDirection::Sent => "→",
                    WireDirection::Received => "←",
                };
                self.push_diagnostic(
                    network,
                    format!("[{:.1}s] {arrow} {line}", elapsed.as_secs_f32()),
                );
            }
            Event::TransportConnected => {
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.connection_started = None;
                }
                self.state
                    .set_status(network, ConnectionStatus::TransportConnected);
                self.state
                    .append_server_message(network, self.i18n.text("event_transport_connected"));
            }
            Event::Registered { nickname } => {
                // A new connection: nobody's avatar is known yet.
                self.state.end_avatars(network);
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.connection_started = None;
                    session.disconnected_at = None;
                    session.retry_attempt = 0;
                    session.own_nickname = Some(nickname.clone());
                }
                self.state.set_status(network, ConnectionStatus::Registered);
                self.state.append_server_message(
                    network,
                    self.i18n
                        .format("event_registered", &[("nickname", &nickname)]),
                );
            }
            Event::Joined { channel } => {
                self.state.joined_channel(network, &channel);
                self.state.append_server_message(
                    network,
                    self.i18n.format("event_joined", &[("channel", &channel)]),
                );
            }
            Event::Parted { channel } => {
                self.state.parted_channel(network, &channel);
                self.state.append_server_message(
                    network,
                    self.i18n.format("event_parted", &[("channel", &channel)]),
                );
            }
            Event::NickChanged { nickname } => {
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.own_nickname = Some(nickname.clone());
                }
                self.state.append_server_message(
                    network,
                    self.i18n
                        .format("event_nick_changed", &[("nickname", &nickname)]),
                );
            }
            Event::ChannelMessage {
                channel,
                sender,
                text,
                notice,
                mentioned,
                server_time,
                msgid,
                account,
                replayed,
            } => {
                // A copy of a message the conversation already has (overlapping
                // history) neither notifies nor highlights again.
                if !self.state.append_channel_message_at(
                    network,
                    &channel,
                    &sender,
                    &text,
                    notice,
                    irc_message_meta(server_time, msgid.as_deref(), account.as_deref(), replayed),
                ) {
                    return;
                }
                let highlighted = !replayed
                    && !self.is_own_nickname(network, &sender)
                    && (mentioned
                        || notifications::contains_keyword(
                            &cayenchat_irc_core::text::strip_formatting(&text),
                            &self.notification_rules.keywords,
                        ));
                let conversation = self.state.channel_id(network, &channel);
                self.notify_message(
                    network,
                    ReceivedMessage {
                        channel: Some(&channel),
                        conversation,
                        sender: &sender,
                        text: &text,
                        notice,
                        mentioned,
                        replayed,
                    },
                );
                if highlighted {
                    self.state.mark_highlighted(network, &channel);
                }
            }
            Event::ChannelActivity {
                channel,
                actor,
                kind,
                server_time,
            } => {
                let text = channel_activity_text(&actor, kind);
                self.state.append_channel_activity_at(
                    network,
                    &channel,
                    text,
                    MessageMeta::at(server_time),
                );
            }
            Event::PrivateMessage {
                sender,
                text,
                notice,
                server_time,
                msgid,
                account,
                replayed,
            } => {
                let meta =
                    irc_message_meta(server_time, msgid.as_deref(), account.as_deref(), replayed);
                // A PRIVMSG opens a private conversation; a NOTICE (usually
                // services and bots) joins one only if it already exists,
                // and otherwise stays in the server log as before.
                let key = cayenchat_irc_core::text::nickname_key(&sender);
                let conversation = self
                    .state
                    .private_conversation(network, &key, &sender, !notice);
                match conversation {
                    Some(id) => {
                        if !self
                            .state
                            .append_conversation_message(id, &sender, &text, notice, meta, true)
                        {
                            return;
                        }
                        if !notice && !replayed {
                            self.state.highlight(id);
                        }
                    }
                    None => {
                        let line = if notice {
                            format!("-{sender}- {text}")
                        } else {
                            format!("<{sender}> {text}")
                        };
                        self.state.append_server_message_at(network, line, meta);
                    }
                }
                self.notify_message(
                    network,
                    ReceivedMessage {
                        channel: None,
                        conversation,
                        sender: &sender,
                        text: &text,
                        notice,
                        mentioned: false,
                        replayed,
                    },
                );
            }
            Event::OwnPrivateMessage {
                target,
                text,
                notice,
                server_time,
                msgid,
                replayed,
            } => {
                let meta = irc_message_meta(server_time, msgid.as_deref(), None, replayed);
                self.append_own_private(network, &target, &text, notice, meta);
            }
            Event::UserNickChanged { from, to } => {
                let from_key = cayenchat_irc_core::text::nickname_key(&from);
                if let Some(id) = self.state.private_id(network, &from_key) {
                    let to_key = cayenchat_irc_core::text::nickname_key(&to);
                    self.state.rename_private(network, &from_key, &to_key, &to);
                    self.state
                        .append_conversation_activity(id, format!("{from} is now known as {to}"));
                }
            }
            Event::UserQuit { nickname, reason } => {
                let key = cayenchat_irc_core::text::nickname_key(&nickname);
                if let Some(id) = self.state.private_id(network, &key) {
                    let text =
                        channel_activity_text(&nickname, ChannelActivityKind::Quit { reason });
                    self.state.append_conversation_activity(id, text);
                }
            }
            // A peer's nickname is a direct-message conversation found by
            // target discovery: it is shown from now on, its history follows.
            Event::HistoryRequested {
                channel,
                resumed: false,
            } if !valid_channel(&channel) => {
                let key = cayenchat_irc_core::text::nickname_key(&channel);
                if let Some(id) = self
                    .state
                    .private_conversation(network, &key, &channel, true)
                {
                    self.state.history_requested_for(id);
                }
            }
            Event::HistoryRequested {
                channel,
                resumed: false,
            } => self.state.history_requested(network, &channel),
            // Lines missed while disconnected go where the log was cut off.
            Event::HistoryRequested {
                channel,
                resumed: true,
            } => self.state.history_resumed(network, &channel),
            // Requested history is context, not news: no notification,
            // highlight or unread mark.
            Event::ChannelHistory {
                channel,
                messages,
                incomplete,
            } => {
                let lines = history_lines(messages);
                let gap = incomplete.then(|| HISTORY_GAP_NOTE.to_owned());
                if valid_channel(&channel) {
                    self.state
                        .insert_resumed_history(network, &channel, lines, gap);
                } else {
                    let key = cayenchat_irc_core::text::nickname_key(&channel);
                    if let Some(id) = self.state.private_id(network, &key) {
                        self.state.insert_history_for(id, lines, gap);
                    }
                }
            }
            Event::HistoryAvailable(available) => self.state.set_history_paging(network, available),
            // Older pages go above everything the log holds; the main log
            // keeps its top row in place (`LogList::sync`).
            Event::OlderChannelHistory {
                channel,
                request,
                messages,
                status,
            } => {
                if let Some(id) = self.state.channel_id(network, &channel) {
                    if status == OlderHistoryStatus::Failed {
                        self.state.older_history_failed(id, request);
                    } else {
                        let added = self.state.insert_older_history(
                            id,
                            request,
                            history_lines(messages),
                            status == OlderHistoryStatus::Beginning,
                        );
                        // A text selection names rows by index; keep it on
                        // the same lines.
                        if let Some(selection) = self.log_selection.as_mut()
                            && selection.channel == id
                        {
                            selection.anchor.row += added;
                            selection.cursor.row += added;
                        }
                    }
                }
            }
            Event::Names { channel, users } => {
                self.state.set_members(network, &channel, users);
                // Whoever left (or renamed) is no longer chosen, so a later
                // user of that nickname is not.
                if let Some(id) = self.state.channel_id(network, &channel)
                    && let Some(conversation) =
                        self.state.conversations().iter().find(|c| c.id == id)
                {
                    self.member_selection
                        .retain_present(id, &conversation.members);
                }
            }
            Event::Topic { channel, topic } => self.state.set_topic(network, &channel, &topic),
            Event::ServerLine(line) => self.state.append_server_message(network, line),
            Event::UserAccount {
                nickname,
                account,
                realname,
            } => {
                if let Some(session) = self.sessions.get_mut(&network) {
                    let key = cayenchat_irc_core::text::nickname_key(&nickname);
                    session.user_accounts.insert(key, (account, realname));
                }
            }
            Event::UserAccountForgotten { nickname } => {
                if let Some(session) = self.sessions.get_mut(&network) {
                    let key = cayenchat_irc_core::text::nickname_key(&nickname);
                    session.user_accounts.remove(&key);
                }
            }
            Event::RealNameChanged { realname } => {
                let line = self
                    .i18n
                    .format("event_realname_changed", &[("realname", &realname)]);
                self.state.append_server_message(network, line);
            }
            Event::RealNameFailed(failure) => {
                let line = match failure {
                    RealNameFailure::Unsupported => self.i18n.text("event_realname_unsupported"),
                    RealNameFailure::Busy => self.i18n.text("event_realname_busy"),
                    RealNameFailure::Rejected(reason) => self
                        .i18n
                        .format("event_realname_rejected", &[("reason", &reason)]),
                };
                self.state.append_server_message(network, line);
            }
            Event::Whois(info) => {
                let mut info = *info;
                // Live tracking fills what WHOIS did not report.
                if let Some(session) = self.sessions.get(&network) {
                    complete_whois(&mut info, &session.user_accounts);
                }
                let key = (network, info.nickname.to_lowercase());
                let requested = self
                    .sessions
                    .get_mut(&network)
                    .is_some_and(|session| session.pending_whois.remove(&key.1));
                if requested && !info.found() && !self.whois_windows.contains_key(&key) {
                    self.feedback = Some(
                        self.i18n
                            .format("whois_not_found", &[("nickname", &info.nickname)]),
                    );
                }
                if requested || self.whois_windows.contains_key(&key) {
                    self.whois_replies.push((network, info, requested));
                }
            }
            Event::OutgoingAccepted {
                local_id,
                channel,
                text,
                notice,
            } => {
                let before = self.state.latest_sequence();
                if valid_channel(&channel) {
                    let nickname = self
                        .sessions
                        .get(&network)
                        .and_then(|session| session.own_nickname.as_deref())
                        .unwrap_or("me");
                    self.state
                        .append_channel_message(network, &channel, nickname, &text, notice, false);
                } else if cayenchat_irc_core::valid_nickname(&channel) {
                    self.append_own_private(network, &channel, &text, notice, MessageMeta::live());
                } else {
                    self.state.append_server_message(
                        network,
                        self.i18n
                            .format("event_message_queued", &[("channel", &channel)]),
                    );
                }
                // Confirmed sending: find the line just added again when the
                // server's echo arrives.
                if let Some(local_id) = local_id
                    && self.state.latest_sequence() > before
                {
                    let id = if valid_channel(&channel) {
                        self.state.channel_id(network, &channel)
                    } else {
                        let key = cayenchat_irc_core::text::nickname_key(&channel);
                        self.state.private_id(network, &key)
                    };
                    if let (Some(id), Some(session)) = (id, self.sessions.get_mut(&network))
                        && session.pending_sends.len() < 64
                    {
                        session
                            .pending_sends
                            .insert(local_id, (id, self.state.latest_sequence(), notice));
                    }
                }
            }
            Event::OutgoingConfirmed {
                local_id,
                text,
                msgid,
                server_time,
            } => {
                let pending = self
                    .sessions
                    .get_mut(&network)
                    .and_then(|session| session.pending_sends.remove(&local_id));
                if let Some((id, sequence, notice)) = pending {
                    let meta = irc_message_meta(server_time, msgid.as_deref(), None, false);
                    self.state.confirm_message(id, sequence, text, notice, meta);
                }
            }
            Event::OutgoingFailed { local_id, reason } => {
                let pending = self
                    .sessions
                    .get_mut(&network)
                    .and_then(|session| session.pending_sends.remove(&local_id));
                if let Some((id, sequence, _)) = pending {
                    self.state.fail_message(id, sequence);
                }
                let line = self
                    .i18n
                    .format("event_message_failed", &[("reason", &reason)]);
                self.state.append_server_message(network, line);
            }
            Event::NicknameRejected { nickname } => {
                self.state.append_server_message(
                    network,
                    self.i18n
                        .format("event_nick_rejected", &[("nickname", &nickname)]),
                );
            }
            Event::Disconnected(reason) | Event::Refused(reason) => {
                // What the server never confirmed is shown as not delivered.
                let unconfirmed: Vec<_> = self
                    .sessions
                    .get_mut(&network)
                    .map(|session| session.pending_sends.drain().collect())
                    .unwrap_or_default();
                for (_, (id, sequence, _)) in unconfirmed {
                    self.state.fail_message(id, sequence);
                }
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.user_accounts.clear();
                }
                self.state.end_avatars(network);
                self.update_own_avatar(network, OwnAvatar::connection_ended);
                self.record_disconnect(network, reason);
            }
            // Avatar references are recorded whether or not they are shown;
            // showing them (and downloading images) is the Appearance setting.
            Event::UserAvatar { nickname, url } => {
                let key = cayenchat_irc_core::text::nickname_key(&nickname);
                self.state.set_avatar(network, &key, url.as_deref());
            }
            Event::AvatarMoved { from, to } => {
                let from = cayenchat_irc_core::text::nickname_key(&from);
                let to = cayenchat_irc_core::text::nickname_key(&to);
                self.state.rename_avatar(network, &from, &to);
            }
            Event::AvatarsReset => {
                self.state.end_avatars(network);
                self.update_own_avatar(network, OwnAvatar::capability_lost);
            }
            // Our own avatar: shown on the IRCv3 settings tab only.
            Event::MetadataReady => self.update_own_avatar(network, OwnAvatar::set_ready),
            Event::OwnAvatar { url, request } => {
                self.update_own_avatar(network, |own| own.reported(url, request));
            }
            Event::OwnAvatarFailed { request, failure } => {
                let failure = own_avatar_failure(failure);
                self.update_own_avatar(network, |own| own.failed(request, failure));
            }
        }
    }

    /// Our own message to `target`, in its private conversation (created if
    /// needed); the server log keeps it when there is no room for one.
    fn append_own_private(
        &mut self,
        network: NetworkId,
        target: &str,
        text: &str,
        notice: bool,
        meta: MessageMeta,
    ) {
        let own = self.own_nickname(network).unwrap_or("me").to_owned();
        let key = cayenchat_irc_core::text::nickname_key(target);
        match self.state.private_conversation(network, &key, target, true) {
            Some(id) => {
                self.state
                    .append_conversation_message(id, &own, text, notice, meta, false);
            }
            None => self.state.append_server_message_at(
                network,
                format!("→ {target} <{own}> {text}"),
                meta,
            ),
        }
    }

    fn send_draft(&mut self, notice: bool, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.state.selection();
        let input = self.inputs[&selection].clone();
        if input.read(cx).is_composing() {
            return;
        }
        let text = input.read(cx).text().to_owned();
        if text.trim().is_empty() {
            return;
        }
        let selected = self.state.selected_channel();
        let network = self.selected_network_id();
        let connection = network
            .and_then(|network| self.sessions.get(&network))
            .and_then(|session| session.irc.as_ref());
        let result = if let Some(connection) = connection {
            if network.and_then(|network| self.state.status(network))
                != Some(&ConnectionStatus::Registered)
            {
                Err(self.i18n.text("wait_registration"))
            } else if text.starts_with('/') {
                connection.send_command(&text, selected.map(|channel| channel.name.as_str()))
            } else if let Some(channel) = selected {
                if !self.state.is_active_channel(channel.id) {
                    Err(self.i18n.text("wait_join"))
                } else if channel.is_private() {
                    connection.send_private_message(&channel.name, &text, notice)
                } else {
                    connection.send_message(&channel.name, &text, notice)
                }
            } else {
                Err(self.i18n.text("select_channel"))
            }
        } else {
            Err(self.i18n.text("not_connected"))
        };
        self.feedback = match result {
            Ok(()) => {
                self.input_history.record(&text);
                input.update(cx, |input, cx| input.clear_after_send(cx));
                None
            }
            Err(error) => Some(error),
        };
        cx.notify();
        window.refresh();
    }

    /// Replaces the draft with an older (`older`) or newer sent draft.
    fn recall_history(&mut self, older: bool, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.state.selection();
        let input = self.inputs[&selection].clone();
        if input.read(cx).is_composing() || !input.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        // Each conversation has its own input; browsing never crosses them.
        let scope = match selection {
            Selection::Channel(id) => u64::from(id.0),
            Selection::Server(id) => (1 << 32) + u64::from(id.0),
            Selection::None => u64::MAX,
        };
        let current = input.read(cx).text().to_owned();
        let recalled = if older {
            self.input_history.previous(scope, &current)
        } else {
            self.input_history.next(scope, &current)
        };
        if let Some(text) = recalled {
            input.update(cx, |input, cx| input.set_text(&text, cx));
        }
    }

    fn history_previous(
        &mut self,
        _: &HistoryPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.recall_history(true, window, cx);
    }

    fn history_next(&mut self, _: &HistoryNext, window: &mut Window, cx: &mut Context<Self>) {
        self.recall_history(false, window, cx);
    }

    fn send_message(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(network) = self.focused_nick_prompt(window, cx) {
            self.submit_nick_prompt(network, window, cx);
        } else if self
            .member_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.input.read(cx).focus_handle(cx).is_focused(window))
        {
            self.submit_member_prompt(window, cx);
        } else {
            self.send_draft(false, window, cx);
        }
    }

    fn notice(&mut self, _: &Notice, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(network) = self.focused_nick_prompt(window, cx) {
            self.submit_nick_prompt(network, window, cx);
        } else if self
            .member_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.input.read(cx).focus_handle(cx).is_focused(window))
        {
            self.submit_member_prompt(window, cx);
        } else {
            self.send_draft(true, window, cx);
        }
    }
}

impl SettingsWindow {
    /// A labelled row of mutually exclusive choices stored in the settings.
    fn option_row<T: Copy + PartialEq + 'static, const N: usize>(
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

    fn select_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.values.language = language;
        self.i18n = Localizer::new(language);
        window.set_window_title(&self.i18n.text("settings_title"));
        for (field, key) in [
            (&self.settings.custom_host, "server_host_placeholder"),
            (&self.settings.nickname, "nickname"),
            (&self.settings.username, "username_placeholder"),
            (&self.settings.realname, "realname_placeholder"),
            (&self.settings.quit_message, "quit_message_placeholder"),
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

    fn new(
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
        for field in settings.text_fields() {
            subscriptions.push(cx.observe(field, |this, _, cx| this.schedule_autosave(cx)));
        }
        // The connection switch follows the chat window's connections. The
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
            this.window_activation_changed(window.is_window_active(), cx)
        }));
        // Typed passwords are stored once their field loses focus.
        for field in [&settings.server_password, &settings.sasl_password] {
            let handle = field.read(cx).focus_handle(cx);
            subscriptions.push(
                cx.on_focus_out(&handle, window, |this, _, _, cx| this.schedule_autosave(cx)),
            );
        }
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
            avatar_opening: false,
            connected_shown: false,
            _subscriptions: subscriptions,
        };
        this.probe_system_store(cx);
        this.refresh_upload_account(cx);
        this
    }

    /// Saves typed passwords to the credential store, forgets removed
    /// profiles' passwords, and writes the settings file.
    fn commit_settings(
        &mut self,
        mut settings: Settings,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) -> Result<Settings, String> {
        let store = secrets::store(cx);
        settings.servers.retain(|server| !server.host.is_empty());
        self.settings
            .persist_passwords(&store, &self.i18n, window, cx)?;
        if let Ok(Some(previous)) = cayenchat_storage::load() {
            forget_removed_profiles(&previous, &settings, &store);
        }
        let previous_logging = diagnostics::configuration();
        diagnostics::configure(&settings.experimental)
            .map_err(|error| self.i18n.format("debug_log_error", &[("error", &error)]))?;
        if let Err(error) = cayenchat_storage::save(&settings) {
            let _ = diagnostics::configure(&previous_logging);
            return Err(error);
        }
        Ok(settings)
    }

    /// Diagnostics preferences are independent of an incomplete new server
    /// form. Persist them alone so selecting an experimental log destination
    /// does not wait for the user to finish a connection profile.
    fn persist_experimental_settings(
        &mut self,
        experimental: &cayenchat_storage::Experimental,
    ) -> Result<(), String> {
        let previous_logging = diagnostics::configuration();
        diagnostics::configure(experimental)
            .map_err(|error| self.i18n.format("debug_log_error", &[("error", &error)]))?;
        let mut saved = cayenchat_storage::load()?.unwrap_or_default();
        saved.experimental = experimental.clone();
        if let Err(error) = cayenchat_storage::save(&saved) {
            let _ = diagnostics::configure(&previous_logging);
            return Err(error);
        }
        self.saved.experimental = experimental.clone();
        Ok(())
    }

    fn connect_from_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| {
            let settings = self.settings.snapshot(cx)?;
            let config =
                self.settings
                    .connection_config(&settings, &secrets::store(cx), &self.i18n, cx)?;
            self.autosave = None;
            self.saved = settings.clone();
            let settings = self.commit_settings(settings, None, cx)?;
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
    fn status_message(&self) -> Option<String> {
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

    /// Writes the form when it differs from what was last saved and applies
    /// the changes to the chat window. With `window`, a password still being
    /// typed waits; without it (closing, quitting) every typed password is
    /// stored. A server without a host waits; invalid values are reported.
    fn autosave_now(&mut self, window: Option<&Window>, cx: &mut Context<Self>) {
        self.autosave = None;
        let settings = match self.settings.snapshot(cx) {
            Ok(settings) => settings,
            Err(error) => {
                if self.autosave_error.as_ref() != Some(&error) {
                    self.autosave_error = Some(error);
                    cx.notify();
                }
                return;
            }
        };
        let passwords = self.settings.pending_passwords(window, cx);
        let waiting = settings
            .selected_profile()
            .is_some_and(|profile| profile.host.is_empty());
        if waiting {
            if settings.experimental != self.saved.experimental {
                match self.persist_experimental_settings(&settings.experimental) {
                    Ok(()) => self.autosave_error = None,
                    Err(error) => {
                        self.autosave_error = Some(error);
                        cx.notify();
                        return;
                    }
                }
            }
            // Saving now would drop the server; wait until it has a host.
            let error = Some(self.i18n.text("server_required"));
            if self.autosave_error != error {
                self.autosave_error = error;
                cx.notify();
            }
            return;
        }
        if settings == self.saved && !passwords {
            if self.autosave_error.take().is_some() {
                cx.notify();
            }
            return;
        }
        let previous = std::mem::replace(&mut self.saved, settings.clone());
        let saved = match self.commit_settings(settings, window, cx) {
            Ok(saved) => saved,
            Err(error) => {
                // Try again on the next edit.
                self.saved = previous;
                self.autosave_error = Some(error);
                cx.notify();
                return;
            }
        };
        if self.autosave_error.take().is_some() {
            cx.notify();
        }
        let servers_changed = previous.servers != self.saved.servers
            || previous.selected_server != self.saved.selected_server;
        let appearance_changed =
            previous.appearance != saved.appearance || previous.theme != saved.theme;
        let language_changed = previous.language != saved.language;
        let layout_changed = previous.restore_window_layout != saved.restore_window_layout;
        let restore_layout = saved.restore_window_layout;
        let shortcuts = ShortcutPrefs::from(&saved);
        let shortcuts_changed = ShortcutPrefs::from(&previous) != shortcuts;
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
                // Turning it on remembers where the window is right away.
                owner.note_window_bounds(window);
                owner.save_layout_now();
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

    fn toggle_remember_passwords(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = secrets::store(cx);
        let Some(profile) = self.settings.values.selected_profile().cloned() else {
            return;
        };
        if profile.remember_passwords {
            let result = store
                .delete(&profile.server_password_key())
                .and_then(|()| store.delete(&profile.sasl_password_key()))
                .map_err(|error| secrets::error_text(&self.i18n, &error))
                .and_then(|()| cayenchat_storage::clear_saved_passwords(&profile.id));
            match result {
                Ok(()) => {
                    if let Some(profile) = self.settings.values.selected_profile_mut() {
                        profile.remember_passwords = false;
                    }
                    self.show_selected_server(cx);
                    self.feedback = Some(self.i18n.text("passwords_removed"));
                }
                Err(error) => self.feedback = Some(error),
            }
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

    fn show_selected_server(&mut self, cx: &mut Context<Self>) {
        self.settings
            .show_selected(&secrets::store(cx), &self.i18n, cx);
        self.feedback = None;
        cx.notify();
    }

    fn select_server(&mut self, id: String, cx: &mut Context<Self>) {
        self.switch_server(move |settings| settings.selected_server = id, cx);
    }

    /// Adds a server, blank or filled in from a preset's `host`.
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
        let store = secrets::store(cx);
        match self.settings.switch_server(change, &store, &self.i18n, cx) {
            Ok(()) => self.feedback = None,
            Err(error) => self.feedback = Some(error),
        }
        cx.notify();
    }

    /// Removes the selected server. Removal is saved at once, disconnecting
    /// it and forgetting its passwords, so a saved server asks first.
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
        let selected_id = profile.as_ref().map(|profile| profile.id.clone());
        let server_fields = profile
            .as_ref()
            .map(|profile| self.render_server_fields(profile, cx));
        let mut language_selector = div().flex().flex_col().child(
            div()
                .id("language-select")
                .px_2()
                .py_1()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(format!(
                    "{}  ▾",
                    self.i18n.preference_label(self.settings.values.language)
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings.language_list_open = !this.settings.language_list_open;
                    this.settings.server_list_open = false;
                    this.settings.encoding_list_open = false;
                    cx.notify();
                })),
        );
        if self.settings.language_list_open {
            let mut menu = div()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface);
            for (index, language) in [Language::System, Language::Japanese, Language::English]
                .into_iter()
                .enumerate()
            {
                menu = menu.child(
                    div()
                        .id(("language-option", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .when(self.settings.values.language == language, |d| {
                            d.bg(theme.selected)
                        })
                        .child(self.i18n.preference_label(language))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.settings.language_list_open = false;
                            this.select_language(language, window, cx)
                        })),
                );
            }
            language_selector = language_selector.child(menu);
        }
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
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface);
            for (index, server) in self.settings.values.ordered_servers().enumerate() {
                let id = server.id.clone();
                let (host, port) = if Some(&id) == selected_id.as_ref() {
                    (current_host.as_str(), current_port.as_str())
                } else {
                    (server.host.as_str(), "")
                };
                let label = if host.is_empty() {
                    self.i18n.text("new_server")
                } else {
                    format!(
                        "{}:{}",
                        host,
                        if port.is_empty() {
                            server.port.to_string()
                        } else {
                            port.to_owned()
                        },
                    )
                };
                menu = menu.child(
                    div()
                        .id(("server-option", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .when(Some(&id) == selected_id.as_ref(), |d| d.bg(theme.selected))
                        .child(label)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select_server(id.clone(), cx)),
                        ),
                );
            }
            // Well-known servers are only suggestions for adding a server.
            for (index, preset) in cayenchat_storage::PRESETS.iter().enumerate() {
                menu = menu.child(
                    div()
                        .id(("add-preset-option", index))
                        .px_2()
                        .py_1()
                        .when(index == 0, |d| d.border_t_1().border_color(theme.border))
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .child(self.i18n.format(
                            "add_preset_server",
                            &[("name", preset.name), ("host", preset.host)],
                        ))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.add_server(preset.host, cx)),
                        ),
                );
            }
            menu = menu.child(
                div()
                    .id("add-server-option")
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.hover))
                    .child(self.i18n.text("add_server"))
                    .on_click(cx.listener(|this, _, _, cx| this.add_server("", cx))),
            );
            server_selector = server_selector.child(menu);
        }
        account_settings::panel(cx)
            .child(self.tab_heading("connection"))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("connection_intro")),
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
                            .child(self.i18n.text("language")),
                    )
                    .child(language_selector),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("language_hint")),
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
                    .when(!no_server, |d| {
                        // On while the server being edited is connected, being
                        // connected or waiting to retry.
                        d.child(
                            div()
                                .id("connection-switch")
                                .debug_selector(|| "connection-switch".into())
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .child(self.i18n.text("connect"))
                                .child(settings_theme::switch(connected, cx))
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
                    .child(self.i18n.text("remove_server"))
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
            .child(settings_field(
                &self.i18n.text("auto_join_channels"),
                self.settings.channels.clone(),
            ))
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
    fn toggle_color_picker(&mut self, field: &Entity<TextInput>, cx: &mut Context<Self>) {
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
    fn apply_palette_color(&mut self, color: &str, cx: &mut Context<Self>) {
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
            .child(
                div()
                    .id("image-previews")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.appearance.image_previews,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("image_previews"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.image_previews;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("image_previews_hint")),
            )
            .child(
                div()
                    .id("user-avatars")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.appearance.user_avatars,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("user_avatars"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.user_avatars;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("user_avatars_hint")),
            )
            .child(
                div()
                    .id("wrap-long-nicknames")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.appearance.wrap_long_nicknames,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("wrap_long_nicknames"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.wrap_long_nicknames;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("restore-window-layout")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.restore_window_layout,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("restore_window_layout"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.restore_window_layout;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(self.font_field(FontTarget::MainLog, &self.i18n.text("channel_log"), cx))
            .child(self.font_field(FontTarget::SubLog, &self.i18n.text("combined_log"), cx))
            .child(settings_field(
                &self.i18n.text("combined_log_name_width"),
                self.settings.sub_log_name_width.clone(),
            ))
            .child(self.font_field(FontTarget::Members, &self.i18n.text("member_list"), cx))
            .child(self.font_field(FontTarget::Channels, &self.i18n.text("channel_list"), cx))
            .child(self.font_field(FontTarget::Input, &self.i18n.text("draft_input"), cx))
            .child(self.font_field(FontTarget::Time, &self.i18n.text("timestamp_monospace"), cx))
            .when(cfg!(target_os = "linux"), |d| {
                d.child(self.option_row(
                    "linux_display",
                    [
                        (LinuxDisplay::Wayland, "linux_display_wayland"),
                        (LinuxDisplay::X11, "linux_display_x11"),
                    ],
                    self.settings.values.linux_display,
                    |settings, display| settings.linux_display = display,
                    cx,
                ))
                .child(
                    div()
                        .ml(px(158.))
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text("linux_display_hint")),
                )
            })
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
            .child(
                div()
                    .id("menu-bar-auto-hide")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.values.menu_bar_auto_hide,
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("menu_bar_auto_hide"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let hide = !this.settings.values.menu_bar_auto_hide;
                        this.settings.values.menu_bar_auto_hide = hide;
                        cx.notify();
                    })),
            )
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
        self.show_tab(tab);
        self.font_picker = None;
        window.focus(&self.nav_focus);
        cx.notify();
    }

    /// The categories in list order; the last, Experimental, stands apart.
    fn settings_tabs() -> Vec<(SettingsTab, &'static str, &'static str)> {
        let mut tabs = vec![
            (SettingsTab::Connection, "connection-tab", "connection"),
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
    fn show_tab(&mut self, tab: SettingsTab) {
        if self.tab != tab {
            self.feedback = None;
            self.avatar_feedback = None;
            // A key being recorded does not wait on another tab.
            self.shortcut_recording = None;
            // A picker open under a color row does not wait for a return.
            self.color_picker = None;
        }
        self.tab = tab;
    }

    fn render_settings(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = settings_theme::palette(cx);
        let nav_focused = self.nav_focus.is_focused(window);
        let mut tabs = Self::settings_tabs();
        let experimental = tabs.pop();
        let mut nav = div()
            .id("settings-nav")
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
            SettingsTab::Appearance => self.render_appearance_settings(cx).into_any_element(),
            SettingsTab::Keyboard => self.render_keyboard_settings(cx).into_any_element(),
            SettingsTab::Shortcuts => self.render_shortcut_settings(cx).into_any_element(),
            SettingsTab::Notifications => self.render_notification_settings(cx).into_any_element(),
            SettingsTab::Ircv3 => self.render_ircv3_settings(cx).into_any_element(),
            SettingsTab::ImageUpload => self.render_image_upload_settings(cx).into_any_element(),
            SettingsTab::Credentials => self.render_credential_settings(cx).into_any_element(),
            SettingsTab::Experimental => self.render_experimental_settings(cx).into_any_element(),
        };
        field_traversal(div().id("settings-screen"))
            .key_context("SettingsWindow")
            .size_full()
            .flex()
            .bg(theme.window)
            .text_size(px(13.))
            .when_some(settings_theme::current(cx), |d, native| {
                d.font_family(native.defaults.font.family.clone())
                    .text_size(px(native.defaults.font.size))
            })
            .text_color(theme.text)
            .child(nav)
            .child(
                div()
                    .id("settings-pane")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(div().w_full().max_w(px(720.)).p_4().child(panel)),
            )
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
    }
}

fn notification_rules(settings: &Notifications) -> NotificationRules {
    NotificationRules {
        enabled: settings.enabled,
        mentions: settings.mentions,
        keyword_alerts: settings.keyword_alerts,
        keywords: settings.keywords.clone(),
        private_messages: settings.private_messages,
    }
}

fn settings_field(label: &str, input: Entity<TextInput>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(px(150.)).flex_shrink_0().child(label.to_owned()))
        .child(div().flex_1().min_w_0().child(input))
}

fn default_time_font() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(target_os = "windows")]
    {
        "Consolas"
    }
    #[cfg(target_os = "linux")]
    {
        "DejaVu Sans Mono"
    }
}

fn log_urls(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let next = ["https://", "http://"]
            .into_iter()
            .filter_map(|scheme| text[cursor..].find(scheme).map(|offset| cursor + offset))
            .min();
        let Some(start) = next else { break };
        let mut end = text[start..]
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace() || "<>\"'。、".contains(*ch))
            .map(|(offset, _)| start + offset)
            .unwrap_or(text.len());
        while end > start
            && text[..end]
                .chars()
                .last()
                .is_some_and(|ch| ".,;:!?)]}」』".contains(ch))
        {
            end -= text[..end].chars().last().unwrap().len_utf8();
        }
        let candidate = &text[start..end];
        if let Ok(url) = url::Url::parse(candidate)
            && matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
        {
            found.push((start..end, url.into()));
        }
        cursor = end.max(start + 1);
    }
    found
}

fn styled_log_text(
    text: &str,
    urls: &[(std::ops::Range<usize>, String)],
    highlights: &[std::ops::Range<usize>],
    selected: Option<std::ops::Range<usize>>,
    theme: &Theme,
) -> StyledText {
    let mut boundaries = vec![0, text.len()];
    for range in urls.iter().map(|(range, _)| range).chain(highlights) {
        boundaries.extend([range.start, range.end]);
    }
    if let Some(range) = &selected {
        boundaries.extend([range.start, range.end]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let highlights = boundaries.windows(2).filter_map(|pair| {
        let range = pair[0]..pair[1];
        let is_url = urls
            .iter()
            .any(|(url, _)| url.start <= range.start && range.end <= url.end);
        let is_selected = selected
            .as_ref()
            .is_some_and(|selection| selection.start <= range.start && range.end <= selection.end);
        let is_highlight = highlights
            .iter()
            .any(|word| word.start <= range.start && range.end <= word.end);
        (is_url || is_selected || is_highlight).then_some((
            range,
            HighlightStyle {
                color: if is_url {
                    Some(theme.link.into())
                } else {
                    is_highlight.then_some(theme.panes.highlight.into())
                },
                font_weight: is_highlight.then_some(FontWeight::BOLD),
                underline: is_url.then_some(UnderlineStyle {
                    color: Some(theme.link.into()),
                    thickness: px(1.),
                    wavy: false,
                }),
                background_color: is_selected.then_some(theme.selected.into()),
                ..Default::default()
            },
        ))
    });
    StyledText::new(text.to_owned()).with_highlights(highlights)
}

/// A preview below a message: the thumbnail, or a box of the full height
/// while it loads. A double-click opens the link, like on the text.
fn preview_element(
    link: String,
    shown: previews::Shown,
    index: usize,
    theme: &Theme,
    cx: &mut Context<ChatWindow>,
) -> AnyElement {
    let frame = div()
        .id(("message-preview", index))
        .mt(px(2.))
        .mb(px(1.))
        .flex_shrink_0();
    match shown {
        previews::Shown::Image(preview) => frame
            .w(px(preview.width))
            .h(px(preview.height))
            .cursor_pointer()
            .child(img(preview.image).size_full())
            .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    cx.open_url(&link);
                }
            }))
            .into_any_element(),
        previews::Shown::Pending => frame
            .w(px(previews::BOX_HEIGHT as f32 * 4. / 3.))
            .h(px(previews::BOX_HEIGHT as f32))
            .border_1()
            .border_color(theme.border)
            .into_any_element(),
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.i18n.text("settings_title");
        let content = self.render_settings(window, cx).into_any_element();
        decorations::window_frame(window, cx, title, content)
    }
}

impl Render for ChatWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The topic can change with any server event, not only on selection.
        let title = self.window_title();
        if title != self.shown_title {
            window.set_window_title(&title);
            self.shown_title = title.clone();
        }
        let content = self.render_chat(window, cx);
        let content = menu_bar::wrap(
            &self.menu_bar,
            cx.get_menus().unwrap_or_default(),
            content,
            |this| &mut this.menu_bar,
            window,
            cx,
        );
        decorations::window_frame(window, cx, title, content)
    }
}

impl ChatWindow {
    fn render_channel_tree(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::current(cx);
        // The tree is virtualized like the logs: it redraws on every state
        // change, but builds only the rows near the viewport. Keys ascend in
        // display order (server position, then conversation id), so adding a
        // channel keeps the other rows' measured heights and the scroll.
        self.tree_rows.clear();
        let mut keys = Vec::new();
        for (index, network) in self.state.networks().iter().enumerate() {
            let base = (index as u64) << 32;
            self.tree_rows.push(TreeRow::Server(network.id));
            keys.push(base);
            for conversation in self
                .state
                .conversations()
                .iter()
                .filter(|c| c.network == network.id)
            {
                self.tree_rows.push(TreeRow::Channel(conversation.id));
                // Private conversations follow the network's channels; the
                // bit keeps keys ascending in that order.
                let private = if conversation.is_private() {
                    1 << 31
                } else {
                    0
                };
                keys.push(base | private | (u64::from(conversation.id.0) + 1));
            }
        }
        self.tree_list.sync(0, &keys);
        let appearance = &self.appearance;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.channel_tree)
            .when(!appearance.channel_font.is_empty(), |d| {
                d.font_family(appearance.channel_font.clone())
            })
            .border_t_1()
            .border_color(theme.border)
            .child(
                list(
                    self.tree_list.state.clone(),
                    cx.processor(Self::render_tree_row),
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn render_tree_row(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme::current(cx);
        let selection = self.state.selection();
        match self.tree_rows.get(row).copied() {
            Some(TreeRow::Server(server_id)) => {
                let Some(network) = self
                    .state
                    .networks()
                    .iter()
                    .find(|network| network.id == server_id)
                else {
                    return div().into_any_element();
                };
                let used = self
                    .sessions
                    .get(&server_id)
                    .is_some_and(ServerSession::used);
                let status_mark = match self.state.status(server_id) {
                    // Servers not connected in this run show no mark.
                    _ if !used => "",
                    Some(ConnectionStatus::Registered) => " ●",
                    Some(ConnectionStatus::Connecting | ConnectionStatus::TransportConnected) => {
                        " …"
                    }
                    Some(ConnectionStatus::Disconnected(_)) => " ×",
                    _ => "",
                };
                div()
                    .id(("server", server_id.0))
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .when(selection == Selection::Server(server_id), |d| {
                        d.bg(theme.selected)
                    })
                    .hover(|d| d.bg(theme.hover_strong))
                    .child(format!("{}{}", network.name, status_mark))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let viewport = window.viewport_size();
                            this.server_menu = Some(ServerMenu {
                                position: point(
                                    event
                                        .position
                                        .x
                                        .min((viewport.width - px(176.)).max(px(0.))),
                                    event
                                        .position
                                        .y
                                        .min((viewport.height - px(76.)).max(px(0.))),
                                ),
                                network: server_id,
                            });
                            this.member_menu = None;
                            this.channel_menu = None;
                            this.member_prompt = None;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dispatch(Command::SelectServer(server_id), window, cx);
                    }))
                    .into_any_element()
            }
            Some(TreeRow::Channel(id)) => {
                let Some(conversation) = self.state.conversations().iter().find(|c| c.id == id)
                else {
                    return div().into_any_element();
                };
                let unread = self.state.is_unread(id);
                let highlighted = self.state.is_highlighted(id);
                let name = conversation.name.clone();
                let network = conversation.network;
                let private = conversation.is_private();
                let joined = self.state.is_active_channel(id);
                div()
                    .id(("channel", id.0))
                    .pl_4()
                    .pr_2()
                    .py(px(2.))
                    .cursor_pointer()
                    .when(selection == Selection::Channel(id), |d| {
                        d.bg(theme.selected)
                    })
                    .when(unread, |d| d.font_weight(FontWeight::BOLD))
                    .when(!joined, |d| d.text_color(theme.text_muted))
                    .when(highlighted, |d| d.text_color(theme.panes.highlight))
                    .hover(|d| d.bg(theme.hover_strong))
                    .child(format!(
                        "{}{}",
                        if unread { "● " } else { "" },
                        conversation.name
                    ))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let viewport = window.viewport_size();
                            this.channel_menu = Some(ChannelMenu {
                                position: point(
                                    event
                                        .position
                                        .x
                                        .min((viewport.width - px(176.)).max(px(0.))),
                                    event
                                        .position
                                        .y
                                        .min((viewport.height - px(76.)).max(px(0.))),
                                ),
                                network,
                                conversation: id,
                                channel: name.clone(),
                                joined,
                                private,
                            });
                            this.server_menu = None;
                            this.member_menu = None;
                            this.member_prompt = None;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dispatch(Command::SelectChannel(id), window, cx);
                    }))
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    /// Content of a cached pane; see [`ChatPane`].
    fn render_pane(&mut self, kind: PaneKind, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        {
            self.pane_renders += 1;
        }
        match kind {
            PaneKind::MainLog => {
                let selection = self.state.selection();
                let main = self
                    .main_lists
                    .get_mut(&selection)
                    .expect("synced before drawing");
                if let Selection::Channel(id) = selection {
                    let chat = cx.weak_entity();
                    main.on_scroll(move |first_visible, cx| {
                        let chat = chat.clone();
                        cx.defer(move |cx| {
                            let _ = chat.update(cx, |chat, _| {
                                chat.scrolled_main_log(id, first_visible);
                            });
                        });
                    });
                }
                list(main.state.clone(), cx.processor(Self::render_main_row))
                    .size_full()
                    .into_any_element()
            }
            PaneKind::SubLog => list(
                self.sub_list.state.clone(),
                cx.processor(Self::render_sub_row),
            )
            .size_full()
            .into_any_element(),
            PaneKind::Members => {
                let member_count = self
                    .state
                    .selected_channel()
                    .map_or(0, |channel| channel.members.len());
                uniform_list(
                    "members",
                    member_count,
                    cx.processor(Self::render_member_rows),
                )
                .size_full()
                .into_any_element()
            }
            PaneKind::Channels => self.render_channel_tree(cx),
        }
    }

    fn render_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::current(cx);
        self.sync_log_lists();
        let selection = self.state.selection();
        let mut origin = decorations::content_origin(window);
        origin.y += self.menu_bar.height();
        let log_id = match selection {
            Selection::Channel(id) => id.0,
            Selection::Server(id) => u32::MAX - id.0,
            Selection::None => u32::MAX,
        };
        let border = theme.border;
        let appearance = &self.appearance;
        let main_bg = theme.panes.main_log;
        let sub_bg = theme.panes.sub_log;

        // Panes that do not change while the draft is edited are separate cached
        // views: typing redraws this window, but they reuse their last layout
        // and paint until the chat state they show is notified.
        let panes = self.panes.get_or_insert_with(|| ChatPanes::new(cx)).clone();
        let main_log = div()
            .id(("log", log_id))
            .key_context("MainLog")
            .track_focus(&self.log_focus)
            .on_action(cx.listener(Self::copy_log_selection))
            .on_action(cx.listener(Self::copy_log_selection_menu))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.finish_log_selection()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.finish_log_selection()),
            )
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .px_2()
            .py_1()
            .bg(main_bg)
            .when(!appearance.main_log_font.is_empty(), |d| {
                d.font_family(appearance.main_log_font.clone())
            })
            .child(panes.main_log.clone().cached(pane_style()));

        let sub_log = div()
            .id("sub-log")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .px_2()
            .py_1()
            .bg(sub_bg)
            .when(!appearance.sub_log_font.is_empty(), |d| {
                d.font_family(appearance.sub_log_font.clone())
            })
            .child(panes.sub_log.clone().cached(pane_style()));

        let members = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(theme.panes.member_list)
            .when(!appearance.member_font.is_empty(), |d| {
                d.font_family(appearance.member_font.clone())
            })
            .child(panes.members.clone().cached(pane_style()));

        let mut main_pane = div().flex().flex_col().flex_1().min_h_0().child(main_log);
        main_pane.style().flex_grow = Some(self.log_split * 2.);
        // The bottom edge of the draft row is the handle between the logs;
        // a double click restores the even split.
        let split_handle = div()
            .id("log-split-handle")
            .absolute()
            .top(px(-3.))
            .left_0()
            .right_0()
            .h(px(6.))
            .cursor(CursorStyle::ResizeUpDown)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if event.click_count >= 2 {
                        this.log_split = DEFAULT_LOG_SPLIT;
                        this.schedule_layout_save(cx);
                    } else {
                        this.log_split_dragging = true;
                    }
                    cx.notify();
                }),
            );
        let mut sub_pane = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(border)
            .child(sub_log)
            .child(split_handle);
        sub_pane.style().flex_grow = Some((1. - self.log_split) * 2.);
        let editor = div()
            .flex()
            .items_center()
            .h(px(DRAFT_ROW_HEIGHT))
            .flex_shrink_0()
            .px_1()
            .border_t_1()
            .border_b_1()
            .border_color(border)
            .bg(theme.surface)
            .when(!appearance.input_font.is_empty(), |d| {
                d.font_family(appearance.input_font.clone())
            })
            .id("draft-row")
            // Dropped image files join the same attachment flow as pasted ones.
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(theme.selected))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_paths(paths.paths(), window, cx)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.inputs[&selection].clone()),
            )
            .when_some(self.upload_status(), |d, status| {
                d.child(
                    div()
                        .flex()
                        .gap_2()
                        .px_1()
                        .text_color(theme.text_secondary)
                        .child(status)
                        .child(
                            div()
                                .id("cancel-upload")
                                .px_1()
                                .border_1()
                                .border_color(border)
                                .cursor_pointer()
                                .child(self.i18n.text("upload_cancel"))
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_upload(cx))),
                        ),
                )
            })
            .when_some(self.feedback.clone(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            });
        let column_bounds = self.left_column_bounds.clone();
        let left = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                canvas(
                    move |bounds, _, _| column_bounds.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.log_split_dragging {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_log_split(event.position.y, cx);
                    } else {
                        this.log_split_dragging = false;
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.log_split_dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.log_split_dragging = false),
            )
            .child(main_pane)
            .child(editor)
            .child(sub_pane);
        // The right column keeps its width inside what the window can spare,
        // and the member list its height inside the column.
        let right_max =
            (f32::from(window.viewport_size().width) - LEFT_MIN_WIDTH).max(RIGHT_WIDTH_MIN);
        let right_width = self.right_width.clamp(RIGHT_WIDTH_MIN, right_max);
        let column_height = self
            .right_column_bounds
            .get()
            .map(|bounds| f32::from(bounds.size.height));
        let members_max = column_height.map_or(RIGHT_PANE_MIN_HEIGHT, |height| {
            (height - RIGHT_PANE_MIN_HEIGHT).max(RIGHT_PANE_MIN_HEIGHT)
        });
        let members_height = self
            .members_height
            .map(|height| height.clamp(RIGHT_PANE_MIN_HEIGHT, members_max));
        // Before the first drag the list takes half the column.
        let members_size = members_height
            .or(column_height.map(|height| height / 2.))
            .unwrap_or(RIGHT_PANE_MIN_HEIGHT);
        let right_split = splitter::splitter(
            "right-split",
            splitter::Axis::Horizontal,
            splitter::Anchor::After,
            right_width,
            RIGHT_WIDTH_MIN..=right_max,
            border,
            cx,
            |this: &mut Self, width, _, cx| {
                this.right_width = width;
                this.schedule_layout_save(cx);
            },
        );
        let members_split = splitter::splitter(
            "members-split",
            splitter::Axis::Vertical,
            splitter::Anchor::Before,
            members_size,
            RIGHT_PANE_MIN_HEIGHT..=members_max,
            border,
            cx,
            |this: &mut Self, height, _, cx| {
                this.members_height = Some(height);
                this.schedule_layout_save(cx);
            },
        );
        let right_column_bounds = self.right_column_bounds.clone();
        let right = div()
            .relative()
            .flex()
            .flex_col()
            .w(px(right_width))
            .flex_shrink_0()
            .h_full()
            .child(
                canvas(
                    move |bounds, _, _| right_column_bounds.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // `flex_1` gave the list a zero flex basis, which would win over
            // a plain height.
            .child(members.when_some(members_height, |d, height| {
                d.flex_none().flex_basis(px(height)).h(px(height))
            }))
            .child(members_split)
            .child(panes.channels.clone().cached(pane_style().w_full()));

        let server_menu = self.server_menu.as_ref().map(|menu| {
            let network = menu.network;
            let position = menu.position;
            let session = self.sessions.get(&network);
            let connected = session.is_some_and(|session| session.irc.is_some());
            let can_disconnect = self.can_disconnect(network);
            let registered = self.registered_connection(network).is_ok();
            // A server not connected in this run offers Connect, not Reconnect.
            let connect_key = if session.is_some_and(ServerSession::used) {
                "reconnect"
            } else {
                "connect"
            };
            div()
                .id("server-context-menu")
                .absolute()
                .left(position.x - origin.x)
                .top(position.y - origin.y)
                .w(px(176.))
                .p_1()
                .bg(theme.surface)
                .border_1()
                .border_color(border)
                .shadow_md()
                .child(
                    div()
                        .id("server-menu-reconnect")
                        .px_2()
                        .py_1()
                        .child(self.i18n.text(connect_key))
                        .when(!connected, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(theme.hover_strong))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.reconnect(network, window, cx);
                                }))
                        })
                        .when(connected, |d| d.text_color(theme.text_muted)),
                )
                .child(
                    div()
                        .id("server-menu-disconnect")
                        .px_2()
                        .py_1()
                        .child(self.i18n.text("disconnect"))
                        .when(can_disconnect, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(theme.hover_strong))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.disconnect(network, cx)),
                                )
                        })
                        .when(!can_disconnect, |d| d.text_color(theme.text_muted)),
                )
                .child(div().my_1().border_t_1().border_color(theme.separator))
                .children(
                    [
                        (MemberPromptKind::Join, "server_menu_join"),
                        (MemberPromptKind::Nick, "server_menu_nick"),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (kind, key))| {
                        div()
                            .id(("server-menu-prompt", index))
                            .px_2()
                            .py_1()
                            .child(self.i18n.text(key))
                            .when(registered, |d| {
                                d.cursor_pointer()
                                    .hover(|d| d.bg(theme.hover_strong))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_server_prompt(kind, window, cx)
                                    }))
                            })
                            .when(!registered, |d| d.text_color(theme.text_muted))
                    }),
                )
        });
        let channel_menu = self.channel_menu.as_ref().map(|menu| {
            let registered = self.registered_connection(menu.network).is_ok();
            let mut popup = div()
                .id("channel-context-menu")
                .absolute()
                .left(menu.position.x - origin.x)
                .top(menu.position.y - origin.y)
                .w(px(176.))
                .p_1()
                .bg(theme.surface)
                .border_1()
                .border_color(border)
                .shadow_md();
            if menu.private {
                return popup.child(
                    div()
                        .id("channel-menu-close")
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover_strong))
                        .child(self.i18n.text("conversation_close"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.close_private_conversation(cx)),
                        ),
                );
            }
            for (join, key) in [(true, "channel_join"), (false, "channel_part")] {
                let enabled = registered && menu.joined != join;
                popup = popup.child(
                    div()
                        .id(if join {
                            "channel-menu-join"
                        } else {
                            "channel-menu-part"
                        })
                        .px_2()
                        .py_1()
                        .child(self.i18n.text(key))
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(theme.hover_strong))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.channel_menu_command(join, cx)
                                }))
                        })
                        .when(!enabled, |d| d.text_color(theme.text_muted)),
                );
            }
            popup
        });
        let member_menu = self.member_menu.as_ref().map(|menu| {
            let mut popup = div()
                .id("member-context-menu")
                .absolute()
                .left(menu.position.x - origin.x)
                .top(menu.position.y - origin.y)
                .w(px(210.))
                .p_1()
                .bg(theme.surface)
                .border_1()
                .border_color(border)
                .shadow_md();
            let enabled = self.registered_connection(menu.network).is_ok();
            if !menu.group.is_empty() {
                use cayenchat_irc_core::MemberMode;
                popup = popup
                    .child(div().px_2().py_1().text_color(theme.text_secondary).child(
                        self.i18n.format(
                            "member_group_title",
                            &[("count", &menu.group.len().to_string())],
                        ),
                    ))
                    .child(div().my_1().border_t_1().border_color(theme.separator));
                for (index, (mode, key)) in [
                    (MemberMode::Op, "member_give_op"),
                    (MemberMode::Deop, "member_deop"),
                    (MemberMode::Voice, "member_give_voice"),
                    (MemberMode::Devoice, "member_devoice"),
                ]
                .into_iter()
                .enumerate()
                {
                    popup = popup.child(
                        div()
                            .id(("member-group-action", index))
                            .px_2()
                            .py_1()
                            .child(self.i18n.text(key))
                            .when(enabled, |d| {
                                d.cursor_pointer()
                                    .hover(|d| d.bg(theme.hover_strong))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.member_modes(mode, cx)
                                    }))
                            })
                            .when(!enabled, |d| d.text_color(theme.text_muted)),
                    );
                }
                return popup;
            }
            for (index, (choice, key)) in [
                (MemberMenuChoice::Whois, "member_whois"),
                (MemberMenuChoice::PrivateMessage, "member_private_message"),
                (MemberMenuChoice::Invite, "member_invite"),
                (MemberMenuChoice::GiveOp, "member_give_op"),
                (MemberMenuChoice::Deop, "member_deop"),
            ]
            .into_iter()
            .enumerate()
            {
                if index == 3 {
                    popup = popup.child(div().my_1().border_t_1().border_color(theme.separator));
                }
                let channel = menu.channel.clone();
                popup = popup.child(
                    div()
                        .id(("member-menu-action", index))
                        .px_2()
                        .py_1()
                        .child(self.i18n.text(key))
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(theme.hover_strong))
                                .on_click(cx.listener(move |this, _, window, cx| match choice {
                                    MemberMenuChoice::Whois => {
                                        this.member_command(MemberCommand::Whois, cx)
                                    }
                                    MemberMenuChoice::PrivateMessage => this.open_member_prompt(
                                        MemberPromptKind::PrivateMessage,
                                        window,
                                        cx,
                                    ),
                                    MemberMenuChoice::Invite => this.open_member_prompt(
                                        MemberPromptKind::Invite,
                                        window,
                                        cx,
                                    ),
                                    MemberMenuChoice::GiveOp => this.member_command(
                                        MemberCommand::GiveOp {
                                            channel: channel.clone(),
                                        },
                                        cx,
                                    ),
                                    MemberMenuChoice::Deop => this.member_command(
                                        MemberCommand::Deop {
                                            channel: channel.clone(),
                                        },
                                        cx,
                                    ),
                                }))
                        })
                        .when(!enabled, |d| d.text_color(theme.text_muted)),
                );
            }
            popup
        });
        if let Some(prompt) = self.member_prompt.as_mut()
            && prompt.focus_pending
        {
            prompt.focus_pending = false;
            window.focus(&prompt.input.focus_handle(cx));
        }
        let viewport = window.viewport_size();
        let member_prompt = self.member_prompt.as_ref().map(|prompt| {
            let title = self.i18n.format(
                match prompt.kind {
                    MemberPromptKind::PrivateMessage => "member_message_title",
                    MemberPromptKind::Invite => "member_invite_title",
                    MemberPromptKind::Join => "channel_join_title",
                    MemberPromptKind::Nick => "nickname_change_title",
                },
                &[("nickname", &prompt.nickname)],
            );
            let position = prompt.position.unwrap_or_else(|| {
                point(
                    ((viewport.width - px(300.)) / 2.).max(px(0.)),
                    ((viewport.height - px(140.)) / 2.).max(px(0.)),
                )
            });
            let submit = self.i18n.text(match prompt.kind {
                MemberPromptKind::Join => "channel_join",
                MemberPromptKind::Nick => "nickname_change_submit",
                MemberPromptKind::PrivateMessage | MemberPromptKind::Invite => "member_submit",
            });
            div()
                .id("member-prompt")
                .absolute()
                .left(position.x - origin.x)
                .top(position.y - origin.y)
                .w(px(300.))
                .p_2()
                .bg(theme.surface)
                .border_1()
                .border_color(border)
                .shadow_md()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().font_weight(FontWeight::BOLD).child(title))
                .child(prompt.input.clone())
                .when_some(self.feedback.clone(), |d, feedback| {
                    d.child(div().text_color(theme.warning).child(feedback))
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("member-prompt-submit")
                                .px_2()
                                .py_1()
                                .bg(theme.selected)
                                .cursor_pointer()
                                .child(submit)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit_member_prompt(window, cx)
                                })),
                        )
                        .child(
                            div()
                                .id("member-prompt-cancel")
                                .px_2()
                                .py_1()
                                .border_1()
                                .border_color(border)
                                .cursor_pointer()
                                .child(self.i18n.text("cancel"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cancel_member_prompt(window, cx)
                                })),
                        ),
                )
        });
        let nick_prompts = self.render_nick_prompts(origin, window, cx);
        div()
            .id("chat-window")
            .key_context("ChatWindow")
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                if this.dismiss_menus() {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    if this.dismiss_menus() {
                        cx.notify();
                    }
                }),
            )
            .size_full()
            .flex()
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(theme.text)
            .bg(theme.surface)
            .on_action(cx.listener(Self::navigate))
            .on_action(cx.listener(Self::complete_nickname))
            .on_action(cx.listener(Self::send_message))
            .on_action(cx.listener(Self::notice))
            .on_action(cx.listener(Self::history_previous))
            .on_action(cx.listener(Self::history_next))
            .on_action(cx.listener(Self::open_settings))
            .when(self.can_disconnect_selected(), |d| {
                d.on_action(cx.listener(Self::disconnect_action))
            })
            .on_action(cx.listener(Self::reconnect_action))
            .on_action(cx.listener(Self::toggle_debug))
            .on_action(cx.listener(Self::copy_diagnostics))
            .on_action(cx.listener(Self::paste_image))
            .child(left)
            .child(right_split)
            .child(right)
            .when_some(server_menu, |d, menu| d.child(menu))
            .when_some(member_menu, |d, menu| d.child(menu))
            .when_some(channel_menu, |d, menu| d.child(menu))
            .when_some(member_prompt, |d, prompt| d.child(prompt))
            .when_some(nick_prompts, |d, prompts| d.child(prompts))
            .into_any_element()
    }
}

impl ChatWindow {
    /// One centered dialog with a row per server waiting for a nickname.
    fn render_nick_prompts(
        &mut self,
        origin: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.nick_prompts.is_empty() {
            return None;
        }
        // Focus a newly shown row unless the user is already typing in one.
        let typing = self.focused_nick_prompt(window, cx).is_some();
        let pending = self
            .nick_prompts
            .iter()
            .position(|prompt| prompt.focus_pending);
        for prompt in &mut self.nick_prompts {
            prompt.focus_pending = false;
        }
        if let Some(index) = pending
            && !typing
        {
            window.focus(&self.nick_prompts[index].input.focus_handle(cx));
        }
        let theme = theme::current(cx);
        let border = theme.border;
        let viewport = window.viewport_size();
        let width = px(360.);
        let mut dialog = div()
            .id("nick-prompts")
            .absolute()
            .left(((viewport.width - width) / 2.).max(px(0.)) - origin.x)
            .top(px(60.).min(viewport.height / 4.) - origin.y)
            .w(width)
            .p_2()
            .bg(theme.surface)
            .border_1()
            .border_color(border)
            .shadow_md()
            .flex()
            .flex_col()
            .gap_2();
        for (index, prompt) in self.nick_prompts.iter().enumerate() {
            let network = prompt.network;
            let server = self
                .state
                .networks()
                .iter()
                .find(|server| server.id == network)
                .map(|server| server.name.clone())
                .unwrap_or_default();
            let title = self
                .i18n
                .format("nick_prompt_title", &[("nickname", &prompt.rejected)]);
            dialog = dialog
                .when(index > 0, |d| {
                    d.child(div().border_t_1().border_color(theme.separator))
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().font_weight(FontWeight::BOLD).child(server))
                        .child(title)
                        .child(prompt.input.clone())
                        .when_some(prompt.error.clone(), |d, error| {
                            d.child(div().text_color(theme.warning).child(error))
                        })
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .id(("nick-prompt-submit", network.0))
                                        .px_2()
                                        .py_1()
                                        .bg(theme.selected)
                                        .cursor_pointer()
                                        .child(self.i18n.text("nick_prompt_submit"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.submit_nick_prompt(network, window, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .id(("nick-prompt-cancel", network.0))
                                        .px_2()
                                        .py_1()
                                        .border_1()
                                        .border_color(border)
                                        .cursor_pointer()
                                        .child(self.i18n.text("nick_prompt_cancel"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.cancel_nick_prompt(network, window, cx)
                                        })),
                                ),
                        ),
                );
        }
        Some(dialog.into_any_element())
    }
}

/// A row of the channel tree.
#[derive(Clone, Copy)]
enum TreeRow {
    Server(NetworkId),
    Channel(ConversationId),
}

#[derive(Clone, Copy)]
enum PaneKind {
    MainLog,
    SubLog,
    Members,
    Channels,
}

/// A chat window pane rendered as its own view. GPUI redraws the whole window
/// whenever the draft input changes; used with [`AnyView::cached`], a pane
/// instead reuses its previous layout and paint unless it was notified. The
/// pane re-renders whenever the chat window is notified (state changes) and
/// when its own list scrolls or a row's hover state changes.
struct ChatPane {
    chat: WeakEntity<ChatWindow>,
    kind: PaneKind,
    _chat_changed: Subscription,
}

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        // Rows and their listeners belong to the chat window, so build them in
        // its context; it is not being updated while panes lay out.
        self.chat
            .update(cx, |chat, cx| {
                // Only while the main log redraws: a reused (cached) pane
                // replays sprites that may still point at these images.
                if matches!(kind, PaneKind::MainLog) {
                    chat.previews.release(window);
                }
                // Avatars appear in two cached panes; an image is freed only
                // after both redrew. Redraw both once more if one lags.
                let avatar_pane = match kind {
                    PaneKind::MainLog => Some(avatars::Pane::MainLog),
                    PaneKind::Members => Some(avatars::Pane::Members),
                    _ => None,
                };
                if let Some(pane) = avatar_pane
                    && chat.avatars.release(pane, window)
                {
                    cx.on_next_frame(window, |_, _, cx| cx.notify());
                }
                chat.render_pane(kind, cx)
            })
            .unwrap_or_else(|_| div().into_any_element())
    }
}

#[derive(Clone)]
struct ChatPanes {
    main_log: AnyView,
    sub_log: AnyView,
    members: AnyView,
    channels: AnyView,
}

impl ChatPanes {
    fn new(cx: &mut Context<ChatWindow>) -> Self {
        let chat = cx.entity();
        let mut pane = |kind| {
            let chat = chat.clone();
            AnyView::from(cx.new(|cx| ChatPane {
                chat: chat.downgrade(),
                kind,
                _chat_changed: cx.observe(&chat, |_, _, cx| cx.notify()),
            }))
        };
        Self {
            main_log: pane(PaneKind::MainLog),
            sub_log: pane(PaneKind::SubLog),
            members: pane(PaneKind::Members),
            channels: pane(PaneKind::Channels),
        }
    }
}

/// Layout of a cached pane inside its column.
fn pane_style() -> StyleRefinement {
    StyleRefinement::default().flex_1().min_h_0()
}

/// Colors and fonts shared by log rows, derived from the appearance settings.
struct LogStyle {
    theme: Theme,
    main_alt: Rgba,
    event_color: Rgba,
    sub_alt: Rgba,
    time_font: SharedString,
    alternate_rows: bool,
    wrap_nicknames: bool,
    /// Width of the channel name column of the combined log.
    sub_name_width: f32,
}

/// Width of the main log's nickname column: nicknames of up to 15 typical
/// characters (the longest some servers allow) fit without shortening.
const NICK_COLUMN_WIDTH: f32 = 124.;

impl LogStyle {
    fn new(appearance: &Appearance, theme: Theme) -> Self {
        Self {
            theme,
            main_alt: theme.panes.main_alternate,
            event_color: theme.panes.channel_event,
            sub_alt: theme.panes.sub_alternate,
            // Built for every visible row on each redraw; the default font
            // name needs no allocation.
            time_font: if appearance.time_font.is_empty() {
                SharedString::new_static(default_time_font())
            } else {
                appearance.time_font.clone().into()
            },
            alternate_rows: appearance.alternate_rows,
            wrap_nicknames: appearance.wrap_long_nicknames,
            sub_name_width: appearance
                .sub_log_name_width
                .clamp(*SUB_LOG_NAME_WIDTHS.start(), *SUB_LOG_NAME_WIDTHS.end())
                as f32,
        }
    }

    fn time(&self, time: TimeOfDay) -> Div {
        div()
            .w(px(42.))
            .flex_shrink_0()
            .font_family(self.time_font.clone())
            .text_color(self.theme.time)
            .child(time.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainRow {
    Status,
    DiagnosticsHeading,
    Diagnostic(usize),
    Message(usize),
}

/// Fixed rows shown above the selected log's messages.
struct MainLayout {
    status: bool,
    heading: bool,
    diagnostics: std::ops::Range<usize>,
}

impl MainLayout {
    fn prefix(&self) -> usize {
        usize::from(self.status) + usize::from(self.heading) + self.diagnostics.len()
    }

    fn row(&self, mut index: usize) -> MainRow {
        if self.status {
            if index == 0 {
                return MainRow::Status;
            }
            index -= 1;
        }
        if self.heading {
            if index == 0 {
                return MainRow::DiagnosticsHeading;
            }
            index -= 1;
        }
        if index < self.diagnostics.len() {
            return MainRow::Diagnostic(self.diagnostics.start + index);
        }
        MainRow::Message(index - self.diagnostics.len())
    }
}

impl ChatWindow {
    fn main_layout(&self) -> MainLayout {
        let count = self
            .selected_session()
            .map_or(0, |session| session.diagnostics.len());
        let registration_incomplete = self
            .selected_network_id()
            .and_then(|network| self.state.status(network))
            != Some(&ConnectionStatus::Registered);
        if self.state.selected_channel().is_some() {
            if registration_incomplete && count > 0 {
                MainLayout {
                    status: true,
                    heading: true,
                    diagnostics: 0..count,
                }
            } else {
                MainLayout {
                    status: registration_incomplete,
                    heading: false,
                    diagnostics: if self.debug_enabled && count > 0 {
                        count - 1..count
                    } else {
                        0..0
                    },
                }
            }
        } else {
            let diagnostics = (self.debug_enabled || registration_incomplete) && count > 0;
            MainLayout {
                status: true,
                heading: diagnostics,
                diagnostics: if diagnostics { 0..count } else { 0..0 },
            }
        }
    }

    /// Updates the virtualized log lists to match application state before
    /// they lay out their visible rows.
    fn sync_log_lists(&mut self) {
        let prefix = self.main_layout().prefix();
        let sequences: Vec<u64> = match self.state.selected_channel() {
            Some(channel) => channel.messages.iter().map(|m| m.sequence).collect(),
            None => self
                .selected_network_id()
                .map(|network| {
                    self.state
                        .server_messages(network)
                        .iter()
                        .map(|m| m.sequence)
                        .collect()
                })
                .unwrap_or_default(),
        };
        self.main_lists
            .entry(self.state.selection())
            .or_insert_with(LogList::new)
            .sync(prefix, &sequences);

        // The combined log shows the newest conversation lines from every other
        // channel; JOIN/PART/QUIT/MODE activity stays in its own channel log.
        // Only the tail of each channel can reach the combined tail. Rebuild it
        // only when a message arrives or the selection changes, not on every
        // redraw (each keystroke redraws the window).
        let selected = self.state.selected_channel().map(|channel| channel.id);
        let source = (selected, self.state.last_message_sequence());
        if self.sub_source == Some(source) {
            return;
        }
        self.sub_source = Some(source);
        let rows = newest_lines(self.state.conversations(), selected, SUB_LOG_LIMIT);
        let sequences: Vec<u64> = rows.iter().map(|(sequence, _, _)| *sequence).collect();
        self.sub_list.sync(0, &sequences);
        self.sub_rows = rows.into_iter().map(|(_, id, index)| (id, index)).collect();
    }

    fn render_main_row(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme::current(cx);
        let style = LogStyle::new(&self.appearance, theme::current(cx));
        match self.main_layout().row(row) {
            MainRow::Status => {
                let text = match self.selected_network_id() {
                    Some(network) => self.status_text(self.state.status(network)),
                    None => format!(
                        "{} — {}",
                        self.i18n.text("no_servers"),
                        self.i18n.text("no_servers_hint")
                    ),
                };
                div()
                    .w_full()
                    .when(self.state.selected_channel().is_some(), |d| {
                        d.text_color(theme.warning)
                    })
                    .child(text)
                    .into_any_element()
            }
            MainRow::DiagnosticsHeading => div()
                .w_full()
                .py_1()
                .font_weight(FontWeight::BOLD)
                .child(self.i18n.text("diagnostics_heading"))
                .into_any_element(),
            MainRow::Diagnostic(index) => div()
                .w_full()
                .text_color(theme.text_secondary)
                .child(
                    self.selected_session()
                        .and_then(|session| session.diagnostics.get(index).cloned())
                        .unwrap_or_default(),
                )
                .into_any_element(),
            MainRow::Message(index) => match self.state.selected_channel() {
                Some(channel) => self.render_channel_message(channel.id, index, &style, cx),
                None => {
                    let Some(network) = self.selected_network_id() else {
                        return div().into_any_element();
                    };
                    let Some(message) = self.state.server_messages(network).get(index) else {
                        return div().into_any_element();
                    };
                    div()
                        .w_full()
                        .flex()
                        .gap_1()
                        .py(px(1.))
                        .when(style.alternate_rows && index % 2 == 1, |d| {
                            d.bg(style.main_alt)
                        })
                        .child(style.time(message.time))
                        .child(div().flex_1().min_w_0().child(message.text.clone()))
                        .into_any_element()
                }
            },
        }
    }

    fn render_channel_message(
        &self,
        selected_channel: ConversationId,
        index: usize,
        style: &LogStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme::current(cx);
        let Some(channel) = self.state.selected_channel() else {
            return div().into_any_element();
        };
        let network = channel.network;
        let Some(message) = channel.messages.get(index) else {
            return div().into_any_element();
        };
        let urls = log_urls(&message.text);
        // Channel activity lines stay text-only.
        let preview = if message.activity {
            None
        } else {
            self.previews.lookup(
                &urls,
                previews::RowRef {
                    selection: Selection::Channel(selected_channel),
                    sequence: message.sequence,
                },
            )
        };
        if matches!(preview, Some((_, previews::Shown::Pending))) {
            self.pump_previews(cx);
        }
        let selected_range = self
            .log_selection
            .filter(|selection| selection.channel == selected_channel)
            .and_then(|selection| selection.range(index, message.text.len()));
        let highlights = self.highlight_ranges(network, message);
        let styled = styled_log_text(
            &message.text,
            &urls,
            &highlights,
            selected_range,
            &style.theme,
        );
        let layout = styled.layout().clone();
        let down_layout = layout.clone();
        let move_layout = layout.clone();
        let click_layout = layout;
        let move_urls = urls.clone();
        let over_url = self.url_hover == Some((selected_channel, index));
        let text_len = message.text.len();
        div()
            .w_full()
            .flex()
            .items_start()
            .gap_1()
            .py(px(1.))
            .when(style.alternate_rows && index % 2 == 1, |d| {
                d.bg(style.main_alt)
            })
            .child(style.time(message.time))
            .when(!message.activity && self.avatars.enabled(), |row| {
                row.child(self.avatar_slot(
                    self.own_avatar_for(network, &message.sender).or_else(|| {
                        self.state
                            .avatars()
                            .for_message(
                                network,
                                &cayenchat_irc_core::text::nickname_key(&message.sender),
                                message.sequence,
                            )
                            .cloned()
                    }),
                    &message.sender,
                    cx,
                ))
            })
            .when(!message.activity, |row| {
                row.child(
                    div()
                        .w(px(NICK_COLUMN_WIDTH))
                        .flex_shrink_0()
                        .flex()
                        .justify_end()
                        .text_right()
                        .text_color(theme.nickname)
                        .child(
                            div()
                                .min_w_0()
                                .when(!style.wrap_nicknames, |d| {
                                    d.overflow_hidden().whitespace_nowrap().text_ellipsis()
                                })
                                .child(message.sender.clone()),
                        )
                        .child(":"),
                )
            })
            .child({
                let text = div()
                    .id(("message-text", index))
                    .debug_selector(move || format!("message-text-{index}"))
                    .when(preview.is_none(), |d| d.flex_1())
                    .min_w_0()
                    .when(message.activity, |d| d.text_color(style.event_color))
                    .when(message.delivery_failed, |d| d.text_color(theme.warning))
                    .cursor(if over_url {
                        CursorStyle::PointingHand
                    } else {
                        CursorStyle::IBeam
                    })
                    .child(styled)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let byte = down_layout
                                .index_for_position(event.position)
                                .unwrap_or_else(|index| index)
                                .min(text_len);
                            this.start_log_selection(selected_channel, index, byte, window, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        let position = move_layout.index_for_position(event.position);
                        let byte = position.unwrap_or_else(|index| index).min(text_len);
                        this.extend_log_selection(selected_channel, index, byte, cx);
                        // Over a URL (not while selecting) the pointer is a
                        // hand: a double click opens it.
                        let hover = (event.pressed_button.is_none()
                            && position.is_ok()
                            && move_urls.iter().any(|(range, _)| range.contains(&byte)))
                        .then_some((selected_channel, index));
                        if this.url_hover != hover {
                            this.url_hover = hover;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                        if event.click_count() == 2 {
                            let byte = click_layout
                                .index_for_position(event.position())
                                .unwrap_or_else(|index| index);
                            if let Some((_, url)) =
                                urls.iter().find(|(range, _)| range.contains(&byte))
                            {
                                cx.open_url(url);
                            }
                        }
                    }));
                match preview {
                    None => text.into_any_element(),
                    Some((link, shown)) => div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(text)
                        .child(preview_element(link, shown, index, &theme, cx))
                        .into_any_element(),
                }
            })
            .into_any_element()
    }

    /// The fixed avatar slot of a message or member row: the image when it
    /// is ready, blank while it loads, and the nickname's default avatar
    /// when there is none or it failed. It never changes the row's height.
    fn avatar_slot(&self, avatar: Option<Arc<str>>, nickname: &str, cx: &mut Context<Self>) -> Div {
        let slot = div()
            .w(px(avatars::SLOT))
            .h(px(avatars::SLOT))
            .mt(px(2.))
            .flex_shrink_0()
            .overflow_hidden();
        let shown = match &avatar {
            Some(avatar) => self.avatars.lookup(avatar),
            None => avatars::Shown::None,
        };
        match shown {
            avatars::Shown::Image(image) => slot.child(img(image).size_full()),
            avatars::Shown::Pending => {
                self.pump_avatars(cx);
                slot
            }
            avatars::Shown::None => slot.child(img(self.avatars.default_for(nickname)).size_full()),
        }
    }

    fn render_sub_row(&mut self, row: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::current(cx);
        let style = LogStyle::new(&self.appearance, theme::current(cx));
        let Some(&(id, index)) = self.sub_rows.get(row) else {
            return div().into_any_element();
        };
        let Some(conversation) = self.state.conversations().iter().find(|c| c.id == id) else {
            return div().into_any_element();
        };
        let Some(message) = conversation.messages.get(index) else {
            return div().into_any_element();
        };
        let network = self
            .state
            .networks()
            .iter()
            .find(|network| network.id == conversation.network)
            .map(|network| network.name.split_whitespace().next().unwrap_or(""))
            .unwrap_or("");
        div()
            .id(("sub-message", row))
            .w_full()
            .flex()
            .gap_2()
            .py(px(1.))
            .when(style.alternate_rows && row % 2 == 1, |d| {
                d.bg(style.sub_alt)
            })
            .cursor_pointer()
            .hover(|d| d.bg(theme.hover))
            .child(style.time(message.time))
            .child(
                div()
                    .w(px(style.sub_name_width))
                    .flex_shrink_0()
                    .flex()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(theme.nickname)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(conversation.name.clone()),
                    )
                    .child(
                        div()
                            .max_w(px(90.))
                            .min_w_0()
                            .flex_shrink_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(format!(" [{network}]")),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(message.activity, |d| d.text_color(style.event_color))
                    .child(if message.activity {
                        StyledText::new(message.text.clone())
                    } else {
                        let offset = message.sender.len() + 2;
                        let highlight = HighlightStyle {
                            color: Some(theme.panes.highlight.into()),
                            font_weight: Some(FontWeight::BOLD),
                            ..Default::default()
                        };
                        StyledText::new(format!("{}: {}", message.sender, message.text))
                            .with_highlights(
                                self.highlight_ranges(conversation.network, message)
                                    .into_iter()
                                    .map(|range| {
                                        (range.start + offset..range.end + offset, highlight)
                                    }),
                            )
                    }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                // A single click is too easy to hit while reading the combined log.
                if event.click_count() == 2 {
                    this.dispatch(Command::SelectChannel(id), window, cx);
                }
            }))
            .into_any_element()
    }

    fn render_member_rows(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = theme::current(cx);
        let Some(channel) = self.state.selected_channel() else {
            return Vec::new();
        };
        let end = range.end.min(channel.members.len());
        let start = range.start.min(end);
        let network = channel.network;
        let conversation = channel.id;
        let avatars_shown = self.avatars.enabled();
        (start..end)
            .map(|index| {
                let member = channel.members[index].clone();
                let selected = self.member_selection.contains(conversation, &member);
                let channel = channel.name.clone();
                let nickname = member
                    .trim_start_matches(['~', '&', '@', '%', '+'])
                    .to_owned();
                let avatar = avatars_shown.then(|| {
                    self.own_avatar_for(network, &nickname).or_else(|| {
                        self.state
                            .avatars()
                            .current(network, &cayenchat_irc_core::text::nickname_key(&nickname))
                            .cloned()
                    })
                });
                let has_avatar = avatar.is_some();
                exclusive_row(div().id(("member", index)))
                    .debug_selector(move || format!("member-row-{index}"))
                    .px_2()
                    .py(px(1.))
                    .when(selected, |d| d.bg(theme.selected))
                    .when(!selected, |d| d.hover(|d| d.bg(theme.hover)))
                    .when_some(avatar, |row, avatar| {
                        row.flex()
                            .items_center()
                            .gap_1()
                            .child(self.avatar_slot(avatar, &nickname, cx))
                    })
                    // The list gives every row the height of the first, so a
                    // nickname that wrapped would overlap the next row.
                    // Beside an avatar the row is a flex row and the name
                    // takes what is left of it; without one the row is a
                    // block and the name is as wide as the row.
                    .child(
                        div()
                            .when(has_avatar, |name| name.flex_1())
                            .min_w_0()
                            .truncate()
                            .child(member.clone()),
                    )
                    // A shortened name is read in full on hover.
                    .tooltip({
                        let full: SharedString = member.into();
                        move |_, cx| {
                            let full = full.clone();
                            cx.new(|_| ircv3_settings::TextTooltip(full)).into()
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            let click = if event.modifiers.secondary() {
                                member_selection::Click::Toggle
                            } else if event.modifiers.shift {
                                member_selection::Click::Range
                            } else {
                                member_selection::Click::Only
                            };
                            this.click_member(index, click);
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            // A menu for a member outside the chosen ones
                            // acts on that member alone.
                            if !selected {
                                this.click_member(index, member_selection::Click::Only);
                            }
                            // On one of several chosen members the menu acts
                            // on all of them.
                            let group = this
                                .state
                                .selected_channel()
                                .map(|channel| {
                                    this.member_selection
                                        .nicknames(channel.id, &channel.members)
                                })
                                .filter(|chosen| chosen.len() >= 2)
                                .unwrap_or_default();
                            let viewport = window.viewport_size();
                            this.server_menu = None;
                            this.channel_menu = None;
                            this.member_prompt = None;
                            this.member_menu = Some(MemberMenu {
                                position: point(
                                    event
                                        .position
                                        .x
                                        .min((viewport.width - px(210.)).max(px(0.))),
                                    event
                                        .position
                                        .y
                                        .min((viewport.height - px(190.)).max(px(0.))),
                                ),
                                network,
                                nickname: nickname.clone(),
                                channel: channel.clone(),
                                group,
                            });
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

/// Makes a row of a list the only one under the pointer when it is on the
/// line between two rows. GPUI counts a bounds' lower and right edges as
/// inside it, so the line shared by two rows is inside both, and both would
/// be hovered (and colored) at once, while a click reaches only the one in
/// front. Scrolling still passes through to the list.
fn exclusive_row<E: InteractiveElement>(row: E) -> E {
    row.block_mouse_except_scroll()
}

fn navigation_binding(key: &str, command: Command) -> KeyBinding {
    KeyBinding::new(key, Navigate { command }, Some("ChatWindow"))
}

fn app_menus(debug_enabled: bool, i18n: &Localizer) -> Vec<Menu> {
    let mut app_items = vec![
        MenuItem::action(i18n.text("menu_settings"), OpenSettings),
        MenuItem::separator(),
    ];
    #[cfg(target_os = "macos")]
    app_items.push(MenuItem::os_submenu(
        i18n.text("menu_services"),
        SystemMenuType::Services,
    ));
    app_items.push(MenuItem::action(i18n.text("menu_quit"), Quit));
    vec![
        Menu {
            name: "CayenChat".into(),
            items: app_items,
        },
        Menu {
            name: i18n.text("menu_connection").into(),
            items: vec![
                MenuItem::action(i18n.text("menu_settings"), OpenSettings),
                MenuItem::action(i18n.text("disconnect"), Disconnect),
                MenuItem::action(i18n.text("reconnect"), Reconnect),
            ],
        },
        Menu {
            name: i18n.text("menu_edit").into(),
            items: vec![
                MenuItem::action(i18n.text("menu_undo"), input::Undo),
                MenuItem::action(i18n.text("menu_redo"), input::Redo),
                MenuItem::separator(),
                MenuItem::action(i18n.text("menu_cut"), input::Cut),
                MenuItem::action(i18n.text("menu_copy"), input::Copy),
                MenuItem::action(i18n.text("menu_paste"), input::Paste),
                MenuItem::action(i18n.text("menu_select_all"), input::SelectAll),
            ],
        },
        Menu {
            name: i18n.text("menu_view").into(),
            items: vec![
                MenuItem::action(
                    if debug_enabled {
                        i18n.text("menu_hide_diagnostics")
                    } else {
                        i18n.text("menu_show_diagnostics")
                    },
                    ToggleDebug,
                ),
                MenuItem::action(i18n.text("menu_copy_diagnostics"), CopyDiagnostics),
            ],
        },
        Menu {
            name: i18n.text("menu_window").into(),
            items: vec![MenuItem::action(i18n.text("menu_settings"), OpenSettings)],
        },
    ]
}

/// Saved key preferences, kept so a desktop key-theme change can rebind
/// without the settings window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ShortcutPrefs {
    channel_modifier: ChannelNumberModifier,
    text_keys: TextKeyTheme,
    /// Navigation keys the user changed (action id → key).
    overrides: shortcuts::Overrides,
}

impl Global for ShortcutPrefs {}

impl From<&Settings> for ShortcutPrefs {
    fn from(settings: &Settings) -> Self {
        Self {
            channel_modifier: settings.channel_number_modifier,
            text_keys: settings.text_key_theme,
            overrides: settings.keybindings.clone(),
        }
    }
}

/// Replaces every key binding, so changed key preferences apply without
/// restarting.
fn apply_shortcuts(prefs: ShortcutPrefs, cx: &mut App) {
    // Keys the user changed can end up shared with another action or a fixed
    // shortcut (a hand-edited settings file, or before the shortcuts tab can
    // warn). Both bindings are kept, as before, and the clash is reported.
    if !prefs.overrides.is_empty() {
        let fixed = fixed_bindings(prefs.channel_modifier);
        for conflict in shortcuts::conflicts(&prefs.overrides, &fixed) {
            eprintln!(
                "CayenChat: the shortcut {} is taken by more than one action: {:?}",
                conflict.key, conflict.owners
            );
        }
    }
    cx.set_global(prefs);
    rebind_shortcuts(cx);
}

fn rebind_shortcuts(cx: &mut App) {
    let prefs = cx
        .try_global::<ShortcutPrefs>()
        .cloned()
        .unwrap_or_default();
    let emacs = match prefs.text_keys {
        TextKeyTheme::Auto => desktop::current(cx).emacs_keys,
        TextKeyTheme::Standard => false,
        TextKeyTheme::Emacs => true,
    };
    cx.clear_key_bindings();
    input::bind_keys(emacs, cx);
    cx.bind_keys(shortcut_bindings(prefs.channel_modifier, &prefs.overrides));
}

/// Standard Tab / Shift+Tab movement between settings fields (tab stops).
fn field_traversal<E: InteractiveElement>(element: E) -> E {
    element
        .on_action(|_: &FocusNextField, window, _| window.focus_next())
        .on_action(|_: &FocusPreviousField, window, _| window.focus_prev())
}

/// Every application shortcut: the fixed ones and the navigation actions with
/// what the user changed (see `shortcuts`).
fn shortcut_bindings(
    channel_modifier: ChannelNumberModifier,
    overrides: &shortcuts::Overrides,
) -> Vec<KeyBinding> {
    let mut bindings = fixed_bindings(channel_modifier);
    bindings.extend(shortcuts::bindings(overrides));
    bindings
}

/// The shortcuts that cannot be changed: sending, completion, settings, the
/// numbered channels and servers.
#[cfg_attr(target_os = "macos", allow(unused_variables))]
fn fixed_bindings(channel_modifier: ChannelNumberModifier) -> Vec<KeyBinding> {
    let mut bindings = vec![
        // Chat-only: settings fields keep the platform's Tab traversal and
        // are never captured by chat commands.
        KeyBinding::new("tab", CompleteNickname, Some("ChatWindow > TextInput")),
        KeyBinding::new("enter", SendMessage, Some("ChatWindow > TextInput")),
        KeyBinding::new("ctrl-enter", Notice, Some("ChatWindow > TextInput")),
        KeyBinding::new("up", HistoryPrevious, Some("ChatWindow > TextInput")),
        KeyBinding::new("down", HistoryNext, Some("ChatWindow > TextInput")),
        KeyBinding::new("tab", FocusNextField, Some("SettingsWindow")),
        KeyBinding::new("shift-tab", FocusPreviousField, Some("SettingsWindow")),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-shift-d", ToggleDebug, None),
        KeyBinding::new("secondary-shift-l", CopyDiagnostics, None),
        KeyBinding::new("secondary-c", CopyLogSelection, Some("MainLog")),
        KeyBinding::new("secondary-q", Quit, None),
    ];

    for index in 0..10 {
        let digit = (index + 1) % 10;
        #[cfg(target_os = "macos")]
        {
            bindings.push(navigation_binding(
                &format!("cmd-{digit}"),
                Command::SelectChannelAt(index),
            ));
            bindings.push(navigation_binding(
                &format!("cmd-ctrl-{digit}"),
                Command::SelectServerAt(index),
            ));
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            let modifier = match channel_modifier {
                ChannelNumberModifier::Ctrl => "ctrl",
                ChannelNumberModifier::Alt => "alt",
                ChannelNumberModifier::Super => "super",
            };
            bindings.push(navigation_binding(
                &format!("{modifier}-{digit}"),
                Command::SelectChannelAt(index),
            ));
            bindings.push(navigation_binding(
                &format!("ctrl-alt-{digit}"),
                Command::SelectServerAt(index),
            ));
        }
    }
    bindings
}

/// Chooses the Linux display server before GPUI picks one from the
/// environment. `CAYENCHAT_DISPLAY=x11|wayland` overrides the saved choice.
#[cfg(target_os = "linux")]
fn select_linux_display(saved: LinuxDisplay) {
    let choice = match std::env::var("CAYENCHAT_DISPLAY")
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Ok("x11") => LinuxDisplay::X11,
        Ok("wayland") => LinuxDisplay::Wayland,
        _ => saved,
    };
    let has_x11 = std::env::var_os("DISPLAY").is_some_and(|display| !display.is_empty());
    if choice == LinuxDisplay::X11 && has_x11 {
        // SAFETY: called at the start of main, before GPUI or any other thread
        // exists, so nothing reads the environment concurrently.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }
}

/// Loads settings and moves plaintext passwords saved by older versions into
/// the credential store. Returns a notice for the chat window.
fn load_settings_at_startup() -> (Settings, Option<String>) {
    let mut saved = match cayenchat_storage::load() {
        Ok(saved) => saved.unwrap_or_default(),
        Err(error) => return (Settings::default(), Some(error)),
    };
    let i18n = Localizer::new(saved.language);
    let logging_error = diagnostics::configure(&saved.experimental)
        .err()
        .map(|error| i18n.format("debug_log_error", &[("error", &error)]));
    let notice = match cayenchat_storage::migrate_legacy_secrets(&mut saved, &CredentialStore::open)
    {
        Ok(None) => None,
        Ok(Some(report)) if report.used_local_file => Some(i18n.text("legacy_migrated_local")),
        Ok(Some(_)) => Some(i18n.text("legacy_migrated")),
        Err(error) => Some(i18n.format("credential_error", &[("error", &error)])),
    };
    let notice = match (notice, logging_error) {
        (Some(first), Some(second)) => Some(format!("{first}\n{second}")),
        (notice, logging_error) => notice.or(logging_error),
    };
    (saved, notice)
}

fn main() {
    diagnostics::init();
    #[cfg(feature = "test-build")]
    match cayenchat_storage::test_build_directory() {
        Ok(directory) => eprintln!(
            "CayenChat test build: fresh settings in {}",
            directory.display()
        ),
        Err(error) => eprintln!("CayenChat test build: {error}"),
    }
    let (saved, notice) = load_settings_at_startup();
    #[cfg(target_os = "linux")]
    select_linux_display(saved.linux_display);
    Application::new().run(move |cx: &mut App| {
        secrets::install(saved.credential_backend, cx);
        theme::apply(saved.theme, &saved.appearance, cx);
        desktop::watch(cx);
        apply_shortcuts(ShortcutPrefs::from(&saved), cx);
        cx.set_menus(app_menus(false, &Localizer::new(Language::System)));
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // Where the window was left, when that is remembered and a connected
        // display still shows it; otherwise the usual place.
        let min_size = size(px(720.), px(420.));
        let layout = if saved.restore_window_layout {
            cayenchat_storage::layout::load_layout()
        } else {
            Default::default()
        };
        let displays = cx.displays();
        let display_bounds: Vec<_> = displays.iter().map(|display| display.bounds()).collect();
        let restored = window_layout::restored_bounds(&layout, &display_bounds, min_size);
        // Windows opens a window on the primary display unless told which
        // one, and replaces bounds off that display with a centered default
        // (#125). Its displays share one desktop coordinate space, so the
        // display chosen above is the one to name. macOS and X11 report every
        // display at the origin, so there it would be a guess.
        let display_id = restored
            .as_ref()
            .filter(|_| cfg!(target_os = "windows"))
            .map(|restored| displays[restored.display].id());
        let window_bounds = restored.map(|restored| restored.bounds).unwrap_or_else(|| {
            WindowBounds::Windowed(Bounds::centered(None, size(px(960.), px(600.)), cx))
        });
        let chat_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(window_bounds),
                    display_id,
                    window_min_size: Some(min_size),
                    titlebar: Some(TitlebarOptions {
                        title: Some("CayenChat".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |window, cx| cx.new(|cx| ChatWindow::with_settings(saved, notice, window, cx)),
            )
            .expect("could not open CayenChat window");
        cx.activate(true);
        // Settings open when nothing connects at startup or a saved server
        // cannot start, showing the first such server; valid servers still
        // connect.
        let settings_for = chat_window
            .update(cx, |chat, window, cx| {
                chat.layout_file = cayenchat_storage::layout::layout_path().ok();
                chat.apply_layout(&layout);
                cx.set_menus(app_menus(false, &chat.i18n));
                let startup = std::mem::take(&mut chat.startup_connections);
                let mut settings_for = startup.is_empty().then(|| chat.selected_profile_id());
                for (network, config) in startup {
                    let failed = match config {
                        Ok(config) => chat.apply_connection(network, config, window, cx).is_err(),
                        Err(error) => {
                            chat.feedback =
                                Some(chat.i18n.format("startup_invalid", &[("error", &error)]));
                            cx.notify();
                            true
                        }
                    };
                    if failed && settings_for.is_none() {
                        settings_for = Some(
                            chat.sessions
                                .get(&network)
                                .map(|session| session.profile_id.clone()),
                        );
                    }
                }
                settings_for
            })
            .expect("could not initialize the chat window");
        if let Some(profile) = settings_for {
            chat_window
                .update(cx, |chat, window, cx| {
                    chat.open_settings_for(profile, window, cx)
                })
                .expect("could not open the initial settings window");
        }
    });
}

#[cfg(test)]
mod combined_log_tests {
    use super::newest_lines;
    use cayenchat_model::{Conversation, ConversationId, Message, NetworkId, TimeOfDay};

    /// The previous implementation: every tail collected, then sorted.
    fn collect_and_sort(
        conversations: &[Conversation],
        excluded: Option<ConversationId>,
        limit: usize,
    ) -> Vec<(u64, ConversationId, usize)> {
        let mut rows: Vec<_> = conversations
            .iter()
            .filter(|c| Some(c.id) != excluded)
            .flat_map(|c| {
                c.messages
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, m)| {
                        !m.activity && m.provenance != cayenchat_model::Provenance::Requested
                    })
                    .take(limit)
                    .map(move |(index, m)| (m.sequence, c.id, index))
            })
            .collect();
        rows.sort_unstable_by_key(|(sequence, _, _)| *sequence);
        rows.drain(..rows.len().saturating_sub(limit));
        rows
    }

    #[test]
    fn merged_tails_match_collecting_and_sorting() {
        // Interleaved arrivals across three conversations on two networks,
        // with activity lines and an empty conversation.
        let mut conversations: Vec<Conversation> = (0..4)
            .map(|id| Conversation {
                id: ConversationId(id + 1),
                network: NetworkId(id / 2 + 1),
                kind: cayenchat_model::ConversationKind::Channel,
                name: format!("#c{id}"),
                topic: String::new(),
                messages: Vec::new(),
                members: Vec::new(),
            })
            .collect();
        for sequence in 1..=500u64 {
            let target = [0, 1, 1, 2, 0, 2, 1][sequence as usize % 7];
            conversations[target].messages.push(Message {
                time: TimeOfDay::new(0, 0),
                sequence,
                timestamp: None,
                native_id: None,
                account: None,
                delivery_failed: false,
                sender: "bob".into(),
                text: String::new(),
                activity: sequence % 5 == 0,
                provenance: if sequence % 11 == 0 {
                    cayenchat_model::Provenance::Requested
                } else {
                    cayenchat_model::Provenance::Live
                },
            });
        }
        for excluded in [None, Some(ConversationId(2)), Some(ConversationId(4))] {
            for limit in [0, 1, 7, 100, 1_000] {
                assert_eq!(
                    newest_lines(&conversations, excluded, limit),
                    collect_and_sort(&conversations, excluded, limit),
                    "excluded {excluded:?}, limit {limit}"
                );
            }
        }
    }
}

#[cfg(test)]
mod log_tests {
    use super::{LogPosition, LogSelection, channel_activity_text, log_urls};
    use cayenchat_irc_core::ChannelActivityKind;
    use cayenchat_model::ConversationId;

    #[test]
    fn finds_only_web_urls_without_sentence_punctuation() {
        let text =
            "see https://example.org/a?q=1, and http://example.jp/path。 ftp://example.org/x";
        let urls = log_urls(text);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].1, "https://example.org/a?q=1");
        assert_eq!(urls[1].1, "http://example.jp/path");
        assert_eq!(&text[urls[0].0.clone()], urls[0].1);
    }

    #[test]
    fn channel_activity_uses_english_phrases_with_optional_details() {
        assert_eq!(
            channel_activity_text(
                "kaeru",
                ChannelActivityKind::Joined {
                    mask: Some("~kaeru@host".into()),
                },
            ),
            "kaeru has joined (~kaeru@host)"
        );
        assert_eq!(
            channel_activity_text("kaeru", ChannelActivityKind::Left { reason: None }),
            "kaeru has left"
        );
        assert_eq!(
            channel_activity_text(
                "kaeru",
                ChannelActivityKind::Quit {
                    reason: Some("bye".into()),
                },
            ),
            "kaeru has quit (bye)"
        );
        assert_eq!(
            channel_activity_text(
                "kaeru",
                ChannelActivityKind::NickChanged { to: "tepeu".into() },
            ),
            "kaeru is now known as tepeu"
        );
        assert_eq!(
            channel_activity_text(
                "tepeu",
                ChannelActivityKind::ModeChanged {
                    modes: "+o kaeru".into(),
                },
            ),
            "tepeu has changed mode: +o kaeru"
        );
    }

    #[test]
    fn log_selection_spans_partial_messages() {
        let selection = LogSelection {
            channel: ConversationId(1),
            anchor: LogPosition { row: 2, byte: 3 },
            cursor: LogPosition { row: 0, byte: 2 },
        };
        assert_eq!(selection.range(0, 5), Some(2..5));
        assert_eq!(selection.range(1, 5), Some(0..5));
        assert_eq!(selection.range(2, 5), Some(0..3));
        assert_eq!(selection.range(3, 5), None);
    }
}

#[cfg(test)]
mod startup_tests {
    use super::{connection_config, forget_removed_profiles, startup_connections};
    use cayenchat_storage::{
        CredentialBackendKind, CredentialStore, Secret, SecretKey, Settings,
        credentials::MemoryBackend,
    };
    use std::sync::Arc;

    fn memory_store() -> CredentialStore {
        CredentialStore::with_backend(Arc::new(MemoryBackend::new(CredentialBackendKind::System)))
    }

    #[test]
    fn startup_uses_only_saved_credentials_when_enabled() {
        let store = memory_store();
        let mut settings = Settings::default();
        settings.add_server(cayenchat_storage::PRESETS[0].host);
        settings.selected_profile_mut().unwrap().nickname = "alice".into();
        settings.selected_profile_mut().unwrap().username = "ident".into();
        assert!(startup_connections(&settings, &store).is_empty());

        let profile = settings.selected_profile_mut().unwrap();
        profile.connect_on_startup = true;
        profile.sasl_enabled = true;
        profile.sasl_username = "account".into();
        profile.use_tls = true;
        assert!(startup_connections(&settings, &store)[0].1.is_err());

        let profile = settings.selected_profile().unwrap().clone();
        store
            .set(&profile.sasl_password_key(), &Secret::new("secret"))
            .unwrap();
        store
            .set(
                &profile.server_password_key(),
                &Secret::new("server-secret"),
            )
            .unwrap();
        // Saved secrets are ignored until password saving is on.
        assert!(startup_connections(&settings, &store)[0].1.is_err());
        settings.selected_profile_mut().unwrap().remember_passwords = true;
        let startup = startup_connections(&settings, &store);
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].0, settings.selected_server);
        let config = startup.into_iter().next().unwrap().1.unwrap();
        assert_eq!(config.host, "irc.ircnet.ne.jp");
        assert_eq!(config.nickname, "alice");
        assert_eq!(config.username, "ident");
        assert_eq!(config.server_password.as_deref(), Some("server-secret"));
        let sasl = config.sasl.unwrap();
        assert_eq!(sasl.username, "account");
        assert_eq!(sasl.password, "secret");
    }

    #[test]
    fn every_server_marked_for_startup_connects_with_its_own_identity() {
        let store = memory_store();
        let mut settings = Settings::default();
        settings.add_server("");
        let custom = settings.selected_profile_mut().unwrap();
        custom.host = "irc.example.org".into();
        custom.nickname = "bob".into();
        custom.username = "bob".into();
        custom.channels = "#b".into();
        custom.connect_on_startup = true;
        let preset = settings.add_server(cayenchat_storage::PRESETS[0].host);
        preset.nickname = "alice".into();
        preset.username = "alice".into();
        preset.channels = "#a".into();
        preset.connect_on_startup = true;
        let startup = startup_connections(&settings, &store);
        // Tree order: the order the servers were added.
        let hosts: Vec<_> = startup
            .iter()
            .map(|(_, config)| {
                let config = config.as_ref().unwrap();
                (
                    config.host.as_str(),
                    config.nickname.as_str(),
                    config.channels.clone(),
                )
            })
            .collect();
        assert_eq!(
            hosts,
            [
                ("irc.example.org", "bob", vec!["#b".to_owned()]),
                ("irc.ircnet.ne.jp", "alice", vec!["#a".to_owned()]),
            ]
        );
    }

    #[test]
    fn irc_metadata_keeps_a_usable_account_only() {
        let meta = |account| super::irc_message_meta(None, None, account, false);
        assert_eq!(meta(Some("alice")).account.unwrap().as_str(), "alice");
        assert!(meta(Some("*")).account.is_none());
        assert!(meta(Some("")).account.is_none());
        assert!(meta(None).account.is_none());
        // The same account text on two networks is just two messages.
        assert_eq!(meta(Some("alice")).account, meta(Some("alice")).account);
    }

    #[test]
    fn the_saved_realname_is_used_for_every_connection() {
        let mut settings = Settings::default();
        settings.add_server(cayenchat_storage::PRESETS[0].host);
        let profile = settings.selected_profile_mut().unwrap();
        profile.nickname = "alice".into();
        profile.username = "ident".into();
        let language = settings.language;
        // Unset keeps the built-in default on the wire.
        let config =
            connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
        assert_eq!(config.realname, "");
        assert_eq!(config.username, "ident");
        settings.selected_profile_mut().unwrap().realname = "Alice Liddell".into();
        // A reconnect builds its configuration from the persisted value.
        let config =
            connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
        assert_eq!(config.realname, "Alice Liddell");
        assert_eq!(config.nickname, "alice");
        assert_eq!(config.username, "ident");
        settings.selected_profile_mut().unwrap().realname = "two\nlines".into();
        assert!(
            connection_config(settings.selected_profile().unwrap(), language, None, None).is_err()
        );
    }

    #[test]
    fn each_server_sends_its_own_quit_message() {
        let mut settings = Settings::default();
        settings.add_server(cayenchat_storage::PRESETS[0].host);
        let profile = settings.selected_profile_mut().unwrap();
        profile.nickname = "alice".into();
        profile.username = "ident".into();
        let language = settings.language;
        let config =
            connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
        assert_eq!(config.quit_message, "");
        settings.selected_profile_mut().unwrap().quit_message = "Back soon".into();
        let config =
            connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
        assert_eq!(config.quit_message, "Back soon");
    }

    #[test]
    fn nickname_and_username_stay_independent_without_credentials() {
        let mut settings = Settings::default();
        settings.add_server(cayenchat_storage::PRESETS[0].host);
        settings.selected_profile_mut().unwrap().nickname = "alice".into();
        settings.selected_profile_mut().unwrap().username = "someone".into();
        let language = settings.language;
        let config =
            connection_config(settings.selected_profile().unwrap(), language, None, None).unwrap();
        assert_eq!(config.nickname, "alice");
        assert_eq!(config.username, "someone");
        assert!(config.server_password.is_none());
        assert!(config.sasl.is_none());
        settings.selected_profile_mut().unwrap().username.clear();
        assert!(
            connection_config(settings.selected_profile().unwrap(), language, None, None).is_err()
        );
    }

    #[test]
    fn plaintext_server_password_needs_the_profile_opt_in() {
        let mut settings = Settings::default();
        let profile = settings.add_server("znc.lan");
        profile.nickname = "alice".into();
        profile.username = "alice".into();
        let language = settings.language;
        let password = || Some(Secret::new("alice/net:secret"));
        let profile = settings.selected_profile().unwrap();
        assert!(!profile.use_tls);
        assert!(connection_config(profile, language, password(), None).is_err());
        settings
            .selected_profile_mut()
            .unwrap()
            .allow_plaintext_pass = true;
        let config = connection_config(
            settings.selected_profile().unwrap(),
            language,
            password(),
            None,
        )
        .unwrap();
        assert!(!config.use_tls && config.allow_plaintext_pass);
        assert_eq!(config.server_password.as_deref(), Some("alice/net:secret"));
    }

    #[test]
    fn replacing_a_profile_before_saving_never_inherits_its_passwords() {
        // Cover both legacy sequential IDs and IDs assigned to new profiles.
        for legacy in [true, false] {
            let store = memory_store();
            let mut previous = Settings::default();
            previous.add_server("");
            previous.selected_profile_mut().unwrap().host = "old.example.org".into();
            previous.selected_profile_mut().unwrap().remember_passwords = true;
            if legacy {
                previous.selected_profile_mut().unwrap().id = "custom-1".into();
                previous.selected_server = "custom-1".into();
            }
            let removed = previous.selected_profile().unwrap().clone();
            for key in [removed.server_password_key(), removed.sasl_password_key()] {
                store.set(&key, &Secret::new("old-password")).unwrap();
            }

            let mut next = previous.clone();
            next.remove_selected_server();
            next.add_server("");
            next.selected_profile_mut().unwrap().host = "new.example.org".into();
            next.selected_profile_mut().unwrap().remember_passwords = true;
            next.selected_profile_mut().unwrap().sasl_enabled = true;

            // Connecting reads saved credentials before committing settings.
            let i18n = super::Localizer::new(next.language);
            let (server, sasl) =
                super::saved_connection_secrets(next.selected_profile().unwrap(), &store, &i18n)
                    .unwrap();
            assert!(server.is_none());
            assert!(sasl.is_none());
            assert_ne!(next.selected_profile().unwrap().id, removed.id);

            forget_removed_profiles(&previous, &next, &store);
            assert!(!store.contains(&removed.server_password_key()).unwrap());
            assert!(!store.contains(&removed.sasl_password_key()).unwrap());
        }
    }

    #[test]
    fn removed_profiles_lose_their_saved_passwords() {
        let store = memory_store();
        let mut previous = Settings::default();
        previous.add_server("");
        previous.selected_profile_mut().unwrap().host = "irc.example.org".into();
        let removed = previous.selected_profile().unwrap().clone();
        let kept = SecretKey::server_password(cayenchat_storage::IRCNET_ID);
        store
            .set(&removed.server_password_key(), &Secret::new("x"))
            .unwrap();
        store.set(&kept, &Secret::new("y")).unwrap();
        let mut next = previous.clone();
        next.remove_selected_server();
        forget_removed_profiles(&previous, &next, &store);
        assert!(store.get(&removed.server_password_key()).unwrap().is_none());
        assert!(store.get(&kept).unwrap().is_some());
    }
}

#[cfg(test)]
mod server_settings_tests {
    use super::{Localizer, SettingsForm, saved_connection_config};
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
            ["irc.ircnet.ne.jp", "6667", "", "", "", "", ""].map(str::to_owned)
        );
        let settings = cx.read(|cx| form.snapshot(cx)).unwrap();
        let preset = settings.selected_profile().unwrap();
        assert!(preset.nickname.is_empty() && preset.channels.is_empty());
        assert_eq!(preset.ircv3, Ircv3Preferences::default());
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
        assert_eq!(
            store.get(&a_key).unwrap().map(|s| s.expose().to_owned()),
            Some("a-typed".to_owned()),
            "the password went to the server it was typed for"
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

/// Default settings whose selected server auto-joins `channels`.
#[cfg(test)]
fn settings_with_channels(channels: &str) -> Settings {
    let mut settings = Settings::default();
    settings
        .add_server(cayenchat_storage::PRESETS[0].host)
        .channels = channels.into();
    settings
}

#[cfg(test)]
mod url_hover_tests {
    use super::ChatWindow;
    use cayenchat_irc_core::Event;
    use cayenchat_model::NetworkId;
    use gpui::{Modifiers, TestAppContext, point, px};

    #[gpui::test]
    fn the_pointer_is_a_hand_over_a_url_in_the_channel_log(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let message = |text: &str| Event::ChannelMessage {
            channel: "#a".into(),
            sender: "bob".into(),
            text: text.into(),
            notice: false,
            mentioned: false,
            server_time: None,
            msgid: None,
            account: None,
            replayed: false,
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "me".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    message("https://example.org/x"),
                    message("hello there"),
                ],
                false,
                cx,
            );
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            cx.notify();
        });
        cx.run_until_parked();
        let over = |row: usize, cx: &mut gpui::VisualTestContext| {
            let selector: &'static str = ["message-text-0", "message-text-1"][row];
            let bounds = cx.debug_bounds(selector).expect("row drawn");
            let at = point(bounds.origin.x + px(4.), bounds.center().y);
            cx.simulate_mouse_move(at, None, Modifiers::none());
            cx.run_until_parked();
            chat.read_with(cx, |chat, _| chat.url_hover.is_some())
        };
        assert!(over(0, cx), "on the link");
        assert!(!over(1, cx), "on plain text");
        assert!(over(0, cx), "back on the link");
    }
}

#[cfg(test)]
mod pane_tests {
    use super::{ChatWindow, LogPosition, LogSelection, Selection};
    use cayenchat_model::NetworkId;
    use cayenchat_storage::Settings;
    use gpui::{Focusable, TestAppContext};

    #[gpui::test]
    fn the_title_shows_the_member_count_of_a_channel(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "me".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                ],
                false,
                cx,
            );
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            // Without a roster there is no count to show.
            assert!(
                chat.window_title().starts_with("#a @ "),
                "{}",
                chat.window_title()
            );
            let members = ["@op", "alice", "bob"].map(String::from).to_vec();
            chat.state.set_members(NetworkId(1), "#a", members);
            assert!(
                chat.window_title().starts_with("#a (3) @ "),
                "{}",
                chat.window_title()
            );
            chat.state.set_topic(NetworkId(1), "#a", "Welcome");
            assert!(
                chat.window_title().starts_with("#a (3) @ ")
                    && chat.window_title().contains(": Welcome"),
                "{}",
                chat.window_title()
            );
        });
    }

    #[gpui::test]
    fn clicking_members_selects_them_and_the_menu_keeps_a_chosen_group(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;
        use gpui::{Modifiers, MouseButton, point, px};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        // A menu bar above the panes would move the rows clicked below.
        let mut settings = crate::settings_with_channels("#a");
        settings.menu_bar_auto_hide = true;
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "me".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                ],
                false,
                cx,
            );
            let members = ["@op", "alice", "bob", "carol"].map(String::from).to_vec();
            chat.state.set_members(NetworkId(1), "#a", members);
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            cx.notify();
        });
        cx.run_until_parked();
        // The member list is the top of the 240 px column at the right edge;
        // its rows are 22 px tall (a 20 px line and 1 px padding each side).
        let viewport = cx.update(|window, _| window.viewport_size());
        let x = f32::from(viewport.width) - 240. + 30.;
        let row = |index: usize| point(px(x), px(22. * index as f32 + 11.));
        let none = Modifiers::none();
        let chosen = |chat: &gpui::Entity<ChatWindow>, cx: &mut gpui::VisualTestContext| {
            chat.read_with(cx, |chat, _| {
                let channel = chat.state.selected_channel().unwrap();
                chat.member_selection
                    .nicknames(channel.id, &channel.members)
            })
        };
        let click = |cx: &mut gpui::VisualTestContext, at, modifiers| {
            cx.simulate_mouse_move(at, None, modifiers);
            cx.simulate_mouse_down(at, MouseButton::Left, modifiers);
            cx.simulate_mouse_up(at, MouseButton::Left, modifiers);
            cx.run_until_parked();
        };

        click(cx, row(1), none);
        assert_eq!(chosen(&chat, cx), ["alice"]);
        click(cx, row(3), Modifiers::secondary_key());
        assert_eq!(chosen(&chat, cx), ["alice", "carol"]);
        // The range starts at the member clicked last (carol).
        click(cx, row(0), Modifiers::shift());
        assert_eq!(chosen(&chat, cx), ["op", "alice", "bob", "carol"]);
        click(cx, row(2), none);
        assert_eq!(chosen(&chat, cx), ["bob"]);
        click(cx, row(0), Modifiers::shift());
        assert_eq!(chosen(&chat, cx), ["op", "alice", "bob"]);

        // A menu on a chosen member keeps the group; on another member it
        // acts on that member alone.
        click(cx, row(1), none);
        click(cx, row(3), Modifiers::secondary_key());
        let right_click = |cx: &mut gpui::VisualTestContext, at| {
            cx.simulate_mouse_move(at, None, none);
            cx.simulate_mouse_down(at, MouseButton::Right, none);
            cx.run_until_parked();
        };
        right_click(cx, row(3));
        assert_eq!(chosen(&chat, cx), ["alice", "carol"]);
        assert!(chat.read_with(cx, |chat, _| chat.member_menu.is_some()));
        // On one of several chosen members the menu acts on all of them.
        let group = |chat: &gpui::Entity<ChatWindow>, cx: &mut gpui::VisualTestContext| {
            chat.read_with(cx, |chat, _| {
                chat.member_menu.as_ref().map(|menu| menu.group.clone())
            })
        };
        assert_eq!(
            group(&chat, cx),
            Some(vec!["alice".to_owned(), "carol".to_owned()])
        );
        right_click(cx, row(2));
        assert_eq!(chosen(&chat, cx), ["bob"]);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat
                .member_menu
                .as_ref()
                .map(|menu| menu.nickname.clone())),
            Some("bob".to_owned())
        );
        // On a member who was not chosen it is the menu for that one member.
        assert_eq!(group(&chat, cx), Some(Vec::new()));
        // A single chosen member is not a group either.
        click(cx, row(1), none);
        right_click(cx, row(1));
        assert_eq!(group(&chat, cx), Some(Vec::new()));

        // Choosing a mode closes the menu; without a connection it says so
        // and sends nothing.
        click(cx, row(1), none);
        click(cx, row(3), Modifiers::secondary_key());
        right_click(cx, row(3));
        assert_eq!(group(&chat, cx).map(|group| group.len()), Some(2));
        chat.update(cx, |chat, cx| {
            chat.member_modes(cayenchat_irc_core::MemberMode::Op, cx);
            assert!(chat.member_menu.is_none());
            assert_eq!(chat.feedback, Some(chat.i18n.text("not_connected")));
        });
    }

    use gpui::{Context, IntoElement, Window, div, prelude::*, px};

    /// Two rows one above the other, each noting whether it is hovered.
    struct TwoRows {
        hovered: std::rc::Rc<std::cell::Cell<[bool; 2]>>,
        exclusive: bool,
    }

    impl gpui::Render for TwoRows {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let row = |index: usize| {
                let hovered = self.hovered.clone();
                let row = div()
                    .id(("row", index))
                    .h(px(22.))
                    .on_hover(move |on, _, _| {
                        let mut now = hovered.get();
                        now[index] = *on;
                        hovered.set(now);
                    });
                if self.exclusive {
                    super::exclusive_row(row)
                } else {
                    row
                }
            };
            div().size_full().child(row(0)).child(row(1))
        }
    }

    #[gpui::test]
    fn only_one_row_is_hovered_on_the_line_between_two(cx: &mut TestAppContext) {
        use gpui::{Modifiers, point};

        for (exclusive, expected) in [(false, [true, true]), (true, [false, true])] {
            let hovered = std::rc::Rc::new(std::cell::Cell::new([false; 2]));
            let (_view, cx) = cx.add_window_view(|_, _| TwoRows {
                hovered: hovered.clone(),
                exclusive,
            });
            cx.run_until_parked();
            // Rows are 22 px tall: the line at 22 belongs to both.
            cx.simulate_mouse_move(point(px(10.), px(22.)), None, Modifiers::none());
            cx.run_until_parked();
            assert_eq!(hovered.get(), expected, "exclusive: {exclusive}");
            // Inside a row, only that row.
            cx.simulate_mouse_move(point(px(10.), px(11.)), None, Modifiers::none());
            cx.run_until_parked();
            assert_eq!(hovered.get(), [true, false], "exclusive: {exclusive}");
        }
    }

    #[gpui::test]
    fn member_rows_do_not_overlap(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;

        for avatars in [false, true] {
            cx.update(|cx| {
                crate::secrets::install_memory(cx);
                cx.set_global(crate::theme::Theme::new(
                    cayenchat_storage::ThemeMode::Light,
                    gpui::WindowAppearance::Light,
                    &cayenchat_storage::Appearance::default(),
                ));
            });
            let mut settings = crate::settings_with_channels("#a");
            settings.menu_bar_auto_hide = true;
            settings.appearance.user_avatars = avatars;
            let (chat, cx) = cx.add_window_view(|window, cx| {
                ChatWindow::with_settings(settings.clone(), None, window, cx)
            });
            chat.update(cx, |chat, cx| {
                chat.handle_events(
                    NetworkId(1),
                    vec![
                        Event::Registered {
                            nickname: "me".into(),
                        },
                        Event::Joined {
                            channel: "#a".into(),
                        },
                        Event::Names {
                            channel: "#a".into(),
                            // The middle one is too long for the list and
                            // has places where a line could break.
                            users: vec![
                                "@op".into(),
                                "a-very-long-nickname-that-does-not-fit|in[the]narrow-list".into(),
                                "bob".into(),
                            ],
                        },
                    ],
                    false,
                    cx,
                );
                let channel = chat.state.conversations()[0].id;
                chat.state
                    .dispatch(cayenchat_app::Command::SelectChannel(channel));
                cx.notify();
            });
            cx.run_until_parked();
            let rows: Vec<_> = ["member-row-0", "member-row-1", "member-row-2"]
                .into_iter()
                .map(|selector| cx.debug_bounds(selector).expect("row drawn"))
                .collect();
            for pair in rows.windows(2) {
                assert!(
                    pair[0].bottom() <= pair[1].top(),
                    "avatars {avatars}: {:?} overlaps {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[gpui::test]
    fn a_member_who_leaves_stays_unchosen_when_the_nickname_is_taken_again(
        cx: &mut TestAppContext,
    ) {
        use cayenchat_irc_core::Event;
        use gpui::{Modifiers, MouseButton, point, px};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.menu_bar_auto_hide = true;
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let names = |users: &[&str]| Event::Names {
            channel: "#a".into(),
            users: users.iter().map(|user| (*user).to_owned()).collect(),
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "me".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    names(&["@op", "alice", "bob"]),
                ],
                false,
                cx,
            );
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            cx.notify();
        });
        cx.run_until_parked();
        let chosen = |chat: &gpui::Entity<ChatWindow>, cx: &mut gpui::VisualTestContext| {
            chat.read_with(cx, |chat, _| {
                let channel = chat.state.selected_channel().unwrap();
                chat.member_selection
                    .nicknames(channel.id, &channel.members)
            })
        };
        let width = f32::from(cx.update(|window, _| window.viewport_size()).width);
        // Rows are 22 px tall; the second row is alice.
        let alice = point(px(width - 240. + 30.), px(22. + 11.));
        let none = Modifiers::none();
        cx.simulate_mouse_move(alice, None, none);
        cx.simulate_mouse_down(alice, MouseButton::Left, none);
        cx.simulate_mouse_up(alice, MouseButton::Left, none);
        cx.run_until_parked();
        assert_eq!(chosen(&chat, cx), ["alice"]);

        // She quits, and someone else uses the nickname: not chosen.
        for users in [&["@op", "bob"][..], &["@op", "alice", "bob"][..]] {
            chat.update(cx, |chat, cx| {
                chat.handle_events(NetworkId(1), vec![names(users)], false, cx)
            });
            cx.run_until_parked();
        }
        assert!(chosen(&chat, cx).is_empty());

        // The same while a group menu is open: it keeps the nicknames it was
        // opened with, but what it acts on is who is still chosen when an item
        // is clicked.
        let bob = point(px(width - 240. + 30.), px(22. * 2. + 11.));
        cx.simulate_mouse_move(alice, None, none);
        cx.simulate_mouse_down(alice, MouseButton::Left, none);
        cx.simulate_mouse_up(alice, MouseButton::Left, none);
        let secondary = Modifiers::secondary_key();
        cx.simulate_mouse_move(bob, None, secondary);
        cx.simulate_mouse_down(bob, MouseButton::Left, secondary);
        cx.simulate_mouse_up(bob, MouseButton::Left, secondary);
        cx.simulate_mouse_move(bob, None, none);
        cx.simulate_mouse_down(bob, MouseButton::Right, none);
        cx.run_until_parked();
        let (menu_group, now) = chat.read_with(cx, |chat, _| {
            let menu = chat.member_menu.as_ref().expect("group menu open");
            (menu.group.clone(), chat.menu_group_now(menu))
        });
        assert_eq!(menu_group, ["alice", "bob"]);
        assert_eq!(now, ["alice", "bob"]);
        for users in [&["@op", "bob"][..], &["@op", "alice", "bob"][..]] {
            chat.update(cx, |chat, cx| {
                chat.handle_events(NetworkId(1), vec![names(users)], false, cx)
            });
            cx.run_until_parked();
        }
        let (menu_group, now) = chat.read_with(cx, |chat, _| {
            let menu = chat.member_menu.as_ref().expect("the menu stays open");
            (menu.group.clone(), chat.menu_group_now(menu))
        });
        assert_eq!(menu_group, ["alice", "bob"], "opened with both");
        assert_eq!(now, ["bob"], "the newcomer named alice is not acted on");
    }

    #[gpui::test]
    fn the_color_picker_and_palette_edit_the_color_fields(cx: &mut TestAppContext) {
        use crate::color_picker::{ColorChanged, format_hex};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let owner = cx
            .add_window(|window, cx| ChatWindow::with_settings(settings.clone(), None, window, cx));
        let (form, cx) = cx.add_window_view(|window, cx| {
            let mut form = super::SettingsWindow::new(owner, settings.clone(), window, cx);
            form.tab = super::SettingsTab::Appearance;
            form
        });
        let field = form.read_with(cx, |form, _| form.settings.main_log_background.clone());
        let text =
            |cx: &mut gpui::VisualTestContext| field.read_with(cx, |f, _| f.text().to_owned());
        let before = text(cx);

        // Opening the picker for a field starts from that field's color;
        // opening it again closes it.
        form.update(cx, |form, cx| form.toggle_color_picker(&field, cx));
        cx.run_until_parked();
        let picker = form
            .read_with(cx, |form, _| {
                form.color_picker.as_ref().map(|open| open.picker.clone())
            })
            .expect("open");
        assert_eq!(
            format_hex(picker.read_with(cx, |p, _| p.color())),
            before.to_uppercase()
        );

        // Picking writes the color into the field.
        picker.update(cx, |_, cx| cx.emit(ColorChanged(0xAA0000)));
        cx.run_until_parked();
        assert_eq!(text(cx), "#AA0000");
        // Typing in the field moves the picker.
        field.update(cx, |field, cx| field.set_text("#112233", cx));
        cx.run_until_parked();
        assert_eq!(picker.read_with(cx, |p, _| p.color()), 0x112233);

        // The palette keeps colors, applies one, and forgets one.
        let palette = |form: &gpui::Entity<super::SettingsWindow>,
                       cx: &mut gpui::VisualTestContext| {
            form.read_with(cx, |form, _| {
                form.settings.values.appearance.saved_colors.clone()
            })
        };
        form.update(cx, |form, cx| {
            let _ = form.settings.values.appearance.save_color("#112233");
            let _ = form.settings.values.appearance.save_color("#FFEE00");
            form.apply_palette_color("#FFEE00", cx);
        });
        cx.run_until_parked();
        assert_eq!(palette(&form, cx), ["#112233", "#FFEE00"]);
        assert_eq!(text(cx), "#FFEE00");
        assert_eq!(picker.read_with(cx, |p, _| p.color()), 0xFFEE00);
        form.update(cx, |form, _| {
            form.settings
                .values
                .appearance
                .remove_saved_color("#112233")
        });
        assert_eq!(palette(&form, cx), ["#FFEE00"]);

        // The field is what gets saved, palette included.
        let saved = form
            .update(cx, |form, cx| form.settings.snapshot(cx))
            .unwrap();
        assert_eq!(saved.appearance.main_log_background, "#FFEE00");
        assert_eq!(saved.appearance.saved_colors, ["#FFEE00"]);

        // The same swatch closes it; another field's swatch moves it.
        let other = form.read_with(cx, |form, _| form.settings.dark_main_log_background.clone());
        form.update(cx, |form, cx| form.toggle_color_picker(&other, cx));
        assert!(
            form.read_with(cx, |form, _| form.color_picker.as_ref().unwrap().field
                == other)
        );
        form.update(cx, |form, cx| form.toggle_color_picker(&other, cx));
        assert!(form.read_with(cx, |form, _| form.color_picker.is_none()));

        // Leaving the tab closes an open picker; coming back finds it closed.
        form.update(cx, |form, cx| form.toggle_color_picker(&field, cx));
        assert!(form.read_with(cx, |form, _| form.color_picker.is_some()));
        form.update(cx, |form, _| form.show_tab(super::SettingsTab::Connection));
        assert!(form.read_with(cx, |form, _| form.color_picker.is_none()));
        // The picker opened here has no field of its own (the row has one),
        // and still follows what is typed in the row.
        form.update(cx, |form, cx| form.toggle_color_picker(&field, cx));
        let picker = form
            .read_with(cx, |form, _| {
                form.color_picker.as_ref().map(|open| open.picker.clone())
            })
            .expect("open");
        assert!(!picker.read_with(cx, |picker, _| picker.shows_hex_field()));
        field.update(cx, |field, cx| field.set_text("#123456", cx));
        cx.run_until_parked();
        assert_eq!(picker.read_with(cx, |picker, _| picker.color()), 0x123456);
    }

    #[gpui::test]
    fn dragging_the_splitters_resizes_the_right_column_within_limits(cx: &mut TestAppContext) {
        use super::{LEFT_MIN_WIDTH, RIGHT_PANE_MIN_HEIGHT, RIGHT_WIDTH_MIN};
        use gpui::{Modifiers, MouseButton, point, px};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        cx.run_until_parked();
        let none = Modifiers::none();
        let viewport = cx.update(|window, _| window.viewport_size());
        let width = f32::from(viewport.width);
        let drag = |cx: &mut gpui::VisualTestContext, from: (f32, f32), steps: &[(f32, f32)]| {
            let at = |(x, y): (f32, f32)| point(px(x), px(y));
            cx.simulate_mouse_move(at(from), None, none);
            cx.simulate_mouse_down(at(from), MouseButton::Left, none);
            for step in steps {
                cx.simulate_mouse_move(at(*step), MouseButton::Left, none);
            }
            cx.run_until_parked();
            cx.simulate_mouse_up(at(*steps.last().unwrap()), MouseButton::Left, none);
        };

        // The handle sits just left of the 240 px column; pulling it left
        // widens the column, and the window keeps room for the left column.
        let handle_x = width - 240. - 2.;
        drag(
            cx,
            (handle_x, 300.),
            &[(handle_x - 10., 300.), (handle_x - 60., 300.)],
        );
        let widened = chat.read_with(cx, |chat, _| chat.right_width);
        assert!((295.0..=305.0).contains(&widened), "{widened}");
        let x = width - widened - 2.;
        drag(cx, (x, 300.), &[(x - 10., 300.), (-500., 300.)]);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.right_width),
            width - LEFT_MIN_WIDTH
        );
        let x = width - (width - LEFT_MIN_WIDTH) - 2.;
        drag(cx, (x, 300.), &[(x + 10., 300.), (width + 500., 300.)]);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.right_width),
            RIGHT_WIDTH_MIN
        );

        // The member list and channel tree share the column; the boundary
        // starts in the middle and cannot squeeze either below its minimum.
        assert_eq!(chat.read_with(cx, |chat, _| chat.members_height), None);
        let column = chat
            .read_with(cx, |chat, _| chat.right_column_bounds.get())
            .expect("the column was laid out");
        let (left, top, height) = (
            f32::from(column.origin.x) + 20.,
            f32::from(column.origin.y),
            f32::from(column.size.height),
        );
        let y = top + height / 2. + 2.;
        drag(cx, (left, y), &[(left, y + 10.), (left, y + 40.)]);
        let members = chat
            .read_with(cx, |chat, _| chat.members_height)
            .expect("dragged");
        assert!(
            (height / 2. + 33.0..=height / 2. + 43.0).contains(&members),
            "{members}"
        );
        let y = top + members + 2.;
        // The list really is that tall: the handle sits right below it.
        let handle = cx.debug_bounds("members-split").expect("handle drawn");
        assert!(
            (f32::from(handle.origin.y) - (top + members)).abs() < 1.,
            "{handle:?} for a list of {members}"
        );
        drag(cx, (left, y), &[(left, y - 10.), (left, -300.)]);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.members_height),
            Some(RIGHT_PANE_MIN_HEIGHT)
        );
    }

    #[gpui::test]
    fn the_window_layout_is_saved_after_changes_and_applied_at_startup(cx: &mut TestAppContext) {
        use super::{DEFAULT_LOG_SPLIT, DEFAULT_RIGHT_WIDTH, LAYOUT_SAVE_DELAY, LOG_SPLIT_LIMITS};
        use cayenchat_storage::layout::{Layout, load_layout_from};
        use gpui::{Modifiers, MouseButton, point, px};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        cx.run_until_parked();
        // Tests never write the user's real layout file: the path is given
        // here, and without one nothing is written.
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("window.json");
        // Saving needs no window at hand, as when the application quits.
        let save =
            |cx: &mut gpui::VisualTestContext| chat.update(cx, |chat, _| chat.save_layout_now());
        save(cx);
        assert!(!file.exists(), "no path, no file");

        // Applying a saved layout takes over the pane sizes; a split from
        // damage is held within limits.
        chat.update(cx, |chat, _| {
            chat.layout_file = Some(file.clone());
            chat.apply_layout(&Layout {
                right_width: Some(300.),
                members_height: Some(200.),
                log_split: Some(5.0),
                ..Layout::default()
            });
            assert_eq!(chat.right_width, 300.);
            assert_eq!(chat.members_height, Some(200.));
            assert_eq!(chat.log_split, LOG_SPLIT_LIMITS.1);
            chat.apply_layout(&Layout::default());
            assert_eq!(chat.right_width, 300., "nothing saved leaves it alone");
            chat.right_width = DEFAULT_RIGHT_WIDTH;
            chat.members_height = None;
            chat.log_split = DEFAULT_LOG_SPLIT;
        });

        // Saving records the window and the pane sizes.
        save(cx);
        let saved = load_layout_from(&file);
        assert!(saved.window.is_some());
        assert_eq!(saved.right_width, Some(DEFAULT_RIGHT_WIDTH));
        assert_eq!(saved.members_height, None);
        assert_eq!(saved.log_split, Some(DEFAULT_LOG_SPLIT));

        // A resize is noted, so a later write without the window has it.
        cx.simulate_resize(gpui::size(px(1111.), px(777.)));
        cx.run_until_parked();
        save(cx);
        let resized = load_layout_from(&file).window.expect("window saved");
        assert_eq!((resized.width, resized.height), (1111., 777.));

        // Switched off, nothing is written.
        std::fs::remove_file(&file).unwrap();
        chat.update(cx, |chat, _| chat.restore_layout = false);
        save(cx);
        assert!(!file.exists());
        chat.update(cx, |chat, _| chat.restore_layout = true);

        // Dragging the splitter writes it shortly after the last move.
        let none = Modifiers::none();
        let width = f32::from(cx.update(|window, _| window.viewport_size()).width);
        let handle = point(px(width - DEFAULT_RIGHT_WIDTH - 2.), px(300.));
        cx.simulate_mouse_move(handle, None, none);
        cx.simulate_mouse_down(handle, MouseButton::Left, none);
        for step in [10., 60.] {
            cx.simulate_mouse_move(
                point(handle.x - px(step), handle.y),
                MouseButton::Left,
                none,
            );
        }
        cx.run_until_parked();
        cx.simulate_mouse_up(handle, MouseButton::Left, none);
        assert!(!file.exists(), "not before the pause is over");
        cx.executor().advance_clock(LAYOUT_SAVE_DELAY * 2);
        cx.run_until_parked();
        let dragged = load_layout_from(&file);
        let widened = dragged.right_width.expect("saved after the drag");
        assert!((295.0..=305.0).contains(&widened), "{widened}");
    }

    #[gpui::test]
    fn server_prompts_open_where_the_menu_was_and_need_a_connection(cx: &mut TestAppContext) {
        use super::{MemberPromptKind, ServerMenu};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.servers[0].nickname = "alice".into();
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        for kind in [MemberPromptKind::Join, MemberPromptKind::Nick] {
            chat.update_in(cx, |chat, window, cx| {
                chat.server_menu = Some(ServerMenu {
                    position: gpui::point(gpui::px(10.), gpui::px(10.)),
                    network: NetworkId(1),
                });
                chat.open_server_prompt(kind, window, cx);
                assert!(chat.server_menu.is_none());
                let prompt = chat.member_prompt.as_ref().expect("prompt opened");
                assert_eq!(prompt.network, NetworkId(1));
                // A new nickname starts from the current one; a channel from nothing.
                let initial = prompt.input.read(cx).text().to_owned();
                match kind {
                    MemberPromptKind::Nick => {
                        let saved = chat.saved.servers[0].nickname.clone();
                        assert!(!saved.is_empty());
                        assert_eq!(initial, saved);
                    }
                    _ => assert_eq!(initial, ""),
                }
                let prompt = chat.member_prompt.as_ref().expect("prompt opened");
                prompt
                    .input
                    .update(cx, |input, cx| input.set_text("#b", cx));
                chat.submit_member_prompt(window, cx);
                // Not connected: kept open with the reason.
                assert!(chat.member_prompt.is_some());
                assert_eq!(chat.feedback, Some(chat.i18n.text("not_connected")));
            });
        }
    }

    #[gpui::test]
    fn ircv3_choices_are_per_server_and_wait_for_the_next_connection(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Ircv3Options;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.servers[0].nickname = "me".into();
        settings.servers[0].username = "me".into();
        settings.add_server("irc.example.org");
        settings.servers[1].nickname = "me".into();
        settings.servers[1].username = "me".into();
        settings.servers[1].ircv3.server_time = true;
        settings.servers[1].ircv3.batch = true;
        settings.servers[1].ircv3.peer_avatars = true;
        let config = |settings: &Settings, index: usize| {
            crate::connection_config(
                &settings.servers[index],
                cayenchat_storage::Language::English,
                None,
                None,
            )
            .unwrap()
        };
        // Avatar metadata is always asked for; the rest follows the options.
        assert_eq!(
            config(&settings, 0).ircv3,
            Ircv3Options {
                metadata: true,
                ..Ircv3Options::default()
            }
        );
        assert_eq!(
            config(&settings, 1).ircv3,
            Ircv3Options {
                message_tags: false,
                server_time: true,
                batch: true,
                metadata: true,
                peer_avatars: true,
                chathistory: false,
                confirmed_sending: false,
                accounts: false,
            }
        );
        // Peer avatars alone share nothing and leave the realname unmarked.
        assert_eq!(config(&settings, 1).shared_avatar, None);
        assert!(!config(&settings, 1).advertises_avatar());
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let (first, second) = (NetworkId(1), NetworkId(2));
        chat.update(cx, |chat, cx| {
            // Both servers were used in this run (as after a connection attempt).
            chat.sessions.get_mut(&first).unwrap().active_config = Some(config(&settings, 0));
            chat.sessions.get_mut(&second).unwrap().active_config = Some(config(&settings, 1));
            let generation = chat.sessions[&first].generation;
            let mut next = settings.clone();
            next.servers[0].ircv3.message_tags = true;
            chat.apply_servers(next, cx);
            let ircv3 = |network| {
                chat.sessions[&network]
                    .active_config
                    .as_ref()
                    .unwrap()
                    .ircv3
            };
            assert_eq!(
                ircv3(first),
                Ircv3Options {
                    message_tags: true,
                    server_time: false,
                    batch: false,
                    metadata: true,
                    peer_avatars: false,
                    chathistory: false,
                    confirmed_sending: false,
                    accounts: false,
                },
                "reconnects use the new choice; batch stays off here"
            );
            assert_eq!(
                ircv3(second),
                config(&settings, 1).ircv3,
                "other server unchanged"
            );
            assert_eq!(chat.sessions[&first].generation, generation, "no reconnect");
            assert!(chat.sessions[&first].irc.is_none());
        });
    }

    #[gpui::test]
    fn disconnect_is_offered_only_while_there_is_something_to_stop(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        chat.update(cx, |chat, cx| {
            let network = NetworkId(1);
            assert!(!chat.can_disconnect(network), "never connected");
            // A failed connection waiting to retry can still be stopped.
            chat.sessions.get_mut(&network).unwrap().retry_pending = true;
            assert!(chat.can_disconnect(network));
            chat.disconnect(network, cx);
            let session = &chat.sessions[&network];
            assert!(session.manual_disconnect && !session.retry_pending);
            assert!(!chat.can_disconnect(network));
        });
    }

    #[gpui::test]
    fn repeated_message_ids_are_shown_and_notified_once(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a,#b");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_790_553_511_123);
        let mention = |msgid: &str, replayed| Event::ChannelMessage {
            channel: "#b".into(),
            sender: "bob".into(),
            text: "alice: look".into(),
            notice: false,
            mentioned: true,
            server_time: Some(stamp),
            msgid: Some(msgid.into()),
            account: None,
            replayed,
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "alice".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    Event::Joined {
                        channel: "#b".into(),
                    },
                    mention("m1", false),
                    // A bouncer or history request delivering it again.
                    mention("m1", true),
                    mention("m1", false),
                    mention("m2", false),
                ],
                false,
                cx,
            );
            assert_eq!(chat.notifier.shown.len(), 2);
            let b = &chat.state.conversations()[1];
            assert_eq!(b.messages.len(), 2);
            let ids: Vec<_> = b
                .messages
                .iter()
                .map(|m| m.native_id.as_ref().map(|id| id.as_str()))
                .collect();
            assert_eq!(ids, [Some("m1"), Some("m2")]);
            assert!(b.messages.iter().all(|m| {
                m.timestamp
                    .is_some_and(|t| t.as_millis() == 1_790_553_511_123)
            }));
        });
    }

    #[gpui::test]
    fn requested_history_is_inserted_quietly_before_newer_lines(cx: &mut TestAppContext) {
        use cayenchat_irc_core::{Event, HistoryMessage};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a,#b");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let old = |text: &str, msgid: &str| HistoryMessage {
            sender: "bob".into(),
            text: text.into(),
            notice: false,
            server_time: Some(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_550_000),
            ),
            msgid: Some(msgid.into()),
            account: None,
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "alice".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    Event::Joined {
                        channel: "#b".into(),
                    },
                    Event::HistoryRequested {
                        channel: "#b".into(),
                        resumed: false,
                    },
                    Event::ChannelMessage {
                        channel: "#b".into(),
                        sender: "bob".into(),
                        text: "alice: live".into(),
                        notice: false,
                        mentioned: true,
                        server_time: None,
                        msgid: Some("live1".into()),
                        account: None,
                        replayed: false,
                    },
                    Event::ChannelHistory {
                        channel: "#b".into(),
                        incomplete: false,
                        messages: vec![
                            old("alice: from yesterday", "old1"),
                            HistoryMessage {
                                notice: true,
                                ..old("maintenance", "old2")
                            },
                            old("alice: live", "live1"),
                        ],
                    },
                ],
                false,
                cx,
            );
            assert_eq!(chat.notifier.shown.len(), 1, "only the live mention");
            let b = &chat.state.conversations()[1];
            let texts: Vec<_> = b.messages.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(
                texts,
                [
                    "alice: from yesterday",
                    "[NOTICE] maintenance",
                    "alice: live"
                ]
            );
            assert!(b.messages[0].is_history());
            assert!(
                chat.highlight_ranges(NetworkId(1), &b.messages[0])
                    .is_empty()
            );
            // The combined log shows the live line only.
            let rows = super::newest_lines(chat.state.conversations(), None, 10);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, b.messages[2].sequence);
        });
    }

    #[gpui::test]
    fn discovered_direct_messages_open_quietly_and_overlap_is_dropped(cx: &mut TestAppContext) {
        use cayenchat_irc_core::{Event, HistoryMessage};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let line = |sender: &str, text: &str, secs: u64, msgid: &str| HistoryMessage {
            sender: sender.into(),
            text: text.into(),
            notice: false,
            server_time: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
            msgid: Some(msgid.into()),
            account: None,
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "alice".into(),
                    },
                    // A conversation we already know, with one live line.
                    Event::PrivateMessage {
                        sender: "Bob".into(),
                        text: "live".into(),
                        notice: false,
                        server_time: Some(
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_550_100),
                        ),
                        msgid: Some("d2".into()),
                        account: None,
                        replayed: false,
                    },
                    // Discovery: a new peer, and Bob again in another case.
                    Event::HistoryRequested {
                        channel: "carol".into(),
                        resumed: false,
                    },
                    Event::ChannelHistory {
                        channel: "carol".into(),
                        incomplete: false,
                        messages: vec![
                            line("carol", "call me", 1_790_550_000, "c1"),
                            line("alice", "ok", 1_790_550_050, "c2"),
                        ],
                    },
                    Event::HistoryRequested {
                        channel: "BOB".into(),
                        resumed: false,
                    },
                    Event::ChannelHistory {
                        channel: "BOB".into(),
                        incomplete: false,
                        messages: vec![
                            line("Bob", "missed", 1_790_550_000, "d1"),
                            line("Bob", "live", 1_790_550_100, "d2"),
                        ],
                    },
                ],
                false,
                cx,
            );
            let key = |nick: &str| cayenchat_irc_core::text::nickname_key(nick);
            let carol = chat.state.private_id(NetworkId(1), &key("carol")).unwrap();
            let bob = chat.state.private_id(NetworkId(1), &key("bob")).unwrap();
            let texts = |id| -> Vec<String> {
                chat.state
                    .conversations()
                    .iter()
                    .find(|c| c.id == id)
                    .unwrap()
                    .messages
                    .iter()
                    .map(|m| m.text.clone())
                    .collect()
            };
            assert_eq!(texts(carol), ["call me", "ok"]);
            // No second Bob conversation, and the repeated live line is dropped.
            assert_eq!(
                chat.state
                    .conversations()
                    .iter()
                    .filter(|c| c.is_private())
                    .count(),
                2
            );
            // Logs keep arrival order: the reserved block sits where the
            // request was made, after the live line that was already there.
            assert_eq!(texts(bob), ["live", "missed"]);
            // History is context: no unread mark, notification or highlight.
            assert!(!chat.state.is_unread(carol));
            assert_eq!(chat.notifier.shown.len(), 1, "only Bob's live line");
            let messages = &chat
                .state
                .conversations()
                .iter()
                .find(|c| c.id == carol)
                .unwrap()
                .messages;
            assert!(messages.iter().all(|m| m.is_history()));
        });
    }

    #[gpui::test]
    fn scrolling_to_the_top_loads_one_older_page_quietly(cx: &mut TestAppContext) {
        use cayenchat_irc_core::{
            Connection, ConnectionConfig, Event, HistoryMessage, OlderHistoryStatus,
        };

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a,#b");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let at = |secs: u64| Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
        let line = |text: &str, secs: u64, msgid: &str| HistoryMessage {
            sender: "bob".into(),
            text: text.into(),
            notice: false,
            server_time: at(secs),
            msgid: Some(msgid.into()),
            account: None,
        };
        // A connection to a local listener that never registers: requests
        // can be queued on it, nothing reaches a server.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
        config.port = listener.local_addr().unwrap().port();
        config.use_tls = false;
        let connection = Connection::connect(config).unwrap();
        let (b, main_rows) = chat.update(cx, |chat, cx| {
            chat.sessions.get_mut(&NetworkId(1)).unwrap().irc = Some(connection);
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "alice".into(),
                    },
                    Event::HistoryAvailable(true),
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    Event::Joined {
                        channel: "#b".into(),
                    },
                    Event::HistoryRequested {
                        channel: "#b".into(),
                        resumed: false,
                    },
                    Event::ChannelHistory {
                        channel: "#b".into(),
                        incomplete: false,
                        messages: vec![line("recent", 1_790_550_000, "r1")],
                    },
                ],
                false,
                cx,
            );
            let b = chat.state.channel_id(NetworkId(1), "#b").unwrap();
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(b));
            chat.sync_log_lists();
            (
                b,
                chat.main_lists[&Selection::Channel(b)].state.item_count(),
            )
        });
        cx.run_until_parked();

        let request = chat.update(cx, |chat, _| {
            // Far from the top, or another conversation: nothing is asked.
            chat.scrolled_main_log(b, 40);
            let a = chat.state.channel_id(NetworkId(1), "#a").unwrap();
            chat.scrolled_main_log(a, 0);
            assert!(chat.state.older_history_in_flight(b).is_none());
            assert!(chat.state.older_history_in_flight(a).is_none());
            // Scrolled to the top: one page is asked for, and only one.
            chat.scrolled_main_log(b, 0);
            let request = chat.state.older_history_in_flight(b).expect("asked");
            chat.scrolled_main_log(b, 1);
            assert_eq!(chat.state.older_history_in_flight(b), Some(request));
            request
        });
        chat.update(cx, |chat, cx| {
            // The user has selected text in the line that is on screen.
            chat.log_selection = Some(LogSelection {
                channel: b,
                anchor: LogPosition { row: 0, byte: 0 },
                cursor: LogPosition { row: 0, byte: 3 },
            });
            let before = chat.notifier.shown.len();
            chat.handle_events(
                NetworkId(1),
                vec![
                    // A live line while the page is on its way.
                    Event::ChannelMessage {
                        channel: "#b".into(),
                        sender: "carol".into(),
                        text: "live".into(),
                        notice: false,
                        mentioned: false,
                        server_time: at(1_790_560_000),
                        msgid: Some("l1".into()),
                        account: None,
                        replayed: false,
                    },
                    Event::OlderChannelHistory {
                        channel: "#b".into(),
                        request,
                        messages: vec![
                            line("alice: older mention", 1_790_540_000, "o1"),
                            line("recent", 1_790_550_000, "r1"),
                        ],
                        status: OlderHistoryStatus::Beginning,
                    },
                ],
                false,
                cx,
            );
            assert_eq!(chat.notifier.shown.len(), before, "history never notifies");
            let texts: Vec<_> = chat.state.conversations()[1]
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect();
            assert_eq!(texts, ["alice: older mention", "recent", "live"]);
            let older = &chat.state.conversations()[1].messages[0];
            assert!(older.is_history());
            assert!(chat.highlight_ranges(NetworkId(1), older).is_empty());
            assert!(!chat.state.is_highlighted(b));
            let selection = chat.log_selection.unwrap();
            assert_eq!(
                (selection.anchor.row, selection.cursor.row),
                (1, 1),
                "still on \"recent\""
            );
            chat.sync_log_lists();
            assert_eq!(
                chat.main_lists[&Selection::Channel(b)].state.item_count(),
                main_rows + 2
            );
            // The beginning was reached: scrolling asks nothing more.
            chat.scrolled_main_log(b, 0);
            assert!(chat.state.older_history_in_flight(b).is_none());
            // A page answering an old request is ignored.
            chat.handle_events(
                NetworkId(1),
                vec![Event::OlderChannelHistory {
                    channel: "#b".into(),
                    request,
                    messages: vec![line("stale", 1_790_530_000, "s1")],
                    status: OlderHistoryStatus::More,
                }],
                false,
                cx,
            );
            assert_eq!(chat.state.conversations()[1].messages.len(), 3);
        });
    }

    #[gpui::test]
    fn sent_messages_are_confirmed_in_place_and_unconfirmed_ones_are_marked(
        cx: &mut TestAppContext,
    ) {
        use cayenchat_irc_core::Event;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let accepted = |local_id, text: &str| Event::OutgoingAccepted {
            local_id,
            channel: "#a".into(),
            text: text.into(),
            notice: false,
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "me".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    // Shown at once, before any confirmation.
                    accepted(Some(1), "hello  world"),
                    accepted(Some(2), "rejected"),
                    accepted(Some(3), "never answered"),
                    accepted(None, "no echo-message here"),
                ],
                false,
                cx,
            );
            let id = chat.state.channel_id(NetworkId(1), "#a").unwrap();
            let lines = |chat: &ChatWindow| {
                chat.state
                    .conversations()
                    .iter()
                    .find(|c| c.id == id)
                    .unwrap()
                    .messages
                    .iter()
                    .map(|m| (m.text.clone(), m.delivery_failed))
                    .collect::<Vec<_>>()
            };
            assert_eq!(chat.sessions[&NetworkId(1)].pending_sends.len(), 3);
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::OutgoingConfirmed {
                        local_id: 1,
                        text: Some("hello world".into()),
                        msgid: Some("m1".into()),
                        server_time: None,
                    },
                    Event::OutgoingFailed {
                        local_id: 2,
                        reason: "Cannot send to channel".into(),
                    },
                ],
                false,
                cx,
            );
            // Replaced in place, not added again.
            assert_eq!(
                lines(chat)
                    .iter()
                    .filter(|(text, _)| text.starts_with("hello"))
                    .collect::<Vec<_>>(),
                [&("hello world".to_owned(), false)]
            );
            assert!(lines(chat).contains(&("rejected".to_owned(), true)));
            assert_eq!(chat.sessions[&NetworkId(1)].pending_sends.len(), 1);
            // The rejection is also said in the server log.
            assert!(
                chat.state
                    .server_messages(NetworkId(1))
                    .iter()
                    .any(|m| m.text.contains("Cannot send to channel"))
            );
            // A link that ends with a message unconfirmed marks it.
            chat.handle_events(
                NetworkId(1),
                vec![Event::Disconnected("gone".into())],
                false,
                cx,
            );
            assert!(lines(chat).contains(&("never answered".to_owned(), true)));
            assert!(lines(chat).contains(&("no echo-message here".to_owned(), false)));
            assert!(chat.sessions[&NetworkId(1)].pending_sends.is_empty());
        });
    }

    #[gpui::test]
    fn tracked_accounts_fill_whois_and_are_forgotten_with_the_connection(cx: &mut TestAppContext) {
        use cayenchat_irc_core::{Event, WhoisInfo};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "me".into(),
                    },
                    Event::UserAccount {
                        nickname: "Bob".into(),
                        account: Some("bob-acct".into()),
                        realname: Some("Bob Builder".into()),
                    },
                    Event::UserAccount {
                        nickname: "Eve".into(),
                        account: None,
                        realname: None,
                    },
                    Event::UserAccountForgotten {
                        nickname: "EVE".into(),
                    },
                ],
                false,
                cx,
            );
            let accounts = |chat: &ChatWindow| chat.sessions[&NetworkId(1)].user_accounts.clone();
            assert_eq!(accounts(chat).len(), 1, "casemapped removal");
            let tracked = accounts(chat);
            let mut info = WhoisInfo {
                nickname: "bob".into(),
                username: Some("u".into()),
                ..WhoisInfo::default()
            };
            super::complete_whois(&mut info, &tracked);
            assert_eq!(info.account.as_deref(), Some("bob-acct"));
            assert_eq!(info.realname.as_deref(), Some("Bob Builder"));
            let mut said = WhoisInfo {
                nickname: "BOB".into(),
                account: Some("server-acct".into()),
                ..WhoisInfo::default()
            };
            super::complete_whois(&mut said, &tracked);
            assert_eq!(said.account.as_deref(), Some("server-acct"));
            chat.handle_events(
                NetworkId(1),
                vec![Event::Disconnected("gone".into())],
                false,
                cx,
            );
            assert!(accounts(chat).is_empty());
        });
    }

    #[gpui::test]
    fn a_reconnect_recovers_missed_lines_where_the_log_was_cut_off(cx: &mut TestAppContext) {
        use cayenchat_irc_core::{ConnectionConfig, Event, HistoryMessage};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a,#b");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let at = |secs: u64| Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
        let message = |text: &str, secs: u64, msgid: &str| Event::ChannelMessage {
            channel: "#a".into(),
            sender: "bob".into(),
            text: text.into(),
            notice: false,
            mentioned: text.starts_with("alice"),
            server_time: at(secs),
            msgid: Some(msgid.into()),
            account: None,
            replayed: false,
        };
        let history = |text: &str, secs: u64, msgid: &str| HistoryMessage {
            sender: "bob".into(),
            text: text.into(),
            notice: false,
            server_time: at(secs),
            msgid: Some(msgid.into()),
            account: None,
        };
        let session = |events: Vec<Event>| {
            let mut all = vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::HistoryAvailable(true),
                Event::Joined {
                    channel: "#a".into(),
                },
            ];
            all.extend(events);
            all
        };
        chat.update(cx, |chat, cx| {
            chat.sessions.get_mut(&NetworkId(1)).unwrap().active_config = Some(
                ConnectionConfig::tls("irc.example".into(), "alice".into(), vec!["#a".into()]),
            );
            chat.handle_events(
                NetworkId(1),
                session(vec![
                    message("A", 1_790_550_000, "m1"),
                    Event::Disconnected("connection reset".into()),
                ]),
                false,
                cx,
            );
            // The next connection resumes #a after its last line.
            let config = chat.reconnect_config(NetworkId(1)).unwrap();
            assert_eq!(config.resume_history.len(), 1);
            assert_eq!(config.resume_history[0].channel, "#a");
            assert_eq!(config.resume_history[0].after.msgid.as_deref(), Some("m1"));
            assert_eq!(config.resume_history[0].after.time, at(1_790_550_000));
            // The saved configuration itself is not changed.
            assert!(
                chat.sessions[&NetworkId(1)]
                    .active_config
                    .as_ref()
                    .unwrap()
                    .resume_history
                    .is_empty()
            );
            let shown = chat.notifier.shown.len();
            chat.handle_events(
                NetworkId(1),
                session(vec![
                    Event::HistoryRequested {
                        channel: "#a".into(),
                        resumed: true,
                    },
                    message("D", 1_790_560_000, "m4"),
                    Event::ChannelHistory {
                        channel: "#a".into(),
                        messages: vec![
                            history("alice: while you were away", 1_790_555_000, "m3"),
                            history("D", 1_790_560_000, "m4"),
                        ],
                        incomplete: true,
                    },
                ]),
                false,
                cx,
            );
            assert_eq!(
                chat.notifier.shown.len(),
                shown,
                "recovered lines never notify"
            );
            let texts: Vec<_> = chat.state.conversations()[0]
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect();
            assert_eq!(
                texts,
                [
                    "A",
                    super::HISTORY_GAP_NOTE,
                    "alice: while you were away",
                    "D"
                ]
            );
            assert!(chat.state.history_resume(NetworkId(1)).is_empty());
            assert!(
                chat.reconnect_config(NetworkId(1))
                    .unwrap()
                    .resume_history
                    .is_empty()
            );
        });
    }

    #[gpui::test]
    fn private_messages_get_their_own_conversations(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.language = cayenchat_storage::Language::English;
        settings.add_server("two.example");
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let pm = |sender: &str, text: &str, notice, replayed| Event::PrivateMessage {
            sender: sender.into(),
            text: text.into(),
            notice,
            server_time: None,
            msgid: None,
            account: None,
            replayed,
        };
        chat.update(cx, |chat, cx| {
            let (one, two) = (chat.state.networks()[0].id, chat.state.networks()[1].id);
            for network in [one, two] {
                chat.handle_events(
                    network,
                    vec![Event::Registered {
                        nickname: "alice".into(),
                    }],
                    false,
                    cx,
                );
            }
            chat.handle_events(
                one,
                vec![
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    pm("Bob", "hello", false, false),
                    pm("bob", "again", false, false),
                    // Services' notices stay in the server log.
                    pm("NickServ", "This nickname is registered", true, false),
                    pm("carol", "old", false, true),
                    Event::OutgoingAccepted {
                        local_id: None,
                        channel: "BOB".into(),
                        text: "hi bob".into(),
                        notice: false,
                    },
                    Event::OwnPrivateMessage {
                        target: "bob".into(),
                        text: "from my phone".into(),
                        notice: false,
                        server_time: None,
                        msgid: None,
                        replayed: false,
                    },
                    // A notice from someone we already talk to joins them.
                    pm("bob", "psst", true, false),
                    Event::ChannelMessage {
                        channel: "#a".into(),
                        sender: "bob".into(),
                        text: "in the channel".into(),
                        notice: false,
                        mentioned: false,
                        server_time: None,
                        msgid: None,
                        account: None,
                        replayed: false,
                    },
                ],
                false,
                cx,
            );
            // The same nickname on the other server is someone else.
            chat.handle_events(
                two,
                vec![pm("bob", "other server", false, false)],
                false,
                cx,
            );

            let conversations = chat.state.conversations();
            let names: Vec<_> = conversations
                .iter()
                .map(|c| (c.network, c.name.as_str(), c.is_private()))
                .collect();
            assert_eq!(
                names,
                [
                    (one, "#a", false),
                    (one, "bob", true),
                    (one, "carol", true),
                    (two, "bob", true),
                ]
            );
            let texts = |index: usize| -> Vec<(String, String)> {
                chat.state.conversations()[index]
                    .messages
                    .iter()
                    .map(|m| (m.sender.clone(), m.text.clone()))
                    .collect()
            };
            assert_eq!(texts(0), [("bob".into(), "in the channel".into())]);
            assert_eq!(
                texts(1),
                [
                    ("Bob".into(), "hello".into()),
                    ("bob".into(), "again".into()),
                    ("alice".into(), "hi bob".into()),
                    ("alice".into(), "from my phone".into()),
                    ("bob".into(), "[NOTICE] psst".into()),
                ]
            );
            assert!(chat.state.conversations()[2].messages[0].is_history());
            assert!(
                chat.state
                    .server_messages(one)
                    .iter()
                    .any(|m| m.text == "-NickServ- This nickname is registered")
            );
            assert!(
                !chat
                    .state
                    .server_messages(one)
                    .iter()
                    .any(|m| m.text.contains("hello")),
                "not duplicated in the server log"
            );
            let bob = chat.state.conversations()[1].id;
            assert!(chat.state.is_unread(bob) && chat.state.is_highlighted(bob));
            // Two live PRIVMSGs from bob and one from the other server's bob;
            // the replayed one and the notices do not notify.
            assert_eq!(chat.notifier.shown.len(), 3);

            // Reading the conversation in the focused window silences it.
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(bob));
            chat.window_active = true;
            chat.handle_events(one, vec![pm("bob", "seen", false, false)], false, cx);
            assert_eq!(chat.notifier.shown.len(), 3);
            assert!(!chat.state.is_highlighted(bob));

            // Nick changes follow the peer; a quit leaves a boundary line.
            chat.handle_events(
                one,
                vec![
                    Event::UserNickChanged {
                        from: "Bob".into(),
                        to: "robert".into(),
                    },
                    Event::UserQuit {
                        nickname: "robert".into(),
                        reason: Some("bye".into()),
                    },
                    pm("bob", "a new bob", false, false),
                ],
                false,
                cx,
            );
            let robert = chat
                .state
                .conversations()
                .iter()
                .find(|c| c.id == bob)
                .unwrap();
            assert_eq!(robert.name, "robert");
            let tail: Vec<_> = robert
                .messages
                .iter()
                .rev()
                .take(2)
                .map(|m| (m.activity, m.text.as_str()))
                .collect();
            assert_eq!(
                tail,
                [
                    (true, "robert has quit (bye)"),
                    (true, "Bob is now known as robert")
                ]
            );
            let new_bob = chat.state.private_id(one, "bob").unwrap();
            assert_ne!(new_bob, bob);

            // Closing drops the conversation and its draft.
            chat.channel_menu = Some(crate::ChannelMenu {
                position: gpui::point(gpui::px(0.), gpui::px(0.)),
                network: one,
                conversation: new_bob,
                channel: "bob".into(),
                joined: true,
                private: true,
            });
            chat.close_private_conversation(cx);
            assert!(chat.state.private_id(one, "bob").is_none());
            assert!(!chat.inputs.contains_key(&Selection::Channel(new_bob)));
        });
    }

    #[gpui::test]
    fn highlights_and_private_messages_notify_unless_visible(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a,#b");
        settings.language = cayenchat_storage::Language::English;
        settings.notifications.keywords = vec!["deploy".into()];
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let message = |channel: &str, text: &str, mentioned| Event::ChannelMessage {
            channel: channel.into(),
            sender: "bob".into(),
            text: text.into(),
            notice: false,
            mentioned,
            server_time: None,
            msgid: None,
            account: None,
            replayed: false,
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "alice".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    Event::Joined {
                        channel: "#b".into(),
                    },
                ],
                false,
                cx,
            );
            chat.state.dispatch(cayenchat_app::Command::SelectChannel(
                chat.state.conversations()[0].id,
            ));
            chat.window_active = true;
            chat.handle_events(
                NetworkId(1),
                vec![
                    message("#a", "alice: visible already", true),
                    message("#b", "hello", false),
                    message("#b", "\u{2}alice\u{2}: ping", true),
                    message("#b", "Deploy done", false),
                    message("#b", "\u{1}ACTION deploys\u{1}", false),
                    Event::ChannelMessage {
                        channel: "#b".into(),
                        sender: "bob".into(),
                        text: "alice: deploy from the backlog".into(),
                        notice: false,
                        mentioned: true,
                        replayed: true,
                        server_time: None,
                        msgid: None,
                        account: None,
                    },
                    Event::PrivateMessage {
                        sender: "carol".into(),
                        text: "old psst".into(),
                        notice: false,
                        replayed: true,
                        server_time: None,
                        msgid: None,
                        account: None,
                    },
                    Event::PrivateMessage {
                        sender: "carol".into(),
                        text: "psst".into(),
                        notice: false,
                        server_time: None,
                        msgid: None,
                        account: None,
                        replayed: false,
                    },
                    Event::PrivateMessage {
                        sender: "NickServ".into(),
                        text: "notice".into(),
                        notice: true,
                        server_time: None,
                        msgid: None,
                        account: None,
                        replayed: false,
                    },
                ],
                false,
                cx,
            );
            chat.window_active = false;
            chat.handle_events(
                NetworkId(1),
                vec![message("#a", "alice: away now", true)],
                false,
                cx,
            );
            chat.notification_rules.mentions = false;
            chat.handle_events(
                NetworkId(1),
                vec![message("#a", "alice: ignored", true)],
                false,
                cx,
            );
            let summaries: Vec<_> = chat
                .notifier
                .shown
                .iter()
                .map(|n| (n.summary.as_str(), n.body.as_str()))
                .collect();
            assert_eq!(
                summaries,
                [
                    ("bob in #b", "alice: ping"),
                    ("bob in #b", "Deploy done"),
                    ("bob in #b", "* bob deploys"),
                    ("carol (private message)", "psst"),
                    ("bob in #a", "alice: away now"),
                ]
            );
            let (a, b) = (
                &chat.state.conversations()[0],
                &chat.state.conversations()[1],
            );
            assert!(!chat.state.is_highlighted(a.id));
            assert!(chat.state.is_highlighted(b.id));
            assert!(
                chat.highlight_ranges(NetworkId(1), &b.messages[0])
                    .is_empty()
            );
            assert_eq!(
                chat.highlight_ranges(NetworkId(1), &b.messages[1]),
                vec![(1..6)]
            );
            assert_eq!(
                chat.highlight_ranges(NetworkId(1), &b.messages[2]),
                vec![(0..6)]
            );
            assert!(b.messages[4].is_history());
            assert!(
                chat.highlight_ranges(NetworkId(1), &b.messages[4])
                    .is_empty()
            );

            // Replayed history leaves an unselected channel unmarked.
            let (a, b) = (a.id, b.id);
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(b));
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(a));
            chat.handle_events(
                NetworkId(1),
                vec![Event::ChannelMessage {
                    channel: "#b".into(),
                    sender: "tiarra".into(),
                    text: "12:34 <bob> alice: deploy".into(),
                    notice: true,
                    mentioned: true,
                    replayed: true,
                    server_time: None,
                    msgid: None,
                    account: None,
                }],
                false,
                cx,
            );
            assert!(!chat.state.is_highlighted(b));
        });
    }

    #[gpui::test]
    fn old_server_times_display_locally_and_still_notify(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a,#b");
        settings.language = cayenchat_storage::Language::English;
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let old = UNIX_EPOCH + Duration::from_secs(1_319_042_451);
        let local = |time: SystemTime| {
            use chrono::Timelike;
            let time = chrono::DateTime::<chrono::Local>::from(time);
            cayenchat_model::TimeOfDay::new(time.hour() as u8, time.minute() as u8)
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "alice".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    Event::Joined {
                        channel: "#b".into(),
                    },
                ],
                false,
                cx,
            );
            chat.state.dispatch(cayenchat_app::Command::SelectChannel(
                chat.state.conversations()[0].id,
            ));
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::ChannelMessage {
                        channel: "#b".into(),
                        sender: "bob".into(),
                        text: "now".into(),
                        notice: false,
                        mentioned: false,
                        server_time: None,
                        msgid: None,
                        account: None,
                        replayed: false,
                    },
                    Event::ChannelMessage {
                        channel: "#b".into(),
                        sender: "bob".into(),
                        text: "alice: from years ago".into(),
                        notice: false,
                        mentioned: true,
                        server_time: Some(old),
                        msgid: None,
                        account: None,
                        replayed: false,
                    },
                ],
                false,
                cx,
            );
            let b = &chat.state.conversations()[1];
            assert_eq!(b.messages[1].text, "alice: from years ago");
            assert!(b.messages[0].sequence < b.messages[1].sequence);
            assert_eq!(b.messages[1].time, local(old));
            assert!(chat.state.is_unread(b.id));
            assert_eq!(
                chat.notifier.shown.len(),
                1,
                "an old timestamp does not mute"
            );
        });
    }

    #[gpui::test]
    fn safe_channel_events_keep_messages_and_members_in_the_channel(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("");
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let channel = "!ABCDEtest";
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "alice".into(),
                    },
                    Event::Joined {
                        channel: channel.into(),
                    },
                    Event::Names {
                        channel: channel.into(),
                        users: vec!["@alice".into(), "bob".into()],
                    },
                    Event::ChannelMessage {
                        channel: channel.into(),
                        sender: "bob".into(),
                        text: "hello".into(),
                        notice: false,
                        mentioned: false,
                        server_time: None,
                        msgid: None,
                        account: None,
                        replayed: false,
                    },
                    Event::OutgoingAccepted {
                        local_id: None,
                        channel: channel.into(),
                        text: "reply".into(),
                        notice: false,
                    },
                    Event::OutgoingAccepted {
                        local_id: None,
                        channel: channel.into(),
                        text: "notice".into(),
                        notice: true,
                    },
                ],
                false,
                cx,
            );
            let conversations = chat.state.conversations();
            assert_eq!(conversations.len(), 1);
            let conversation = &conversations[0];
            assert_eq!(conversation.name, channel);
            assert!(chat.state.is_active_channel(conversation.id));
            assert_eq!(conversation.members, ["@alice", "bob"]);
            assert_eq!(conversation.messages.len(), 3);
            assert_eq!(conversation.messages[0].text, "hello");
            assert_eq!(conversation.messages[1].text, "reply");
            assert_eq!(conversation.messages[1].sender, "alice");
            assert_eq!(conversation.messages[2].text, "[NOTICE] notice");
            assert!(
                chat.inputs
                    .contains_key(&Selection::Channel(conversation.id))
            );
            let id = conversation.id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(id));
            chat.handle_events(
                NetworkId(1),
                vec![Event::Parted {
                    channel: channel.into(),
                }],
                false,
                cx,
            );
            assert!(!chat.state.is_active_channel(id));
            assert!(chat.state.conversations()[0].members.is_empty());
        });
        cx.run_until_parked();
        // The server and the joined channel.
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.tree_list.state.item_count()),
            2
        );
    }

    #[gpui::test]
    fn events_stay_with_their_server_and_removed_servers_disappear(cx: &mut TestAppContext) {
        use cayenchat_app::ConnectionStatus;
        use cayenchat_irc_core::{Event, WireDirection};
        use std::time::Duration;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.add_server("");
        settings.selected_profile_mut().unwrap().host = "irc.example.org".into();
        settings.selected_profile_mut().unwrap().channels = "#a".into();
        let custom = settings.selected_server.clone();
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        // Tree order: the order the servers were added.
        let (ircnet, custom_net) = (NetworkId(1), NetworkId(2));
        let session_events = |channel: &str, text: &str| {
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: channel.into(),
                },
                Event::Wire {
                    elapsed: Duration::ZERO,
                    direction: WireDirection::Received,
                    line: text.into(),
                },
                Event::ChannelMessage {
                    channel: channel.into(),
                    sender: "bob".into(),
                    text: text.into(),
                    notice: false,
                    mentioned: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ]
        };
        chat.update(cx, |chat, cx| {
            assert_eq!(chat.state.networks().len(), 2);
            assert_eq!(chat.sessions[&custom_net].profile_id, custom);
            chat.handle_events(custom_net, session_events("#a", "custom"), false, cx);
            chat.handle_events(ircnet, session_events("#a", "ircnet"), false, cx);
            fn texts(chat: &ChatWindow, network: NetworkId) -> Vec<String> {
                chat.state
                    .conversations()
                    .iter()
                    .filter(|c| c.network == network)
                    .flat_map(|c| c.messages.iter().map(|m| m.text.clone()))
                    .collect()
            }
            // Same channel name on two servers: separate conversations.
            assert_eq!(texts(chat, custom_net), ["custom"]);
            assert_eq!(texts(chat, ircnet), ["ircnet"]);
            assert_eq!(chat.sessions[&custom_net].diagnostics.len(), 1);
            assert_eq!(chat.sessions[&ircnet].diagnostics.len(), 1);

            // A disconnect on one server leaves the other registered.
            chat.handle_events(
                custom_net,
                vec![Event::Disconnected("gone".into())],
                false,
                cx,
            );
            assert!(matches!(
                chat.state.status(custom_net),
                Some(ConnectionStatus::Disconnected(_))
            ));
            assert_eq!(
                chat.state.status(ircnet),
                Some(&ConnectionStatus::Registered)
            );

            // Removing the server from the settings drops it and its state.
            let removed: Vec<_> = chat
                .state
                .conversations()
                .iter()
                .filter(|c| c.network == custom_net)
                .map(|c| c.id)
                .collect();
            let mut next = settings.clone();
            next.remove_selected_server();
            chat.apply_servers(next, cx);
            assert_eq!(chat.state.networks().len(), 1);
            assert!(!chat.sessions.contains_key(&custom_net));
            assert!(
                removed
                    .iter()
                    .all(|id| !chat.inputs.contains_key(&Selection::Channel(*id)))
            );
            assert_eq!(texts(chat, ircnet), ["ircnet"]);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn each_server_that_rejects_a_nickname_gets_its_own_field(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;

        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.add_server("");
        settings.selected_profile_mut().unwrap().host = "irc.example.org".into();
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let (first, second) = (NetworkId(1), NetworkId(2));
        let rejected = |nickname: &str| {
            vec![Event::NicknameRejected {
                nickname: nickname.into(),
            }]
        };
        chat.update(cx, |chat, cx| {
            chat.handle_events(first, rejected("alice"), false, cx);
            chat.handle_events(second, rejected("bob"), false, cx);
        });
        cx.run_until_parked();
        let rows = |chat: &ChatWindow, cx: &gpui::App| {
            chat.nick_prompts
                .iter()
                .map(|prompt| {
                    (
                        prompt.network,
                        prompt.rejected.clone(),
                        prompt.input.read(cx).text().to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            chat.read_with(cx, |chat, cx| rows(chat, cx)),
            [
                (first, "alice".into(), "alice_".into()),
                (second, "bob".into(), "bob_".into())
            ]
        );
        // The first new row has focus; typing goes there, not into the draft.
        cx.simulate_input("x");
        assert_eq!(
            chat.read_with(cx, |chat, cx| rows(chat, cx))[0].2,
            "alice_x"
        );

        // A second rejection on the same server updates its row.
        chat.update(cx, |chat, cx| {
            chat.handle_events(first, rejected("alice_x"), false, cx)
        });
        let after = chat.read_with(cx, |chat, cx| rows(chat, cx));
        assert_eq!(after.len(), 2);
        assert_eq!(after[0], (first, "alice_x".into(), "alice_x_".into()));

        // Registration on one server closes only its row.
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                first,
                vec![Event::Registered {
                    nickname: "alice_x_".into(),
                }],
                false,
                cx,
            )
        });
        assert_eq!(
            chat.read_with(cx, |chat, cx| rows(chat, cx)),
            [(second, "bob".into(), "bob_".into())]
        );
    }

    #[gpui::test]
    fn starts_without_servers_and_shows_one_once_added(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(Settings::default(), None, window, cx)
        });
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            assert!(chat.state.networks().is_empty());
            assert_eq!(chat.state.selection(), Selection::None);
            assert_eq!(chat.tree_list.state.item_count(), 0);
            assert!(chat.startup_connections.is_empty());
        });
        // The draft still accepts text and Enter reports that nothing is connected.
        cx.simulate_input("hello");
        cx.dispatch_action(super::SendMessage);
        chat.read_with(cx, |chat, _| assert!(chat.feedback.is_some()));

        let settings = crate::settings_with_channels("#a");
        chat.update(cx, |chat, cx| chat.apply_servers(settings, cx));
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.state.networks().len(), 1);
            assert_eq!(chat.state.networks()[0].name, "irc.ircnet.ne.jp");
            assert_eq!(chat.tree_list.state.item_count(), 2);
        });
    }

    #[gpui::test]
    fn typing_reuses_panes_and_new_messages_redraw_them(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ))
        });
        let settings = crate::settings_with_channels("#a,#b");
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        cx.run_until_parked();
        let (channel, input) = chat.update(cx, |chat, _| {
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            (channel, chat.inputs[&Selection::Channel(channel)].clone())
        });
        cx.update(|window, cx| {
            window.focus(&input.focus_handle(cx));
            window.refresh();
        });
        cx.run_until_parked();
        let rendered = chat.read_with(cx, |chat, _| chat.pane_renders);
        assert!(rendered >= 4, "panes rendered {rendered} times");
        // The server row and the two configured channels.
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.tree_list.state.item_count()),
            3
        );

        cx.simulate_input("hello");
        cx.run_until_parked();
        assert_eq!(
            input.read_with(cx, |input, _| input.text().to_owned()),
            "hello"
        );
        assert_eq!(chat.read_with(cx, |chat, _| chat.pane_renders), rendered);

        chat.update(cx, |chat, cx| {
            let name = chat.state.conversations()[0].name.clone();
            chat.state
                .append_channel_message(NetworkId(1), &name, "bob", "hi", false, false);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(chat.read_with(cx, |chat, _| chat.pane_renders) > rendered);
        let rows = chat.read_with(cx, |chat, _| {
            chat.main_lists[&Selection::Channel(channel)]
                .state
                .item_count()
        });
        assert!(rows >= 1);
    }
}

#[cfg(test)]
mod field_traversal_tests {
    use super::{CompleteNickname, field_traversal, shortcut_bindings};
    use crate::input::TextInput;
    use cayenchat_storage::ChannelNumberModifier;
    use gpui::{
        Context, Entity, Focusable, Render, TestAppContext, VisualTestContext, Window, div,
        prelude::*,
    };

    struct Settings {
        fields: Vec<Entity<TextInput>>,
    }
    impl Render for Settings {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            field_traversal(div().id("settings"))
                .key_context("SettingsWindow")
                .children(self.fields.clone())
        }
    }

    struct Chat {
        draft: Entity<TextInput>,
        completions: usize,
    }
    impl Render for Chat {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .key_context("ChatWindow")
                .on_action(cx.listener(|this, _: &CompleteNickname, _, _| this.completions += 1))
                .child(self.draft.clone())
        }
    }

    fn bind(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::theme::apply(
                cayenchat_storage::ThemeMode::Light,
                &cayenchat_storage::Appearance::default(),
                cx,
            );
            crate::input::bind_keys(false, cx);
            cx.bind_keys(shortcut_bindings(
                ChannelNumberModifier::Ctrl,
                &Default::default(),
            ));
        });
    }

    fn focused(view: &Entity<Settings>, cx: &mut VisualTestContext) -> Option<usize> {
        cx.update(|window, cx| {
            view.read(cx)
                .fields
                .iter()
                .position(|field| field.focus_handle(cx).is_focused(window))
        })
    }

    #[gpui::test]
    fn tab_moves_between_settings_fields_without_chat_commands(cx: &mut TestAppContext) {
        bind(cx);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let fields: Vec<_> = ["Host", "Port", "Password"]
                .into_iter()
                .map(|name| {
                    cx.new(|cx| TextInput::new_settings_field(name, "", name == "Password", cx))
                })
                .collect();
            window.focus(&fields[0].focus_handle(cx));
            Settings { fields }
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        assert_eq!(focused(&view, cx), Some(1));
        cx.simulate_keystrokes("tab");
        assert_eq!(focused(&view, cx), Some(2));
        cx.simulate_keystrokes("tab");
        assert_eq!(focused(&view, cx), Some(0), "wraps like platform forms");
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(focused(&view, cx), Some(2));
        // Typing, including Enter, still edits the field it lands in.
        cx.simulate_input("secret");
        cx.simulate_keystrokes("enter");
        assert_eq!(
            view.read_with(cx, |view, cx| view.fields[2].read(cx).text().to_owned()),
            "secret"
        );
    }

    #[gpui::test]
    fn tab_still_completes_nicknames_in_chat(cx: &mut TestAppContext) {
        bind(cx);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let draft = cx.new(|cx| TextInput::new_live("Draft", cx));
            window.focus(&draft.focus_handle(cx));
            Chat {
                draft,
                completions: 0,
            }
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        assert_eq!(view.read_with(cx, |view, _| view.completions), 1);
        assert!(cx.update(|window, cx| view.read(cx).draft.focus_handle(cx).is_focused(window)));
    }
}

#[cfg(test)]
mod navigation_binding_tests {
    use super::{Navigate, shortcut_bindings};
    use cayenchat_app::Command;
    use cayenchat_storage::ChannelNumberModifier;

    /// The navigation command a typed key combination runs, and that no other
    /// navigation binding takes the same keys.
    #[cfg(target_os = "macos")]
    fn command_for(keys: &str) -> Option<Command> {
        let typed = gpui::Keystroke::parse(keys).unwrap();
        let bindings = shortcut_bindings(ChannelNumberModifier::Ctrl, &Default::default());
        let matching: Vec<_> = bindings
            .iter()
            .filter(|binding| binding.match_keystrokes(&[typed.clone()]) == Some(false))
            .collect();
        assert!(
            matching.len() <= 1,
            "{keys} is bound {} times",
            matching.len()
        );
        matching.first().and_then(|binding| {
            binding
                .action()
                .as_any()
                .downcast_ref::<Navigate>()
                .map(|navigate| navigate.command)
        })
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_bracket_keys_move_between_channels_and_servers() {
        // Decided in #72.
        assert_eq!(command_for("cmd-["), Some(Command::PreviousChannel));
        assert_eq!(command_for("cmd-]"), Some(Command::NextChannel));
        // Pressing Cmd+Shift+[ on macOS arrives as `cmd-{` (see the bindings),
        // so that is what must be bound; `cmd-shift-[` never arrives.
        assert_eq!(command_for("cmd-{"), Some(Command::PreviousServer));
        assert_eq!(command_for("cmd-}"), Some(Command::NextServer));
        assert_eq!(command_for("cmd-shift-["), None);
        assert_eq!(command_for("cmd-shift-]"), None);
        // The arrow-based keys stay.
        assert_eq!(command_for("ctrl-up"), Some(Command::PreviousChannel));
        assert_eq!(command_for("ctrl-right"), Some(Command::NextServer));
        assert_eq!(command_for("cmd-up"), Some(Command::PreviousActiveChannel));
        assert_eq!(
            command_for("cmd-alt-down"),
            Some(Command::NextActiveChannel)
        );
    }

    #[test]
    fn no_two_navigation_bindings_share_keys() {
        for modifier in [
            ChannelNumberModifier::Ctrl,
            ChannelNumberModifier::Alt,
            ChannelNumberModifier::Super,
        ] {
            let mut seen = std::collections::HashSet::new();
            for binding in shortcut_bindings(modifier, &Default::default()) {
                if binding
                    .action()
                    .as_any()
                    .downcast_ref::<Navigate>()
                    .is_none()
                {
                    continue;
                }
                let keys = format!("{:?}", binding.keystrokes());
                assert!(
                    seen.insert(keys.clone()),
                    "{keys} is bound twice ({modifier:?})"
                );
            }
        }
    }
}
