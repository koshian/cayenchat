//! What a protocol adapter supplies with each timeline item besides its
//! content, and the bounded duplicate filter that lets history deliveries
//! overlap without showing a message twice.
//!
//! Nothing here knows a protocol. The IRC adapter fills [`MessageMeta`] from
//! `server-time`, `msgid` and its history classification; another backend
//! would fill it from its own event metadata.

use std::{
    collections::{HashSet, VecDeque},
    hash::{BuildHasher, Hash, Hasher, RandomState},
    time::SystemTime,
};

use cayenchat_model::{Message, NativeMessageId, Provenance, Timestamp};

/// Keys remembered per conversation. History deliveries overlap by at most a
/// few hundred lines (a bouncer's join playback, one history request), so
/// this covers them while staying small: about 10 KiB per conversation that
/// receives identifiable messages, and nothing for one that does not.
pub const DUPLICATE_KEYS_PER_CONVERSATION: usize = 512;

/// Text bytes that go into a fallback fingerprint. IRC bodies are one line,
/// but the line codec enforces no length, so hashing is capped; the full
/// length is hashed as well.
const FINGERPRINT_TEXT_BYTES: usize = 512;

/// A timeline item's metadata as its source reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageMeta {
    /// The source's time for the message (IRC: a valid `time` tag on a
    /// connection that negotiated server-time). `None` shows the receipt
    /// time and retains no timestamp.
    pub server_time: Option<SystemTime>,
    /// The source's identifier (IRC: a usable `msgid` tag).
    pub native_id: Option<NativeMessageId>,
    pub provenance: Provenance,
}

impl MessageMeta {
    pub fn live() -> Self {
        Self::default()
    }

    pub fn replayed(replayed: bool) -> Self {
        Self {
            provenance: if replayed {
                Provenance::Replayed
            } else {
                Provenance::Live
            },
            ..Self::default()
        }
    }

    pub fn at(server_time: Option<SystemTime>) -> Self {
        Self {
            server_time,
            ..Self::default()
        }
    }
}

/// One incoming line with its metadata, for callers that add several at
/// once (requested history).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineLine {
    pub sender: String,
    /// As shown, including any notice marker.
    pub text: String,
    pub meta: MessageMeta,
}

/// Remembers the most recent message keys of one conversation, so the same
/// message delivered again (bouncer playback, requested history, reconnect
/// recovery, later a server echo) is recognized. Bounded: the oldest key is
/// forgotten when full, so memory does not grow with the conversation's
/// lifetime, and a repeat older than the window is shown again.
///
/// Keys, strongest first:
///
/// 1. the native identifier when the message has one. Sources promise these
///    are unique, so any message, live or not, is matched by it.
/// 2. otherwise a fingerprint of the source's timestamp (to the
///    millisecond), sender, activity flag and text, but only when the source
///    supplied a timestamp. It is not collision-free: two different lines
///    can share it (the same text from the same sender in one millisecond),
///    so only history is suppressed by it; a live message never is.
///
/// Messages with neither are neither checked nor remembered.
#[derive(Debug, Default)]
pub struct DuplicateFilter {
    order: VecDeque<u64>,
    seen: HashSet<u64>,
    hasher: RandomState,
}

impl DuplicateFilter {
    /// Records `message` and returns whether it is new. A duplicate is not
    /// recorded again.
    pub fn admit(&mut self, message: &Message) -> bool {
        let Some((key, suppress)) = self.key(message) else {
            return true;
        };
        if self.seen.contains(&key) {
            return !suppress;
        }
        if self.order.len() >= DUPLICATE_KEYS_PER_CONVERSATION
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        self.order.push_back(key);
        self.seen.insert(key);
        true
    }

    /// Whether [`DuplicateFilter::admit`] would drop `message`, without
    /// recording it.
    pub fn contains(&self, message: &Message) -> bool {
        self.key(message)
            .is_some_and(|(key, suppress)| suppress && self.seen.contains(&key))
    }

    /// The message's key and whether a match suppresses it.
    fn key(&self, message: &Message) -> Option<(u64, bool)> {
        match (&message.native_id, message.timestamp) {
            (Some(id), _) => Some((self.native_key(id), true)),
            (None, Some(timestamp)) => {
                Some((self.fingerprint(timestamp, message), message.is_history()))
            }
            (None, None) => None,
        }
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    fn native_key(&self, id: &NativeMessageId) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        0u8.hash(&mut hasher);
        id.as_str().hash(&mut hasher);
        hasher.finish()
    }

    fn fingerprint(&self, timestamp: Timestamp, message: &Message) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        1u8.hash(&mut hasher);
        timestamp.hash(&mut hasher);
        message.activity.hash(&mut hasher);
        message.sender.hash(&mut hasher);
        let text = message.text.as_bytes();
        text.len().hash(&mut hasher);
        hasher.write(&text[..text.len().min(FINGERPRINT_TEXT_BYTES)]);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_model::TimeOfDay;

    fn message(
        native_id: Option<&str>,
        millis: Option<u64>,
        text: &str,
        provenance: Provenance,
    ) -> Message {
        Message {
            time: TimeOfDay::new(0, 0),
            sequence: 0,
            timestamp: millis.map(Timestamp::from_millis),
            native_id: native_id.and_then(NativeMessageId::new),
            sender: "alice".into(),
            text: text.into(),
            activity: false,
            provenance,
            delivery_failed: false,
        }
    }

    #[test]
    fn native_ids_match_across_provenances() {
        let mut filter = DuplicateFilter::default();
        assert!(filter.admit(&message(Some("a1"), Some(1), "hi", Provenance::Live)));
        // History copies of a live line, with or without a timestamp.
        assert!(!filter.admit(&message(Some("a1"), Some(1), "hi", Provenance::Requested)));
        assert!(!filter.admit(&message(Some("a1"), None, "hi", Provenance::Replayed)));
        // The identifier alone decides; case matters.
        assert!(filter.admit(&message(Some("A1"), Some(1), "hi", Provenance::Requested)));
        assert!(filter.admit(&message(Some("a2"), Some(1), "hi", Provenance::Requested)));
        assert_eq!(filter.len(), 3);
    }

    #[test]
    fn fingerprints_suppress_only_history_and_need_a_timestamp() {
        let mut filter = DuplicateFilter::default();
        assert!(filter.admit(&message(None, Some(5), "ok", Provenance::Replayed)));
        assert!(!filter.admit(&message(None, Some(5), "ok", Provenance::Requested)));
        // A live line with the same fingerprint is someone repeating themselves.
        assert!(filter.admit(&message(None, Some(5), "ok", Provenance::Live)));
        // Another millisecond, text or sender is another message.
        assert!(filter.admit(&message(None, Some(6), "ok", Provenance::Requested)));
        assert!(filter.admit(&message(None, Some(5), "ok!", Provenance::Requested)));
        let mut other = message(None, Some(5), "ok", Provenance::Requested);
        other.sender = "bob".into();
        assert!(filter.admit(&other));
        // Without a timestamp nothing identifies a line: always shown, never kept.
        let before = filter.len();
        assert!(filter.admit(&message(None, None, "ok", Provenance::Replayed)));
        assert!(filter.admit(&message(None, None, "ok", Provenance::Replayed)));
        assert_eq!(filter.len(), before);
    }

    #[test]
    fn contains_checks_without_recording() {
        let mut filter = DuplicateFilter::default();
        filter.admit(&message(Some("a1"), Some(1), "hi", Provenance::Live));
        filter.admit(&message(None, Some(2), "ok", Provenance::Live));
        assert!(filter.contains(&message(Some("a1"), None, "x", Provenance::Requested)));
        assert!(filter.contains(&message(None, Some(2), "ok", Provenance::Requested)));
        // A fingerprint never matches a live line, and unknown keys stay unknown.
        assert!(!filter.contains(&message(None, Some(2), "ok", Provenance::Live)));
        assert!(!filter.contains(&message(Some("a2"), None, "x", Provenance::Requested)));
        assert_eq!(filter.len(), 2);
    }

    #[test]
    fn long_texts_are_fingerprinted_from_a_bounded_prefix_and_their_length() {
        let mut filter = DuplicateFilter::default();
        let long = "x".repeat(FINGERPRINT_TEXT_BYTES * 4);
        assert!(filter.admit(&message(None, Some(1), &long, Provenance::Replayed)));
        assert!(!filter.admit(&message(None, Some(1), &long, Provenance::Replayed)));
        // Same prefix, different length.
        let longer = format!("{long}y");
        assert!(filter.admit(&message(None, Some(1), &longer, Provenance::Replayed)));
    }

    #[test]
    fn the_window_is_bounded_and_forgets_the_oldest_keys() {
        let mut filter = DuplicateFilter::default();
        let total = DUPLICATE_KEYS_PER_CONVERSATION + 100;
        for index in 0..total {
            let id = format!("m{index}");
            assert!(filter.admit(&message(Some(&id), None, "x", Provenance::Live)));
        }
        assert_eq!(filter.len(), DUPLICATE_KEYS_PER_CONVERSATION);
        assert_eq!(filter.seen.len(), DUPLICATE_KEYS_PER_CONVERSATION);
        // The oldest 100 were forgotten, the rest are still recognized.
        assert!(filter.admit(&message(Some("m0"), None, "x", Provenance::Replayed)));
        let newest = format!("m{}", total - 1);
        assert!(!filter.admit(&message(Some(&newest), None, "x", Provenance::Replayed)));
        assert_eq!(filter.len(), DUPLICATE_KEYS_PER_CONVERSATION);
    }
}
