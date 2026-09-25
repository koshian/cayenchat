//! Small, UI-independent domain types. These are not IRC wire types.

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

#[derive(Clone, Debug)]
pub struct Message {
    /// Display-only mock time, not a protocol timestamp.
    pub time: String,
    /// Monotonic arrival order for combining messages from different channels.
    pub sequence: u64,
    pub sender: String,
    pub text: String,
}
