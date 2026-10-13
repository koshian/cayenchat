// SPDX-License-Identifier: GPL-3.0-only
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account_settings;
mod application_settings;
mod autostart;
mod avatar_editor;
mod avatars;
mod char_refs;
mod color_picker;
mod compact_urls;
mod conversation_log;
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
mod list_keys;
mod localization;
mod log_list;
mod member_selection;
mod menu_bar;
mod notifier;
#[cfg(test)]
mod perf_baseline;
mod previews;
mod scrollbar;
mod secrets;
mod session;
mod settings_file;
mod settings_reset;
mod settings_theme;
mod settings_window;
mod shortcut_settings;
mod shortcuts;
#[cfg(target_os = "linux")]
mod signals;
mod splitter;
mod theme;
mod whois;
mod window_layout;

use cayenchat_app::{
    AppState, Command, ConnectionStatus, Kind, MessageMeta, Missing, MoveTo, NetworkConfig,
    Selection, Unread,
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
use cayenchat_model::{
    ConversationId, Network, NetworkId, Provenance, TimeOfDay, Timestamp, display, names,
};
use cayenchat_storage::chatlog::{Kind as LogKind, Scope};
use cayenchat_storage::{
    Appearance, AutoJoinEntry, ChannelNumberModifier, CredentialError, CredentialStore,
    Ircv3Preferences, Language, Notifications, Secret, SecretKey, ServerProfile, Settings,
    TextKeyTheme, ThemeMode,
};
use futures_util::StreamExt;
use gpui::{prelude::*, *};
use input::TextInput;
use ircv3_settings::own_avatar_failure;
use localization::Localizer;
use log_list::LogList;
use notifier::{DesktopNotification, Notifier};
use session::ServerSession;
use settings_window::{SettingsTab, SettingsWindow, settings_field};
use std::ops::Range;
use std::{
    borrow::Cow,
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use theme::Theme;
use whois::{JoinedChannels, WhoisWindow};

/// The channel log's share of the log height when nothing was dragged, and
/// the bounds of what dragging may leave to either log.
const DEFAULT_LOG_SPLIT: f32 = 0.5;
/// Pause after the last move or resize before the window layout is written.
const LAYOUT_SAVE_DELAY: Duration = Duration::from_millis(500);
/// Width of the right column (members and channel tree), its limits, and the
/// width the left column keeps.
const DEFAULT_RIGHT_WIDTH: f32 = 240.;
const RIGHT_WIDTH_MIN: f32 = 160.;
const LEFT_MIN_WIDTH: f32 = 160.;
/// Smallest main window; panes shrink and clip below their comfortable sizes.
const MIN_WINDOW_SIZE: (f32, f32) = (320., 240.);
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

/// Whether the IRC worker is still delivering events to
/// [`ChatWindow::handle_events`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Worker {
    Running,
    /// Its event stream ended.
    Closed,
}

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
        ChannelActivityKind::Kicked {
            reason, own: true, ..
        } => with_detail("You were kicked from the channel by", actor, reason),
        ChannelActivityKind::Kicked { target, reason, .. } => {
            with_detail(&format!("{target} was kicked by"), actor, reason)
        }
        ChannelActivityKind::JoinFailed { banned, reason } => {
            if banned {
                format!("Cannot join: you are banned from this channel ({reason})")
            } else {
                format!("Cannot join: {reason}")
            }
        }
    }
}

/// Positions in `batch` of member lists that a later member list of the same
/// channel in the batch replaces. A change of case mapping can make the same
/// name another conversation, so no member list is dropped across one.
fn superseded_rosters(batch: &[Event]) -> std::collections::HashSet<usize> {
    let mut later = std::collections::HashSet::new();
    let mut superseded = std::collections::HashSet::new();
    for (index, event) in batch.iter().enumerate().rev() {
        match event {
            Event::Names { channel, .. } => {
                if !later.insert(channel.as_str()) {
                    superseded.insert(index);
                }
            }
            Event::CaseMapping(_) | Event::TransportConnected => later.clear(),
            _ => {}
        }
    }
    superseded
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
    config.part_message = profile.part_message.clone();
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
        own_nick_avatar: preferences.own_nick_avatar,
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

/// Deletes the saved passwords of profiles that no longer exist or no longer
/// keep them. New profiles have fresh IDs even if cleanup fails or a removal
/// has not been saved yet. Every deletion is tried; the first failure is
/// returned so the save stays pending and a later Save retries.
fn forget_removed_profiles(
    previous: &Settings,
    next: &Settings,
    store: &CredentialStore,
) -> Result<(), CredentialError> {
    let mut first_error = None;
    for server in &previous.servers {
        let kept = next.servers.iter().find(|kept| kept.id == server.id);
        let stopped_keeping =
            server.remember_passwords && kept.is_some_and(|kept| !kept.remember_passwords);
        if kept.is_none() || stopped_keeping {
            for key in [server.server_password_key(), server.sasl_password_key()] {
                if let Err(error) = store.delete(&key) {
                    first_error.get_or_insert(error);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
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
    provenance: Provenance,
) -> MessageMeta {
    MessageMeta {
        server_time,
        native_id: msgid.and_then(cayenchat_model::NativeMessageId::new),
        account: account.and_then(cayenchat_model::ServicesAccount::new),
        ..MessageMeta::with_provenance(provenance)
    }
}

/// The provenance of an event's `replayed` flag.
fn event_provenance(replayed: bool) -> Provenance {
    if replayed {
        Provenance::Replayed
    } else {
        Provenance::Live
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
            notice: message.notice,
            meta: irc_message_meta(
                message.server_time,
                message.msgid.as_deref(),
                message.account.as_deref(),
                Provenance::Replayed,
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

/// A draft that failed to send and the warning shown for it.
struct SendFailure {
    selection: Selection,
    text: String,
    message: String,
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
    /// The draft whose failed send put `feedback` on screen; editing that
    /// draft makes the warning obsolete, unrelated feedback stays.
    send_failure: Option<SendFailure>,
    /// What startup reported, such as where unreadable settings were kept.
    /// It is shown again when the settings cannot be opened for the same reason.
    startup_notice: Option<String>,
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
    /// The channel order chosen by dragging or the menu, and the file it is
    /// written to (`None` writes nothing, as for `layout_file`).
    channel_orders: cayenchat_storage::order::ChannelOrders,
    order_file: Option<std::path::PathBuf>,
    /// The channel being dragged in the tree, while its numbers are shown.
    dragging_channel: Option<ConversationId>,
    /// The tree row under the pointer during that drag.
    drop_target: Option<ConversationId>,
    layout_save: Option<Task<()>>,
    /// Layout writes made so far, and the number of the newest one written.
    /// A write that is late finds a newer one done and leaves it alone.
    layout_writes: u64,
    layout_written: Arc<std::sync::Mutex<u64>>,
    /// Where the window was last seen, for writing the layout without it.
    window_bounds: Option<WindowBounds>,
    /// The native window title last set from `render`.
    shown_title: String,
    /// Open WHOIS windows by network and lowercase nickname.
    whois_windows: HashMap<(NetworkId, String), WindowHandle<WhoisWindow>>,
    whois_replies: Vec<(NetworkId, WhoisInfo, bool)>,
    debug_enabled: bool,
    conversation_logs: conversation_log::ConversationLogs,
    menu_bar: menu_bar::MenuBar,
    appearance: Appearance,
    theme_mode: ThemeMode,
    i18n: Localizer,
    log_focus: FocusHandle,
    /// Focus of the channel tree and of the member list, which take the
    /// standard list keys (see [`list_keys`]) while focused.
    tree_focus: FocusHandle,
    members_focus: FocusHandle,
    members_scroll: UniformListScrollHandle,
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
    found: &'a Found,
    replayed: bool,
}

/// What an incoming message's text mentions: found once when it arrives, and
/// shared by the log, the channel tree and notifications.
#[derive(Default)]
struct Found {
    mentioned: bool,
    keyword: bool,
    /// Byte ranges of the text as received ([`Message::highlights`]).
    ranges: Vec<std::ops::Range<usize>>,
}

struct ServerMenu {
    position: Point<Pixels>,
    network: NetworkId,
}

/// What a drag in the channel tree carries: the conversation and the place
/// it was picked up from.
#[derive(Clone)]
struct DraggedChannel {
    id: ConversationId,
    network: NetworkId,
    private: bool,
    name: String,
}

/// The label that follows the pointer while a channel is dragged.
struct ChannelDragPreview(String);

impl ChannelDragPreview {
    /// The name is drawn, so bidirectional controls in it are neutralized.
    fn new(name: &str) -> Self {
        Self(display::neutralize_bidi(name).into_owned())
    }
}

impl Render for ChannelDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::current(cx);
        div()
            .px_2()
            .py(px(2.))
            .bg(theme.selected)
            .border_1()
            .border_color(theme.border)
            .child(self.0.clone())
    }
}

/// The digit a numbered channel shortcut gives the conversation at `index`
/// of the tree (D009): 1 to 9, then 0; none beyond the tenth.
fn shortcut_digit(index: usize) -> Option<char> {
    char::from_digit(((index + 1) % 10) as u32, 10).filter(|_| index < 10)
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
    /// The menu is about the peer of a private conversation: no channel
    /// actions (invite, op).
    private: bool,
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

/// The dialog heading, with the nickname as drawn.
fn member_prompt_title(i18n: &Localizer, kind: MemberPromptKind, nickname: &str) -> String {
    i18n.format_nickname(
        match kind {
            MemberPromptKind::PrivateMessage => "member_message_title",
            MemberPromptKind::Invite => "member_invite_title",
            MemberPromptKind::Join => "channel_join_title",
            MemberPromptKind::Nick => "nickname_change_title",
        },
        nickname,
    )
}

/// A connection diagnostics line as drawn; copying keeps the original.
fn diagnostic_text(line: &str) -> String {
    display::neutralize_bidi(line).into_owned()
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

    /// The label for a server: its saved display name when set, else the
    /// name the connection reports.
    fn network_label(&self, network: &Network) -> String {
        self.sessions
            .get(&network.id)
            .and_then(|session| self.saved.profile(&session.profile_id))
            .map(|profile| profile.display_name.as_str())
            .filter(|alias| !alias.is_empty())
            .unwrap_or(network.name.as_str())
            .to_string()
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
        let network_name = self.network_label(network);
        match self.state.selected_channel() {
            Some(channel) => {
                // A channel shows how many members it has, once the roster is known.
                let name = if channel.is_private() || channel.members.is_empty() {
                    channel.name.clone()
                } else {
                    format!("{} ({})", channel.name, channel.members.len())
                };
                let name = display::neutralize_bidi_owned(name);
                let topic = cayenchat_irc_core::text::strip_formatting(&channel.topic);
                let topic = topic.split_whitespace().collect::<Vec<_>>().join(" ");
                if topic.is_empty() {
                    format!("{name} @ {network_name} — {app}")
                } else {
                    format!("{name} @ {network_name}: {topic} — {app}")
                }
            }
            None => format!("{network_name} — {app}"),
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
        cx.set_global(input::ShortenPastedAmazon(saved.shorten_amazon_urls));
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
        // The conversation log thread wakes the window when it fails or finds
        // its directory locked, so the user hears of it without waiting for
        // an IRC event.
        let (log_wake, log_woken) = futures_channel::mpsc::channel::<()>(1);
        let log_notify: cayenchat_storage::chatlog::Notify = Arc::new(move || {
            let _ = log_wake.clone().try_send(());
        });
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
            startup_notice: feedback.clone(),
            feedback,
            send_failure: None,
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
            channel_orders: Default::default(),
            order_file: None,
            dragging_channel: None,
            drop_target: None,
            layout_save: None,
            layout_writes: 0,
            layout_written: Default::default(),
            window_bounds: None,
            shown_title: String::new(),
            whois_windows: HashMap::new(),
            whois_replies: Vec::new(),
            debug_enabled: false,
            conversation_logs: conversation_log::ConversationLogs::new(
                &saved.conversation_log,
                log_notify,
            ),
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
            tree_focus: cx.focus_handle(),
            members_focus: cx.focus_handle(),
            members_scroll: UniformListScrollHandle::new(),
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
        cx.spawn(async move |this, cx| {
            let mut woken = log_woken;
            while woken.next().await.is_some() {
                let alive = this.update(cx, |this, cx| {
                    this.announce_conversation_log();
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        let keys: Vec<Selection> = this.inputs.keys().copied().collect();
        for key in keys {
            this.watch_input(key, cx);
        }
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
        // A choice of members (it can be acted on all at once) ends when
        // focus moves anywhere else; GPUI also reports it when the window
        // becomes inactive.
        let members_focus = this.members_focus.clone();
        cx.on_focus_out(&members_focus, window, |this, _, _, cx| {
            this.clear_member_selection(cx);
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
                this.quit_connections();
            });
            true
        });
        cx.on_app_quit(|this, _| {
            this.save_layout_now();
            this.quit_connections();
            // What is queued is written before the process ends, if the disk
            // lets it.
            this.conversation_logs.finish(Duration::from_secs(2));
            async {}
        })
        .detach();
        this.note_window_bounds(window);
        this
    }

    /// Sends QUIT on every connection and waits briefly for it to be
    /// written, since the process ends right after the window closes. The
    /// wait is bounded and only happens on the way out.
    fn quit_connections(&mut self) {
        let closing: Vec<_> = self
            .sessions
            .values_mut()
            .filter_map(ServerSession::close)
            .collect();
        // The UI no longer drains events and the queues may be full, so ask
        // the workers directly.
        for connection in &closing {
            connection.shutdown();
        }
        let deadline = std::time::Instant::now() + cayenchat_irc_core::QUIT_WAIT;
        for connection in closing {
            connection.wait_closed(deadline.saturating_duration_since(std::time::Instant::now()));
        }
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
            // The write waits for the disk, so it runs off the UI thread.
            let Ok(Some(write)) = this.update(cx, |this, _| this.take_layout_write()) else {
                return;
            };
            cx.background_executor().spawn(async move { write() }).await;
        }));
    }

    /// Writes the window's position and size (as last noted) and the pane
    /// sizes now, and returns once they are on disk; a write that is already
    /// under way finishes first. A failure only means the layout is not
    /// remembered, so it is not shown.
    fn save_layout_now(&mut self) {
        self.layout_save = None;
        if let Some(write) = self.take_layout_write() {
            write();
        }
    }

    /// The write of the layout as it is now, or `None` when it is not kept.
    /// Writes run one at a time and the newest one wins, so an older layout
    /// never replaces a newer one.
    fn take_layout_write(&mut self) -> Option<Box<dyn FnOnce() + Send>> {
        let (true, Some(path), Some(window_bounds)) = (
            self.restore_layout,
            self.layout_file.clone(),
            self.window_bounds,
        ) else {
            return None;
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
        self.layout_writes += 1;
        let number = self.layout_writes;
        let written = self.layout_written.clone();
        Some(Box::new(move || {
            let mut newest = written.lock().unwrap_or_else(|error| error.into_inner());
            if *newest > number {
                return;
            }
            *newest = number;
            if let Err(error) = cayenchat_storage::layout::save_layout_to(&path, &layout) {
                eprintln!("CayenChat: {error}");
            }
        }))
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

    /// A right click on `nickname` in a private conversation: nothing is
    /// offered on ourselves, and a menu left open for the peer is closed so it
    /// does not act on a row that is no longer the chosen one. Returns whether
    /// the click was ours.
    fn right_click_on_self(&mut self, private: bool, network: NetworkId, nickname: &str) -> bool {
        let own = private && self.is_own_nickname(network, nickname);
        if own {
            self.dismiss_menus();
        }
        own
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

    /// What the right-hand list shows for the selected conversation: a
    /// channel's members, or for a private conversation its two participants
    /// (us, then the peer). The peer stays listed after they quit, as long as
    /// the conversation exists.
    fn participants(&self) -> member_selection::Participants<'_> {
        selected_participants(&self.state, &self.sessions)
    }

    /// Mentions of our nickname and keywords in an incoming message, looked
    /// for in its text (the action of a `/me`) without formatting codes. Our
    /// own lines and replayed history have none.
    fn find_mentions(&self, network: NetworkId, sender: &str, text: &str, replayed: bool) -> Found {
        use cayenchat_irc_core::text::{action_text, mention_ranges, plain_ranges};

        if replayed || self.is_own_nickname(network, sender) {
            return Found::default();
        }
        let (offset, body) = match action_text(text) {
            // The prefix `action_text` removes.
            Some(action) => ("\u{1}ACTION ".len(), action),
            None => (0, text),
        };
        let own = self.own_nickname(network);
        let keywords = &self.notification_rules.keywords;
        let (mut mentioned, mut keyword) = (false, false);
        let ranges = plain_ranges(body, |plain| {
            let mut ranges = own
                .map(|own| mention_ranges(plain, own))
                .unwrap_or_default();
            // A nickname inside a link is part of the address, not a mention.
            if !ranges.is_empty() {
                let links = cayenchat_app::links::find(plain);
                // Both lists ascend, so one forward sweep suffices.
                let mut next = 0;
                ranges.retain(|range| {
                    while next < links.len() && links[next].0.end <= range.start {
                        next += 1;
                    }
                    links
                        .get(next)
                        .is_none_or(|(link, _)| range.end <= link.start)
                });
            }
            mentioned = !ranges.is_empty();
            let matches = notifications::keyword_ranges(plain, keywords);
            keyword = !matches.is_empty();
            ranges.extend(matches);
            ranges.sort_by_key(|range| range.start);
            ranges
        });
        let mut found = Found {
            mentioned,
            keyword,
            ranges,
        };
        for range in &mut found.ranges {
            *range = range.start + offset..range.end + offset;
        }
        found
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
            found,
            replayed,
        } = message;

        let Some(trigger) = self.notification_rules.trigger(IncomingMessage {
            channel: channel.is_some(),
            notice,
            from_self: self.is_own_nickname(network, sender),
            mentioned: found.mentioned,
            keyword: found.keyword,
            replayed,
        }) else {
            return;
        };
        let plain = display::neutralize_bidi_owned(match action_text(text) {
            Some(action) => format!("* {sender} {}", strip_formatting(action)),
            None => strip_formatting(text),
        });
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
            summary: display::neutralize_bidi_owned(summary),
            body: notifications::body_text(&plain),
            sound: self.notification_rules.sound,
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
                let worker = if first.is_none() {
                    Worker::Closed
                } else {
                    Worker::Running
                };
                let mut batch: Vec<Event> = first.into_iter().collect();
                while batch.len() < EVENT_BATCH_LIMIT
                    && let Some(event) = events.try_recv()
                {
                    batch.push(event);
                }
                let keep_going = this
                    .update(cx, |this, cx| {
                        this.is_current(network, generation)
                            && this.handle_events(network, batch, worker, cx)
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

    /// Gives every network its saved channel order (D041).
    fn restore_channel_orders(&mut self) {
        let orders: Vec<_> = self
            .sessions
            .iter()
            .map(|(id, session)| (*id, self.channel_orders.order(&session.profile_id).to_vec()))
            .collect();
        for (network, order) in orders {
            self.state.set_channel_order(network, order);
        }
    }

    /// Moves a channel or private conversation within its server's tree
    /// group and remembers the order of channels.
    fn move_conversation(&mut self, id: ConversationId, to: MoveTo, cx: &mut Context<Self>) {
        let Some(network) = self.state.move_conversation(id, to) else {
            return;
        };
        if let Some(profile_id) = self.sessions.get(&network).map(|s| s.profile_id.clone()) {
            self.channel_orders
                .set(&profile_id, self.state.channel_order(network));
            self.channel_orders
                .retain_servers(|id| self.sessions.values().any(|s| s.profile_id == id));
            if let Some(path) = &self.order_file
                && let Err(error) =
                    cayenchat_storage::order::save_orders_to(path, &self.channel_orders)
            {
                self.set_feedback(Some(error));
            }
        }
        cx.notify();
    }

    /// Moves a server one place up or down the tree by reordering the saved
    /// profiles.
    fn move_server(&mut self, up: bool, cx: &mut Context<Self>) {
        let Some(menu) = self.server_menu.take() else {
            return;
        };
        let Some(profile_id) = self
            .sessions
            .get(&menu.network)
            .map(|session| session.profile_id.clone())
        else {
            return;
        };
        // Start from the file, which a settings window may have updated.
        let mut settings = match settings_file::load() {
            Ok(Some(settings)) => settings,
            Ok(None) => self.saved.clone(),
            Err(error) => {
                self.set_feedback(Some(error));
                cx.notify();
                return;
            }
        };
        let Some(at) = settings.servers.iter().position(|p| p.id == profile_id) else {
            return;
        };
        let to = if up { at.checked_sub(1) } else { Some(at + 1) };
        let Some(to) = to.filter(|to| *to < settings.servers.len()) else {
            return;
        };
        settings.servers.swap(at, to);
        match settings_file::save(&settings) {
            Ok(()) => self.apply_servers(settings, cx),
            Err(error) => {
                self.set_feedback(Some(error));
                cx.notify();
            }
        }
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
            if let std::collections::hash_map::Entry::Vacant(slot) = self.inputs.entry(key) {
                slot.insert(cx.new(|cx| TextInput::new_live(&placeholder, cx)));
                self.watch_input(key, cx);
            }
        }
    }

    /// Replaces the warning. Whatever it said before, it no longer belongs to
    /// a failed send, even when a later failure words it the same.
    fn set_feedback(&mut self, feedback: Option<String>) {
        self.send_failure = None;
        self.feedback = feedback;
    }

    /// Clears the warning of a failed send once its draft is edited.
    fn watch_input(&mut self, key: Selection, cx: &mut Context<Self>) {
        let input = self.inputs[&key].clone();
        cx.observe(&input, move |this, input, cx| {
            let Some(failure) = &this.send_failure else {
                return;
            };
            if failure.selection != key || input.read(cx).text() == failure.text {
                return;
            }
            if this.feedback.as_deref() == Some(failure.message.as_str()) {
                this.set_feedback(None);
            }
            this.send_failure = None;
            cx.notify();
        })
        .detach();
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
                    // The auto-join list too: reconnects join the saved one.
                    config.channels = profile.channels();
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
        self.restore_channel_orders();
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
                connection.set_transcript(self.debug_enabled);
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.irc = Some(connection);
                }
                self.set_feedback(None);
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
            let result = connection.disconnect();
            self.set_feedback(result.err());
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
                    self.set_feedback(Some(error));
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
        self.set_feedback(None);
        match Connection::connect(config) {
            Ok(connection) => {
                connection.set_transcript(self.debug_enabled);
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
        // Only a lost registered connection marks the conversations; failed
        // retries would repeat the line in every log.
        let was_registered = self.state.is_registered(network);
        self.state
            .set_status(network, ConnectionStatus::Disconnected(reason.clone()));
        let message = self
            .i18n
            .format("status_disconnected", &[("reason", &reason)]);
        if was_registered {
            self.state.append_network_activity(network, &message);
        }
        self.state.append_server_message(network, message.clone());
        // Another server's disconnect is shown in its tree row and log only.
        if self.selected_network_id() == Some(network) {
            self.set_feedback(Some(message));
        }
    }

    fn member_command(&mut self, command: MemberCommand, cx: &mut Context<Self>) {
        let Some(menu) = self.member_menu.take() else {
            return;
        };
        if command == MemberCommand::Whois {
            let result = self.request_whois(menu.network, &menu.nickname, cx);
            self.set_feedback(result.err());
            cx.notify();
            return;
        }
        let result = self
            .registered_connection(menu.network)
            .and_then(|connection| connection.send_member_command(&menu.nickname, command));
        self.set_feedback(result.err());
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
        let Some(conversation) = self.state.conversation(id) else {
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
        let feedback = if group.is_empty() {
            None
        } else {
            self.registered_connection(menu.network)
                .and_then(|connection| connection.send_member_modes(&menu.channel, mode, &group))
                .err()
        };
        self.set_feedback(feedback);
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
        let key = self.state.casemapping(network).fold(nickname);
        if let Some(session) = self.sessions.get_mut(&network) {
            session.pending_whois.insert(key);
        }
        cx.notify();
        Ok(())
    }

    fn joined_channels(&self, network: NetworkId) -> JoinedChannels {
        let mapping = self.state.casemapping(network);
        JoinedChannels {
            mapping,
            names: self
                .state
                .conversations()
                .iter()
                .filter(|conversation| {
                    conversation.network == network
                        && !conversation.is_private()
                        && self.state.is_active_channel(conversation.id)
                })
                .map(|conversation| mapping.fold(&conversation.name))
                .collect(),
        }
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
        let feedback = if join {
            self.join_channel(menu.network, &menu.channel, cx).err()
        } else {
            self.registered_connection(menu.network)
                .and_then(|connection| {
                    connection.send_command(&format!("/part {}", menu.channel), None)
                })
                .err()
        };
        self.set_feedback(feedback);
        cx.notify();
    }

    /// Whether `channel` is an enabled auto-join entry of `network`'s server.
    fn auto_join_enabled(&self, network: NetworkId, channel: &str) -> bool {
        let mapping = self.state.casemapping(network);
        self.sessions
            .get(&network)
            .and_then(|session| self.saved.profile(&session.profile_id))
            .is_some_and(|profile| {
                profile
                    .channels()
                    .iter()
                    .any(|name| mapping.same(name, channel))
            })
    }

    /// Adds (enables) or disables the menu's channel in its server's saved
    /// auto-join list. Only the saved configuration changes: nothing is
    /// joined or parted now, and a disabled entry stays in the list.
    fn toggle_auto_join(&mut self, add: bool, cx: &mut Context<Self>) {
        let Some(menu) = self.channel_menu.take() else {
            return;
        };
        let Some(profile_id) = self
            .sessions
            .get(&menu.network)
            .map(|session| session.profile_id.clone())
        else {
            return;
        };
        let mapping = self.state.casemapping(menu.network);
        // Start from the file, which a settings window may have updated.
        let mut settings = match settings_file::load() {
            Ok(Some(settings)) => settings,
            Ok(None) => self.saved.clone(),
            Err(error) => {
                // Saving over a file that cannot be read could destroy it.
                self.set_feedback(Some(error));
                cx.notify();
                return;
            }
        };
        let Some(profile) = settings.servers.iter_mut().find(|p| p.id == profile_id) else {
            return;
        };
        let mut entries = profile.auto_join_entries();
        let found = entries
            .iter_mut()
            .filter(|entry| mapping.same(&entry.name, &menu.channel))
            .map(|entry| entry.enabled = add)
            .count();
        if found == 0 && add {
            entries.push(AutoJoinEntry {
                name: menu.channel.clone(),
                enabled: true,
            });
        }
        profile.set_auto_join_entries(&entries);
        let feedback = match settings_file::save(&settings) {
            Ok(()) => {
                if let Some(saved) = self.saved.servers.iter_mut().find(|p| p.id == profile_id) {
                    saved.channels = settings
                        .servers
                        .iter()
                        .find(|p| p.id == profile_id)
                        .map(|p| p.channels.clone())
                        .unwrap_or_default();
                }
                // The next reconnect joins the new list; nothing is sent now.
                if let (Some(config), Some(profile)) = (
                    self.sessions
                        .get_mut(&menu.network)
                        .and_then(|session| session.active_config.as_mut()),
                    settings.servers.iter().find(|p| p.id == profile_id),
                ) {
                    config.channels = profile.channels();
                }
                None
            }
            Err(error) => Some(error),
        };
        self.set_feedback(feedback);
        cx.notify();
    }

    fn move_menu_conversation(&mut self, to: MoveTo, cx: &mut Context<Self>) {
        if let Some(menu) = self.channel_menu.take() {
            self.move_conversation(menu.conversation, to, cx);
        }
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

    /// Remembers the channels of a `/join` command just sent, so a refusal of
    /// one that has no conversation yet can still be shown. Bounded.
    fn note_join_request(&mut self, network: NetworkId, command: &str) {
        let mut words = command.split_whitespace();
        if !words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("/join"))
        {
            return;
        }
        let Some(channels) = words.next() else {
            return;
        };
        let casemapping = self.state.casemapping(network);
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        for channel in channels.split(',').filter(|name| valid_channel(name)) {
            if session.pending_joins.len() >= 32 {
                session.pending_joins.clear();
            }
            session.pending_joins.insert(casemapping.fold(channel));
        }
    }

    fn join_channel(
        &mut self,
        network: NetworkId,
        channel: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.registered_connection(network)?
            .send_command(&format!("/join {channel}"), None)?;
        self.note_join_request(network, &format!("/join {channel}"));
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
        let key = (
            network,
            self.state.casemapping(network).fold(&info.nickname),
        );
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
                let message = self.i18n.format("whois_open_failed", &[("error", &error)]);
                self.set_feedback(Some(message));
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
        self.set_feedback(None);
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
        self.set_feedback(None);
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
                if let Some(prompt) = self
                    .member_prompt
                    .take()
                    .filter(|prompt| matches!(prompt.kind, MemberPromptKind::Join))
                {
                    self.note_join_request(prompt.network, &format!("/join {}", value.trim()));
                }
                self.set_feedback(None);
                window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
            }
            Err(error) => self.set_feedback(Some(error)),
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
                        settings.show_tab(tab, cx);
                        cx.notify();
                    } else if let Some(profile) = profile.clone() {
                        settings.show_tab(tab, cx);
                        settings.select_server(profile, cx);
                    } else {
                        // The system may have changed it while the window was away.
                        settings.refresh_autostart(cx);
                    }
                    window.activate_window()
                })
                .is_ok()
        {
            return;
        }
        let mut settings = match settings_file::load() {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                // The startup notice is this error plus where a copy was kept.
                let message = match &self.startup_notice {
                    Some(notice) if notice.starts_with(&error) => notice.clone(),
                    _ => error,
                };
                self.set_feedback(Some(message));
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
                let message = self
                    .i18n
                    .format("settings_open_failed", &[("error", &error.to_string())]);
                self.set_feedback(Some(message))
            }
        }
        cx.notify();
    }

    fn toggle_debug(&mut self, _: &ToggleDebug, window: &mut Window, cx: &mut Context<Self>) {
        if !self.debug_enabled {
            self.show_diagnostics(window, cx);
            return;
        }
        self.set_debug(false, cx);
        cx.notify();
    }

    /// Turns the debug transcript on or off. IRC lines are recorded after
    /// registration only while it is on.
    fn set_debug(&mut self, on: bool, cx: &mut Context<Self>) {
        self.debug_enabled = on;
        cx.set_menus(app_menus(on, &self.i18n));
        for connection in self
            .sessions
            .values()
            .filter_map(|session| session.irc.as_ref())
        {
            connection.set_transcript(on);
        }
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
        self.set_debug(true, cx);
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
        let Some(conversation) = self.state.conversation(id) else {
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
        let decode = self.appearance.decode_char_refs;
        let pieces: Vec<_> = (start.row..=end.row)
            .filter_map(|row| {
                channel.messages.get(row).map(|message| {
                    let range = selection.range(row, message.text.len()).unwrap_or(0..0);
                    if !decode {
                        return message.text[range].to_owned();
                    }
                    // Decoded references are copied as the characters shown.
                    compact_urls::Compact::new(&message.text, message.links.as_slice(), false, true)
                        .copy_text(&message.text, range)
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

    fn clear_member_selection(&mut self, cx: &mut Context<Self>) {
        self.member_selection.clear();
        cx.notify();
    }

    /// Applies a click on row `index` of the selected channel's member list.
    fn click_member(&mut self, index: usize, click: member_selection::Click) {
        let Some(channel) = self.state.selected_channel() else {
            return;
        };
        let conversation = channel.id;
        let members = selected_participants(&self.state, &self.sessions);
        self.member_selection
            .click(conversation, &members, index, click);
    }

    /// The standard list keys while the channel tree has focus. Moving
    /// selects as it goes and keeps focus on the tree; Enter selects and
    /// returns to the draft.
    fn tree_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        use list_keys::ListKey;
        if event.keystroke.modifiers.modified() {
            return;
        }
        let Some(key) = ListKey::from_key(event.keystroke.key.as_str()) else {
            return;
        };
        let selection = self.state.selection();
        let current = self.tree_rows.iter().position(|row| match *row {
            TreeRow::Server(id) => selection == Selection::Server(id),
            TreeRow::Channel(id) => selection == Selection::Channel(id),
        });
        let target = match (key, current) {
            (ListKey::Parent, Some(at)) => self.tree_rows[..at]
                .iter()
                .rposition(|row| matches!(row, TreeRow::Server(_)))
                .filter(|_| matches!(self.tree_rows[at], TreeRow::Channel(_))),
            (ListKey::Child, Some(at)) => (at + 1 < self.tree_rows.len()
                && matches!(self.tree_rows[at], TreeRow::Server(_))
                && matches!(self.tree_rows[at + 1], TreeRow::Channel(_)))
            .then_some(at + 1),
            (ListKey::Activate, at) => at,
            (ListKey::Parent | ListKey::Child, None) => None,
            _ => {
                let page =
                    (f32::from(self.tree_list.state.viewport_bounds().size.height) / 22.) as usize;
                list_keys::target(current, self.tree_rows.len(), key, page)
            }
        };
        cx.stop_propagation();
        let Some(row) = target.and_then(|at| self.tree_rows.get(at).copied().map(|r| (at, r)))
        else {
            return;
        };
        let command = match row.1 {
            TreeRow::Server(id) => Command::SelectServer(id),
            TreeRow::Channel(id) => Command::SelectChannel(id),
        };
        if key == ListKey::Activate {
            self.dispatch(command, window, cx);
        } else {
            self.dispatch_keeping_focus(command, window, cx);
            self.tree_list.state.scroll_to_reveal_item(row.0);
        }
    }

    /// The standard list keys while the member list has focus: they move
    /// the one chosen member.
    fn members_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let Some(key) = list_keys::ListKey::from_key(event.keystroke.key.as_str()) else {
            return;
        };
        let Some(channel) = self.state.selected_channel() else {
            return;
        };
        cx.stop_propagation();
        let conversation = channel.id;
        let members = selected_participants(&self.state, &self.sessions);
        let current = self.member_selection.anchor_index(conversation, &members);
        // The measured row height and the pixel range now in view.
        let (row_height, view_top, view_bottom) = {
            let state = self.members_scroll.0.borrow();
            let row_height = state
                .last_item_size
                .map_or(22., |size| {
                    f32::from(size.contents.height) / members.len() as f32
                })
                .max(1.);
            let top = -f32::from(state.base_handle.offset().y);
            let height = f32::from(state.base_handle.bounds().size.height);
            (row_height, top, top + height)
        };
        let page = ((view_bottom - view_top) / row_height) as usize;
        if let Some(index) = list_keys::target(current, members.len(), key, page) {
            self.member_selection.click(
                conversation,
                &members,
                index,
                member_selection::Click::Only,
            );
            // Scrolls only when the row is out of view, by the least amount:
            // a row above the view lands at the top, one below at the bottom.
            let (row_top, row_bottom) =
                (index as f32 * row_height, (index + 1) as f32 * row_height);
            if row_top < view_top {
                self.members_scroll
                    .scroll_to_item(index, ScrollStrategy::Top);
            } else if row_bottom > view_bottom {
                self.members_scroll
                    .scroll_to_item(index, ScrollStrategy::Bottom);
            }
            cx.notify();
        }
    }

    fn push_diagnostic(&mut self, network: NetworkId, line: String) {
        if let Some(session) = self.sessions.get_mut(&network) {
            session.push_diagnostic(line);
        }
    }

    fn dispatch(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch_keeping_focus(command, window, cx);
        window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
    }

    /// [`Self::dispatch`] without moving focus to the draft, for the lists
    /// that are driven from the keyboard.
    fn dispatch_keeping_focus(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.state.dispatch(command);
        self.log_selection = None;
        self.set_feedback(None);
        self.update_title(window);
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

    /// Applies a batch of worker events. [`Worker::Closed`] means the
    /// worker's event stream ended. Returns whether to keep listening.
    fn handle_events(
        &mut self,
        network: NetworkId,
        batch: Vec<Event>,
        worker: Worker,
        cx: &mut Context<Self>,
    ) -> bool {
        let worker_closed = worker == Worker::Closed;
        let changed = !batch.is_empty();
        self.announce_conversation_log();
        let mut disconnected = false;
        let mut refused = false;
        // The IRCv3 settings tab shows our own avatar and whether it can
        // be published; it is redrawn only when that may have changed.
        let own_avatar_changed = batch.iter().any(|event| {
            matches!(
                event,
                Event::Registered { .. }
                    | Event::Disconnected(_)
                    | Event::Closed(_)
                    | Event::Refused(_)
                    | Event::AvatarsReset
                    | Event::MetadataReady
                    | Event::OwnAvatar { .. }
                    | Event::OwnAvatarFailed { .. }
            )
        });
        let superseded = superseded_rosters(&batch);
        for (index, event) in batch.into_iter().enumerate() {
            if superseded.contains(&index)
                && let Event::Names { channel, users } = &event
            {
                // A later roster of the channel in this batch replaces this
                // one (large channels republish on every JOIN and PART);
                // only who left in between still matters.
                if let Some(id) = self.state.channel_id(network, channel) {
                    self.member_selection.retain_present(id, users);
                }
                continue;
            }
            match &event {
                Event::Disconnected(_) => disconnected = true,
                // Disconnect or /quit: the user ended it, so no reconnecting.
                Event::Closed(_) => {
                    disconnected = true;
                    if let Some(session) = self.sessions.get_mut(&network) {
                        session.manual_disconnect = true;
                    }
                }
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
            Event::CaseMapping(mapping) => self.state.set_casemapping(network, mapping),
            Event::TransportConnected => {
                // A new connection compares names as RFC 1459 until the
                // server advertises otherwise.
                self.state
                    .set_casemapping(network, names::CaseMapping::default());
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
                let folded = self.state.casemapping(network).fold(&channel);
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.pending_joins.remove(&folded);
                }
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
                server_time,
                msgid,
                account,
                replayed,
            } => {
                let found = self.find_mentions(network, &sender, &text, replayed);
                let meta = MessageMeta {
                    highlights: found.ranges.clone(),
                    ..irc_message_meta(
                        server_time,
                        msgid.as_deref(),
                        account.as_deref(),
                        event_provenance(replayed),
                    )
                };
                // A copy of a message the conversation already has (overlapping
                // history) neither notifies nor highlights again.
                if !self.state.append_channel_message_at(
                    network,
                    &channel,
                    &sender,
                    &text,
                    Kind::from_notice(notice),
                    meta,
                ) {
                    return;
                }
                let conversation = self.state.channel_id(network, &channel);
                if !replayed && conversation.is_some() {
                    self.log_message(
                        network,
                        Scope::Channel,
                        &channel,
                        &sender,
                        &text,
                        notice,
                        server_time,
                    );
                }
                self.notify_message(
                    network,
                    ReceivedMessage {
                        channel: Some(&channel),
                        conversation,
                        sender: &sender,
                        text: &text,
                        notice,
                        found: &found,
                        replayed,
                    },
                );
                if found.mentioned || found.keyword {
                    self.state.mark_highlighted(network, &channel);
                }
            }
            Event::ChannelActivity {
                channel,
                actor,
                kind,
                server_time,
            } => {
                self.log_channel_activity(network, &channel, &actor, &kind, server_time);
                let alert = kind.is_alert();
                let join_failed = matches!(kind, ChannelActivityKind::JoinFailed { .. });
                let text = channel_activity_text(&actor, kind);
                let meta = MessageMeta::at(server_time);
                let folded = self.state.casemapping(network).fold(&channel);
                let requested = join_failed
                    && self
                        .sessions
                        .get_mut(&network)
                        .is_some_and(|session| session.pending_joins.remove(&folded));
                if requested {
                    self.state
                        .append_requested_join_alert_at(network, &channel, text, meta);
                } else if alert {
                    self.state
                        .append_channel_alert_at(network, &channel, text, meta);
                } else {
                    self.state
                        .append_channel_activity_at(network, &channel, text, meta);
                }
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
                let found = self.find_mentions(network, &sender, &text, replayed);
                let meta = MessageMeta {
                    highlights: found.ranges.clone(),
                    ..irc_message_meta(
                        server_time,
                        msgid.as_deref(),
                        account.as_deref(),
                        event_provenance(replayed),
                    )
                };
                // A PRIVMSG opens a private conversation; a NOTICE (usually
                // services and bots) joins one only if it already exists,
                // and otherwise stays in the server log as before.
                let key = cayenchat_irc_core::text::nickname_key(&sender);
                let missing = if notice {
                    Missing::Ignore
                } else {
                    Missing::Create
                };
                let conversation = self
                    .state
                    .private_conversation(network, &key, &sender, missing);
                match conversation {
                    Some(id) => {
                        if !self.state.append_conversation_message(
                            id,
                            &sender,
                            &text,
                            Kind::from_notice(notice),
                            meta,
                            Unread::Mark,
                        ) {
                            return;
                        }
                        if !replayed {
                            self.log_message(
                                network,
                                Scope::Private,
                                &sender,
                                &sender,
                                &text,
                                notice,
                                server_time,
                            );
                        }
                        if !notice && !replayed {
                            self.state.highlight(id);
                        }
                    }
                    None => {
                        let prefix = if notice {
                            format!("-{sender}- ")
                        } else {
                            format!("<{sender}> ")
                        };
                        let meta = meta.after_prefix(prefix.len());
                        self.state
                            .append_server_message_at(network, prefix + &text, meta);
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
                        found: &found,
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
                let meta = irc_message_meta(
                    server_time,
                    msgid.as_deref(),
                    None,
                    event_provenance(replayed),
                );
                if !replayed {
                    let own = self.own_nickname(network).unwrap_or("me");
                    self.log_message(
                        network,
                        Scope::Private,
                        &target,
                        own,
                        &text,
                        notice,
                        server_time,
                    );
                }
                self.append_own_private(network, &target, &text, notice, meta);
            }
            Event::UserNickChanged { from, to } => {
                let from_key = cayenchat_irc_core::text::nickname_key(&from);
                if let Some(id) = self.state.private_id(network, &from_key) {
                    self.log_event(
                        network,
                        Scope::Private,
                        &from,
                        LogKind::Nick,
                        &from,
                        &to,
                        None,
                    );
                    let to_key = cayenchat_irc_core::text::nickname_key(&to);
                    self.state.rename_private(network, &from_key, &to_key, &to);
                    self.state
                        .append_conversation_activity(id, format!("{from} is now known as {to}"));
                }
            }
            Event::UserQuit { nickname, reason } => {
                let key = cayenchat_irc_core::text::nickname_key(&nickname);
                if let Some(id) = self.state.private_id(network, &key) {
                    let detail = reason.clone().unwrap_or_default();
                    self.log_event(
                        network,
                        Scope::Private,
                        &nickname,
                        LogKind::Quit,
                        &nickname,
                        &detail,
                        None,
                    );
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
                if let Some(id) =
                    self.state
                        .private_conversation(network, &key, &channel, Missing::Create)
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
                    && let Some(conversation) = self.state.conversation(id)
                {
                    self.member_selection
                        .retain_present(id, &conversation.members);
                }
            }
            Event::Topic {
                channel,
                topic,
                changed_by,
                server_time,
            } => {
                self.state.set_topic(network, &channel, &topic);
                if let Some(by) = changed_by
                    && self.state.channel_id(network, &channel).is_some()
                {
                    self.log_event(
                        network,
                        Scope::Channel,
                        &channel,
                        LogKind::Topic,
                        &by,
                        &topic,
                        server_time,
                    );
                }
            }
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
                let key = (
                    network,
                    self.state.casemapping(network).fold(&info.nickname),
                );
                let requested = self
                    .sessions
                    .get_mut(&network)
                    .is_some_and(|session| session.pending_whois.remove(&key.1));
                if requested && !info.found() && !self.whois_windows.contains_key(&key) {
                    let message = self.i18n.format_nickname("whois_not_found", &info.nickname);
                    self.set_feedback(Some(message));
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
                action,
            } => {
                let before = self.state.latest_sequence();
                if valid_channel(&channel) {
                    let nickname = self
                        .sessions
                        .get(&network)
                        .and_then(|session| session.own_nickname.as_deref())
                        .unwrap_or("me");
                    self.state.append_channel_message(
                        network,
                        &channel,
                        nickname,
                        &text,
                        Kind::from_notice(notice),
                        Provenance::Live,
                    );
                } else if cayenchat_irc_core::valid_nickname(&channel) {
                    self.append_own_private(network, &channel, &text, notice, MessageMeta::live());
                } else {
                    self.state.append_server_message(
                        network,
                        self.i18n
                            .format("event_message_queued", &[("channel", &channel)]),
                    );
                }
                // Our line is logged once the server's echo confirms it, or
                // at once when the server sends none.
                if self.state.latest_sequence() > before {
                    let scope = if valid_channel(&channel) {
                        Scope::Channel
                    } else {
                        Scope::Private
                    };
                    let own = self.own_nickname(network).unwrap_or("me");
                    let record = self.own_message_record(
                        network,
                        scope,
                        &channel,
                        own,
                        &text,
                        notice,
                        action.as_deref(),
                    );
                    match (local_id, record) {
                        (Some(local_id), Some(record)) => {
                            self.hold_own_message(network, local_id, record)
                        }
                        (None, record) => self.write_own_message(record),
                        _ => {}
                    }
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
                action,
                msgid,
                server_time,
            } => {
                self.release_own_message(
                    network,
                    local_id,
                    text.as_deref(),
                    action.as_deref(),
                    server_time,
                );
                let pending = self
                    .sessions
                    .get_mut(&network)
                    .and_then(|session| session.pending_sends.remove(&local_id));
                if let Some((id, sequence, notice)) = pending {
                    let meta =
                        irc_message_meta(server_time, msgid.as_deref(), None, Provenance::Live);
                    self.state
                        .confirm_message(id, sequence, text, Kind::from_notice(notice), meta);
                }
            }
            Event::OutgoingFailed { local_id, reason } => {
                self.drop_own_message(network, local_id);
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
            Event::Disconnected(reason) | Event::Closed(reason) | Event::Refused(reason) => {
                self.drop_own_messages(network);
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
        match self
            .state
            .private_conversation(network, &key, target, Missing::Create)
        {
            Some(id) => {
                self.state.append_conversation_message(
                    id,
                    &own,
                    text,
                    Kind::from_notice(notice),
                    meta,
                    Unread::Leave,
                );
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
        let private_peer = selected
            .filter(|channel| channel.is_private())
            .map(|channel| channel.name.clone());
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
        self.send_failure = None;
        self.feedback = match result {
            Ok(()) => {
                if let Some(network) = network {
                    self.note_join_request(network, &text);
                }
                self.input_history.record(&text, private_peer.as_deref());
                input.update(cx, |input, cx| input.clear_after_send(cx));
                None
            }
            Err(error) => {
                self.send_failure = Some(SendFailure {
                    selection,
                    text,
                    message: error.clone(),
                });
                Some(error)
            }
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

fn notification_rules(settings: &Notifications) -> NotificationRules {
    NotificationRules {
        enabled: settings.enabled,
        mentions: settings.mentions,
        keyword_alerts: settings.keyword_alerts,
        keywords: settings.keywords.clone(),
        private_messages: settings.private_messages,
        sound: settings.sound,
    }
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

/// One log row's text with its styling: links underlined in the link color,
/// shortened links on a tinted background, mentions and keywords bold in the
/// highlight color, and the selection shaded.
///
/// `prefix` (the sender and colon in the flowing layout) is drawn in the
/// nickname color before `text`; every other range, and `selected`, is a
/// byte offset into `text`.
#[allow(clippy::too_many_arguments)] // Each is one independent set of ranges.
fn styled_log_text(
    prefix: &str,
    text: &str,
    urls: &[(std::ops::Range<usize>, String)],
    chips: &[std::ops::Range<usize>],
    decoded: &[std::ops::Range<usize>],
    highlights: &[std::ops::Range<usize>],
    selected: Option<std::ops::Range<usize>>,
    theme: &Theme,
) -> StyledText {
    let shift = prefix.len();
    let moved = |range: &std::ops::Range<usize>| range.start + shift..range.end + shift;
    let urls: Vec<_> = urls.iter().map(|(range, _)| moved(range)).collect();
    let chips: Vec<_> = chips.iter().map(moved).collect();
    let decoded: Vec<_> = decoded.iter().map(moved).collect();
    let highlights: Vec<_> = highlights.iter().map(moved).collect();
    let selected = selected.as_ref().map(moved);
    let mut boundaries = vec![0, shift, shift + text.len()];
    for range in urls.iter().chain(&highlights).chain(&chips).chain(&decoded) {
        boundaries.extend([range.start, range.end]);
    }
    if let Some(range) = &selected {
        boundaries.extend([range.start, range.end]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let highlights = boundaries.windows(2).filter_map(|pair| {
        let range = pair[0]..pair[1];
        let is_prefix = range.end <= shift;
        let is_url = urls
            .iter()
            .any(|url| url.start <= range.start && range.end <= url.end);
        let is_chip = chips
            .iter()
            .any(|chip| chip.start <= range.start && range.end <= chip.end);
        let is_selected = selected
            .as_ref()
            .is_some_and(|selection| selection.start <= range.start && range.end <= selection.end);
        let is_highlight = highlights
            .iter()
            .any(|word| word.start <= range.start && range.end <= word.end);
        let is_decoded = decoded
            .iter()
            .any(|span| span.start <= range.start && range.end <= span.end);
        (is_prefix || is_url || is_chip || is_decoded || is_selected || is_highlight).then_some((
            range,
            HighlightStyle {
                color: if is_prefix {
                    Some(theme.nickname.into())
                } else if is_url {
                    Some(theme.panes.log_link.into())
                } else {
                    is_highlight.then_some(theme.panes.highlight.into())
                },
                font_weight: is_highlight.then_some(FontWeight::BOLD),
                underline: is_url.then_some(UnderlineStyle {
                    color: Some(theme.panes.log_link.into()),
                    thickness: px(1.),
                    wavy: false,
                }),
                background_color: if is_selected {
                    Some(theme.selected.into())
                } else if is_decoded {
                    // Neutral, so it is not mistaken for a link.
                    Some(
                        Rgba {
                            a: 0.22,
                            ..theme.text_secondary
                        }
                        .into(),
                    )
                } else {
                    link_background(theme, is_url, is_chip).map(Into::into)
                },
                ..Default::default()
            },
        ))
    });
    StyledText::new(format!("{prefix}{text}")).with_highlights(highlights)
}

/// The background behind a URL in a log: the configured color for any URL,
/// otherwise none, or a faint tint of the link color for a shortened one.
fn link_background(theme: &Theme, is_url: bool, is_chip: bool) -> Option<Rgba> {
    let tint = || Rgba {
        a: 0.14,
        ..theme.panes.log_link
    };
    match theme.panes.log_link_background {
        Some(background) if is_url || is_chip => Some(background),
        _ => is_chip.then(tint),
    }
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

/// The input method of the main log: the log holds no text, so it stands in
/// for the selected draft. Every call, queries and unmarking included, goes to
/// the draft so ranges and the composition state stay consistent, and anything
/// that changes text also moves focus there. Plain typing arrives here too
/// (unhandled key presses fall through to the input handler), so the first key
/// still reaches the platform input method. The handler is swapped only at the
/// next frame, so the calls before it keep arriving here.
impl EntityInputHandler for ChatWindow {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let input = self.draft_for_log_input()?;
        input.update(cx, |input, cx| {
            input.text_for_range(range, actual, window, cx)
        })
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let input = self.draft_for_log_input()?;
        input.update(cx, |input, cx| {
            input.selected_text_range(ignore_disabled_input, window, cx)
        })
    }

    fn marked_text_range(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let input = self.draft_for_log_input()?;
        input.update(cx, |input, cx| input.marked_text_range(window, cx))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.draft_for_log_input() {
            input.update(cx, |input, cx| input.unmark_text(window, cx));
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.draft_for_log_input() {
            window.focus(&input.focus_handle(cx));
            input.update(cx, |input, cx| {
                input.replace_text_in_range(range, text, window, cx)
            });
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.draft_for_log_input() {
            window.focus(&input.focus_handle(cx));
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(range, text, selected, window, cx)
            });
        }
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let input = self.draft_for_log_input()?;
        input.update(cx, |input, cx| {
            input.bounds_for_range(range, bounds, window, cx)
        })
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let input = self.draft_for_log_input()?;
        input.update(cx, |input, cx| {
            input.character_index_for_point(point, window, cx)
        })
    }
}

impl ChatWindow {
    /// An invisible element that registers this window as the input method
    /// while `focus` holds focus, so typing and IME composition in a pane that
    /// holds no text reach the draft.
    fn input_to_draft(&self, focus: &FocusHandle, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = focus.clone();
        let window_entity = cx.entity();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                window.handle_input(
                    &focus,
                    ElementInputHandler::new(bounds, window_entity.clone()),
                    cx,
                );
            },
        )
        .absolute()
        .size_full()
    }

    /// The draft that input arriving at the log is meant for.
    fn draft_for_log_input(&self) -> Option<Entity<TextInput>> {
        self.inputs.get(&self.state.selection()).cloned()
    }
}

impl ChatWindow {
    fn render_channel_tree(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::current(cx);
        // The tree is virtualized like the logs: it redraws on every state
        // change, but builds only the rows near the viewport. Keys identify
        // rows (server position, then conversation id) and follow the order
        // the user chose, so they need not ascend; rows that kept their
        // place keep their measured heights and the scroll.
        if !cx.has_active_drag() {
            self.dragging_channel = None;
            self.drop_target = None;
        }
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
        self.tree_list.sync_unordered(0, &keys);
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
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        list(
                            self.tree_list.state.clone(),
                            cx.processor(Self::render_tree_row),
                        )
                        .size_full(),
                    )
                    .child(scrollbar::scrollbar(
                        "scrollbar-channels",
                        &self.tree_list.state,
                        theme.text_muted,
                    )),
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
                    .child(format!("{}{}", self.network_label(network), status_mark))
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
                        this.dispatch_keeping_focus(Command::SelectServer(server_id), window, cx);
                    }))
                    .into_any_element()
            }
            Some(TreeRow::Channel(id)) => {
                let Some(conversation) = self.state.conversation(id) else {
                    return div().into_any_element();
                };
                let unread = self.state.is_unread(id);
                let highlighted = self.state.is_highlighted(id);
                let name = conversation.name.clone();
                let network = conversation.network;
                let private = conversation.is_private();
                let joined = self.state.is_active_channel(id);
                let conversations = self.state.conversations();
                let index = conversations.iter().position(|c| c.id == id).unwrap_or(0);
                // While a channel is dragged, every row shows the digit that
                // reaches it, and the row under the pointer shows where the
                // drop would put the dragged channel.
                let dragging = self.dragging_channel.is_some();
                let digit = shortcut_digit(index).filter(|_| dragging);
                let drop_line_on_top = self
                    .dragging_channel
                    .zip(self.drop_target)
                    .filter(|(dragged, target)| dragged != target && *target == id)
                    .and_then(|(dragged, _)| conversations.iter().position(|c| c.id == dragged))
                    .map(|from| from > index);
                let dragged = DraggedChannel {
                    id,
                    network,
                    private,
                    name: name.clone(),
                };
                let chat = cx.weak_entity();
                div()
                    .id(("channel", id.0))
                    .relative()
                    // The whole row width is the drop area, also while the
                    // digit gutter makes it a flex row.
                    .w_full()
                    .when(dragging, |d| d.flex().pl_1())
                    .when(!dragging, |d| d.pl_4())
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
                    .when(dragging, |d| {
                        d.child(
                            div()
                                .w(px(12.))
                                .flex_none()
                                .text_color(theme.text_muted)
                                .children(digit.map(|digit| digit.to_string())),
                        )
                    })
                    .child(format!(
                        "{}{}",
                        if unread { "● " } else { "" },
                        display::neutralize_bidi(&conversation.name)
                    ))
                    .when_some(drop_line_on_top, |d, on_top| {
                        d.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .h(px(3.))
                                .bg(theme.text)
                                .when(on_top, |line| line.top_0())
                                .when(!on_top, |line| line.bottom_0()),
                        )
                    })
                    .on_drag(dragged, move |dragged, _, _, cx| {
                        let (id, label) = (dragged.id, dragged.name.clone());
                        let _ = chat.update(cx, |this, cx| {
                            this.dragging_channel = Some(id);
                            this.drop_target = None;
                            cx.notify();
                        });
                        cx.new(|_| ChannelDragPreview::new(&label))
                    })
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<DraggedChannel>, _, cx| {
                            let dragged = event.drag(cx);
                            let target = (event.bounds.contains(&event.event.position)
                                && dragged.network == network
                                && dragged.private == private)
                                .then_some(id);
                            if target.is_some() && this.drop_target != target {
                                this.drop_target = target;
                                cx.notify();
                            } else if target.is_none() && this.drop_target == Some(id) {
                                this.drop_target = None;
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop(cx.listener(move |this, dragged: &DraggedChannel, _, cx| {
                        this.dragging_channel = None;
                        this.drop_target = None;
                        if dragged.network == network && dragged.private == private {
                            this.move_conversation(dragged.id, MoveTo::Place(id), cx);
                        }
                        cx.notify();
                    }))
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
                                        .min((viewport.height - px(110.)).max(px(0.))),
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
                        this.dispatch_keeping_focus(Command::SelectChannel(id), window, cx);
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
        let theme = theme::current(cx);
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
                let bar = scrollbar::scrollbar("scrollbar-main", &main.state, theme.text_muted);
                div()
                    .relative()
                    .size_full()
                    .child(
                        list(main.state.clone(), cx.processor(Self::render_main_row)).size_full(),
                    )
                    .child(bar)
                    .into_any_element()
            }
            PaneKind::SubLog => div()
                .relative()
                .size_full()
                .child(
                    list(
                        self.sub_list.state.clone(),
                        cx.processor(Self::render_sub_row),
                    )
                    .size_full(),
                )
                .child(scrollbar::scrollbar(
                    "scrollbar-sub",
                    &self.sub_list.state,
                    theme.text_muted,
                ))
                .into_any_element(),
            PaneKind::Members => {
                let member_count = self.participants().len();
                div()
                    .relative()
                    .size_full()
                    .child(
                        uniform_list(
                            "members",
                            member_count,
                            cx.processor(Self::render_member_rows),
                        )
                        .track_scroll(self.members_scroll.clone())
                        .size_full(),
                    )
                    .child(scrollbar::scrollbar(
                        "scrollbar-members",
                        &self.members_scroll,
                        theme.text_muted,
                    ))
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
            .child(self.input_to_draft(&self.log_focus, cx))
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
            .id("members-pane")
            .key_context("MemberList")
            .track_focus(&self.members_focus)
            .on_key_down(cx.listener(Self::members_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| window.focus(&this.members_focus)),
            )
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(theme.panes.member_list)
            .when(!appearance.member_font.is_empty(), |d| {
                d.font_family(appearance.member_font.clone())
            })
            .child(self.input_to_draft(&self.members_focus, cx))
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
            .child(
                div()
                    .id("channels-pane")
                    .key_context("ChannelTree")
                    .track_focus(&self.tree_focus)
                    .on_key_down(cx.listener(Self::tree_key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, _| window.focus(&this.tree_focus)),
                    )
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.input_to_draft(&self.tree_focus, cx))
                    .child(panes.channels.clone().cached(pane_style().w_full())),
            );

        let server_menu = self.server_menu.as_ref().map(|menu| {
            let network = menu.network;
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
                .occlude()
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
                .child(div().my_1().border_t_1().border_color(theme.separator))
                .children(
                    [(true, "tree_move_up"), (false, "tree_move_down")].map(|(up, key)| {
                        let position = self
                            .state
                            .networks()
                            .iter()
                            .position(|server| server.id == network);
                        let last = self.state.networks().len().saturating_sub(1);
                        let enabled =
                            position.is_some_and(|at| if up { at > 0 } else { at < last });
                        div()
                            .id(key)
                            .px_2()
                            .py_1()
                            .child(self.i18n.text(key))
                            .when(enabled, |d| {
                                d.cursor_pointer()
                                    .hover(|d| d.bg(theme.hover_strong))
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.move_server(up, cx)),
                                    )
                            })
                            .when(!enabled, |d| d.text_color(theme.text_muted))
                    }),
                )
        });
        let channel_menu = self.channel_menu.as_ref().map(|menu| {
            let registered = self.registered_connection(menu.network).is_ok();
            let mut popup = div()
                .id("channel-context-menu")
                .occlude()
                .w(px(176.))
                .p_1()
                .bg(theme.surface)
                .border_1()
                .border_color(border)
                .shadow_md();
            let moves = [
                (MoveTo::Up, "tree_move_up"),
                (MoveTo::Down, "tree_move_down"),
                (MoveTo::First, "tree_move_first"),
                (MoveTo::Last, "tree_move_last"),
            ]
            .map(|(to, key)| {
                div()
                    .id(key)
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.hover_strong))
                    .child(self.i18n.text(key))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.move_menu_conversation(to, cx)),
                    )
            });
            if menu.private {
                return popup
                    .child(
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
                    )
                    .child(div().my_1().border_t_1().border_color(theme.separator))
                    .children(moves);
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
            let auto_joined = self.auto_join_enabled(menu.network, &menu.channel);
            popup
                .child(div().my_1().border_t_1().border_color(theme.separator))
                .child(
                    div()
                        .id("channel-menu-auto-join")
                        .px_2()
                        .py_1()
                        .child(self.i18n.text(if auto_joined {
                            "auto_join_remove_from"
                        } else {
                            "auto_join_add_to"
                        }))
                        .when(registered, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(theme.hover_strong))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_auto_join(!auto_joined, cx)
                                }))
                        })
                        .when(!registered, |d| d.text_color(theme.text_muted)),
                )
                .child(div().my_1().border_t_1().border_color(theme.separator))
                .children(moves)
        });
        let member_menu = self.member_menu.as_ref().map(|menu| {
            let mut popup = div()
                .id("member-context-menu")
                .occlude()
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
            let choices = [
                (MemberMenuChoice::Whois, "member_whois"),
                (MemberMenuChoice::PrivateMessage, "member_private_message"),
                (MemberMenuChoice::Invite, "member_invite"),
                (MemberMenuChoice::GiveOp, "member_give_op"),
                (MemberMenuChoice::Deop, "member_deop"),
            ];
            let shown = member_menu_choice_count(menu.private, choices.len());
            for (index, (choice, key)) in choices.into_iter().take(shown).enumerate() {
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
            let title = member_prompt_title(&self.i18n, prompt.kind, &prompt.nickname);
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
            .on_action(|_: &Quit, _, cx| cx.quit())
            .on_action(cx.listener(Self::paste_image))
            .child(left)
            .child(right_split)
            .child(right)
            .when_some(
                self.server_menu.as_ref().zip(server_menu),
                |d, (at, menu)| d.child(snapped_menu(at.position, menu)),
            )
            .when_some(
                self.member_menu.as_ref().zip(member_menu),
                |d, (at, menu)| d.child(snapped_menu(at.position, menu)),
            )
            .when_some(
                self.channel_menu.as_ref().zip(channel_menu),
                |d, (at, menu)| d.child(snapped_menu(at.position, menu)),
            )
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
                .format_nickname("nick_prompt_title", &prompt.rejected);
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

/// Places a context menu at the pointer, moving it back inside the window when
/// it would be cut off at the bottom or right edge (issue #144).
fn snapped_menu(position: Point<Pixels>, menu: impl IntoElement) -> impl IntoElement {
    deferred(
        anchored()
            .position(position)
            .snap_to_window_with_margin(px(4.))
            .child(menu),
    )
    .with_priority(1)
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
    notice_color: Rgba,
    sub_alt: Rgba,
    time_font: SharedString,
    alternate_rows: bool,
    compact_urls: bool,
    decode_char_refs: bool,
    reiwa_mode: bool,
}

impl LogStyle {
    fn new(appearance: &Appearance, theme: Theme) -> Self {
        Self {
            theme,
            main_alt: theme.panes.main_alternate,
            event_color: theme.panes.channel_event,
            notice_color: theme.panes.notice,
            sub_alt: theme.panes.sub_alternate,
            // Built for every visible row on each redraw; the default font
            // name needs no allocation.
            time_font: if appearance.time_font.is_empty() {
                SharedString::new_static(default_time_font())
            } else {
                appearance.time_font.clone().into()
            },
            alternate_rows: appearance.alternate_rows,
            compact_urls: appearance.compact_urls,
            decode_char_refs: appearance.decode_char_refs,
            reiwa_mode: appearance.reiwa_mode,
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
                        .and_then(|session| session.diagnostics.get(index))
                        .map(|line| diagnostic_text(line))
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
        let urls = message.links.as_slice();
        // Channel activity lines stay text-only.
        let preview = if message.activity {
            None
        } else {
            self.previews.lookup(
                urls,
                previews::RowRef {
                    selection: Selection::Channel(selected_channel),
                    sequence: message.sequence,
                },
            )
        };
        if matches!(preview, Some((_, previews::Shown::Pending))) {
            self.pump_previews(cx);
        }
        // Drawn text may differ from the message's; positions map back.
        let compact = Rc::new(compact_urls::Compact::new(
            &message.text,
            urls,
            style.compact_urls,
            style.decode_char_refs,
        ));
        let full_urls: Vec<_> = compact
            .shortened(urls)
            .map(|(range, url)| (range, SharedString::from(url.to_owned())))
            .collect();
        let originals: Vec<_> = compact
            .decoded(&message.text)
            .map(|(range, original)| (range, SharedString::from(original.to_owned())))
            .collect();
        let urls = compact.shown_urls(urls);
        let chips: Vec<_> = full_urls.iter().map(|(range, _)| range.clone()).collect();
        let decoded: Vec<_> = originals.iter().map(|(range, _)| range.clone()).collect();
        let selected_range = self
            .log_selection
            .filter(|selection| selection.channel == selected_channel)
            .and_then(|selection| selection.range(index, message.text.len()))
            .map(|range| compact.shown_range(range));
        let highlights: Vec<_> = message
            .highlights
            .as_slice()
            .iter()
            .map(|range| compact.shown_range(range.clone()))
            .collect();
        // Default layout: the nickname flows into the message text, so
        // wrapped lines return to the left edge of the text column. Reiwa
        // mode shows it on its own first line instead.
        let reiwa = style.reiwa_mode && !message.activity;
        let prefix = if message.activity || reiwa {
            String::new()
        } else {
            format!("{}: ", display::neutralize_bidi(&message.sender))
        };
        let prefix_len = prefix.len();
        let styled = styled_log_text(
            &prefix,
            compact.text(),
            &urls,
            &chips,
            &decoded,
            &highlights,
            selected_range,
            &style.theme,
        );
        let layout = styled.layout().clone();
        let down_layout = layout.clone();
        let move_layout = layout.clone();
        let click_layout = layout;
        let move_urls = urls.clone();
        let (down_compact, move_compact) = (compact.clone(), compact.clone());
        let over_url = self.url_hover == Some((selected_channel, index));
        let text_len = compact.text().len();
        // Only rows with decoded references pay for the bubble's texts.
        let labels = (!originals.is_empty()).then(|| {
            (
                self.i18n.text("char_ref_origin"),
                self.i18n.text("copy_original_text"),
            )
        });
        let styled = if full_urls.is_empty() && originals.is_empty() {
            styled.into_any_element()
        } else {
            // The full URL of a shortened one shows on hover; clicking it
            // opens the URL. A decoded character reference shows its
            // original text, which a button copies.
            InteractiveText::new(("message-urls", index), styled)
                .hoverable_tooltip(
                    {
                        let full_urls = full_urls.clone();
                        let originals = originals.clone();
                        move |byte| {
                            let byte = byte.checked_sub(prefix_len)?;
                            let (range, _) = full_urls
                                .iter()
                                .chain(&originals)
                                .find(|(range, _)| range.contains(&byte))?;
                            Some(range.start + prefix_len..range.end + prefix_len)
                        }
                    },
                    move |range, _, cx| {
                        let start = range.start - prefix_len;
                        if let Some((_, original)) =
                            originals.iter().find(|(range, _)| range.start == start)
                        {
                            let (title, copy) = labels.clone().unwrap_or_default();
                            let original = original.clone();
                            return cx
                                .new(|_| ircv3_settings::CharRefTooltip::new(original, title, copy))
                                .into();
                        }
                        let url = full_urls
                            .iter()
                            .find(|(url, _)| url.start == start)
                            .map(|(_, url)| url.clone())
                            .unwrap_or_default();
                        cx.new(|_| ircv3_settings::UrlTooltip(url)).into()
                    },
                )
                .tooltip_bridge(
                    ircv3_settings::URL_TOOLTIP_BRIDGE.0,
                    ircv3_settings::URL_TOOLTIP_BRIDGE.1,
                )
                .into_any_element()
        };
        let avatar = (!message.activity && self.avatars.enabled()).then(|| {
            self.avatar_slot(
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
                if reiwa {
                    avatars::SLOT * 2.
                } else {
                    avatars::SLOT
                },
                cx,
            )
        });
        // One element for the whole message, so both lines of Reiwa mode
        // share the row's alternating background.
        let row = div()
            .w_full()
            .flex()
            .items_start()
            .gap_1()
            .py(px(1.))
            .when(style.alternate_rows && index % 2 == 1, |d| {
                d.bg(style.main_alt)
            });
        let row = if reiwa {
            row.children(avatar)
        } else {
            row.child(style.time(message.time)).children(avatar)
        };
        let body = {
            let text = div()
                .id(("message-text", index))
                .debug_selector(move || format!("message-text-{index}"))
                .when(preview.is_none(), |d| d.flex_1())
                .min_w_0()
                .when(message.activity, |d| d.text_color(style.event_color))
                .when(message.alert, |d| d.text_color(theme.warning))
                .when(message.notice, |d| d.text_color(style.notice_color))
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
                            .saturating_sub(prefix_len)
                            .min(text_len);
                        let byte = down_compact.original(byte);
                        this.start_log_selection(selected_channel, index, byte, window, cx);
                    }),
                )
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                    let position = move_layout.index_for_position(event.position);
                    let raw = position.unwrap_or_else(|index| index);
                    let byte = raw.saturating_sub(prefix_len).min(text_len);
                    this.extend_log_selection(
                        selected_channel,
                        index,
                        move_compact.original(byte),
                        cx,
                    );
                    // Over a URL (not while selecting) the pointer is a
                    // hand: a double click opens it.
                    let hover = (event.pressed_button.is_none()
                        && position.is_ok()
                        && raw >= prefix_len
                        && move_urls.iter().any(|(range, _)| range.contains(&byte)))
                    .then_some((selected_channel, index));
                    if this.url_hover != hover {
                        this.url_hover = hover;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                    if event.click_count() == 2 {
                        // A click on the nickname prefix is not on a link.
                        let Some(byte) = click_layout
                            .index_for_position(event.position())
                            .unwrap_or_else(|index| index)
                            .checked_sub(prefix_len)
                        else {
                            return;
                        };
                        if let Some((_, url)) = urls.iter().find(|(range, _)| range.contains(&byte))
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
        };
        if reiwa {
            // Line one: nickname and time; line two: the message.
            row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .text_color(theme.nickname)
                                    .child(display::neutralize_bidi(&message.sender).into_owned()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .font_family(style.time_font.clone())
                                    .text_color(theme.time)
                                    .child(message.time.to_string()),
                            ),
                    )
                    .child(body),
            )
        } else {
            row.child(body)
        }
        .into_any_element()
    }

    /// The fixed avatar slot of a message or member row: the image when it
    /// is ready, blank while it loads, and the nickname's default avatar
    /// when there is none or it failed. It never changes the row's height.
    fn avatar_slot(
        &self,
        avatar: Option<Arc<str>>,
        nickname: &str,
        size: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let slot = div()
            .w(px(size))
            .h(px(size))
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
        let Some(conversation) = self.state.conversation(id) else {
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
            .map(|network| self.network_label(network))
            .unwrap_or_default();
        // Same flow as the main log's default layout: channel, network and
        // nickname lead the text, so wrapped lines return to the text column.
        let prefix = if message.activity {
            format!(
                "{} [{network}] ",
                display::neutralize_bidi(&conversation.name)
            )
        } else {
            format!(
                "{} [{network}] {}: ",
                display::neutralize_bidi(&conversation.name),
                display::neutralize_bidi(&message.sender)
            )
        };
        // Long URLs and character references are drawn as in the channel
        // log, but stay plain text.
        let compact = compact_urls::Compact::new(
            &message.text,
            message.links.as_slice(),
            style.compact_urls,
            style.decode_char_refs,
        );
        let decoded: Vec<_> = compact
            .decoded(&message.text)
            .map(|(range, _)| range)
            .collect();
        let highlights: Vec<_> = message
            .highlights
            .as_slice()
            .iter()
            .map(|range| compact.shown_range(range.clone()))
            .collect();
        let styled = styled_log_text(
            &prefix,
            compact.text(),
            &[],
            &[],
            &decoded,
            &highlights,
            None,
            &style.theme,
        );
        div()
            .id(("sub-message", row))
            .w_full()
            .flex()
            .items_start()
            .gap_1()
            .py(px(1.))
            .when(style.alternate_rows && row % 2 == 1, |d| {
                d.bg(style.sub_alt)
            })
            .cursor_pointer()
            .hover(|d| d.bg(theme.hover))
            .child(style.time(message.time))
            .child(
                div()
                    .debug_selector(move || format!("sub-text-{row}"))
                    .flex_1()
                    .min_w_0()
                    .when(message.activity, |d| d.text_color(style.event_color))
                    .when(message.alert, |d| d.text_color(theme.warning))
                    .when(message.notice, |d| d.text_color(style.notice_color))
                    .child(styled),
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
        let participants = self.participants();
        let end = range.end.min(participants.len());
        let start = range.start.min(end);
        let network = channel.network;
        let conversation = channel.id;
        let private = channel.is_private();
        let avatars_shown = self.avatars.enabled();
        (start..end)
            .map(|index| {
                let member = participants.get(index).cloned().unwrap_or_default();
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
                        row.flex().items_center().gap_1().child(self.avatar_slot(
                            avatar,
                            &nickname,
                            avatars::SLOT,
                            cx,
                        ))
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
                            .child(display::neutralize_bidi(&member).into_owned()),
                    )
                    // A shortened name is read in full on hover.
                    .tooltip({
                        let full: SharedString =
                            display::neutralize_bidi(&member).into_owned().into();
                        move |_, cx| {
                            let full = full.clone();
                            cx.new(|_| ircv3_settings::TextTooltip(full)).into()
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            // The row blocks the pane's own handler below it.
                            window.focus(&this.members_focus);
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
                            // Nothing to offer on ourselves in a private
                            // conversation.
                            if this.right_click_on_self(private, network, &nickname) {
                                cx.stop_propagation();
                                cx.notify();
                                return;
                            }
                            // On one of several chosen members the menu acts
                            // on all of them; participants of a private
                            // conversation have no channel modes to give.
                            let group = if private {
                                Vec::new()
                            } else {
                                let participants = this.participants();
                                this.state
                                    .selected_channel()
                                    .map(|channel| {
                                        this.member_selection.nicknames(channel.id, &participants)
                                    })
                                    .filter(|chosen| chosen.len() >= 2)
                                    .unwrap_or_default()
                            };
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
                                private,
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
fn select_linux_display(saved: cayenchat_storage::LinuxDisplay) {
    use cayenchat_storage::LinuxDisplay;
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
        Err(error) => {
            // The next save replaces the file, so keep what could not be read.
            let error = match cayenchat_storage::keep_unreadable_copy() {
                Ok(copy) => format!("{error}\nA copy was kept at {}.", copy.display()),
                Err(_) => error,
            };
            return (Settings::default(), Some(error));
        }
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
    #[cfg(target_os = "linux")]
    let signal_quit = signals::install();
    Application::new().run(move |cx: &mut App| {
        #[cfg(target_os = "linux")]
        if let Some(receiver) = signal_quit {
            signals::quit_on_signal(receiver, cx);
        }
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
        let min_size = size(px(MIN_WINDOW_SIZE.0), px(MIN_WINDOW_SIZE.1));
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
                chat.order_file = cayenchat_storage::order::order_path().ok();
                chat.channel_orders = cayenchat_storage::order::load_orders();
                chat.restore_channel_orders();
                chat.apply_layout(&layout);
                cx.set_menus(app_menus(false, &chat.i18n));
                let startup = std::mem::take(&mut chat.startup_connections);
                let mut settings_for = startup.is_empty().then(|| chat.selected_profile_id());
                for (network, config) in startup {
                    let failed = match config {
                        Ok(config) => chat.apply_connection(network, config, window, cx).is_err(),
                        Err(error) => {
                            let message = chat.i18n.format("startup_invalid", &[("error", &error)]);
                            chat.set_feedback(Some(message));
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
        let settings_opened = settings_for.is_some();
        if let Some(profile) = settings_for {
            chat_window
                .update(cx, |chat, window, cx| {
                    chat.open_settings_for(profile, window, cx)
                })
                .expect("could not open the initial settings window");
        }
        // GPUI's `activate` does nothing on Windows, so a window opened by
        // hand can land behind others (#259). `activate_window` is queued and
        // runs after this callback, so it goes last and to the topmost window;
        // a login launch stays out of the user's way.
        #[cfg(target_os = "windows")]
        match autostart::startup_front(autostart::launched_by_registration(), settings_opened) {
            Some(autostart::StartupFront::Settings) => {
                let settings = chat_window
                    .update(cx, |chat, _, _| chat.settings_window)
                    .ok()
                    .flatten();
                if let Some(settings) = settings {
                    let _ = settings.update(cx, |_, window, _| window.activate_window());
                }
            }
            Some(autostart::StartupFront::Chat) => {
                let _ = chat_window.update(cx, |_, window, _| window.activate_window());
            }
            None => {}
        }
        #[cfg(not(target_os = "windows"))]
        let _ = settings_opened;
    });
}

#[cfg(test)]
#[path = "main_tests/combined_log_tests.rs"]
mod combined_log_tests;

#[cfg(test)]
#[path = "main_tests/roster_batch_tests.rs"]
mod roster_batch_tests;

#[cfg(test)]
#[path = "main_tests/conversation_log_tests.rs"]
mod conversation_log_tests;

#[cfg(test)]
#[path = "main_tests/log_tests.rs"]
mod log_tests;

#[cfg(test)]
#[path = "main_tests/startup_tests.rs"]
mod startup_tests;

/// [`conversation_participants`] of the selected conversation. Takes the
/// fields rather than the window so the rows can be held while the member
/// selection is changed.
fn selected_participants<'a>(
    state: &'a AppState,
    sessions: &HashMap<NetworkId, ServerSession>,
) -> member_selection::Participants<'a> {
    let Some(conversation) = state.selected_channel() else {
        return member_selection::Participants::plain(Cow::Borrowed(&[]));
    };
    let own = sessions
        .get(&conversation.network)
        .and_then(|session| session.own_nickname.as_deref());
    conversation_participants(conversation, own)
}

/// The rows of the right-hand list for `conversation`: a channel's members
/// with our own row first (borrowed, as this runs on every draw), or for a
/// private conversation us followed by the peer, once when both are the same
/// nickname.
fn conversation_participants<'a>(
    conversation: &'a cayenchat_model::Conversation,
    own_nickname: Option<&str>,
) -> member_selection::Participants<'a> {
    if !conversation.is_private() {
        return member_selection::Participants::own_first(&conversation.members, own_nickname);
    }
    let mut participants = Vec::with_capacity(2);
    if let Some(own) = own_nickname
        && !cayenchat_irc_core::text::same_nickname(own, &conversation.name)
    {
        participants.push(own.to_owned());
    }
    participants.push(conversation.name.clone());
    member_selection::Participants::plain(Cow::Owned(participants))
}

/// How many of the member menu's choices apply: a private conversation's
/// peer gets only WHOIS and a private message, no channel operations.
fn member_menu_choice_count(private: bool, total: usize) -> usize {
    if private { 2.min(total) } else { total }
}

#[cfg(test)]
#[path = "main_tests/support.rs"]
mod support;
#[cfg(test)]
use support::settings_with_channels;

#[cfg(test)]
#[path = "main_tests/participants_tests.rs"]
mod participants_tests;

#[cfg(test)]
#[path = "main_tests/participant_menu_tests.rs"]
mod participant_menu_tests;

#[cfg(test)]
#[path = "main_tests/url_hover_tests.rs"]
mod url_hover_tests;

#[cfg(test)]
#[path = "main_tests/pane_tests.rs"]
mod pane_tests;

#[cfg(test)]
#[path = "main_tests/field_traversal_tests.rs"]
mod field_traversal_tests;

#[cfg(test)]
#[path = "main_tests/navigation_binding_tests.rs"]
mod navigation_binding_tests;

#[cfg(test)]
#[path = "main_tests/bidi_display_tests.rs"]
mod bidi_display_tests;
