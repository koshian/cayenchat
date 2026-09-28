//! Small, UI-independent domain types. These are not IRC wire types.

pub mod attachment;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NetworkId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConversationId(pub u32);

#[derive(Clone, Debug)]
pub struct Network {
    pub id: NetworkId,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Conversation {
    pub id: ConversationId,
    pub network: NetworkId,
    pub name: String,
    pub topic: String,
    pub messages: Vec<Message>,
    /// Display-only mock roster; live membership will arrive as application events.
    pub members: Vec<String>,
}

/// Local wall-clock time a message arrived, to the minute. Stored as a number
/// rather than text: logs keep thousands of messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeOfDay {
    minutes: u16,
}

impl TimeOfDay {
    pub fn new(hour: u8, minute: u8) -> Self {
        Self {
            minutes: u16::from(hour % 24) * 60 + u16::from(minute % 60),
        }
    }
}

impl std::fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.minutes / 60, self.minutes % 60)
    }
}

/// When a message happened according to its source, as milliseconds since
/// the Unix epoch (UTC). Only a source-supplied time is kept here (IRC:
/// server-time); receipt times are not stored, so this can later serve as a
/// history reference. Presentation uses [`TimeOfDay`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    millis: u64,
}

impl Timestamp {
    pub fn from_millis(millis: u64) -> Self {
        Self { millis }
    }

    pub fn as_millis(self) -> u64 {
        self.millis
    }

    /// `None` for instants before 1970 or beyond the representable range.
    pub fn from_system_time(time: std::time::SystemTime) -> Option<Self> {
        let millis = time.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
        Some(Self::from_millis(u64::try_from(millis).ok()?))
    }

    pub fn to_system_time(self) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(self.millis)
    }
}

/// A message's identifier in its source protocol (IRC: the `msgid` tag),
/// opaque and compared exactly. It is only meaningful within the backend and
/// conversation it came from; it is never the application's identity for a
/// message, which is [`Message::sequence`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NativeMessageId(Box<str>);

impl NativeMessageId {
    /// Longest identifier kept. Real identifiers are short (IRC servers use
    /// 20–40 characters); a longer one is not retained.
    pub const MAX_BYTES: usize = 128;

    /// `None` when `id` is empty, too long, or contains anything but visible
    /// ASCII. Such an identifier is treated as absent rather than truncated,
    /// so it can never compare equal to a different message's.
    pub fn new(id: &str) -> Option<Self> {
        (!id.is_empty()
            && id.len() <= Self::MAX_BYTES
            && id.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| Self(id.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a message reached the client. Only live messages may notify or be
/// highlighted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Delivered as it happened.
    #[default]
    Live,
    /// History the server or a bouncer sent by itself (playback on join,
    /// a history batch nobody asked for, a bouncer's own log replay).
    Replayed,
    /// History this client explicitly requested.
    Requested,
}

impl Provenance {
    /// Replayed or requested history rather than a live message.
    pub fn is_history(self) -> bool {
        self != Self::Live
    }
}

/// One retained timeline item, as the logs present it. Protocol adapters
/// fill it; nothing in it is a wire type.
#[derive(Clone, Debug)]
pub struct Message {
    /// Display-only local time of day: the source's time when it supplied
    /// one, otherwise the receipt time.
    pub time: TimeOfDay,
    /// The application's identity for this message, unique within the
    /// running app and never reused, and its order key: messages keep the
    /// order in which they were added (arrival order), which later lines
    /// never change. Logs are never re-sorted by [`Message::timestamp`].
    pub sequence: u64,
    /// The source's own time for this message (IRC: server-time), when it
    /// supplied a valid one.
    pub timestamp: Option<Timestamp>,
    /// The source's own identifier (IRC: `msgid`), when it supplied a
    /// usable one.
    pub native_id: Option<NativeMessageId>,
    pub sender: String,
    pub text: String,
    pub activity: bool,
    pub provenance: Provenance,
}

impl Message {
    /// History replayed by a server or bouncer, or requested by us; never
    /// highlighted or notified.
    pub fn is_history(&self) -> bool {
        self.provenance.is_history()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_of_day_formats_with_two_digit_fields() {
        assert_eq!(TimeOfDay::new(9, 5).to_string(), "09:05");
        assert_eq!(TimeOfDay::new(23, 59).to_string(), "23:59");
    }

    #[test]
    fn timestamps_round_trip_milliseconds_and_reject_pre_epoch_times() {
        use std::time::{Duration, UNIX_EPOCH};
        let time = UNIX_EPOCH + Duration::from_millis(1_319_042_451_620);
        let stamp = Timestamp::from_system_time(time).unwrap();
        assert_eq!(stamp.as_millis(), 1_319_042_451_620);
        assert_eq!(stamp.to_system_time(), time);
        // Sub-millisecond precision is not kept.
        let finer = time + Duration::from_micros(700);
        assert_eq!(Timestamp::from_system_time(finer), Some(stamp));
        assert_eq!(
            Timestamp::from_system_time(UNIX_EPOCH).unwrap().as_millis(),
            0
        );
        assert!(Timestamp::from_system_time(UNIX_EPOCH - Duration::from_secs(1)).is_none());
    }

    #[test]
    fn native_ids_are_opaque_bounded_visible_ascii() {
        assert_eq!(
            NativeMessageId::new("01K6ABCDEF").map(|id| id.as_str().to_owned()),
            Some("01K6ABCDEF".into())
        );
        // Case matters; identifiers are compared exactly.
        assert_ne!(NativeMessageId::new("abc"), NativeMessageId::new("ABC"));
        let longest = "x".repeat(NativeMessageId::MAX_BYTES);
        assert!(NativeMessageId::new(&longest).is_some());
        for unusable in [
            String::new(),
            format!("{longest}x"),
            "has space".into(),
            "tab\tx".into(),
            "caf\u{e9}".into(),
            "bad\u{FFFD}".into(),
            "nul\0".into(),
        ] {
            assert!(NativeMessageId::new(&unusable).is_none(), "{unusable:?}");
        }
    }

    #[test]
    fn provenance_separates_live_messages_from_both_kinds_of_history() {
        assert!(!Provenance::Live.is_history());
        assert!(Provenance::Replayed.is_history());
        assert!(Provenance::Requested.is_history());
        assert_eq!(Provenance::default(), Provenance::Live);
    }

    #[test]
    fn retained_message_size_stays_bounded() {
        // 64 bytes before timestamps and native IDs were retained; the
        // identifier's text lives on the heap only when present.
        println!("size_of::<Message>() = {}", std::mem::size_of::<Message>());
        assert!(std::mem::size_of::<Message>() <= 96);
    }
}
