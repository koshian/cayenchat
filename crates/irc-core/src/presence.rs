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
use std::sync::Arc;

use cayenchat_model::names::CaseMapping;

use crate::text::nickname_key;

/// One user's presence on this connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct UserId(u64);

struct Presence {
    key: String,
    /// The nickname as the last roster or NICK spelled it.
    nick: String,
    /// Shares the key allocation with [`Members::key`]; a user is in few
    /// channels, so a vector is smaller than a set.
    channels: Vec<Arc<str>>,
}

#[derive(Default)]
struct Members {
    key: Arc<str>,
    name: String,
    users: HashSet<UserId>,
}

#[derive(Default)]
pub(crate) struct PresenceIndex {
    next: u64,
    by_nick: HashMap<String, UserId>,
    users: HashMap<UserId, Presence>,
    channels: HashMap<String, Members>,
    /// How the server compares channel names; nicknames always use RFC 1459.
    casemapping: CaseMapping,
}

impl PresenceIndex {
    fn channel_key(&self, channel: &str) -> String {
        self.casemapping.fold(channel)
    }

    /// Compares channel names with `casemapping` from now on, re-keying the
    /// channels held. Channels that fold alike under it become one.
    pub(crate) fn set_casemapping(&mut self, casemapping: CaseMapping) {
        if self.casemapping == casemapping {
            return;
        }
        self.casemapping = casemapping;
        let mut moved: HashMap<Arc<str>, Arc<str>> = HashMap::new();
        let mut channels: HashMap<String, Members> = HashMap::new();
        for (_, members) in std::mem::take(&mut self.channels) {
            let key = casemapping.fold(&members.name);
            let merged = channels.entry(key.clone()).or_default();
            if merged.key.is_empty() {
                merged.key = Arc::from(key.as_str());
                merged.name.clone_from(&members.name);
            }
            moved.insert(members.key, merged.key.clone());
            merged.users.extend(members.users);
        }
        self.channels = channels;
        for presence in self.users.values_mut() {
            let mut kept: Vec<Arc<str>> = Vec::new();
            for old in &presence.channels {
                if let Some(new) = moved.get(old)
                    && !kept.contains(new)
                {
                    kept.push(new.clone());
                }
            }
            presence.channels = kept;
        }
    }

    pub(crate) fn user(&self, nickname: &str) -> Option<UserId> {
        self.by_nick.get(&nickname_key(nickname)).copied()
    }

    /// Whether `nickname` is in `channel`.
    pub(crate) fn in_channel(&self, channel: &str, nickname: &str) -> bool {
        self.user(nickname).is_some_and(|id| {
            self.channels
                .get(&self.channel_key(channel))
                .is_some_and(|members| members.users.contains(&id))
        })
    }

    /// The case-mapped keys of the channels holding `nickname`.
    pub(crate) fn channel_keys_of(&self, nickname: &str) -> Vec<String> {
        self.user(nickname)
            .and_then(|id| self.users.get(&id))
            .into_iter()
            .flat_map(|presence| &presence.channels)
            .map(|key| key.to_string())
            .collect()
    }

    /// The users (as spelled) in `channel` and in no other channel.
    pub(crate) fn only_in(&self, channel: &str) -> Vec<String> {
        let key = self.channel_key(channel);
        let mut only: Vec<String> = self
            .channels
            .get(&key)
            .into_iter()
            .flat_map(|members| &members.users)
            .filter_map(|id| self.users.get(id))
            .filter(|presence| presence.channels.len() == 1)
            .map(|presence| presence.nick.clone())
            .collect();
        only.sort_unstable();
        only
    }

    /// Whether `nickname` is in a channel other than `except`.
    pub(crate) fn shares(&self, nickname: &str, except: Option<&str>) -> bool {
        let except = except.map(|channel| self.channel_key(channel));
        self.user(nickname)
            .and_then(|id| self.users.get(&id))
            .is_some_and(|presence| {
                presence
                    .channels
                    .iter()
                    .any(|channel| Some(&**channel) != except.as_deref())
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
            .filter_map(|key| self.channels.get(&**key))
            .map(|members| members.name.as_str())
            .collect();
        channels.sort_unstable();
        channels
    }

    /// Replaces the membership of `channel` with `members` (nicknames,
    /// possibly with rank prefixes). Users already known keep their id.
    pub(crate) fn replace_channel(&mut self, channel: &str, members: &[String]) {
        let channel_key = self.channel_key(channel);
        let mut present = HashSet::new();
        for member in members {
            let nickname = member.trim_start_matches(['~', '&', '@', '%', '+']);
            if nickname.is_empty() {
                continue;
            }
            let key = nickname_key(nickname);
            let id = match self.by_nick.get(&key) {
                Some(id) => {
                    if let Some(presence) = self.users.get_mut(id)
                        && presence.nick != nickname
                    {
                        presence.nick = nickname.to_owned();
                    }
                    *id
                }
                None => {
                    let id = UserId(self.next);
                    self.next += 1;
                    self.by_nick.insert(key.clone(), id);
                    self.users.insert(
                        id,
                        Presence {
                            key,
                            nick: nickname.to_owned(),
                            channels: Vec::new(),
                        },
                    );
                    id
                }
            };
            present.insert(id);
        }
        let entry = self.channels.entry(channel_key.clone()).or_default();
        entry.name = channel.to_owned();
        if entry.key.is_empty() {
            entry.key = Arc::from(channel_key.as_str());
        }
        let key = entry.key.clone();
        let departed: Vec<UserId> = entry.users.difference(&present).copied().collect();
        for id in &present {
            if let Some(presence) = self.users.get_mut(id)
                && !presence.channels.contains(&key)
            {
                presence.channels.push(key.clone());
            }
        }
        entry.users = present;
        for id in departed {
            self.leave(id, &channel_key);
        }
    }

    /// Forgets `channel` and everybody only there (we left it).
    pub(crate) fn remove_channel(&mut self, channel: &str) {
        let channel_key = self.channel_key(channel);
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
        let key = nickname_key(new);
        // A stale holder of the new name (it quit unseen) is another presence.
        // A case-only change maps to the same key and keeps this presence.
        if let Some(stale) = self.by_nick.remove(&key)
            && stale != id
        {
            self.forget(stale);
        }
        self.by_nick.insert(key.clone(), id);
        if let Some(presence) = self.users.get_mut(&id) {
            presence.key = key;
            presence.nick = new.to_owned();
        }
    }

    /// Builds the index the published `rosters` describe.
    #[cfg(test)]
    pub(crate) fn from_rosters(rosters: &HashMap<String, Vec<String>>) -> Self {
        let mut index = Self::default();
        for (channel, members) in rosters {
            index.replace_channel(channel, members);
        }
        index
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
                if let Some(members) = self.channels.get_mut(&*channel) {
                    members.users.remove(&id);
                }
            }
        }
    }

    fn leave(&mut self, id: UserId, channel_key: &str) {
        let Some(presence) = self.users.get_mut(&id) else {
            return;
        };
        presence
            .channels
            .retain(|channel| &**channel != channel_key);
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
                        .get(&**channel)
                        .is_some_and(|members| members.users.contains(id))
                })
        }) && self.by_nick.len() == self.users.len()
            && self.channels.iter().all(|(_, members)| {
                members.users.iter().all(|id| {
                    self.users.get(id).is_some_and(|presence| {
                        presence
                            .channels
                            .iter()
                            .any(|c| **c == *self.channel_key(&members.name))
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
    fn channels_the_server_keeps_apart_have_their_own_members() {
        let mut index = PresenceIndex::default();
        index.set_casemapping(CaseMapping::Ascii);
        index.replace_channel("#foo[1]", &names(&["bob"]));
        index.replace_channel("#foo{1}", &names(&["carol"]));
        assert!(index.in_channel("#foo[1]", "bob"));
        assert!(!index.in_channel("#foo{1}", "bob"));
        assert!(index.consistent());
        index.remove_channel("#foo{1}");
        assert!(index.in_channel("#foo[1]", "bob"));
        assert!(index.user("carol").is_none());

        // A mapping that folds them together merges what is held.
        index.replace_channel("#foo{1}", &names(&["carol"]));
        index.set_casemapping(CaseMapping::Rfc1459);
        assert!(index.in_channel("#FOO{1}", "bob"));
        assert!(index.in_channel("#foo[1]", "carol"));
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
    fn case_only_rename_keeps_the_presence() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["bob"]));
        index.replace_channel("#b", &names(&["bob"]));
        let bob = index.user("bob");
        index.rename("bob", "BOB");
        assert_eq!(index.user("BOB"), bob);
        assert!(bob.is_some());
        assert_eq!(index.channels_of("BOB").len(), 2);
        assert!(index.consistent());
    }

    #[test]
    fn sole_members_and_channel_keys_follow_the_channels() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#Room", &names(&["@me", "Bob", "carol"]));
        index.replace_channel("#b", &names(&["me", "carol"]));
        assert_eq!(index.only_in("#ROOM"), ["Bob"]);
        assert_eq!(index.only_in("#b"), Vec::<String>::new());
        assert_eq!(index.channel_keys_of("CAROL").len(), 2);
        assert!(index.channel_keys_of("zed").is_empty());
        index.rename("Bob", "Robert");
        assert_eq!(index.only_in("#room"), ["Robert"]);
    }

    #[test]
    fn empty_entries_from_padded_names_are_skipped() {
        let mut index = PresenceIndex::default();
        index.replace_channel("#a", &names(&["@", "bob"]));
        assert!(index.user("").is_none());
        assert!(index.consistent());
    }
}

/// Cost of the index against scanning the published rosters. Ignored; run with
/// `cargo test --release -p cayenchat-irc-core presence_cost -- --ignored --nocapture`.
#[cfg(test)]
mod cost {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    struct Counting;
    static LIVE: AtomicUsize = AtomicUsize::new(0);

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    fn live() -> usize {
        LIVE.load(Ordering::Relaxed)
    }

    fn scan(rosters: &HashMap<String, Vec<String>>, channel: &str, nick: &str) -> bool {
        rosters.get(channel).is_some_and(|members| {
            members.iter().any(|member| {
                member
                    .trim_start_matches(['~', '&', '@', '%', '+'])
                    .eq_ignore_ascii_case(nick)
            })
        })
    }

    #[test]
    #[ignore]
    fn presence_cost() {
        // (channels, members each, distinct users): overlap grows as users shrink.
        for (channels, members, distinct) in [
            (10, 50, 500),
            (10, 50, 100),
            (20, 5_000, 100_000),
            (20, 5_000, 20_000),
        ] {
            let before = live();
            let mut rosters: HashMap<String, Vec<String>> = HashMap::new();
            for c in 0..channels {
                let list = (0..members)
                    .map(|m| {
                        let user = (c * 7919 + m * 31) % distinct;
                        format!("{}user{user}", if m % 10 == 0 { "@" } else { "" })
                    })
                    .collect();
                rosters.insert(format!("#perf{c:02}"), list);
            }
            let roster_bytes = live() - before;

            let before = live();
            let mut index = PresenceIndex::default();
            let start = Instant::now();
            for (name, list) in &rosters {
                index.replace_channel(name, list);
            }
            let first = start.elapsed();
            let index_bytes = live() - before;
            let start = Instant::now();
            for (name, list) in &rosters {
                index.replace_channel(name, list);
            }
            let again = start.elapsed();

            let probes: Vec<String> = (0..200)
                .map(|i| format!("user{}", i * 13 % distinct))
                .collect();
            let names: Vec<&String> = rosters.keys().collect();
            let start = Instant::now();
            let mut hits = 0;
            for nick in &probes {
                for name in &names {
                    hits += usize::from(scan(&rosters, name, nick));
                }
            }
            let scan_time = start.elapsed() / probes.len() as u32;
            let start = Instant::now();
            let mut hits_index = 0;
            for nick in &probes {
                for name in &names {
                    hits_index += usize::from(index.in_channel(name, nick));
                }
            }
            let index_time = start.elapsed() / probes.len() as u32;
            assert_eq!(hits, hits_index);

            // Cost of NICK and QUIT in the index alone.
            let start = Instant::now();
            for nick in &probes {
                index.rename(nick, &format!("{nick}_"));
            }
            let rename = start.elapsed() / probes.len() as u32;
            let start = Instant::now();
            for nick in &probes {
                index.quit(&format!("{nick}_"));
            }
            let quit = start.elapsed() / probes.len() as u32;
            assert!(index.consistent());

            println!(
                "{channels}x{members} users={distinct} roster={} KiB index={} KiB \
                 first={first:?} reapply={again:?} \
                 lookup(all channels): scan={scan_time:?} index={index_time:?} \
                 rename={rename:?} quit={quit:?}",
                roster_bytes / 1024,
                index_bytes / 1024,
            );
        }
    }
}
