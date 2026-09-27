//! Which avatar belongs to which user, per network, and which messages may
//! show it.
//!
//! Avatars are references as a protocol supplies them (for IRC, the URL
//! template of the `avatar` metadata key); this module neither fetches nor
//! interprets them, and nothing is copied into retained messages. Users are
//! identified by a key the caller folds by the protocol's rules (IRC:
//! case-mapped nickname), always within one network, so a same-named user on
//! another server never shares an avatar.
//!
//! Identity policy. A nickname is only a temporary name, so an avatar is
//! tied to an *occupancy* of it, delimited by message sequence numbers:
//!
//! - An avatar seen for a user who had none starts an occupancy at the next
//!   message sequence. Messages that arrived before it (including replayed
//!   history) show no avatar: they may have come from an earlier occupant.
//! - Updates keep the occupancy; earlier messages of it show the new image.
//! - Removal, QUIT, leaving the last shared channel, a nickname change away
//!   from the name, a disconnect or losing the capability end the occupancy.
//!   Its avatar is kept as the name's *retired* avatar for the messages that
//!   arrived during it, and a later occupant of the name starts afresh.
//! - A nickname change moves the avatar: messages under the old name keep
//!   it (retired), the new name starts a new occupancy.
//!
//! Only the latest retired occupancy per name is kept. When the identity is
//! uncertain the avatar is simply omitted. Both tables are bounded.

use std::{collections::HashMap, sync::Arc};

use cayenchat_model::NetworkId;

/// Users with a current avatar per network; more are ignored.
pub const MAX_CURRENT_AVATARS: usize = 2048;
/// Ended occupancies remembered per network for older messages. The
/// oldest-ended go first.
pub const MAX_RETIRED_AVATARS: usize = 512;

#[derive(Clone, Debug)]
struct Occupancy {
    avatar: Arc<str>,
    /// First message sequence that belongs to this occupancy.
    since: u64,
}

#[derive(Clone, Debug)]
struct Retired {
    avatar: Arc<str>,
    since: u64,
    /// First message sequence that no longer belongs to it.
    until: u64,
}

#[derive(Debug, Default)]
struct NetworkAvatars {
    current: HashMap<String, Occupancy>,
    retired: HashMap<String, Retired>,
}

impl NetworkAvatars {
    fn retire(&mut self, key: &str, next: u64) {
        let Some(occupancy) = self.current.remove(key) else {
            return;
        };
        if occupancy.since >= next {
            // No message can have belonged to it.
            return;
        }
        self.retired.insert(
            key.to_owned(),
            Retired {
                avatar: occupancy.avatar,
                since: occupancy.since,
                until: next,
            },
        );
        if self.retired.len() > MAX_RETIRED_AVATARS {
            // Drop the earliest-ended quarter at once, so a mass reset does
            // not rescan the table for every entry.
            let mut ends: Vec<u64> = self.retired.values().map(|retired| retired.until).collect();
            ends.sort_unstable();
            let cutoff = ends[MAX_RETIRED_AVATARS / 4];
            self.retired.retain(|_, retired| retired.until > cutoff);
        }
    }
}

/// Per-network avatar occupancies. See the module documentation.
#[derive(Debug, Default)]
pub struct AvatarDirectory {
    networks: HashMap<NetworkId, NetworkAvatars>,
}

impl AvatarDirectory {
    /// Sets or removes a user's avatar. `next` is the sequence the next
    /// message will get.
    pub fn set(&mut self, network: NetworkId, key: &str, avatar: Option<&str>, next: u64) {
        let avatars = self.networks.entry(network).or_default();
        match avatar {
            Some(avatar) => {
                if let Some(occupancy) = avatars.current.get_mut(key) {
                    if *occupancy.avatar != *avatar {
                        occupancy.avatar = avatar.into();
                    }
                } else if avatars.current.len() < MAX_CURRENT_AVATARS {
                    avatars.current.insert(
                        key.to_owned(),
                        Occupancy {
                            avatar: avatar.into(),
                            since: next,
                        },
                    );
                }
            }
            None => avatars.retire(key, next),
        }
        if avatars.current.is_empty() && avatars.retired.is_empty() {
            self.networks.remove(&network);
        }
    }

    /// A user changed name: the avatar follows them to `to`.
    pub fn rename(&mut self, network: NetworkId, from: &str, to: &str, next: u64) {
        let Some(avatars) = self.networks.get_mut(&network) else {
            return;
        };
        let avatar = avatars
            .current
            .get(from)
            .map(|occupancy| occupancy.avatar.clone());
        avatars.retire(from, next);
        avatars.retire(to, next);
        if let Some(avatar) = avatar {
            avatars.current.insert(
                to.to_owned(),
                Occupancy {
                    avatar,
                    since: next,
                },
            );
        }
    }

    /// Ends every occupancy of a network (disconnect, capability lost).
    /// Older messages keep their avatars.
    pub fn end_all(&mut self, network: NetworkId, next: u64) {
        if let Some(avatars) = self.networks.get_mut(&network) {
            let keys: Vec<String> = avatars.current.keys().cloned().collect();
            for key in keys {
                avatars.retire(&key, next);
            }
        }
    }

    /// Forgets a network entirely (removed, or its logs were cleared).
    pub fn remove_network(&mut self, network: NetworkId) {
        self.networks.remove(&network);
    }

    /// The avatar for a message by `key` with `sequence`, if its sender is
    /// known to be the avatar's owner.
    pub fn for_message(&self, network: NetworkId, key: &str, sequence: u64) -> Option<&Arc<str>> {
        let avatars = self.networks.get(&network)?;
        if let Some(occupancy) = avatars.current.get(key)
            && sequence >= occupancy.since
        {
            return Some(&occupancy.avatar);
        }
        avatars
            .retired
            .get(key)
            .filter(|retired| (retired.since..retired.until).contains(&sequence))
            .map(|retired| &retired.avatar)
    }

    /// The current avatar of a user who is present now (member list).
    pub fn current(&self, network: NetworkId, key: &str) -> Option<&Arc<str>> {
        self.networks
            .get(&network)?
            .current
            .get(key)
            .map(|occupancy| &occupancy.avatar)
    }

    /// Current and retired entries of a network (tests, measurements).
    pub fn len(&self, network: NetworkId) -> (usize, usize) {
        self.networks.get(&network).map_or((0, 0), |avatars| {
            (avatars.current.len(), avatars.retired.len())
        })
    }

    pub fn is_empty(&self) -> bool {
        self.networks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: NetworkId = NetworkId(1);
    const B: NetworkId = NetworkId(2);

    fn shown<'a>(directory: &'a AvatarDirectory, key: &str, sequence: u64) -> Option<&'a str> {
        directory
            .for_message(A, key, sequence)
            .map(|avatar| &**avatar)
    }

    #[test]
    fn avatars_belong_to_one_network_and_one_occupancy() {
        let mut directory = AvatarDirectory::default();
        // Messages 1..=4 arrived before bob's avatar.
        directory.set(A, "bob", Some("a1"), 5);
        assert_eq!(shown(&directory, "bob", 4), None, "earlier occupant?");
        assert_eq!(shown(&directory, "bob", 5), Some("a1"));
        assert_eq!(directory.for_message(B, "bob", 5), None, "other server");
        // An update keeps the occupancy: earlier messages show the new one.
        directory.set(A, "bob", Some("a2"), 9);
        assert_eq!(shown(&directory, "bob", 6), Some("a2"));
        // Bob quits at 12; someone else takes the name at 15.
        directory.set(A, "bob", None, 12);
        assert_eq!(directory.current(A, "bob"), None);
        assert_eq!(shown(&directory, "bob", 11), Some("a2"), "history keeps it");
        assert_eq!(shown(&directory, "bob", 12), None);
        directory.set(A, "bob", Some("new"), 15);
        assert_eq!(shown(&directory, "bob", 11), Some("a2"));
        assert_eq!(shown(&directory, "bob", 13), None, "gap: nobody known");
        assert_eq!(shown(&directory, "bob", 15), Some("new"));
    }

    #[test]
    fn nickname_changes_move_the_avatar_and_free_the_old_name() {
        let mut directory = AvatarDirectory::default();
        directory.set(A, "bob", Some("b"), 1);
        directory.set(A, "carol", Some("c"), 1);
        // bob -> robert at 10.
        directory.rename(A, "bob", "robert", 10);
        assert_eq!(shown(&directory, "bob", 5), Some("b"));
        assert_eq!(shown(&directory, "robert", 5), None, "not robert then");
        assert_eq!(shown(&directory, "robert", 10), Some("b"));
        assert_eq!(directory.current(A, "bob"), None);
        // dave (no avatar) takes the name carol after carol renamed away.
        directory.rename(A, "carol", "caz", 20);
        directory.rename(A, "dave", "carol", 21);
        assert_eq!(shown(&directory, "carol", 15), Some("c"));
        assert_eq!(shown(&directory, "carol", 21), None);
        assert_eq!(directory.current(A, "caz").map(|a| &**a), Some("c"));
    }

    #[test]
    fn ending_all_keeps_history_and_removal_forgets_the_network() {
        let mut directory = AvatarDirectory::default();
        directory.set(A, "bob", Some("b"), 1);
        directory.set(B, "bob", Some("other"), 1);
        directory.end_all(A, 7);
        assert_eq!(directory.current(A, "bob"), None);
        assert_eq!(shown(&directory, "bob", 6), Some("b"));
        assert_eq!(shown(&directory, "bob", 7), None, "reconnected: unknown");
        assert_eq!(directory.len(B), (1, 0), "other server untouched");
        directory.remove_network(A);
        assert_eq!(directory.len(A), (0, 0));
        directory.remove_network(B);
        assert!(directory.is_empty());
        // Removing an avatar that no message used leaves nothing behind.
        directory.set(A, "x", Some("x"), 3);
        directory.set(A, "x", None, 3);
        assert!(directory.is_empty());
    }

    #[test]
    fn tables_are_bounded() {
        let mut directory = AvatarDirectory::default();
        for n in 0..MAX_CURRENT_AVATARS + 100 {
            directory.set(A, &format!("u{n}"), Some("a"), 1);
        }
        assert_eq!(directory.len(A).0, MAX_CURRENT_AVATARS);
        for (n, next) in (0..MAX_CURRENT_AVATARS).zip(2..) {
            directory.set(A, &format!("u{n}"), None, next);
        }
        let (current, retired) = directory.len(A);
        assert_eq!(current, 0);
        assert!(retired <= MAX_RETIRED_AVATARS, "{retired}");
        // The most recently ended are the ones kept.
        let last = format!("u{}", MAX_CURRENT_AVATARS - 1);
        assert_eq!(shown(&directory, &last, 1), Some("a"));
        assert_eq!(shown(&directory, "u0", 1), None);
    }
}
