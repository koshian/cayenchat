mod account_settings;
mod decorations;
mod desktop;
mod diagnostics;
mod image_upload;
mod input;
mod localization;
mod log_list;
mod menu_bar;
#[cfg(test)]
mod perf_baseline;
mod secrets;
mod session;
mod theme;
mod whois;

use cayenchat_app::{
    AppState, Command, ConnectionStatus, NetworkConfig, Selection, attachments::AttachmentFlow,
};
use cayenchat_irc_core::{
    ChannelActivityKind, Connection, ConnectionConfig, Event, MemberCommand, SaslCredentials,
    WhoisInfo, WireDirection, valid_channel,
};
use cayenchat_model::{ConversationId, NetworkId, TimeOfDay};
use cayenchat_storage::{
    Appearance, ChannelNumberModifier, CredentialBackendKind, CredentialStore, DarkColors,
    Language, LinuxDisplay, Secret, SecretKey, ServerProfile, Settings, TextEncoding, TextKeyTheme,
    ThemeMode, color_value,
};
use gpui::{prelude::*, *};
use input::TextInput;
use localization::Localizer;
use log_list::LogList;
use session::ServerSession;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use theme::Theme;
use whois::WhoisWindow;

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
        OpenSettings,
        Disconnect,
        Reconnect,
        ToggleDebug,
        CopyDiagnostics,
        CopyLogSelection,
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
    custom_host: Entity<TextInput>,
    port: Entity<TextInput>,
    nickname: Entity<TextInput>,
    username: Entity<TextInput>,
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
    sub_log_background: Entity<TextInput>,
    sub_log_alternate: Entity<TextInput>,
    dark_member_list_background: Entity<TextInput>,
    dark_main_log_background: Entity<TextInput>,
    dark_main_log_alternate: Entity<TextInput>,
    dark_channel_event_color: Entity<TextInput>,
    dark_sub_log_background: Entity<TextInput>,
    dark_sub_log_alternate: Entity<TextInput>,
    main_log_font: Entity<TextInput>,
    sub_log_font: Entity<TextInput>,
    member_font: Entity<TextInput>,
    channel_font: Entity<TextInput>,
    input_font: Entity<TextInput>,
    time_font: Entity<TextInput>,
}

impl SettingsForm {
    fn new(
        values: Settings,
        i18n: &Localizer,
        store: &CredentialStore,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let (saved_server_password, saved_sasl_password) =
            saved_passwords(values.selected_profile(), store);
        let field =
            |placeholder: &str, value: &str, secret: bool, cx: &mut Context<SettingsWindow>| {
                cx.new(|cx| TextInput::new_field(placeholder, value, secret, cx))
            };
        Self {
            custom_host: field(
                &i18n.text("server_host_placeholder"),
                &values.selected_profile().host,
                false,
                cx,
            ),
            port: field(
                "6667",
                &values.selected_profile().port.to_string(),
                false,
                cx,
            ),
            nickname: field(
                &i18n.text("nickname"),
                &values.selected_profile().nickname,
                false,
                cx,
            ),
            username: field(
                &i18n.text("username_placeholder"),
                &values.selected_profile().username,
                false,
                cx,
            ),
            channels: field(
                "#first,#second",
                &values.selected_profile().channels,
                false,
                cx,
            ),
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
                &values.selected_profile().sasl_username,
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
            server_list_open: false,
            encoding_list_open: false,
            values,
        }
    }

    fn snapshot(&self, cx: &App) -> Result<Settings, String> {
        let mut settings = self.values.clone();
        let host = self.custom_host.read(cx).text().trim().to_owned();
        let port = self
            .port
            .read(cx)
            .text()
            .trim()
            .parse()
            .map_err(|_| i18n_error(settings.language, "port_invalid"))?;
        if port == 0 {
            return Err(i18n_error(settings.language, "port_invalid"));
        }
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        let profile = settings.selected_profile_mut();
        if profile.custom {
            profile.host = host;
        }
        profile.port = port;
        profile.nickname = value(&self.nickname);
        profile.username = value(&self.username);
        profile.channels = value(&self.channels);
        profile.sasl_username = value(&self.sasl_username);
        settings.appearance = Appearance {
            member_list_background: value(&self.member_list_background),
            main_log_background: value(&self.main_log_background),
            main_log_alternate: value(&self.main_log_alternate),
            channel_event_color: value(&self.channel_event_color),
            sub_log_background: value(&self.sub_log_background),
            sub_log_alternate: value(&self.sub_log_alternate),
            alternate_rows: self.values.appearance.alternate_rows,
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
                sub_log_background: value(&self.dark_sub_log_background),
                sub_log_alternate: value(&self.dark_sub_log_alternate),
            },
        };
        settings.appearance.validate()?;
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
        let profile = settings.selected_profile();
        let (saved_server, saved_sasl) = saved_connection_secrets(profile, store, i18n)?;
        connection_config(
            profile,
            settings.language,
            typed(&self.server_password).or(saved_server),
            typed(&self.sasl_password).or(saved_sasl),
        )
    }

    /// Stores typed passwords when saving is on, then empties the fields so
    /// plaintext does not stay in the form.
    fn persist_passwords(
        &mut self,
        store: &CredentialStore,
        i18n: &Localizer,
        cx: &mut Context<SettingsWindow>,
    ) -> Result<(), String> {
        let profile = self.values.selected_profile().clone();
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
            let text = field.read(cx).text().to_owned();
            if text.is_empty() {
                continue;
            }
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
    config.port = profile.port;
    config.use_tls = profile.use_tls;
    config.verify_tls_certificates = profile.verify_tls_certificates;
    config.encoding = profile.encoding.label().into();
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

/// The newest `limit` conversation lines (not channel activity) across every
/// conversation except `excluded`, oldest first, as (sequence, conversation,
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
        messages[..before]
            .iter()
            .rposition(|message| !message.activity)
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
    startup_connections: Vec<(NetworkId, Result<ConnectionConfig, String>)>,
    settings_window: Option<WindowHandle<SettingsWindow>>,
    window_handle: Option<WindowHandle<ChatWindow>>,
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
    /// Pasted or dropped images on their way to the external uploader.
    attachments: AttachmentFlow,
    /// Configured image hosting provider ID (IRC external uploads).
    image_provider: Option<String>,
    /// Replaces the configured uploader; tests use a fake one.
    uploader_override: Option<Arc<dyn cayenchat_upload::ExternalUploader>>,
    #[cfg(test)]
    pane_renders: usize,
}

struct ServerMenu {
    position: Point<Pixels>,
    network: NetworkId,
}

struct ChannelMenu {
    position: Point<Pixels>,
    network: NetworkId,
    channel: String,
    joined: bool,
}

struct MemberMenu {
    position: Point<Pixels>,
    network: NetworkId,
    nickname: String,
    channel: String,
}

#[derive(Clone, Copy)]
enum MemberPromptKind {
    PrivateMessage,
    Invite,
    /// The server rejected `nickname` during registration.
    AlternateNick,
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
    ImageUpload,
    Credentials,
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

struct SettingsWindow {
    menu_bar: menu_bar::MenuBar,
    owner: WindowHandle<ChatWindow>,
    settings: SettingsForm,
    feedback: Option<String>,
    tab: SettingsTab,
    font_picker: Option<FontTarget>,
    fonts: Vec<String>,
    i18n: Localizer,
    /// Result of probing the system credential store; `None` while checking.
    system_store: Option<Result<(), String>>,
    /// Access token being entered to connect an image hosting account.
    upload_token: Entity<TextInput>,
    upload_connected: bool,
    upload_token_open: bool,
}

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
        let network = self.state.selected_network();
        match self.state.selected_channel() {
            Some(channel) => format!("{} @ {} — CayenChat", channel.name, network.name),
            None => format!("{} — CayenChat", network.name),
        }
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
        for key in state
            .networks()
            .iter()
            .map(|server| Selection::Server(server.id))
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
        let this = Self {
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
            startup_connections,
            settings_window: None,
            window_handle: window.window_handle().downcast::<ChatWindow>(),
            whois_windows: HashMap::new(),
            whois_replies: Vec::new(),
            debug_enabled: false,
            menu_bar: menu_bar::MenuBar::new(window, cx, |this| &mut this.menu_bar),
            appearance: saved.appearance.clone(),
            theme_mode: saved.theme,
            image_provider: saved.image_upload.provider.clone(),
            saved,
            i18n,
            log_focus: cx.focus_handle(),
            log_selection: None,
            log_dragging: false,
            attachments: AttachmentFlow::default(),
            uploader_override: None,
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
        this
    }

    fn apply_appearance(
        &mut self,
        appearance: Appearance,
        mode: ThemeMode,
        cx: &mut Context<Self>,
    ) {
        theme::apply(mode, &appearance, cx);
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
        let keys: Vec<Selection> = self
            .state
            .networks()
            .iter()
            .map(|server| Selection::Server(server.id))
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
            networks.push((id, profile.host.clone()));
        }
        let stale: Vec<NetworkId> = self
            .sessions
            .keys()
            .copied()
            .filter(|id| !networks.iter().any(|(kept, _)| kept == id))
            .collect();
        for id in stale {
            if let Some(mut session) = self.sessions.remove(&id) {
                session.close();
            }
            self.inputs.remove(&Selection::Server(id));
            self.main_lists.remove(&Selection::Server(id));
            self.whois_windows.retain(|(network, _), _| *network != id);
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
        self.disconnect(self.state.selected_network().id, cx);
    }

    fn reconnect_action(&mut self, _: &Reconnect, window: &mut Window, cx: &mut Context<Self>) {
        self.reconnect(self.state.selected_network().id, window, cx);
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
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        let Some(config) = session.active_config.clone() else {
            return;
        };
        session.retry_pending = false;
        if let Some(connection) = session.irc.take() {
            let _ = connection.disconnect();
        }
        session.generation += 1;
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
        if self.state.selected_network().id == network {
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
                conversation.network == network && self.state.is_active_channel(conversation.id)
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
            MemberPromptKind::Invite => "member_channel_placeholder",
            MemberPromptKind::AlternateNick => "nick_prompt_placeholder",
        });
        let input = cx.new(|cx| TextInput::new_field(&placeholder, "", false, cx));
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

    /// Opens a centered prompt for another nickname after the server
    /// rejected `rejected` during registration.
    fn show_nick_prompt(&mut self, network: NetworkId, rejected: String, cx: &mut Context<Self>) {
        let placeholder = self.i18n.text("nick_prompt_placeholder");
        let suggestion = format!("{rejected}_");
        let input = cx.new(|cx| TextInput::new_field(&placeholder, &suggestion, false, cx));
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.feedback = None;
        self.member_prompt = Some(MemberPrompt {
            position: None,
            network,
            nickname: rejected,
            kind: MemberPromptKind::AlternateNick,
            input,
            focus_pending: true,
        });
    }

    /// Retries registration with the nickname from the prompt. The nickname
    /// replaces the configured one for this session's reconnects only.
    fn submit_nick_prompt(
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

    fn cancel_member_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let alternate_nick = self
            .member_prompt
            .take()
            .filter(|prompt| matches!(prompt.kind, MemberPromptKind::AlternateNick))
            .map(|prompt| prompt.network);
        self.feedback = None;
        if let Some(network) = alternate_nick
            && self
                .sessions
                .get(&network)
                .is_some_and(|session| session.irc.is_some())
        {
            self.disconnect(network, cx);
        }
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
        if matches!(prompt.kind, MemberPromptKind::AlternateNick) {
            match self.submit_nick_prompt(network, value, cx) {
                Ok(()) => {
                    self.member_prompt = None;
                    self.feedback = None;
                    window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
                }
                Err(error) => self.feedback = Some(error),
            }
            cx.notify();
            return;
        }
        let prompt = self.member_prompt.as_ref().expect("checked above");
        let result = match self.registered_connection(network) {
            Ok(connection) => match prompt.kind {
                MemberPromptKind::PrivateMessage if value.trim().is_empty() => {
                    Err(self.i18n.text("member_message_required"))
                }
                MemberPromptKind::PrivateMessage => {
                    connection.send_private_message(&prompt.nickname, &value)
                }
                MemberPromptKind::Invite => connection.send_member_command(
                    &prompt.nickname,
                    MemberCommand::Invite {
                        channel: value.trim().to_owned(),
                    },
                ),
                MemberPromptKind::AlternateNick => unreachable!("submitted above"),
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
            .get(&self.state.selected_network().id)
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
                        settings.tab = tab;
                        cx.notify();
                    } else if let Some(profile) = profile.clone() {
                        settings.tab = tab;
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
        let bounds = Bounds::centered(None, size(px(740.), px(750.)), cx);
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(620.), px(540.))),
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
        self.sessions.get(&self.state.selected_network().id)
    }

    fn show_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.debug_enabled = true;
        cx.set_menus(app_menus(true, &self.i18n));
        let network = self.state.selected_network().id;
        self.dispatch(Command::SelectServer(network), window, cx);
        self.sync_log_lists();
        self.main_lists[&self.state.selection()]
            .state
            .scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
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
                Event::Registered { .. }
                    if self.member_prompt.as_ref().is_some_and(|prompt| {
                        prompt.network == network
                            && matches!(prompt.kind, MemberPromptKind::AlternateNick)
                    }) =>
                {
                    self.member_prompt = None;
                }
                _ => {}
            }
            self.handle_event(network, event);
        }
        for (network, info, requested) in std::mem::take(&mut self.whois_replies) {
            self.show_whois(network, info, requested, cx);
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
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.connection_started = None;
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
            } => {
                self.state
                    .append_channel_message(network, &channel, &sender, &text, notice);
            }
            Event::ChannelActivity {
                channel,
                actor,
                kind,
            } => {
                let text = channel_activity_text(&actor, kind);
                self.state.append_channel_activity(network, &channel, text);
            }
            Event::Names { channel, users } => self.state.set_members(network, &channel, users),
            Event::ServerLine(line) => self.state.append_server_message(network, line),
            Event::Whois(info) => {
                let info = *info;
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
                channel,
                text,
                notice,
            } => {
                if valid_channel(&channel) {
                    let nickname = self
                        .sessions
                        .get(&network)
                        .and_then(|session| session.own_nickname.as_deref())
                        .unwrap_or("me");
                    self.state
                        .append_channel_message(network, &channel, nickname, &text, notice);
                } else {
                    self.state.append_server_message(
                        network,
                        self.i18n
                            .format("event_message_queued", &[("channel", &channel)]),
                    );
                }
            }
            Event::NicknameRejected { nickname } => {
                self.state.append_server_message(
                    network,
                    self.i18n
                        .format("event_nick_rejected", &[("nickname", &nickname)]),
                );
            }
            Event::Disconnected(reason) | Event::Refused(reason) => {
                self.record_disconnect(network, reason);
            }
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
        let network = self.state.selected_network().id;
        let connection = self
            .sessions
            .get(&network)
            .and_then(|session| session.irc.as_ref());
        let result = if let Some(connection) = connection {
            if self.state.status(network) != Some(&ConnectionStatus::Registered) {
                Err(self.i18n.text("wait_registration"))
            } else if text.starts_with('/') {
                connection.send_command(&text, selected.map(|channel| channel.name.as_str()))
            } else if let Some(channel) = selected {
                if !self.state.is_active_channel(channel.id) {
                    Err(self.i18n.text("wait_join"))
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
                input.update(cx, |input, cx| input.clear_after_send(cx));
                None
            }
            Err(error) => Some(error),
        };
        cx.notify();
        window.refresh();
    }

    fn send_message(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        if self
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
        if self
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
        let i18n = Localizer::new(values.language);
        let settings = SettingsForm::new(values, &i18n, &secrets::store(cx), cx);
        window.focus(&settings.nickname.focus_handle(cx));
        let mut fonts = window.text_system().all_font_names();
        fonts.sort_unstable();
        fonts.dedup();
        let upload_token =
            cx.new(|cx| TextInput::new_field(&i18n.text("image_token_placeholder"), "", true, cx));
        let mut this = Self {
            menu_bar: menu_bar::MenuBar::new(window, cx, |this| &mut this.menu_bar),
            owner,
            settings,
            feedback: None,
            tab: SettingsTab::Connection,
            font_picker: None,
            fonts,
            i18n,
            system_store: None,
            upload_token,
            upload_connected: false,
            upload_token_open: false,
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
        cx: &mut Context<Self>,
    ) -> Result<Settings, String> {
        let store = secrets::store(cx);
        settings
            .servers
            .retain(|server| !server.custom || !server.host.is_empty());
        self.settings.persist_passwords(&store, &self.i18n, cx)?;
        if let Ok(Some(previous)) = cayenchat_storage::load() {
            forget_removed_profiles(&previous, &settings, &store);
        }
        cayenchat_storage::save(&settings)?;
        Ok(settings)
    }

    fn connect_from_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| {
            let settings = self.settings.snapshot(cx)?;
            let config =
                self.settings
                    .connection_config(&settings, &secrets::store(cx), &self.i18n, cx)?;
            let settings = self.commit_settings(settings, cx)?;
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

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.feedback = match self.settings.snapshot(cx).and_then(|settings| {
            if settings.selected_profile().host.is_empty() {
                return Err(self.i18n.text("server_required"));
            }
            let settings = self.commit_settings(settings, cx)?;
            self.settings.values = settings;
            let appearance = self.settings.values.appearance.clone();
            let mode = self.settings.values.theme;
            let language = self.settings.values.language;
            let shortcuts = ShortcutPrefs::from(&self.settings.values);
            let provider = self.settings.values.image_upload.provider.clone();
            let saved = self.settings.values.clone();
            let _ = self.owner.update(cx, |owner, window, cx| {
                owner.apply_servers(saved, cx);
                owner.image_provider = provider;
                apply_shortcuts(shortcuts, cx);
                owner.apply_appearance(appearance, mode, cx);
                owner.apply_language(language, window, cx);
            });
            Ok(())
        }) {
            Ok(()) => Some(self.i18n.text("settings_saved")),
            Err(error) => Some(error),
        };
        cx.notify();
    }

    fn close_settings(&mut self, window: &mut Window) {
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
            owner.disconnect(owner.state.selected_network().id, cx)
        });
        cx.notify();
    }

    fn reconnect_action(&mut self, _: &Reconnect, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.owner.update(cx, |owner, window, cx| {
            owner.reconnect(owner.state.selected_network().id, window, cx)
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
        if self.settings.values.selected_profile().remember_passwords {
            let profile = self.settings.values.selected_profile().clone();
            let result = store
                .delete(&profile.server_password_key())
                .and_then(|()| store.delete(&profile.sasl_password_key()))
                .map_err(|error| secrets::error_text(&self.i18n, &error))
                .and_then(|()| cayenchat_storage::clear_saved_passwords(&profile.id));
            match result {
                Ok(()) => {
                    self.settings
                        .values
                        .selected_profile_mut()
                        .remember_passwords = false;
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
            match cayenchat_storage::credentials::SystemBackend::probe() {
                Ok(()) => {
                    self.settings
                        .values
                        .selected_profile_mut()
                        .remember_passwords = true;
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
                    this.settings
                        .values
                        .selected_profile_mut()
                        .remember_passwords = true;
                    this.feedback = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn toggle_tls(&mut self, cx: &mut Context<Self>) {
        if self.settings.values.selected_profile().use_tls
            && self.settings.values.selected_profile().sasl_enabled
        {
            self.feedback = Some(self.i18n.text("disable_sasl_first"));
            cx.notify();
            return;
        }
        self.feedback = None;
        let use_tls = !self.settings.values.selected_profile().use_tls;
        let profile = self.settings.values.selected_profile_mut();
        profile.use_tls = use_tls;
        if !use_tls {
            profile.verify_tls_certificates = true;
        }
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
        let profile = self.settings.values.selected_profile_mut();
        if profile.use_tls {
            profile.verify_tls_certificates = !profile.verify_tls_certificates;
            cx.notify();
        }
    }

    fn toggle_sasl(&mut self, cx: &mut Context<Self>) {
        self.feedback = None;
        let profile = self.settings.values.selected_profile_mut();
        profile.sasl_enabled = !profile.sasl_enabled;
        if profile.sasl_enabled && !profile.use_tls {
            self.toggle_tls(cx);
        }
        cx.notify();
    }

    fn show_selected_server(&mut self, cx: &mut Context<Self>) {
        let profile = self.settings.values.selected_profile().clone();
        self.settings
            .custom_host
            .update(cx, |field, cx| field.set_text(&profile.host, cx));
        self.settings.port.update(cx, |field, cx| {
            field.set_text(&profile.port.to_string(), cx)
        });
        for (field, value) in [
            (&self.settings.nickname, &profile.nickname),
            (&self.settings.username, &profile.username),
            (&self.settings.channels, &profile.channels),
            (&self.settings.sasl_username, &profile.sasl_username),
        ] {
            field.update(cx, |field, cx| field.set_text(value, cx));
        }
        let (saved_server, saved_sasl) =
            saved_passwords(self.settings.values.selected_profile(), &secrets::store(cx));
        self.settings.saved_server_password = saved_server;
        self.settings.saved_sasl_password = saved_sasl;
        for (field, saved, key) in [
            (
                &self.settings.server_password,
                saved_server,
                "server_password_placeholder",
            ),
            (&self.settings.sasl_password, saved_sasl, "sasl_password"),
        ] {
            let placeholder = self.i18n.text(if saved {
                "password_saved_placeholder"
            } else {
                key
            });
            field.update(cx, |field, cx| {
                field.set_text("", cx);
                field.set_placeholder(&placeholder, cx);
            });
        }
        self.settings.server_list_open = false;
        self.settings.encoding_list_open = false;
        self.feedback = None;
        cx.notify();
    }

    fn select_server(&mut self, id: String, cx: &mut Context<Self>) {
        match self.settings.snapshot(cx) {
            Ok(mut settings) => {
                settings.selected_server = id;
                self.settings.values = settings;
                self.show_selected_server(cx);
            }
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
            }
        }
    }

    fn add_server(&mut self, cx: &mut Context<Self>) {
        match self.settings.snapshot(cx) {
            Ok(mut settings) => {
                settings.add_custom_server();
                self.settings.values = settings;
                self.show_selected_server(cx);
            }
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
            }
        }
    }

    fn remove_server(&mut self, cx: &mut Context<Self>) {
        self.settings.values.remove_selected_custom_server();
        self.show_selected_server(cx);
    }

    fn render_connection_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = theme::current(cx);
        let profile = self.settings.values.selected_profile().clone();
        let tls = profile.use_tls;
        let sasl = profile.sasl_enabled;
        let mut language_selector = div().flex().gap_1();
        for (index, language) in [Language::System, Language::Japanese, Language::English]
            .into_iter()
            .enumerate()
        {
            language_selector = language_selector.child(
                div()
                    .id(("language-option", index))
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(self.settings.values.language == language, |d| {
                        d.bg(theme.selected)
                    })
                    .child(self.i18n.preference_label(language))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_language(language, window, cx)
                    })),
            );
        }
        let current_host = if profile.custom {
            self.settings.custom_host.read(cx).text().trim().to_owned()
        } else {
            profile.host.clone()
        };
        let current_port = self.settings.port.read(cx).text().trim().to_owned();
        let selected_label = if current_host.is_empty() {
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
                let (host, port) = if id == profile.id {
                    (current_host.as_str(), current_port.as_str())
                } else {
                    (server.host.as_str(), "")
                };
                let label = if host.is_empty() {
                    self.i18n.text("new_server")
                } else {
                    let kind = self.i18n.text(if server.custom {
                        "custom_server"
                    } else {
                        "preset_server"
                    });
                    format!(
                        "{}:{}{}",
                        host,
                        if port.is_empty() {
                            server.port.to_string()
                        } else {
                            port.to_owned()
                        },
                        kind
                    )
                };
                menu = menu.child(
                    div()
                        .id(("server-option", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover))
                        .when(profile.id == id, |d| d.bg(theme.selected))
                        .child(label)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select_server(id.clone(), cx)),
                        ),
                );
            }
            menu = menu.child(
                div()
                    .id("add-server-option")
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.hover))
                    .child(self.i18n.text("add_server"))
                    .on_click(cx.listener(|this, _, _, cx| this.add_server(cx))),
            );
            server_selector = server_selector.child(menu);
        }
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
                            this.settings.values.selected_profile_mut().encoding = encoding;
                            this.settings.encoding_list_open = false;
                            cx.notify();
                        })),
                );
            }
            encoding_selector = encoding_selector.child(menu);
        }
        div()
            .w(px(680.))
            .p_4()
            .mb_4()
            .bg(theme.surface)
            .border_1()
            .border_t_0()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("connection")),
            )
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
            .when(profile.custom, |d| {
                d.child(settings_field(
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
                        .on_click(cx.listener(|this, _, _, cx| this.remove_server(cx))),
                )
            })
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
                            .px_2()
                            .py_1()
                            .border_1()
                            .border_color(theme.border)
                            .cursor_pointer()
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
                                .px_2()
                                .py_1()
                                .border_1()
                                .border_color(theme.border)
                                .cursor_pointer()
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
                    .child(
                        if self.settings.values.selected_profile().connect_on_startup {
                            "☑"
                        } else {
                            "☐"
                        },
                    )
                    .child(self.i18n.text("connect_on_startup"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let profile = this.settings.values.selected_profile_mut();
                        profile.connect_on_startup = !profile.connect_on_startup;
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
            .child(
                div()
                    .id("remember-passwords")
                    .ml(px(158.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(if profile.remember_passwords {
                        "☑"
                    } else {
                        "☐"
                    })
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
                            .px_2()
                            .py_1()
                            .border_1()
                            .border_color(theme.border)
                            .cursor_pointer()
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
            .when_some(self.feedback.clone(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        div()
                            .id("connect-button")
                            .px_3()
                            .py_1()
                            .bg(theme.selected)
                            .cursor_pointer()
                            .child(self.i18n.text("save_and_connect"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.connect_from_settings(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("save-button")
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .child(self.i18n.text("save"))
                            .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
                    )
                    .child(
                        div()
                            .id("back-button")
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .child(self.i18n.text("back"))
                            .on_click(
                                cx.listener(|this, _, window, _| this.close_settings(window)),
                            ),
                    )
                    .child(
                        div()
                            .id("disconnect-button")
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .child(self.i18n.text("disconnect"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                // Disconnects the server being edited.
                                let profile = this.settings.values.selected_server.clone();
                                let _ = this.owner.update(cx, |owner, _, cx| {
                                    if let Some(network) = owner.network_of_profile(&profile) {
                                        owner.disconnect(network, cx);
                                    }
                                });
                                cx.notify();
                            })),
                    ),
            )
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

    fn font_field(&self, target: FontTarget, label: &str, cx: &mut Context<Self>) -> Div {
        let theme = theme::current(cx);
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.font_picker = if this.font_picker == Some(target) {
                                None
                            } else {
                                Some(target)
                            };
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
        let theme = theme::current(cx);
        div()
            .w(px(680.))
            .p_4()
            .mb_4()
            .bg(theme.surface)
            .border_1()
            .border_t_0()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("appearance")),
            )
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
            .child(color_pair(
                &self.i18n.text("member_list_background"),
                &self.settings.member_list_background,
                &self.settings.dark_member_list_background,
                cx,
            ))
            .child(color_pair(
                &self.i18n.text("channel_log"),
                &self.settings.main_log_background,
                &self.settings.dark_main_log_background,
                cx,
            ))
            .child(color_pair(
                &self.i18n.text("channel_log_alternate"),
                &self.settings.main_log_alternate,
                &self.settings.dark_main_log_alternate,
                cx,
            ))
            .child(color_pair(
                &self.i18n.text("channel_event_color"),
                &self.settings.channel_event_color,
                &self.settings.dark_channel_event_color,
                cx,
            ))
            .child(color_pair(
                &self.i18n.text("combined_log"),
                &self.settings.sub_log_background,
                &self.settings.dark_sub_log_background,
                cx,
            ))
            .child(color_pair(
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
                    .child(if self.settings.values.appearance.alternate_rows {
                        "☑"
                    } else {
                        "☐"
                    })
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
            .when_some(self.feedback.clone(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(
                div()
                    .id("save-appearance")
                    .px_3()
                    .py_1()
                    .bg(theme.selected)
                    .cursor_pointer()
                    .child(self.i18n.text("save_and_apply"))
                    .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
            )
    }

    /// Channel-number and draft-editing keys; only Windows and Linux have
    /// choices here, so macOS hides this tab.
    fn render_keyboard_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = theme::current(cx);
        let hint = |key: &str| {
            div()
                .ml(px(158.))
                .text_color(theme.text_secondary)
                .child(self.i18n.text(key))
        };
        div()
            .w(px(680.))
            .p_4()
            .mb_4()
            .bg(theme.surface)
            .border_1()
            .border_t_0()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("keyboard")),
            )
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
            .when_some(self.feedback.clone(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(
                div()
                    .id("save-keyboard")
                    .px_3()
                    .py_1()
                    .bg(theme.selected)
                    .cursor_pointer()
                    .child(self.i18n.text("save_and_apply"))
                    .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
            )
    }

    fn settings_tab(
        &self,
        tab: SettingsTab,
        id: &'static str,
        label_key: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = theme::current(cx);
        div()
            .id(id)
            .px_4()
            .py_2()
            .border_1()
            .when(tab != SettingsTab::Connection, |d| d.border_l_0())
            .border_color(theme.border)
            .cursor_pointer()
            .when(self.tab == tab, |d| {
                d.bg(theme.surface)
                    .border_b_0()
                    .font_weight(FontWeight::BOLD)
            })
            .when(self.tab != tab, |d| {
                d.bg(theme.tab_inactive).hover(|d| d.bg(theme.window))
            })
            .child(self.i18n.text(label_key))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tab = tab;
                this.font_picker = None;
                cx.notify();
            }))
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::current(cx);
        let border = theme.border;
        let tabs = div()
            .flex()
            .w_full()
            .border_b_1()
            .border_color(border)
            .child(self.settings_tab(SettingsTab::Connection, "connection-tab", "connection", cx))
            .child(self.settings_tab(SettingsTab::Appearance, "appearance-tab", "appearance", cx))
            .when(!cfg!(target_os = "macos"), |d| {
                d.child(self.settings_tab(SettingsTab::Keyboard, "keyboard-tab", "keyboard", cx))
            })
            .child(self.settings_tab(
                SettingsTab::ImageUpload,
                "image-upload-tab",
                "image_upload_tab",
                cx,
            ))
            .child(self.settings_tab(
                SettingsTab::Credentials,
                "credentials-tab",
                "credentials_tab",
                cx,
            ));
        let panel = match self.tab {
            SettingsTab::Connection => self.render_connection_settings(cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_settings(cx).into_any_element(),
            SettingsTab::Keyboard => self.render_keyboard_settings(cx).into_any_element(),
            SettingsTab::ImageUpload => self.render_image_upload_settings(cx).into_any_element(),
            SettingsTab::Credentials => self.render_credential_settings(cx).into_any_element(),
        };
        div()
            .id("settings-screen")
            .key_context("SettingsWindow")
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .bg(theme.window)
            .text_size(px(13.))
            .text_color(theme.text)
            .child(
                div().w_full().flex().justify_center().child(
                    div()
                        .w(px(680.))
                        .mt_4()
                        .flex()
                        .flex_col()
                        .child(tabs)
                        .child(panel),
                ),
            )
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::disconnect_action))
            .on_action(cx.listener(Self::reconnect_action))
            .on_action(cx.listener(Self::toggle_debug_action))
            .on_action(cx.listener(Self::copy_diagnostics_action))
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

fn color_input(input: &Entity<TextInput>, cx: &App) -> Div {
    let theme = theme::current(cx);
    let swatch = color_value(input.read(cx).text()).unwrap_or(0xffffff);
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(div().flex_1().min_w_0().child(input.clone()))
        .child(
            div()
                .w(px(24.))
                .h(px(24.))
                .flex_shrink_0()
                .border_1()
                .border_color(theme.border)
                .bg(rgb(swatch)),
        )
}

/// A color setting with its light-theme and dark-theme values side by side.
fn color_pair(label: &str, light: &Entity<TextInput>, dark: &Entity<TextInput>, cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(px(150.)).flex_shrink_0().child(label.to_owned()))
        .child(color_input(light, cx))
        .child(color_input(dark, cx))
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
    selected: Option<std::ops::Range<usize>>,
    theme: &Theme,
) -> StyledText {
    let mut boundaries = vec![0, text.len()];
    for (range, _) in urls {
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
        (is_url || is_selected).then_some((
            range,
            HighlightStyle {
                color: is_url.then_some(theme.link.into()),
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

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.i18n.text("settings_title");
        let content = self.render_settings(cx).into_any_element();
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

impl Render for ChatWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.render_chat(window, cx);
        let content = menu_bar::wrap(
            &self.menu_bar,
            cx.get_menus().unwrap_or_default(),
            content,
            |this| &mut this.menu_bar,
            window,
            cx,
        );
        decorations::window_frame(window, cx, self.window_title(), content)
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
                keys.push(base | (u64::from(conversation.id.0) + 1));
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
                let name = conversation.name.clone();
                let network = conversation.network;
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
                                channel: name.clone(),
                                joined,
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
            PaneKind::MainLog => list(
                self.main_lists[&self.state.selection()].state.clone(),
                cx.processor(Self::render_main_row),
            )
            .size_full()
            .into_any_element(),
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

        let main_pane = div().flex().flex_col().flex_1().min_h_0().child(main_log);
        let sub_pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(border)
            .child(sub_log);
        let editor = div()
            .flex()
            .items_center()
            .h(px(38.))
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
        let left = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(main_pane)
            .child(editor)
            .child(sub_pane);
        let right = div()
            .flex()
            .flex_col()
            .w(px(240.))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(border)
            .child(members)
            .child(panes.channels.clone().cached(pane_style().w_full()));

        let server_menu = self.server_menu.as_ref().map(|menu| {
            let network = menu.network;
            let position = menu.position;
            let session = self.sessions.get(&network);
            let connected = session.is_some_and(|session| session.irc.is_some());
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
                        .when(connected, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(theme.hover_strong))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.disconnect(network, cx)),
                                )
                        })
                        .when(!connected, |d| d.text_color(theme.text_muted)),
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
                    MemberPromptKind::AlternateNick => "nick_prompt_title",
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
                MemberPromptKind::AlternateNick => "nick_prompt_submit",
                _ => "member_submit",
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
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::disconnect_action))
            .on_action(cx.listener(Self::reconnect_action))
            .on_action(cx.listener(Self::toggle_debug))
            .on_action(cx.listener(Self::copy_diagnostics))
            .on_action(cx.listener(Self::paste_image))
            .child(left)
            .child(right)
            .when_some(server_menu, |d, menu| d.child(menu))
            .when_some(member_menu, |d, menu| d.child(menu))
            .when_some(channel_menu, |d, menu| d.child(menu))
            .when_some(member_prompt, |d, prompt| d.child(prompt))
            .into_any_element()
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        // Rows and their listeners belong to the chat window, so build them in
        // its context; it is not being updated while panes lay out.
        self.chat
            .update(cx, |chat, cx| chat.render_pane(kind, cx))
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
}

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
        let registration_incomplete = self.state.status(self.state.selected_network().id)
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
                .state
                .server_messages(self.state.selected_network().id)
                .iter()
                .map(|m| m.sequence)
                .collect(),
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
                let text = self.status_text(self.state.status(self.state.selected_network().id));
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
                    let network = self.state.selected_network().id;
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
        let Some(message) = self
            .state
            .selected_channel()
            .and_then(|channel| channel.messages.get(index))
        else {
            return div().into_any_element();
        };
        let urls = log_urls(&message.text);
        let selected_range = self
            .log_selection
            .filter(|selection| selection.channel == selected_channel)
            .and_then(|selection| selection.range(index, message.text.len()));
        let styled = styled_log_text(&message.text, &urls, selected_range, &style.theme);
        let layout = styled.layout().clone();
        let down_layout = layout.clone();
        let move_layout = layout.clone();
        let click_layout = layout;
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
            .when(!message.activity, |row| {
                row.child(
                    div()
                        .w(px(84.))
                        .flex_shrink_0()
                        .flex()
                        .justify_end()
                        .text_right()
                        .text_color(theme.nickname)
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(message.sender.clone()),
                        )
                        .child(":"),
                )
            })
            .child(
                div()
                    .id(("message-text", index))
                    .flex_1()
                    .min_w_0()
                    .when(message.activity, |d| d.text_color(style.event_color))
                    .cursor(CursorStyle::IBeam)
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
                        let byte = move_layout
                            .index_for_position(event.position)
                            .unwrap_or_else(|index| index)
                            .min(text_len);
                        this.extend_log_selection(selected_channel, index, byte, cx);
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
                    })),
            )
            .into_any_element()
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
                    .w(px(162.))
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
                        message.text.clone()
                    } else {
                        format!("{}: {}", message.sender, message.text)
                    }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.dispatch(Command::SelectChannel(id), window, cx);
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
        (start..end)
            .map(|index| {
                let member = channel.members[index].clone();
                let channel = channel.name.clone();
                let nickname = member
                    .trim_start_matches(['~', '&', '@', '%', '+'])
                    .to_owned();
                div()
                    .id(("member", index))
                    .px_2()
                    .py(px(1.))
                    .hover(|d| d.bg(theme.hover))
                    .child(member)
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
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
#[derive(Clone, Copy, Debug, Default)]
struct ShortcutPrefs {
    channel_modifier: ChannelNumberModifier,
    text_keys: TextKeyTheme,
}

impl Global for ShortcutPrefs {}

impl From<&Settings> for ShortcutPrefs {
    fn from(settings: &Settings) -> Self {
        Self {
            channel_modifier: settings.channel_number_modifier,
            text_keys: settings.text_key_theme,
        }
    }
}

/// Replaces every key binding, so changed key preferences apply without
/// restarting.
fn apply_shortcuts(prefs: ShortcutPrefs, cx: &mut App) {
    cx.set_global(prefs);
    rebind_shortcuts(cx);
}

fn rebind_shortcuts(cx: &mut App) {
    let prefs = cx
        .try_global::<ShortcutPrefs>()
        .copied()
        .unwrap_or_default();
    let emacs = match prefs.text_keys {
        TextKeyTheme::Auto => desktop::current(cx).emacs_keys,
        TextKeyTheme::Standard => false,
        TextKeyTheme::Emacs => true,
    };
    cx.clear_key_bindings();
    input::bind_keys(emacs, cx);
    cx.bind_keys(shortcut_bindings(prefs.channel_modifier));
}

#[cfg_attr(target_os = "macos", allow(unused_variables))]
fn shortcut_bindings(channel_modifier: ChannelNumberModifier) -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("tab", CompleteNickname, Some("TextInput")),
        KeyBinding::new("enter", SendMessage, Some("TextInput")),
        KeyBinding::new("ctrl-enter", Notice, Some("TextInput")),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-shift-d", ToggleDebug, None),
        KeyBinding::new("secondary-shift-l", CopyDiagnostics, None),
        KeyBinding::new("secondary-c", CopyLogSelection, Some("MainLog")),
        navigation_binding("ctrl-tab", Command::NextUnreadChannel),
        navigation_binding("ctrl-shift-tab", Command::PreviousUnreadChannel),
        KeyBinding::new("secondary-q", Quit, None),
    ];

    #[cfg(target_os = "macos")]
    bindings.extend([
        navigation_binding("alt-space", Command::NextUnreadChannel),
        navigation_binding("alt-shift-space", Command::PreviousUnreadChannel),
        navigation_binding("alt-tab", Command::PreviousSelectedChannel),
        navigation_binding("cmd-up", Command::PreviousActiveChannel),
        navigation_binding("cmd-down", Command::NextActiveChannel),
        navigation_binding("cmd-alt-up", Command::PreviousActiveChannel),
        navigation_binding("cmd-alt-down", Command::NextActiveChannel),
        navigation_binding("cmd-{", Command::PreviousActiveChannel),
        navigation_binding("cmd-}", Command::NextActiveChannel),
        navigation_binding("ctrl-up", Command::PreviousChannel),
        navigation_binding("ctrl-down", Command::NextChannel),
        navigation_binding("cmd-alt-left", Command::PreviousActiveServer),
        navigation_binding("cmd-alt-right", Command::NextActiveServer),
        navigation_binding("ctrl-left", Command::PreviousServer),
        navigation_binding("ctrl-right", Command::NextServer),
    ]);

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    bindings.extend([
        navigation_binding("alt-left", Command::PreviousSelectedChannel),
        navigation_binding("ctrl-pageup", Command::PreviousActiveChannel),
        navigation_binding("ctrl-pagedown", Command::NextActiveChannel),
        navigation_binding("alt-up", Command::PreviousChannel),
        navigation_binding("alt-down", Command::NextChannel),
        navigation_binding("ctrl-alt-pageup", Command::PreviousActiveServer),
        navigation_binding("ctrl-alt-pagedown", Command::NextActiveServer),
        navigation_binding("alt-pageup", Command::PreviousServer),
        navigation_binding("alt-pagedown", Command::NextServer),
    ]);

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
    let notice = match cayenchat_storage::migrate_legacy_secrets(&mut saved, &CredentialStore::open)
    {
        Ok(None) => None,
        Ok(Some(report)) if report.used_local_file => Some(i18n.text("legacy_migrated_local")),
        Ok(Some(_)) => Some(i18n.text("legacy_migrated")),
        Err(error) => Some(i18n.format("credential_error", &[("error", &error)])),
    };
    (saved, notice)
}

fn main() {
    diagnostics::init();
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
        let bounds = Bounds::centered(None, size(px(960.), px(600.)), cx);
        let chat_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(720.), px(420.))),
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
        let needs_settings = chat_window
            .update(cx, |chat, window, cx| {
                cx.set_menus(app_menus(false, &chat.i18n));
                let startup = std::mem::take(&mut chat.startup_connections);
                // Settings open when nothing connects at startup or a saved
                // server is invalid; the servers that are valid still connect.
                let mut needs_settings = startup.is_empty();
                for (network, config) in startup {
                    match config {
                        Ok(config) => {
                            needs_settings |=
                                chat.apply_connection(network, config, window, cx).is_err();
                        }
                        Err(error) => {
                            chat.feedback =
                                Some(chat.i18n.format("startup_invalid", &[("error", &error)]));
                            cx.notify();
                            needs_settings = true;
                        }
                    }
                }
                needs_settings
            })
            .expect("could not initialize the chat window");
        if needs_settings {
            chat_window
                .update(cx, |chat, window, cx| {
                    chat.open_settings(&OpenSettings, window, cx)
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
                    .filter(|(_, m)| !m.activity)
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
                sender: "bob".into(),
                text: String::new(),
                activity: sequence % 5 == 0,
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
        settings.selected_profile_mut().nickname = "alice".into();
        settings.selected_profile_mut().username = "ident".into();
        assert!(startup_connections(&settings, &store).is_empty());

        let profile = settings.selected_profile_mut();
        profile.connect_on_startup = true;
        profile.sasl_enabled = true;
        profile.sasl_username = "account".into();
        profile.use_tls = true;
        assert!(startup_connections(&settings, &store)[0].1.is_err());

        let profile = settings.selected_profile().clone();
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
        settings.selected_profile_mut().remember_passwords = true;
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
        settings.add_custom_server();
        let custom = settings.selected_profile_mut();
        custom.host = "irc.example.org".into();
        custom.nickname = "bob".into();
        custom.username = "bob".into();
        custom.channels = "#b".into();
        custom.connect_on_startup = true;
        let preset = settings
            .servers
            .iter_mut()
            .find(|server| server.id == cayenchat_storage::IRCNET_ID)
            .unwrap();
        preset.nickname = "alice".into();
        preset.username = "alice".into();
        preset.channels = "#a".into();
        preset.connect_on_startup = true;
        let startup = startup_connections(&settings, &store);
        // Tree order: user-added servers before the presets.
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
    fn nickname_and_username_stay_independent_without_credentials() {
        let mut settings = Settings::default();
        settings.selected_profile_mut().nickname = "alice".into();
        settings.selected_profile_mut().username = "someone".into();
        let language = settings.language;
        let config = connection_config(settings.selected_profile(), language, None, None).unwrap();
        assert_eq!(config.nickname, "alice");
        assert_eq!(config.username, "someone");
        assert!(config.server_password.is_none());
        assert!(config.sasl.is_none());
        settings.selected_profile_mut().username.clear();
        assert!(connection_config(settings.selected_profile(), language, None, None).is_err());
    }

    #[test]
    fn replacing_a_profile_before_saving_never_inherits_its_passwords() {
        // Cover both legacy sequential IDs and IDs assigned to new profiles.
        for legacy in [true, false] {
            let store = memory_store();
            let mut previous = Settings::default();
            previous.add_custom_server();
            previous.selected_profile_mut().host = "old.example.org".into();
            previous.selected_profile_mut().remember_passwords = true;
            if legacy {
                previous.selected_profile_mut().id = "custom-1".into();
                previous.selected_server = "custom-1".into();
            }
            let removed = previous.selected_profile().clone();
            for key in [removed.server_password_key(), removed.sasl_password_key()] {
                store.set(&key, &Secret::new("old-password")).unwrap();
            }

            let mut next = previous.clone();
            next.remove_selected_custom_server();
            next.add_custom_server();
            next.selected_profile_mut().host = "new.example.org".into();
            next.selected_profile_mut().remember_passwords = true;
            next.selected_profile_mut().sasl_enabled = true;

            // Connecting reads saved credentials before committing settings.
            let i18n = super::Localizer::new(next.language);
            let (server, sasl) =
                super::saved_connection_secrets(next.selected_profile(), &store, &i18n).unwrap();
            assert!(server.is_none());
            assert!(sasl.is_none());
            assert_ne!(next.selected_profile().id, removed.id);

            forget_removed_profiles(&previous, &next, &store);
            assert!(!store.contains(&removed.server_password_key()).unwrap());
            assert!(!store.contains(&removed.sasl_password_key()).unwrap());
        }
    }

    #[test]
    fn removed_profiles_lose_their_saved_passwords() {
        let store = memory_store();
        let mut previous = Settings::default();
        previous.add_custom_server();
        previous.selected_profile_mut().host = "irc.example.org".into();
        let removed = previous.selected_profile().clone();
        let kept = SecretKey::server_password(cayenchat_storage::IRCNET_ID);
        store
            .set(&removed.server_password_key(), &Secret::new("x"))
            .unwrap();
        store.set(&kept, &Secret::new("y")).unwrap();
        let mut next = previous.clone();
        next.remove_selected_custom_server();
        forget_removed_profiles(&previous, &next, &store);
        assert!(store.get(&removed.server_password_key()).unwrap().is_none());
        assert!(store.get(&kept).unwrap().is_some());
    }
}

/// Default settings whose selected server auto-joins `channels`.
#[cfg(test)]
fn settings_with_channels(channels: &str) -> Settings {
    let mut settings = Settings::default();
    settings.selected_profile_mut().channels = channels.into();
    settings
}

#[cfg(test)]
mod pane_tests {
    use super::{ChatWindow, Selection};
    use cayenchat_model::NetworkId;
    use cayenchat_storage::Settings;
    use gpui::{Focusable, TestAppContext};

    #[gpui::test]
    fn safe_channel_events_keep_messages_and_members_in_the_channel(cx: &mut TestAppContext) {
        use cayenchat_irc_core::Event;

        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = Settings::default();
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
                    },
                    Event::OutgoingAccepted {
                        channel: channel.into(),
                        text: "reply".into(),
                        notice: false,
                    },
                    Event::OutgoingAccepted {
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
        // Both preset servers and the joined channel.
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.tree_list.state.item_count()),
            3
        );
    }

    #[gpui::test]
    fn events_stay_with_their_server_and_removed_servers_disappear(cx: &mut TestAppContext) {
        use cayenchat_app::ConnectionStatus;
        use cayenchat_irc_core::{Event, WireDirection};
        use std::time::Duration;

        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.add_custom_server();
        settings.selected_profile_mut().host = "irc.example.org".into();
        settings.selected_profile_mut().channels = "#a".into();
        let custom = settings.selected_server.clone();
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        // Tree order: the user-added server, then both presets.
        let (custom_net, ircnet) = (NetworkId(1), NetworkId(2));
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
                },
            ]
        };
        chat.update(cx, |chat, cx| {
            assert_eq!(chat.state.networks().len(), 3);
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
            next.remove_selected_custom_server();
            chat.apply_servers(next, cx);
            assert_eq!(chat.state.networks().len(), 2);
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
    fn typing_reuses_panes_and_new_messages_redraw_them(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
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
        // Both preset server rows and the two configured channels.
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.tree_list.state.item_count()),
            4
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
                .append_channel_message(NetworkId(1), &name, "bob", "hi", false);
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
