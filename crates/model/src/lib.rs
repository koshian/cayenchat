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

#[derive(Clone, Debug)]
pub struct Message {
    /// Display-only arrival time, not a protocol timestamp.
    pub time: TimeOfDay,
    /// Monotonic arrival order for combining messages from different channels.
    pub sequence: u64,
    pub sender: String,
    pub text: String,
    pub activity: bool,
}

#[cfg(test)]
mod tests {
    use super::TimeOfDay;

    #[test]
    fn time_of_day_formats_with_two_digit_fields() {
        assert_eq!(TimeOfDay::new(9, 5).to_string(), "09:05");
        assert_eq!(TimeOfDay::new(23, 59).to_string(), "23:59");
    }
}
