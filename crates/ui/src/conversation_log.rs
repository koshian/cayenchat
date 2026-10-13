// SPDX-License-Identifier: GPL-3.0-only
//! Opt-in conversation logs written to files (`cayenchat_storage::chatlog`):
//! which events become lines, and the settings that switch them on.
//!
//! Only what is sent and received now is logged. Replayed history, the
//! server log, failed sends and CTCP other than ACTION never are.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use cayenchat_irc_core::{
    ChannelActivityKind,
    text::{action_text, strip_formatting},
};
use cayenchat_model::NetworkId;
use cayenchat_storage::{
    ConversationLog,
    chatlog::{ChatLogger, Kind, Notify, Record, Retired, Scope, Status},
};
use gpui::{prelude::*, *};

use crate::{ChatWindow, SettingsWindow, settings_theme};

/// Own messages waiting for the server's echo that are remembered at most.
const PENDING_LIMIT: usize = 64;

/// Stopped loggers that are remembered at most.
const MAX_RETIRED: usize = 8;

/// What the user should be told about the logger, once.
pub enum Notice {
    Failed(String),
    Locked,
}

/// The logger and the settings it was started with.
pub struct ConversationLogs {
    options: ConversationLog,
    directory: Option<PathBuf>,
    logger: Option<ChatLogger>,
    /// Loggers that were stopped and may still be writing; the next one on
    /// the same directory waits (a while) for them before it takes the lock.
    retired: Vec<Retired>,
    /// Wakes the window when a logger fails or is locked out.
    notify: Notify,
    /// Own messages whose echo has not arrived (`echo-message`): they are
    /// logged once the server confirms them, not when it refuses them.
    pending: HashMap<(NetworkId, u64), Record>,
    announced_lock: bool,
}

impl ConversationLogs {
    pub fn new(options: &ConversationLog, notify: Notify) -> Self {
        let mut logs = Self {
            options: ConversationLog::default(),
            directory: None,
            logger: None,
            retired: Vec::new(),
            notify,
            pending: HashMap::new(),
            announced_lock: false,
        };
        logs.apply(options);
        logs
    }

    /// Follows changed settings: starts, stops or moves the logger. Turning
    /// a log off stops the writing at once; files stay. Nothing here waits
    /// for the disk.
    pub fn apply(&mut self, options: &ConversationLog) {
        let directory = options.resolved_directory();
        let wanted = options.enabled() && directory.is_some();
        if !wanted || self.directory != directory {
            self.stop();
        }
        if wanted && self.logger.is_none() {
            self.logger = directory.clone().map(|directory| {
                ChatLogger::start_after(directory, &self.retired, Some(Arc::clone(&self.notify)))
            });
            self.directory = directory;
            self.announced_lock = false;
        }
        self.options = options.clone();
        // A message held for a kind of log that is now off is never written.
        let options = &self.options;
        self.pending.retain(|_, record| match record.scope {
            Scope::Channel => options.channels,
            Scope::Private => options.private,
        });
    }

    /// Ends the writer thread, which writes what is queued first, without
    /// waiting for it.
    pub fn stop(&mut self) {
        if let Some(logger) = self.logger.take() {
            self.retired.retain(|retired| !retired.is_finished());
            // Only the newest few are remembered; a disk that hangs cannot
            // make this list, or the waiting loggers, grow.
            if self.retired.len() >= MAX_RETIRED {
                self.retired.remove(0);
            }
            self.retired.push(logger.retire());
        }
        self.directory = None;
        self.pending.clear();
    }

    /// Stops and waits (at most `timeout`) for what is queued to be written:
    /// for the way out of the application.
    pub fn finish(&mut self, timeout: Duration) {
        self.stop();
        let deadline = std::time::Instant::now() + timeout;
        for retired in &self.retired {
            retired.wait(deadline.saturating_duration_since(std::time::Instant::now()));
        }
    }

    fn wants(&self, scope: Scope) -> bool {
        self.logger.is_some()
            && match scope {
                Scope::Channel => self.options.channels,
                Scope::Private => self.options.private,
            }
    }

    pub fn status(&self) -> Option<Status> {
        self.logger.as_ref().map(ChatLogger::status)
    }

    /// A failure or a lock the user has not been told about.
    pub fn notice(&mut self) -> Option<Notice> {
        let logger = self.logger.as_ref()?;
        if let Some(failure) = logger.take_announcement() {
            return Some(Notice::Failed(failure));
        }
        if logger.status().locked && !self.announced_lock {
            self.announced_lock = true;
            return Some(Notice::Locked);
        }
        None
    }
}

impl ChatWindow {
    /// Follows the saved settings.
    pub(crate) fn apply_conversation_log(&mut self, options: &ConversationLog) {
        self.conversation_logs.apply(options);
    }

    /// Tells every server tab about a logger that failed or is locked out,
    /// once.
    pub(crate) fn announce_conversation_log(&mut self) {
        let Some(notice) = self.conversation_logs.notice() else {
            return;
        };
        let line = match notice {
            Notice::Failed(error) => self
                .i18n
                .format("conversation_log_failed", &[("error", &error)]),
            Notice::Locked => self.i18n.text("conversation_log_locked"),
        };
        let networks: Vec<_> = self.sessions.keys().copied().collect();
        for network in networks {
            self.state.append_server_message(network, line.clone());
        }
    }

    fn log_network_name(&self, network: NetworkId) -> Option<String> {
        let session = self.sessions.get(&network)?;
        network_directory(
            &self.saved.servers,
            self.saved.profile(&session.profile_id)?,
        )
    }

    /// The record for one line, or `None` when that kind of conversation
    /// is not logged. `conversation` is the channel or peer as the server
    /// wrote it; `text` still has IRC formatting.
    #[allow(clippy::too_many_arguments)]
    fn log_record(
        &self,
        network: NetworkId,
        scope: Scope,
        conversation: &str,
        kind: Kind,
        nick: &str,
        text: &str,
        time: Option<SystemTime>,
    ) -> Option<Record> {
        if !self.conversation_logs.wants(scope) {
            return None;
        }
        let conversation = match scope {
            Scope::Channel => self.state.casemapping(network).fold(conversation),
            Scope::Private => cayenchat_irc_core::text::nickname_key(conversation),
        };
        Some(Record {
            time: time.unwrap_or_else(SystemTime::now),
            network: self.log_network_name(network)?,
            scope,
            conversation,
            kind,
            nick: nick.to_owned(),
            body: strip_formatting(text),
        })
    }

    /// Writes `record` if its kind of log is on now, whenever it was made.
    fn write_log(&self, record: Option<Record>) {
        if let Some(record) = record
            && self.conversation_logs.wants(record.scope)
            && let Some(logger) = &self.conversation_logs.logger
        {
            logger.log(record);
        }
    }

    /// A line of activity (join, part, nick change…) in a conversation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn log_event(
        &self,
        network: NetworkId,
        scope: Scope,
        conversation: &str,
        kind: Kind,
        nick: &str,
        text: &str,
        time: Option<SystemTime>,
    ) {
        let record = self.log_record(network, scope, conversation, kind, nick, text, time);
        self.write_log(record);
    }

    /// Someone joined, left, was kicked… in a channel we have. Modes and
    /// refused joins are not logged.
    pub(crate) fn log_channel_activity(
        &self,
        network: NetworkId,
        channel: &str,
        actor: &str,
        kind: &ChannelActivityKind,
        time: Option<SystemTime>,
    ) {
        if !self.conversation_logs.wants(Scope::Channel)
            || self.state.channel_id(network, channel).is_none()
        {
            return;
        }
        let detail = |reason: &Option<String>| reason.clone().unwrap_or_default();
        let (kind, nick, body) = match kind {
            ChannelActivityKind::Joined { .. } => (Kind::Join, actor, String::new()),
            ChannelActivityKind::Left { reason } => (Kind::Part, actor, detail(reason)),
            ChannelActivityKind::Quit { reason } => (Kind::Quit, actor, detail(reason)),
            ChannelActivityKind::NickChanged { to } => (Kind::Nick, actor, to.clone()),
            ChannelActivityKind::Kicked { target, reason, .. } => {
                let body = match reason.as_deref().filter(|reason| !reason.is_empty()) {
                    Some(reason) => format!("{target} {reason}"),
                    None => target.clone(),
                };
                (Kind::Kick, actor, body)
            }
            ChannelActivityKind::ModeChanged { .. } | ChannelActivityKind::JoinFailed { .. } => {
                return;
            }
        };
        self.log_event(network, Scope::Channel, channel, kind, nick, &body, time);
    }

    /// The record for a message, an ACTION or a NOTICE. CTCP other than
    /// ACTION is not a message.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn message_record(
        &self,
        network: NetworkId,
        scope: Scope,
        conversation: &str,
        sender: &str,
        text: &str,
        notice: bool,
        time: Option<SystemTime>,
    ) -> Option<Record> {
        let (kind, body) = match action_text(text) {
            Some(action) => (Kind::Action, action),
            None if text.starts_with('\u{1}') => return None,
            None if notice => (Kind::Notice, text),
            None => (Kind::Msg, text),
        };
        self.log_record(network, scope, conversation, kind, sender, body, time)
    }

    /// The record for a message we sent. `text` is what the conversation
    /// shows; `action` is the text of a CTCP ACTION, which `text` only shows
    /// as `* …`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn own_message_record(
        &self,
        network: NetworkId,
        scope: Scope,
        conversation: &str,
        own: &str,
        text: &str,
        notice: bool,
        action: Option<&str>,
    ) -> Option<Record> {
        let (kind, body) = match action {
            Some(action) => (Kind::Action, action),
            None if notice => (Kind::Notice, text),
            None => (Kind::Msg, text),
        };
        self.log_record(network, scope, conversation, kind, own, body, None)
    }

    /// A message received (or relayed) just now.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn log_message(
        &self,
        network: NetworkId,
        scope: Scope,
        conversation: &str,
        sender: &str,
        text: &str,
        notice: bool,
        time: Option<SystemTime>,
    ) {
        let record = self.message_record(network, scope, conversation, sender, text, notice, time);
        self.write_log(record);
    }

    /// Our own message that the server will echo: logged when it confirms.
    pub(crate) fn hold_own_message(&mut self, network: NetworkId, local_id: u64, record: Record) {
        let pending = &mut self.conversation_logs.pending;
        if pending.len() < PENDING_LIMIT {
            pending.insert((network, local_id), record);
        }
    }

    /// The echo came: the held message is logged with the server's time and
    /// text.
    pub(crate) fn release_own_message(
        &mut self,
        network: NetworkId,
        local_id: u64,
        text: Option<&str>,
        action: Option<&str>,
        server_time: Option<SystemTime>,
    ) {
        if let Some(mut record) = self.conversation_logs.pending.remove(&(network, local_id)) {
            if let Some(time) = server_time {
                record.time = time;
            }
            // The server may have changed the text; an ACTION's own text
            // comes apart from the `* …` the conversation shows.
            let text = if record.kind == Kind::Action {
                action
            } else {
                text
            };
            if let Some(text) = text {
                record.body = strip_formatting(text);
            }
            self.write_log(Some(record));
        }
    }

    /// The server refused the message, or the connection ended first.
    pub(crate) fn drop_own_message(&mut self, network: NetworkId, local_id: u64) {
        self.conversation_logs.pending.remove(&(network, local_id));
    }

    pub(crate) fn drop_own_messages(&mut self, network: NetworkId) {
        self.conversation_logs
            .pending
            .retain(|(owner, _), _| *owner != network);
    }

    /// Our own message that needs no echo, written straight away.
    pub(crate) fn write_own_message(&self, record: Option<Record>) {
        self.write_log(record);
    }
}

/// The server's name in the log's path: the display name, else the host.
/// Servers that share it are told apart by the end of their profile ID
/// (generated IDs share their `custom-` start).
fn network_directory(
    servers: &[cayenchat_storage::ServerProfile],
    profile: &cayenchat_storage::ServerProfile,
) -> Option<String> {
    let label = |profile: &cayenchat_storage::ServerProfile| {
        if profile.display_name.is_empty() {
            profile.host.clone()
        } else {
            profile.display_name.clone()
        }
    };
    let name = label(profile);
    let shared = servers
        .iter()
        .any(|other| other.id != profile.id && label(other) == name);
    Some(if shared {
        let mut short: Vec<char> = profile.id.chars().rev().take(8).collect();
        short.reverse();
        let short: String = short.into_iter().collect();
        format!("{name} ({short})")
    } else {
        name
    })
}

impl SettingsWindow {
    fn choose_log_directory(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.i18n.text("conversation_log_choose_prompt").into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.settings.values.conversation_log.directory = Some(path);
                this.autosave_now(None, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn open_log_directory(&mut self, cx: &mut Context<Self>) {
        let Some(directory) = self.settings.values.conversation_log.resolved_directory() else {
            return;
        };
        // The folder is made on demand so that there is something to open.
        // The disk (a network share, say) may be slow, so not on this thread.
        let creating = cx.background_spawn({
            let directory = directory.clone();
            async move { std::fs::create_dir_all(&directory) }
        });
        cx.spawn(async move |this, cx| {
            let result = creating.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => cx.reveal_path(&directory),
                Err(error) => {
                    this.feedback = Some(
                        this.i18n
                            .format("conversation_log_failed", &[("error", &error.to_string())]),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn log_toggle(
        &self,
        id: &'static str,
        label: &'static str,
        checked: bool,
        change: fn(&mut ConversationLog),
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .ml(px(158.))
            .flex()
            .gap_2()
            .cursor_pointer()
            .child(settings_theme::checkbox(checked, true, cx))
            .child(self.i18n.text(label))
            .on_click(cx.listener(move |this, _, _, cx| {
                change(&mut this.settings.values.conversation_log);
                cx.notify();
            }))
    }

    /// The conversation log options of the Application page.
    pub(crate) fn render_conversation_log_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let options = self.settings.values.conversation_log.clone();
        let directory = options
            .resolved_directory()
            .map(|directory| directory.display().to_string())
            .unwrap_or_else(|| self.i18n.text("conversation_log_no_directory"));
        let status = self
            .owner
            .read(cx)
            .ok()
            .and_then(|chat| chat.conversation_logs.status());
        self.log_status_shown = status.clone();
        let status = status.and_then(|status| {
            if let Some(error) = status.failure {
                Some(
                    self.i18n
                        .format("conversation_log_failed", &[("error", &error)]),
                )
            } else if status.locked {
                Some(self.i18n.text("conversation_log_locked"))
            } else if status.dropped > 0 {
                Some(self.i18n.format(
                    "conversation_log_dropped",
                    &[("count", &status.dropped.to_string())],
                ))
            } else {
                None
            }
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .ml(px(158.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("conversation_log")),
            )
            .child(self.log_toggle(
                "conversation-log-channels",
                "conversation_log_channels",
                options.channels,
                |log| log.channels = !log.channels,
                cx,
            ))
            .child(self.log_toggle(
                "conversation-log-private",
                "conversation_log_private",
                options.private,
                |log| log.private = !log.private,
                cx,
            ))
            .child(
                div()
                    .ml(px(158.))
                    .text_color(theme.text_secondary)
                    .child(self.i18n.text("conversation_log_hint")),
            )
            .child(
                div()
                    .ml(px(158.))
                    .flex()
                    .flex_col()
                    .child(self.i18n.text("conversation_log_directory"))
                    .child(div().id("conversation-log-directory").child(directory)),
            )
            .child(
                div()
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .child(
                        settings_theme::button("open-conversation-log", false, cx)
                            .debug_selector(|| "open-conversation-log".into())
                            .child(self.i18n.text("conversation_log_open"))
                            .on_click(cx.listener(|this, _, _, cx| this.open_log_directory(cx))),
                    )
                    .child(
                        settings_theme::button("choose-conversation-log", false, cx)
                            .debug_selector(|| "choose-conversation-log".into())
                            .child(self.i18n.text("conversation_log_choose"))
                            .on_click(cx.listener(|this, _, _, cx| this.choose_log_directory(cx))),
                    ),
            )
            .when_some(status, |d, status| {
                d.child(
                    div()
                        .id("conversation-log-status")
                        .ml(px(158.))
                        .text_color(theme.warning)
                        .child(status),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use cayenchat_storage::Settings;

    use super::network_directory;

    #[test]
    fn servers_added_with_the_same_name_get_different_directories() {
        let mut settings = Settings::default();
        for _ in 0..2 {
            settings.add_server("irc.example.net").display_name = "SameNetwork".into();
        }
        let names: Vec<_> = settings
            .servers
            .iter()
            .filter_map(|profile| network_directory(&settings.servers, profile))
            .collect();
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1], "{names:?}");
    }
}
