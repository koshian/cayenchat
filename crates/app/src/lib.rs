//! Application state and commands, independent of any rendering framework.
use std::collections::{HashMap, HashSet};

use cayenchat_model::{Conversation, ConversationId, Message, Network, NetworkId};

/// Upper bound on conversations per network, so a hostile server or bouncer
/// cannot grow memory without limit by announcing endless channel joins.
const MAX_CONVERSATIONS_PER_NETWORK: usize = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Selection {
    Server(NetworkId),
    Channel(ConversationId),
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
    active_channels: HashSet<ConversationId>,
    active_servers: HashSet<NetworkId>,
    statuses: HashMap<NetworkId, ConnectionStatus>,
    server_messages: HashMap<NetworkId, Vec<Message>>,
    next_message_sequence: u64,
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
        let mut next_message_sequence = 0;
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
                name: name.into(),
                topic: topic.into(),
                messages: messages
                    .into_iter()
                    .map(|(time, sender, text)| {
                        next_message_sequence += 1;
                        Message {
                            time: time.into(),
                            sequence: next_message_sequence,
                            sender: sender.into(),
                            text: text.into(),
                            activity: false,
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
            active_channels,
            active_servers,
            statuses,
            server_messages: HashMap::new(),
            next_message_sequence,
        }
    }

    pub fn live(host: String, channels: Vec<String>) -> Self {
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
            active_channels: HashSet::new(),
            active_servers: HashSet::new(),
            server_messages: HashMap::new(),
            next_message_sequence: 0,
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
            } else if matches!(status, ConnectionStatus::Disconnected(_)) {
                self.active_servers.remove(&id);
                for channel in self
                    .conversations
                    .iter_mut()
                    .filter(|channel| channel.network == id)
                {
                    self.active_channels.remove(&channel.id);
                    channel.members.clear();
                }
            }
            self.statuses.insert(id, status);
        }
    }

    pub fn append_server_message(&mut self, id: NetworkId, text: String) {
        if !self.networks.iter().any(|network| network.id == id) {
            return;
        }
        self.next_message_sequence += 1;
        let messages = self.server_messages.entry(id).or_default();
        push_bounded(
            messages,
            Message {
                time: local_time(),
                sequence: self.next_message_sequence,
                sender: "server".into(),
                text,
                activity: false,
            },
        );
    }

    fn channel_id(&self, network: NetworkId, name: &str) -> Option<ConversationId> {
        self.conversations
            .iter()
            .find(|channel| channel.network == network && channel.name.eq_ignore_ascii_case(name))
            .map(|channel| channel.id)
    }

    fn ensure_channel(&mut self, network: NetworkId, name: &str) -> Option<ConversationId> {
        if let Some(id) = self.channel_id(network, name) {
            return Some(id);
        }
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
        let id = ConversationId(
            self.conversations
                .iter()
                .map(|channel| channel.id.0)
                .max()
                .unwrap_or(0)
                + 1,
        );
        self.conversations.push(Conversation {
            id,
            network,
            name: name.to_owned(),
            topic: String::new(),
            messages: Vec::new(),
            members: Vec::new(),
        });
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

    pub fn append_channel_message(
        &mut self,
        network: NetworkId,
        name: &str,
        sender: &str,
        text: &str,
        notice: bool,
    ) {
        // Only joined or configured channels get a conversation; anything else
        // a server sends lands in the bounded server log instead.
        let Some(id) = self.channel_id(network, name) else {
            let prefix = if notice { "[NOTICE] " } else { "" };
            self.append_server_message(network, format!("{name} <{sender}> {prefix}{text}"));
            return;
        };
        self.next_message_sequence += 1;
        if let Some(channel) = self
            .conversations
            .iter_mut()
            .find(|channel| channel.id == id)
        {
            push_bounded(
                &mut channel.messages,
                Message {
                    time: local_time(),
                    sequence: self.next_message_sequence,
                    sender: sender.into(),
                    text: if notice {
                        format!("[NOTICE] {text}")
                    } else {
                        text.into()
                    },
                    activity: false,
                },
            );
        }
        self.mark_unread(id);
    }

    pub fn append_channel_activity(&mut self, network: NetworkId, name: &str, text: String) {
        if let Some(id) = self.channel_id(network, name) {
            self.next_message_sequence += 1;
            if let Some(channel) = self
                .conversations
                .iter_mut()
                .find(|channel| channel.id == id)
            {
                push_bounded(
                    &mut channel.messages,
                    Message {
                        time: local_time(),
                        sequence: self.next_message_sequence,
                        sender: String::new(),
                        text,
                        activity: true,
                    },
                );
            }
        }
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

    pub fn selected_network(&self) -> &Network {
        let id = match self.selected {
            Selection::Server(id) => id,
            Selection::Channel(id) => {
                self.conversations
                    .iter()
                    .find(|channel| channel.id == id)
                    .expect("selected channel exists")
                    .network
            }
        };
        self.networks
            .iter()
            .find(|network| network.id == id)
            .expect("selected network exists")
    }

    pub fn is_unread(&self, id: ConversationId) -> bool {
        self.unread.contains(&id)
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
        let selected_network = self.selected_network().id;
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
    members.sort_by(|a, b| {
        let a_nick = a.trim_start_matches(['~', '&', '@', '%', '+']);
        let b_nick = b.trim_start_matches(['~', '&', '@', '%', '+']);
        let a_op = a.starts_with(['~', '&', '@', '%']);
        let b_op = b.starts_with(['~', '&', '@', '%']);
        b_op.cmp(&a_op)
            .then_with(|| a_nick.to_lowercase().cmp(&b_nick.to_lowercase()))
            .then_with(|| a_nick.cmp(b_nick))
    });
    members
}

fn local_time() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

fn push_bounded(messages: &mut Vec<Message>, message: Message) {
    messages.push(message);
    if messages.len() > 2_000 {
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
        let original_network = state.selected_network().id;
        let original_message = state.selected_channel().unwrap().messages[0].text.clone();
        state.dispatch(Command::SelectChannel(ConversationId(3)));
        assert_eq!(state.selected_channel().unwrap().name, "#general");
        assert_ne!(state.selected_network().id, original_network);
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
        state.append_channel_message(NetworkId(1), "#two", "alice", "hello", false);
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
        state.append_channel_message(NetworkId(1), "#two", "alice", "first", false);
        state.append_channel_message(NetworkId(1), "#one", "bob", "latest", false);

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
    fn channel_activity_is_ordered_without_marking_unread() {
        let mut state =
            AppState::live("irc.example.org".into(), vec!["#one".into(), "#two".into()]);
        state.append_channel_activity(NetworkId(1), "#two", "alice has joined (u@h)".into());
        assert!(!state.is_unread(ConversationId(2)));
        state.append_channel_message(NetworkId(1), "#two", "alice", "hello", false);
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
        state.append_channel_message(NetworkId(1), "#stray", "mallory", "hi", false);
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
}
