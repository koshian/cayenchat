// SPDX-License-Identifier: MPL-2.0
//! Getting operator status back after an unexpected disconnect (#444).
//!
//! When a link dies without the server noticing, the new connection may have
//! to register under a temporary nickname, and once the old link times out
//! we are alone in the channel without the operator status we had. On
//! networks where an empty channel disappears and its next creator becomes
//! operator (IRCnet), parting and joining again gets it back. The price is
//! that the channel is created anew: its topic and modes are gone, and so
//! are its lists. Only the topic and the plain flags in [`RESTORED_FLAGS`]
//! are put back; never a key, a limit or a ban, exception or reop list.
//!
//! The state machine here is fed what the worker sees and says what to send.
//! A channel qualifies only if it is in the set of channels the previous,
//! unexpectedly ended connection had operator status in (`carried`): a
//! channel that is merely opless and empty, a `/deop`, or a channel we
//! never were operator in is left alone. An attempt is made once per
//! channel, after the channel has stayed unchanged for [`STABLE`], and
//! whatever happens is final: failure is reported, never retried.

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

use crate::{CaseMapping, text::same_nickname};

/// How long the roster must stay "only us, no operator" before acting, so a
/// netsplit rejoin, a late NAMES or a queued JOIN of someone else is seen.
const STABLE: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(5)
};
/// How long the PART and JOIN of an attempt may take before it is given up.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long after the connection started a channel can still be carried
/// over; the old link is gone from the server well before that.
const CARRY_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Most channels tracked; beyond it a channel is neither watched nor
/// reported.
const MAX_TRACKED: usize = 512;
/// The longest topic put back, in bytes; a longer one is not restored.
const MAX_TOPIC: usize = 400;
/// The channel flags (no argument) put back after the channel was created
/// anew: moderated, invite-only, no outside messages, private, secret and
/// topic-locked. Keys, limits and anything unknown are never restored.
const RESTORED_FLAGS: &str = "imnpst";

/// Whether a roster entry shows operator status (or more); only voice is
/// less. A half-op alone also counts, because it is not what we lost.
fn privileged(entry: &str) -> bool {
    entry.starts_with(['~', '&', '@', '%'])
}

/// Where an attempt is, as the worker reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpRegainStatus {
    /// We parted the (empty, opless) channel and are joining it again.
    Rejoining,
    /// Operator status is back. `modes` and `topic` say what was put back.
    Regained { modes: bool, topic: bool },
    /// The channel was joined again but without operator status.
    NotRegained,
    /// The channel was not joined again (server refusal or no answer); we
    /// are out of it.
    RejoinFailed,
}

/// What the worker is to do next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Send `PART` and `JOIN` for the channel.
    Rejoin(String),
    /// Send `MODE channel +flags` (if any) and `TOPIC channel :topic`.
    Restore {
        channel: String,
        flags: String,
        topic: Option<String>,
    },
    /// Tell the user.
    Status(String, OpRegainStatus),
}

#[derive(Debug, PartialEq, Eq)]
enum Stage {
    Watching,
    /// Alone and opless since the roster last changed; act at the instant.
    Settling(Instant),
    /// PART and JOIN were sent. `parted` / `joined` are our own echoes.
    Attempt {
        until: Instant,
        parted: bool,
        joined: bool,
    },
    /// Done, one way or the other; nothing more is ever done for this loss.
    Finished,
}

struct Restore {
    flags: String,
    topic: Option<String>,
}

struct Channel {
    name: String,
    /// We were operator here when the previous connection ended, and have
    /// not been given reason to forget it since.
    carried: bool,
    in_channel: bool,
    /// The end of NAMES came after our JOIN: the roster below is complete.
    synced: bool,
    op: bool,
    alone: bool,
    topic: String,
    stage: Stage,
    restore: Option<Restore>,
}

impl Channel {
    fn new(name: &str, carried: bool) -> Self {
        Self {
            name: name.to_owned(),
            carried,
            in_channel: false,
            synced: false,
            op: false,
            alone: false,
            topic: String::new(),
            stage: Stage::Watching,
            restore: None,
        }
    }
}

pub(crate) struct OpRegain {
    enabled: bool,
    mapping: CaseMapping,
    started: Instant,
    channels: HashMap<String, Channel>,
    /// The last set handed out by [`OpRegain::changed_ops`].
    reported: Vec<String>,
    /// Kept up to date by [`OpRegain::touch`], so that the per-message
    /// questions "is anything due" and "did the set change" cost nothing
    /// while no channel changed.
    deadline: Option<Instant>,
    dirty: bool,
}

impl OpRegain {
    /// `carried`: the channels the previous connection had operator status
    /// in when it ended unexpectedly. Disabled, nothing is tracked.
    pub(crate) fn new(enabled: bool, carried: &[String], started: Instant) -> Self {
        let mapping = CaseMapping::default();
        let mut channels = HashMap::new();
        if enabled {
            for name in carried.iter().take(MAX_TRACKED) {
                if eligible(name) {
                    channels.insert(mapping.fold(name), Channel::new(name, true));
                }
            }
        }
        let mut regain = Self {
            enabled,
            mapping,
            started,
            channels,
            reported: Vec::new(),
            deadline: None,
            dirty: false,
        };
        // The carried set is what the application already holds.
        regain.reported = regain.ops();
        regain.touch();
        regain.dirty = false;
        regain
    }

    /// Call after any change of a channel.
    fn touch(&mut self) {
        let carried = self
            .channels
            .values()
            .any(|channel| channel.carried)
            .then_some(self.started + CARRY_WINDOW);
        self.deadline = self
            .channels
            .values()
            .filter_map(|channel| match channel.stage {
                Stage::Settling(at) => Some(at),
                Stage::Attempt { until, .. } => Some(until),
                _ => None,
            })
            .chain(carried)
            .min();
        self.dirty = true;
    }

    pub(crate) fn set_casemapping(&mut self, mapping: CaseMapping) {
        let old = std::mem::take(&mut self.channels);
        self.mapping = mapping;
        for (_, channel) in old {
            self.channels
                .entry(mapping.fold(&channel.name))
                .or_insert(channel);
        }
        self.touch();
    }

    /// Our own JOIN of `name`.
    pub(crate) fn joined(&mut self, name: &str) {
        if !self.enabled {
            return;
        }
        let key = self.mapping.fold(name);
        if !self.channels.contains_key(&key) && self.channels.len() >= MAX_TRACKED {
            return;
        }
        let channel = self
            .channels
            .entry(key)
            .or_insert_with(|| Channel::new(name, false));
        channel.name = name.to_owned();
        channel.in_channel = true;
        channel.synced = false;
        channel.op = false;
        channel.alone = false;
        channel.topic.clear();
        match &mut channel.stage {
            Stage::Attempt {
                parted: true,
                joined,
                ..
            } => *joined = true,
            stage => *stage = Stage::Watching,
        }
        self.touch();
    }

    /// Our own PART, or a KICK of us. Outside an attempt (or a second time
    /// within it) the user or a moderator left: forget the channel.
    pub(crate) fn parted(&mut self, name: &str) {
        let key = self.mapping.fold(name);
        let Some(channel) = self.channels.get_mut(&key) else {
            return;
        };
        match &mut channel.stage {
            Stage::Attempt { parted, joined, .. } if !*parted && !*joined => {
                *parted = true;
                channel.in_channel = false;
            }
            _ => {
                self.channels.remove(&key);
            }
        }
        self.touch();
    }

    pub(crate) fn topic(&mut self, name: &str, topic: &str) {
        if let Some(channel) = self.channels.get_mut(&self.mapping.fold(name)) {
            channel.topic = topic.to_owned();
        }
    }

    /// The server refused our JOIN of `name`.
    pub(crate) fn join_failed(&mut self, name: &str) -> Vec<Step> {
        let key = self.mapping.fold(name);
        let steps = match self.channels.get_mut(&key) {
            Some(channel) if matches!(channel.stage, Stage::Attempt { .. }) => {
                channel.stage = Stage::Finished;
                channel.in_channel = false;
                vec![Step::Status(
                    channel.name.clone(),
                    OpRegainStatus::RejoinFailed,
                )]
            }
            _ => Vec::new(),
        };
        self.touch();
        steps
    }

    /// A roster of `name`; `complete`: it came from the end of NAMES.
    pub(crate) fn roster(
        &mut self,
        name: &str,
        users: &[String],
        nick: &str,
        complete: bool,
        now: Instant,
    ) -> Vec<Step> {
        let steps = self.roster_changed(name, users, nick, complete, now);
        self.touch();
        steps
    }

    fn roster_changed(
        &mut self,
        name: &str,
        users: &[String],
        nick: &str,
        complete: bool,
        now: Instant,
    ) -> Vec<Step> {
        let Some(channel) = self.channels.get_mut(&self.mapping.fold(name)) else {
            return Vec::new();
        };
        if !channel.in_channel {
            return Vec::new();
        }
        if complete {
            channel.synced = true;
        }
        if !channel.synced {
            return Vec::new();
        }
        let display = |entry: &str| {
            entry
                .trim_start_matches(['~', '&', '@', '%', '+'])
                .to_owned()
        };
        let entries: Vec<&String> = users
            .iter()
            .filter(|user| !display(user).is_empty())
            .collect();
        let own = entries
            .iter()
            .find(|entry| same_nickname(&display(entry), nick));
        channel.op = own.is_some_and(|entry| privileged(entry));
        channel.alone = entries.len() == 1 && own.is_some();
        if channel.op {
            channel.carried = false;
        }
        match &channel.stage {
            Stage::Attempt { joined: true, .. } if complete => {
                let name = channel.name.clone();
                let restore = channel.restore.take();
                channel.stage = Stage::Finished;
                if !channel.op {
                    return vec![Step::Status(name, OpRegainStatus::NotRegained)];
                }
                let Restore { flags, topic } = restore.unwrap_or(Restore {
                    flags: String::new(),
                    topic: None,
                });
                let status = OpRegainStatus::Regained {
                    modes: !flags.is_empty(),
                    topic: topic.is_some(),
                };
                let mut steps = Vec::new();
                if !flags.is_empty() || topic.is_some() {
                    steps.push(Step::Restore {
                        channel: name.clone(),
                        flags,
                        topic,
                    });
                }
                steps.push(Step::Status(name, status));
                steps
            }
            Stage::Attempt { .. } | Stage::Finished => Vec::new(),
            _ => {
                channel.stage = if channel.carried && channel.alone && !channel.op {
                    match channel.stage {
                        Stage::Settling(at) => Stage::Settling(at),
                        _ => Stage::Settling(now + STABLE),
                    }
                } else {
                    Stage::Watching
                };
                Vec::new()
            }
        }
    }

    /// Acts on what has been waited for. `ready`: registered, no alive
    /// check running and no nickname being asked for. `flags_of` is the
    /// channel's flags as tracked, `None` while unknown (nothing is done
    /// then: the modes could not be put back).
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        ready: bool,
        mut flags_of: impl FnMut(&str) -> Option<String>,
    ) -> Vec<Step> {
        let mut steps = Vec::new();
        if now >= self.started + CARRY_WINDOW {
            for channel in self.channels.values_mut() {
                channel.carried = false;
                if matches!(channel.stage, Stage::Settling(_)) {
                    channel.stage = Stage::Watching;
                }
            }
            self.channels.retain(|_, channel| {
                channel.in_channel || !matches!(channel.stage, Stage::Watching)
            });
        }
        for channel in self.channels.values_mut() {
            match channel.stage {
                Stage::Settling(at) if at <= now => {
                    let flags = flags_of(&channel.name);
                    let (Some(flags), true) = (flags, ready) else {
                        channel.stage = Stage::Settling(now + STABLE);
                        continue;
                    };
                    if !(channel.carried
                        && channel.alone
                        && !channel.op
                        && channel.synced
                        && channel.in_channel)
                    {
                        channel.stage = Stage::Watching;
                        continue;
                    }
                    let flags: String = flags
                        .chars()
                        .filter(|letter| RESTORED_FLAGS.contains(*letter))
                        .collect();
                    let topic = (!channel.topic.is_empty() && channel.topic.len() <= MAX_TOPIC)
                        .then(|| channel.topic.clone());
                    channel.restore = Some(Restore { flags, topic });
                    channel.carried = false;
                    channel.stage = Stage::Attempt {
                        until: now + ATTEMPT_TIMEOUT,
                        parted: false,
                        joined: false,
                    };
                    steps.push(Step::Rejoin(channel.name.clone()));
                    steps.push(Step::Status(
                        channel.name.clone(),
                        OpRegainStatus::Rejoining,
                    ));
                }
                Stage::Attempt { until, .. } if until <= now => {
                    let in_channel = channel.in_channel;
                    channel.stage = Stage::Finished;
                    channel.restore = None;
                    steps.push(Step::Status(
                        channel.name.clone(),
                        if in_channel {
                            OpRegainStatus::NotRegained
                        } else {
                            OpRegainStatus::RejoinFailed
                        },
                    ));
                }
                _ => {}
            }
        }
        self.touch();
        steps
    }

    /// When [`OpRegain::poll`] next has something to do.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// The channels to carry over should the connection now end
    /// unexpectedly: those we are operator in, and those still waited on.
    fn ops(&self) -> Vec<String> {
        let mut ops: Vec<String> = self
            .channels
            .values()
            .filter(|channel| channel.op || channel.carried)
            .map(|channel| channel.name.clone())
            .collect();
        ops.sort();
        ops
    }

    /// The set from [`OpRegain::ops`] if it differs from the last one given.
    pub(crate) fn changed_ops(&mut self) -> Option<Vec<String>> {
        if !self.enabled || !std::mem::take(&mut self.dirty) {
            return None;
        }
        let ops = self.ops();
        (ops != self.reported).then(|| {
            self.reported = ops.clone();
            ops
        })
    }
}

/// Channels worth a new attempt: `#` and `&`, whose creator is made
/// operator. `+` channels have no operators, and a `!` safe channel has a
/// creation procedure of its own (the first JOIN names it), so both are
/// left alone. Names are used exactly as the server gave them, which keeps
/// IRCnet's server-mask channels (`#name:*.jp`) intact.
fn eligible(name: &str) -> bool {
    name.starts_with(['#', '&']) && crate::valid_channel(name)
}

#[cfg(test)]
#[path = "regain_tests.rs"]
mod tests;
