//! Application state and commands, independent of any rendering framework.
pub mod attachments;
pub mod avatars;
pub mod notifications;
pub mod own_avatar;
pub mod timeline;

use std::{
    collections::{HashMap, HashSet},
    time::SystemTime,
};

use cayenchat_model::{
    Conversation, ConversationId, ConversationKind, Message, NativeMessageId, Network, NetworkId,
    Provenance, TimeOfDay, Timestamp,
};
use timeline::DuplicateFilter;
pub use timeline::MessageMeta;

/// Upper bound on conversations per network, so a hostile server or bouncer
/// cannot grow memory without limit by announcing endless channel joins.
const MAX_CONVERSATIONS_PER_NETWORK: usize = 1_000;
/// Upper bound on private conversations per network (within the limit
/// above), so a flood of messages from new nicknames cannot fill the channel
/// tree; further ones go to the server log.
pub const MAX_PRIVATE_CONVERSATIONS_PER_NETWORK: usize = 100;
/// Sequences set aside when history is requested for a conversation: the
/// most lines one reply can add. They sit between the lines that arrived
/// before the request and those after it, so inserting the reply keeps
/// every log in ascending sequence order.
pub const HISTORY_RESERVE: usize = 256;
/// Lines kept per conversation; the oldest 1,000 go when it is exceeded.
const MAX_RETAINED: usize = 2_000;
/// Lines asked for per older page. The backend lowers it to what its
/// server allows.
pub const OLDER_PAGE_LIMIT: usize = 50;
/// Oldest lines of a conversation that an older page is checked against
/// (and among which its reference is chosen): a page ends where the log
/// begins, so only overlap with the top of the log is possible.
const OLDER_PAGE_OVERLAP_WINDOW: usize = HISTORY_RESERVE;
/// Where message sequences start. Ordinary messages count up from here;
/// older pages, which go before everything a conversation holds, count
/// down from it, so every log stays in ascending sequence order without
/// renumbering what is already shown.
const SEQUENCE_ORIGIN: u64 = 1 << 62;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Selection {
    Server(NetworkId),
    Channel(ConversationId),
    /// No server is configured.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    SelectChannel(ConversationId),
    SelectServer(NetworkId),
    NextUnreadChannel,
    PreviousUnreadChannel,
    PreviousSelectedChannel,
    NextActiveChannel,
    PreviousActiveChannel,
    NextChannel,
    PreviousChannel,
    NextActiveServer,
    PreviousActiveServer,
    NextServer,
    PreviousServer,
    SelectChannelAt(usize),
    SelectServerAt(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    OfflineMock,
    Connecting,
    TransportConnected,
    Registered,
    Disconnected(String),
}

#[derive(Clone, Copy)]
enum ChannelFilter {
    All,
    Active,
    Unread,
}

pub struct AppState {
    networks: Vec<Network>,
    conversations: Vec<Conversation>,
    selected: Selection,
    previous_channel: Option<ConversationId>,
    unread: HashSet<ConversationId>,
    /// Unread channels where someone mentioned us or a keyword appeared.
    highlighted: HashSet<ConversationId>,
    active_channels: HashSet<ConversationId>,
    active_servers: HashSet<NetworkId>,
    statuses: HashMap<NetworkId, ConnectionStatus>,
    server_messages: HashMap<NetworkId, Vec<Message>>,
    next_message_sequence: u64,
    /// Conversation IDs are never reused, so UI state keyed by a removed
    /// conversation cannot attach to a new one.
    next_conversation_id: u32,
    /// User avatars per network, tied to message sequences.
    avatars: avatars::AvatarDirectory,
    /// Recently seen message keys per conversation, created only for
    /// conversations that receive identifiable messages.
    duplicates: HashMap<ConversationId, DuplicateFilter>,
    /// Conversations waiting for requested history, with the first of their
    /// reserved sequences. A reply without an entry is stale and ignored.
    pending_history: HashMap<ConversationId, u64>,
    /// Networks whose backend can page back through history now.
    history_paging: HashSet<NetworkId>,
    /// Older-page state of conversations that asked for one this session.
    older_history: HashMap<ConversationId, OlderHistory>,
    /// Identifies older-page requests; never reused.
    next_history_request: u64,
    /// The lowest sequence given to an older page so far.
    older_sequence_floor: u64,
    /// Channels whose log a disconnect cut off, until the missed lines
    /// arrive or recovery is given up.
    resume_points: HashMap<ConversationId, ResumePoint>,
}

/// Where a disconnect cut a channel's log off: the newest message its
/// source identified, and sequences reserved right after the lines
/// received until then, where the missed lines go.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ResumePoint {
    start: u64,
    native_id: Option<NativeMessageId>,
    timestamp: Option<Timestamp>,
}

/// A conversation to resume on the next connection of its network: its
/// name for the backend (IRC: the channel) and the newest message received
/// before the disconnect, which the missed lines follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryResume {
    pub conversation: ConversationId,
    pub name: String,
    pub native_id: Option<NativeMessageId>,
    pub timestamp: Option<Timestamp>,
}

/// Paging back through one conversation's history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct OlderHistory {
    /// The request being answered.
    in_flight: Option<u64>,
    /// Nothing older can be had this session: the source said so, a page
    /// added nothing new, or a request failed.
    finished: bool,
}

/// One page of a conversation's older history, for its backend to ask for.
/// Backends refer to the page by `request` when they answer
/// ([`AppState::insert_older_history`], [`AppState::older_history_failed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OlderHistoryRequest {
    pub request: u64,
    /// The conversation's oldest message that its source identified: its
    /// native identifier and source time, whichever it has. A backend with
    /// its own pagination state may ignore them.
    pub native_id: Option<NativeMessageId>,
    pub timestamp: Option<Timestamp>,
    /// Lines wanted: [`OLDER_PAGE_LIMIT`], or less when the log is close to
    /// its bound.
    pub limit: usize,
}

/// A server as configured: its display name and auto-join channels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkConfig {
    pub id: NetworkId,
    pub name: String,
    pub channels: Vec<String>,
}

impl AppState {
    pub fn mock() -> Self {
        let networks = vec![
            Network {
                id: NetworkId(1),
                name: "Libera (mock)".into(),
            },
            Network {
                id: NetworkId(2),
                name: "Local friends (mock)".into(),
            },
        ];
        let mut next_message_sequence = SEQUENCE_ORIGIN;
        let conversations: Vec<_> = [
            (
                1,
                1,
                "#general",
                "Welcome — an offline conversation",
                vec![
                    ("09:41", "alice", "Good morning. Welcome to CayenChat."),
                    (
                        "09:42",
                        "bob",
                        "Four panes keep channels, both logs and users in view.",
                    ),
                    (
                        "09:43",
                        "alice",
                        "Try #rust or use Ctrl+Tab to switch channels.",
                    ),
                    (
                        "09:44",
                        "yuki",
                        "こんにちは。これはオフラインのサンプルです。",
                    ),
                ],
                vec!["@alice", "alex", "bob", "yuki", "guest"],
            ),
            (
                2,
                1,
                "#rust",
                "Rust and native desktop tools",
                vec![
                    (
                        "10:02",
                        "ferris",
                        "Application state lives outside the renderer.",
                    ),
                    (
                        "10:03",
                        "alice",
                        "The protocol layer will send events to the application.",
                    ),
                    ("10:04", "bob", "No sockets yet — just a useful mock."),
                ],
                vec!["@ferris", "alice", "bob", "crab"],
            ),
            (
                3,
                2,
                "#general",
                "A different network, a different conversation",
                vec![
                    (
                        "11:10",
                        "mika",
                        "Same channel name, separate conversation ID.",
                    ),
                    ("11:11", "ren", "This is the local friends network."),
                ],
                vec!["@mika", "ren", "sora"],
            ),
            (
                4,
                2,
                "#random",
                "Off-topic",
                vec![
                    ("12:30", "ren", "Anyone up for coffee?"),
                    ("12:31", "mika", "After this build finishes."),
                ],
                vec!["@ren", "mika", "hana"],
            ),
        ]
        .into_iter()
        .map(
            |(id, network, name, topic, messages, members)| Conversation {
                id: ConversationId(id),
                network: NetworkId(network),
                kind: ConversationKind::Channel,
                name: name.into(),
                topic: topic.into(),
                messages: messages
                    .into_iter()
                    .map(|(time, sender, text)| {
                        next_message_sequence += 1;
                        Message {
                            time: mock_time(time),
                            sequence: next_message_sequence,
                            timestamp: None,
                            native_id: None,
                            sender: sender.into(),
                            text: text.into(),
                            activity: false,
                            provenance: Provenance::Live,
                        }
                    })
                    .collect(),
                members: sorted_members(members.into_iter().map(str::to_owned).collect()),
            },
        )
        .collect();
        let active_channels = conversations.iter().map(|channel| channel.id).collect();
        let active_servers = networks.iter().map(|server| server.id).collect();
        let statuses = networks
            .iter()
            .map(|server| (server.id, ConnectionStatus::OfflineMock))
            .collect();
        Self {
            networks,
            conversations,
            selected: Selection::Channel(ConversationId(1)),
            previous_channel: None,
            // Unread fixtures make the shortcut observable before IRC events exist.
            unread: HashSet::from([ConversationId(2), ConversationId(4)]),
            highlighted: HashSet::new(),
            active_channels,
            active_servers,
            statuses,
            server_messages: HashMap::new(),
            next_message_sequence,
            next_conversation_id: 5,
            avatars: avatars::AvatarDirectory::default(),
            duplicates: HashMap::new(),
            pending_history: HashMap::new(),
            history_paging: HashSet::new(),
            older_history: HashMap::new(),
            next_history_request: 0,
            older_sequence_floor: SEQUENCE_ORIGIN,
            resume_points: HashMap::new(),
        }
    }

    pub fn live(host: String, channels: Vec<String>) -> Self {
        let channel_count = channels.len() as u32;
        let network = Network {
            id: NetworkId(1),
            name: host,
        };
        let conversations: Vec<_> = channels
            .into_iter()
            .enumerate()
            .map(|(index, name)| Conversation {
                id: ConversationId(index as u32 + 1),
                network: network.id,
                kind: ConversationKind::Channel,
                name,
                topic: String::new(),
                messages: Vec::new(),
                members: Vec::new(),
            })
            .collect();
        let selected = conversations
            .first()
            .map(|channel| Selection::Channel(channel.id))
            .unwrap_or(Selection::Server(network.id));
        Self {
            statuses: HashMap::from([(network.id, ConnectionStatus::Connecting)]),
            networks: vec![network],
            conversations,
            selected,
            previous_channel: None,
            unread: HashSet::new(),
            highlighted: HashSet::new(),
            active_channels: HashSet::new(),
            active_servers: HashSet::new(),
            server_messages: HashMap::new(),
            next_message_sequence: SEQUENCE_ORIGIN,
            next_conversation_id: channel_count + 1,
            avatars: avatars::AvatarDirectory::default(),
            duplicates: HashMap::new(),
            pending_history: HashMap::new(),
            history_paging: HashSet::new(),
            older_history: HashMap::new(),
            next_history_request: 0,
            older_sequence_floor: SEQUENCE_ORIGIN,
            resume_points: HashMap::new(),
        }
    }

    pub fn configured(host: String, channels: Vec<String>) -> Self {
        let mut state = Self::live(host, channels);
        state.set_status(
            NetworkId(1),
            ConnectionStatus::Disconnected("Not connected.".into()),
        );
        state
    }

    /// Every configured server in display order, none connected yet. The
    /// first configured channel (or the first server, or nothing when there
    /// are no servers) is selected.
    pub fn with_networks(networks: Vec<NetworkConfig>) -> Self {
        let mut state = Self {
            networks: Vec::new(),
            conversations: Vec::new(),
            selected: networks
                .first()
                .map_or(Selection::None, |network| Selection::Server(network.id)),
            previous_channel: None,
            unread: HashSet::new(),
            highlighted: HashSet::new(),
            active_channels: HashSet::new(),
            active_servers: HashSet::new(),
            statuses: HashMap::new(),
            server_messages: HashMap::new(),
            next_message_sequence: SEQUENCE_ORIGIN,
            next_conversation_id: 1,
            avatars: avatars::AvatarDirectory::default(),
            duplicates: HashMap::new(),
            pending_history: HashMap::new(),
            history_paging: HashSet::new(),
            older_history: HashMap::new(),
            next_history_request: 0,
            older_sequence_floor: SEQUENCE_ORIGIN,
            resume_points: HashMap::new(),
        };
        for config in networks {
            state.networks.push(Network {
                id: config.id,
                name: config.name,
            });
            state.statuses.insert(
                config.id,
                ConnectionStatus::Disconnected("Not connected.".into()),
            );
            for channel in config.channels {
                state.ensure_channel(config.id, &channel);
            }
        }
        if let Some(first) = state.conversations.first() {
            state.selected = Selection::Channel(first.id);
        }
        state
    }

    /// Makes the networks match `networks` (display order and names): new
    /// ones are added disconnected with their configured channels, missing
    /// ones are removed with their conversations and logs. Existing networks
    /// keep their conversations. Returns the removed conversation IDs so the
    /// caller can drop state kept for them.
    pub fn sync_networks(&mut self, networks: &[NetworkConfig]) -> Vec<ConversationId> {
        let removed_networks: Vec<NetworkId> = self
            .networks
            .iter()
            .map(|network| network.id)
            .filter(|id| !networks.iter().any(|kept| kept.id == *id))
            .collect();
        let added: Vec<&NetworkConfig> = networks
            .iter()
            .filter(|config| !self.networks.iter().any(|network| network.id == config.id))
            .collect();
        let mut removed = Vec::new();
        for id in removed_networks {
            removed.extend(self.remove_conversations(id));
            self.statuses.remove(&id);
            self.server_messages.remove(&id);
            self.active_servers.remove(&id);
            self.avatars.remove_network(id);
        }
        self.networks = networks
            .iter()
            .map(|config| Network {
                id: config.id,
                name: config.name.clone(),
            })
            .collect();
        for config in networks {
            self.statuses
                .entry(config.id)
                .or_insert_with(|| ConnectionStatus::Disconnected("Not connected.".into()));
        }
        self.sort_conversations();
        for config in added {
            for channel in &config.channels {
                self.ensure_channel(config.id, channel);
            }
        }
        self.repair_selection();
        removed
    }

    /// Starts a fresh session on `network`: its conversations are replaced
    /// by the configured `channels` with empty logs, its server log is
    /// cleared and its status becomes connecting. Other networks are kept.
    /// Returns the removed conversation IDs.
    pub fn reset_network(
        &mut self,
        network: NetworkId,
        channels: Vec<String>,
    ) -> Vec<ConversationId> {
        if !self.networks.iter().any(|server| server.id == network) {
            return Vec::new();
        }
        let selected_here = self.selected_network().map(|selected| selected.id) == Some(network);
        let removed = self.remove_conversations(network);
        self.server_messages.remove(&network);
        self.active_servers.remove(&network);
        // The logs that could show old avatars are gone.
        self.avatars.remove_network(network);
        self.statuses.insert(network, ConnectionStatus::Connecting);
        let mut first = None;
        for channel in channels {
            let id = self.ensure_channel(network, &channel);
            first = first.or(id);
        }
        match (selected_here, first) {
            (true, Some(id)) => self.selected = Selection::Channel(id),
            (true, None) => self.selected = Selection::Server(network),
            _ => {}
        }
        self.repair_selection();
        removed
    }

    fn remove_conversations(&mut self, network: NetworkId) -> Vec<ConversationId> {
        let removed: Vec<ConversationId> = self
            .conversations
            .iter()
            .filter(|channel| channel.network == network)
            .map(|channel| channel.id)
            .collect();
        self.conversations
            .retain(|channel| channel.network != network);
        for id in &removed {
            self.duplicates.remove(id);
            self.pending_history.remove(id);
            self.older_history.remove(id);
            self.resume_points.remove(id);
            self.unread.remove(id);
            self.highlighted.remove(id);
            self.active_channels.remove(id);
            if self.previous_channel == Some(*id) {
                self.previous_channel = None;
            }
            if self.selected == Selection::Channel(*id) {
                self.selected = Selection::Server(network);
            }
        }
        removed
    }

    /// Keeps each network's conversations together, in network order, so
    /// channel navigation and numbered shortcuts follow the tree.
    fn sort_conversations(&mut self) {
        let position = |network: NetworkId, networks: &[Network]| {
            networks
                .iter()
                .position(|server| server.id == network)
                .unwrap_or(usize::MAX)
        };
        let networks = &self.networks;
        self.conversations
            .sort_by_key(|channel| position(channel.network, networks));
    }

    fn repair_selection(&mut self) {
        let valid = match self.selected {
            Selection::Server(id) => self.networks.iter().any(|server| server.id == id),
            Selection::Channel(id) => self.conversations.iter().any(|channel| channel.id == id),
            Selection::None => self.networks.is_empty(),
        };
        if !valid {
            self.selected = self
                .networks
                .first()
                .map_or(Selection::None, |network| Selection::Server(network.id));
        }
    }

    pub fn status(&self, id: NetworkId) -> Option<&ConnectionStatus> {
        self.statuses.get(&id)
    }

    pub fn server_messages(&self, id: NetworkId) -> &[Message] {
        self.server_messages
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn set_status(&mut self, id: NetworkId, status: ConnectionStatus) {
        if self.networks.iter().any(|network| network.id == id) {
            if status == ConnectionStatus::Registered {
                self.active_servers.insert(id);
                // Private conversations need no join: usable once registered.
                for conversation in &self.conversations {
                    if conversation.network == id && conversation.is_private() {
                        self.active_channels.insert(conversation.id);
                    }
                }
            } else if matches!(status, ConnectionStatus::Disconnected(_)) {
                self.active_servers.remove(&id);
                // Paging state belongs to the session that ended; a page
                // still on its way can no longer be accepted.
                let could_page = self.history_paging.remove(&id);
                self.cut_off_channels(id, could_page);
                for channel in self
                    .conversations
                    .iter_mut()
                    .filter(|channel| channel.network == id)
                {
                    self.active_channels.remove(&channel.id);
                    // A reply can only come from the connection that ended.
                    self.pending_history.remove(&channel.id);
                    self.older_history.remove(&channel.id);
                    channel.members.clear();
                }
            }
            self.statuses.insert(id, status);
        }
    }

    pub fn append_server_message(&mut self, id: NetworkId, text: String) {
        self.append_server_message_at(id, text, MessageMeta::live());
    }

    /// Like [`AppState::append_server_message`], keeping the source's time,
    /// identifier and provenance from `meta`.
    pub fn append_server_message_at(&mut self, id: NetworkId, text: String, meta: MessageMeta) {
        if !self.networks.iter().any(|network| network.id == id) {
            return;
        }
        let mut message = new_message("server".into(), text, false, meta);
        message.sequence = self.next_sequence();
        let messages = self.server_messages.entry(id).or_default();
        push_bounded(messages, message);
    }

    fn next_sequence(&mut self) -> u64 {
        self.next_message_sequence += 1;
        self.next_message_sequence
    }

    /// The channel conversation `name` of `network`.
    pub fn channel_id(&self, network: NetworkId, name: &str) -> Option<ConversationId> {
        self.conversations
            .iter()
            .find(|channel| {
                channel.network == network
                    && channel.kind == ConversationKind::Channel
                    && channel.name.eq_ignore_ascii_case(name)
            })
            .map(|channel| channel.id)
    }

    /// The private conversation with `peer_key` on `network`.
    pub fn private_id(&self, network: NetworkId, peer_key: &str) -> Option<ConversationId> {
        self.conversations
            .iter()
            .find(|conversation| {
                conversation.network == network && conversation.peer_key() == Some(peer_key)
            })
            .map(|conversation| conversation.id)
    }

    /// The private conversation with `peer_key` on `network`, named `name`
    /// (the peer's current display name). It is created when `create` is
    /// set and the network has room; `None` means the caller keeps the
    /// message in the server log. An existing conversation takes `name`,
    /// so it follows the spelling the peer uses now.
    pub fn private_conversation(
        &mut self,
        network: NetworkId,
        peer_key: &str,
        name: &str,
        create: bool,
    ) -> Option<ConversationId> {
        if let Some(id) = self.private_id(network, peer_key) {
            if let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == id)
                && conversation.name != name
            {
                conversation.name = name.to_owned();
            }
            return Some(id);
        }
        if !create
            || self
                .conversations
                .iter()
                .filter(|c| c.network == network && c.is_private())
                .count()
                >= MAX_PRIVATE_CONVERSATIONS_PER_NETWORK
        {
            return None;
        }
        let id = self.add_conversation(
            network,
            ConversationKind::Private {
                peer_key: peer_key.to_owned(),
            },
            name,
        )?;
        if self.status(network) == Some(&ConnectionStatus::Registered) {
            self.active_channels.insert(id);
        }
        Some(id)
    }

    /// A private conversation's peer changed name. The conversation follows
    /// them unless one with the new name already exists (then both stay as
    /// they are, rather than merging two histories). Returns the
    /// conversation that was renamed.
    pub fn rename_private(
        &mut self,
        network: NetworkId,
        from_key: &str,
        to_key: &str,
        to_name: &str,
    ) -> Option<ConversationId> {
        let id = self.private_id(network, from_key)?;
        if from_key != to_key && self.private_id(network, to_key).is_some() {
            return None;
        }
        let conversation = self.conversations.iter_mut().find(|c| c.id == id)?;
        conversation.kind = ConversationKind::Private {
            peer_key: to_key.to_owned(),
        };
        conversation.name = to_name.to_owned();
        Some(id)
    }

    /// Closes a private conversation (channels are left with PART instead).
    /// Returns whether it was removed; the caller drops its UI state.
    pub fn close_private(&mut self, id: ConversationId) -> bool {
        let Some(index) = self
            .conversations
            .iter()
            .position(|c| c.id == id && c.is_private())
        else {
            return false;
        };
        let network = self.conversations.remove(index).network;
        self.duplicates.remove(&id);
        self.pending_history.remove(&id);
        self.older_history.remove(&id);
        self.resume_points.remove(&id);
        self.unread.remove(&id);
        self.highlighted.remove(&id);
        self.active_channels.remove(&id);
        if self.previous_channel == Some(id) {
            self.previous_channel = None;
        }
        if self.selected == Selection::Channel(id) {
            self.selected = Selection::Server(network);
        }
        true
    }

    /// Adds a message to conversation `id` (any kind). `unread` marks it
    /// unread when it is not selected. Returns `false` for a duplicate.
    pub fn append_conversation_message(
        &mut self,
        id: ConversationId,
        sender: &str,
        text: &str,
        notice: bool,
        meta: MessageMeta,
        unread: bool,
    ) -> bool {
        let text = if notice {
            format!("[NOTICE] {text}")
        } else {
            text.into()
        };
        if !self.append_to_conversation(id, new_message(sender.into(), text, false, meta)) {
            return false;
        }
        if unread {
            self.mark_unread(id);
        }
        true
    }

    /// Adds an activity line (a nick change, a quit) to conversation `id`.
    pub fn append_conversation_activity(&mut self, id: ConversationId, text: String) {
        self.append_to_conversation(
            id,
            new_message(String::new(), text, true, MessageMeta::live()),
        );
    }

    /// Marks conversation `id` highlighted until it is selected.
    pub fn highlight(&mut self, id: ConversationId) {
        if self.selected != Selection::Channel(id) && self.conversations.iter().any(|c| c.id == id)
        {
            self.highlighted.insert(id);
        }
    }

    fn ensure_channel(&mut self, network: NetworkId, name: &str) -> Option<ConversationId> {
        if let Some(id) = self.channel_id(network, name) {
            return Some(id);
        }
        self.add_conversation(network, ConversationKind::Channel, name)
    }

    /// Adds a conversation to `network`, within its limit. Each network's
    /// conversations stay together in network order, channels before
    /// private conversations, so navigation and numbered shortcuts follow
    /// the tree.
    fn add_conversation(
        &mut self,
        network: NetworkId,
        kind: ConversationKind,
        name: &str,
    ) -> Option<ConversationId> {
        if !self.networks.iter().any(|server| server.id == network)
            || self
                .conversations
                .iter()
                .filter(|channel| channel.network == network)
                .count()
                >= MAX_CONVERSATIONS_PER_NETWORK
        {
            return None;
        }
        let id = ConversationId(self.next_conversation_id);
        self.next_conversation_id += 1;
        let private = kind != ConversationKind::Channel;
        let index = self
            .conversations
            .iter()
            .rposition(|c| c.network == network && (private || !c.is_private()))
            .map(|index| index + 1)
            .or_else(|| {
                // A network's first channel goes before its private ones.
                self.conversations.iter().position(|c| c.network == network)
            })
            .unwrap_or_else(|| {
                let order = self
                    .networks
                    .iter()
                    .position(|server| server.id == network)
                    .unwrap_or(usize::MAX);
                self.conversations
                    .iter()
                    .position(|channel| {
                        self.networks
                            .iter()
                            .position(|server| server.id == channel.network)
                            .unwrap_or(usize::MAX)
                            > order
                    })
                    .unwrap_or(self.conversations.len())
            });
        self.conversations.insert(
            index,
            Conversation {
                id,
                network,
                kind,
                name: name.to_owned(),
                topic: String::new(),
                messages: Vec::new(),
                members: Vec::new(),
            },
        );
        Some(id)
    }

    pub fn joined_channel(&mut self, network: NetworkId, name: &str) {
        if let Some(id) = self.ensure_channel(network, name) {
            self.active_channels.insert(id);
        }
    }

    pub fn parted_channel(&mut self, network: NetworkId, name: &str) {
        if let Some(id) = self.channel_id(network, name) {
            self.active_channels.remove(&id);
            // A page asked for while joined is not accepted after leaving,
            // and a channel left on purpose has nothing to recover.
            self.older_history.remove(&id);
            self.resume_points.remove(&id);
            if let Some(channel) = self
                .conversations
                .iter_mut()
                .find(|channel| channel.id == id)
            {
                channel.members.clear();
            }
        }
    }

    pub fn set_members(&mut self, network: NetworkId, name: &str, members: Vec<String>) {
        if let Some(id) = self.channel_id(network, name)
            && let Some(channel) = self
                .conversations
                .iter_mut()
                .find(|channel| channel.id == id)
        {
            channel.members = sorted_members(members);
        }
    }

    /// Sets the topic of a joined channel (empty clears it). Topics of
    /// channels not in the tree are ignored.
    pub fn set_topic(&mut self, network: NetworkId, name: &str, topic: &str) {
        if let Some(id) = self.channel_id(network, name)
            && let Some(channel) = self
                .conversations
                .iter_mut()
                .find(|channel| channel.id == id)
        {
            channel.topic = topic.to_owned();
        }
    }

    pub fn append_channel_message(
        &mut self,
        network: NetworkId,
        name: &str,
        sender: &str,
        text: &str,
        notice: bool,
        replayed: bool,
    ) {
        self.append_channel_message_at(
            network,
            name,
            sender,
            text,
            notice,
            MessageMeta::replayed(replayed),
        );
    }

    /// Like [`AppState::append_channel_message`], keeping the source's time,
    /// identifier and provenance from `meta`. Ordering still follows
    /// arrival. Returns `false` when nothing was added because the
    /// conversation already has the message (see
    /// [`timeline::DuplicateFilter`]).
    pub fn append_channel_message_at(
        &mut self,
        network: NetworkId,
        name: &str,
        sender: &str,
        text: &str,
        notice: bool,
        meta: MessageMeta,
    ) -> bool {
        // Only joined or configured channels get a conversation; anything else
        // a server sends lands in the bounded server log instead.
        let Some(id) = self.channel_id(network, name) else {
            let prefix = if notice { "[NOTICE] " } else { "" };
            self.append_server_message_at(
                network,
                format!("{name} <{sender}> {prefix}{text}"),
                meta,
            );
            return true;
        };
        let text = if notice {
            format!("[NOTICE] {text}")
        } else {
            text.into()
        };
        if !self.append_to_conversation(id, new_message(sender.into(), text, false, meta)) {
            return false;
        }
        self.mark_unread(id);
        true
    }

    pub fn append_channel_activity(&mut self, network: NetworkId, name: &str, text: String) {
        self.append_channel_activity_at(network, name, text, MessageMeta::live());
    }

    pub fn append_channel_activity_at(
        &mut self,
        network: NetworkId,
        name: &str,
        text: String,
        meta: MessageMeta,
    ) {
        if let Some(id) = self.channel_id(network, name) {
            self.append_to_conversation(id, new_message(String::new(), text, true, meta));
        }
    }

    /// Appends to a conversation's bounded log unless its duplicate filter
    /// recognizes the message. Only a message that is added takes a
    /// sequence.
    fn append_to_conversation(&mut self, id: ConversationId, mut message: Message) -> bool {
        let Some(position) = self
            .conversations
            .iter()
            .position(|conversation| conversation.id == id)
        else {
            return false;
        };
        if (message.native_id.is_some() || message.timestamp.is_some())
            && !self.duplicates.entry(id).or_default().admit(&message)
        {
            return false;
        }
        message.sequence = self.next_sequence();
        push_bounded(&mut self.conversations[position].messages, message);
        true
    }

    /// The joined channels of `network` a disconnect just cut off. With
    /// history available on the session that ended, each gets a resume
    /// point from its newest identified line, unless it still has one that
    /// was not answered (an earlier cut, possibly asked for on an attempt
    /// that failed too: the earliest cut is the one to recover from).
    /// Without it, the server cannot recover them, so earlier points of
    /// channels this session joined are dropped. Channels not joined are
    /// left alone (the session ended before rejoining them).
    fn cut_off_channels(&mut self, network: NetworkId, could_page: bool) {
        for position in 0..self.conversations.len() {
            let conversation = &self.conversations[position];
            let id = conversation.id;
            if conversation.network != network
                || conversation.is_private()
                || !self.active_channels.contains(&id)
            {
                continue;
            }
            if !could_page {
                self.resume_points.remove(&id);
                continue;
            }
            if self.resume_points.contains_key(&id) {
                continue;
            }
            let messages = &conversation.messages;
            let tail = &messages[messages.len().saturating_sub(HISTORY_RESERVE)..];
            let Some(newest) = tail
                .iter()
                .filter(|message| message.timestamp.is_some())
                .max_by_key(|message| message.timestamp)
                .or_else(|| tail.iter().rev().find(|m| m.native_id.is_some()))
            else {
                continue;
            };
            let point = ResumePoint {
                start: self.next_message_sequence + 1,
                native_id: newest.native_id.clone(),
                timestamp: newest.timestamp,
            };
            self.next_message_sequence += HISTORY_RESERVE as u64;
            self.resume_points.insert(id, point);
        }
    }

    /// Conversations of `network` to resume on its next connection (see
    /// [`HistoryResume`]).
    pub fn history_resume(&self, network: NetworkId) -> Vec<HistoryResume> {
        self.conversations
            .iter()
            .filter(|conversation| conversation.network == network)
            .filter_map(|conversation| {
                let point = self.resume_points.get(&conversation.id)?;
                Some(HistoryResume {
                    conversation: conversation.id,
                    name: conversation.name.clone(),
                    native_id: point.native_id.clone(),
                    timestamp: point.timestamp,
                })
            })
            .collect()
    }

    /// The missed lines of a channel cut off by a disconnect were asked for
    /// ([`AppState::history_resume`]): they go where the cut was, before
    /// every line received since, not where the request was made. Without
    /// a resume point this is [`AppState::history_requested`].
    pub fn history_resumed(&mut self, network: NetworkId, name: &str) {
        let Some(id) = self.channel_id(network, name) else {
            return;
        };
        match self.resume_points.get(&id) {
            Some(point) => {
                self.pending_history.insert(id, point.start);
            }
            None => self.history_requested(network, name),
        }
    }

    /// History was requested for a channel: sequences are reserved here, so
    /// the reply lands before every line that arrives after this call. A
    /// channel cut off by a disconnect that asks for its latest lines
    /// instead of what it missed gives up recovering.
    pub fn history_requested(&mut self, network: NetworkId, name: &str) {
        let Some(id) = self.channel_id(network, name) else {
            return;
        };
        self.resume_points.remove(&id);
        let start = self.next_message_sequence + 1;
        self.next_message_sequence += HISTORY_RESERVE as u64;
        self.pending_history.insert(id, start);
    }

    /// Inserts the reply to [`AppState::history_requested`], oldest first,
    /// where the request was made. Lines the conversation already has
    /// (playback, or live lines that the reply repeats) are skipped by the
    /// duplicate filter, lines beyond [`HISTORY_RESERVE`] are dropped, and
    /// nothing is marked unread or highlighted. A reply without a pending
    /// request (a reset, a disconnect, another connection) changes nothing.
    /// Returns how many lines were added.
    pub fn insert_channel_history(
        &mut self,
        network: NetworkId,
        name: &str,
        lines: Vec<timeline::TimelineLine>,
    ) -> usize {
        self.insert_resumed_history(network, name, lines, None)
    }

    /// Like [`AppState::insert_channel_history`]; also ends recovery of a
    /// channel cut off by a disconnect when the reply is for its cut.
    /// `gap_note` (an activity line) goes first, where the reply says lines
    /// may be missing: a recovery that reached its limit.
    pub fn insert_resumed_history(
        &mut self,
        network: NetworkId,
        name: &str,
        lines: Vec<timeline::TimelineLine>,
        gap_note: Option<String>,
    ) -> usize {
        let Some(id) = self.channel_id(network, name) else {
            return 0;
        };
        let Some(start) = self.pending_history.remove(&id) else {
            return 0;
        };
        if self
            .resume_points
            .get(&id)
            .is_some_and(|point| point.start == start)
        {
            self.resume_points.remove(&id);
        }
        let Some(position) = self
            .conversations
            .iter()
            .position(|conversation| conversation.id == id)
        else {
            return 0;
        };
        let end = start + HISTORY_RESERVE as u64;
        let mut block = Vec::with_capacity(lines.len().min(HISTORY_RESERVE) + 1);
        let mut next = start;
        if let Some(note) = gap_note {
            let mut message = new_message(
                String::new(),
                note,
                true,
                MessageMeta {
                    provenance: Provenance::Requested,
                    ..MessageMeta::live()
                },
            );
            message.sequence = next;
            next += 1;
            block.push(message);
        }
        for line in lines {
            if next >= end {
                break;
            }
            let mut message = new_message(
                line.sender,
                line.text,
                false,
                MessageMeta {
                    provenance: Provenance::Requested,
                    ..line.meta
                },
            );
            if (message.native_id.is_some() || message.timestamp.is_some())
                && !self.duplicates.entry(id).or_default().admit(&message)
            {
                continue;
            }
            message.sequence = next;
            next += 1;
            block.push(message);
        }
        let added = block.len();
        let messages = &mut self.conversations[position].messages;
        let at = messages.partition_point(|message| message.sequence < start);
        messages.splice(at..at, block);
        trim_retained(messages);
        added
    }

    /// Whether `network`'s backend can page back through history now (IRC:
    /// registered with `draft/chathistory`). Disconnecting turns it off.
    pub fn set_history_paging(&mut self, network: NetworkId, available: bool) {
        if available && self.networks.iter().any(|server| server.id == network) {
            self.history_paging.insert(network);
        } else {
            self.history_paging.remove(&network);
        }
    }

    /// Starts one older-history page for conversation `id` and returns
    /// what the backend should ask for, or `None` when no page may be asked
    /// for now: the network cannot page, the conversation is not a joined
    /// channel, its recent history or another page is on its way, it
    /// reached the beginning of what can be had, its log is full, or it
    /// holds no message its source identified. Call it only when the user
    /// scrolls to the top; answers come back through
    /// [`AppState::insert_older_history`] or
    /// [`AppState::older_history_failed`].
    pub fn request_older_history(&mut self, id: ConversationId) -> Option<OlderHistoryRequest> {
        let conversation = self.conversations.iter().find(|c| c.id == id)?;
        let state = self.older_history.get(&id).copied().unwrap_or_default();
        if !self.history_paging.contains(&conversation.network)
            || conversation.is_private()
            || !self.active_channels.contains(&id)
            || self.pending_history.contains_key(&id)
            || state.in_flight.is_some()
            || state.finished
            || conversation.messages.len() >= MAX_RETAINED
        {
            return None;
        }
        // The oldest line by source time near the top: recent history is
        // placed after the lines that arrived before it was asked for (our
        // own JOIN), so the first line is not necessarily the oldest.
        let head =
            &conversation.messages[..conversation.messages.len().min(OLDER_PAGE_OVERLAP_WINDOW)];
        let oldest = head
            .iter()
            .filter(|message| message.timestamp.is_some())
            .min_by_key(|message| message.timestamp)
            .or_else(|| head.iter().find(|message| message.native_id.is_some()))?;
        let limit = OLDER_PAGE_LIMIT.min(MAX_RETAINED - conversation.messages.len());
        let native_id = oldest.native_id.clone();
        let timestamp = oldest.timestamp;
        self.next_history_request += 1;
        let request = self.next_history_request;
        self.older_history.insert(
            id,
            OlderHistory {
                in_flight: Some(request),
                finished: false,
            },
        );
        Some(OlderHistoryRequest {
            request,
            native_id,
            timestamp,
            limit,
        })
    }

    /// The older-history request of conversation `id` that is on its way.
    pub fn older_history_in_flight(&self, id: ConversationId) -> Option<u64> {
        self.older_history
            .get(&id)
            .and_then(|state| state.in_flight)
    }

    /// Puts the answer to older-history request `request` before everything
    /// conversation `id` holds, oldest first, and returns how many lines
    /// were added. Lines the conversation already has near its top (a
    /// repeated or overlapping page, recent history, playback) or recently
    /// received are skipped; the rest fills the log up to its bound, the
    /// oldest lines of the page giving way. Nothing is marked unread or
    /// highlighted and nothing already shown moves or is renumbered. An
    /// answer to another request (a stale one: the session ended, the
    /// channel was left, the conversation was removed) changes nothing.
    /// `beginning` says the source has nothing older; a page that adds
    /// nothing new ends paging as well, so the same request is not repeated.
    pub fn insert_older_history(
        &mut self,
        id: ConversationId,
        request: u64,
        lines: Vec<timeline::TimelineLine>,
        beginning: bool,
    ) -> usize {
        let Some(state) = self.older_history.get_mut(&id) else {
            return 0;
        };
        if state.in_flight != Some(request) {
            return 0;
        }
        state.in_flight = None;
        let Some(position) = self.conversations.iter().position(|c| c.id == id) else {
            return 0;
        };
        let messages = &self.conversations[position].messages;
        // A temporary window over the top of the log (topmost last, so it
        // is the last to be forgotten) catches overlap; the conversation's
        // own filter catches lines received recently. Neither grows: older
        // lines are not recorded in the conversation's filter, which is
        // kept for what arrives live.
        let mut near_top = DuplicateFilter::default();
        for message in messages[..messages.len().min(OLDER_PAGE_OVERLAP_WINDOW)]
            .iter()
            .rev()
        {
            near_top.admit(message);
        }
        let recent = self.duplicates.get(&id);
        let mut block: Vec<Message> = lines
            .into_iter()
            .map(|line| {
                new_message(
                    line.sender,
                    line.text,
                    false,
                    MessageMeta {
                        provenance: Provenance::Requested,
                        ..line.meta
                    },
                )
            })
            .filter(|message| {
                !recent.is_some_and(|filter| filter.contains(message)) && near_top.admit(message)
            })
            .collect();
        let room = MAX_RETAINED.saturating_sub(messages.len());
        if block.len() > room {
            block.drain(..block.len() - room);
        }
        let added = block.len();
        let first = self.older_sequence_floor - added as u64;
        for (offset, message) in block.iter_mut().enumerate() {
            message.sequence = first + offset as u64;
        }
        self.older_sequence_floor = first;
        self.conversations[position].messages.splice(0..0, block);
        if beginning || added == 0 {
            self.older_history.insert(
                id,
                OlderHistory {
                    in_flight: None,
                    finished: true,
                },
            );
        }
        added
    }

    /// Older-history request `request` for conversation `id` ended without
    /// lines (it failed, timed out, or could not be sent). Paging stops for
    /// this session, so a failing request is not repeated on every scroll.
    pub fn older_history_failed(&mut self, id: ConversationId, request: u64) {
        if let Some(state) = self.older_history.get_mut(&id)
            && state.in_flight == Some(request)
        {
            *state = OlderHistory {
                in_flight: None,
                finished: true,
            };
        }
    }

    /// Sequence of the newest message in any log. It changes whenever a
    /// message is added (and with it, when a bounded log drops old lines),
    /// except for older pages, which go before everything else.
    pub fn last_message_sequence(&self) -> u64 {
        self.next_message_sequence
    }

    /// Records a user's avatar (`None` removes it or ends the user's
    /// occupancy of the name). `key` is the protocol-folded user name. See
    /// [`avatars`] for the identity policy.
    pub fn set_avatar(&mut self, network: NetworkId, key: &str, avatar: Option<&str>) {
        let next = self.next_message_sequence + 1;
        self.avatars.set(network, key, avatar, next);
    }

    /// A user changed name; their avatar follows them.
    pub fn rename_avatar(&mut self, network: NetworkId, from: &str, to: &str) {
        let next = self.next_message_sequence + 1;
        self.avatars.rename(network, from, to, next);
    }

    /// Every avatar of `network` becomes unknown (disconnect, reconnect or
    /// lost capability); messages already shown keep theirs.
    pub fn end_avatars(&mut self, network: NetworkId) {
        let next = self.next_message_sequence + 1;
        self.avatars.end_all(network, next);
    }

    pub fn avatars(&self) -> &avatars::AvatarDirectory {
        &self.avatars
    }

    pub fn networks(&self) -> &[Network] {
        &self.networks
    }

    pub fn conversations(&self) -> &[Conversation] {
        &self.conversations
    }

    pub fn selection(&self) -> Selection {
        self.selected
    }

    pub fn selected_channel(&self) -> Option<&Conversation> {
        let Selection::Channel(id) = self.selected else {
            return None;
        };
        self.conversations.iter().find(|channel| channel.id == id)
    }

    /// The selected server, or the server of the selected channel; `None`
    /// only when no server is configured.
    pub fn selected_network(&self) -> Option<&Network> {
        let id = match self.selected {
            Selection::Server(id) => id,
            Selection::Channel(id) => {
                self.conversations
                    .iter()
                    .find(|channel| channel.id == id)
                    .expect("selected channel exists")
                    .network
            }
            Selection::None => return None,
        };
        Some(
            self.networks
                .iter()
                .find(|network| network.id == id)
                .expect("selected network exists"),
        )
    }

    pub fn is_unread(&self, id: ConversationId) -> bool {
        self.unread.contains(&id)
    }

    pub fn is_highlighted(&self, id: ConversationId) -> bool {
        self.highlighted.contains(&id)
    }

    /// Marks a channel that is not selected until the user opens it.
    pub fn mark_highlighted(&mut self, network: NetworkId, name: &str) {
        if let Some(id) = self.channel_id(network, name)
            && self.selected != Selection::Channel(id)
        {
            self.highlighted.insert(id);
        }
    }

    pub fn is_active_channel(&self, id: ConversationId) -> bool {
        self.active_channels.contains(&id)
    }

    pub fn mark_unread(&mut self, id: ConversationId) {
        if self.conversations.iter().any(|channel| channel.id == id)
            && self.selected != Selection::Channel(id)
        {
            self.unread.insert(id);
        }
    }

    fn select_channel(&mut self, id: ConversationId) {
        if !self.conversations.iter().any(|channel| channel.id == id) {
            return;
        }
        if let Selection::Channel(current) = self.selected
            && current != id
        {
            self.previous_channel = Some(current);
        }
        self.selected = Selection::Channel(id);
        self.unread.remove(&id);
        self.highlighted.remove(&id);
    }

    fn select_server(&mut self, id: NetworkId) {
        if self.networks.iter().any(|server| server.id == id) {
            if let Selection::Channel(current) = self.selected {
                self.previous_channel = Some(current);
            }
            self.selected = Selection::Server(id);
        }
    }

    fn cycle_channel(&mut self, forward: bool, filter: ChannelFilter) {
        let len = self.conversations.len();
        if len == 0 {
            return;
        }
        let current = match self.selected {
            Selection::Channel(id) => self
                .conversations
                .iter()
                .position(|channel| channel.id == id)
                .expect("selected channel exists"),
            Selection::Server(network_id) => {
                let first = self
                    .conversations
                    .iter()
                    .position(|c| c.network == network_id);
                let last = self
                    .conversations
                    .iter()
                    .rposition(|c| c.network == network_id);
                if forward {
                    first
                        .map(|index| (index + len - 1) % len)
                        .unwrap_or(len - 1)
                } else {
                    last.map(|index| (index + 1) % len).unwrap_or(0)
                }
            }
            // Unreachable with conversations present; start from the ends.
            Selection::None => {
                if forward {
                    len - 1
                } else {
                    0
                }
            }
        };
        for step in 1..=len {
            let index = if forward {
                (current + step) % len
            } else {
                (current + len - step) % len
            };
            let id = self.conversations[index].id;
            let matches = match filter {
                ChannelFilter::All => true,
                ChannelFilter::Active => self.active_channels.contains(&id),
                ChannelFilter::Unread => self.unread.contains(&id),
            };
            if matches {
                self.select_channel(id);
                return;
            }
        }
    }

    fn cycle_server(&mut self, forward: bool, active_only: bool) {
        let len = self.networks.len();
        if len == 0 {
            return;
        }
        let Some(selected_network) = self.selected_network().map(|network| network.id) else {
            self.select_server(self.networks[0].id);
            return;
        };
        let current = self
            .networks
            .iter()
            .position(|server| server.id == selected_network)
            .expect("selected server exists");
        for step in 1..=len {
            let index = if forward {
                (current + step) % len
            } else {
                (current + len - step) % len
            };
            let id = self.networks[index].id;
            if !active_only || self.active_servers.contains(&id) {
                self.select_server(id);
                return;
            }
        }
    }

    pub fn dispatch(&mut self, command: Command) {
        match command {
            Command::SelectChannel(id) => self.select_channel(id),
            Command::SelectServer(id) => self.select_server(id),
            Command::NextUnreadChannel => self.cycle_channel(true, ChannelFilter::Unread),
            Command::PreviousUnreadChannel => self.cycle_channel(false, ChannelFilter::Unread),
            Command::PreviousSelectedChannel => {
                if let Some(id) = self.previous_channel {
                    self.select_channel(id);
                }
            }
            Command::NextActiveChannel => self.cycle_channel(true, ChannelFilter::Active),
            Command::PreviousActiveChannel => self.cycle_channel(false, ChannelFilter::Active),
            Command::NextChannel => self.cycle_channel(true, ChannelFilter::All),
            Command::PreviousChannel => self.cycle_channel(false, ChannelFilter::All),
            Command::NextActiveServer => self.cycle_server(true, true),
            Command::PreviousActiveServer => self.cycle_server(false, true),
            Command::NextServer => self.cycle_server(true, false),
            Command::PreviousServer => self.cycle_server(false, false),
            Command::SelectChannelAt(index) => {
                if let Some(id) = self.conversations.get(index).map(|channel| channel.id) {
                    self.select_channel(id);
                }
            }
            Command::SelectServerAt(index) => {
                if let Some(id) = self.networks.get(index).map(|server| server.id) {
                    self.select_server(id);
                }
            }
        }
    }
}

fn sorted_members(mut members: Vec<String>) -> Vec<String> {
    // Operators first, then case-insensitively by nickname. Keys are computed
    // once per member; large channels republish their roster on every change.
    members.sort_by_cached_key(|member| {
        let nickname = member.trim_start_matches(['~', '&', '@', '%', '+']);
        (
            !member.starts_with(['~', '&', '@', '%']),
            nickname.to_lowercase(),
            nickname.to_owned(),
        )
    });
    members
}

/// Local time of day for a message: the server-provided instant when there
/// is one, otherwise now (receipt time).
fn display_time(received: Option<SystemTime>) -> TimeOfDay {
    use chrono::Timelike;
    let local = received.map_or_else(chrono::Local::now, chrono::DateTime::<chrono::Local>::from);
    TimeOfDay::new(local.hour() as u8, local.minute() as u8)
}

/// A timeline item from `meta`, without its sequence yet.
fn new_message(sender: String, text: String, activity: bool, meta: MessageMeta) -> Message {
    Message {
        time: display_time(meta.server_time),
        sequence: 0,
        timestamp: meta.server_time.and_then(Timestamp::from_system_time),
        native_id: meta.native_id,
        sender,
        text,
        activity,
        provenance: meta.provenance,
    }
}

/// Parses the `HH:MM` literals of the offline mock data.
fn mock_time(time: &str) -> TimeOfDay {
    let (hour, minute) = time.split_once(':').expect("mock time is HH:MM");
    TimeOfDay::new(
        hour.parse().expect("mock hour"),
        minute.parse().expect("mock minute"),
    )
}

fn push_bounded(messages: &mut Vec<Message>, message: Message) {
    messages.push(message);
    trim_retained(messages);
}

fn trim_retained(messages: &mut Vec<Message>) {
    while messages.len() > MAX_RETAINED {
        messages.drain(..1_000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_roster_after_every_update_with_operators_first() {
        let mut state = AppState::live("irc.example.org".into(), vec!["#test".into()]);
        state.set_members(
            NetworkId(1),
            "#test",
            vec![
                "zoe".into(),
                "+bob".into(),
                "@Alice".into(),
                "%carol".into(),
            ],
        );
        assert_eq!(
            state.selected_channel().unwrap().members,
            ["@Alice", "%carol", "+bob", "zoe"]
        );
        state.set_members(
            NetworkId(1),
            "#test",
            vec!["zoe".into(), "@bob".into(), "alice".into()],
        );
        assert_eq!(
            state.selected_channel().unwrap().members,
            ["@bob", "alice", "zoe"]
        );
    }

    #[test]
    fn same_named_channels_keep_networks_and_logs_separate() {
        let mut state = AppState::mock();
        let original_network = state.selected_network().unwrap().id;
        let original_message = state.selected_channel().unwrap().messages[0].text.clone();
        state.dispatch(Command::SelectChannel(ConversationId(3)));
        assert_eq!(state.selected_channel().unwrap().name, "#general");
        assert_ne!(state.selected_network().unwrap().id, original_network);
        assert_ne!(
            state.selected_channel().unwrap().messages[0].text,
            original_message
        );
        state.dispatch(Command::SelectChannel(ConversationId(999)));
        assert_eq!(state.selection(), Selection::Channel(ConversationId(3)));
    }

    #[test]
    fn unread_navigation_consumes_unread_and_skips_read_channels() {
        let mut state = AppState::mock();
        state.dispatch(Command::NextUnreadChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(2)));
        assert!(!state.is_unread(ConversationId(2)));
        state.dispatch(Command::NextUnreadChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(4)));
        state.dispatch(Command::NextUnreadChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(4)));
        state.mark_unread(ConversationId(1));
        state.dispatch(Command::PreviousUnreadChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(1)));
        assert!(!state.is_unread(ConversationId(1)));
    }

    #[test]
    fn previous_selection_toggles_and_active_navigation_filters() {
        let mut state = AppState::mock();
        state.active_channels.remove(&ConversationId(2));
        state.dispatch(Command::NextActiveChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(3)));
        state.dispatch(Command::PreviousSelectedChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(1)));
        state.dispatch(Command::PreviousSelectedChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(3)));
        state.dispatch(Command::PreviousActiveChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(1)));
        state.dispatch(Command::SelectServer(NetworkId(2)));
        state.dispatch(Command::PreviousSelectedChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(1)));
    }

    #[test]
    fn channel_and_server_indexing_and_cyclic_navigation() {
        let mut state = AppState::mock();
        state.dispatch(Command::PreviousChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(4)));
        state.dispatch(Command::NextChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(1)));
        state.dispatch(Command::SelectChannelAt(2));
        assert_eq!(state.selection(), Selection::Channel(ConversationId(3)));
        state.dispatch(Command::SelectServerAt(1));
        assert_eq!(state.selection(), Selection::Server(NetworkId(2)));
        state.dispatch(Command::NextChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(3)));
        state.dispatch(Command::SelectServerAt(1));
        state.dispatch(Command::PreviousChannel);
        assert_eq!(state.selection(), Selection::Channel(ConversationId(4)));
        state.dispatch(Command::SelectServerAt(1));
        state.dispatch(Command::NextServer);
        assert_eq!(state.selection(), Selection::Server(NetworkId(1)));
        state.active_servers.remove(&NetworkId(2));
        state.dispatch(Command::NextActiveServer);
        assert_eq!(state.selection(), Selection::Server(NetworkId(1)));
        state.dispatch(Command::SelectServerAt(9));
        assert_eq!(state.selection(), Selection::Server(NetworkId(1)));
    }

    #[test]
    fn live_events_update_channel_roster_unread_and_connection_state() {
        let mut state = AppState::live("irc.example.org".into(), vec!["#one".into()]);
        assert_eq!(
            state.status(NetworkId(1)),
            Some(&ConnectionStatus::Connecting)
        );
        state.set_status(NetworkId(1), ConnectionStatus::Registered);
        state.joined_channel(NetworkId(1), "#two");
        state.set_members(NetworkId(1), "#two", vec!["@alice".into()]);
        state.append_channel_message(NetworkId(1), "#two", "alice", "hello", false, false);
        assert_eq!(state.conversations().len(), 2);
        assert!(state.is_unread(ConversationId(2)));
        state.dispatch(Command::SelectChannel(ConversationId(2)));
        assert!(!state.is_unread(ConversationId(2)));
        assert_eq!(state.selected_channel().unwrap().members, ["@alice"]);
        assert_eq!(state.selected_channel().unwrap().messages[0].text, "hello");
        state.parted_channel(NetworkId(1), "#two");
        assert!(!state.is_active_channel(ConversationId(2)));
        assert!(state.selected_channel().unwrap().members.is_empty());
        state.joined_channel(NetworkId(1), "#two");
        assert!(state.is_active_channel(ConversationId(2)));
        state.set_members(NetworkId(1), "#two", vec!["@alice".into()]);
        state.set_status(
            NetworkId(1),
            ConnectionStatus::Disconnected("closed".into()),
        );
        assert_eq!(
            state.status(NetworkId(1)),
            Some(&ConnectionStatus::Disconnected("closed".into()))
        );
        assert!(state.active_channels.is_empty());
        assert!(state.selected_channel().unwrap().members.is_empty());
    }

    #[test]
    fn channel_messages_keep_arrival_order_across_channels() {
        let mut state =
            AppState::live("irc.example.org".into(), vec!["#one".into(), "#two".into()]);
        state.append_channel_message(NetworkId(1), "#two", "alice", "first", false, false);
        state.append_channel_message(NetworkId(1), "#one", "bob", "latest", false, false);

        let mut messages: Vec<_> = state
            .conversations()
            .iter()
            .flat_map(|channel| &channel.messages)
            .collect();
        messages.sort_by_key(|message| message.sequence);
        assert_eq!(
            messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["first", "latest"]
        );
    }

    #[test]
    fn highlights_mark_unselected_channels_until_selected() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into(), "#b".into()]);
        let (a, b) = (state.conversations[0].id, state.conversations[1].id);
        state.dispatch(Command::SelectChannel(a));
        state.mark_highlighted(NetworkId(1), "#a");
        state.mark_highlighted(NetworkId(1), "#b");
        state.mark_highlighted(NetworkId(1), "#unknown");
        assert!(!state.is_highlighted(a));
        assert!(state.is_highlighted(b));
        state.dispatch(Command::SelectChannel(b));
        assert!(!state.is_highlighted(b));
    }

    #[test]
    fn channel_activity_is_ordered_without_marking_unread() {
        let mut state =
            AppState::live("irc.example.org".into(), vec!["#one".into(), "#two".into()]);
        state.append_channel_activity(NetworkId(1), "#two", "alice has joined (u@h)".into());
        assert!(!state.is_unread(ConversationId(2)));
        state.append_channel_message(NetworkId(1), "#two", "alice", "hello", false, false);
        assert!(state.is_unread(ConversationId(2)));
        let channel = &state.conversations()[1];
        assert!(channel.messages[0].activity);
        assert!(channel.messages[0].sequence < channel.messages[1].sequence);
        assert_eq!(channel.messages[0].text, "alice has joined (u@h)");
        assert!(!channel.messages[1].activity);
    }

    #[test]
    fn server_traffic_for_unjoined_channels_does_not_create_conversations() {
        let mut state = AppState::live("irc.example.org".into(), vec!["#one".into()]);
        state.append_channel_message(NetworkId(1), "#stray", "mallory", "hi", false, false);
        state.append_channel_activity(NetworkId(1), "#stray", "mallory has joined".into());
        state.set_members(NetworkId(1), "#stray", vec!["mallory".into()]);
        assert_eq!(state.conversations().len(), 1);
        assert!(
            state
                .server_messages(NetworkId(1))
                .iter()
                .any(|message| message.text == "#stray <mallory> hi")
        );

        for index in 0..MAX_CONVERSATIONS_PER_NETWORK + 10 {
            state.joined_channel(NetworkId(1), &format!("#c{index}"));
        }
        assert_eq!(state.conversations().len(), MAX_CONVERSATIONS_PER_NETWORK);
    }

    fn two_networks() -> AppState {
        AppState::with_networks(vec![
            NetworkConfig {
                id: NetworkId(1),
                name: "one.example".into(),
                channels: vec!["#a".into(), "#b".into()],
            },
            NetworkConfig {
                id: NetworkId(2),
                name: "two.example".into(),
                channels: vec!["#a".into()],
            },
        ])
    }

    fn names(state: &AppState) -> Vec<(u32, String)> {
        state
            .conversations()
            .iter()
            .map(|c| (c.network.0, c.name.clone()))
            .collect()
    }

    #[test]
    fn configured_networks_start_disconnected_and_keep_channels_grouped() {
        let mut state = two_networks();
        assert_eq!(state.networks().len(), 2);
        for id in [NetworkId(1), NetworkId(2)] {
            assert!(matches!(
                state.status(id),
                Some(ConnectionStatus::Disconnected(_))
            ));
        }
        assert_eq!(state.selected_channel().unwrap().name, "#a");
        // A channel joined later on the first network stays with it, so
        // navigation and numbered shortcuts follow the tree.
        state.joined_channel(NetworkId(1), "#late");
        assert_eq!(
            names(&state),
            [
                (1, "#a".into()),
                (1, "#b".into()),
                (1, "#late".into()),
                (2, "#a".into())
            ]
        );
        state.dispatch(Command::SelectChannelAt(3));
        assert_eq!(state.selected_network().unwrap().id, NetworkId(2));
    }

    #[test]
    fn resetting_one_network_keeps_the_others() {
        let mut state = two_networks();
        state.append_channel_message(NetworkId(2), "#a", "bob", "kept", false, false);
        state.append_channel_message(NetworkId(1), "#b", "bob", "dropped", false, false);
        state.append_server_message(NetworkId(1), "old".into());
        let old: Vec<_> = state
            .conversations()
            .iter()
            .filter(|c| c.network == NetworkId(1))
            .map(|c| c.id)
            .collect();
        let selected = state.selection();

        let removed = state.reset_network(NetworkId(1), vec!["#c".into()]);
        assert_eq!(removed, old);
        assert_eq!(names(&state), [(1, "#c".into()), (2, "#a".into())]);
        assert!(state.server_messages(NetworkId(1)).is_empty());
        assert_eq!(
            state.status(NetworkId(1)),
            Some(&ConnectionStatus::Connecting)
        );
        let other = &state.conversations()[1];
        assert_eq!(other.messages[0].text, "kept");
        // The selected channel belonged to the reset network.
        assert_ne!(state.selection(), selected);
        assert_eq!(state.selected_channel().unwrap().name, "#c");
        // IDs are never reused.
        assert!(state.conversations().iter().all(|c| !old.contains(&c.id)));
    }

    #[test]
    fn syncing_networks_adds_renames_reorders_and_removes() {
        let mut state = two_networks();
        state.dispatch(Command::SelectServer(NetworkId(2)));
        let removed = state.sync_networks(&[
            NetworkConfig {
                id: NetworkId(3),
                name: "three.example".into(),
                channels: vec!["#new".into()],
            },
            NetworkConfig {
                id: NetworkId(1),
                name: "renamed.example".into(),
                channels: vec!["#ignored-for-existing".into()],
            },
        ]);
        assert_eq!(removed.len(), 1, "the second network's #a");
        let networks: Vec<_> = state
            .networks()
            .iter()
            .map(|n| (n.id.0, n.name.clone()))
            .collect();
        assert_eq!(
            networks,
            [(3, "three.example".into()), (1, "renamed.example".into())]
        );
        assert!(matches!(
            state.status(NetworkId(3)),
            Some(ConnectionStatus::Disconnected(_))
        ));
        assert!(state.status(NetworkId(2)).is_none());
        // The selected server disappeared; the first remaining one is shown.
        assert_eq!(state.selection(), Selection::Server(NetworkId(3)));
        // A new network shows its configured channels; an existing one keeps
        // its conversations, in network order.
        let names: Vec<_> = state
            .conversations()
            .iter()
            .map(|c| (c.network.0, c.name.as_str()))
            .collect();
        assert_eq!(names, [(3, "#new"), (1, "#a"), (1, "#b")]);
        // Messages for the removed network are ignored.
        state.append_channel_message(NetworkId(2), "#a", "bob", "late", false, false);
        assert!(state.server_messages(NetworkId(2)).is_empty());
    }

    #[test]
    fn no_servers_select_nothing_until_one_is_added() {
        let mut state = AppState::with_networks(Vec::new());
        assert_eq!(state.selection(), Selection::None);
        assert!(state.selected_network().is_none());
        for command in [
            Command::NextChannel,
            Command::NextServer,
            Command::NextUnreadChannel,
        ] {
            state.dispatch(command);
        }
        assert_eq!(state.selection(), Selection::None);
        state.sync_networks(&[NetworkConfig {
            id: NetworkId(1),
            name: "one.example".into(),
            channels: Vec::new(),
        }]);
        assert_eq!(state.selection(), Selection::Server(NetworkId(1)));
        state.sync_networks(&[]);
        assert_eq!(state.selection(), Selection::None);
        assert!(state.networks().is_empty());
    }

    #[test]
    fn server_times_show_as_local_time_without_changing_arrival_order() {
        use std::time::{Duration, UNIX_EPOCH};
        let local = |time: SystemTime| {
            use chrono::Timelike;
            let local = chrono::DateTime::<chrono::Local>::from(time);
            TimeOfDay::new(local.hour() as u8, local.minute() as u8)
        };
        let mut state = AppState::live("irc.example.org".into(), vec!["#test".into()]);
        let network = state.networks()[0].id;
        let later = UNIX_EPOCH + Duration::from_secs(1_319_042_451);
        let earlier = later - Duration::from_secs(3 * 3600);
        state.append_channel_message_at(
            network,
            "#test",
            "bob",
            "first",
            false,
            MessageMeta::at(Some(later)),
        );
        state.append_channel_message_at(
            network,
            "#test",
            "bob",
            "second",
            false,
            MessageMeta::at(Some(earlier)),
        );
        state.append_channel_message_at(
            network,
            "#test",
            "bob",
            "third",
            false,
            MessageMeta::live(),
        );
        state.append_channel_activity_at(
            network,
            "#test",
            "bob left".into(),
            MessageMeta::at(Some(earlier)),
        );
        state.append_server_message_at(network, "notice".into(), MessageMeta::at(Some(later)));
        let received_now = display_time(None);
        let messages = &state
            .conversations()
            .iter()
            .find(|channel| channel.name == "#test")
            .unwrap()
            .messages;
        let texts: Vec<_> = messages
            .iter()
            .map(|message| message.text.as_str())
            .collect();
        assert_eq!(texts, ["first", "second", "third", "bob left"]);
        assert!(
            messages
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert_eq!(messages[0].time, local(later));
        assert_eq!(messages[1].time, local(earlier));
        // Missing server time falls back to the receipt time (allow a minute tick).
        let fallback = messages[2].time;
        assert!(fallback == received_now || fallback == display_time(None));
        assert_eq!(messages[3].time, local(earlier));
        assert_eq!(state.server_messages(network)[0].time, local(later));
    }

    fn meta(millis: Option<u64>, msgid: Option<&str>, provenance: Provenance) -> MessageMeta {
        use std::time::{Duration, UNIX_EPOCH};
        MessageMeta {
            server_time: millis.map(|millis| UNIX_EPOCH + Duration::from_millis(millis)),
            native_id: msgid.and_then(cayenchat_model::NativeMessageId::new),
            provenance,
        }
    }

    fn texts(state: &AppState, index: usize) -> Vec<&str> {
        state.conversations()[index]
            .messages
            .iter()
            .map(|message| message.text.as_str())
            .collect()
    }

    #[test]
    fn retained_messages_keep_full_server_time_identity_and_provenance() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        // 2026-09-27T23:58:31.123Z
        let stamp = 1_790_553_511_123;
        state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "tagged",
            false,
            meta(Some(stamp), Some("abc"), Provenance::Live),
        );
        state.append_channel_message_at(network, "#a", "bob", "plain", false, MessageMeta::live());
        state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "played back",
            false,
            meta(Some(stamp + 1), None, Provenance::Replayed),
        );
        state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "requested",
            false,
            meta(None, Some("def"), Provenance::Requested),
        );
        let messages = &state.conversations()[0].messages;
        let first = &messages[0];
        assert_eq!(first.timestamp.map(Timestamp::as_millis), Some(stamp));
        assert_eq!(first.native_id.as_ref().map(|id| id.as_str()), Some("abc"));
        assert_eq!(first.provenance, Provenance::Live);
        // The display time is still the local HH:MM of the same instant.
        assert_eq!(
            first.time,
            display_time(first.timestamp.map(Timestamp::to_system_time))
        );
        let plain = &messages[1];
        assert!(plain.timestamp.is_none() && plain.native_id.is_none());
        assert!(!plain.is_history());
        assert_eq!(messages[2].provenance, Provenance::Replayed);
        assert!(messages[2].native_id.is_none());
        assert_eq!(messages[3].provenance, Provenance::Requested);
        assert!(messages[3].timestamp.is_none());
        assert!(messages.iter().skip(2).all(Message::is_history));
        assert!(
            messages
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );

        // Server-log lines (private messages, unknown channels) keep them too.
        state.append_channel_message_at(
            network,
            "#stray",
            "eve",
            "hi",
            false,
            meta(Some(stamp), Some("stray1"), Provenance::Replayed),
        );
        let stray = state.server_messages(network).last().unwrap();
        assert_eq!(stray.timestamp.map(Timestamp::as_millis), Some(stamp));
        assert_eq!(stray.native_id.as_ref().unwrap().as_str(), "stray1");
        assert_eq!(stray.provenance, Provenance::Replayed);
    }

    #[test]
    fn duplicates_by_msgid_are_dropped_without_taking_a_sequence_or_unread_mark() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into(), "#b".into()]);
        let network = NetworkId(1);
        state.dispatch(Command::SelectChannel(ConversationId(2)));
        assert!(state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "live",
            false,
            meta(Some(10), Some("m1"), Provenance::Live),
        ));
        state.dispatch(Command::SelectChannel(ConversationId(1)));
        state.dispatch(Command::SelectChannel(ConversationId(2)));
        assert!(!state.is_unread(ConversationId(1)));
        let before = state.last_message_sequence();
        // Played back and requested copies of the same message.
        for provenance in [Provenance::Replayed, Provenance::Requested] {
            assert!(!state.append_channel_message_at(
                network,
                "#a",
                "bob",
                "live",
                false,
                meta(Some(10), Some("m1"), provenance),
            ));
        }
        assert_eq!(state.last_message_sequence(), before);
        assert!(
            !state.is_unread(ConversationId(1)),
            "a duplicate is not news"
        );
        assert_eq!(texts(&state, 0), ["live"]);
        // The same identifier in another conversation is another message
        // (one PRIVMSG to several channels).
        assert!(state.append_channel_message_at(
            network,
            "#b",
            "bob",
            "live",
            false,
            meta(Some(10), Some("m1"), Provenance::Live),
        ));
        // Without an identifier, history is matched by its fingerprint only.
        state.append_channel_message_at(
            network,
            "#a",
            "carol",
            "no id",
            false,
            meta(Some(20), None, Provenance::Replayed),
        );
        state.append_channel_message_at(
            network,
            "#a",
            "carol",
            "no id",
            false,
            meta(Some(20), None, Provenance::Requested),
        );
        state.append_channel_message_at(
            network,
            "#a",
            "carol",
            "no id",
            false,
            meta(Some(20), None, Provenance::Live),
        );
        // Nothing identifies lines without a timestamp or msgid; all stay.
        for _ in 0..2 {
            state.append_channel_message(network, "#a", "dave", "again", false, true);
        }
        assert_eq!(
            texts(&state, 0),
            ["live", "no id", "no id", "again", "again"]
        );
    }

    #[test]
    fn duplicate_state_is_bounded_and_dropped_with_its_conversations() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        for index in 0..timeline::DUPLICATE_KEYS_PER_CONVERSATION * 3 {
            let id = format!("m{index}");
            state.append_channel_message_at(
                network,
                "#a",
                "bob",
                "x",
                false,
                meta(None, Some(&id), Provenance::Live),
            );
        }
        let id = state.conversations()[0].id;
        assert_eq!(
            state.duplicates[&id].len(),
            timeline::DUPLICATE_KEYS_PER_CONVERSATION
        );
        // Plain lines create no filter.
        state.joined_channel(network, "#plain");
        state.append_channel_message(network, "#plain", "bob", "x", false, false);
        assert_eq!(state.duplicates.len(), 1);
        state.reset_network(network, vec!["#a".into()]);
        assert!(state.duplicates.is_empty());
        // After a reset the old identifiers are new again.
        assert!(state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "x",
            false,
            meta(None, Some("m0"), Provenance::Replayed),
        ));
    }

    fn line(text: &str, millis: Option<u64>, msgid: Option<&str>) -> timeline::TimelineLine {
        timeline::TimelineLine {
            sender: "bob".into(),
            text: text.into(),
            meta: meta(millis, msgid, Provenance::Live),
        }
    }

    #[test]
    fn requested_history_goes_where_it_was_requested_without_unread_marks() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into(), "#b".into()]);
        let network = NetworkId(1);
        state.dispatch(Command::SelectChannel(ConversationId(2)));
        state.append_channel_activity(network, "#a", "alice has joined".into());
        state.history_requested(network, "#a");
        // Live traffic while the request is answered, in #a and elsewhere.
        state.append_channel_message_at(
            network,
            "#a",
            "carol",
            "live",
            false,
            meta(Some(30), Some("live1"), Provenance::Live),
        );
        state.append_channel_message(network, "#b", "dave", "other channel", false, false);
        state.dispatch(Command::SelectChannel(ConversationId(1)));
        state.dispatch(Command::SelectChannel(ConversationId(2)));
        let added = state.insert_channel_history(
            network,
            "#A",
            vec![
                line("old one", Some(10), Some("h1")),
                line("old two", Some(20), None),
                // The live line again: already shown, not added.
                line("live", Some(30), Some("live1")),
            ],
        );
        assert_eq!(added, 2);
        assert_eq!(
            texts(&state, 0),
            ["alice has joined", "old one", "old two", "live"]
        );
        let messages = &state.conversations()[0].messages;
        assert!(
            messages
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert_eq!(messages[1].provenance, Provenance::Requested);
        assert_eq!(messages[2].provenance, Provenance::Requested);
        assert_eq!(messages[3].provenance, Provenance::Live);
        assert!(!state.is_unread(ConversationId(1)), "history is not news");
        // Unrelated channels are untouched, and later lines follow.
        assert_eq!(texts(&state, 1), ["other channel"]);
        state.append_channel_message(network, "#a", "carol", "later", false, false);
        let messages = &state.conversations()[0].messages;
        assert_eq!(messages.last().unwrap().text, "later");
        assert!(
            messages
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        // A second reply to the same request is stale.
        assert_eq!(
            state.insert_channel_history(network, "#a", vec![line("again", None, None)]),
            0
        );
    }

    #[test]
    fn requested_history_skips_what_playback_already_delivered() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "played",
            false,
            meta(Some(5), None, Provenance::Replayed),
        );
        state.history_requested(network, "#a");
        state.insert_channel_history(
            network,
            "#a",
            vec![line("played", Some(5), None), line("newer", Some(6), None)],
        );
        assert_eq!(texts(&state, 0), ["played", "newer"]);
        // Playback repeating requested history later is dropped too.
        state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "newer",
            false,
            meta(Some(6), None, Provenance::Replayed),
        );
        assert_eq!(texts(&state, 0), ["played", "newer"]);
    }

    #[test]
    fn stale_history_replies_are_ignored() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        // Never requested.
        assert_eq!(
            state.insert_channel_history(network, "#a", vec![line("x", None, None)]),
            0
        );
        // The connection that asked ended.
        state.history_requested(network, "#a");
        state.set_status(network, ConnectionStatus::Disconnected("gone".into()));
        assert_eq!(
            state.insert_channel_history(network, "#a", vec![line("x", None, None)]),
            0
        );
        // A new session replaced the conversation.
        state.history_requested(network, "#a");
        state.reset_network(network, vec!["#a".into()]);
        assert_eq!(
            state.insert_channel_history(network, "#a", vec![line("x", None, None)]),
            0
        );
        assert!(texts(&state, 0).is_empty());
        assert!(state.pending_history.is_empty());
    }

    #[test]
    fn requested_history_is_bounded_by_the_reserve_and_the_log_limit() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        for index in 0..MAX_RETAINED - 10 {
            state.append_channel_message(
                network,
                "#a",
                "bob",
                &format!("live {index}"),
                false,
                false,
            );
        }
        state.history_requested(network, "#a");
        state.append_channel_message(network, "#a", "bob", "after", false, false);
        let lines = (0..HISTORY_RESERVE + 50)
            .map(|index| line(&format!("h{index}"), None, None))
            .collect();
        assert_eq!(
            state.insert_channel_history(network, "#a", lines),
            HISTORY_RESERVE
        );
        let messages = &state.conversations()[0].messages;
        assert!(messages.len() <= MAX_RETAINED);
        assert_eq!(messages.last().unwrap().text, "after");
        assert!(
            messages
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        let history = messages.iter().filter(|m| m.is_history()).count();
        assert_eq!(history, HISTORY_RESERVE);
    }

    /// A peer's messages as the IRC adapter routes them.
    fn private(
        state: &mut AppState,
        network: NetworkId,
        nick: &str,
        text: &str,
    ) -> Option<ConversationId> {
        let key = nick.to_lowercase();
        let id = state.private_conversation(network, &key, nick, true)?;
        state.append_conversation_message(id, nick, text, false, MessageMeta::live(), true);
        Some(id)
    }

    #[test]
    fn private_conversations_are_created_once_per_peer_and_network() {
        let mut state = two_networks();
        state.set_status(NetworkId(1), ConnectionStatus::Registered);
        let first = private(&mut state, NetworkId(1), "Bob", "hi").unwrap();
        let again = private(&mut state, NetworkId(1), "bob", "again").unwrap();
        assert_eq!(first, again, "the adapter's folded key decides");
        // The same nickname on another server is another person.
        let other = private(&mut state, NetworkId(2), "Bob", "elsewhere").unwrap();
        assert_ne!(first, other);
        let bob = state
            .conversations()
            .iter()
            .find(|c| c.id == first)
            .unwrap();
        assert_eq!(bob.name, "bob", "follows the spelling used now");
        assert!(bob.is_private());
        let texts: Vec<_> = bob.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["hi", "again"]);
        assert!(state.is_unread(first));
        assert!(state.is_active_channel(first), "registered: can send");
        assert!(
            !state.is_active_channel(other),
            "network 2 is not connected"
        );
        // Channels stay first within their network, private ones after.
        state.joined_channel(NetworkId(1), "#late");
        assert_eq!(
            names(&state),
            [
                (1, "#a".into()),
                (1, "#b".into()),
                (1, "#late".into()),
                (1, "bob".into()),
                (2, "#a".into()),
                (2, "Bob".into()),
            ]
        );
        // A channel lookup never finds a private conversation.
        assert!(state.channel_id(NetworkId(1), "bob").is_none());
        state.append_channel_message(NetworkId(1), "bob", "x", "y", false, false);
        assert_eq!(
            state
                .conversations()
                .iter()
                .find(|c| c.id == first)
                .unwrap()
                .messages
                .len(),
            2
        );
    }

    #[test]
    fn notices_and_overflow_do_not_create_private_conversations() {
        let mut state = AppState::live("irc.example".into(), vec![]);
        let network = NetworkId(1);
        assert!(
            state
                .private_conversation(network, "nickserv", "NickServ", false)
                .is_none()
        );
        assert!(state.conversations().is_empty());
        for index in 0..MAX_PRIVATE_CONVERSATIONS_PER_NETWORK {
            assert!(private(&mut state, network, &format!("n{index}"), "x").is_some());
        }
        assert!(private(&mut state, network, "one-too-many", "x").is_none());
        // Channels are still allowed.
        state.joined_channel(network, "#room");
        assert!(state.channel_id(network, "#room").is_some());
    }

    #[test]
    fn renames_follow_the_peer_but_never_merge_conversations() {
        let mut state = AppState::live("irc.example".into(), vec![]);
        let network = NetworkId(1);
        let bob = private(&mut state, network, "bob", "hi").unwrap();
        assert_eq!(
            state.rename_private(network, "bob", "robert", "Robert"),
            Some(bob)
        );
        assert_eq!(state.private_id(network, "robert"), Some(bob));
        assert!(state.private_id(network, "bob").is_none());
        // A new user taking the old nickname starts a new conversation.
        let newcomer = private(&mut state, network, "bob", "who am i").unwrap();
        assert_ne!(newcomer, bob);
        // Renaming into an existing conversation leaves both alone.
        assert_eq!(state.rename_private(network, "robert", "bob", "bob"), None);
        assert_eq!(state.private_id(network, "robert"), Some(bob));
        assert_eq!(state.private_id(network, "bob"), Some(newcomer));
        // Unknown users change nothing; a case-only change keeps the key.
        assert_eq!(state.rename_private(network, "carol", "dave", "dave"), None);
        assert_eq!(
            state.rename_private(network, "bob", "bob", "BOB"),
            Some(newcomer)
        );
        let texts: Vec<_> = state
            .conversations()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(texts, ["Robert", "BOB"]);
    }

    #[test]
    fn private_conversations_end_with_their_session_and_can_be_closed() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        state.set_status(network, ConnectionStatus::Registered);
        let bob = private(&mut state, network, "bob", "hi").unwrap();
        state.dispatch(Command::SelectChannel(bob));
        state.set_status(network, ConnectionStatus::Disconnected("gone".into()));
        assert!(
            !state.is_active_channel(bob),
            "cannot send while disconnected"
        );
        state.set_status(network, ConnectionStatus::Registered);
        assert!(state.is_active_channel(bob));
        // Channels cannot be closed this way.
        let channel = state.channel_id(network, "#a").unwrap();
        assert!(!state.close_private(channel));
        assert!(state.close_private(bob));
        assert_eq!(state.selection(), Selection::Server(network));
        assert!(!state.is_unread(bob) && !state.is_active_channel(bob));
        // A new session keeps only the configured channels.
        let carol = private(&mut state, network, "carol", "hi").unwrap();
        let removed = state.reset_network(network, vec!["#a".into()]);
        assert!(removed.contains(&carol));
        assert!(state.private_id(network, "carol").is_none());
    }

    #[test]
    fn private_messages_use_timeline_identity_and_bounds() {
        let mut state = AppState::live("irc.example".into(), vec![]);
        let network = NetworkId(1);
        let bob = state
            .private_conversation(network, "bob", "bob", true)
            .unwrap();
        assert!(state.append_conversation_message(
            bob,
            "bob",
            "hi",
            false,
            meta(Some(1), Some("p1"), Provenance::Live),
            true,
        ));
        assert!(!state.append_conversation_message(
            bob,
            "bob",
            "hi",
            false,
            meta(Some(1), Some("p1"), Provenance::Replayed),
            true,
        ));
        // Our own lines do not mark it unread.
        state.dispatch(Command::SelectServer(network));
        state.dispatch(Command::SelectChannel(bob));
        state.dispatch(Command::SelectServer(network));
        state.append_conversation_message(bob, "me", "sent", false, MessageMeta::live(), false);
        assert!(!state.is_unread(bob));
        for index in 0..MAX_RETAINED + 5 {
            state.append_conversation_message(
                bob,
                "bob",
                &index.to_string(),
                false,
                MessageMeta::live(),
                true,
            );
        }
        let messages = &state.conversations()[0].messages;
        assert!(messages.len() <= MAX_RETAINED);
        assert!(
            messages
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    #[test]
    fn avatars_follow_message_sequences_and_network_lifetime() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        state.joined_channel(network, "#a");
        state.append_channel_message(network, "#a", "bob", "before", false, false);
        state.set_avatar(network, "bob", Some("https://example.com/b.png"));
        state.append_channel_message(network, "#a", "bob", "after", false, false);
        let messages = &state.conversations()[0].messages;
        let avatar = |state: &AppState, index: usize| {
            let message = &state.conversations()[0].messages[index];
            state
                .avatars()
                .for_message(network, "bob", message.sequence)
                .map(|avatar| avatar.to_string())
        };
        assert_eq!(messages.len(), 2);
        assert_eq!(avatar(&state, 0), None);
        assert_eq!(
            avatar(&state, 1).as_deref(),
            Some("https://example.com/b.png")
        );

        // Reconnecting ends the occupancy; the shown message keeps it.
        state.end_avatars(network);
        state.append_channel_message(network, "#a", "bob", "reconnected", false, false);
        assert_eq!(
            avatar(&state, 1).as_deref(),
            Some("https://example.com/b.png")
        );
        assert_eq!(avatar(&state, 2), None);

        // A fresh session clears the logs and with them every avatar.
        state.set_avatar(network, "bob", Some("x"));
        state.reset_network(network, vec!["#a".into()]);
        assert_eq!(state.avatars().len(network), (0, 0));
        state.set_avatar(network, "bob", Some("x"));
        state.sync_networks(&[]);
        assert!(state.avatars().is_empty(), "removed server");
    }

    /// #a joined on a network that can page, with our JOIN, recent history
    /// (placed after the JOIN) and one live line.
    fn paging_channel() -> (AppState, NetworkId, ConversationId) {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into(), "#b".into()]);
        let network = NetworkId(1);
        let id = ConversationId(1);
        state.set_history_paging(network, true);
        state.joined_channel(network, "#a");
        state.append_channel_activity_at(
            network,
            "#a",
            "alice has joined".into(),
            meta(Some(1_000), None, Provenance::Live),
        );
        state.history_requested(network, "#a");
        state.insert_channel_history(
            network,
            "#a",
            vec![
                line("h1", Some(500), Some("m500")),
                line("h2", Some(600), Some("m600")),
            ],
        );
        state.append_channel_message_at(
            network,
            "#a",
            "carol",
            "live",
            false,
            meta(Some(1_100), Some("m1100"), Provenance::Live),
        );
        (state, network, id)
    }

    fn page(range: std::ops::Range<u64>) -> Vec<timeline::TimelineLine> {
        range
            .map(|n| line(&format!("p{n}"), Some(n), Some(&format!("m{n}"))))
            .collect()
    }

    fn ascending(state: &AppState, index: usize) -> bool {
        state.conversations()[index]
            .messages
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    }

    #[test]
    fn older_pages_refer_to_the_oldest_line_and_go_before_everything() {
        let (mut state, network, id) = paging_channel();
        let before: Vec<u64> = state.conversations()[0]
            .messages
            .iter()
            .map(|m| m.sequence)
            .collect();
        let request = state.request_older_history(id).expect("a page");
        // Not our JOIN (first by position) but the oldest by server time.
        assert_eq!(
            request.native_id.as_ref().map(|id| id.as_str()),
            Some("m500")
        );
        assert_eq!(request.timestamp.map(Timestamp::as_millis), Some(500));
        assert_eq!(request.limit, OLDER_PAGE_LIMIT);
        assert!(state.request_older_history(id).is_none(), "one at a time");

        // A live line arrives while the page is on its way.
        state.dispatch(Command::SelectChannel(ConversationId(2)));
        state.append_channel_message(network, "#a", "dave", "meanwhile", false, false);
        let last = state.last_message_sequence();
        let added = state.insert_older_history(id, request.request, page(400..403), false);
        assert_eq!(added, 3);
        assert_eq!(
            texts(&state, 0),
            [
                "p400",
                "p401",
                "p402",
                "alice has joined",
                "h1",
                "h2",
                "live",
                "meanwhile"
            ]
        );
        assert!(ascending(&state, 0));
        let messages = &state.conversations()[0].messages;
        assert!(
            messages[..3]
                .iter()
                .all(|m| m.provenance == Provenance::Requested)
        );
        // Nothing already shown moved or was renumbered, and older pages
        // do not count as news.
        let after: Vec<u64> = messages[3..].iter().map(|m| m.sequence).collect();
        assert_eq!(&after[..before.len()], before.as_slice());
        assert_eq!(state.last_message_sequence(), last);
        assert!(!state.is_highlighted(id));
        let rows = state.conversations()[0].messages.len();

        // The next page refers to the new oldest line and goes above it.
        let request = state.request_older_history(id).unwrap();
        assert_eq!(
            request.native_id.as_ref().map(|id| id.as_str()),
            Some("m400")
        );
        state.insert_older_history(id, request.request, page(390..400), false);
        assert_eq!(texts(&state, 0)[..2], ["p390", "p391"]);
        assert_eq!(state.conversations()[0].messages.len(), rows + 10);
        assert!(ascending(&state, 0));
        // Other conversations are untouched.
        assert!(texts(&state, 1).is_empty());
    }

    #[test]
    fn overlapping_older_pages_add_only_new_lines() {
        let (mut state, _, id) = paging_channel();
        let request = state.request_older_history(id).unwrap();
        state.insert_older_history(id, request.request, page(495..500), false);
        let request = state.request_older_history(id).unwrap();
        // The server repeats part of the previous page, recent history
        // (h1 = m500) and a live line, and repeats a line inside the page.
        let mut lines = page(490..498);
        lines.push(line("h1", Some(500), Some("m500")));
        lines.push(line("live", Some(1_100), Some("m1100")));
        lines.push(line("p491", Some(491), Some("m491")));
        let added = state.insert_older_history(id, request.request, lines, false);
        assert_eq!(added, 5);
        assert_eq!(
            texts(&state, 0)[..11],
            [
                "p490",
                "p491",
                "p492",
                "p493",
                "p494",
                "p495",
                "p496",
                "p497",
                "p498",
                "p499",
                "alice has joined"
            ]
        );
        assert!(ascending(&state, 0));
        // Without msgid (legacy encodings), server time, sender and text
        // identify a history repeat of a line already shown.
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        state.set_history_paging(network, true);
        state.joined_channel(network, "#a");
        state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "x",
            false,
            meta(Some(50), None, Provenance::Live),
        );
        let request = state.request_older_history(id).unwrap();
        assert!(request.native_id.is_none());
        assert_eq!(request.timestamp.map(Timestamp::as_millis), Some(50));
        let lines = vec![line("p40", Some(40), None), line("x", Some(50), None)];
        assert_eq!(
            state.insert_older_history(id, request.request, lines, false),
            1
        );
        assert_eq!(texts(&state, 0), ["p40", "x"]);
    }

    #[test]
    fn the_beginning_of_history_and_empty_pages_stop_paging_until_the_next_session() {
        let (mut state, network, id) = paging_channel();
        let request = state.request_older_history(id).unwrap();
        state.insert_older_history(id, request.request, page(400..402), true);
        assert!(
            state.request_older_history(id).is_none(),
            "beginning reached"
        );

        // A page with nothing new would only be asked for again.
        let (mut state, _, id) = paging_channel();
        let request = state.request_older_history(id).unwrap();
        let added = state.insert_older_history(
            id,
            request.request,
            vec![line("h2", Some(600), Some("m600"))],
            false,
        );
        assert_eq!(added, 0);
        assert!(state.request_older_history(id).is_none());

        // A failure also stops, and a wrong answer changes nothing.
        let (mut state, _, id) = paging_channel();
        let request = state.request_older_history(id).unwrap();
        state.older_history_failed(id, request.request + 1);
        assert!(state.request_older_history(id).is_none(), "still in flight");
        state.older_history_failed(id, request.request);
        assert!(state.request_older_history(id).is_none());

        // A new session may try again.
        state.set_status(network, ConnectionStatus::Disconnected("gone".into()));
        assert!(state.request_older_history(id).is_none(), "cannot page");
        state.set_history_paging(network, true);
        state.joined_channel(network, "#a");
        assert!(state.request_older_history(id).is_some());
    }

    #[test]
    fn stale_older_pages_are_ignored() {
        let (mut state, network, id) = paging_channel();
        let rows = state.conversations()[0].messages.len();

        // The connection ended while the page was on its way.
        let request = state.request_older_history(id).unwrap();
        state.set_status(network, ConnectionStatus::Disconnected("gone".into()));
        assert_eq!(
            state.insert_older_history(id, request.request, page(1..3), false),
            0
        );
        // The next session's request is not answered by the old page.
        state.set_history_paging(network, true);
        state.joined_channel(network, "#a");
        let current = state.request_older_history(id).unwrap();
        assert_ne!(current.request, request.request);
        assert_eq!(
            state.insert_older_history(id, request.request, page(1..3), false),
            0
        );
        assert_eq!(state.conversations()[0].messages.len(), rows);

        // The channel was left.
        state.parted_channel(network, "#a");
        assert_eq!(
            state.insert_older_history(id, current.request, page(1..3), false),
            0
        );
        assert!(state.request_older_history(id).is_none(), "not joined");

        // The conversation was replaced by a fresh session.
        state.joined_channel(network, "#a");
        let request = state.request_older_history(id).unwrap();
        state.reset_network(network, vec!["#a".into()]);
        let new_id = state.channel_id(network, "#a").unwrap();
        assert_ne!(new_id, id);
        assert_eq!(
            state.insert_older_history(id, request.request, page(1..3), false),
            0
        );
        assert_eq!(
            state.insert_older_history(new_id, request.request, page(1..3), false),
            0
        );
        assert!(state.older_history.is_empty());
    }

    #[test]
    fn older_pages_wait_for_recent_history_and_skip_private_or_unidentified_logs() {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        let network = NetworkId(1);
        let id = ConversationId(1);
        state.joined_channel(network, "#a");
        state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "x",
            false,
            meta(Some(5), Some("m5"), Provenance::Live),
        );
        assert!(state.request_older_history(id).is_none(), "no paging yet");
        state.set_history_paging(network, true);
        state.history_requested(network, "#a");
        assert!(
            state.request_older_history(id).is_none(),
            "recent history first"
        );
        state.insert_channel_history(network, "#a", Vec::new());
        assert!(state.request_older_history(id).is_some());

        // Lines without identity give nothing to refer to.
        let mut plain = AppState::live("irc.example".into(), vec!["#a".into()]);
        plain.set_history_paging(network, true);
        plain.joined_channel(network, "#a");
        plain.append_channel_message(network, "#a", "bob", "x", false, false);
        assert!(plain.request_older_history(id).is_none());

        // Private conversations are not paged yet.
        let peer = state
            .private_conversation(network, "bob", "bob", true)
            .unwrap();
        state.append_conversation_message(
            peer,
            "bob",
            "hi",
            false,
            meta(Some(9), Some("p9"), Provenance::Live),
            false,
        );
        state.set_status(network, ConnectionStatus::Registered);
        assert!(state.request_older_history(peer).is_none());
    }

    #[test]
    fn older_pages_stay_within_the_log_bound() {
        let (mut state, _, id) = paging_channel();
        let mut next = 1_000_000u64;
        loop {
            let Some(request) = state.request_older_history(id) else {
                break;
            };
            assert!(request.limit <= OLDER_PAGE_LIMIT && request.limit > 0);
            // A server returning more than asked for.
            let lines = page(next - 80..next);
            next -= 80;
            state.insert_older_history(id, request.request, lines, false);
            assert!(state.conversations()[0].messages.len() <= MAX_RETAINED);
        }
        let messages = &state.conversations()[0].messages;
        assert_eq!(messages.len(), MAX_RETAINED, "filled, never trimmed");
        assert!(ascending(&state, 0));
        assert_eq!(messages.last().unwrap().text, "live", "live lines stay");
        // Live traffic trims the oldest lines as before; paging may resume.
        for index in 0..10 {
            state.append_channel_message(
                NetworkId(1),
                "#a",
                "bob",
                &index.to_string(),
                false,
                false,
            );
        }
        assert!(state.conversations()[0].messages.len() <= MAX_RETAINED);
        assert!(state.request_older_history(id).is_some());
        assert!(state.duplicates[&id].len() < timeline::DUPLICATE_KEYS_PER_CONVERSATION);
    }

    fn live(state: &mut AppState, text: &str, millis: u64, msgid: Option<&str>) {
        state.append_channel_message_at(
            NetworkId(1),
            "#a",
            "bob",
            text,
            false,
            meta(Some(millis), msgid, Provenance::Live),
        );
    }

    /// #a and #b joined on a session with history, #a with two identified
    /// lines, then the link drops.
    fn cut_off() -> (AppState, NetworkId, ConversationId) {
        let mut state = AppState::live("irc.example".into(), vec!["#a".into(), "#b".into()]);
        let network = NetworkId(1);
        state.set_status(network, ConnectionStatus::Registered);
        state.set_history_paging(network, true);
        state.joined_channel(network, "#a");
        state.joined_channel(network, "#b");
        live(&mut state, "A0", 900, Some("m900"));
        live(&mut state, "A", 1_000, Some("m1000"));
        // Our own echo carries nothing identifying.
        state.append_channel_message(network, "#a", "alice", "mine", false, false);
        state.append_channel_message(network, "#b", "carol", "unidentified", false, false);
        state.set_status(network, ConnectionStatus::Disconnected("gone".into()));
        (state, network, ConversationId(1))
    }

    /// The next session rejoins #a, whose JOIN shows up before the missed
    /// lines are asked for.
    fn rejoin(state: &mut AppState, network: NetworkId, paging: bool) {
        state.set_status(network, ConnectionStatus::Registered);
        state.set_history_paging(network, paging);
        state.joined_channel(network, "#a");
        state.append_channel_activity_at(
            network,
            "#a",
            "alice has joined".into(),
            meta(Some(2_000), None, Provenance::Live),
        );
    }

    #[test]
    fn a_disconnect_records_where_each_joined_channel_was_cut_off() {
        let (state, _, id) = cut_off();
        let resume = state.history_resume(NetworkId(1));
        assert_eq!(resume.len(), 1, "#b has no identified line: {resume:?}");
        assert_eq!(resume[0].conversation, id);
        assert_eq!(resume[0].name, "#a");
        assert_eq!(
            resume[0].native_id.as_ref().map(|id| id.as_str()),
            Some("m1000")
        );
        assert_eq!(resume[0].timestamp.map(Timestamp::as_millis), Some(1_000));

        // Legacy encodings: server time only.
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        state.set_history_paging(NetworkId(1), true);
        state.joined_channel(NetworkId(1), "#a");
        live(&mut state, "A", 1_000, None);
        state.set_status(NetworkId(1), ConnectionStatus::Disconnected("gone".into()));
        let resume = state.history_resume(NetworkId(1));
        assert!(resume[0].native_id.is_none());
        assert_eq!(resume[0].timestamp.map(Timestamp::as_millis), Some(1_000));

        // Left on purpose, or a session without history: nothing to resume.
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        state.set_history_paging(NetworkId(1), true);
        state.joined_channel(NetworkId(1), "#a");
        live(&mut state, "A", 1_000, Some("m1"));
        state.parted_channel(NetworkId(1), "#a");
        state.set_status(NetworkId(1), ConnectionStatus::Disconnected("gone".into()));
        assert!(state.history_resume(NetworkId(1)).is_empty());
        let mut state = AppState::live("irc.example".into(), vec!["#a".into()]);
        state.joined_channel(NetworkId(1), "#a");
        live(&mut state, "A", 1_000, Some("m1"));
        state.set_status(NetworkId(1), ConnectionStatus::Disconnected("gone".into()));
        assert!(state.history_resume(NetworkId(1)).is_empty());
    }

    #[test]
    fn missed_lines_go_where_the_cut_was_without_duplicates_or_news() {
        let (mut state, network, id) = cut_off();
        rejoin(&mut state, network, true);
        state.history_resumed(network, "#a");
        // Live traffic, and a bouncer's own playback of a missed line,
        // while the missed lines are on their way.
        live(&mut state, "D", 2_100, Some("m2100"));
        state.append_channel_message_at(
            network,
            "#a",
            "bob",
            "C",
            false,
            meta(Some(1_600), Some("m1600"), Provenance::Replayed),
        );
        state.dispatch(Command::SelectChannel(ConversationId(2)));
        state.dispatch(Command::SelectChannel(id));
        let added = state.insert_resumed_history(
            network,
            "#a",
            vec![
                line("A", Some(1_000), Some("m1000")),
                line("B", Some(1_500), Some("m1500")),
                line("C", Some(1_600), Some("m1600")),
                line("D", Some(2_100), Some("m2100")),
            ],
            None,
        );
        assert_eq!(added, 1, "only B is new");
        assert_eq!(
            texts(&state, 0),
            ["A0", "A", "mine", "B", "alice has joined", "D", "C"]
        );
        assert!(ascending(&state, 0));
        assert_eq!(
            state.conversations()[0].messages[3].provenance,
            Provenance::Requested
        );
        assert!(!state.is_unread(id), "missed lines are not news");
        assert!(state.history_resume(network).is_empty(), "recovered");
        // The next disconnect cuts at the newest line again.
        state.set_status(network, ConnectionStatus::Disconnected("again".into()));
        assert_eq!(
            state.history_resume(network)[0]
                .native_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("m2100")
        );
    }

    #[test]
    fn a_recovery_that_hit_its_limit_says_lines_may_be_missing() {
        let (mut state, network, _) = cut_off();
        rejoin(&mut state, network, true);
        state.history_resumed(network, "#a");
        state.insert_resumed_history(
            network,
            "#a",
            vec![line("Y", Some(1_900), Some("m1900"))],
            Some("Some messages sent while disconnected are not shown.".into()),
        );
        let messages = &state.conversations()[0].messages;
        let note = &messages[3];
        assert!(note.activity && note.is_history());
        assert!(note.text.starts_with("Some messages"));
        assert_eq!(messages[4].text, "Y");
        assert!(ascending(&state, 0));
    }

    #[test]
    fn results_of_an_earlier_attempt_are_not_applied_to_a_later_one() {
        let (mut state, network, _) = cut_off();
        // Attempt 1 rejoins and asks, then drops before the answer.
        rejoin(&mut state, network, true);
        state.history_resumed(network, "#a");
        live(&mut state, "E", 2_200, Some("m2200"));
        state.set_status(network, ConnectionStatus::Disconnected("again".into()));
        // Its answer arriving late changes nothing.
        let stale = vec![line("B", Some(1_500), Some("m1500"))];
        assert_eq!(state.insert_resumed_history(network, "#a", stale, None), 0);
        // The earliest cut is kept, not the attempt's newest line.
        let resume = state.history_resume(network);
        assert_eq!(
            resume[0].native_id.as_ref().map(|id| id.as_str()),
            Some("m1000")
        );
        // Attempt 2 dies before rejoining: nothing changes.
        state.set_status(network, ConnectionStatus::Registered);
        state.set_history_paging(network, true);
        state.set_status(network, ConnectionStatus::Disconnected("third".into()));
        assert_eq!(state.history_resume(network), resume);
        // Attempt 3 gets its answer, at the original cut.
        rejoin(&mut state, network, true);
        state.history_resumed(network, "#a");
        state.insert_resumed_history(
            network,
            "#a",
            vec![
                line("B", Some(1_500), Some("m1500")),
                line("E", Some(2_200), Some("m2200")),
            ],
            None,
        );
        assert_eq!(texts(&state, 0)[..4], ["A0", "A", "mine", "B"]);
        assert_eq!(texts(&state, 0).iter().filter(|t| **t == "E").count(), 1);
        assert!(ascending(&state, 0));
    }

    #[test]
    fn recovery_is_given_up_without_history_or_a_usable_reference() {
        // The next session has no chathistory: its disconnect drops the cut.
        let (mut state, network, _) = cut_off();
        rejoin(&mut state, network, false);
        state.set_status(network, ConnectionStatus::Disconnected("again".into()));
        assert!(state.history_resume(network).is_empty());

        // The server accepts no reference we have: the latest lines are
        // asked for instead and placed as usual, after our JOIN.
        let (mut state, network, _) = cut_off();
        rejoin(&mut state, network, true);
        state.history_requested(network, "#a");
        assert!(state.history_resume(network).is_empty());
        state.insert_channel_history(network, "#a", vec![line("B", Some(1_500), Some("m1500"))]);
        assert_eq!(
            texts(&state, 0),
            ["A0", "A", "mine", "alice has joined", "B"]
        );

        // Another server configuration starts a fresh session.
        let (mut state, network, _) = cut_off();
        state.reset_network(network, vec!["#a".into()]);
        assert!(state.history_resume(network).is_empty());
        assert!(state.resume_points.is_empty());
    }
}
