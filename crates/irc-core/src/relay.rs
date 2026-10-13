// SPDX-License-Identifier: MPL-2.0
//! How much of a 512-byte line the server takes for itself when it relays our
//! `PRIVMSG` or `NOTICE`.
//!
//! The server puts `:<nick>!<user>@<host> ` in front of the line, so the text
//! that reaches the other side fits less than the command we send; whatever
//! does not fit is cut off silently. The worker tracks our own `user@host` and
//! publishes the prefix size to the connection handle, which checks messages
//! before they are queued.

use std::sync::atomic::{AtomicUsize, Ordering};

use irc::proto::{Command, Message, Prefix, Response};

/// A username's size assumed until the server shows ours: the usual limit
/// (`USERLEN`) of 10.
const ASSUMED_USER_BYTES: usize = 10;
/// A hostname's size assumed until the server shows ours: the 63 bytes of a
/// DNS label, which is also what most servers cut a host to.
const ASSUMED_HOST_BYTES: usize = 63;
/// Longest username or host taken from the server; longer values are not
/// believed.
const MAX_MASK_PART_BYTES: usize = 255;

/// Our `user` and `host` as the server relays them; `None` while unknown.
#[derive(Debug, Default, Clone)]
pub struct OwnMask {
    user: Option<String>,
    host: Option<String>,
}

impl OwnMask {
    /// Learns from `message` (already parsed, ours or not) what our
    /// `user@host` is: our own JOIN, echo or any other line with our mask, the
    /// 001 welcome text, CHGHOST and RPL_HOSTHIDDEN. Returns whether the mask
    /// changed.
    pub fn observe(&mut self, message: &Message, current_nick: &str) -> bool {
        let own = message.source_nickname() == Some(current_nick);
        match &message.command {
            Command::CHGHOST(user, host) if own => self.set_mask(user, host),
            Command::Response(Response::RPL_WELCOME, args) => {
                let mask = args
                    .last()
                    .and_then(|text| text.split_whitespace().next_back())
                    .and_then(|word| word.split_once('!'))
                    .filter(|(nick, _)| args.first().map(String::as_str) == Some(*nick));
                match mask.and_then(|(_, user_host)| user_host.split_once('@')) {
                    Some((user, host)) => self.set_mask(user, host),
                    None => false,
                }
            }
            Command::Response(Response::RPL_HOSTHIDDEN, args) => {
                match (args.first(), args.get(1)) {
                    (Some(nick), Some(host)) if nick == current_nick => self.set_host(host),
                    _ => false,
                }
            }
            _ => match &message.prefix {
                Some(Prefix::Nickname(nick, user, host)) if nick == current_nick => {
                    self.set_mask(user, host)
                }
                _ => false,
            },
        }
    }

    /// Takes both parts or neither. Returns whether anything changed.
    fn set_mask(&mut self, user: &str, host: &str) -> bool {
        if !(plausible(user) && plausible(host)) {
            return false;
        }
        let changed = self.user.as_deref() != Some(user) || self.host.as_deref() != Some(host);
        if changed {
            self.user = Some(user.to_owned());
            self.host = Some(host.to_owned());
        }
        changed
    }

    fn set_host(&mut self, host: &str) -> bool {
        if !plausible(host) || self.host.as_deref() == Some(host) {
            return false;
        }
        self.host = Some(host.to_owned());
        true
    }

    /// Bytes the server adds in front of a line we send: `:`, our
    /// `nick!user@host` and a space. Parts not yet known count at the size
    /// that is usually the most.
    pub fn prefix_bytes(&self, nick: &str, encoding: &str) -> usize {
        let user = self
            .user
            .as_deref()
            .map_or(ASSUMED_USER_BYTES, |user| wire_len(user, encoding));
        let host = self
            .host
            .as_deref()
            .map_or(ASSUMED_HOST_BYTES, |host| wire_len(host, encoding));
        // ":" + nick + "!" + user + "@" + host + " "
        4 + wire_len(nick, encoding) + user + host
    }
}

fn plausible(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= MAX_MASK_PART_BYTES
        && !part.contains(|c: char| c.is_whitespace() || c.is_control() || c == '!' || c == '@')
}

/// The size of `text` in the connection's encoding; its UTF-8 size when the
/// label is unknown or the text cannot be encoded (sending fails then anyway).
fn wire_len(text: &str, encoding: &str) -> usize {
    crate::encode_strict(text, encoding).map_or(text.len(), |bytes| bytes.len())
}

/// The prefix size the connection handle reads.
pub fn publish(shared: &AtomicUsize, bytes: usize) {
    shared.store(bytes, Ordering::Relaxed);
}

pub fn published(shared: &AtomicUsize) -> usize {
    shared.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Message {
        line.parse().unwrap()
    }

    #[test]
    fn unknown_mask_is_counted_generously() {
        let mask = OwnMask::default();
        assert_eq!(mask.prefix_bytes("alice", "UTF-8"), 4 + 5 + 10 + 63);
    }

    #[test]
    fn own_join_gives_the_mask() {
        let mut mask = OwnMask::default();
        assert!(mask.observe(&parse(":alice!~al@host.example JOIN #c"), "alice"));
        assert_eq!(mask.prefix_bytes("alice", "UTF-8"), 4 + 5 + 3 + 12);
        // Someone else's mask is not ours.
        assert!(!mask.observe(&parse(":bob!bob@elsewhere.example JOIN #c"), "alice"));
    }

    #[test]
    fn welcome_text_gives_the_mask() {
        let mut mask = OwnMask::default();
        let welcome = parse(":srv 001 alice :Welcome to the Network alice!u@h.example");
        assert!(mask.observe(&welcome, "alice"));
        assert_eq!(mask.prefix_bytes("alice", "UTF-8"), 4 + 5 + 1 + 9);
        let mut plain = OwnMask::default();
        assert!(!plain.observe(&parse(":srv 001 alice :Welcome alice"), "alice"));
    }

    #[test]
    fn host_changes_are_followed() {
        let mut mask = OwnMask::default();
        mask.observe(&parse(":alice!u@old.example JOIN #c"), "alice");
        assert!(mask.observe(
            &parse(":alice!u@old.example CHGHOST newuser new.example"),
            "alice"
        ));
        assert_eq!(mask.prefix_bytes("alice", "UTF-8"), 4 + 5 + 7 + 11);
        assert!(mask.observe(
            &parse(":srv 396 alice cloak/x :is now your displayed host"),
            "alice"
        ));
        assert_eq!(mask.prefix_bytes("alice", "UTF-8"), 4 + 5 + 7 + 7);
        // Another user's CHGHOST changes nothing.
        assert!(!mask.observe(&parse(":bob!b@h CHGHOST b other.example"), "alice"));
    }

    #[test]
    fn implausible_values_are_ignored() {
        let mut mask = OwnMask::default();
        let long = "h".repeat(300);
        assert!(!mask.observe(&parse(&format!(":alice!u@{long} JOIN #c")), "alice"));
        assert_eq!(mask.prefix_bytes("alice", "UTF-8"), 4 + 5 + 10 + 63);
    }

    #[test]
    fn nickname_is_counted_in_the_connection_encoding() {
        let mask = OwnMask::default();
        let utf8 = mask.prefix_bytes("あ", "UTF-8");
        let sjis = mask.prefix_bytes("あ", "Shift_JIS");
        assert_eq!(utf8 - sjis, 1);
    }
}
