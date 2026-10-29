// SPDX-License-Identifier: MPL-2.0
//! Getting operator status back when we are alone without it (#444, #478).
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
//! Whatever the history of the connection, a joined channel qualifies when
//! its roster holds only us, without operator status, and has stayed so for
//! [`STABLE`]. An attempt is made once per channel and connection, and
//! whatever happens is final: failure is reported, never retried. A channel
//! gets another attempt after we were seen as operator in it, or after the
//! user left and joined it again.

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
/// How long the server has to show the restored modes and topic before they
/// are reported as not restored.
const RESTORE_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(5)
};
/// Most channels parted and joined again by one poll; the others due at the
/// same time follow [`REJOIN_SPACING`] later, so 512 channels never send
/// about 1000 commands at once.
const MAX_REJOINS_PER_POLL: usize = 4;
/// How long the channels left over by [`MAX_REJOINS_PER_POLL`] wait.
const REJOIN_SPACING: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(2)
};
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
    /// Operator status is back, and the server confirmed what was put back:
    /// `modes` and `topic` say which of the two.
    Regained { modes: bool, topic: bool },
    /// Operator status is back, but the server did not confirm the modes
    /// and/or the topic that were to be put back (`true`: not confirmed).
    RestoreFailed { modes: bool, topic: bool },
    /// The channel was left alone: its topic is too long to be put back, and
    /// parting would lose it.
    TopicTooLong,
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
    /// Operator status is back and the restoring commands were sent; the
    /// server's answer is judged at the instant.
    Restoring {
        at: Instant,
        flags: String,
        topic: Option<String>,
        topic_seen: bool,
    },
}

struct Restore {
    flags: String,
    topic: Option<String>,
}

struct Channel {
    name: String,
    /// An attempt was made (or declined) here; no other until we are seen
    /// as operator or the channel is forgotten.
    tried: bool,
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
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            tried: false,
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
    channels: HashMap<String, Channel>,
    /// Kept up to date by [`OpRegain::touch`], so that the per-message
    /// question "is anything due" costs nothing while no channel changed.
    deadline: Option<Instant>,
}

impl OpRegain {
    /// Disabled, nothing is tracked.
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            mapping: CaseMapping::default(),
            channels: HashMap::new(),
            deadline: None,
        }
    }

    /// Call after any change of a channel.
    fn touch(&mut self) {
        self.deadline = self
            .channels
            .values()
            .filter_map(|channel| match channel.stage {
                Stage::Settling(at) => Some(at),
                Stage::Attempt { until, .. } => Some(until),
                Stage::Restoring { at, .. } => Some(at),
                Stage::Watching => None,
            })
            .min();
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
        if !self.enabled || !eligible(name) {
            return;
        }
        let key = self.mapping.fold(name);
        if !self.channels.contains_key(&key) && self.channels.len() >= MAX_TRACKED {
            return;
        }
        let channel = self
            .channels
            .entry(key)
            .or_insert_with(|| Channel::new(name));
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

    /// The user sent a PART for `name`: forget the channel at once, without
    /// waiting for the server's echo, which a slow server may send after a
    /// pending attempt would have fired.
    pub(crate) fn leaving(&mut self, name: &str) {
        if self.channels.remove(&self.mapping.fold(name)).is_some() {
            self.touch();
        }
    }

    pub(crate) fn topic(&mut self, name: &str, topic: &str) {
        if let Some(channel) = self.channels.get_mut(&self.mapping.fold(name)) {
            channel.topic = topic.to_owned();
            if let Stage::Restoring {
                topic: Some(expected),
                topic_seen,
                ..
            } = &mut channel.stage
                && expected == topic
            {
                *topic_seen = true;
            }
        }
    }

    /// The server refused our JOIN of `name`.
    pub(crate) fn join_failed(&mut self, name: &str) -> Vec<Step> {
        let key = self.mapping.fold(name);
        let steps = match self.channels.get_mut(&key) {
            Some(channel) if matches!(channel.stage, Stage::Attempt { .. }) => {
                channel.stage = Stage::Watching;
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
        fn display(entry: &str) -> &str {
            entry.trim_start_matches(['~', '&', '@', '%', '+'])
        }
        let entries: Vec<&String> = users
            .iter()
            .filter(|user| !display(user).is_empty())
            .collect();
        let own = entries
            .iter()
            .find(|entry| same_nickname(display(entry), nick));
        channel.op = own.is_some_and(|entry| privileged(entry));
        channel.alone = entries.len() == 1 && own.is_some();
        // Having been operator earns another attempt should we be left
        // alone without it again.
        if channel.op {
            channel.tried = false;
        }
        match &channel.stage {
            Stage::Attempt { joined: true, .. } if complete => {
                let name = channel.name.clone();
                let restore = channel.restore.take();
                channel.stage = Stage::Watching;
                if !channel.op {
                    return vec![Step::Status(name, OpRegainStatus::NotRegained)];
                }
                let Restore { flags, topic } = restore.unwrap_or(Restore {
                    flags: String::new(),
                    topic: None,
                });
                if flags.is_empty() && topic.is_none() {
                    return vec![Step::Status(
                        name,
                        OpRegainStatus::Regained {
                            modes: false,
                            topic: false,
                        },
                    )];
                }
                channel.stage = Stage::Restoring {
                    at: now + RESTORE_WAIT,
                    flags: flags.clone(),
                    topic: topic.clone(),
                    topic_seen: false,
                };
                vec![Step::Restore {
                    channel: name,
                    flags,
                    topic,
                }]
            }
            Stage::Attempt { .. } | Stage::Restoring { .. } => Vec::new(),
            _ => {
                channel.stage = if !channel.tried && channel.alone && !channel.op {
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
        let mut rejoins = 0;
        for channel in self.channels.values_mut() {
            match channel.stage {
                Stage::Settling(at) if at <= now => {
                    let flags = flags_of(&channel.name);
                    let (Some(flags), true) = (flags, ready) else {
                        channel.stage = Stage::Settling(now + STABLE);
                        continue;
                    };
                    if !(!channel.tried
                        && channel.alone
                        && !channel.op
                        && channel.synced
                        && channel.in_channel)
                    {
                        channel.stage = Stage::Watching;
                        continue;
                    }
                    // A topic that cannot be put back would be lost by the
                    // new channel: leave the channel as it is.
                    if channel.topic.len() > MAX_TOPIC {
                        channel.tried = true;
                        channel.stage = Stage::Watching;
                        steps.push(Step::Status(
                            channel.name.clone(),
                            OpRegainStatus::TopicTooLong,
                        ));
                        continue;
                    }
                    // Many channels due at once (a ghost session gone) must not
                    // become a burst of PART and JOIN that trips the server's
                    // flood protection: the rest wait for a later poll, where
                    // the conditions are checked again.
                    if rejoins >= MAX_REJOINS_PER_POLL {
                        channel.stage = Stage::Settling(now + REJOIN_SPACING);
                        continue;
                    }
                    rejoins += 1;
                    let flags: String = flags
                        .chars()
                        .filter(|letter| RESTORED_FLAGS.contains(*letter))
                        .collect();
                    let topic = (!channel.topic.is_empty()).then(|| channel.topic.clone());
                    channel.restore = Some(Restore { flags, topic });
                    channel.tried = true;
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
                    channel.stage = Stage::Watching;
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
                Stage::Restoring { at, .. } if at <= now => {
                    let Stage::Restoring {
                        flags,
                        topic,
                        topic_seen,
                        ..
                    } = std::mem::replace(&mut channel.stage, Stage::Watching)
                    else {
                        continue;
                    };
                    // Roster changes during the check were tracked but not
                    // acted on: if we lost operator status again meanwhile,
                    // the wait starts now.
                    if !channel.tried
                        && channel.alone
                        && !channel.op
                        && channel.synced
                        && channel.in_channel
                    {
                        channel.stage = Stage::Settling(now + STABLE);
                    }
                    let known = flags_of(&channel.name).unwrap_or_default();
                    let modes_failed = flags.chars().any(|letter| !known.contains(letter));
                    let topic_failed = topic.is_some() && !topic_seen;
                    steps.push(Step::Status(
                        channel.name.clone(),
                        if modes_failed || topic_failed {
                            OpRegainStatus::RestoreFailed {
                                modes: modes_failed,
                                topic: topic_failed,
                            }
                        } else {
                            OpRegainStatus::Regained {
                                modes: !flags.is_empty(),
                                topic: topic.is_some(),
                            }
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
