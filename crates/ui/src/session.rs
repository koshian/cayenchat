//! Connection state kept for each configured server.
//!
//! Every server profile in the settings has one session, whether or not it is
//! connected. The chat window routes each worker's events to the session and
//! network they belong to, so several servers run side by side without any
//! shared connection state.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Instant,
};

use cayenchat_app::own_avatar::OwnAvatar;
use cayenchat_irc_core::{Connection, ConnectionConfig};
use cayenchat_model::ConversationId;

/// Newest transcript lines kept per server.
pub const DIAGNOSTIC_LIMIT: usize = 1000;

pub struct ServerSession {
    /// Settings profile this session belongs to.
    pub profile_id: String,
    pub irc: Option<Connection>,
    /// Configuration of the last connection attempt, reused by reconnects.
    pub active_config: Option<ConnectionConfig>,
    pub manual_disconnect: bool,
    pub retry_pending: bool,
    pub retry_attempt: usize,
    /// Invalidates scheduled retries.
    pub retry_token: u64,
    pub own_nickname: Option<String>,
    /// Lowercase nicknames this client asked WHOIS for; replies requested by
    /// other clients sharing a bouncer only reach the server log.
    pub pending_whois: HashSet<String>,
    /// Parsed IRC transcript and connection stages, bounded.
    pub diagnostics: VecDeque<String>,
    pub connection_started: Option<Instant>,
    /// When the connection was first lost since it last registered, so a
    /// reconnect can look for direct messages that arrived meanwhile.
    pub disconnected_at: Option<std::time::SystemTime>,
    pub watchdog_stage: u8,
    /// Invalidates the event pump and watchdog of a replaced connection.
    pub generation: u64,
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
    /// connection's id: the conversation, the line's sequence and whether
    /// it is a NOTICE. The connection bounds what it tracks.
    pub pending_sends: HashMap<u64, (ConversationId, u64, bool)>,
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
            manual_disconnect: false,
            retry_pending: false,
            retry_attempt: 0,
            retry_token: 0,
            own_nickname: None,
            pending_whois: HashSet::new(),
            diagnostics: VecDeque::new(),
            connection_started: None,
            disconnected_at: None,
            watchdog_stage: 0,
            generation: 0,
            own_avatar: OwnAvatar::default(),
            metadata_requested: false,
            peer_avatars: PeerAvatarConnection::default(),
            pending_sends: HashMap::new(),
        }
    }

    /// Records what a connection starting with `config` asked for.
    pub fn connection_starting(&mut self, config: &ConnectionConfig) {
        self.pending_sends.clear();
        self.metadata_requested = config.ircv3.metadata;
        self.peer_avatars = PeerAvatarConnection {
            enabled: config.ircv3.peer_avatars,
            advertised: config.advertises_avatar(),
            answering: config
                .shared_avatar
                .clone()
                .filter(|_| config.ircv3.peer_avatars),
        };
    }

    pub fn push_diagnostic(&mut self, line: String) {
        // Every IRC line is recorded, so drop the oldest without shifting.
        self.diagnostics.push_back(line);
        if self.diagnostics.len() > DIAGNOSTIC_LIMIT {
            self.diagnostics.pop_front();
        }
    }

    /// Whether this server was connected or tried to connect in this run;
    /// untouched servers show no connection mark in the tree.
    pub fn used(&self) -> bool {
        self.irc.is_some() || self.active_config.is_some()
    }

    /// Stops the worker (it flushes QUIT) and cancels pending retries.
    pub fn close(&mut self) {
        self.manual_disconnect = true;
        self.retry_pending = false;
        self.retry_token += 1;
        self.generation += 1;
        self.own_avatar.connection_ended();
        if let Some(connection) = self.irc.take() {
            let _ = connection.disconnect();
        }
    }
}
