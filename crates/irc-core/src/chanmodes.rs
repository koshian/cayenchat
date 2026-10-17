// SPDX-License-Identifier: MPL-2.0
//! The modes set on each joined channel, for display. Only the mode letters
//! are kept: arguments (a key, a limit) are never read, so they cannot reach
//! the UI whatever the server's `CHANMODES` says.

use std::collections::{HashMap, HashSet, VecDeque, hash_map::Entry};
use std::time::Duration;

use tokio::time::Instant;

use irc::proto::mode::Mode;
use irc::proto::{Command, Message, Response};

use crate::{CaseMapping, valid_channel};

/// Most letters kept for one channel; a server cannot grow the state.
const MAX_FLAGS: usize = 64;
/// Most channels tracked, and so most queries waiting; a channel beyond it
/// keeps unknown modes (no title prefix) instead of growing the state.
const MAX_TRACKED: usize = 512;
/// Most `MODE` queries sent and unanswered at once, and how long one is
/// waited for; a channel that never answers keeps unknown modes.
const MAX_IN_FLIGHT: usize = 4;
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// List modes (type A) when the server announces no `CHANMODES`, and the
/// membership modes when it announces no `PREFIX`.
const DEFAULT_LIST_MODES: &str = "beI";
const DEFAULT_PREFIX_MODES: &str = "ov";

pub(crate) struct ChannelModes {
    list_modes: String,
    prefix_modes: String,
    /// The letters that are not channel flags: they take a list entry or a
    /// member as argument.
    not_flags: HashSet<char>,
    /// The channels we are in, by folded name: the name as the server wrote
    /// it (to fold again when `CASEMAPPING` changes) and the flags, `None`
    /// until the `324`.
    channels: HashMap<String, (String, Option<Vec<char>>)>,
    /// Channels whose modes are still to be asked, oldest first.
    pending: VecDeque<String>,
    /// Queries sent and not yet answered, with the time they give up. At
    /// most `MAX_IN_FLIGHT`: the next one goes out on a `324` or a timeout,
    /// whatever else the server sends.
    in_flight: Vec<(String, Instant)>,
    /// The limit was reported once.
    warned: bool,
}

impl Default for ChannelModes {
    fn default() -> Self {
        Self {
            list_modes: DEFAULT_LIST_MODES.into(),
            prefix_modes: DEFAULT_PREFIX_MODES.into(),
            not_flags: DEFAULT_LIST_MODES
                .chars()
                .chain(DEFAULT_PREFIX_MODES.chars())
                .collect(),
            channels: HashMap::new(),
            pending: VecDeque::new(),
            in_flight: Vec::new(),
            warned: false,
        }
    }
}

impl ChannelModes {
    /// Learns `CHANMODES` and `PREFIX` from an `RPL_ISUPPORT` line.
    pub(crate) fn isupport(&mut self, message: &Message) {
        let Command::Response(Response::RPL_ISUPPORT, args) = &message.command else {
            return;
        };
        let mut changed = false;
        for token in args.iter().skip(1).flat_map(|arg| arg.split_whitespace()) {
            if token.eq_ignore_ascii_case("-CHANMODES") {
                self.list_modes = DEFAULT_LIST_MODES.into();
                changed = true;
            } else if token.eq_ignore_ascii_case("-PREFIX") {
                self.prefix_modes = DEFAULT_PREFIX_MODES.into();
                changed = true;
            } else if let Some((name, value)) = token.split_once('=') {
                if name.eq_ignore_ascii_case("CHANMODES") {
                    self.list_modes = value.split(',').next().unwrap_or("").to_owned();
                    changed = true;
                } else if name.eq_ignore_ascii_case("PREFIX") {
                    // `(ov)@+`
                    self.prefix_modes = value
                        .strip_prefix('(')
                        .and_then(|rest| rest.split_once(')'))
                        .map(|(letters, _)| letters.to_owned())
                        .unwrap_or_default();
                    changed = true;
                }
            }
        }
        if changed {
            self.not_flags = self
                .list_modes
                .chars()
                .chain(self.prefix_modes.chars())
                .collect();
        }
    }

    /// The channel's flags after `message`, when it is a `324` or a `MODE`
    /// of a channel we are in. A `MODE` before the `324` is not applied:
    /// its changes have no baseline.
    pub(crate) fn observe(
        &mut self,
        message: &Message,
        mapping: CaseMapping,
        replayed: bool,
    ) -> Option<(String, String)> {
        if replayed {
            return None;
        }
        match &message.command {
            Command::Response(Response::RPL_CHANNELMODEIS, args) => {
                let channel = args.get(1).filter(|name| valid_channel(name))?;
                let key = mapping.fold(channel);
                self.in_flight
                    .retain(|(queried, _)| mapping.fold(queried) != key);
                if !self.channels.contains_key(&key) {
                    return None;
                }
                let mut flags = Vec::new();
                let letters = args.get(2).map(String::as_str).unwrap_or("");
                self.apply(&mut flags, true, letters.chars());
                let shown = flags.iter().collect();
                if let Some(entry) = self.channels.get_mut(&key) {
                    entry.1 = Some(flags);
                }
                Some((channel.clone(), shown))
            }
            Command::ChannelMODE(channel, modes) => {
                let key = mapping.fold(channel);
                let mut updated = self.channels.get_mut(&key)?.1.take()?;
                for mode in modes {
                    let (plus, mode) = match mode {
                        Mode::Plus(mode, _) => (true, mode),
                        Mode::Minus(mode, _) => (false, mode),
                        Mode::NoPrefix(_) => continue,
                    };
                    if let Some(letter) = mode.to_string().chars().next() {
                        self.set(&mut updated, plus, letter);
                    }
                }
                let shown = updated.iter().collect();
                if let Some(entry) = self.channels.get_mut(&key) {
                    entry.1 = Some(updated);
                }
                Some((channel.clone(), shown))
            }
            _ => None,
        }
    }

    /// Our own join starts from unknown modes and queues the query. Past
    /// `MAX_TRACKED` channels the join is not tracked at all.
    /// Returns a diagnostic the first time the limit is hit.
    pub(crate) fn joined(&mut self, channel: &str, mapping: CaseMapping) -> Option<String> {
        let key = mapping.fold(channel);
        if !self.channels.contains_key(&key) && self.channels.len() >= MAX_TRACKED {
            return (!std::mem::replace(&mut self.warned, true))
                .then(|| format!("Channel modes are not tracked beyond {MAX_TRACKED} channels."));
        }
        self.pending.retain(|queued| mapping.fold(queued) != key);
        self.pending.push_back(channel.to_owned());
        self.channels.insert(key, (channel.to_owned(), None));
        None
    }

    /// The flags of a channel we are in, `None` until the `324`.
    pub(crate) fn flags(&self, channel: &str, mapping: CaseMapping) -> Option<String> {
        let (_, flags) = self.channels.get(&mapping.fold(channel))?;
        flags.as_ref().map(|flags| flags.iter().collect())
    }

    /// A part or kick ends tracking.
    pub(crate) fn left(&mut self, channel: &str, mapping: CaseMapping) {
        let key = mapping.fold(channel);
        self.channels.remove(&key);
        self.pending.retain(|queued| mapping.fold(queued) != key);
        self.in_flight
            .retain(|(queried, _)| mapping.fold(queried) != key);
    }

    /// The channels whose modes are to be asked now: as many as free
    /// places remain in the window of unanswered queries. The rest wait
    /// for a `324` or a timeout, however many lines the server sends.
    pub(crate) fn take_queries(&mut self, now: Instant) -> Vec<String> {
        self.in_flight.retain(|(_, until)| *until > now);
        let count = MAX_IN_FLIGHT
            .saturating_sub(self.in_flight.len())
            .min(self.pending.len());
        let queries: Vec<String> = self.pending.drain(..count).collect();
        self.in_flight.extend(
            queries
                .iter()
                .map(|name| (name.clone(), now + QUERY_TIMEOUT)),
        );
        queries
    }

    /// When a waiting query may go out for want of an answer; none while
    /// the queue is empty, so no timer runs then.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        if self.pending.is_empty() {
            return None;
        }
        self.in_flight.iter().map(|(_, until)| *until).min()
    }

    /// The server changed its case mapping: fold the names again. Names
    /// that now collide keep one entry, with unknown modes.
    pub(crate) fn set_casemapping(&mut self, mapping: CaseMapping) {
        let old = std::mem::take(&mut self.channels);
        for (name, flags) in old.into_values() {
            match self.channels.entry(mapping.fold(&name)) {
                Entry::Occupied(mut taken) => taken.get_mut().1 = None,
                Entry::Vacant(free) => {
                    free.insert((name, flags));
                }
            }
        }
        // Queued and sent queries live only for channels still tracked under
        // their own name, so the queue never outgrows the table.
        let channels = &self.channels;
        let tracked = |name: &String| {
            channels
                .get(&mapping.fold(name))
                .is_some_and(|(own, _)| own == name)
        };
        self.pending.retain(tracked);
        self.in_flight.retain(|(name, _)| tracked(name));
    }

    fn apply(&self, flags: &mut Vec<char>, plus: bool, letters: impl Iterator<Item = char>) {
        let mut plus = plus;
        for letter in letters {
            match letter {
                '+' => plus = true,
                '-' => plus = false,
                letter => self.set(flags, plus, letter),
            }
        }
    }

    fn set(&self, flags: &mut Vec<char>, plus: bool, letter: char) {
        if !letter.is_ascii_alphabetic() || self.not_flags.contains(&letter) {
            return;
        }
        if plus {
            if !flags.contains(&letter) && flags.len() < MAX_FLAGS {
                flags.push(letter);
            }
        } else {
            flags.retain(|known| *known != letter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Message {
        line.parse().unwrap()
    }

    fn feed(modes: &mut ChannelModes, line: &str) -> Option<(String, String)> {
        modes.observe(&parse(line), CaseMapping::default(), false)
    }

    /// Tracking `#x`, as after our own join.
    fn in_x() -> ChannelModes {
        let mut modes = ChannelModes::default();
        modes.joined("#x", CaseMapping::default());
        modes
    }

    #[test]
    fn reply_sets_flags_and_changes_follow() {
        let mut modes = in_x();
        assert_eq!(
            feed(&mut modes, ":s 324 me #x +snt"),
            Some(("#x".into(), "snt".into()))
        );
        assert_eq!(
            feed(&mut modes, ":a!u@h MODE #x +m-s"),
            Some(("#x".into(), "ntm".into()))
        );
        // Repeats are not shown twice; membership and list modes are no flags.
        assert_eq!(
            feed(&mut modes, ":a!u@h MODE #x +mo-b+v a b c"),
            Some(("#x".into(), "ntm".into()))
        );
    }

    #[test]
    fn arguments_never_become_flags() {
        let mut modes = in_x();
        let shown = feed(&mut modes, ":s 324 me #x +ntkl secret 50").unwrap().1;
        assert_eq!(shown, "ntkl");
        let shown = feed(&mut modes, ":a!u@h MODE #x +Zk unknownarg secret")
            .unwrap()
            .1;
        assert!(!shown.contains("secret") && !shown.contains("unknownarg"));
    }

    #[test]
    fn changes_before_the_reply_and_other_channels_are_ignored() {
        let mut modes = in_x();
        assert_eq!(feed(&mut modes, ":a!u@h MODE #x +m"), None);
        assert_eq!(feed(&mut modes, ":s 324 me #other +n"), None);
        assert_eq!(feed(&mut modes, ":a!u@h MODE #other +m"), None);
    }

    #[test]
    fn replay_leaves_no_state_and_leaving_or_rejoining_forgets() {
        let mut modes = in_x();
        let replayed = modes.observe(&parse(":s 324 me #x +n"), CaseMapping::default(), true);
        assert_eq!(replayed, None);
        feed(&mut modes, ":s 324 me #x +n");
        modes.joined("#X", CaseMapping::default());
        assert_eq!(feed(&mut modes, ":a!u@h MODE #x +m"), None);
        feed(&mut modes, ":s 324 me #x +n");
        modes.left("#X", CaseMapping::default());
        assert_eq!(feed(&mut modes, ":a!u@h MODE #x +m"), None);
        assert_eq!(feed(&mut modes, ":s 324 me #x +n"), None);
    }

    #[test]
    fn isupport_overrides_the_grammar() {
        let mut modes = in_x();
        modes.isupport(&parse(
            ":s 005 me CHANMODES=beI,k,l,imnpst PREFIX=(qaohv)~&@%+ :are supported",
        ));
        let shown = feed(&mut modes, ":s 324 me #x +nqa").unwrap().1;
        assert_eq!(shown, "n");
    }

    #[test]
    fn tracking_and_queries_are_bounded() {
        let mut modes = ChannelModes::default();
        let mapping = CaseMapping::default();
        let mut warnings = 0;
        for n in 0..MAX_TRACKED + 100 {
            warnings += usize::from(modes.joined(&format!("#c{n}"), mapping).is_some());
        }
        assert_eq!(warnings, 1);
        assert_eq!(modes.channels.len(), MAX_TRACKED);
        assert_eq!(
            feed(&mut modes, &format!(":s 324 me #c{MAX_TRACKED} +n")),
            None
        );
        // The queue holds each channel once.
        modes.joined("#c2", mapping);
        assert_eq!(modes.pending.len(), MAX_TRACKED);
        modes.left("#c3", mapping);
        assert_eq!(modes.pending.len(), MAX_TRACKED - 1);
    }

    #[test]
    fn unanswered_queries_are_limited_and_move_on_by_reply_or_timeout() {
        let mut modes = ChannelModes::default();
        let mapping = CaseMapping::default();
        for n in 0..10 {
            modes.joined(&format!("#c{n}"), mapping);
        }
        let start = Instant::now();
        assert_eq!(modes.take_queries(start), ["#c0", "#c1", "#c2", "#c3"]);
        // Nothing more however often it is asked.
        assert!(modes.take_queries(start).is_empty());
        assert_eq!(modes.next_deadline(), Some(start + QUERY_TIMEOUT));
        // A reply frees one place.
        feed(&mut modes, ":s 324 me #c1 +n");
        assert_eq!(modes.take_queries(start), ["#c4"]);
        // A part frees one too.
        modes.left("#c0", mapping);
        assert_eq!(modes.take_queries(start), ["#c5"]);
        // Silence ends at the timeout; those channels stay unknown.
        let late = start + QUERY_TIMEOUT;
        assert_eq!(modes.take_queries(late), ["#c6", "#c7", "#c8", "#c9"]);
        assert_eq!(modes.next_deadline(), None);
        assert_eq!(modes.channels["#c2"].1, None);
    }

    #[test]
    fn refolding_again_and_again_keeps_the_queue_within_the_table() {
        let mut modes = ChannelModes::default();
        let ascii = CaseMapping::Ascii;
        let rfc = CaseMapping::Rfc1459;
        for round in 0..6 {
            // Distinct under ASCII, one name under RFC 1459.
            for n in 0..MAX_TRACKED {
                let bracket = if n % 2 == 0 { '[' } else { '{' };
                modes.joined(&format!("#r{round}x{n}{bracket}"), ascii);
            }
            modes.set_casemapping(rfc);
            assert!(modes.pending.len() <= modes.channels.len());
            assert!(modes.pending.len() <= MAX_TRACKED);
            modes.set_casemapping(ascii);
            assert!(modes.pending.len() <= modes.channels.len());
            assert!(modes.pending.len() <= MAX_TRACKED);
        }
        for name in &modes.pending {
            assert_eq!(modes.channels[name].0, *name);
        }
    }

    #[test]
    fn colliding_names_become_unknown_whatever_their_order() {
        for _ in 0..20 {
            let mut modes = ChannelModes::default();
            modes.joined("#x[", CaseMapping::Ascii);
            modes.joined("#x{", CaseMapping::Ascii);
            for name in ["#x[", "#x{"] {
                modes.observe(
                    &parse(&format!(":s 324 me {name} +n")),
                    CaseMapping::Ascii,
                    false,
                );
            }
            modes.set_casemapping(CaseMapping::default());
            assert_eq!(modes.channels.len(), 1);
            assert!(modes.channels.values().all(|(_, flags)| flags.is_none()));
        }
    }

    #[test]
    fn a_casemapping_change_keeps_the_state_reachable() {
        let mut modes = ChannelModes::default();
        modes.joined("#foo[", CaseMapping::default());
        feed(&mut modes, ":s 324 me #foo[ +n");
        modes.set_casemapping(CaseMapping::Ascii);
        let seen = modes.observe(&parse(":a!u@h MODE #foo[ +m"), CaseMapping::Ascii, false);
        assert_eq!(seen, Some(("#foo[".into(), "nm".into())));
        modes.left("#foo[", CaseMapping::Ascii);
        assert!(modes.channels.is_empty());
    }

    #[test]
    fn an_empty_reply_is_known_and_empty() {
        let mut modes = in_x();
        assert_eq!(
            feed(&mut modes, ":s 324 me #x +"),
            Some(("#x".into(), String::new()))
        );
    }
}
