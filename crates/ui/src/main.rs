// SPDX-License-Identifier: GPL-3.0-only
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod about;
mod account_settings;
mod application_settings;
mod autostart;
mod avatar_editor;
mod avatar_quick_setup;
mod avatars;
mod button_order;
mod channel_keys;
mod char_refs;
mod charset_command;
mod color_picker;
mod compact_urls;
mod conversation_log;
mod decorations;
mod default_avatar;
mod desktop;
mod diagnostics;
mod dialog;
mod disconnect_dialog;
mod double_enter;
mod experimental_settings;
mod focus_reveal;
mod help_command;
mod image_upload;
mod input;
mod input_history;
mod ircv3_settings;
mod key_recorder;
mod list_keys;
mod localization;
mod log_list;
mod log_search;
mod member_selection;
mod menu_bar;
mod multiline;
mod notices;
mod notifier;
mod os_text;
mod part_dialog;
mod paste_dialog;
#[cfg(test)]
mod perf_baseline;
mod phrase_wrap;
#[cfg(any(target_os = "windows", test))]
mod placement;
mod previews;
mod quit_dialog;
mod reiwa_group;
mod scrollbar;
mod secrets;
mod session;
mod settings_file;
mod settings_reset;
mod settings_theme;
mod settings_transfer;
mod settings_window;
mod setup_wizard;
mod shortcut_settings;
mod shortcuts;
#[cfg(target_os = "linux")]
mod signals;
mod splitter;
mod theme;
mod toolbar;
mod tooltip;
mod wake;
mod whois;
mod window_layout;

use cayenchat_app::{
    AppState, Command, ConnectionStatus, Kind, MessageMeta, Missing, MoveTo, NetworkConfig,
    Selection, Unread,
    attachments::AttachmentFlow,
    notifications::{self, BurstLimiter, IncomingMessage, NotificationRules, Trigger},
    own_avatar::OwnAvatar,
    timeline::TimelineLine,
};
use cayenchat_irc_core::{
    ChannelActivityKind, ChannelEncoding, Connection, ConnectionConfig, Event, HistoryMessage,
    HistoryResume, Ircv3Options, MemberCommand, MessageReference, OlderHistoryStatus,
    OpRegainStatus, RealNameFailure, SaslCredentials, WhoisInfo, WireDirection, valid_channel,
};
use cayenchat_model::{
    ConversationId, Network, NetworkId, Provenance, TimeOfDay, Timestamp, display, names,
};
use cayenchat_storage::chatlog::{Kind as LogKind, Scope};
use cayenchat_storage::{
    Appearance, AutoJoinEntry, ChannelNumberModifier, CredentialError, CredentialStore,
    Ircv3Preferences, Language, NoticeKey, Notifications, Secret, SecretKey, SendKey,
    ServerProfile, Settings, TextKeyTheme, ThemeMode, UnreadLogPosition,
};
use futures_util::StreamExt;
use gpui::{prelude::*, *};
use input::TextInput;
use ircv3_settings::own_avatar_failure;
use localization::Localizer;
use log_list::LogList;
use notifier::{DesktopNotification, Notifier};
use os_text::WindowOsText;
use session::ServerSession;
use settings_window::{SettingsTab, SettingsWindow, settings_field};
use std::ops::Range;
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use theme::Theme;
use whois::{JoinedChannels, WhoisWindow};

/// Window class / Wayland app id; must equal the basename of `assets/linux/cayenchat.desktop`
/// so the shell matches every window to the launcher (issue #433).
pub(crate) const APP_ID: &str = "cayenchat";

/// The channel log's share of the log height when nothing was dragged, and
/// the bounds of what dragging may leave to either log.
const DEFAULT_LOG_SPLIT: f32 = 0.5;
/// How long a line stays emphasized after a jump to a notified line.
const REVEAL_FLASH: Duration = Duration::from_millis(2_000);
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
/// How long after waking from sleep the connections are checked, so that a
/// network that is coming back (Wi-Fi rejoining) is not mistaken for a dead
/// server, and repeated wake-up notices are handled once.
const WAKE_GRACE: Duration = Duration::from_secs(3);
/// The longest the UI waits, in total, for the servers to take the QUIT sent
/// before sleep; the system waits for us only briefly.
const SLEEP_WAIT: Duration = cayenchat_irc_core::SLEEP_FLUSH_LIMIT;
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
        ChannelActivityKind::ModeChanged { modes, .. } => {
            format!("{actor} has changed mode: {modes}")
        }
        ChannelActivityKind::Kicked {
            reason, own: true, ..
        } => with_detail("You were kicked from the channel by", actor, reason),
        ChannelActivityKind::Kicked { target, reason, .. } => {
            with_detail(&format!("{target} was kicked by"), actor, reason)
        }
        ChannelActivityKind::JoinFailed { banned, reason, .. } => {
            if banned {
                format!("Cannot join: you are banned from this channel ({reason})")
            } else {
                format!("Cannot join: {reason}")
            }
        }
    }
}

/// The valid channel names a typed `/join` command lists; none for any other
/// command.
fn join_command_channels(command: &str) -> Vec<&str> {
    let mut words = command.split_whitespace();
    if !words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("/join"))
    {
        return Vec::new();
    }
    words
        .next()
        .map(|channels| {
            channels
                .split(',')
                .filter(|name| valid_channel(name))
                .collect()
        })
        .unwrap_or_default()
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
        SendMessageWithCtrl,
        SendNotice,
        HistoryPrevious,
        HistoryNext,
        OpenSettings,
        ExportSettings,
        ImportSettings,
        About,
        Connect,
        Disconnect,
        Reconnect,
        MinimizeWindow,
        ZoomWindow,
        ToggleDebug,
        ToggleToolbar,
        ToggleMenuBar,
        CopyDiagnostics,
        CopyLogSelection,
        ScrollToLatest,
        FindInLog,
        CloseSearch,
        SearchNext,
        SearchPrevious,
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
    config.sleep_quit_message = profile.sleep_quit_message.clone();
    config.part_message = profile.part_message.clone();
    config.port = profile.port;
    config.use_tls = profile.use_tls;
    config.verify_tls_certificates = profile.verify_tls_certificates;
    config.allow_plaintext_pass = profile.allow_plaintext_pass;
    config.regain_ops = profile.regain_ops;
    config.encoding = profile.encoding.label().into();
    config.char_refs_for_iso2022jp = profile.iso2022jp_char_refs;
    config.channel_encodings = channel_encodings(profile);
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

/// The channels of `profile` with an encoding of their own, as a connection
/// takes them.
fn channel_encodings(profile: &ServerProfile) -> Vec<ChannelEncoding> {
    profile
        .channel_charsets
        .iter()
        .map(|entry| ChannelEncoding {
            channel: entry.channel.clone(),
            decode: entry.decode.map(|encoding| encoding.label().to_owned()),
            encode: entry.encode.map(|encoding| encoding.label().to_owned()),
        })
        .collect()
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
    let mut config = connection_config(profile, language, server_password, sasl_password)?;
    channel_keys::use_stored_keys(&mut config, profile, store);
    Ok(config)
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

/// What leads a message's text in the log: `alice: ` before a message,
/// `* alice ` before the text of a `/me` action.
fn sender_prefix(message: &cayenchat_model::Message) -> String {
    let sender = display::neutralize_bidi(&message.sender);
    if message.action {
        format!("* {sender} ")
    } else {
        format!("{sender}: ")
    }
}

/// The kind of a received PRIVMSG or NOTICE and the text it shows: a
/// PRIVMSG that is a CTCP ACTION (`/me`) is told apart here, once, and shown
/// by its action text. A NOTICE is never an action.
fn received_kind(text: &str, notice: bool) -> (Kind, &str) {
    if notice {
        return (Kind::Notice, text);
    }
    match cayenchat_irc_core::text::action_text(text) {
        Some(action) => (Kind::Action, action),
        None => (Kind::Message, text),
    }
}

/// Requested history lines as timeline items: history, never news.
fn history_lines(messages: Vec<HistoryMessage>) -> Vec<TimelineLine> {
    messages
        .into_iter()
        .map(|message| {
            let (kind, shown) = received_kind(&message.text, message.notice);
            let action = kind.is_action().then(|| shown.to_owned());
            TimelineLine {
                action: action.is_some(),
                text: match action {
                    Some(action) => action,
                    None if message.notice => format!("[NOTICE] {}", message.text),
                    None => message.text,
                },
                sender: message.sender,
                notice: message.notice,
                meta: irc_message_meta(
                    message.server_time,
                    message.msgid.as_deref(),
                    message.account.as_deref(),
                    Provenance::Replayed,
                ),
            }
        })
        .collect()
}

/// Shown where a reconnect recovered only the most recent missed lines of a
/// channel; like other channel activity, in English.
const HISTORY_GAP_NOTE: &str = "Some messages sent while disconnected are not shown.";

/// Message rows between the top of the main log and the first visible row
/// within which scrolling asks for older history.
const OLDER_HISTORY_TRIGGER_ROWS: usize = 5;

/// What a row of the combined log shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubRow {
    /// A conversation line: conversation and message index.
    Line(ConversationId, usize),
    /// A system line: index into `AppState::system_messages`.
    System(usize),
}

/// Merges `lines` (oldest first, from [`newest_lines`]) with the system
/// lines (oldest first) by sequence, keeping the newest `limit`.
fn sub_rows(
    lines: Vec<(u64, ConversationId, usize)>,
    system: &std::collections::VecDeque<(NetworkId, cayenchat_model::Message)>,
    limit: usize,
) -> Vec<(u64, SubRow)> {
    let mut rows: Vec<(u64, SubRow)> = Vec::with_capacity(lines.len() + system.len());
    let mut lines = lines.into_iter().peekable();
    let mut system = system.iter().enumerate().peekable();
    loop {
        let take_line = match (lines.peek(), system.peek()) {
            (Some(line), Some((_, (_, message)))) => line.0 <= message.sequence,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_line {
            let (sequence, id, index) = lines.next().expect("peeked");
            rows.push((sequence, SubRow::Line(id, index)));
        } else {
            let (index, (_, message)) = system.next().expect("peeked");
            rows.push((message.sequence, SubRow::System(index)));
        }
    }
    rows.drain(..rows.len().saturating_sub(limit));
    rows
}

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
    // The selection the tree last scrolled to; a different one is revealed.
    tree_revealed: Option<Selection>,
    // A channel a typed `/join` just selected; the window catches up (focus,
    // title) after the event batch.
    join_selected: Option<ConversationId>,
    // The reveal waits for the rows up to the selection to be laid out.
    tree_reveal_pending: bool,
    // Selection and newest message sequence the combined log was built for.
    sub_source: Option<(Option<ConversationId>, u64)>,
    // The Reiwa mode leads of the selected channel's lines, with what they
    // were derived from: rebuilt only when that changes, not on every redraw.
    reiwa_shapes: Option<(ReiwaShapeKey, Vec<u8>)>,
    sub_rows: Vec<SubRow>,
    // Editing and IME state belong to each server or channel.
    inputs: HashMap<Selection, Entity<TextInput>>,
    /// The error of the open member prompt, drawn inside it. Errors anywhere
    /// else go to `notices`, never into the draft row (#528).
    prompt_feedback: Option<String>,
    /// Errors waiting for, or shown in, a dialog.
    notices: notices::Notices,
    /// A dialog of this window is open; another one must wait for it.
    prompt_open: bool,
    /// When the user last spoke on each server, to tell the 417
    /// or 400 that answers it from one a background process caused.
    sent_messages: notices::SentMessages,
    /// The image file being read off the UI thread; a newer drop replaces it.
    file_read: Option<Task<()>>,
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
    /// The menu of a right click in the channel log.
    log_menu: Option<LogMenu>,
    member_prompt: Option<MemberPrompt>,
    /// Members chosen in the member list of the selected channel.
    member_selection: member_selection::MemberSelection,
    /// Sent drafts recalled with Up/Down in the draft input.
    input_history: input_history::InputHistory,
    /// One row per server whose nickname was rejected during registration,
    /// in the order the rejections arrived.
    nick_prompts: Vec<NickPrompt>,
    /// One row per channel whose JOIN a server refused for want of a key.
    key_prompts: Vec<channel_keys::KeyPrompt>,
    startup_connections: Vec<(NetworkId, Result<ConnectionConfig, String>)>,
    settings_window: Option<WindowHandle<SettingsWindow>>,
    window_handle: Option<WindowHandle<ChatWindow>>,
    /// Remember where the window and its panes were left (a setting), and
    /// the file that is written to. `None` writes nothing: tests, and the
    /// moment before startup supplies the path.
    restore_layout: bool,
    /// Ask before the user quits (a setting), and whether that question is
    /// on screen, so a second quit request does not open another.
    confirm_quit: bool,
    /// Which key press sends the chat draft (a setting), and the first Enter
    /// of "Enter twice" while it waits for the second.
    send_key: SendKey,
    double_enter: double_enter::DoubleEnter,
    _enter_keys: Subscription,
    /// Where the log goes when an unread conversation is selected (a setting).
    unread_log_position: UnreadLogPosition,
    quit_prompt_open: bool,
    quit_dialog: quit_dialog::QuitDialog,
    /// The export and import of the settings file (#520).
    transfer_dialog: settings_transfer::TransferDialog,
    setup_wizard: setup_wizard::SetupWizard,
    /// The question a double click on a joined channel row opens (#451).
    part_prompt: Option<part_dialog::PartPrompt>,
    /// The question a double click on a registered server row opens (#625).
    disconnect_prompt: Option<disconnect_dialog::DisconnectPrompt>,
    /// Ask before a double click disconnects a server / leaves a channel
    /// (settings; switched off by "Don't ask again").
    confirm_server_dblclick_disconnect: bool,
    confirm_channel_dblclick_part: bool,
    paste_prompt: Option<paste_dialog::PastePrompt>,
    /// A multi-line paste waiting for the next frame, which has the window to
    /// ask about it in (the draft inputs are made without one).
    pending_paste: Option<(Entity<TextInput>, String)>,
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
    /// Keep every CayenChat window above ordinary windows (Windows only).
    /// Applied to the windows opened later; the native state is set by
    /// `apply_always_on_top`, never while building a window.
    #[cfg(target_os = "windows")]
    always_on_top: bool,
    /// Open WHOIS windows by network and lowercase nickname.
    whois_windows: HashMap<(NetworkId, String), WindowHandle<WhoisWindow>>,
    whois_replies: Vec<(NetworkId, WhoisInfo, bool)>,
    debug_enabled: bool,
    conversation_logs: conversation_log::ConversationLogs,
    menu_bar: menu_bar::MenuBar,
    /// The toolbar is on (View menu, saved in the settings), and its cached
    /// view, made when it is first drawn.
    toolbar_shown: bool,
    toolbar: Option<AnyView>,
    /// The log search bar (#404), made when it is first opened.
    search: Option<log_search::LogSearch>,
    appearance: Appearance,
    /// Experimental "Auto Phrase": wrap Japanese chat text at phrase
    /// boundaries. The cache is behind a `RefCell` because rows borrow the
    /// window state while they are drawn.
    auto_phrase: bool,
    phrase_joins: RefCell<phrase_wrap::PhraseJoins>,
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
    /// The line a jump to a notified line just chose, drawn emphasized for
    /// `REVEAL_FLASH`.
    reveal_flash: Option<(ConversationId, u64)>,
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
    /// Whether the last draw of the main log included the jump button.
    #[cfg(test)]
    latest_button_drawn: bool,
}

/// An incoming channel or private message, as `notify_message` needs it.
struct ReceivedMessage<'a> {
    /// `None` for a private message.
    channel: Option<&'a str>,
    /// Where it was added; `None` for the server log.
    conversation: Option<ConversationId>,
    sender: &'a str,
    /// The text the conversation shows: an action's own text for a `/me`.
    text: &'a str,
    kind: Kind,
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
/// of its server's part of the tree (D009): 1 to 9, then 0; none beyond the tenth.
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

/// The menu of a right click in the log of a channel or private
/// conversation. `sender` is the speaker of the clicked message row, only
/// when that row names one peer we can open a conversation with.
struct LogMenu {
    position: Point<Pixels>,
    network: NetworkId,
    /// The log clicked: a conversation, or a server's own log (then
    /// `channel` is empty and the channel entries are left out).
    source: Selection,
    channel: String,
    private: bool,
    sender: Option<String>,
    /// The clipboard held text when the menu opened.
    can_paste: bool,
}

#[derive(Clone, Copy)]
enum LogMenuChoice {
    Copy,
    Paste,
    Mode,
    Topic,
    Nick,
    Private,
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
    /// The CTCP submenu is open.
    ctcp_open: bool,
}

/// The CTCP submenu items in order: the menu label key and the request tag.
const MEMBER_CTCP_ITEMS: [(&str, &str); 5] = [
    ("member_ctcp_ping", "PING"),
    ("member_ctcp_time", "TIME"),
    ("member_ctcp_version", "VERSION"),
    ("member_ctcp_userinfo", "USERINFO"),
    ("member_ctcp_clientinfo", "CLIENTINFO"),
];

#[derive(Clone, Copy)]
enum MemberPromptKind {
    PrivateMessage,
    Invite,
    /// Join a channel; the prompt is about the server, not a member.
    Join,
    /// Change our own nickname on the server.
    Nick,
    /// Change the modes of the channel named by the prompt's nickname.
    Mode,
    /// Change the topic of the channel named by the prompt's nickname.
    Topic,
}

/// The dialog heading, with the nickname as drawn.
fn member_prompt_title(i18n: &Localizer, kind: MemberPromptKind, nickname: &str) -> String {
    i18n.format_nickname(
        match kind {
            MemberPromptKind::PrivateMessage => "member_message_title",
            MemberPromptKind::Invite => "member_invite_title",
            MemberPromptKind::Join => "channel_join_title",
            MemberPromptKind::Nick => "nickname_change_title",
            MemberPromptKind::Mode => "channel_mode_title",
            MemberPromptKind::Topic => "channel_topic_title",
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
    /// The log the rows belong to: a conversation or a server's own log.
    source: Selection,
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
    /// Applies a changed send key: a wait of "Enter twice" ends and the
    /// drafts' hint names the new key.
    fn set_send_key(&mut self, send_key: SendKey, cx: &mut Context<Self>) {
        self.send_key = send_key;
        self.double_enter.reset();
        let placeholder = draft_placeholder(&self.i18n, send_key);
        for input in self.inputs.values() {
            input.update(cx, |input, cx| input.set_placeholder(&placeholder, cx));
        }
    }

    fn apply_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        self.i18n = Localizer::new(language);
        cx.set_global(input::MenuLabels::new(&self.i18n));
        let placeholder = draft_placeholder(&self.i18n, self.send_key);
        for input in self.inputs.values() {
            input.update(cx, |input, cx| input.set_placeholder(&placeholder, cx));
        }
        self.refresh_menus(cx);
        for handle in self.whois_windows.values() {
            let i18n = self.i18n.clone();
            let _ = handle.update(cx, |view, _, cx| view.set_localizer(i18n, cx));
        }
        about::set_localizer(&self.i18n, cx);
        cx.notify();
        window.refresh();
    }

    fn status_text(&self, status: Option<&ConnectionStatus>) -> String {
        match status {
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

    /// The server heading of a prompt: `network_label`, made safe to show.
    pub(crate) fn prompt_server_label(&self, network: NetworkId) -> String {
        self.state
            .networks()
            .iter()
            .find(|server| server.id == network)
            .map(|server| display::neutralize_bidi_owned(self.network_label(server)))
            .unwrap_or_default()
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
        // Without a connection the server and channel names would suggest
        // something that can be switched to, so show the state instead.
        if matches!(
            self.state.status(network.id),
            None | Some(ConnectionStatus::Disconnected(_))
        ) {
            return format!("{} — {app}", self.i18n.text("title_not_connected"));
        }
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
                // Known, non-empty modes come first; unknown ones are left out.
                let name = match channel.modes.as_deref() {
                    Some(modes) if !modes.is_empty() => format!("[+{modes}] {name}"),
                    _ => name,
                };
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
        window.os_set_title(&self.window_title());
    }

    fn with_settings(
        saved: Settings,
        feedback: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.set_global(input::ShortenPastedAmazon(saved.shorten_amazon_urls));
        cx.set_global(input::ConfirmMultilinePaste(saved.confirm_multiline_paste));
        let i18n = Localizer::new(saved.language);
        cx.set_global(input::MenuLabels::new(&i18n));
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
        let placeholder = draft_placeholder(&i18n, saved.send_key);
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
            inputs.insert(key, Self::new_draft_input(&placeholder, cx));
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
            tree_revealed: None,
            join_selected: None,
            tree_reveal_pending: false,
            sub_source: None,
            reiwa_shapes: None,
            sub_rows: Vec::new(),
            inputs,
            startup_notice: feedback.clone(),
            prompt_feedback: None,
            notices: {
                let mut notices = notices::Notices::default();
                if let Some(detail) = feedback {
                    let title = i18n.text("notice_title");
                    notices.post(
                        notices::Notice { title, detail },
                        notices::Origin::User,
                        Instant::now(),
                    );
                }
                notices
            },
            prompt_open: false,
            sent_messages: Default::default(),
            file_read: None,
            sessions,
            next_network_id,
            server_menu: None,
            member_menu: None,
            channel_menu: None,
            log_menu: None,
            member_prompt: None,
            member_selection: Default::default(),
            input_history: Default::default(),
            nick_prompts: Vec::new(),
            key_prompts: Vec::new(),
            startup_connections,
            settings_window: None,
            #[cfg(target_os = "windows")]
            always_on_top: saved.always_on_top,
            window_handle: window.window_handle().downcast::<ChatWindow>(),
            restore_layout: saved.restore_window_layout,
            confirm_quit: saved.confirm_quit,
            send_key: saved.send_key,
            double_enter: Default::default(),
            _enter_keys: Self::watch_enter_keys(window, cx),
            unread_log_position: saved.unread_log_position,
            quit_prompt_open: false,
            quit_dialog: quit_dialog::QuitDialog::new(cx),
            transfer_dialog: settings_transfer::TransferDialog::new(cx),
            setup_wizard: setup_wizard::SetupWizard::new(cx),
            part_prompt: None,
            disconnect_prompt: None,
            confirm_server_dblclick_disconnect: saved.confirm_server_dblclick_disconnect,
            confirm_channel_dblclick_part: saved.confirm_channel_dblclick_part,
            paste_prompt: None,
            pending_paste: None,
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
            auto_phrase: saved.experimental.auto_phrase,
            phrase_joins: RefCell::default(),
            theme_mode: saved.theme,
            image_provider: saved.image_upload.provider.clone(),
            notification_rules: notification_rules(&saved.notifications),
            previews: previews::Previews::new(saved.appearance.image_previews),
            avatars: avatars::Avatars::new(saved.servers.iter().any(|p| p.user_avatars)),
            toolbar_shown: saved.show_toolbar,
            toolbar: None,
            search: None,
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
            reveal_flash: None,
            attachments: AttachmentFlow::default(),
            uploader_override: None,
            notifier: Notifier::new(),
            notification_burst: BurstLimiter::default(),
            window_active: window.is_window_active(),
            #[cfg(test)]
            pane_renders: 0,
            #[cfg(test)]
            latest_button_drawn: false,
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
        // GPUI reports the macOS/Windows appearance and, on Linux, the XDG
        // desktop portal color scheme.
        cx.observe_window_appearance(window, |this, _, cx| {
            theme::apply(this.theme_mode, &this.appearance, cx);
        })
        .detach();
        cx.observe_window_activation(window, |this, window, _| {
            this.window_active = window.is_window_active();
            // The draft keeps its logical focus while another window is in
            // front, so a wait of "Enter twice" ends with the activation.
            this.double_enter.reset();
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
        window.on_window_should_close(cx, move |_, cx| {
            // Closing the main window quits the application. The window is
            // never closed here: the request is handed to the ordinary quit
            // path after this callback, which asks if the question is on and
            // ends in `quit_app`; the usual shutdown runs in `on_app_quit`.
            request_quit_anywhere(cx);
            false
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

    /// The user asked to quit the application (menu, shortcut, closing the
    /// main window). Quits at once unless the confirmation is on. Quits by
    /// the system or a signal call `cx.quit()` directly and never ask.
    fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ask_before_quit(window, cx) {
            quit_app(cx);
        }
    }

    /// Opens the quit question when it is on and not yet open. Returns
    /// whether the question applies, so the caller must not quit now. No
    /// connection, window or draft is touched here; QUIT is only sent after
    /// "Quit" is chosen.
    fn ask_before_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.confirm_quit {
            return false;
        }
        if self.quit_prompt_open {
            return true;
        }
        // The request may come from the settings window or the Dock; show
        // the question in front. It is drawn in this window (`quit_dialog`)
        // so that Enter cancels whatever the OS's button order is.
        window.activate_window();
        self.open_quit_dialog(window, cx);
        true
    }

    /// Sends QUIT on every connection and waits briefly for it to be
    /// written, since the process ends right after the window closes. The
    /// wait is bounded and only happens on the way out.
    fn quit_connections(&mut self) {
        let networks: Vec<_> = self.sessions.keys().copied().collect();
        let mut closing = Vec::new();
        for network in networks {
            // Logged before the channels are left, with the QUIT text sent.
            let reason = self.sessions.get(&network).and_then(|session| {
                session
                    .active_config
                    .as_ref()
                    .map(ConnectionConfig::wire_quit_message)
            });
            let registered = self.state.is_registered(network);
            if let Some(connection) = self
                .sessions
                .get_mut(&network)
                .and_then(ServerSession::close)
            {
                if registered && let Some(reason) = reason {
                    self.log_own_quit(network, &reason);
                }
                closing.push(connection);
            }
        }
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

    /// Whether `network`'s server shows user avatars (its profile's setting).
    fn avatars_shown(&self, network: NetworkId) -> bool {
        self.sessions
            .get(&network)
            .and_then(|session| self.saved.profile(&session.profile_id))
            .is_some_and(|profile| profile.user_avatars)
    }

    /// Our own avatar on `network`, shown for our nickname instead of asking
    /// anyone for it: the saved icon of this server's settings (#612), not
    /// what the server confirmed, so a saved or removed icon shows at once
    /// and offline. `None` is some other nickname, `Some(None)` is us
    /// without an icon (the placeholder). Read at draw time, so every row
    /// follows a change at once.
    fn own_avatar_for(&self, network: NetworkId, nickname: &str) -> Option<Option<Arc<str>>> {
        if !self.is_own_nickname(network, nickname) {
            return None;
        }
        let session = self.sessions.get(&network)?;
        let url = self
            .saved
            .profile(&session.profile_id)
            .map(|profile| profile.avatar_url.trim())
            .filter(|url| !url.is_empty());
        Some(url.map(Into::into))
    }

    /// What the right-hand list shows for the selected conversation: a
    /// channel's members, or for a private conversation its two participants
    /// (us, then the peer). The peer stays listed after they quit, as long as
    /// the conversation exists.
    fn participants(&self) -> member_selection::Participants<'_> {
        selected_participants(&self.state, &self.sessions)
    }

    /// Mentions of our nickname and keywords in an incoming message, looked
    /// for in its text (the action's own text for a `/me`) without formatting
    /// codes. Our own lines and replayed history have none.
    fn find_mentions(&self, network: NetworkId, sender: &str, body: &str, replayed: bool) -> Found {
        use cayenchat_irc_core::text::{mention_ranges, plain_ranges};

        if replayed || self.is_own_nickname(network, sender) {
            return Found::default();
        }
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
            // The excluded text is searched once for both kinds of match.
            let excluded = &self.notification_rules.exclude_keywords;
            let cuts = notifications::keyword_ranges(plain, excluded);
            let ranges = notifications::ranges_outside(ranges, &cuts);
            mentioned = !ranges.is_empty();
            let matches = notifications::keyword_ranges_outside(plain, keywords, &cuts);
            keyword = !matches.is_empty();
            let mut ranges = ranges;
            ranges.extend(matches);
            ranges.sort_by_key(|range| range.start);
            ranges
        });
        Found {
            mentioned,
            keyword,
            ranges,
        }
    }

    /// Shows a desktop notification for an incoming IRC message when the
    /// rules ask for one and the message is not already in front of the user.
    fn notify_message(&mut self, network: NetworkId, message: ReceivedMessage) {
        use cayenchat_irc_core::text::strip_formatting;

        let ReceivedMessage {
            channel,
            conversation,
            sender,
            text,
            kind,
            found,
            replayed,
        } = message;

        let incoming = IncomingMessage {
            channel: channel.is_some(),
            notice: kind.is_notice(),
            from_self: self.is_own_nickname(network, sender),
            mentioned: found.mentioned,
            keyword: found.keyword,
            replayed,
        };
        let Some(trigger) = self.notification_rules.attention(incoming) else {
            return;
        };
        let visible = self.window_active
            && self.state.selection()
                == conversation.map_or(Selection::Server(network), Selection::Channel);
        // Visitable with desktop notifications off; a line in front of the
        // user needs no visit.
        if let (Some(conversation), false) = (conversation, visible) {
            self.state.note_attention(conversation);
        }
        if !self.notification_rules.enabled {
            return;
        }
        let plain = display::neutralize_bidi_owned(if kind.is_action() {
            format!("* {sender} {}", strip_formatting(text))
        } else {
            strip_formatting(text)
        });
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
                self.raise_error(Some(network), error, notices::Origin::User, cx);
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
                self.report(Some(error), cx);
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
                self.report(Some(error), cx);
                cx.notify();
            }
        }
    }

    /// A message draft that hands multi-line pastes to the paste dialog.
    fn new_draft_input(placeholder: &str, cx: &mut Context<Self>) -> Entity<TextInput> {
        let input = cx.new(|cx| TextInput::new_live(placeholder, cx));
        cx.subscribe(&input, |this, input, paste: &input::MultilinePaste, cx| {
            this.pending_paste = Some((input, paste.0.clone()));
            cx.notify();
        })
        .detach();
        input
    }

    /// Creates draft inputs for servers and conversations that lack one.
    fn ensure_inputs(&mut self, cx: &mut Context<Self>) {
        let placeholder = draft_placeholder(&self.i18n, self.send_key);
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
                slot.insert(Self::new_draft_input(&placeholder, cx));
            }
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
            .is_some_and(|selection| match selection.source {
                Selection::Channel(id) => removed.contains(&id),
                _ => false,
            })
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
        let mut key_refreshes = Vec::new();
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
                    // Read by the next connection, automatic reconnects
                    // included, so switching it off stops them at once.
                    config.regain_ops = profile.regain_ops;
                    config.char_refs_for_iso2022jp = profile.iso2022jp_char_refs;
                    config.shared_avatar = shared.clone();
                    // The auto-join list too: reconnects join the saved one.
                    let before = std::mem::replace(&mut config.channels, profile.channels());
                    // The sleep QUIT reads these when the machine sleeps, so
                    // saved changes count for the current connection too.
                    // Settings saving does not validate them, and a bad one
                    // would also fail the reconnect's validation, so the
                    // previous value stays instead.
                    if config.quit_reason_fits(&profile.quit_message) {
                        config.quit_message = profile.quit_message.clone();
                    }
                    if config.quit_reason_fits(&profile.sleep_quit_message) {
                        config.sleep_quit_message = profile.sleep_quit_message.clone();
                    }
                    // The store is read again only when the list gained a
                    // channel with a saved key; other changes leave it alone.
                    channel_keys::follow_channel_spelling(config);
                    if channel_keys::keyed_channel_added(&before, profile) {
                        key_refreshes.push((id, profile.clone()));
                    }
                    config.channel_encodings = channel_encodings(profile);
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
        for (id, profile) in key_refreshes {
            self.refresh_stored_keys(id, &profile, cx);
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
            if self
                .log_selection
                .is_some_and(|selection| selection.source == Selection::Server(id))
            {
                self.log_selection = None;
            }
            self.whois_windows.retain(|(network, _), _| *network != id);
            self.nick_prompts.retain(|prompt| prompt.network != id);
            self.key_prompts.retain(|prompt| prompt.network != id);
            if self
                .member_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.network == id)
            {
                self.member_prompt = None;
            }
            if self
                .part_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.network() == id)
            {
                self.part_prompt = None;
            }
            if self
                .disconnect_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.network() == id)
            {
                self.disconnect_prompt = None;
            }
            if self
                .paste_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.network() == id)
            {
                self.paste_prompt = None;
            }
        }
        let removed = self.state.sync_networks(&networks);
        self.restore_channel_orders();
        self.forget_conversations(&removed);
        self.ensure_inputs(cx);
        // The shared cache and fetcher live while any server shows avatars;
        // a server turned off only stops asking (loads it started may finish,
        // for the cache is shared by URL), and all off cancels everything.
        self.avatars
            .set_enabled(settings.servers.iter().any(|profile| profile.user_avatars));
        self.saved = settings;
        self.refresh_whois_avatars(cx);
        cx.notify();
    }

    /// Fetches the saved icon of server `profile_id` again: the explicit
    /// register / update step saved it, and its image may have changed behind
    /// the same URL (#612). Never called for redraws or other settings saves,
    /// and other people's avatars stay cached.
    pub(crate) fn refresh_own_avatar(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let Some(profile) = self.saved.profile(profile_id) else {
            return;
        };
        if profile.user_avatars && !profile.avatar_url.is_empty() {
            self.avatars.refresh(&profile.avatar_url);
            // An open WHOIS holds its own copy of the image and starts the
            // new load itself, even if no log or member list shows us.
            self.refresh_whois_avatars(cx);
            cx.notify();
        }
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
        mut config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(session) = self.sessions.get_mut(&network) else {
            return Err(self.i18n.text("not_connected"));
        };
        session.close();
        session.manual_disconnect = false;
        session.retry_attempt = 0;
        // CTCP AVATAR pictures remembered by mask outlive connections of
        // the same network (D058).
        if let Some(previous) = &session.active_config {
            config.peer_avatar_cache = previous.peer_avatar_cache.clone();
        }
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
            self.log_menu = None;
        }
        if self
            .member_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.network == network)
        {
            self.member_prompt = None;
        }
        if self
            .part_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.network() == network)
        {
            self.part_prompt = None;
        }
        if self
            .disconnect_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.network() == network)
        {
            self.disconnect_prompt = None;
        }
        if self
            .paste_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.network() == network)
        {
            self.paste_prompt = None;
        }
        if let Some(session) = self.sessions.get_mut(&network) {
            session.pending_parts.clear();
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
                self.prompt_feedback = None;
                self.spawn_event_pump(network, cx);
                window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
                Ok(())
            }
            Err(error) => {
                self.record_disconnect(network, error.clone(), true, cx);
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
            || self
                .log_menu
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

    /// Whether the disconnect was requested without an error.
    fn disconnect(&mut self, network: NetworkId, cx: &mut Context<Self>) -> bool {
        self.server_menu = None;
        let Some(session) = self.sessions.get_mut(&network) else {
            return false;
        };
        let mut requested = true;
        let cancelled_retry = session.retry_pending;
        session.manual_disconnect = true;
        session.asleep = false;
        session.retry_pending = false;
        session.retry_token += 1;
        if let Some(connection) = &session.irc {
            if let Err(error) = connection.disconnect() {
                requested = false;
                self.raise_error(Some(network), error, notices::Origin::User, cx);
            }
        } else if cancelled_retry {
            let message = self.i18n.text("event_retry_cancelled");
            self.state.append_server_message(network, message);
        }
        cx.notify();
        requested
    }

    fn disconnect_action(&mut self, _: &Disconnect, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(network) = self.selected_network_id() {
            self.disconnect(network, cx);
        }
    }

    /// Asks whether to disconnect every server (a long press on the toolbar's
    /// Disconnect button), then does so. Servers with nothing to stop are
    /// left alone.
    fn disconnect_all_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disconnectable_networks().is_empty() {
            return;
        }
        let answer = self.chat_prompt(
            PromptLevel::Warning,
            &self.i18n.text("disconnect_all_title"),
            Some(&self.i18n.text("disconnect_all_detail")),
            &[
                PromptButton::ok(self.i18n.text("disconnect_all_confirm")),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            window,
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    for network in this.disconnectable_networks() {
                        this.disconnect(network, cx);
                    }
                });
            }
        })
        .detach();
    }

    /// The servers that are connected or waiting to retry.
    fn disconnectable_networks(&self) -> Vec<NetworkId> {
        self.sessions
            .keys()
            .copied()
            .filter(|network| self.can_disconnect(*network))
            .collect()
    }

    /// Whether Connect applies: the selected server has not been connected in
    /// this run, or there is no server yet (it then opens settings).
    fn can_connect_selected(&self) -> bool {
        match self.selected_network_id() {
            Some(network) => self
                .sessions
                .get(&network)
                .is_some_and(|session| !session.used()),
            None => true,
        }
    }

    /// Whether Reconnect applies: the selected server has been connected in
    /// this run and has no live connection (a scheduled retry may be skipped
    /// ahead). Reconnecting a live one would drop it and register again before
    /// the server noticed, which collides with our own nickname.
    fn can_reconnect_selected(&self) -> bool {
        self.selected_network_id().is_some_and(|network| {
            self.sessions
                .get(&network)
                .is_some_and(|session| session.used() && session.irc.is_none())
        })
    }

    fn connect_action(&mut self, _: &Connect, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected_network_id() {
            Some(network) => self.reconnect(network, window, cx),
            None => self.open_settings_for(None, window, cx),
        }
    }

    fn reconnect_action(&mut self, _: &Reconnect, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(network) = self.selected_network_id() {
            self.reconnect(network, window, cx);
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
                    self.raise_error(Some(network), error, notices::Origin::User, cx);
                    self.open_settings_for(Some(profile_id), window, cx);
                }
            }
            return;
        }
        session.manual_disconnect = false;
        session.retry_pending = false;
        session.retry_attempt = 0;
        session.retry_token += 1;
        self.start_reconnect(network, true, cx);
    }

    fn start_reconnect(&mut self, network: NetworkId, announce: bool, cx: &mut Context<Self>) {
        let Some(config) = self.reconnect_config(network) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        session.retry_pending = false;
        session.asleep = false;
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
        self.prompt_feedback = None;
        match Connection::connect(config) {
            Ok(connection) => {
                connection.set_transcript(self.debug_enabled);
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.irc = Some(connection);
                }
                self.spawn_event_pump(network, cx);
            }
            Err(error) => {
                // An automatic retry that fails is only logged; one the user
                // asked for is announced.
                self.record_disconnect(network, error, announce, cx);
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
        // A connection ended for sleep is reconnected by the wake-up.
        if session.manual_disconnect || session.asleep || session.active_config.is_none() {
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
                        && !session.asleep
                        && session.irc.is_none()
                });
                if due {
                    this.start_reconnect(network, false, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Records that `network`'s connection ended. `announce` raises the
    /// selected server's end as a notice; it is set for a refusal and a
    /// failed user-made connect. A connection that ended by itself, a failed
    /// automatic retry and an end the user asked for are only logged.
    fn record_disconnect(
        &mut self,
        network: NetworkId,
        reason: String,
        announce: bool,
        cx: &mut Context<Self>,
    ) {
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
        if was_registered {
            self.log_own_quit(network, &reason);
        }
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
        if announce && self.selected_network_id() == Some(network) {
            self.raise_error(Some(network), message, notices::Origin::Background, cx);
        }
    }

    /// Opens or closes the CTCP submenu of the open member menu; nothing is
    /// sent.
    fn set_member_ctcp_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(menu) = self.member_menu.as_mut()
            && menu.ctcp_open != open
        {
            menu.ctcp_open = open;
            cx.notify();
        }
    }

    fn member_command(&mut self, command: MemberCommand, cx: &mut Context<Self>) {
        let Some(menu) = self.member_menu.take() else {
            return;
        };
        if command == MemberCommand::Whois {
            let result = self.request_whois(menu.network, &menu.nickname, cx);
            self.report_sent(menu.network, result, cx);
            cx.notify();
            return;
        }
        let result = self
            .registered_connection(menu.network)
            .and_then(|connection| connection.send_member_command(&menu.nickname, command.clone()));
        self.report_sent(menu.network, result, cx);
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
        if !group.is_empty() {
            let result = self
                .registered_connection(menu.network)
                .and_then(|connection| connection.send_member_modes(&menu.channel, mode, &group));
            self.report_sent(menu.network, result, cx);
        }
        cx.notify();
    }

    /// Just before the machine sleeps: sends QUIT with the sleep message on
    /// every registered connection and closes those still connecting, all at
    /// once, and waits at most [`SLEEP_WAIT`] in total for the workers to end.
    /// The sessions are marked `asleep`, so they are neither counted as ended
    /// by the user nor retried; [`ChatWindow::check_connections_after_wake`]
    /// reconnects them. Ones the user ended are left alone.
    fn prepare_for_sleep(&mut self, cx: &mut Context<Self>) {
        let networks: Vec<_> = self.sessions.keys().copied().collect();
        let mut closing = Vec::new();
        for network in networks {
            let Some(session) = self.sessions.get_mut(&network) else {
                continue;
            };
            if session.manual_disconnect || session.asleep {
                continue;
            }
            let Some(connection) = &session.irc else {
                if session.retry_pending {
                    // Waiting to retry: hold the timer back too, or it could
                    // connect right before the machine sleeps. The wake-up
                    // reconnects it like the ones closed below.
                    session.asleep = true;
                    session.retry_pending = false;
                    session.retry_token += 1;
                }
                continue;
            };
            session.asleep = true;
            // The worker closes a connection that is not registered yet
            // without QUIT, whatever the queues hold.
            match &session.active_config {
                Some(config) => connection.sleep(&config.checked_sleep_quit_message()),
                None => connection.shutdown(),
            }
            closing.push(network);
        }
        let deadline = Instant::now() + SLEEP_WAIT;
        for network in &closing {
            if let Some(connection) = self.sessions.get(network).and_then(|s| s.irc.as_ref()) {
                connection.wait_closed(deadline.saturating_duration_since(Instant::now()));
            }
        }
        cx.notify();
    }

    /// After waking from sleep: reconnects the connections that were ended
    /// for it, then has every registered connection that the user did not
    /// end prove it is alive. The worker disconnects those that stay silent,
    /// which reconnects them like any other lost connection; the others are
    /// not touched.
    fn check_connections_after_wake(&mut self, cx: &mut Context<Self>) {
        let networks: Vec<_> = self.sessions.keys().copied().collect();
        for network in networks {
            let session = self.sessions.get_mut(&network).expect("listed above");
            if session.manual_disconnect {
                session.asleep = false;
                continue;
            }
            if session.asleep {
                // Retries restart from the shortest wait if this fails.
                session.retry_attempt = 0;
                session.retry_token += 1;
                self.start_reconnect(network, false, cx);
                continue;
            }
            if let Ok(connection) = self.registered_connection(network)
                && let Err(error) = connection.check_alive()
            {
                self.push_diagnostic(network, error);
            }
        }
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

    /// Asks `nickname` for their avatar with CTCP AVATAR (the WHOIS
    /// window's button).
    fn request_peer_avatar(
        &mut self,
        network: NetworkId,
        nickname: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if !self.peer_avatars_on(network) {
            return Err(self.i18n.text("whois_avatar_off"));
        }
        self.registered_connection(network)?
            .request_peer_avatar(nickname)?;
        cx.notify();
        Ok(())
    }

    fn peer_avatars_on(&self, network: NetworkId) -> bool {
        self.sessions
            .get(&network)
            .is_some_and(|session| session.peer_avatars.enabled)
    }

    /// Pushes each WHOIS window the avatar image of its user; the cache is
    /// here, and a window cannot read this one while drawing.
    fn refresh_whois_avatars(&mut self, cx: &mut Context<Self>) {
        if self.whois_windows.is_empty() {
            return;
        }
        let mut pending = false;
        let shown: Vec<_> = self
            .whois_windows
            .iter()
            .map(|((network, key), handle)| {
                let (nickname, pixels) = handle
                    .update(cx, |view, window, _| {
                        (
                            view.nickname().to_owned(),
                            whois::avatar_pixels(window.scale_factor()),
                        )
                    })
                    .unwrap_or_else(|_| (key.clone(), whois::avatar_pixels(1.)));
                // Ourselves first, as in the log: the confirmed or shared
                // avatar, never a CTCP query to ourselves.
                let avatar = self.own_avatar_for(*network, &nickname).unwrap_or_else(|| {
                    self.state
                        .avatars()
                        .for_message(
                            *network,
                            &cayenchat_irc_core::text::nickname_key(key),
                            u64::MAX,
                        )
                        .cloned()
                });
                let shown = avatar
                    .filter(|_| self.avatars_shown(*network))
                    .map(|avatar| self.avatars.lookup(&avatar, pixels))
                    .unwrap_or(avatars::Shown::None);
                // Loading keeps the slot empty; no image falls back to the
                // icon made from the nickname as the server reported it
                // (the lowercased key only if the window is gone).
                let (image, default, loading) =
                    whois::slot_contents(shown, || self.avatars.default_sized(&nickname, pixels));
                pending |= loading;
                (*handle, image, default, self.peer_avatars_on(*network))
            })
            .collect();
        for (handle, image, default, can_request) in shown {
            let _ = handle.update(cx, |view, _, cx| {
                view.set_avatar(image, default, can_request, cx)
            });
        }
        if pending {
            self.pump_avatars(cx);
        }
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
        let log = self.log_menu.take().is_some();
        server || member || channel || log
    }

    /// Whether `source` has a non-empty text selection in its log.
    fn has_log_selection(&self, source: Selection) -> bool {
        self.log_selection.is_some_and(|selection| {
            let (start, end) = selection.bounds();
            selection.source == source && start != end
        })
    }

    /// Opens the log menu at `position` for the selected log.
    /// `sender` is the speaker of the clicked row, if it has one: it must be
    /// a plain nickname and not ourselves. The log selection is untouched.
    fn open_log_menu(
        &mut self,
        position: Point<Pixels>,
        sender: Option<&str>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let (source, network, channel, private) = match self.state.selected_channel() {
            Some(conversation) => (
                Selection::Channel(conversation.id),
                conversation.network,
                conversation.name.clone(),
                conversation.is_private(),
            ),
            None => match self.state.selection() {
                Selection::Server(network) => {
                    (Selection::Server(network), network, String::new(), false)
                }
                _ => return,
            },
        };
        let sender = sender
            .filter(|sender| cayenchat_irc_core::valid_nickname(sender))
            .filter(|sender| !self.is_own_nickname(network, sender))
            .map(str::to_owned);
        let viewport = window.viewport_size();
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.member_prompt = None;
        self.log_menu = Some(LogMenu {
            position: point(
                position.x.min((viewport.width - px(230.)).max(px(0.))),
                position.y.min((viewport.height - px(190.)).max(px(0.))),
            ),
            network,
            source,
            channel,
            private,
            sender,
            can_paste: self.can_paste_into_input(cx),
        });
        cx.notify();
    }

    /// Whether any input of the chat window holds uncommitted IME text.
    fn is_composing(&self, cx: &App) -> bool {
        self.inputs
            .values()
            .chain(self.member_prompt.iter().map(|prompt| &prompt.input))
            .chain(self.part_prompt.iter().map(|prompt| &prompt.input))
            .chain(self.paste_prompt.iter().flat_map(|prompt| prompt.inputs()))
            .chain(self.nick_prompts.iter().map(|prompt| &prompt.input))
            .chain(self.key_prompts.iter().map(|prompt| prompt.input()))
            .chain(std::iter::once(self.setup_wizard.nickname()))
            .any(|input| input.read(cx).is_composing())
    }

    /// Whether the clipboard has text and the selected conversation's input
    /// is not composing (a paste would replace the composition).
    fn can_paste_into_input(&self, cx: &App) -> bool {
        cx.read_from_clipboard()
            .is_some_and(|item| item.text().is_some())
            && self
                .inputs
                .get(&self.state.selection())
                .is_some_and(|input| !input.read(cx).is_composing())
    }

    fn log_menu_choice(
        &mut self,
        choice: LogMenuChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.log_menu.take() else {
            return;
        };
        // A menu left over from another conversation does nothing.
        if self.state.selection() != menu.source {
            cx.notify();
            return;
        }
        match choice {
            LogMenuChoice::Copy => self.copy_selected_log(cx),
            LogMenuChoice::Paste if !self.can_paste_into_input(cx) => cx.notify(),
            LogMenuChoice::Paste => {
                if let Some(input) = self.inputs.get(&self.state.selection()).cloned() {
                    window.focus(&input.focus_handle(cx));
                    input.update(cx, |input, cx| input.paste_clipboard(window, cx));
                }
            }
            LogMenuChoice::Mode | LogMenuChoice::Topic => {
                let topic = matches!(choice, LogMenuChoice::Topic);
                let initial = if topic {
                    self.state
                        .selected_channel()
                        .map(|channel| channel.topic.clone())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let kind = if topic {
                    MemberPromptKind::Topic
                } else {
                    MemberPromptKind::Mode
                };
                self.show_member_prompt(
                    menu.network,
                    menu.channel.clone(),
                    kind,
                    menu.position,
                    window,
                    cx,
                );
                if let Some(prompt) = self.member_prompt.as_ref().filter(|_| !initial.is_empty()) {
                    prompt.input.update(cx, |input, cx| {
                        input.set_text(&initial, cx);
                        input.select_everything(cx);
                    });
                }
            }
            LogMenuChoice::Nick => {
                let nickname = self
                    .own_nickname(menu.network)
                    .unwrap_or_default()
                    .to_owned();
                self.show_member_prompt(
                    menu.network,
                    nickname,
                    MemberPromptKind::Nick,
                    menu.position,
                    window,
                    cx,
                );
            }
            LogMenuChoice::Private => {
                // Opens the conversation without sending anything.
                if let Some(sender) = menu.sender {
                    let key = cayenchat_irc_core::text::nickname_key(&sender);
                    if let Some(id) = self.state.private_conversation(
                        menu.network,
                        &key,
                        &sender,
                        Missing::Create,
                    ) {
                        self.ensure_inputs(cx);
                        self.dispatch(Command::SelectChannel(id), window, cx);
                    } else {
                        let message = self.i18n.text("private_conversation_limit");
                        self.raise_error(Some(menu.network), message, notices::Origin::User, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    fn channel_menu_command(&mut self, join: bool, cx: &mut Context<Self>) {
        let Some(menu) = self.channel_menu.take() else {
            return;
        };
        self.join_or_part(menu.network, &menu.channel, join, cx);
    }

    /// Joins or parts `channel`, showing why it could not be done.
    fn join_or_part(
        &mut self,
        network: NetworkId,
        channel: &str,
        join: bool,
        cx: &mut Context<Self>,
    ) {
        let result = if join {
            self.join_channel(network, channel, cx)
        } else {
            self.registered_connection(network)
                .and_then(|connection| connection.send_command(&format!("/part {channel}"), None))
        };
        self.report_sent(network, result, cx);
        cx.notify();
    }

    /// What the toolbar's buttons can do now. Computed when the chat state
    /// changes (see `toolbar`), never while drawing.
    fn toolbar_state(&self) -> toolbar::State {
        let channel = self.state.selected_channel().filter(|c| !c.is_private());
        let registered = self
            .selected_network_id()
            .is_some_and(|network| self.state.is_registered(network));
        toolbar::State {
            connected: self.can_disconnect_selected(),
            can_connect: self.can_connect_selected() || self.can_reconnect_selected(),
            can_reconnect: self.can_reconnect_selected(),
            always_on_top: self.toolbar_always_on_top_state(),
            joined: channel.is_some_and(|c| self.state.is_active_channel(c.id)),
            can_join_or_part: registered && channel.is_some(),
            can_topic: registered && channel.is_some(),
            can_mode: registered && channel.is_some(),
            can_nick: registered,
            can_next_unread: self
                .state
                .conversations()
                .iter()
                .any(|conversation| self.state.is_unread(conversation.id)),
            can_whois: registered && self.can_whois_selected(),
            can_upload: self.inputs.contains_key(&self.state.selection()),
        }
    }

    /// Whether `whois_target` has someone, without walking the member list,
    /// which this runs for on every chat notification.
    fn can_whois_selected(&self) -> bool {
        let Some(conversation) = self.state.selected_channel() else {
            return false;
        };
        conversation.is_private() || self.member_selection.count(conversation.id) == 1
    }

    /// Whom the toolbar's WHOIS asks about: the peer of a private
    /// conversation, or the one member chosen in the member list.
    fn whois_target(&self) -> Option<String> {
        let conversation = self.state.selected_channel()?;
        if conversation.is_private() {
            return Some(conversation.name.clone());
        }
        let mut chosen = self
            .member_selection
            .nicknames(conversation.id, &conversation.members);
        (chosen.len() == 1).then(|| chosen.remove(0))
    }

    fn toolbar_join_or_part(&mut self, cx: &mut Context<Self>) {
        let Some(channel) = self.state.selected_channel().filter(|c| !c.is_private()) else {
            return;
        };
        let (network, name, join) = (
            channel.network,
            channel.name.clone(),
            !self.state.is_active_channel(channel.id),
        );
        self.join_or_part(network, &name, join, cx);
    }

    fn toolbar_whois(&mut self, cx: &mut Context<Self>) {
        let (Some(network), Some(nickname)) = (self.selected_network_id(), self.whois_target())
        else {
            return;
        };
        let result = self.request_whois(network, &nickname, cx);
        self.report_sent(network, result, cx);
        cx.notify();
    }

    /// The place of a prompt opened from the toolbar, in the middle of the
    /// window, where the member menu's prompts sit by default.
    fn centered_prompt_position(window: &Window) -> Point<Pixels> {
        let viewport = window.viewport_size();
        point(
            ((viewport.width - px(300.)) / 2.).max(px(0.)),
            ((viewport.height - px(140.)) / 2.).max(px(0.)),
        )
    }

    /// The toolbar's topic or mode change for the selected channel (#370):
    /// the same prompt the log's right-click menu opens, in the middle of
    /// the window.
    fn toolbar_channel_prompt(
        &mut self,
        kind: MemberPromptKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(channel) = self.state.selected_channel().filter(|c| !c.is_private()) else {
            return;
        };
        let topic = matches!(kind, MemberPromptKind::Topic);
        let initial = if topic {
            channel.topic.clone()
        } else {
            String::new()
        };
        self.show_member_prompt(
            channel.network,
            channel.name.clone(),
            kind,
            Self::centered_prompt_position(window),
            window,
            cx,
        );
        if let Some(prompt) = self.member_prompt.as_ref().filter(|_| !initial.is_empty()) {
            prompt.input.update(cx, |input, cx| {
                input.set_text(&initial, cx);
                input.select_everything(cx);
            });
        }
    }

    fn toolbar_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toolbar_channel_prompt(MemberPromptKind::Topic, window, cx);
    }

    fn toolbar_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toolbar_channel_prompt(MemberPromptKind::Mode, window, cx);
    }

    /// The toolbar's nickname change for the selected channel's network
    /// (#370): the same prompt the server menu opens, in the middle of the
    /// window.
    fn toolbar_nick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(network) = self.selected_network_id() else {
            return;
        };
        // The nickname in use, else the one the profile would connect with.
        let nickname = self
            .own_nickname(network)
            .map(str::to_owned)
            .or_else(|| {
                let session = self.sessions.get(&network)?;
                Some(self.saved.profile(&session.profile_id)?.nickname.clone())
            })
            .unwrap_or_default();
        self.show_member_prompt(
            network,
            nickname,
            MemberPromptKind::Nick,
            Self::centered_prompt_position(window),
            window,
            cx,
        );
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
                self.report(Some(error), cx);
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
                    let before = std::mem::replace(&mut config.channels, profile.channels());
                    channel_keys::follow_channel_spelling(config);
                    if channel_keys::keyed_channel_added(&before, profile) {
                        self.refresh_stored_keys(menu.network, profile, cx);
                    }
                }
                None
            }
            Err(error) => Some(error),
        };
        self.report(feedback, cx);
        cx.notify();
    }

    /// The text of a `/charset` line that cannot be run.
    fn charset_problem(&self, problem: &charset_command::Problem) -> String {
        match problem {
            charset_command::Problem::UnknownEncoding(name) => self.i18n.format(
                "charset_unknown",
                &[
                    ("name", &display::neutralize_bidi(name)),
                    ("encodings", &charset_command::supported_names()),
                ],
            ),
            charset_command::Problem::Usage => self.i18n.text("charset_usage"),
        }
    }

    /// Runs `/help`: the command list as lines of the selected server's log,
    /// whatever the connection state. The selection does not change.
    fn run_help(&mut self, request: help_command::Request) -> Result<(), String> {
        let network = self
            .selected_network_id()
            .ok_or_else(|| self.i18n.text("not_connected"))?;
        let text = match request {
            help_command::Request::List => self.i18n.text("help_text"),
            help_command::Request::Usage => self.i18n.text("help_usage"),
        };
        for line in text.lines() {
            self.state.append_server_message(network, line.to_owned());
        }
        Ok(())
    }

    /// Runs `/charset` for the selected channel. The result is a line in
    /// that channel's log; the override is saved with the server profile and
    /// applies to the live connection at once, and to later ones through the
    /// stored configuration. An error leaves everything as it was.
    fn run_charset(
        &mut self,
        request: charset_command::Request,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let not_channel = || self.i18n.text("charset_not_channel");
        let channel = self
            .state
            .selected_channel()
            .filter(|channel| !channel.is_private())
            .ok_or_else(not_channel)?;
        let (conversation, name) = (channel.id, channel.name.clone());
        let network = self.selected_network_id().ok_or_else(not_channel)?;
        let profile_id = self
            .sessions
            .get(&network)
            .map(|session| session.profile_id.clone())
            .ok_or_else(not_channel)?;
        let mapping = self.state.casemapping(network);
        let same = |left: &str, right: &str| mapping.same(left, right);
        let profile = self.saved.profile(&profile_id).ok_or_else(not_channel)?;
        let server = profile.encoding;
        let current = profile.channel_charset(&name, same).cloned();
        let shown_name = display::neutralize_bidi(&name);
        // An encoding with where it comes from.
        let describe = |side: Option<cayenchat_storage::TextEncoding>| {
            let (encoding, origin) = match side {
                Some(encoding) => (encoding, "charset_channel"),
                None => (server, "charset_server"),
            };
            format!("{} ({})", encoding.label(), self.i18n.text(origin))
        };
        let report = match request {
            charset_command::Request::Help => self.i18n.format(
                "charset_help",
                &[("encodings", &charset_command::supported_names())],
            ),
            charset_command::Request::Show => {
                let (decode, encode) = charset_command::apply(request, current.as_ref());
                let mut shown = self.i18n.format(
                    "charset_show",
                    &[
                        ("channel", &shown_name),
                        ("decode", &describe(decode)),
                        ("encode", &describe(encode)),
                    ],
                );
                // On a UTF8ONLY connection the saved values are not in effect.
                let ignored = [decode, encode]
                    .into_iter()
                    .flatten()
                    .any(|encoding| encoding != cayenchat_storage::TextEncoding::Utf8);
                let utf8_only = self
                    .sessions
                    .get(&network)
                    .and_then(|session| session.irc.as_ref())
                    .is_some_and(|connection| connection.utf8_only());
                if ignored && utf8_only {
                    shown.push(' ');
                    shown.push_str(&self.i18n.text("charset_note_utf8only"));
                }
                shown
            }
            charset_command::Request::Reset if current.is_none() => self
                .i18n
                .format("charset_reset_none", &[("channel", &shown_name)]),
            charset_command::Request::Reset | charset_command::Request::Set { .. } => {
                let (decode, encode) = charset_command::apply(request, current.as_ref());
                let label = |side: Option<cayenchat_storage::TextEncoding>| {
                    side.map(cayenchat_storage::TextEncoding::label)
                };
                let (was_decode, was_encode) =
                    charset_command::apply(charset_command::Request::Show, current.as_ref());
                let connection = self
                    .sessions
                    .get(&network)
                    .and_then(|session| session.irc.as_ref());
                if let Some(connection) = connection {
                    connection
                        .set_channel_encoding(&name, label(decode), label(encode))
                        .map_err(|error| {
                            if connection.utf8_only() {
                                self.i18n.text("charset_utf8only")
                            } else {
                                error
                            }
                        })?;
                }
                let revert = |this: &Self| {
                    if let Some(connection) = this
                        .sessions
                        .get(&network)
                        .and_then(|session| session.irc.as_ref())
                    {
                        let _ = connection.set_channel_encoding(
                            &name,
                            label(was_decode),
                            label(was_encode),
                        );
                    }
                };
                // Start from the file, which a settings window may have
                // updated (or from what this window knows, when the file
                // does not have the server).
                let mut settings = match settings_file::load() {
                    Ok(Some(settings)) if settings.profile(&profile_id).is_some() => settings,
                    Ok(_) => self.saved.clone(),
                    Err(error) => {
                        revert(self);
                        return Err(error);
                    }
                };
                let saved = settings
                    .servers
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                    .ok_or_else(|| {
                        revert(self);
                        not_channel()
                    })?;
                if let Err(error) = saved.set_channel_charset(&name, decode, encode, same) {
                    revert(self);
                    return Err(error);
                }
                let channel_charsets = saved.channel_charsets.clone();
                if let Err(error) = settings_file::save(&settings) {
                    revert(self);
                    return Err(error);
                }
                if let Some(profile) = self
                    .saved
                    .servers
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                {
                    profile.channel_charsets = channel_charsets;
                    if let Some(config) = self
                        .sessions
                        .get_mut(&network)
                        .and_then(|session| session.active_config.as_mut())
                    {
                        config.channel_encodings = channel_encodings(profile);
                    }
                }
                if decode.is_none() && encode.is_none() {
                    self.i18n.format(
                        "charset_reset",
                        &[("channel", &shown_name), ("encoding", server.label())],
                    )
                } else {
                    self.i18n.format(
                        "charset_set",
                        &[
                            ("channel", &shown_name),
                            ("decode", &describe(decode)),
                            ("encode", &describe(encode)),
                        ],
                    )
                }
            }
        };
        self.state
            .append_conversation_activity(conversation, report);
        cx.notify();
        Ok(())
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
        for channel in join_command_channels(command) {
            self.note_join(network, channel, false);
        }
    }

    /// A `/join` the user typed was sent: selects the channel once our JOIN
    /// of it is confirmed (see [`session::JoinSwitch`]). A target already
    /// joined gets no answer from the server, so it is selected now, unless
    /// a later target of the command is still to be joined.
    fn note_join_switch(
        &mut self,
        network: NetworkId,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if join_command_channels(command).is_empty() {
            return;
        }
        let casemapping = self.state.casemapping(network);
        let mut joined = None;
        let mut waiting = std::collections::HashSet::new();
        for channel in join_command_channels(command) {
            match self
                .state
                .channel_id(network, channel)
                .filter(|id| self.state.is_active_channel(*id))
            {
                Some(id) => {
                    joined = Some(id);
                    waiting.clear();
                }
                None => {
                    waiting.insert(casemapping.fold(channel));
                }
            }
        }
        if let Some(id) = joined {
            self.dispatch(Command::SelectChannel(id), window, cx);
        }
        let expected = self.state.selection();
        if let Some(session) = self.sessions.get_mut(&network) {
            session.join_switch = (!waiting.is_empty()).then_some(session::JoinSwitch {
                channels: waiting,
                expected,
            });
        }
    }

    /// Our JOIN of `channel` was confirmed: selects it if a typed `/join`
    /// asked for it and the user has not chosen another view since.
    fn join_confirmed_switch(&mut self, network: NetworkId, channel: &str) {
        let folded = self.state.casemapping(network).fold(channel);
        let selection = self.state.selection();
        let target = self.state.channel_id(network, channel);
        let Some(session) = self.sessions.get_mut(&network) else {
            return;
        };
        let Some(switch) = session.join_switch.as_mut() else {
            return;
        };
        if !switch.channels.remove(&folded) {
            return;
        }
        match target {
            Some(id) if switch.expected == selection => {
                switch.expected = Selection::Channel(id);
                self.state.dispatch(Command::SelectChannel(id));
                self.join_selected = Some(id);
            }
            _ => switch.channels.clear(),
        }
        if switch.channels.is_empty() {
            session.join_switch = None;
        }
    }

    /// A JOIN of `channel` was refused: it will not be selected.
    fn join_refused_switch(&mut self, network: NetworkId, channel: &str) {
        let folded = self.state.casemapping(network).fold(channel);
        if let Some(session) = self.sessions.get_mut(&network)
            && let Some(switch) = session.join_switch.as_mut()
        {
            switch.channels.remove(&folded);
            if switch.channels.is_empty() {
                session.join_switch = None;
            }
        }
    }

    fn join_channel(
        &mut self,
        network: NetworkId,
        channel: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.join_channels(network, &[channel.to_owned()], cx)?;
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
                #[cfg(target_os = "windows")]
                if self.always_on_top {
                    let _ = handle.update(cx, |_, window, _| {
                        if let Err(error) = set_window_always_on_top(window, true) {
                            log::warn!("Could not keep the WHOIS window on top: {error}");
                        }
                    });
                }
                self.whois_windows.insert(key, handle);
                self.refresh_whois_avatars(cx);
                cx.activate(true);
            }
            Err(error) => {
                let message = self.i18n.format("whois_open_failed", &[("error", &error)]);
                self.report(Some(message), cx);
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
            MemberPromptKind::Mode => "channel_mode_placeholder",
            MemberPromptKind::Topic => "channel_topic_placeholder",
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
        self.prompt_feedback = None;
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
        self.log_menu = None;
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
            // The user submitted this nickname, so a failure is announced.
            self.start_reconnect(network, true, cx);
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
        self.prompt_feedback = None;
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
        let kind = prompt.kind;
        let nickname = prompt.nickname.clone();
        // A channel list without a key joins with the saved keys.
        let plain_join = matches!(kind, MemberPromptKind::Join)
            .then(|| channel_keys::plain_join_channels(&format!("/join {}", value.trim())))
            .flatten();
        let result = if let Some(channels) = &plain_join {
            self.join_channels(network, channels, cx)
        } else {
            match self.registered_connection(network) {
                Ok(connection) => match kind {
                    MemberPromptKind::PrivateMessage if value.trim().is_empty() => {
                        Err(self.i18n.text("member_message_required"))
                    }
                    MemberPromptKind::PrivateMessage => {
                        connection.send_private_message(&nickname, &value, false)
                    }
                    MemberPromptKind::Invite => connection.send_member_command(
                        &nickname,
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
                    // The prompt's nickname is the channel. The value is not
                    // echoed anywhere: a mode may carry a channel key.
                    MemberPromptKind::Mode if value.trim().is_empty() => {
                        Err(self.i18n.text("channel_mode_required"))
                    }
                    MemberPromptKind::Mode => connection
                        .send_command(&format!("/mode {} {}", nickname, value.trim()), None),
                    MemberPromptKind::Topic if value.trim().is_empty() => {
                        Err(self.i18n.text("channel_topic_required"))
                    }
                    MemberPromptKind::Topic => connection
                        .send_command(&format!("/topic {} :{}", nickname, value.trim()), None),
                },
                Err(error) => Err(error),
            }
        };
        match result {
            Ok(()) => {
                if let Some(prompt) = self
                    .member_prompt
                    .take()
                    .filter(|prompt| matches!(prompt.kind, MemberPromptKind::Join))
                    && plain_join.is_none()
                {
                    self.note_join_request(prompt.network, &format!("/join {}", value.trim()));
                }
                self.prompt_feedback = None;
                window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
            }
            // The prompt stays open and shows the error itself.
            Err(error) => self.prompt_feedback = Some(error),
        }
        cx.notify();
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        let profile = self.selected_profile_id();
        self.open_settings_for(profile, window, cx);
    }

    /// Opens the settings window on the server the context menu was opened
    /// on, leaving the selection alone.
    fn open_server_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.server_menu.take() else {
            return;
        };
        let profile = self
            .sessions
            .get(&menu.network)
            .map(|session| session.profile_id.clone());
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
        // A settings window opened while a transfer writes would save its
        // older form over the result.
        if self.transfer_dialog_busy() {
            return;
        }
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
                self.report(Some(message), cx);
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
        let min_size = size(px(760.), px(540.));
        let wanted = size(px(900.), px(750.));
        // Beside the chat window on its own display. Windows and macOS only:
        // GPUI cannot place a window on Linux, where the usual place stays.
        let owner_display = window.display(cx);
        #[cfg(target_os = "windows")]
        let owner_scale = window.scale_factor();
        let (bounds, display_id) = match owner_display {
            Some(display) if cfg!(any(target_os = "windows", target_os = "macos")) => (
                window_layout::secondary_bounds(
                    window.bounds(),
                    display.bounds(),
                    wanted,
                    min_size,
                ),
                Some(display.id()),
            ),
            _ => (Bounds::centered(None, wanted, cx), None),
        };
        match cx.open_window(
            WindowOptions {
                app_id: Some(APP_ID.into()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                display_id,
                window_min_size: Some(min_size),
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
            Ok(handle) => {
                // GPUI converts the bounds with the scale of the display the
                // window is created on; put it right in physical pixels.
                // Test windows have no native handle, so they skip it.
                #[cfg(target_os = "windows")]
                if display_id.is_some() && !cfg!(test) {
                    let _ = handle.update(cx, |_, settings_window, _| {
                        placement::place_native(settings_window, bounds, owner_scale)
                    });
                }
                #[cfg(target_os = "windows")]
                if self.always_on_top {
                    let _ = handle.update(cx, |_, window, _| {
                        if let Err(error) = set_window_always_on_top(window, true) {
                            log::warn!("Could not keep the settings window on top: {error}");
                        }
                    });
                }
                self.settings_window = Some(handle)
            }
            Err(error) => {
                let message = self
                    .i18n
                    .format("settings_open_failed", &[("error", &error.to_string())]);
                self.report(Some(message), cx);
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

    /// Shows or hides the toolbar and remembers the choice in the settings
    /// file, starting from the file, which a settings window may have updated.
    fn toggle_toolbar(&mut self, _: &ToggleToolbar, _: &mut Window, cx: &mut Context<Self>) {
        let shown = !self.toolbar_shown;
        self.save_view_setting(|settings| settings.show_toolbar = shown, cx);
        self.set_toolbar_shown(shown, cx);
    }

    /// Hides the menu bar until a keyboard or pointer request (the settings window's checkbox)
    /// or shows it all the time, and remembers the choice.
    fn toggle_menu_bar(&mut self, _: &ToggleMenuBar, _: &mut Window, cx: &mut Context<Self>) {
        let auto_hide = self.menu_bar.always();
        self.save_view_setting(|settings| settings.menu_bar_auto_hide = auto_hide, cx);
        self.set_menu_bar_auto_hide(auto_hide, cx);
    }

    /// Saves a View menu choice, starting from the settings file, which a
    /// settings window may have updated.
    fn save_view_setting(&mut self, apply: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut settings = match settings_file::load() {
            Ok(Some(settings)) => settings,
            Ok(None) => self.saved.clone(),
            Err(error) => {
                self.report(Some(error), cx);
                return;
            }
        };
        apply(&mut settings);
        if let Err(error) = settings_file::save(&settings) {
            self.report(Some(error), cx);
        }
        self.saved.show_toolbar = settings.show_toolbar;
        self.saved.menu_bar_auto_hide = settings.menu_bar_auto_hide;
        self.saved.confirm_server_dblclick_disconnect = settings.confirm_server_dblclick_disconnect;
        self.saved.confirm_channel_dblclick_part = settings.confirm_channel_dblclick_part;
    }

    fn set_toolbar_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.toolbar_shown = shown;
        self.refresh_menus(cx);
        cx.notify();
    }

    fn set_menu_bar_auto_hide(&mut self, auto_hide: bool, cx: &mut Context<Self>) {
        self.menu_bar.set_always(!auto_hide);
        self.refresh_menus(cx);
        cx.notify();
    }

    fn refresh_menus(&self, cx: &mut Context<Self>) {
        cx.set_menus(app_menus(
            self.debug_enabled,
            self.toolbar_shown,
            !self.menu_bar.always(),
            &self.i18n,
        ));
    }

    /// Turns the debug transcript on or off. IRC lines are recorded after
    /// registration only while it is on.
    fn set_debug(&mut self, on: bool, cx: &mut Context<Self>) {
        self.debug_enabled = on;
        self.refresh_menus(cx);
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
        source: Selection,
        row: usize,
        byte: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = LogPosition { row, byte };
        self.log_selection = Some(LogSelection {
            source,
            anchor: position,
            cursor: position,
        });
        self.log_dragging = true;
        window.focus(&self.log_focus);
        cx.notify();
    }

    fn extend_log_selection(
        &mut self,
        source: Selection,
        row: usize,
        byte: usize,
        cx: &mut Context<Self>,
    ) {
        if self.log_dragging
            && let Some(selection) = self.log_selection.as_mut()
            && selection.source == source
        {
            selection.cursor = LogPosition { row, byte };
            cx.notify();
        }
    }

    fn finish_log_selection(&mut self) {
        self.log_dragging = false;
    }

    fn copy_selected_log(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.log_selection else {
            return;
        };
        if selection.source != self.state.selection() {
            return;
        }
        let (start, end) = selection.bounds();
        if start == end {
            return;
        }
        let Some(channel) = self.state.selected_channel() else {
            // A server's log shows its lines as they are.
            let Selection::Server(network) = selection.source else {
                return;
            };
            let messages = self.state.server_messages(network);
            let pieces: Vec<_> = (start.row..=end.row)
                .filter_map(|row| {
                    let message = messages.get(row)?;
                    let range = selection.range(row, message.text.len()).unwrap_or(0..0);
                    Some(message.text.get(range).unwrap_or_default().to_owned())
                })
                .collect();
            if !pieces.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(pieces.join("\n")));
            }
            return;
        };
        let decode = self.appearance.decode_char_refs;
        let pieces: Vec<_> = (start.row..=end.row)
            .filter_map(|row| {
                channel.messages.get(row).map(|message| {
                    let range = selection.range(row, message.text.len()).unwrap_or(0..0);
                    // An action copied whole keeps the `* alice` that makes it
                    // a `/me`; a part of it is copied as selected.
                    let lead = if message.action
                        && range.start == 0
                        && range.end == message.text.len()
                        && !range.is_empty()
                    {
                        sender_prefix(message)
                    } else {
                        String::new()
                    };
                    if !decode {
                        return lead + &message.text[range];
                    }
                    // Decoded references are copied as the characters shown.
                    lead + &compact_urls::Compact::new(
                        &message.text,
                        message.links.as_slice(),
                        false,
                        true,
                    )
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

    fn scroll_to_latest(&mut self, _: &ScrollToLatest, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_main_log_to_latest(cx);
    }

    /// Scrolls the selected conversation's main log to its newest line and
    /// follows new lines again. The one place the shortcut and the button
    /// (and, later, #545) go through. Nothing else changes: not the draft,
    /// the focus, the selection or the search; nothing happens under a
    /// question that has the window's attention.
    fn scroll_main_log_to_latest(&mut self, cx: &mut Context<Self>) {
        // The shortcut runs before a dialog's own key handling, so every
        // dialog drawn over the window has to be excluded here.
        if self.quit_prompt_open
            || self.part_prompt.is_some()
            || self.disconnect_prompt.is_some()
            || self.paste_prompt.is_some()
            || self.member_prompt.is_some()
            || !self.nick_prompts.is_empty()
            || !self.key_prompts.is_empty()
            || !self.transfer_dialog.is_idle()
            || self.setup_wizard_visible()
        {
            return;
        }
        self.sync_log_lists();
        if let Some(list) = self.main_lists.get(&self.state.selection()) {
            list.scroll_to_end();
            cx.notify();
        }
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
            TreeRow::Channel(id, _) => selection == Selection::Channel(id),
        });
        let target = match (key, current) {
            (ListKey::Parent, Some(at)) => self.tree_rows[..at]
                .iter()
                .rposition(|row| matches!(row, TreeRow::Server(_)))
                .filter(|_| matches!(self.tree_rows[at], TreeRow::Channel(..))),
            (ListKey::Child, Some(at)) => (at + 1 < self.tree_rows.len()
                && matches!(self.tree_rows[at], TreeRow::Server(_))
                && matches!(self.tree_rows[at + 1], TreeRow::Channel(..)))
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
            TreeRow::Channel(id, _) => Command::SelectChannel(id),
        };
        if key == ListKey::Activate {
            self.dispatch(command, window, cx);
        } else {
            self.dispatch_keeping_focus(command, window, cx);
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
        self.log_menu = None;
        // Selecting clears the unread mark, so note it first (#545).
        let unread_before: Vec<ConversationId> =
            if self.unread_log_position == UnreadLogPosition::Latest {
                self.state
                    .conversations()
                    .iter()
                    .map(|conversation| conversation.id)
                    .filter(|id| self.state.is_unread(*id))
                    .collect()
            } else {
                Vec::new()
            };
        let selection_before = self.state.selection();
        self.state.dispatch(command);
        self.log_selection = None;
        self.prompt_feedback = None;
        if let Some((id, sequence)) = self.state.take_reveal() {
            self.reveal_message(id, sequence, cx);
        } else if let selection @ Selection::Channel(id) = self.state.selection()
            && selection != selection_before
            && unread_before.contains(&id)
        {
            self.sync_log_lists();
            if let Some(list) = self.main_lists.get(&selection) {
                list.scroll_to_end();
            }
        }
        self.update_title(window);
        cx.notify();
        // Redraw the pane contents together with the native title change.
        window.refresh();
    }

    /// Scrolls the selected conversation's log to the line `sequence` and
    /// emphasizes it for a moment.
    fn reveal_message(&mut self, id: ConversationId, sequence: u64, cx: &mut Context<Self>) {
        self.sync_log_lists();
        if let Some(list) = self.main_lists.get_mut(&Selection::Channel(id)) {
            list.scroll_to_sequence(sequence);
        }
        self.reveal_flash = Some((id, sequence));
        cx.spawn(async move |this, cx| {
            Timer::after(REVEAL_FLASH).await;
            let _ = this.update(cx, |this, cx| {
                if this.reveal_flash == Some((id, sequence)) {
                    this.reveal_flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
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
        if self.wizard_nickname_focused(window, cx) {
            self.wizard_move_focus(false, window, cx);
            return;
        }
        if self
            .member_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.input.read(cx).focus_handle(cx).is_focused(window))
            || self.focused_nick_prompt(window, cx).is_some()
            || self.focused_key_prompt(window, cx).is_some()
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
                    // Ended for sleep is not the user's doing.
                    if let Some(session) = self.sessions.get_mut(&network)
                        && !session.asleep
                    {
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
                    // Joins answered on a new connection are not typed ones.
                    if let Some(session) = self.sessions.get_mut(&network) {
                        session.join_switch = None;
                    }
                    self.nick_prompts.retain(|prompt| prompt.network != network);
                    self.start_key_tracking(network);
                }
                Event::Joined { channel } | Event::JoinedReplayed { channel } => {
                    self.key_join_succeeded(network, channel, cx)
                }
                Event::ChannelActivity {
                    channel,
                    kind: ChannelActivityKind::JoinFailed { bad_key, .. },
                    ..
                } if self.key_join_refused(network, channel, *bad_key, cx) => {
                    // The key dialog is the notice; the refusal is not
                    // logged, as the JOIN usually works with the key.
                    self.join_refused_switch(network, channel);
                    let folded = self.state.casemapping(network).fold(channel);
                    if let Some(session) = self.sessions.get_mut(&network) {
                        session.pending_joins.remove(&folded);
                    }
                    continue;
                }
                _ => {}
            }
            self.handle_event(network, event, cx);
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
            self.refresh_whois_avatars(cx);
            self.ensure_inputs(cx);
            cx.notify();
        }
        if let Some(id) = self.join_selected.take()
            && let Some(handle) = self.window_handle
        {
            // The window is being updated now; catch up once it is free.
            cx.spawn(async move |_, cx| {
                let _ = handle.update(cx, |this, window, cx| {
                    if this.state.selection() == Selection::Channel(id) {
                        this.dispatch(Command::SelectChannel(id), window, cx);
                    }
                });
            })
            .detach();
        }
        if worker_closed && !disconnected {
            self.push_diagnostic(
                network,
                "IRC worker ended without a disconnect event.".into(),
            );
            self.handle_event(
                network,
                Event::Disconnected("IRC worker stopped unexpectedly.".into()),
                cx,
            );
            cx.notify();
        }
        if disconnected || worker_closed {
            if let Some(session) = self.sessions.get_mut(&network) {
                session.irc = None;
                session.join_switch = None;
            }
            self.end_key_tracking(network);
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

    fn handle_event(&mut self, network: NetworkId, event: Event, cx: &mut Context<Self>) {
        let refused = matches!(event, Event::Refused(_));
        let replayed = matches!(event, Event::JoinedReplayed { .. });
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
            Event::Joined { channel } | Event::JoinedReplayed { channel } => {
                let folded = self.state.casemapping(network).fold(&channel);
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.pending_joins.remove(&folded);
                }
                self.state.joined_channel(network, &channel);
                // A join from a history batch never confirms a typed `/join`.
                if !replayed {
                    self.join_confirmed_switch(network, &channel);
                }
                self.state.append_server_message(
                    network,
                    self.i18n.format("event_joined", &[("channel", &channel)]),
                );
            }
            Event::ChannelModes { channel, flags } => {
                self.state.set_channel_modes(network, &channel, &flags);
            }
            Event::OpRegain { channel, status } => {
                let key = match status {
                    OpRegainStatus::Rejoining => "event_op_regain_rejoining",
                    OpRegainStatus::Regained { modes, topic } => match (modes, topic) {
                        (true, true) => "event_op_regained_modes_topic",
                        (true, false) => "event_op_regained_modes",
                        (false, true) => "event_op_regained_topic",
                        (false, false) => "event_op_regained",
                    },
                    OpRegainStatus::RestoreFailed { modes, topic } => match (modes, topic) {
                        (true, true) => "event_op_restore_failed_modes_topic",
                        (true, false) => "event_op_restore_failed_modes",
                        _ => "event_op_restore_failed_topic",
                    },
                    OpRegainStatus::TopicTooLong => "event_op_topic_too_long",
                    OpRegainStatus::NotRegained => "event_op_not_regained",
                    OpRegainStatus::RejoinFailed => "event_op_rejoin_failed",
                };
                self.state.append_server_message(
                    network,
                    self.i18n.format(key, &[("channel", &channel)]),
                );
            }
            Event::Parted { channel } => {
                let folded = self.state.casemapping(network).fold(&channel);
                if let Some(session) = self.sessions.get_mut(&network) {
                    session.pending_parts.remove(&folded);
                }
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
                let (kind, text) = received_kind(&text, notice);
                // NOTICE is displayed, but never runs normal chat attention rules.
                let found = if kind.is_notice() {
                    Found::default()
                } else {
                    self.find_mentions(network, &sender, text, replayed)
                };
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
                if !self
                    .state
                    .append_channel_message_at(network, &channel, &sender, text, kind, meta)
                {
                    return;
                }
                let conversation = self.state.channel_id(network, &channel);
                if !replayed && conversation.is_some() {
                    self.log_message(
                        network,
                        Scope::Channel,
                        &channel,
                        &sender,
                        text,
                        kind,
                        server_time,
                    );
                }
                self.notify_message(
                    network,
                    ReceivedMessage {
                        channel: Some(&channel),
                        conversation,
                        sender: &sender,
                        text,
                        kind,
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
                if join_failed {
                    self.join_refused_switch(network, &channel);
                }
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
                let (kind, text) = received_kind(&text, notice);
                // NOTICE is displayed, but never runs normal chat attention rules.
                let found = if kind.is_notice() {
                    Found::default()
                } else {
                    self.find_mentions(network, &sender, text, replayed)
                };
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
                            text,
                            kind,
                            meta,
                            if notice { Unread::Leave } else { Unread::Mark },
                        ) {
                            return;
                        }
                        if !replayed {
                            self.log_message(
                                network,
                                Scope::Private,
                                &sender,
                                &sender,
                                text,
                                kind,
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
                        } else if kind.is_action() {
                            format!("* {sender} ")
                        } else {
                            format!("<{sender}> ")
                        };
                        let meta = meta.after_prefix(prefix.len());
                        self.state
                            .append_server_message_at(network, prefix + text, meta);
                    }
                }
                self.notify_message(
                    network,
                    ReceivedMessage {
                        channel: None,
                        conversation,
                        sender: &sender,
                        text,
                        kind,
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
                let (kind, text) = received_kind(&text, notice);
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
                        text,
                        kind,
                        server_time,
                    );
                }
                self.append_own_private(network, &target, text, kind, meta);
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
                            && selection.source == Selection::Channel(id)
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
            Event::CommandRejected {
                code,
                command,
                reason,
            } => self.server_rejected(network, code, command, reason, cx),
            Event::CtcpLine(line) => self.state.append_system_message(network, line),
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
                    self.raise_error(Some(network), message, notices::Origin::User, cx);
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
                // A `/me` is shown by its own text like a received one.
                let (kind, shown) = match action.as_deref() {
                    Some(action) => (Kind::Action, action),
                    None => (Kind::from_notice(notice), text.as_str()),
                };
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
                        shown,
                        kind,
                        Provenance::Live,
                    );
                } else if cayenchat_irc_core::valid_nickname(&channel) {
                    self.append_own_private(network, &channel, shown, kind, MessageMeta::live());
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
                            .insert(local_id, (id, self.state.latest_sequence(), kind));
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
                if let Some((id, sequence, kind)) = pending {
                    let meta =
                        irc_message_meta(server_time, msgid.as_deref(), None, Provenance::Live);
                    // The final text of an ACTION is its own text.
                    let text = if kind.is_action() { action } else { text };
                    self.state.confirm_message(id, sequence, text, kind, meta);
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
                // Only a refusal (no automatic reconnect) needs the user; a
                // connection that ended by itself is reconnected and logged.
                let announce = refused;
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
                self.record_disconnect(network, reason, announce, cx);
            }
            // Avatar references are recorded whether or not they are shown;
            // showing them (and downloading images) is the Appearance setting.
            Event::UserAvatar { nickname, url } => {
                let key = cayenchat_irc_core::text::nickname_key(&nickname);
                self.state.set_avatar(network, &key, url.as_deref());
            }
            Event::PeerAvatar { nickname, url } => {
                let key = cayenchat_irc_core::text::nickname_key(&nickname);
                self.state.set_peer_avatar(network, &key, url.as_deref());
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
        kind: Kind,
        meta: MessageMeta,
    ) {
        let own = self.own_nickname(network).unwrap_or("me").to_owned();
        let key = cayenchat_irc_core::text::nickname_key(target);
        match self
            .state
            .private_conversation(network, &key, target, Missing::Create)
        {
            Some(id) => {
                self.state
                    .append_conversation_message(id, &own, text, kind, meta, Unread::Leave);
            }
            None => self.state.append_server_message_at(
                network,
                if kind.is_action() {
                    format!("→ {target} * {own} {text}")
                } else {
                    format!("→ {target} <{own}> {text}")
                },
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
        // Local to the application: nothing of it goes to the server, so it
        // works before registration and while disconnected too.
        let charset = charset_command::parse(&text);
        let help = help_command::parse(&text);
        let local_command = charset.is_some() || help.is_some();
        let result = if let Some(request) = help {
            self.run_help(request)
        } else if let Some(parsed) = charset {
            match parsed {
                Ok(request) => self.run_charset(request, cx),
                Err(problem) => Err(self.charset_problem(&problem)),
            }
        } else if let Some((network, channels)) = network
            .filter(|network| {
                connection.is_some()
                    && self.state.status(*network) == Some(&ConnectionStatus::Registered)
            })
            .zip(channel_keys::plain_join_channels(&text))
        {
            // Joins with the saved keys; `/join #c key` is sent as typed.
            self.join_channels(network, &channels, cx)
        } else if let Some(connection) = connection {
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
        match result {
            Ok(()) => {
                // A plain `/join` was noted when it was sent, with its keys.
                if let Some(network) = network {
                    if channel_keys::plain_join_channels(&text).is_none() {
                        self.note_join_request(network, &text);
                    }
                    self.note_join_switch(network, &text, window, cx);
                    // The one place a send is remembered for a 417 or 400:
                    // what the user said, not commands and not local ones.
                    if !local_command && notices::is_spoken(&text) {
                        self.note_spoken(network);
                    }
                }
                self.input_history.record(&text, private_peer.as_deref());
                input.update(cx, |input, cx| input.clear_after_send(cx));
            }
            // The draft stays as typed; the dialog says why it was not sent.
            Err(error) => self.raise_error(network, error, notices::Origin::User, cx),
        }
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
        self.submit(false, window, cx);
    }

    fn send_message_with_ctrl(
        &mut self,
        _: &SendMessageWithCtrl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.submit(true, window, cx);
    }

    /// The optional NOTICE key (bound only when the setting picks one).
    fn send_notice(&mut self, _: &SendNotice, window: &mut Window, cx: &mut Context<Self>) {
        self.submit_with(Submit::Notice, window, cx);
    }

    /// Enter (`ctrl` false) or Ctrl+Enter. The other inputs treat both as
    /// "done"; the chat draft sends only with the key the setting chose.
    fn submit(&mut self, ctrl: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.submit_with(Submit::Key { ctrl }, window, cx);
    }

    fn submit_with(&mut self, press: Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.wizard_nickname_focused(window, cx) {
            self.wizard_connect(window, cx);
        } else if self.part_prompt_field_focused(window, cx) {
            self.confirm_part_prompt(window, cx);
        } else if self.submit_paste_prompt_field(window, cx) {
        } else if let Some(network) = self.focused_nick_prompt(window, cx) {
            self.submit_nick_prompt(network, window, cx);
        } else if let Some((network, channel)) = self.focused_key_prompt(window, cx) {
            self.submit_key_prompt(network, &channel, window, cx);
        } else if self
            .member_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.input.read(cx).focus_handle(cx).is_focused(window))
        {
            self.submit_member_prompt(window, cx);
        } else {
            match press {
                Submit::Key { ctrl } => self.send_chat_draft(ctrl, window, cx),
                Submit::Notice => {
                    self.double_enter.reset();
                    self.send_draft(true, window, cx);
                }
            }
        }
    }

    /// Sends the chat draft as a normal message if this key press is the
    /// chosen one. `/notice` is the only way to send a NOTICE.
    fn send_chat_draft(&mut self, ctrl: bool, window: &mut Window, cx: &mut Context<Self>) {
        match (self.send_key, ctrl) {
            (SendKey::Enter, false) | (SendKey::CtrlEnter, true) => {
                self.double_enter.reset();
                self.send_draft(false, window, cx);
            }
            (SendKey::DoubleEnter, false) => {
                let selection = self.state.selection();
                let input = self.inputs[&selection].clone();
                let input = input.read(cx);
                if input.is_composing() || input.text().trim().is_empty() {
                    self.double_enter.reset();
                    return;
                }
                let (text, cursor) = (input.text().to_owned(), input.selection());
                if self
                    .double_enter
                    .press(selection, &text, cursor, Instant::now())
                {
                    self.send_draft(false, window, cx);
                }
            }
            _ => self.double_enter.reset(),
        }
    }

    /// Any key other than a plain Enter ends the wait of "Enter twice";
    /// releasing Enter tells a second press from the key's auto-repeat.
    fn watch_enter_keys(window: &Window, cx: &mut Context<Self>) -> Subscription {
        let handle = window.window_handle();
        let owner = cx.weak_entity();
        cx.intercept_keystrokes(move |event, window, cx| {
            let key = &event.keystroke;
            if window.window_handle() != handle || (key.key == "enter" && !key.modifiers.modified())
            {
                return;
            }
            let _ = owner.update(cx, |this, _| this.double_enter.reset());
        })
    }

    /// Starts "Enter twice" over when the draft is not the one that waited.
    fn check_double_enter(&mut self, window: &Window, cx: &App) {
        let selection = self.state.selection();
        let focused = self
            .inputs
            .get(&selection)
            .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window));
        self.double_enter.keep_only_for(selection, focused);
    }
}

/// What was pressed to finish an input.
#[derive(Clone, Copy)]
enum Submit {
    /// Enter, or Ctrl+Enter when `ctrl`.
    Key { ctrl: bool },
    /// The NOTICE key from the settings.
    Notice,
}

/// The hint in an empty draft, naming the key that sends.
fn draft_placeholder(i18n: &Localizer, send_key: SendKey) -> String {
    i18n.text(match send_key {
        SendKey::Enter => "draft_placeholder",
        SendKey::CtrlEnter => "draft_placeholder_ctrl_enter",
        SendKey::DoubleEnter => "draft_placeholder_double_enter",
    })
}

fn notification_rules(settings: &Notifications) -> NotificationRules {
    NotificationRules {
        enabled: settings.enabled,
        mentions: settings.mentions,
        keyword_alerts: settings.keyword_alerts,
        keywords: settings.keywords.clone(),
        exclude_keywords: settings.exclude_keywords.clone(),
        private_messages: settings.private_messages,
        sound: settings.sound,
    }
}

/// The toolbar's "keep on top" button exists on Windows only.
#[cfg(not(target_os = "windows"))]
impl ChatWindow {
    fn toolbar_always_on_top_state(&self) -> bool {
        false
    }

    fn toolbar_always_on_top(&mut self, _: &mut Window, _: &mut Context<Self>) {}
}

#[cfg(target_os = "windows")]
impl ChatWindow {
    fn toolbar_always_on_top_state(&self) -> bool {
        self.always_on_top
    }

    /// Switches the preference the way the settings window's checkbox does:
    /// it is saved (starting from the file, which that window may have
    /// updated) and applied to the open windows. An open settings window
    /// shows the new value.
    fn toolbar_always_on_top(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let enabled = !self.always_on_top;
        match settings_file::load() {
            Ok(file) => {
                let mut settings = file.unwrap_or_else(|| self.saved.clone());
                settings.always_on_top = enabled;
                if let Err(error) = settings_file::save(&settings) {
                    self.report(Some(error), cx);
                }
            }
            Err(error) => self.report(Some(error), cx),
        }
        self.saved.always_on_top = enabled;
        self.apply_always_on_top(enabled, window, cx);
        if let Some(handle) = self.settings_window {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |settings, _, cx| settings.follow_saved_file(cx));
            });
        }
    }

    /// Applies the Windows-only preference to the chat window and to the open
    /// settings and WHOIS windows, and remembers it for windows opened later.
    /// Dialogs follow their parent window. Callers apply it only when the
    /// preference is on at startup or changes; test builds skip the native call.
    fn apply_always_on_top(&mut self, enabled: bool, window: &Window, cx: &mut Context<Self>) {
        self.always_on_top = enabled;
        if let Err(error) = set_window_always_on_top(window, enabled) {
            self.report(Some(error), cx);
        }
        for handle in self.whois_windows.values() {
            let _ = handle.update(cx, |_, window, _| {
                if let Err(error) = set_window_always_on_top(window, enabled) {
                    log::warn!("Could not change the WHOIS window's Z-order: {error}");
                }
            });
        }
        // The settings window is mid-update when it saves, so reach it after.
        if let Some(handle) = self.settings_window {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| {
                    if let Err(error) = set_window_always_on_top(window, enabled) {
                        log::warn!("Could not change the settings window's Z-order: {error}");
                    }
                });
            });
        }
        cx.notify();
    }
}

/// Puts a CayenChat window above (or back among) ordinary windows.
/// Uses the native handle without changing vendored GPUI.
#[cfg(target_os = "windows")]
fn set_window_always_on_top(window: &Window, enabled: bool) -> Result<(), String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
        SetWindowPos,
    };

    // Test windows have no native handle (GPUI's is `unimplemented!()`), and
    // tests can change the preference, so they never reach it.
    if cfg!(test) {
        return Ok(());
    }
    // GPUI has an inherent window_handle() method (its own view handle), so
    // explicitly call the raw-window-handle trait for the native HWND.
    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(native) = handle.as_raw() else {
        return Err("The window does not have a Win32 handle".into());
    };
    let hwnd = native.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let order = if enabled {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    let flags = SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER;
    if unsafe { SetWindowPos(hwnd, order, 0, 0, 0, 0, flags) } == 0 {
        return Err(format!(
            "Could not change the window's Z-order: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
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
/// highlight color, search matches on a yellow background and the selection
/// shaded.
///
/// `prefix` (the sender and colon in the flowing layout) is drawn in the
/// nickname color before `text`; every other range, and `selected`, is a
/// byte offset into `text`. `found` are the search matches, `found_current`
/// says this row is the one the search stands on.
#[allow(clippy::too_many_arguments)] // Each is one independent set of ranges.
fn styled_log_text(
    prefix: &str,
    text: &str,
    urls: &[(std::ops::Range<usize>, String)],
    chips: &[std::ops::Range<usize>],
    highlights: &[std::ops::Range<usize>],
    found: &[std::ops::Range<usize>],
    found_current: bool,
    selected: Option<std::ops::Range<usize>>,
    theme: &Theme,
) -> StyledText {
    let shift = prefix.len();
    let moved = |range: &std::ops::Range<usize>| range.start + shift..range.end + shift;
    let found: Vec<_> = found.iter().map(moved).collect();
    let urls: Vec<_> = urls.iter().map(|(range, _)| moved(range)).collect();
    let chips: Vec<_> = chips.iter().map(moved).collect();
    let highlights: Vec<_> = highlights.iter().map(moved).collect();
    let selected = selected.as_ref().map(moved);
    let mut boundaries = vec![0, shift, shift + text.len()];
    for range in urls.iter().chain(&highlights).chain(&chips).chain(&found) {
        boundaries.extend([range.start, range.end]);
    }
    if let Some(range) = &selected {
        boundaries.extend([range.start, range.end]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut next_found = 0;
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
        // Matches ascend and do not overlap, as do the segments: step past
        // the ones already behind instead of testing them all each time.
        while found
            .get(next_found)
            .is_some_and(|span| span.end <= range.start)
        {
            next_found += 1;
        }
        let is_found = found
            .get(next_found)
            .is_some_and(|span| span.start <= range.start && range.end <= span.end);
        (is_prefix || is_url || is_chip || is_selected || is_highlight || is_found).then_some((
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
                } else if is_found {
                    Some(search_match_color(found_current).into())
                } else {
                    link_background(theme, is_url, is_chip).map(Into::into)
                },
                ..Default::default()
            },
        ))
    });
    StyledText::new(format!("{prefix}{text}")).with_highlights(highlights)
}

/// Where the room kept at byte `start` is painted. At a wrap boundary the
/// index belongs to the end of the previous row while the room itself starts
/// the next one; the index just past the room is then on another row.
fn link_icon_origin(layout: &TextLayout, start: usize) -> Option<Point<Pixels>> {
    // The icon sits after the leading pad and before the gap.
    let icon = start + compact_urls::LEAD.len();
    let position = layout.position_for_index(icon)?;
    match layout.position_for_index(start + compact_urls::ICON.len()) {
        Some(after) if after.y > position.y => Some(point(layout.bounds().origin.x, after.y)),
        _ => Some(position),
    }
}

/// The zero-size canvas that paints the link icon over the room
/// ([`compact_urls::ICON`]) kept at the start of each drawn URL. `starts` are
/// byte offsets into the laid-out text (prefix included); the layout is only
/// valid once the text is painted, so the canvas goes after it.
fn link_icons(layout: TextLayout, starts: Vec<usize>, color: Rgba) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |_, _, window, cx| {
            let line = layout.line_height();
            let size = line * 0.65;
            for start in &starts {
                if let Some(position) = link_icon_origin(&layout, *start) {
                    let bounds = Bounds::new(
                        point(position.x, position.y + (line - size) / 2.),
                        gpui::size(size, size),
                    );
                    let _ = window.paint_svg(
                        bounds,
                        "icons/ui/link.svg".into(),
                        TransformationMatrix::unit(),
                        color.into(),
                        cx,
                    );
                }
            }
        },
    )
    .absolute()
    .size_0()
}

/// The background of a search match: yellow, orange on the line the search
/// stands on. Translucent, so the text stays readable in light and dark.
fn search_match_color(current: bool) -> Rgba {
    if current {
        Rgba {
            r: 1.,
            g: 0.55,
            b: 0.,
            a: 0.6,
        }
    } else {
        Rgba {
            r: 1.,
            g: 0.85,
            b: 0.,
            a: 0.4,
        }
    }
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
            window.os_set_title(&title);
            self.shown_title = title.clone();
        }
        self.check_double_enter(window, cx);
        let content = self.render_chat(window, cx);
        let content = if self.toolbar_shown {
            let bar = match &self.toolbar {
                Some(bar) => bar.clone(),
                None => {
                    let bar = toolbar::view(self.toolbar_state(), cx);
                    self.toolbar = Some(bar.clone());
                    bar
                }
            };
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    bar.cached(
                        StyleRefinement::default()
                            .w_full()
                            .h(toolbar::HEIGHT)
                            .flex_shrink_0(),
                    ),
                )
                .child(div().flex_1().min_h_0().child(content))
                .into_any_element()
        } else {
            content
        };
        let content = menu_bar::wrap(
            &self.menu_bar,
            cx.get_menus().unwrap_or_default(),
            menu_access_keys(&self.i18n),
            content,
            |this| &mut this.menu_bar,
            |this, cx| this.is_composing(cx),
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
    fn render_channel_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
            for (position, conversation) in self
                .state
                .conversations()
                .iter()
                .filter(|c| c.network == network.id)
                .enumerate()
            {
                self.tree_rows
                    .push(TreeRow::Channel(conversation.id, position));
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
        // Whatever changed the selection (shortcut, tree key, click), bring
        // its row into view once, by the row order just built; scrolling the
        // tree by hand later is left alone.
        let selection = self.state.selection();
        let changed = self.tree_revealed != Some(selection);
        if changed || self.tree_reveal_pending {
            self.tree_revealed = Some(selection);
            let at = self.tree_rows.iter().position(|row| match *row {
                TreeRow::Server(id) => selection == Selection::Server(id),
                TreeRow::Channel(id, _) => selection == Selection::Channel(id),
            });
            self.tree_reveal_pending = false;
            if let Some(at) = at {
                // Rows far from the viewport are not laid out yet; the list
                // then needs further passes, each after a layout.
                if self.tree_list.reveal(at) {
                    self.tree_reveal_pending = true;
                    // Notifying while drawing does not schedule another
                    // frame; the next one is requested explicitly.
                    cx.on_next_frame(window, |_, _, cx| cx.notify());
                }
            }
        }
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
                            this.log_menu = None;
                            this.member_prompt = None;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.dispatch_keeping_focus(Command::SelectServer(server_id), window, cx);
                        // The double click is one gesture: only its second
                        // click acts, so it cannot connect and then disconnect.
                        if event.click_count() == 2 {
                            this.activate_server_row(server_id, window, cx);
                        }
                    }))
                    .into_any_element()
            }
            Some(TreeRow::Channel(id, index)) => {
                let Some(conversation) = self.state.conversation(id) else {
                    return div().into_any_element();
                };
                let unread = self.state.is_unread(id);
                let highlighted = self.state.is_highlighted(id);
                let name = conversation.name.clone();
                let network = conversation.network;
                let private = conversation.is_private();
                let joined = self.state.is_active_channel(id);
                // While a channel is dragged, every row shows the digit that
                // reaches it, and the row under the pointer shows where the
                // drop would put the dragged channel.
                let dragging = self.dragging_channel.is_some();
                let digit = shortcut_digit(index).filter(|_| dragging);
                let drop_line_on_top = self
                    .dragging_channel
                    .zip(self.drop_target)
                    .filter(|(dragged, target)| dragged != target && *target == id)
                    .and_then(|(dragged, _)| drop_line_on_top(&self.tree_rows, dragged, index));
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
                            this.log_menu = None;
                            this.member_prompt = None;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.dispatch_keeping_focus(Command::SelectChannel(id), window, cx);
                        // Only the second click of a double click acts, on
                        // the row clicked, whichever channel was selected.
                        if event.click_count() == 2 {
                            this.activate_channel_row(id, window, cx);
                        }
                    }))
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    /// Content of a cached pane; see [`ChatPane`].
    fn render_pane(
        &mut self,
        kind: PaneKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                let latest_button = main
                    .is_behind_latest()
                    .then(|| Self::render_latest_button(&theme, cx));
                #[cfg(test)]
                {
                    self.latest_button_drawn = latest_button.is_some();
                }
                // The button follows the previous frame's layout, which
                // changes without a scroll when the window is resized or a
                // row is measured. Check once after this frame is laid out and
                // draw again only when the answer changed.
                let shown = latest_button.is_some();
                let selected = selection;
                let chat = cx.weak_entity();
                cx.defer(move |cx| {
                    let _ = chat.update(cx, |chat, cx| {
                        let behind = chat
                            .main_lists
                            .get(&selected)
                            .is_some_and(|list| list.is_behind_latest());
                        if behind != shown && chat.state.selection() == selected {
                            cx.notify();
                        }
                    });
                });
                div()
                    .relative()
                    .size_full()
                    .child(
                        list(main.state.clone(), cx.processor(Self::render_main_row)).size_full(),
                    )
                    .child(bar)
                    .children(latest_button)
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
            PaneKind::Channels => self.render_channel_tree(window, cx),
        }
    }

    /// The button over the main log's lower right corner that scrolls to the
    /// newest line (#546). Drawn only while the end of the log is out of
    /// view. It sits left of the scrollbar's strip and takes its own clicks,
    /// so neither the log's selection nor the focus is touched.
    fn render_latest_button(theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        const SIZE: f32 = 28.;
        let hover = theme.hover_strong;
        let chat = cx.weak_entity();
        div()
            .id("scroll-to-latest")
            .debug_selector(|| "scroll-to-latest".to_owned())
            .absolute()
            .right(px(16.))
            .bottom(px(8.))
            .size(px(SIZE))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(SIZE / 2.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .shadow_sm()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .occlude()
            .child(
                svg()
                    .path("icons/ui/arrow-down-to-line.svg")
                    .size(px(16.))
                    .text_color(theme.text),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.scroll_main_log_to_latest(cx)))
            .tooltip(move |_, cx| {
                let name = chat
                    .read_with(cx, |chat, _| chat.i18n.text("shortcut_scroll_to_latest"))
                    .unwrap_or_default();
                let text = toolbar::action_tooltip(&name, "scroll_to_latest", cx);
                cx.new(|_| tooltip::TextTooltip(text.into())).into()
            })
            .into_any_element()
    }

    fn render_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::current(cx);
        self.sync_log_lists();
        self.search_refresh_if_stale(window, cx);
        self.search_reveal_step(window);
        let search_bar = self.render_search_bar(cx);
        let selection = self.state.selection();
        let mut origin = decorations::content_origin(window);
        origin.y += self.menu_bar.height();
        if self.toolbar_shown {
            origin.y += toolbar::HEIGHT;
        }
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
            // A right click off the message rows (blank space or a log
            // without rows) offers the menu without a speaker.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.open_log_menu(event.position, None, window, cx);
                    cx.stop_propagation();
                }),
            )
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
                this.drop_paths(paths.paths(), paths.keep_alive(), window, cx)
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
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cancel_upload(window, cx)
                                })),
                        ),
                )
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

        let server_menu =
            self.server_menu.as_ref().map(|menu| {
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
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.disconnect(network, cx);
                                    }))
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
                    .child(
                        div()
                            .id("server-menu-settings")
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.hover_strong))
                            .child(self.i18n.text("server_menu_settings"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_server_settings(window, cx)
                            })),
                    )
                    .child(div().my_1().border_t_1().border_color(theme.separator))
                    .children([(true, "tree_move_up"), (false, "tree_move_down")].map(
                        |(up, key)| {
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
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.move_server(up, cx)
                                        }))
                                })
                                .when(!enabled, |d| d.text_color(theme.text_muted))
                        },
                    ))
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
        let log_menu = self.log_menu.as_ref().map(|menu| {
            let registered = self.registered_connection(menu.network).is_ok();
            let mut groups = vec![vec![
                (
                    LogMenuChoice::Copy,
                    self.i18n.text("log_menu_copy"),
                    self.has_log_selection(menu.source),
                ),
                (
                    LogMenuChoice::Paste,
                    self.i18n.text("log_menu_paste"),
                    menu.can_paste,
                ),
            ]];
            if !menu.private && matches!(menu.source, Selection::Channel(_)) {
                groups.push(vec![
                    (
                        LogMenuChoice::Mode,
                        self.i18n.text("log_menu_mode"),
                        registered,
                    ),
                    (
                        LogMenuChoice::Topic,
                        self.i18n.text("log_menu_topic"),
                        registered,
                    ),
                ]);
            }
            groups.push(vec![(
                LogMenuChoice::Nick,
                self.i18n.text("log_menu_nick"),
                registered,
            )]);
            if let Some(sender) = &menu.sender {
                groups.push(vec![(
                    LogMenuChoice::Private,
                    self.i18n.format_nickname("log_menu_private", sender),
                    true,
                )]);
            }
            let mut popup = div()
                .id("log-context-menu")
                .occlude()
                .w(px(230.))
                .p_1()
                .bg(theme.surface)
                .border_1()
                .border_color(border)
                .shadow_md();
            for (group, entries) in groups.into_iter().enumerate() {
                if group > 0 {
                    popup = popup.child(div().my_1().border_t_1().border_color(theme.separator));
                }
                for (index, (choice, label, enabled)) in entries.into_iter().enumerate() {
                    popup = popup.child(
                        div()
                            .id(("log-menu-item", group * 8 + index))
                            .px_2()
                            .py_1()
                            .child(label)
                            .when(enabled, |d| {
                                d.cursor_pointer()
                                    .hover(|d| d.bg(theme.hover_strong))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.log_menu_choice(choice, window, cx)
                                    }))
                            })
                            .when(!enabled, |d| d.text_color(theme.text_muted)),
                    );
                }
            }
            popup
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
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.set_member_ctcp_open(false, cx);
                            }
                        }))
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
            // The CTCP submenu opens to the right, or to the left when the
            // window has no room there. Opening it sends nothing.
            let submenu_left = menu.position.x + px(210. * 2.) > window.viewport_size().width;
            let mut ctcp_row = div()
                .id("member-menu-ctcp")
                .relative()
                .flex()
                .justify_between()
                .px_2()
                .py_1()
                .child(self.i18n.text("member_ctcp"))
                .child("▶")
                .when(enabled, |d| {
                    d.cursor_pointer()
                        .hover(|d| d.bg(theme.hover_strong))
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.set_member_ctcp_open(true, cx);
                            }
                        }))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let open = this.member_menu.as_ref().is_some_and(|m| !m.ctcp_open);
                            this.set_member_ctcp_open(open, cx);
                        }))
                })
                .when(!enabled, |d| d.text_color(theme.text_muted));
            if enabled && menu.ctcp_open {
                let mut submenu = div()
                    .id("member-menu-ctcp-items")
                    .occlude()
                    .absolute()
                    .top(px(-4.))
                    .w(px(130.))
                    .p_1()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(border)
                    .shadow_md();
                submenu = if submenu_left {
                    submenu.right(px(210. - 8.))
                } else {
                    submenu.left(px(210. - 8.))
                };
                for (index, (key, tag)) in MEMBER_CTCP_ITEMS.into_iter().enumerate() {
                    submenu = submenu.child(
                        div()
                            .id(("member-menu-ctcp-item", index))
                            .px_2()
                            .py_1()
                            .child(self.i18n.text(key))
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.hover_strong))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.member_command(
                                    MemberCommand::Ctcp {
                                        tag: tag.to_owned(),
                                    },
                                    cx,
                                )
                            })),
                    );
                }
                ctcp_row = ctcp_row.child(submenu);
            }
            popup = popup
                .child(div().my_1().border_t_1().border_color(theme.separator))
                .child(ctcp_row);
            // On our own row in a channel, changing the nickname comes last.
            if !menu.private && self.is_own_nickname(menu.network, &menu.nickname) {
                popup = popup
                    .child(div().my_1().border_t_1().border_color(theme.separator))
                    .child(
                        div()
                            .id("member-menu-nick")
                            .px_2()
                            .py_1()
                            .child(self.i18n.text("server_menu_nick"))
                            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                if *hovered {
                                    this.set_member_ctcp_open(false, cx);
                                }
                            }))
                            .when(enabled, |d| {
                                d.cursor_pointer()
                                    .hover(|d| d.bg(theme.hover_strong))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_member_prompt(MemberPromptKind::Nick, window, cx)
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
                MemberPromptKind::Nick | MemberPromptKind::Mode | MemberPromptKind::Topic => {
                    "nickname_change_submit"
                }
                MemberPromptKind::PrivateMessage | MemberPromptKind::Invite => "member_submit",
            });
            div()
                .id("member-prompt")
                .debug_selector(|| "member-prompt".into())
                // Clicks on the dialog must not reach the message input behind it.
                .occlude()
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
                .when_some(self.prompt_feedback.clone(), |d, feedback| {
                    d.child(div().text_color(theme.warning).child(feedback))
                })
                .child(
                    button_order::row(div().flex().gap_2())
                        .child(
                            div()
                                .id("member-prompt-submit")
                                .debug_selector(|| "member-prompt-submit".into())
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
                                .debug_selector(|| "member-prompt-cancel".into())
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
        let key_prompts = self.render_key_prompts(origin, window, cx);
        let quit_dialog = self.render_quit_dialog(window, cx);
        let part_prompt = self.render_part_prompt(window, cx);
        let disconnect_prompt = self.render_disconnect_prompt(window, cx);
        if let Some((input, text)) = self.pending_paste.take() {
            self.open_paste_prompt(&input, &text, window, cx);
        }
        let paste_prompt = self.render_paste_prompt(window, cx);
        let transfer_dialog = self.render_transfer_dialog(window, cx);
        let setup_wizard = self.render_setup_wizard(window, cx);
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
            // Whatever has the focus (the message input, the log, the
            // member list), Escape closes the log menu first.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.log_menu.take().is_some() {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(theme.text)
            .bg(theme.surface)
            .on_action(cx.listener(Self::navigate))
            .on_action(cx.listener(Self::scroll_to_latest))
            .on_action(cx.listener(Self::complete_nickname))
            .on_action(cx.listener(Self::send_message))
            .on_action(cx.listener(Self::send_message_with_ctrl))
            .on_action(cx.listener(Self::send_notice))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                if event.keystroke.key == "enter" {
                    this.double_enter.key_up();
                }
            }))
            .on_action(cx.listener(Self::history_previous))
            .on_action(cx.listener(Self::history_next))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::export_settings))
            .on_action(cx.listener(Self::import_settings))
            .on_action(cx.listener(Self::open_search))
            .when(self.can_disconnect_selected(), |d| {
                d.on_action(cx.listener(Self::disconnect_action))
            })
            .when(self.can_connect_selected(), |d| {
                d.on_action(cx.listener(Self::connect_action))
            })
            .when(self.can_reconnect_selected(), |d| {
                d.on_action(cx.listener(Self::reconnect_action))
            })
            .on_action(cx.listener(Self::toggle_debug))
            .on_action(cx.listener(Self::toggle_toolbar))
            .on_action(cx.listener(Self::toggle_menu_bar))
            .on_action(cx.listener(Self::copy_diagnostics))
            .on_action(cx.listener(|this, _: &Quit, window, cx| this.request_quit(window, cx)))
            // The menu bar greys out an action no window handles, so the chat
            // window handles About itself; `open_about` covers other windows.
            .on_action(cx.listener(|this, _: &About, _, cx| about::open(&this.i18n, cx)))
            .on_action(cx.listener(Self::paste_image))
            .on_action(cx.listener(|this, _: &input::UploadImage, window, cx| {
                this.toolbar_upload(window, cx)
            }))
            // The search bar spans both columns above them.
            .children(search_bar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(left)
                    .child(right_split)
                    .child(right),
            )
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
            .when_some(self.log_menu.as_ref().zip(log_menu), |d, (at, menu)| {
                d.child(snapped_menu(at.position, menu))
            })
            .when_some(member_prompt, |d, prompt| d.child(prompt))
            .when_some(nick_prompts, |d, prompts| d.child(prompts))
            .when_some(key_prompts, |d, prompts| d.child(prompts))
            .when_some(part_prompt, |d, dialog| d.child(dialog))
            .when_some(disconnect_prompt, |d, dialog| d.child(dialog))
            .when_some(paste_prompt, |d, dialog| d.child(dialog))
            .when_some(setup_wizard, |d, dialog| d.child(dialog))
            .when_some(transfer_dialog, |d, dialog| d.child(dialog))
            .when_some(quit_dialog, |d, dialog| d.child(dialog))
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
        let mut dialog = dialog::prompt_list("nick-prompts", origin, window, &theme);
        for (index, prompt) in self.nick_prompts.iter().enumerate() {
            let network = prompt.network;
            let server = self.prompt_server_label(network);
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
                            button_order::row(div().flex().gap_2())
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
    /// The conversation and its position within its server's channels (D009).
    Channel(ConversationId, usize),
}

/// Whether the insertion line of a drop on the channel at `index` of its
/// server goes above the row: the dragged channel comes from below it. Both
/// positions are within the server, as the move itself is. `None` when the
/// dragged channel has no row.
fn drop_line_on_top(rows: &[TreeRow], dragged: ConversationId, index: usize) -> Option<bool> {
    rows.iter().find_map(|row| match *row {
        TreeRow::Channel(id, from) if id == dragged => Some(from > index),
        _ => None,
    })
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
                chat.render_pane(kind, window, cx)
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
            .w(px(TIME_WIDTH))
            .flex_shrink_0()
            .font_family(self.time_font.clone())
            .text_color(self.theme.time)
            .child(time.to_string())
    }
}

/// Width of the time column of a log line.
const TIME_WIDTH: f32 = 42.;

/// What the cached Reiwa mode leads of a channel were derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReiwaShapeKey {
    channel: ConversationId,
    revision: u64,
    mapping: cayenchat_model::names::CaseMapping,
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
        let mut shapes: Vec<u8> = Vec::new();
        let sequences: Vec<u64> = match self.state.selected_channel() {
            Some(channel) => {
                // A Reiwa mode line's height follows the line above it. The
                // leads are derived again only when the lines, the case
                // mapping or the mode changed.
                if self.appearance.reiwa_mode {
                    let mapping = self.state.casemapping(channel.network);
                    let key = ReiwaShapeKey {
                        channel: channel.id,
                        revision: self.state.log_revision(),
                        mapping,
                    };
                    if self.reiwa_shapes.as_ref().map(|(k, _)| k) != Some(&key) {
                        let leads = channel.messages.iter().enumerate().map(|(index, message)| {
                            let above = index.checked_sub(1).map(|above| &channel.messages[above]);
                            reiwa_group::lead(above, message, mapping) as u8
                        });
                        self.reiwa_shapes = Some((key, leads.collect()));
                    }
                    if let Some((_, cached)) = &self.reiwa_shapes {
                        shapes.extend_from_slice(cached);
                    }
                }
                channel.messages.iter().map(|m| m.sequence).collect()
            }
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
            .sync_shaped(prefix, &sequences, &shapes);

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
        let lines = newest_lines(self.state.conversations(), selected, SUB_LOG_LIMIT);
        let rows = sub_rows(lines, self.state.system_messages(), SUB_LOG_LIMIT);
        let sequences: Vec<u64> = rows.iter().map(|(sequence, _)| *sequence).collect();
        self.sub_list.sync(0, &sequences);
        self.sub_rows = rows.into_iter().map(|(_, row)| row).collect();
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
                    let (found, current) = self.search_ranges(&message.text, message.sequence);
                    let source = Selection::Server(network);
                    let selected = self
                        .log_selection
                        .filter(|selection| selection.source == source)
                        .and_then(|selection| selection.range(index, message.text.len()));
                    let text_len = message.text.len();
                    let styled = styled_log_text(
                        "",
                        &message.text,
                        &[],
                        &[],
                        &[],
                        &found,
                        current,
                        selected,
                        &style.theme,
                    );
                    let layout = styled.layout().clone();
                    let probe = (!found.is_empty())
                        .then(|| {
                            self.search_probe_canvas(
                                message.sequence,
                                layout.clone(),
                                found.first().map(|range| range.start),
                            )
                        })
                        .flatten();
                    let down_layout = layout.clone();
                    let text = div()
                        .flex_1()
                        .min_w_0()
                        .cursor(CursorStyle::IBeam)
                        .child(styled)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                let byte = down_layout
                                    .index_for_position(event.position)
                                    .unwrap_or_else(|index| index)
                                    .min(text_len);
                                this.start_log_selection(source, index, byte, window, cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                            let byte = layout
                                .index_for_position(event.position)
                                .unwrap_or_else(|index| index)
                                .min(text_len);
                            this.extend_log_selection(source, index, byte, cx);
                        }));
                    div()
                        .w_full()
                        .flex()
                        .gap_1()
                        .py(px(1.))
                        .when(style.alternate_rows && index % 2 == 1, |d| {
                            d.bg(style.main_alt)
                        })
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.open_log_menu(event.position, None, window, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .child(style.time(message.time))
                        .child(text)
                        .children(probe)
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
        let originals: Vec<_> = compact
            .decoded(&message.text)
            .map(|(range, original)| (range, SharedString::from(original.to_owned())))
            .collect();
        let urls = compact.shown_urls(urls);
        // Every URL shows its full form on hover, shortened or not.
        let full_urls: Vec<_> = urls
            .iter()
            .map(|(range, url)| (range.clone(), SharedString::from(url.clone())))
            .collect();
        let chips: Vec<_> = compact.linked().collect();
        let selected_range = self
            .log_selection
            .filter(|selection| selection.source == Selection::Channel(selected_channel))
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
        // The same speaker right below their own line continues it; decided
        // from the line above as it is now, never stored.
        let lead = if reiwa {
            let above = index
                .checked_sub(1)
                .and_then(|above| channel.messages.get(above));
            reiwa_group::lead(above, message, self.state.casemapping(network))
        } else {
            reiwa_group::Lead::Header
        };
        let prefix = if message.activity || (reiwa && !message.action) {
            String::new()
        } else {
            sender_prefix(message)
        };
        let prefix_len = prefix.len();
        let (found, found_current) = self.search_ranges(&message.text, message.sequence);
        let found: Vec<_> = found
            .into_iter()
            .map(|range| compact.shown_range(range))
            .collect();
        let first_found_start = found.first().map(|range| range.start);
        let styled = styled_log_text(
            &prefix,
            compact.text(),
            &urls,
            &chips,
            &highlights,
            &found,
            found_current,
            selected_range,
            &style.theme,
        );
        // Links, link chips and nick highlights keep their normal wrapping,
        // as do activity lines; the prefix is never analysed.
        let protected: Vec<_> = urls
            .iter()
            .map(|(range, _)| range.clone())
            .chain(chips.iter().cloned())
            .chain(highlights.iter().cloned())
            .collect();
        let styled = match self.phrase_joins.borrow_mut().joins(
            self.auto_phrase && !message.activity,
            prefix_len,
            compact.text(),
            &protected,
        ) {
            Some(joins) => styled.with_wrap_joins(joins),
            None => styled,
        };
        let layout = styled.layout().clone();
        // While a search jump to this line is unfinished, report where its
        // first match is painted (the layout is only valid once painted).
        let probe = self.search_probe_canvas(
            message.sequence,
            layout.clone(),
            first_found_start.map(|start| start + prefix_len),
        );
        let icons = (!chips.is_empty()).then(|| {
            link_icons(
                layout.clone(),
                chips.iter().map(|chip| chip.start + prefix_len).collect(),
                style.theme.panes.log_link,
            )
        });
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
            // The full URL of any link shows on hover; clicking it
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
                                .new(|_| tooltip::CharRefTooltip::new(original, title, copy))
                                .into();
                        }
                        let url = full_urls
                            .iter()
                            .find(|(url, _)| url.start == start)
                            .map(|(_, url)| url.clone())
                            .unwrap_or_default();
                        cx.new(|_| tooltip::UrlTooltip(url)).into()
                    },
                )
                .tooltip_bridge(tooltip::TOOLTIP_OFFSET.0, tooltip::TOOLTIP_OFFSET.1)
                .into_any_element()
        };
        let avatars_on = self.avatars_shown(network);
        let avatar =
            (!message.activity && avatars_on && lead == reiwa_group::Lead::Header).then(|| {
                self.avatar_slot(
                    self.own_avatar_for(network, &message.sender)
                        .unwrap_or_else(|| {
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
        // Only a message line names a speaker to open a conversation with;
        // activity lines (JOIN, MODE, ...) offer the rest of the menu only.
        let speaker = (!message.activity).then(|| message.sender.clone());
        let row = div()
            .w_full()
            .flex()
            .items_start()
            .gap_1()
            .py(px(1.))
            .when(style.alternate_rows && index % 2 == 1, |d| {
                d.bg(style.main_alt)
            })
            .when(
                self.reveal_flash == Some((selected_channel, message.sequence)),
                |d| {
                    d.bg(Rgba {
                        a: 0.35,
                        ..theme.panes.highlight
                    })
                },
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_log_menu(event.position, speaker.as_deref(), window, cx);
                    cx.stop_propagation();
                }),
            );
        // Reiwa mode keeps one lead column left of the text, so it starts at
        // the same x on every line: the avatar, the time of a continuation in
        // another minute, or blank. It is as wide as a time (`StyleCtx::time`)
        // at least, which the avatar alone is not.
        let lead_width = if avatars_on {
            (avatars::SLOT * 2.).max(TIME_WIDTH)
        } else {
            TIME_WIDTH
        };
        let row = if reiwa {
            let column = |width: f32| div().w(px(width)).flex_shrink_0();
            match lead {
                reiwa_group::Lead::Header => row.child(column(lead_width).children(avatar)),
                reiwa_group::Lead::Text => row.child(column(lead_width)),
                reiwa_group::Lead::Time => row.child(
                    column(lead_width)
                        .mt(px(2.))
                        .text_xs()
                        .font_family(style.time_font.clone())
                        .text_color(theme.time)
                        .child(message.time.to_string()),
                ),
            }
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
                .children(icons)
                .children(probe)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        let byte = down_layout
                            .index_for_position(event.position)
                            .unwrap_or_else(|index| index)
                            .saturating_sub(prefix_len)
                            .min(text_len);
                        let byte = down_compact.original(byte);
                        this.start_log_selection(
                            Selection::Channel(selected_channel),
                            index,
                            byte,
                            window,
                            cx,
                        );
                    }),
                )
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                    let position = move_layout.index_for_position(event.position);
                    let raw = position.unwrap_or_else(|index| index);
                    let byte = raw.saturating_sub(prefix_len).min(text_len);
                    this.extend_log_selection(
                        Selection::Channel(selected_channel),
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
        if reiwa && lead != reiwa_group::Lead::Header {
            row.child(body)
        } else if reiwa {
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
            Some(avatar) => self.avatars.lookup(avatar, avatars::PIXELS),
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
        let (id, index) = match self.sub_rows.get(row) {
            Some(&SubRow::Line(id, index)) => (id, index),
            Some(&SubRow::System(index)) => {
                return self.render_system_sub_row(row, index, &style, cx);
            }
            None => return div().into_any_element(),
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
                "{} [{network}] {}",
                display::neutralize_bidi(&conversation.name),
                sender_prefix(message)
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
        let highlights: Vec<_> = message
            .highlights
            .as_slice()
            .iter()
            .map(|range| compact.shown_range(range.clone()))
            .collect();
        // Drawn like the channel log's links, but never clickable.
        let chips: Vec<_> = compact.linked().collect();
        let urls = compact.shown_urls(message.links.as_slice());
        let urls = if style.compact_urls { urls } else { Vec::new() };
        let styled = styled_log_text(
            &prefix,
            compact.text(),
            &urls,
            &chips,
            &highlights,
            &[],
            false,
            None,
            &style.theme,
        );
        let icons = (!chips.is_empty()).then(|| {
            link_icons(
                styled.layout().clone(),
                chips.iter().map(|chip| chip.start + prefix.len()).collect(),
                style.theme.panes.log_link,
            )
        });
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
                    .child(styled)
                    .children(icons),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                // A single click is too easy to hit while reading the combined log.
                if event.click_count() == 2 {
                    this.dispatch(Command::SelectChannel(id), window, cx);
                }
            }))
            .into_any_element()
    }

    /// A system line (CTCP) in the combined log: `[network] text` in the
    /// system message color; a double-click opens the server log.
    fn render_system_sub_row(
        &mut self,
        row: usize,
        index: usize,
        style: &LogStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme::current(cx);
        let Some((network_id, message)) = self.state.system_messages().get(index) else {
            return div().into_any_element();
        };
        let network_id = *network_id;
        let network = self
            .state
            .networks()
            .iter()
            .find(|network| network.id == network_id)
            .map(|network| self.network_label(network))
            .unwrap_or_default();
        let prefix = format!("[{network}] ");
        let styled = styled_log_text(
            &prefix,
            &message.text,
            &[],
            &[],
            &[],
            &[],
            false,
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
                    .text_color(style.event_color)
                    .child(styled),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() == 2 {
                    this.dispatch(Command::SelectServer(network_id), window, cx);
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
        let avatars_shown = self.avatars_shown(network);
        (start..end)
            .map(|index| {
                let member = participants.get(index).cloned().unwrap_or_default();
                let selected = self.member_selection.contains(conversation, &member);
                let channel = channel.name.clone();
                let nickname = member
                    .trim_start_matches(['~', '&', '@', '%', '+'])
                    .to_owned();
                let avatar = avatars_shown.then(|| {
                    self.own_avatar_for(network, &nickname).unwrap_or_else(|| {
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
                            cx.new(|_| tooltip::TextTooltip(full)).into()
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
                            this.log_menu = None;
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
                                        .min((viewport.height - px(220.)).max(px(0.))),
                                ),
                                network,
                                nickname: nickname.clone(),
                                channel: channel.clone(),
                                private,
                                group,
                                ctcp_open: false,
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

/// The user's quit request from any window (the settings window has no
/// handler of its own) or the macOS menu bar: asks first when switched on.
/// The question always opens in the chat window, which is brought to the
/// front and lives until the application quits, so the question cannot
/// vanish with an auxiliary window that was closed before it was answered.
fn request_quit_anywhere(cx: &mut App) {
    // The action may be dispatched inside the settings window's own update,
    // which would then fail to be updated for the save check below; run
    // after the dispatch has finished.
    cx.defer(|cx| request_quit_in_chat_window(cx, true));
}

/// Writes confirmed settings edits before an ordinary quit. Returns whether
/// the quit must not go on: the save failed, or the settings window could not
/// be asked (never treated as saved). On failure the settings window is
/// brought to the front with the form and the error intact.
fn settings_block_quit(cx: &mut App) -> bool {
    let Some(settings) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<SettingsWindow>())
    else {
        return false;
    };
    settings
        .update(cx, |settings, window, cx| {
            let saved = settings.save_before_leaving(cx);
            if !saved {
                window.activate_window();
            }
            saved
        })
        .map_or(true, |saved| !saved)
}

/// The one place where a confirmed, ordinary quit ends the application:
/// asking (menu, shortcut, window close, Dock, the question) only leads here.
/// Quits unless the settings save check blocks it. The system's shutdown and
/// signals call `cx.quit()` themselves and never reach this.
pub(crate) fn quit_app(cx: &mut App) {
    if !settings_block_quit(cx) {
        cx.quit();
    }
}

/// The chat window cannot be updated while it is handling an event, which is
/// the case when the action reached the global handler from the focused
/// fallback prompt. That is not a reason to skip the question, so the request
/// is retried once after the current event; if the window still cannot be
/// updated the request is dropped rather than quitting unasked.
fn request_quit_in_chat_window(cx: &mut App, retry: bool) {
    let chat = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<ChatWindow>());
    let Some(chat) = chat else {
        quit_app(cx);
        return;
    };
    if chat
        .update(cx, |this, window, cx| this.request_quit(window, cx))
        .is_err()
        && retry
    {
        cx.defer(|cx| request_quit_in_chat_window(cx, false));
    }
}

/// Opens the About window in the chat window's language, or raises it.
fn open_about(cx: &mut App) {
    let Some(chat) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<ChatWindow>())
    else {
        return;
    };
    if let Ok(i18n) = chat.read_with(cx, |chat, _| chat.i18n.clone()) {
        about::open(&i18n, cx);
    }
}

/// Access keys for the menus of `app_menus` on Windows and Linux (File,
/// Connection, Edit, View, Help), taken from the locale.
fn menu_access_keys(i18n: &Localizer) -> menu_bar::AccessKeys {
    let key = |id: &str| i18n.text(id).chars().next().map(|c| c.to_ascii_lowercase());
    menu_bar::AccessKeys {
        keys: [
            "menu_file_key",
            "menu_connection_key",
            "menu_edit_key",
            "menu_view_key",
            "menu_help_key",
        ]
        .into_iter()
        .map(key)
        .collect(),
        suffix: i18n.text("menu_access_suffix") == "yes",
    }
}

fn app_menus(
    debug_enabled: bool,
    toolbar_shown: bool,
    menu_bar_hidden: bool,
    i18n: &Localizer,
) -> Vec<Menu> {
    // macOS keeps Settings and Quit in the application menu; Windows and Linux
    // put them in File, with no application-named menu.
    // macOS also puts About first in the application menu; elsewhere it is
    // in the Help menu below.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut app_items = Vec::new();
    #[cfg(target_os = "macos")]
    app_items.extend([
        MenuItem::action(i18n.text("menu_about"), About),
        MenuItem::separator(),
    ]);
    app_items.extend([
        MenuItem::action(i18n.text("menu_settings"), OpenSettings),
        MenuItem::action(i18n.text("menu_export_settings"), ExportSettings),
        MenuItem::action(i18n.text("menu_import_settings"), ImportSettings),
        MenuItem::separator(),
    ]);
    #[cfg(target_os = "macos")]
    app_items.extend([
        MenuItem::os_submenu(i18n.text("menu_services"), SystemMenuType::Services),
        MenuItem::separator(),
    ]);
    let (app_menu_name, quit_label) = if cfg!(target_os = "macos") {
        ("CayenChat".to_owned(), "menu_quit")
    } else {
        (i18n.text("menu_file"), "menu_exit")
    };
    app_items.push(MenuItem::action(i18n.text(quit_label), Quit));
    let mut view_items = Vec::new();
    // The macOS menu bar belongs to the system.
    #[cfg(not(target_os = "macos"))]
    view_items.push(MenuItem::action(
        i18n.text(if menu_bar_hidden {
            "menu_show_menu_bar"
        } else {
            "menu_hide_menu_bar"
        }),
        ToggleMenuBar,
    ));
    #[cfg(target_os = "macos")]
    let _ = menu_bar_hidden;
    view_items.extend([
        MenuItem::action(
            i18n.text(if toolbar_shown {
                "menu_hide_toolbar"
            } else {
                "menu_show_toolbar"
            }),
            ToggleToolbar,
        ),
        MenuItem::separator(),
        MenuItem::action(
            if debug_enabled {
                i18n.text("menu_hide_diagnostics")
            } else {
                i18n.text("menu_show_diagnostics")
            },
            ToggleDebug,
        ),
        MenuItem::action(i18n.text("menu_copy_diagnostics"), CopyDiagnostics),
    ]);
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut menus = vec![
        Menu {
            name: app_menu_name.into(),
            items: app_items,
            menu_type: None,
        },
        Menu {
            name: i18n.text("menu_connection").into(),
            items: vec![
                MenuItem::action(i18n.text("connect"), Connect),
                MenuItem::action(i18n.text("disconnect"), Disconnect),
                MenuItem::action(i18n.text("reconnect"), Reconnect),
            ],
            menu_type: None,
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
            menu_type: None,
        },
        Menu {
            name: i18n.text("menu_view").into(),
            items: view_items,
            menu_type: None,
        },
    ];
    #[cfg(not(target_os = "macos"))]
    menus.push(Menu {
        name: i18n.text("menu_help").into(),
        items: vec![MenuItem::action(i18n.text("menu_about"), About)],
        menu_type: None,
    });
    // Only macOS has a Window menu; AppKit adds the window list, full screen
    // and the like to it, so it holds just Minimize and Zoom.
    #[cfg(target_os = "macos")]
    menus.push(Menu {
        name: i18n.text("menu_window").into(),
        items: vec![
            MenuItem::action(i18n.text("menu_minimize"), MinimizeWindow),
            MenuItem::action(i18n.text("menu_zoom"), ZoomWindow),
        ],
        menu_type: Some(SystemMenuType::Window),
    });
    menus
}

/// Saved key preferences, kept so a desktop key-theme change can rebind
/// without the settings window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ShortcutPrefs {
    channel_modifier: ChannelNumberModifier,
    text_keys: TextKeyTheme,
    /// The NOTICE key in effect: off when it is the send key's own press.
    notice_key: NoticeKey,
    /// Navigation keys the user changed (action id → key).
    overrides: shortcuts::Overrides,
}

impl Global for ShortcutPrefs {}

impl From<&Settings> for ShortcutPrefs {
    fn from(settings: &Settings) -> Self {
        Self {
            channel_modifier: settings.channel_number_modifier,
            text_keys: settings.text_key_theme,
            notice_key: if settings.notice_key.clashes_with(settings.send_key) {
                NoticeKey::Off
            } else {
                settings.notice_key
            },
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
    let emacs = emacs_keys_active(&prefs, cx);
    cx.clear_key_bindings();
    input::bind_keys(emacs, cx);
    cx.bind_keys(themed_bindings(
        prefs.channel_modifier,
        &prefs.overrides,
        emacs,
    ));
    // Bound last, so it wins over Ctrl+Enter's own binding in the draft.
    let notice = match prefs.notice_key {
        NoticeKey::Off => None,
        NoticeKey::CtrlEnter => Some("ctrl-enter"),
        NoticeKey::AltEnter => Some("alt-enter"),
    };
    if let Some(key) = notice {
        cx.bind_keys([KeyBinding::new(
            key,
            SendNotice,
            Some("ChatWindow > TextInput"),
        )]);
    }
    input::bind_multiline_keys(emacs, cx);
}

/// Whether the Emacs text key theme is in effect.
fn emacs_keys_active(prefs: &ShortcutPrefs, cx: &App) -> bool {
    match prefs.text_keys {
        TextKeyTheme::Auto => desktop::current(cx).emacs_keys,
        TextKeyTheme::Standard => false,
        TextKeyTheme::Emacs => true,
    }
}

/// The key that opens the log search, as bound by [`themed_bindings`].
fn find_in_log_key(cx: &App) -> &'static str {
    let emacs = cx
        .try_global::<ShortcutPrefs>()
        .is_some_and(|prefs| emacs_keys_active(prefs, cx));
    if cfg!(target_os = "macos") {
        "cmd-f"
    } else if cfg!(target_os = "linux") && emacs {
        "f3"
    } else {
        "ctrl-f"
    }
}

/// `shortcut_bindings` for the text key theme in effect.
fn themed_bindings(
    channel_modifier: ChannelNumberModifier,
    overrides: &shortcuts::Overrides,
    emacs: bool,
) -> Vec<KeyBinding> {
    let mut bindings = shortcut_bindings(channel_modifier, overrides);
    if cfg!(target_os = "linux") && emacs {
        // Ctrl+F is "cursor right" in the Emacs key theme. A binding without
        // a context outranks the input's, so search opens with F3 instead.
        bindings.retain(|binding| !binding.action().partial_eq(&FindInLog));
        bindings.push(KeyBinding::new("f3", FindInLog, None));
    }
    bindings
}

/// Standard Tab / Shift+Tab movement between settings fields (tab stops).
/// The settings window has its own handlers that also scroll; this is the
/// plain movement the tests exercise.
#[cfg(test)]
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
        // Both reach the same handler; the send key setting decides which of
        // them sends the chat draft (the other inputs take either as "done").
        KeyBinding::new(
            "ctrl-enter",
            SendMessageWithCtrl,
            Some("ChatWindow > TextInput"),
        ),
        KeyBinding::new("up", HistoryPrevious, Some("ChatWindow > TextInput")),
        KeyBinding::new("down", HistoryNext, Some("ChatWindow > TextInput")),
        KeyBinding::new("tab", FocusNextField, Some("SettingsWindow")),
        KeyBinding::new("shift-tab", FocusPreviousField, Some("SettingsWindow")),
        // The leave question inside the chat window: its field and buttons
        // cycle instead of completing nicknames (later wins over the chat
        // field's Tab above).
        KeyBinding::new("tab", FocusNextField, Some("PartDialog")),
        KeyBinding::new("shift-tab", FocusPreviousField, Some("PartDialog")),
        KeyBinding::new("tab", FocusNextField, Some("PartDialog > TextInput")),
        KeyBinding::new(
            "shift-tab",
            FocusPreviousField,
            Some("PartDialog > TextInput"),
        ),
        KeyBinding::new("tab", FocusNextField, Some("PasteDialog")),
        KeyBinding::new("shift-tab", FocusPreviousField, Some("PasteDialog")),
        KeyBinding::new("tab", FocusNextField, Some("PasteDialog > TextInput")),
        KeyBinding::new(
            "shift-tab",
            FocusPreviousField,
            Some("PasteDialog > TextInput"),
        ),
        KeyBinding::new("secondary-,", OpenSettings, None),
        // Replaced by F3 under the Emacs key theme on Linux (see
        // `rebind_shortcuts`).
        KeyBinding::new("secondary-f", FindInLog, None),
        KeyBinding::new("escape", CloseSearch, Some("LogSearch")),
        KeyBinding::new("shift-enter", SearchPrevious, Some("LogSearch")),
        KeyBinding::new("secondary-shift-d", ToggleDebug, None),
        KeyBinding::new("secondary-shift-l", CopyDiagnostics, None),
        KeyBinding::new("secondary-c", CopyLogSelection, Some("MainLog")),
        KeyBinding::new("secondary-q", Quit, None),
    ];
    #[cfg(target_os = "macos")]
    bindings.push(KeyBinding::new("secondary-m", MinimizeWindow, None));

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

/// Whether startup found no saved connection settings: no settings file, or
/// one that was read but holds no server. A file that cannot be read is not a
/// first run, since it may hold what the user set up.
fn is_first_run(loaded: &Result<Option<Settings>, String>) -> bool {
    match loaded {
        Ok(None) => true,
        Ok(Some(settings)) => settings.servers.is_empty(),
        Err(_) => false,
    }
}

/// Loads settings and moves plaintext passwords saved by older versions into
/// the credential store. Returns a notice for the chat window, and whether
/// there was no saved server (the first run, #521); a file that cannot be
/// read is not a first run.
fn load_settings_at_startup() -> (Settings, Option<String>, bool) {
    let loaded = cayenchat_storage::load();
    let first_run = is_first_run(&loaded);
    let mut saved = match loaded {
        Ok(saved) => saved.unwrap_or_default(),
        Err(error) => {
            // The next save replaces the file, so keep what could not be read.
            let error = match cayenchat_storage::keep_unreadable_copy() {
                Ok(copy) => format!("{error}\nA copy was kept at {}.", copy.display()),
                Err(_) => error,
            };
            return (Settings::default(), Some(error), false);
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
    (saved, notice, first_run)
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
    let (saved, notice, first_run) = load_settings_at_startup();
    #[cfg(target_os = "linux")]
    select_linux_display(saved.linux_display);
    #[cfg(target_os = "linux")]
    let signal_quit = signals::install();
    Application::new()
        .with_assets(toolbar::Icons)
        .run(move |cx: &mut App| {
            #[cfg(target_os = "linux")]
            if let Some(receiver) = signal_quit {
                signals::quit_on_signal(receiver, cx);
            }
            secrets::install(saved.credential_backend, cx);
            theme::apply(saved.theme, &saved.appearance, cx);
            desktop::watch(cx);
            apply_shortcuts(ShortcutPrefs::from(&saved), cx);
            cx.set_menus(app_menus(
                false,
                saved.show_toolbar,
                saved.menu_bar_auto_hide,
                &Localizer::new(Language::System),
            ));
            // Reached when no chat window handles it, e.g. from the settings
            // window or the menu bar on macOS.
            cx.on_action(|_: &Quit, cx| request_quit_anywhere(cx));
            // About belongs to no window: the menu bar of any window, or the
            // macOS application menu, reaches it here.
            cx.on_action(|_: &About, cx| open_about(cx));
            // macOS Dock "Quit" is the user's request too; the system's own
            // shutdown and `cx.quit()` never reach this handler.
            cx.on_quit_request(|cx| {
                request_quit_anywhere(cx);
                false
            });
            // Global actions run inside the active window's update, so updating
            // that window again here fails; do it once the dispatch has finished.
            cx.on_action(|_: &MinimizeWindow, cx| {
                cx.defer(|cx| {
                    if let Some(window) = cx.active_window() {
                        let _ = window.update(cx, |_, window, _| window.minimize_window());
                    }
                });
            });
            cx.on_action(|_: &ZoomWindow, cx| {
                cx.defer(|cx| {
                    if let Some(window) = cx.active_window() {
                        let _ = window.update(cx, |_, window, _| window.zoom_window());
                    }
                });
            });
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    quit_app(cx);
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
            #[cfg(target_os = "windows")]
            let always_on_top = saved.always_on_top;
            let chat_window = cx
                .open_window(
                    WindowOptions {
                        app_id: Some(APP_ID.into()),
                        window_bounds: Some(window_bounds),
                        display_id,
                        window_min_size: Some(min_size),
                        titlebar: Some(TitlebarOptions {
                            title: Some("CayenChat".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    move |window, cx| {
                        cx.new(|cx| ChatWindow::with_settings(saved, notice, window, cx))
                    },
                )
                .expect("could not open CayenChat window");
            cx.activate(true);
            let wake::Notices {
                wake: mut wake_ups,
                sleep: mut sleeps,
            } = wake::install();
            cx.spawn(async move |cx| {
                while let Some(notice) = sleeps.next().await {
                    let prepared = chat_window.update(cx, |chat, _, cx| {
                        chat.prepare_for_sleep(cx);
                    });
                    // The system goes on to sleep once this is dropped.
                    drop(notice);
                    if prepared.is_err() {
                        return;
                    }
                }
            })
            .detach();
            cx.spawn(async move |cx| {
                while wake_ups.next().await.is_some() {
                    Timer::after(WAKE_GRACE).await;
                    // Notices that arrived meanwhile belong to this check.
                    while wake_ups.try_recv().is_ok() {}
                    let checked = chat_window.update(cx, |chat, _, cx| {
                        chat.check_connections_after_wake(cx);
                    });
                    if checked.is_err() {
                        return;
                    }
                }
            })
            .detach();
            // Off is the default state, so only an enabled preference touches the
            // native window; the settings window below follows it.
            #[cfg(target_os = "windows")]
            if always_on_top {
                let _ = chat_window.update(cx, |chat, window, cx| {
                    chat.apply_always_on_top(true, window, cx)
                });
            }
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
                    cx.set_menus(app_menus(
                        false,
                        chat.toolbar_shown,
                        !chat.menu_bar.always(),
                        &chat.i18n,
                    ));
                    // What startup reported while the settings were read.
                    chat.open_next_notice(window, cx);
                    let startup = std::mem::take(&mut chat.startup_connections);
                    // With no saved server the setup wizard stands in for
                    // the settings window; it writes nothing unless asked.
                    let wizard = first_run && startup.is_empty();
                    if wizard {
                        chat.open_setup_wizard(cx);
                    }
                    let mut settings_for =
                        (startup.is_empty() && !wizard).then(|| chat.selected_profile_id());
                    for (network, config) in startup {
                        let failed = match config {
                            Ok(config) => {
                                chat.apply_connection(network, config, window, cx).is_err()
                            }
                            Err(error) => {
                                let message =
                                    chat.i18n.format("startup_invalid", &[("error", &error)]);
                                chat.raise_error(Some(network), message, notices::Origin::User, cx);
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
            // a login launch stays out of the user's way. The chat window is
            // fronted even when settings open on top of it (#354), or it would
            // stay behind other apps.
            #[cfg(target_os = "windows")]
            for front in
                autostart::startup_front(autostart::launched_by_registration(), settings_opened)
            {
                match front {
                    autostart::StartupFront::Settings => {
                        let settings = chat_window
                            .update(cx, |chat, _, _| chat.settings_window)
                            .ok()
                            .flatten();
                        if let Some(settings) = settings {
                            let _ = settings.update(cx, |_, window, _| window.activate_window());
                        }
                    }
                    autostart::StartupFront::Chat => {
                        let _ = chat_window.update(cx, |_, window, _| window.activate_window());
                    }
                }
            }
            #[cfg(not(target_os = "windows"))]
            let _ = settings_opened;
        });
}

#[cfg(test)]
#[path = "main_tests/combined_log_tests.rs"]
mod combined_log_tests;

#[cfg(test)]
#[path = "main_tests/link_icon_tests.rs"]
mod link_icon_tests;

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

#[cfg(test)]
#[path = "main_tests/notice_tests.rs"]
mod notice_tests;

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

#[cfg(test)]
#[path = "main_tests/log_menu_tests.rs"]
mod log_menu_tests;

#[cfg(test)]
#[path = "main_tests/menu_tests.rs"]
mod menu_tests;
#[cfg(test)]
#[path = "main_tests/toolbar_tests.rs"]
mod toolbar_tests;

#[cfg(test)]
#[path = "main_tests/wake_tests.rs"]
mod wake_tests;

#[cfg(test)]
#[path = "main_tests/charset_command_tests.rs"]
mod charset_command_tests;

#[cfg(test)]
#[path = "main_tests/help_command_tests.rs"]
mod help_command_tests;

#[cfg(test)]
#[path = "main_tests/search_tests.rs"]
mod search_tests;

#[cfg(test)]
#[path = "main_tests/latest_button_tests.rs"]
mod latest_button_tests;

#[cfg(test)]
#[path = "main_tests/channel_key_tests.rs"]
mod channel_key_tests;

#[cfg(test)]
#[path = "main_tests/key_note_tests.rs"]
mod key_note_tests;

#[cfg(test)]
#[path = "main_tests/join_switch_tests.rs"]
mod join_switch_tests;

#[cfg(test)]
#[path = "main_tests/part_dialog_tests.rs"]
mod part_dialog_tests;

#[cfg(test)]
#[path = "main_tests/dblclick_confirm_tests.rs"]
mod dblclick_confirm_tests;

#[cfg(test)]
#[path = "main_tests/paste_dialog_tests.rs"]
mod paste_dialog_tests;

#[cfg(test)]
#[path = "main_tests/tree_reveal_tests.rs"]
mod tree_reveal_tests;

#[cfg(test)]
#[path = "main_tests/unread_log_position_tests.rs"]
mod unread_log_position_tests;

#[cfg(test)]
#[path = "main_tests/send_key_tests.rs"]
mod send_key_tests;
