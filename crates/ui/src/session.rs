// SPDX-License-Identifier: GPL-3.0-only
//! Connection state kept for each configured server.
//!
//! Every server profile in the settings has one session, whether or not it is
//! connected. The chat window routes each worker's events to the session and
//! network they belong to, so several servers run side by side without any
//! shared connection state.

use std::{
    collections::{HashMap, HashSet},
    time::{Instant, SystemTime},
};

use cayenchat_app::{AppState, DiagnosticsView, Kind, Selection, own_avatar::OwnAvatar};
use cayenchat_irc_core::{Connection, ConnectionConfig};
use cayenchat_model::{ConversationId, NetworkId};
use cayenchat_storage::Secret;

/// What a typed `/join` waits for before it selects a channel. Only the
/// user's own `/join` creates one; auto-joins, rejoins and replayed history
/// never do.
pub struct JoinSwitch {
    /// Case-folded channels still waiting for our JOIN.
    pub channels: HashSet<String>,
    /// The selection this request left or set. Anything else means the user
    /// chose a view meanwhile, which a join must not override.
    pub expected: Selection,
}

pub struct ServerSession {
    /// Settings profile this session belongs to.
    pub profile_id: String,
    pub irc: Option<Connection>,
    /// Configuration of the last connection attempt, reused by reconnects.
    /// It holds no passwords (see `connect_auth`).
    pub active_config: Option<ConnectionConfig>,
    /// The last connection sent a server password, so a reconnect that finds
    /// none in the credential store asks for it. Only the fact is kept.
    pub server_password_used: bool,
    /// The unsaved server and SASL passwords the connection in progress
    /// sent, kept until it registers or ends. A refusal of one password
    /// sends the other again without asking for it (D068); every other end
    /// drops them.
    pub sent_secrets: (Option<Secret>, Option<Secret>),
    pub manual_disconnect: bool,
    /// A startup connection waits for the first frame (D068, Launch): the
    /// tree shows it as connecting before the credential store is read.
    pub starting: bool,
    /// The connection was ended just before the machine slept: no retry
    /// is scheduled, and the wake-up reconnects it unless the user took
    /// over meanwhile.
    pub asleep: bool,
    pub retry_pending: bool,
    pub retry_attempt: usize,
    /// Invalidates scheduled retries.
    pub retry_token: u64,
    pub own_nickname: Option<String>,
    /// Lowercase nicknames this client asked WHOIS for; replies requested by
    /// other clients sharing a bouncer only reach the server log.
    pub pending_whois: HashSet<String>,
    /// Case-folded channels this client asked to join and has no answer for
    /// yet, bounded, so a refusal of a new channel can be shown.
    pub pending_joins: HashSet<String>,
    /// A typed `/join` whose successful channels get selected (#611).
    pub join_switch: Option<JoinSwitch>,
    /// Case-folded channels this client sent PART for and has no answer to
    /// yet, with when; bounded and expiring (see `part_dialog`).
    pub pending_parts: HashMap<String, Instant>,
    /// Channels whose JOIN the server has not answered, for the
    /// channel key dialog and for saving a key; bounded.
    pub key_joins: Vec<crate::channel_keys::KeyJoin>,
    pub connection_started: Option<Instant>,
    /// When the connection was first lost since it last registered, so a
    /// reconnect can look for direct messages that arrived meanwhile.
    pub disconnected_at: Option<std::time::SystemTime>,
    pub watchdog_stage: u8,
    /// Invalidates the event pump and watchdog of a replaced connection.
    ///
    /// Every path that changes the destination (`active_config`'s host, port
    /// or TLS) must also increase it: asynchronous channel-key reads, writes
    /// and refreshes (`channel_keys`) compare only this number to tell
    /// whether they still belong to the current connection.
    pub generation: u64,
    /// Invalidates asynchronous channel-key reads after a key or auto-join
    /// configuration changes within this connection.
    pub channel_key_revision: u64,
    /// Identifies the newest in-flight write for each channel, so an
    /// unrelated channel's later write does not discard this one's result.
    pub channel_key_write_revisions: HashMap<String, u64>,
    /// Prevents a refresh from applying a snapshot while a key write is in
    /// flight.
    pub channel_key_writes_pending: usize,
    /// Our own avatar as this server confirmed it on the current
    /// connection; the draft URL lives in the settings profile.
    pub own_avatar: OwnAvatar,
    /// Whether the current connection asked for avatar metadata (the
    /// option and batch were on when it started), so a server that never
    /// enabled it can be reported as not supporting avatars.
    pub metadata_requested: bool,
    /// CTCP AVATAR on the current connection.
    pub peer_avatars: PeerAvatarConnection,
    /// Our messages waiting for the server's echo (`echo-message`), by the
    /// connection's id: the conversation, the line's sequence and
    /// its kind. The connection bounds what it tracks.
    pub pending_sends: HashMap<u64, (ConversationId, u64, Kind)>,
    /// Services account and real name of the users we share a channel with,
    /// as the connection reports them (`Event::UserAccount`), by
    /// casemapped nickname. The connection bounds it and reports removals.
    pub user_accounts: HashMap<String, (Option<String>, Option<String>)>,
}

/// How the current connection exchanges avatars with other clients: what
/// it was made with (peer avatars on, realname marked) and the URL it
/// answers queries with now, which can change while connected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerAvatarConnection {
    pub enabled: bool,
    pub advertised: bool,
    pub answering: Option<String>,
}

impl ServerSession {
    pub fn new(profile_id: String) -> Self {
        Self {
            profile_id,
            irc: None,
            active_config: None,
            server_password_used: false,
            sent_secrets: (None, None),
            manual_disconnect: false,
            starting: false,
            asleep: false,
            retry_pending: false,
            retry_attempt: 0,
            retry_token: 0,
            own_nickname: None,
            pending_whois: HashSet::new(),
            pending_joins: HashSet::new(),
            join_switch: None,
            pending_parts: HashMap::new(),
            key_joins: Vec::new(),
            connection_started: None,
            disconnected_at: None,
            watchdog_stage: 0,
            generation: 0,
            channel_key_revision: 0,
            channel_key_write_revisions: HashMap::new(),
            channel_key_writes_pending: 0,
            own_avatar: OwnAvatar::default(),
            metadata_requested: false,
            peer_avatars: PeerAvatarConnection::default(),
            pending_sends: HashMap::new(),
            user_accounts: HashMap::new(),
        }
    }

    /// Records what a connection starting with `config` asked for.
    pub fn connection_starting(&mut self, config: &ConnectionConfig) {
        self.pending_sends.clear();
        self.user_accounts.clear();
        self.metadata_requested = config.ircv3.metadata;
        self.peer_avatars = PeerAvatarConnection {
            enabled: config.ircv3.peer_avatars,
            advertised: config.marks_avatar(),
            answering: config
                .shared_avatar
                .clone()
                .filter(|_| config.ircv3.peer_avatars),
        };
    }

    /// A note of the UI itself on `network`, stamped now with the time since
    /// the connection attempt began.
    pub fn push_diagnostic(&self, state: &mut AppState, network: NetworkId, text: String) {
        let elapsed = self.connection_started.map(|started| started.elapsed());
        state.record_diagnostic(
            network,
            DiagnosticsView::Overview,
            SystemTime::now(),
            elapsed,
            text,
        );
    }

    /// Whether this server was connected or tried to connect in this run;
    /// untouched servers show no connection mark in the tree.
    pub fn used(&self) -> bool {
        self.irc.is_some() || self.active_config.is_some()
    }

    /// Stops the worker (it flushes QUIT) and cancels pending retries. The
    /// connection is returned so a caller that is about to exit can wait for
    /// the flush.
    pub fn close(&mut self) -> Option<Connection> {
        self.manual_disconnect = true;
        self.sent_secrets = (None, None);
        self.asleep = false;
        self.retry_pending = false;
        self.retry_token += 1;
        self.generation += 1;
        self.own_avatar.connection_ended();
        let connection = self.irc.take()?;
        let _ = connection.disconnect();
        Some(connection)
    }
}
