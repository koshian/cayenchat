// SPDX-License-Identifier: MPL-2.0
//! Which CTCP AVATAR picture belongs to which user mask (D058).
//!
//! CTCP AVATAR is a stop-gap for servers without IRCv3 metadata, so its
//! pictures are not tied to a nickname's occupancy but to a mask of the
//! form `nick!user@host` with the user and host loosened the way KVIrc
//! does it, per network. The same mask seen again (after a reconnect, or
//! a rejoin) gets the picture without asking. This is written from the
//! rules in the issue, not copied from KVIrc (D042).
//!
//! The table is kept in memory only: it holds other people's data, which
//! is not put on disk. It is shared with the connection's worker through
//! [`PeerAvatarCache`], so it survives reconnects of the same network.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr},
    sync::{Arc, Mutex, PoisonError},
};

use crate::text::nickname_key;

/// Masks remembered per network; the least recently used go first.
pub const MAX_REMEMBERED_MASKS: usize = 512;

/// The key of a user: the case-mapped nickname, the username without
/// ident marks, and the loosened host, all case-insensitive.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AvatarMask(String);

impl AvatarMask {
    pub(crate) fn new(nick: &str, user: &str, host: &str) -> Self {
        let user = user
            .trim_start_matches(['~', '^', '=', '+', '-'])
            .to_lowercase();
        Self(format!(
            "{}!{}@{}",
            nickname_key(nick),
            user,
            loosen_host(host)
        ))
    }

    #[cfg(test)]
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// `host1.example.org` → `*.example.org`; IPv4 keeps two numbers, IPv6 the
/// first four groups (a /64); a cloak (with `/`) and short names stay.
fn loosen_host(host: &str) -> String {
    let host = host.to_lowercase();
    if host.contains('/') {
        return host;
    }
    if let Ok(address) = host.parse::<Ipv4Addr>() {
        let [a, b, ..] = address.octets();
        return format!("{a}.{b}.*");
    }
    if let Ok(address) = host.parse::<Ipv6Addr>() {
        let [a, b, c, d, ..] = address.segments();
        return format!("{a:x}:{b:x}:{c:x}:{d:x}:*");
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() >= 3 {
        format!("*.{}", labels[labels.len() - 2..].join("."))
    } else {
        host
    }
}

#[derive(Debug, Default)]
struct Table {
    entries: HashMap<AvatarMask, (String, u64)>,
    clock: u64,
}

/// The remembered pictures of one network, shared between the application
/// (which keeps it across connections) and the connection's worker.
#[derive(Clone, Debug, Default)]
pub struct PeerAvatarCache(Arc<Mutex<Table>>);

/// The cache is state, not a setting: it never makes two connection
/// configurations differ.
impl PartialEq for PeerAvatarCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for PeerAvatarCache {}

impl PeerAvatarCache {
    fn table(&self) -> std::sync::MutexGuard<'_, Table> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The picture remembered for `mask`, which counts as used now.
    pub(crate) fn get(&self, mask: &AvatarMask) -> Option<String> {
        let mut table = self.table();
        table.clock += 1;
        let clock = table.clock;
        let (url, used) = table.entries.get_mut(mask)?;
        *used = clock;
        Some(url.clone())
    }

    /// Remembers `url` for `mask`; `None` forgets it.
    pub(crate) fn set(&self, mask: &AvatarMask, url: Option<&str>) {
        let mut table = self.table();
        let Some(url) = url else {
            table.entries.remove(mask);
            return;
        };
        table.clock += 1;
        let clock = table.clock;
        if !table.entries.contains_key(mask) && table.entries.len() >= MAX_REMEMBERED_MASKS {
            let oldest = table
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(mask, _)| mask.clone());
            if let Some(oldest) = oldest {
                table.entries.remove(&oldest);
            }
        }
        table.entries.insert(mask.clone(), (url.to_owned(), clock));
    }

    /// Masks remembered now.
    pub fn len(&self) -> usize {
        self.table().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(nick: &str, user: &str, host: &str) -> String {
        AvatarMask::new(nick, user, host).as_str().to_owned()
    }

    #[test]
    fn hosts_are_loosened_like_kvirc() {
        assert_eq!(mask("Bob", "u", "host1.example.org"), "bob!u@*.example.org");
        assert_eq!(mask("bob", "u", "a.b.example.org"), "bob!u@*.example.org");
        assert_eq!(mask("bob", "u", "example.org"), "bob!u@example.org");
        assert_eq!(mask("bob", "u", "localhost"), "bob!u@localhost");
        assert_eq!(mask("bob", "u", "203.0.113.5"), "bob!u@203.0.*");
        assert_eq!(
            mask("bob", "u", "2001:db8:1:2:3:4:5:6"),
            "bob!u@2001:db8:1:2:*"
        );
        assert_eq!(mask("bob", "u", "2001:DB8::1"), "bob!u@2001:db8:0:0:*");
        assert_eq!(mask("bob", "u", "user/Alice"), "bob!u@user/alice");
        assert_eq!(mask("bob", "u", "HOST.Example.ORG"), "bob!u@*.example.org");
    }

    #[test]
    fn ident_marks_and_case_do_not_matter() {
        let plain = mask("bob", "alice", "a.example.org");
        for user in ["~alice", "^Alice", "=alice", "+ALICE", "-alice", "~-alice"] {
            assert_eq!(mask("BOB", user, "z.example.org"), plain, "{user}");
        }
        assert_ne!(
            mask("bob_", "alice", "a.example.org"),
            plain,
            "the nick counts"
        );
        assert_ne!(mask("bob", "carol", "a.example.org"), plain);
        // RFC 1459 case mapping of the nickname.
        assert_eq!(mask("a[b", "u", "h"), mask("A{B", "u", "h"));
    }

    #[test]
    fn the_cache_forgets_the_least_recently_used_and_stays_bounded() {
        let cache = PeerAvatarCache::default();
        let mask = |n: usize| AvatarMask::new(&format!("n{n}"), "u", "h");
        for n in 0..MAX_REMEMBERED_MASKS {
            cache.set(&mask(n), Some("a"));
        }
        // n0 is used, so n1 is the oldest now.
        assert!(cache.get(&mask(0)).is_some());
        cache.set(&mask(MAX_REMEMBERED_MASKS), Some("b"));
        assert_eq!(cache.len(), MAX_REMEMBERED_MASKS);
        assert!(cache.get(&mask(0)).is_some());
        assert!(cache.get(&mask(1)).is_none());
        assert_eq!(cache.get(&mask(MAX_REMEMBERED_MASKS)).as_deref(), Some("b"));
        // Updating needs no room; forgetting removes.
        cache.set(&mask(2), Some("c"));
        assert_eq!(cache.len(), MAX_REMEMBERED_MASKS);
        cache.set(&mask(2), None);
        assert_eq!(cache.len(), MAX_REMEMBERED_MASKS - 1);
        for n in 0..10_000 {
            cache.set(&mask(n + 10_000), Some("x"));
        }
        assert_eq!(cache.len(), MAX_REMEMBERED_MASKS);
    }
}
