// SPDX-License-Identifier: MPL-2.0
//! Application state and commands, independent of any rendering framework.
pub mod amazon;
pub mod attachments;
pub mod avatars;
pub mod links;
pub mod notifications;
pub mod own_avatar;
pub mod timeline;

use std::{
    collections::{HashMap, HashSet},
    time::SystemTime,
};

use cayenchat_model::{
    Conversation, ConversationId, ConversationKind, Highlights, Links, Message, NativeMessageId,
    Network, NetworkId, Provenance, TimeOfDay, Timestamp, display, names::CaseMapping,
};
use timeline::DuplicateFilter;
pub use timeline::{Kind, MessageMeta};

/// Whether adding a message marks its conversation unread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    Mark,
    Leave,
}

/// What [`AppState::private_conversation`] does when the peer has no
/// conversation yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    Create,
    Ignore,
}

/// What a timeline item is, for [`new_message`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Text,
    /// A nick change, a quit, a note.
    Activity,
    /// An activity the user should not miss: a kick, a refused join.
    Alert,
}

/// Upper bound on conversations per network, so a hostile server or bouncer
/// cannot grow memory without limit by announcing endless channel joins.
const MAX_CONVERSATIONS_PER_NETWORK: usize = 1_000;
/// Channel names remembered per network for the chosen tree order.
const MAX_ORDER_NAMES: usize = 2_000;
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
    /// Channel conversations by network and folded name (`CaseMapping::fold`), so
    /// an incoming line finds its channel under IRC case mapping without
    /// comparing it with every conversation.
    channel_ids: HashMap<(NetworkId, String), ConversationId>,
    /// Networks whose server advertised other than the default mapping.
    casemappings: HashMap<NetworkId, CaseMapping>,
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
    /// The channel order the user chose per network (lowercased names, parted
    /// channels included so a rejoined one returns to its place).
    channel_orders: HashMap<NetworkId, Vec<String>>,
    /// Each name of `channel_orders` to its position there.
    channel_ranks: HashMap<NetworkId, HashMap<String, usize>>,
}

/// Where a channel or private conversation moves within its group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveTo {
    Up,
    Down,
    First,
    Last,
    /// Into the place of another conversation of the same group.
    Place(ConversationId),
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
                            account: None,
                            sender: sender.into(),
                            text: text.into(),
                            highlights: Default::default(),
                            links: Links::new(links::find(text)),
                            activity: false,
                            alert: false,
                            notice: false,
                            provenance: Provenance::Live,
                            delivery_failed: false,
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
            channel_ids: channel_index(&conversations, |_| CaseMapping::default()),
            casemappings: HashMap::new(),
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
            channel_orders: HashMap::new(),
            channel_ranks: HashMap::new(),
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
            channel_ids: channel_index(&conversations, |_| CaseMapping::default()),
            casemappings: HashMap::new(),
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
            channel_orders: HashMap::new(),
            channel_ranks: HashMap::new(),
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
            channel_ids: HashMap::new(),
            casemappings: HashMap::new(),
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
            channel_orders: HashMap::new(),
            channel_ranks: HashMap::new(),
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
                state.ensure_configured_channel(config.id, &channel);
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
            self.channel_orders.remove(&id);
            self.channel_ranks.remove(&id);
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
                self.ensure_configured_channel(config.id, channel);
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
            let id = self.ensure_configured_channel(network, &channel);
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
        self.channel_ids.retain(|(owner, _), _| *owner != network);
        self.casemappings.remove(&network);
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

    /// The conversations of `network` that share `id`'s group (channels, or
    /// private conversations), as the range they occupy. Each group is
    /// contiguous in `conversations`.
    fn group_range(&self, id: ConversationId) -> Option<std::ops::Range<usize>> {
        let at = self.conversations.iter().position(|c| c.id == id)?;
        let (network, private) = (
            self.conversations[at].network,
            self.conversations[at].is_private(),
        );
        let same = |c: &Conversation| c.network == network && c.is_private() == private;
        let start = self.conversations[..at]
            .iter()
            .rposition(|c| !same(c))
            .map_or(0, |i| i + 1);
        let end = self.conversations[at..]
            .iter()
            .position(|c| !same(c))
            .map_or(self.conversations.len(), |i| at + i);
        Some(start..end)
    }

    /// Moves a conversation within its network's channels (or private
    /// conversations); channels never cross servers or pass private
    /// conversations. Returns the network when the order changed, after
    /// recording it as that network's chosen order.
    pub fn move_conversation(&mut self, id: ConversationId, to: MoveTo) -> Option<NetworkId> {
        let range = self.group_range(id)?;
        let from = self.conversations[range.clone()]
            .iter()
            .position(|c| c.id == id)?;
        let last = range.len() - 1;
        let target = match to {
            MoveTo::Up => from.saturating_sub(1),
            MoveTo::Down => (from + 1).min(last),
            MoveTo::First => 0,
            MoveTo::Last => last,
            MoveTo::Place(other) => self.conversations[range.clone()]
                .iter()
                .position(|c| c.id == other)?,
        };
        if target == from {
            return None;
        }
        let group = &mut self.conversations[range];
        if from < target {
            group[from..=target].rotate_left(1);
        } else {
            group[target..=from].rotate_right(1);
        }
        let network = self.conversation(id)?.network;
        if !self.conversation(id)?.is_private() {
            let order = self.merged_channel_order(network);
            self.store_channel_order(network, order);
        }
        Some(network)
    }

    /// The order to remember for `network`: its channels as shown, with
    /// remembered channels that are not present kept after the shown channel
    /// that preceded them.
    fn merged_channel_order(&self, network: NetworkId) -> Vec<String> {
        let mut order: Vec<String> = self
            .conversations
            .iter()
            .filter(|c| c.network == network && !c.is_private())
            .map(|c| c.name.to_ascii_lowercase())
            .collect();
        let mut previous: Option<String> = None;
        for name in self.channel_orders.get(&network).into_iter().flatten() {
            if let Some(position) = order.iter().position(|n| n == name) {
                previous = Some(order[position].clone());
                continue;
            }
            let at = previous
                .as_ref()
                .and_then(|p| order.iter().position(|n| n == p))
                .map_or(0, |i| i + 1);
            order.insert(at, name.clone());
            previous = Some(name.clone());
        }
        order
    }

    /// The chosen channel order of `network` to save.
    pub fn channel_order(&self, network: NetworkId) -> Vec<String> {
        self.channel_orders
            .get(&network)
            .cloned()
            .unwrap_or_default()
    }

    /// Adopts a saved channel order for `network` and arranges its channels
    /// by it. Channels it does not list stay at the end in their current order.
    pub fn set_channel_order(&mut self, network: NetworkId, order: Vec<String>) {
        self.store_channel_order(network, order);
        self.apply_channel_order(network);
    }

    /// Remembers `order` for `network` (bounded) with a rank table, so a
    /// channel's place is found without searching the list.
    fn store_channel_order(&mut self, network: NetworkId, mut order: Vec<String>) {
        order.truncate(MAX_ORDER_NAMES);
        if order.is_empty() {
            self.channel_orders.remove(&network);
            self.channel_ranks.remove(&network);
            return;
        }
        let mut ranks = HashMap::with_capacity(order.len());
        for (rank, name) in order.iter().enumerate() {
            ranks.entry(name.clone()).or_insert(rank);
        }
        self.channel_orders.insert(network, order);
        self.channel_ranks.insert(network, ranks);
    }

    /// Where a new channel named `name` goes among `network`'s channels
    /// (`start..end` in `conversations`): after those ranked before it, and
    /// at the end when the order does not list it.
    fn channel_slot(&self, network: NetworkId, name: &str, start: usize, end: usize) -> usize {
        let Some(ranks) = self.channel_ranks.get(&network) else {
            return end;
        };
        let rank = |name: &str| {
            ranks
                .get(&name.to_ascii_lowercase())
                .copied()
                .unwrap_or(usize::MAX)
        };
        let new = rank(name);
        start + self.conversations[start..end].partition_point(|c| rank(&c.name) <= new)
    }

    fn apply_channel_order(&mut self, network: NetworkId) {
        let Some(ranks) = self.channel_ranks.get(&network) else {
            return;
        };
        let rank = |name: &str| {
            ranks
                .get(&name.to_ascii_lowercase())
                .copied()
                .unwrap_or(usize::MAX)
        };
        let Some(start) = self
            .conversations
            .iter()
            .position(|c| c.network == network && !c.is_private())
        else {
            return;
        };
        let end = self.conversations[start..]
            .iter()
            .position(|c| c.network != network || c.is_private())
            .map_or(self.conversations.len(), |i| start + i);
        self.conversations[start..end].sort_by_cached_key(|c| rank(&c.name));
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

    /// Whether `id` had a registered connection that has not ended yet.
    pub fn is_registered(&self, id: NetworkId) -> bool {
        self.active_servers.contains(&id)
    }

    /// Adds an activity line to every channel and conversation of `id`, so
    /// each log shows where the connection was lost.
    pub fn append_network_activity(&mut self, id: NetworkId, text: &str) {
        let ids: Vec<_> = self
            .conversations
            .iter()
            .filter(|conversation| conversation.network == id)
            .map(|conversation| conversation.id)
            .collect();
        for conversation in ids {
            self.append_conversation_activity(conversation, text.into());
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
        let mut message = new_message("server".into(), text, Line::Text, meta);
        message.sequence = self.next_sequence();
        let messages = self.server_messages.entry(id).or_default();
        push_bounded(messages, message);
    }

    fn next_sequence(&mut self) -> u64 {
        self.next_message_sequence += 1;
        self.next_message_sequence
    }

    /// How `network`'s server compares channel names: RFC 1459 until it
    /// advertises otherwise.
    pub fn casemapping(&self, network: NetworkId) -> CaseMapping {
        self.casemappings.get(&network).copied().unwrap_or_default()
    }

    /// Records the server's channel-name comparison and re-keys the network's
    /// channels under it, also when it is the default: configured channels
    /// were added without merging. Channels that differ under the new mapping
    /// stay separate conversations; where they fold alike, the first one is
    /// reached. Only names and IDs are read, never the logs.
    pub fn set_casemapping(&mut self, network: NetworkId, mapping: CaseMapping) {
        if mapping == CaseMapping::default() {
            self.casemappings.remove(&network);
        } else {
            self.casemappings.insert(network, mapping);
        }
        self.channel_ids.retain(|(owner, _), _| *owner != network);
        for conversation in &self.conversations {
            if conversation.network == network && conversation.kind == ConversationKind::Channel {
                self.channel_ids
                    .entry((network, mapping.fold(&conversation.name)))
                    .or_insert(conversation.id);
            }
        }
    }

    /// The channel conversation `name` of `network`.
    /// Names compare as the server compares them ([`Self::casemapping`]).
    pub fn channel_id(&self, network: NetworkId, name: &str) -> Option<ConversationId> {
        let key = (network, self.casemapping(network).fold(name));
        self.channel_ids.get(&key).copied()
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
    /// (the peer's current display name). It is created when `missing` is
    /// [`Missing::Create`] and the network has room; `None` means the caller keeps the
    /// message in the server log. An existing conversation takes `name`,
    /// so it follows the spelling the peer uses now.
    pub fn private_conversation(
        &mut self,
        network: NetworkId,
        peer_key: &str,
        name: &str,
        missing: Missing,
    ) -> Option<ConversationId> {
        if let Some(id) = self.private_id(network, peer_key) {
            if let Some(conversation) = self.conversation_mut(id)
                && conversation.name != name
            {
                conversation.name = name.to_owned();
            }
            return Some(id);
        }
        if missing == Missing::Ignore
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
        let conversation = self.conversation_mut(id)?;
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

    /// Adds a message to conversation `id` (any kind). [`Unread::Mark`]
    /// marks it unread when it is not selected. Returns `false` for a
    /// duplicate.
    pub fn append_conversation_message(
        &mut self,
        id: ConversationId,
        sender: &str,
        text: &str,
        kind: Kind,
        meta: MessageMeta,
        unread: Unread,
    ) -> bool {
        let (text, meta) = notice_marked(text, kind, meta);
        let mut message = new_message(sender.into(), text, Line::Text, meta);
        message.notice = kind.is_notice();
        if !self.append_to_conversation(id, message) {
            return false;
        }
        if unread == Unread::Mark {
            self.mark_unread(id);
        }
        true
    }

    /// Adds an activity line (a nick change, a quit) to conversation `id`.
    pub fn append_conversation_activity(&mut self, id: ConversationId, text: String) {
        self.append_to_conversation(
            id,
            new_message(String::new(), text, Line::Activity, MessageMeta::live()),
        );
    }

    /// Marks conversation `id` highlighted until it is selected.
    pub fn highlight(&mut self, id: ConversationId) {
        if self.selected != Selection::Channel(id) && self.conversations.iter().any(|c| c.id == id)
        {
            self.highlighted.insert(id);
        }
    }

    /// A configured channel, added before the server has said how it
    /// compares names. Only an identical name is the same conversation, so
    /// names the server may keep apart are not merged; [`Self::set_casemapping`]
    /// then decides which of them the index reaches.
    fn ensure_configured_channel(
        &mut self,
        network: NetworkId,
        name: &str,
    ) -> Option<ConversationId> {
        let exact = self.conversations.iter().find(|conversation| {
            conversation.network == network
                && conversation.kind == ConversationKind::Channel
                && conversation.name == name
        });
        if let Some(conversation) = exact {
            return Some(conversation.id);
        }
        self.add_conversation(network, ConversationKind::Channel, name)
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
        if !private {
            let key = (network, self.casemapping(network).fold(name));
            self.channel_ids.entry(key).or_insert(id);
        }
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
        let index = match self
            .conversations
            .iter()
            .position(|c| c.network == network && !c.is_private())
        {
            Some(start) if !private => self.channel_slot(network, name, start, index),
            _ => index,
        };
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
            if let Some(channel) = self.conversation_mut(id) {
                channel.members.clear();
            }
        }
    }

    pub fn set_members(&mut self, network: NetworkId, name: &str, members: Vec<String>) {
        if let Some(id) = self.channel_id(network, name)
            && let Some(channel) = self.conversation_mut(id)
        {
            channel.members = sorted_members(members);
        }
    }

    /// Sets the topic of a joined channel (empty clears it). Topics of
    /// channels not in the tree are ignored.
    pub fn set_topic(&mut self, network: NetworkId, name: &str, topic: &str) {
        if let Some(id) = self.channel_id(network, name)
            && let Some(channel) = self.conversation_mut(id)
        {
            channel.topic = display::neutralize_bidi(topic).into_owned();
        }
    }

    pub fn append_channel_message(
        &mut self,
        network: NetworkId,
        name: &str,
        sender: &str,
        text: &str,
        kind: Kind,
        provenance: Provenance,
    ) {
        self.append_channel_message_at(
            network,
            name,
            sender,
            text,
            kind,
            MessageMeta::with_provenance(provenance),
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
        kind: Kind,
        meta: MessageMeta,
    ) -> bool {
        // Only joined or configured channels get a conversation; anything else
        // a server sends lands in the bounded server log instead.
        let (text, meta) = notice_marked(text, kind, meta);
        let Some(id) = self.channel_id(network, name) else {
            let prefix = format!("{name} <{sender}> ");
            let meta = meta.after_prefix(prefix.len());
            self.append_server_message_at(network, prefix + &text, meta);
            return true;
        };
        let mut message = new_message(sender.into(), text, Line::Text, meta);
        message.notice = kind.is_notice();
        if !self.append_to_conversation(id, message) {
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
            self.append_to_conversation(id, new_message(String::new(), text, Line::Activity, meta));
        }
    }

    /// Like [`AppState::append_channel_activity_at`], but the line is
    /// emphasized (a kick, a refused join).
    pub fn append_channel_alert_at(
        &mut self,
        network: NetworkId,
        name: &str,
        text: String,
        meta: MessageMeta,
    ) {
        if let Some(id) = self.channel_id(network, name) {
            self.append_to_conversation(id, new_message(String::new(), text, Line::Alert, meta));
        }
    }

    /// Like [`AppState::append_channel_alert_at`], but opens the channel's
    /// conversation first when it is missing, for a refusal of a join this
    /// client asked for (within the conversation limit).
    pub fn append_requested_join_alert_at(
        &mut self,
        network: NetworkId,
        name: &str,
        text: String,
        meta: MessageMeta,
    ) {
        if self.ensure_channel(network, name).is_some() {
            self.append_channel_alert_at(network, name, text, meta);
        }
    }

    /// The sequence of the newest message added anywhere, so a caller that
    /// just appended one can find it again ([`AppState::confirm_message`]).
    pub fn latest_sequence(&self) -> u64 {
        self.next_message_sequence
    }

    /// The source confirmed our message `sequence` of conversation `id`:
    /// its final text (when the source changed it), identifier and time
    /// replace the local ones. The confirmed message is remembered by the
    /// duplicate filter, so history that repeats it is dropped. Returns
    /// `false` when the message is gone.
    pub fn confirm_message(
        &mut self,
        id: ConversationId,
        sequence: u64,
        text: Option<String>,
        kind: Kind,
        meta: MessageMeta,
    ) -> bool {
        let Some(conversation) = self.conversation_mut(id) else {
            return false;
        };
        let Ok(index) = conversation
            .messages
            .binary_search_by_key(&sequence, |message| message.sequence)
        else {
            return false;
        };
        let message = &mut conversation.messages[index];
        if let Some(text) = text {
            let text = display::neutralize_bidi_owned(text);
            message.text = if kind.is_notice() {
                format!("[NOTICE] {text}")
            } else {
                text
            };
            message.links = Links::new(links::find(&message.text));
        }
        message.notice = kind.is_notice();
        message.delivery_failed = false;
        if meta.native_id.is_some() {
            message.native_id = meta.native_id;
        }
        if let Some(time) = meta.server_time {
            message.timestamp = Timestamp::from_system_time(time);
            message.time = display_time(Some(time));
        }
        let confirmed = message.clone();
        if confirmed.native_id.is_some() || confirmed.timestamp.is_some() {
            self.duplicates.entry(id).or_default().admit(&confirmed);
        }
        true
    }

    /// Marks our message `sequence` of conversation `id` as not delivered.
    pub fn fail_message(&mut self, id: ConversationId, sequence: u64) {
        if let Some(conversation) = self.conversation_mut(id)
            && let Ok(index) = conversation
                .messages
                .binary_search_by_key(&sequence, |message| message.sequence)
        {
            conversation.messages[index].delivery_failed = true;
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
            None => self.history_requested_for(id),
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
        self.history_requested_for(id);
    }

    /// [`AppState::history_requested`] for any conversation, such as a
    /// private one found by the source's target discovery.
    pub fn history_requested_for(&mut self, id: ConversationId) {
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
        self.insert_history_for(id, lines, gap_note)
    }

    /// [`AppState::insert_resumed_history`] for any conversation.
    pub fn insert_history_for(
        &mut self,
        id: ConversationId,
        lines: Vec<timeline::TimelineLine>,
        gap_note: Option<String>,
    ) -> usize {
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
                Line::Activity,
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
                Line::Text,
                MessageMeta {
                    provenance: Provenance::Requested,
                    ..line.meta
                },
            );
            message.notice = line.notice;
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
        let conversation = self.conversation(id)?;
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
                let mut message = new_message(
                    line.sender,
                    line.text,
                    Line::Text,
                    MessageMeta {
                        provenance: Provenance::Requested,
                        ..line.meta
                    },
                );
                message.notice = line.notice;
                message
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

    pub fn conversation(&self, id: ConversationId) -> Option<&Conversation> {
        self.conversations.iter().find(|c| c.id == id)
    }

    pub fn conversation_mut(&mut self, id: ConversationId) -> Option<&mut Conversation> {
        self.conversations.iter_mut().find(|c| c.id == id)
    }

    pub fn selection(&self) -> Selection {
        self.selected
    }

    pub fn selected_channel(&self) -> Option<&Conversation> {
        let Selection::Channel(id) = self.selected else {
            return None;
        };
        self.conversation(id)
    }

    /// The selected server, or the server of the selected channel; `None`
    /// only when no server is configured.
    pub fn selected_network(&self) -> Option<&Network> {
        let id = match self.selected {
            Selection::Server(id) => id,
            Selection::Channel(id) => {
                self.conversation(id)
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

/// [`AppState::channel_ids`] for conversations built directly.
/// Where two conversations fold alike, the first one is kept.
fn channel_index(
    conversations: &[Conversation],
    mapping: impl Fn(NetworkId) -> CaseMapping,
) -> HashMap<(NetworkId, String), ConversationId> {
    let mut index = HashMap::new();
    for conversation in conversations
        .iter()
        .filter(|conversation| conversation.kind == ConversationKind::Channel)
    {
        let key = (
            conversation.network,
            mapping(conversation.network).fold(&conversation.name),
        );
        index.entry(key).or_insert(conversation.id);
    }
    index
}

fn sorted_members(mut members: Vec<String>) -> Vec<String> {
    // Servers can pad NAMES with extra spaces; an entry without a nickname is
    // not a member and must not become a row.
    members.retain(|member| {
        !member
            .trim_start_matches(['~', '&', '@', '%', '+'])
            .is_empty()
    });
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
/// Every retained line is built here, so its text never carries
/// bidirectional controls (`display::neutralize_bidi`).
/// The text a NOTICE is shown with (behind a `[NOTICE]` marker) and its
/// metadata.
fn notice_marked(text: &str, kind: Kind, meta: MessageMeta) -> (String, MessageMeta) {
    const MARKER: &str = "[NOTICE] ";
    if kind.is_notice() {
        (format!("{MARKER}{text}"), meta.after_prefix(MARKER.len()))
    } else {
        (text.into(), meta)
    }
}

fn new_message(sender: String, text: String, line: Line, meta: MessageMeta) -> Message {
    let text = display::neutralize_bidi_owned(text);
    Message {
        time: display_time(meta.server_time),
        sequence: 0,
        timestamp: meta.server_time.and_then(Timestamp::from_system_time),
        native_id: meta.native_id,
        account: meta.account,
        // The sender stays as sent: it keys self checks and avatars. It is
        // neutralized where it is drawn.
        sender,
        links: Links::new(links::find(&text)),
        text,
        highlights: Highlights::new(meta.highlights),
        activity: line != Line::Text,
        alert: line == Line::Alert,
        notice: false,
        provenance: meta.provenance,
        delivery_failed: false,
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
#[path = "lib_tests.rs"]
mod tests;
