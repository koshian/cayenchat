// SPDX-License-Identifier: GPL-3.0-only
//! Errors and warnings of the chat window (#528), shown one at a time in a
//! dialog that the user closes. They used to be a line in the draft row,
//! where a long one took the room of the text field.
//!
//! This is the queue behind the dialog. It only decides what is shown and
//! when; `ChatWindow` opens the dialog from an event (never while drawing).
//!
//! - A notice equal to the open one or to one already waiting is dropped, so
//!   the same failure repeated while the dialog is open stays one dialog.
//! - Different notices wait and are shown one after another, at most
//!   `MAX_PENDING`; further ones are dropped and logged.
//! - A notice a background process raised (not an answer to something the
//!   user just did) is also dropped for `COOLDOWN` after the same notice was
//!   closed, so a repeated refusal cannot reopen the dialog forever.
//!   What the user triggers is always shown again.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use cayenchat_model::NetworkId;
use cayenchat_model::display;
use gpui::{prelude::*, *};

use crate::{ChatWindow, os_text::WindowOsText};

const MAX_PENDING: usize = 8;
const COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub title: String,
    pub detail: String,
}

/// Why a notice was raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The result of something the user just did.
    User,
    /// Something that happened by itself, such as a connection ending.
    Background,
}

#[derive(Default)]
pub(crate) struct Notices {
    open: Option<Notice>,
    pending: VecDeque<Notice>,
    closed: Vec<(Notice, Instant)>,
}

impl Notices {
    /// Queues `notice`. Returns whether it was kept.
    pub fn post(&mut self, notice: Notice, origin: Origin, now: Instant) -> bool {
        self.closed
            .retain(|(_, at)| now.duration_since(*at) < COOLDOWN);
        if self.open.as_ref() == Some(&notice) || self.pending.contains(&notice) {
            return false;
        }
        if origin == Origin::Background && self.closed.iter().any(|(seen, _)| *seen == notice) {
            return false;
        }
        if self.pending.len() >= MAX_PENDING {
            log::warn!("Dropped a notice, too many are waiting: {}", notice.detail);
            return false;
        }
        self.pending.push_back(notice);
        true
    }

    /// The next notice to open, when none is open.
    pub fn take_next(&mut self) -> Option<Notice> {
        if self.open.is_some() {
            return None;
        }
        self.open = self.pending.pop_front();
        self.open.clone()
    }

    /// The open notice was closed.
    pub fn close(&mut self, now: Instant) {
        if let Some(notice) = self.open.take() {
            self.closed.push((notice, now));
        }
    }

    /// The open notice and the waiting ones, oldest first.
    #[cfg(test)]
    pub fn all(&self) -> Vec<&Notice> {
        self.open.iter().chain(&self.pending).collect()
    }
}

/// How long after a message the user sent a 417 or 400 still counts as its
/// answer.
const ANSWER_WINDOW: Duration = Duration::from_secs(10);
/// The most sends remembered per server.
const MAX_SENT: usize = 8;

/// When the user lately spoke on each server (a message or `/me` from the
/// draft), so that a 417 or 400 can be told from one a background process
/// caused. A reply answers one send and uses it up; a reply with nothing to
/// answer stays in the log.
#[derive(Default)]
pub(crate) struct SentMessages {
    sent: HashMap<NetworkId, VecDeque<Instant>>,
}

impl SentMessages {
    /// Remembers that a message was sent to `network` at `now`.
    pub fn note(&mut self, network: NetworkId, now: Instant) {
        let queue = self.sent.entry(network).or_default();
        queue.retain(|at| now.duration_since(*at) < ANSWER_WINDOW);
        if queue.len() >= MAX_SENT {
            queue.pop_front();
        }
        queue.push_back(now);
    }

    /// Uses up the newest send of `network` that a reply can still answer.
    /// A 400 answers only when it names PRIVMSG or NOTICE (one naming no
    /// command or another one stays in the log); a 417 answers any send.
    /// Returns whether there was one.
    pub fn answer(
        &mut self,
        network: NetworkId,
        code: u16,
        command: Option<&str>,
        now: Instant,
    ) -> bool {
        if code != 417
            && !command.is_some_and(|command| {
                command.eq_ignore_ascii_case("PRIVMSG") || command.eq_ignore_ascii_case("NOTICE")
            })
        {
            return false;
        }
        let Some(queue) = self.sent.get_mut(&network) else {
            return false;
        };
        queue.retain(|at| now.duration_since(*at) < ANSWER_WINDOW);
        let answered = queue.pop_back().is_some();
        if queue.is_empty() {
            self.sent.remove(&network);
        }
        answered
    }
}

/// Whether a draft goes out as something the user said: plain text or `/me`.
pub(crate) fn is_spoken(text: &str) -> bool {
    match text.strip_prefix('/') {
        None => true,
        Some(rest) => rest
            .split_whitespace()
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("me")),
    }
}

/// The longest server text shown in a dialog.
const MAX_REASON_CHARS: usize = 400;

impl ChatWindow {
    /// Reports the error of something the user did, if there is one.
    pub(crate) fn report(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(error) = error {
            self.raise_error(None, error, Origin::User, cx);
        }
    }

    /// The outcome of sending a command the user asked for to `network`: a
    /// failure is reported.
    pub(crate) fn report_sent(
        &mut self,
        network: NetworkId,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = result {
            self.raise_error(Some(network), error, Origin::User, cx);
        }
    }

    /// Reports `detail` in a dialog, naming `network` when the failure
    /// belongs to one server.
    pub(crate) fn raise_error(
        &mut self,
        network: Option<NetworkId>,
        detail: String,
        origin: Origin,
        cx: &mut Context<Self>,
    ) {
        let detail = match network.map(|network| self.prompt_server_label(network)) {
            Some(server) if !server.is_empty() => format!("{server}\n{detail}"),
            _ => detail,
        };
        let title = self.i18n.text("notice_title");
        self.post_notice(Notice { title, detail }, origin, cx);
    }

    /// Queues a notice and opens it from an event of its own, so nothing
    /// depends on a window being at hand or on drawing.
    pub(crate) fn post_notice(&mut self, notice: Notice, origin: Origin, cx: &mut Context<Self>) {
        if !self.notices.post(notice, origin, Instant::now()) {
            return;
        }
        let Some(handle) = self.window_handle else {
            return;
        };
        cx.spawn(async move |_, cx| {
            let _ = handle.update(cx, |chat, window, cx| chat.open_next_notice(window, cx));
        })
        .detach();
    }

    /// Opens the next waiting notice unless a dialog is already open: that
    /// one is not replaced, and this runs again when it closes.
    pub(crate) fn open_next_notice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt_open {
            return;
        }
        let Some(notice) = self.notices.take_next() else {
            return;
        };
        drop(self.chat_prompt(
            PromptLevel::Warning,
            &notice.title,
            Some(&notice.detail),
            &[PromptButton::cancel(self.i18n.text("upload_failed_close"))],
            window,
            cx,
        ));
    }

    /// Shows a dialog of the chat window. Only one is open at a time; the
    /// notice queue waits for it and takes the keyboard focus back to the
    /// draft when it closes with nothing else focused. The answer is
    /// forwarded as `Window::prompt` gives it.
    pub(crate) fn chat_prompt(
        &mut self,
        level: PromptLevel,
        title: &str,
        detail: Option<&str>,
        buttons: &[PromptButton],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> futures_channel::oneshot::Receiver<usize> {
        self.prompt_open = true;
        let answer = window.os_prompt(level, title, detail, buttons, cx);
        let (sender, receiver) = futures_channel::oneshot::channel();
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(chosen) = answer.await {
                let _ = sender.send(chosen);
            } else {
                drop(sender);
            }
            let _ = this.update_in(cx, |this, window, cx| this.prompt_closed(window, cx));
        })
        .detach();
        receiver
    }

    fn prompt_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_open = false;
        self.notices.close(Instant::now());
        // Whatever the user was working in keeps the focus; only a window
        // left with none returns to the draft of the selected conversation.
        if window.focused(cx).is_none()
            && let Some(input) = self.inputs.get(&self.state.selection())
        {
            window.focus(&input.focus_handle(cx));
        }
        self.open_next_notice(window, cx);
        cx.notify();
    }

    /// Remembers that the user just said something on `network` from the
    /// draft (see [`SentMessages`]), so a 417 or 400 that follows can be told
    /// from one a background process caused.
    pub(crate) fn note_spoken(&mut self, network: NetworkId) {
        self.sent_messages.note(network, Instant::now());
    }

    /// A 417 or 400 reply. It is in the server log already. It opens a dialog
    /// only as the answer to something the user just said from the draft on
    /// this server (a 400 only when it names PRIVMSG or NOTICE), and only once
    /// per send; any other reply stays in the log.
    pub(crate) fn server_rejected(
        &mut self,
        network: NetworkId,
        code: u16,
        command: Option<String>,
        reason: String,
        cx: &mut Context<Self>,
    ) {
        if !self
            .sent_messages
            .answer(network, code, command.as_deref(), Instant::now())
        {
            return;
        }
        let reason = cayenchat_irc_core::text::strip_formatting(&reason);
        let reason: String = display::neutralize_bidi(&reason)
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_REASON_CHARS)
            .collect();
        let reply = match command {
            Some(command) => format!("{code} {}", command_text(&command)),
            None => code.to_string(),
        };
        let detail = self.i18n.format(
            "server_rejected_detail",
            &[
                ("server", &self.prompt_server_label(network)),
                ("reply", &reply),
                ("reason", &reason),
            ],
        );
        let title = self.i18n.text("server_rejected_title");
        self.post_notice(Notice { title, detail }, Origin::User, cx);
    }
}

/// A command name from a server, safe to show.
fn command_text(command: &str) -> String {
    display::neutralize_bidi(command)
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{COOLDOWN, MAX_PENDING, Notice, Notices, Origin};
    use std::time::Instant;

    fn notice(detail: &str) -> Notice {
        Notice {
            title: "t".into(),
            detail: detail.into(),
        }
    }

    #[test]
    fn a_repeat_while_open_or_waiting_is_one_dialog() {
        let now = Instant::now();
        let mut notices = Notices::default();
        assert!(notices.post(notice("a"), Origin::User, now));
        assert!(!notices.post(notice("a"), Origin::User, now));
        assert_eq!(notices.take_next(), Some(notice("a")));
        assert!(!notices.post(notice("a"), Origin::User, now));
        assert!(notices.post(notice("b"), Origin::User, now));
        assert_eq!(notices.take_next(), None, "one dialog at a time");
        notices.close(now);
        assert_eq!(notices.take_next(), Some(notice("b")));
    }

    #[test]
    fn what_the_user_triggers_is_shown_again_but_background_repeats_wait() {
        let now = Instant::now();
        let mut notices = Notices::default();
        notices.post(notice("a"), Origin::Background, now);
        notices.take_next();
        notices.close(now);
        assert!(!notices.post(notice("a"), Origin::Background, now));
        assert!(notices.post(notice("a"), Origin::User, now));
        notices.take_next();
        notices.close(now);
        assert!(notices.post(notice("a"), Origin::Background, now + COOLDOWN));
    }

    #[test]
    fn the_queue_is_bounded() {
        let now = Instant::now();
        let mut notices = Notices::default();
        for index in 0..MAX_PENDING {
            assert!(notices.post(notice(&index.to_string()), Origin::User, now));
        }
        assert!(!notices.post(notice("more"), Origin::User, now));
    }
}

#[cfg(test)]
mod sent_tests {
    use super::{ANSWER_WINDOW, SentMessages, is_spoken};
    use cayenchat_model::NetworkId;
    use std::time::Instant;

    #[test]
    fn a_400_for_another_command_does_not_use_up_the_message() {
        let (a, now) = (NetworkId(1), Instant::now());
        let mut sent = SentMessages::default();
        sent.note(a, now);
        assert!(!sent.answer(a, 400, Some("CHATHISTORY"), now));
        assert!(!sent.answer(a, 400, Some("JOIN"), now));
        assert!(!sent.answer(a, 400, None, now), "no command named");
        assert!(sent.answer(a, 400, Some("privmsg"), now));
        assert!(!sent.answer(a, 417, None, now), "one reply per send");
        sent.note(a, now);
        assert!(sent.answer(a, 400, Some("NOTICE"), now));
        sent.note(a, now);
        assert!(sent.answer(a, 417, None, now), "a 417 answers any send");
    }

    #[test]
    fn only_messages_and_me_are_spoken() {
        assert!(is_spoken("hello"));
        assert!(is_spoken("/me waves"));
        assert!(is_spoken("/ME"));
        for text in [
            "/msg bob hi",
            "/notice bob hi",
            "/join #a",
            "/part",
            "/names #a",
            "/ctcp bob VERSION",
            "/raw PRIVMSG #a :hi",
            "/mean",
            "/",
        ] {
            assert!(!is_spoken(text), "{text}");
        }
    }

    #[test]
    fn each_server_keeps_its_own_sends_and_they_expire() {
        let (a, b, now) = (NetworkId(1), NetworkId(2), Instant::now());
        let mut sent = SentMessages::default();
        sent.note(a, now);
        assert!(!sent.answer(b, 417, None, now));
        assert!(sent.answer(a, 417, None, now));
        sent.note(b, now);
        assert!(!sent.answer(b, 417, None, now + ANSWER_WINDOW));
    }
}

#[cfg(test)]
mod disconnect_tests {
    use cayenchat_irc_core::Event;
    use cayenchat_model::NetworkId;
    use gpui::TestAppContext;

    use crate::{ChatWindow, Worker};

    /// A window with one registered server selected; `ended` is the event
    /// that ends its connection. Returns how many notices are waiting.
    fn notices_after(ended: Event, cx: &mut TestAppContext) -> usize {
        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        chat.update(cx, |chat, cx| {
            let events = vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                ended,
            ];
            chat.handle_events(NetworkId(1), events, Worker::Running, cx);
        });
        cx.run_until_parked();
        chat.update(cx, |chat, _| chat.notices.all().len())
    }

    #[gpui::test]
    fn a_disconnect_the_user_asked_for_raises_no_notice(cx: &mut TestAppContext) {
        assert_eq!(
            notices_after(Event::Closed("Disconnected by user.".into()), cx),
            0
        );
    }

    #[gpui::test]
    fn a_lost_connection_raises_no_notice(cx: &mut TestAppContext) {
        assert_eq!(
            notices_after(Event::Disconnected("Connection reset".into()), cx),
            0
        );
    }

    /// Notices raised after a reconnect attempt that fails to start, and
    /// whether a retry was scheduled.
    fn notices_after_failed_reconnect(announce: bool, cx: &mut TestAppContext) -> (usize, bool) {
        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        chat.update(cx, |chat, cx| {
            // Port 0 makes `Connection::connect` fail at validation.
            let mut config =
                cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec![]);
            config.port = 0;
            chat.sessions.get_mut(&NetworkId(1)).unwrap().active_config = Some(config);
            chat.start_reconnect(NetworkId(1), announce, cx);
        });
        cx.run_until_parked();
        chat.update(cx, |chat, _| {
            (
                chat.notices.all().len(),
                chat.sessions[&NetworkId(1)].retry_pending,
            )
        })
    }

    #[gpui::test]
    fn a_failed_automatic_retry_raises_no_notice_and_retries_again(cx: &mut TestAppContext) {
        assert_eq!(notices_after_failed_reconnect(false, cx), (0, true));
    }

    #[gpui::test]
    fn a_failed_reconnect_the_user_asked_for_raises_a_notice(cx: &mut TestAppContext) {
        assert_eq!(notices_after_failed_reconnect(true, cx), (1, true));
    }

    #[gpui::test]
    fn a_failed_nickname_retry_after_the_link_closed_raises_a_notice(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let (chat, cx) =
            cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
        let result = chat.update(cx, |chat, cx| {
            let config =
                cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec![]);
            chat.sessions.get_mut(&NetworkId(1)).unwrap().active_config = Some(config);
            // A nickname with a space fails validation when reconnecting.
            chat.retry_nickname(NetworkId(1), "bad nick".into(), cx)
        });
        cx.run_until_parked();
        assert_eq!(result, Ok(()));
        assert_eq!(chat.update(cx, |chat, _| chat.notices.all().len()), 1);
    }

    #[gpui::test]
    fn a_refused_connection_still_raises_a_notice(cx: &mut TestAppContext) {
        assert_eq!(notices_after(Event::Refused("SASL failed".into()), cx), 1);
    }
}
