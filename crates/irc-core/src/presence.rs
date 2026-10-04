//! Who is present in which joined channel, indexed both ways.
//!
//! The index lives in the connection's worker next to the published rosters
//! and is fed from them: a channel's NAMES snapshot replaces its membership,
//! while the lines that name a user (NICK, QUIT) carry the user's identity
//! across snapshots. A [`UserId`] therefore survives a snapshot being applied
//! again and a NICK in any number of channels, and a nickname that reappears
//! after QUIT or after leaving every channel we share is a new presence.
//!
//! Ids are never reused within a connection and are not meant to be kept
//! across connections. Nicknames and channel names are compared with the
//! same case mapping everywhere (see [`crate::text`]).

use std::collections::{HashMap, HashSet};

use crate::text::nickname_key;

/// One user's presence on this connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct UserId(u64);

struct Presence {
    key: String,
    channels: HashSet<String>,
}

#[derive(Default)]
struct Members {
    name: String,
    users: HashSet<UserId>,
}

#[derive(Default)]
pub(crate) struct PresenceIndex {
    next: u64,
    by_nick: HashMap<String, UserId>,
    users: HashMap<UserId, Presence>,
    channels: HashMap<String, Members>,
}

impl PresenceIndex {
    pub(crate) fn user(&self, nickname: &str) -> Option<UserId> {
        self.by_nick.get(&nickname_key(nickname)).copied()
    }

    /// Whether `nickname` is in `channel`.
    pub(crate) fn in_channel(&self, channel: &str, nickname: &str) -> bool {
        self.user(nickname).is_some_and(|id| {
            self.channels
                .get(&nickname_key(channel))
                .is_some_and(|members| members.users.contains(&id))
        })
    }

    /// Whether `nickname` is in a channel other than `except`.
    #[cfg(test)]
    pub(crate) fn shares(&self, nickname: &str, except: Option<&str>) -> bool {
        let except = except.map(nickname_key);
        self.user(nickname)
            .and_then(|id| self.users.get(&id))
            .is_some_and(|presence| {
                presence
                    .channels
                    .iter()
                    .any(|channel| Some(channel) != except.as_ref())
            })
    }

    /// The channels (as spelled when their membership was last replaced)
    /// holding `nickname`.
    #[cfg(test)]
    pub(crate) fn channels_of(&self, nickname: &str) -> Vec<&str> {
        let mut channels: Vec<&str> = self
            .user(nickname)
            .and_then(|id| self.users.get(&id))
            .into_iter()
            .flat_map(|presence| &presence.channels)
            .filter_map(|key| self.channels.get(key))
            .map(|members| members.name.as_str())
            .collect();
        channels.sort_unstable();
        channels
    }

    /// Replaces the membership of `channel` with `members` (nicknames,
    /// possibly with rank prefixes). Users already known keep their id.
    pub(crate) fn replace_channel(&mut self, channel: &str, members: &[String]) {
        let channel_key = nickname_key(channel);
        let mut present = HashSet::new();
        for member in members {
            let nickname = member.trim_start_matches(['~', '&', '@', '%', '+']);
            if nickname.is_empty() {
                continue;
            }
            let key = nickname_key(nickname);
            let id = match self.by_nick.get(&key) {
                Some(id) => *id,
                None => {
                    let id = UserId(self.next);
                    self.next += 1;
                    self.by_nick.insert(key.clone(), id);
                    self.users.insert(
                        id,
                        Presence {
                            key,
                            channels: HashSet::new(),
                        },
                    );
                    id
                }
            };
            present.insert(id);
        }
        let entry = self.channels.entry(channel_key.clone()).or_default();
        entry.name = channel.to_owned();
        let departed: Vec<UserId> = entry.users.difference(&present).copied().collect();
        entry.users = present.clone();
        for id in &present {
            if let Some(presence) = self.users.get_mut(id) {
                presence.channels.insert(channel_key.clone());
            }
        }
        for id in departed {
            self.leave(id, &channel_key);
        }
    }

    /// Forgets `channel` and everybody only there (we left it).
    pub(crate) fn remove_channel(&mut self, channel: &str) {
        let channel_key = nickname_key(channel);
        if let Some(members) = self.channels.remove(&channel_key) {
            for id in members.users {
                self.leave(id, &channel_key);
            }
        }
    }

    /// `old` is now called `new`; the presence is the same.
    pub(crate) fn rename(&mut self, old: &str, new: &str) {
        let Some(id) = self.by_nick.remove(&nickname_key(old)) else {
            return;
        };
        // A stale holder of the new name (it quit unseen) is another presence.
        if let Some(stale) = self.by_nick.remove(&nickname_key(new)) {
            self.forget(stale);
        }
        let key = nickname_key(new);
        self.by_nick.insert(key.clone(), id);
        if let Some(presence) = self.users.get_mut(&id) {
            presence.key = key;
        }
    }

    /// `nickname` left the network.
    pub(crate) fn quit(&mut self, nickname: &str) {
        if let Some(id) = self.user(nickname) {
            self.forget(id);
        }
    }

    fn forget(&mut self, id: UserId) {
        if let Some(presence) = self.users.remove(&id) {
            self.by_nick.remove(&presence.key);
            for channel in presence.channels {
                if let Some(members) = self.channels.get_mut(&channel) {
                    members.users.remove(&id);
                }
            }
        }
    }

    fn leave(&mut self, id: UserId, channel_key: &str) {
        let Some(presence) = self.users.get_mut(&id) else {
            return;
        };
        presence.channels.remove(channel_key);
        if presence.channels.is_empty() {
            self.forget(id);
        }
    }

    /// Both directions agree and nothing is orphaned.
    #[cfg(test)]
    fn consistent(&self) -> bool {
        self.users.iter().all(|(id, presence)| {
            !presence.channels.is_empty()
                && self.by_nick.get(&presence.key) == Some(id)
                && presence.channels.iter().all(|channel| {
                    self.channels
                        .get(channel)
                        .is_some_and(|members| members.users.contains(id))
                })
        }) && self.by_nick.len() == self.users.len()
            && self.channels.iter().all(|(_, members)| {
                members.users.iter().all(|id| {
                    self.users.get(id).is_some_and(|presence| {
                        presence.channels.contains(&nickname_key(&members.name))
                    })
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn reapplying_a_snapshot_keeps_ids() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["@bob", "carol"]));
        let bob = index.user("bob");
        assert!(bob.is_some());
        index.replace_channel("#a", &names(&["bob", "carol", "dave"]));
        assert_eq!(index.user("BOB"), bob);
        assert!(index.consistent());
    }

    #[test]
    fn channel_spelling_does_not_split_membership() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#Room", &names(&["bob"]));
        assert!(index.in_channel("#room", "bob"));
        index.replace_channel("#room", &names(&["bob", "carol"]));
        assert_eq!(index.channels_of("bob").len(), 1);
        assert!(!index.shares("bob", Some("#ROOM")));
        assert!(index.consistent());
    }

    #[test]
    fn nick_keeps_the_id_across_channels() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["@bob"]));
        index.replace_channel("#b", &names(&["bob", "carol"]));
        let bob = index.user("bob");
        index.rename("bob", "robert");
        // The library's snapshots arrive afterwards, one channel at a time.
        index.replace_channel("#a", &names(&["@robert"]));
        assert_eq!(index.user("robert"), bob);
        assert_eq!(index.user("bob"), None);
        index.replace_channel("#b", &names(&["robert", "carol"]));
        assert_eq!(index.user("robert"), bob);
        assert_eq!(index.channels_of("robert"), ["#a", "#b"]);
        assert!(index.consistent());
    }

    #[test]
    fn rejoining_after_the_last_shared_channel_is_new() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["bob"]));
        index.replace_channel("#b", &names(&["bob"]));
        let first = index.user("bob");
        index.replace_channel("#a", &names(&[]));
        assert_eq!(index.user("bob"), first);
        assert!(!index.shares("bob", Some("#b")));
        index.replace_channel("#b", &names(&[]));
        assert_eq!(index.user("bob"), None);
        index.replace_channel("#a", &names(&["bob"]));
        assert_ne!(index.user("bob"), first);
        assert!(index.consistent());
    }

    #[test]
    fn quit_ends_the_presence_everywhere() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["bob", "carol"]));
        index.replace_channel("#b", &names(&["bob"]));
        let first = index.user("bob");
        index.quit("bob");
        assert_eq!(index.user("bob"), None);
        assert!(!index.in_channel("#a", "bob"));
        index.replace_channel("#a", &names(&["bob", "carol"]));
        assert_ne!(index.user("bob"), first);
        assert!(index.consistent());
    }

    #[test]
    fn leaving_a_channel_forgets_only_its_sole_members() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["bob", "carol"]));
        index.replace_channel("#b", &names(&["bob"]));
        index.remove_channel("#A");
        assert!(index.user("carol").is_none());
        assert_eq!(index.channels_of("bob"), ["#b"]);
        assert!(index.consistent());
    }

    #[test]
    fn rename_over_a_stale_name_replaces_it() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["bob", "robert"]));
        let bob = index.user("bob");
        index.rename("bob", "robert");
        assert_eq!(index.user("robert"), bob);
        assert!(index.consistent());
    }

    #[test]
    fn empty_entries_from_padded_names_are_skipped() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["@", "bob"]));
        assert!(index.user("").is_none());
        assert!(index.consistent());
    }
}
