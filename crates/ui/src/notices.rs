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
//!   closed, so a failing automatic retry cannot reopen the dialog forever.
//!   What the user triggers is always shown again.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use cayenchat_model::NetworkId;
use cayenchat_model::display;
use gpui::{prelude::*, *};

use crate::ChatWindow;

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

/// How long after the user's own command a 417 or 400 still counts as its
/// answer.
const ANSWER_WINDOW: Duration = Duration::from_secs(10);
/// The most commands remembered per server.
const MAX_SENT: usize = 8;

/// The commands the user sent lately, per server, so that a 417 or 400 can be
/// told from one a background process caused. A reply answers one command and
/// uses it up. A 400 names its command: it answers only a command of that
/// name, so a command whose name is not known (`verbs` empty) is never
/// matched by a 400 and only a 417 can answer it. A reply that matches nothing
/// stays in the log.
#[derive(Default)]
pub(crate) struct SentCommands {
    sent: HashMap<NetworkId, VecDeque<(Instant, Vec<String>)>>,
}

impl SentCommands {
    /// Remembers a command sent to `network` that the server may refuse with
    /// one of `verbs` (upper case IRC command names; empty when unknown).
    pub fn note<S: AsRef<str>>(&mut self, network: NetworkId, verbs: &[S], now: Instant) {
        let queue = self.sent.entry(network).or_default();
        queue.retain(|(at, _)| now.duration_since(*at) < ANSWER_WINDOW);
        if queue.len() >= MAX_SENT {
            queue.pop_front();
        }
        queue.push_back((
            now,
            verbs.iter().map(|verb| verb.as_ref().to_string()).collect(),
        ));
    }

    /// Takes the newest command of `network` that a reply naming `command`
    /// (a 400's) can answer. Returns whether there was one.
    pub fn answer(&mut self, network: NetworkId, command: Option<&str>, now: Instant) -> bool {
        let Some(queue) = self.sent.get_mut(&network) else {
            return false;
        };
        queue.retain(|(at, _)| now.duration_since(*at) < ANSWER_WINDOW);
        let found = queue.iter().rposition(|(_, verbs)| match command {
            Some(command) => verbs.iter().any(|verb| verb.eq_ignore_ascii_case(command)),
            None => true,
        });
        if queue.is_empty() {
            self.sent.remove(&network);
            return false;
        }
        match found {
            Some(index) => {
                queue.remove(index);
                true
            }
            None => false,
        }
    }
}

/// The IRC commands a typed line may be refused as, as `irc-core` sends
/// them: plain text, `/me`, `/msg` and the CTCP commands go out as PRIVMSG,
/// `/notice` as NOTICE, `/raw` and `/quote` send their own first word, and any
/// other command is sent as typed. Empty when there is no word to go by; such
/// a line is answered by a 417 only.
pub(crate) fn typed_verbs(text: &str) -> Vec<String> {
    let Some(rest) = text.strip_prefix('/') else {
        return vec!["PRIVMSG".into(), "NOTICE".into()];
    };
    let mut words = rest.split_whitespace();
    let word = words.next().unwrap_or("").to_ascii_uppercase();
    let verb = match word.as_str() {
        "RAW" | "QUOTE" => words.next().unwrap_or("").to_ascii_uppercase(),
        "ME" | "MSG" | "PRIVMSG" | "CTCP" | "PING" => "PRIVMSG".into(),
        _ => word,
    };
    if verb.is_empty() {
        Vec::new()
    } else {
        vec![verb]
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
    /// failure is reported, a success is remembered for [`Self::server_rejected`].
    pub(crate) fn report_sent(
        &mut self,
        network: NetworkId,
        verbs: &[&str],
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => self.note_user_command(network, verbs),
            Err(error) => self.raise_error(Some(network), error, Origin::User, cx),
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
        let answer = window.prompt(level, title, detail, buttons, cx);
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

    /// Remembers that the user just sent a command to `network` (see
    /// [`SentCommands`]), so a 417 or
    /// 400 that follows can be told from one a background process caused.
    pub(crate) fn note_user_command(&mut self, network: NetworkId, verbs: &[&str]) {
        self.sent_commands.note(network, verbs, Instant::now());
    }

    /// A 417 or 400 reply. It is in the server log already. It opens a dialog
    /// only as the answer to a command the user just sent to this server, and
    /// only once per command; any other reply stays in the log.
    pub(crate) fn server_rejected(
        &mut self,
        network: NetworkId,
        code: u16,
        command: Option<String>,
        reason: String,
        cx: &mut Context<Self>,
    ) {
        if !self
            .sent_commands
            .answer(network, command.as_deref(), Instant::now())
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
    use super::{ANSWER_WINDOW, SentCommands, typed_verbs};
    use cayenchat_model::NetworkId;
    use std::time::Instant;

    #[test]
    fn a_400_for_another_command_does_not_use_up_the_users_command() {
        let (a, now) = (NetworkId(1), Instant::now());
        let mut sent = SentCommands::default();
        sent.note(a, &typed_verbs("hello"), now);
        assert!(!sent.answer(a, Some("CHATHISTORY"), now));
        assert!(sent.answer(a, None, now), "the later 417 still shows");
        assert!(!sent.answer(a, None, now), "one reply per command");
    }

    #[test]
    fn raw_lines_use_their_own_verb() {
        let (a, now) = (NetworkId(1), Instant::now());
        assert_eq!(typed_verbs("/raw privmsg #demo :hi"), ["PRIVMSG"]);
        assert_eq!(typed_verbs("/quote MODE #demo +m"), ["MODE"]);
        assert!(typed_verbs("/raw").is_empty());
        let mut sent = SentCommands::default();
        sent.note(a, &typed_verbs("/raw PRIVMSG #demo :hi"), now);
        assert!(!sent.answer(a, Some("CHATHISTORY"), now));
        assert!(sent.answer(a, None, now), "the later 417 still shows");
    }

    #[test]
    fn verbs_follow_what_irc_core_sends() {
        assert_eq!(typed_verbs("/names #demo"), ["NAMES"]);
        assert_eq!(typed_verbs("/ctcp bob VERSION"), ["PRIVMSG"]);
        assert_eq!(typed_verbs("/ping bob"), ["PRIVMSG"]);
        assert_eq!(typed_verbs("/me waves"), ["PRIVMSG"]);
        assert_eq!(typed_verbs("/notice bob hi"), ["NOTICE"]);
        assert_eq!(typed_verbs("/whois bob"), ["WHOIS"]);
    }

    #[test]
    fn each_server_keeps_its_own_command() {
        let (a, b, now) = (NetworkId(1), NetworkId(2), Instant::now());
        let mut sent = SentCommands::default();
        sent.note(a, &[] as &[&str], now);
        sent.note(b, &[] as &[&str], now);
        assert!(sent.answer(a, None, now));
        assert!(sent.answer(b, None, now));
    }

    #[test]
    fn a_command_of_unknown_name_and_old_ones() {
        let (a, now) = (NetworkId(1), Instant::now());
        let mut sent = SentCommands::default();
        sent.note(a, &typed_verbs("/frobnicate"), now);
        assert!(!sent.answer(a, Some("CHATHISTORY"), now));
        assert!(sent.answer(a, None, now), "a 417 still answers it");
        sent.note(a, &typed_verbs("/raw"), now);
        assert!(!sent.answer(a, Some("PRIVMSG"), now), "no word to go by");
        sent.note(a, &[] as &[&str], now);
        assert!(!sent.answer(a, None, now + ANSWER_WINDOW));
    }
}
