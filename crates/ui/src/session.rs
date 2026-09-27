//! Connection state kept for each configured server.
//!
//! Every server profile in the settings has one session, whether or not it is
//! connected. The chat window routes each worker's events to the session and
//! network they belong to, so several servers run side by side without any
//! shared connection state.

use std::{
    collections::{HashSet, VecDeque},
    time::Instant,
};

use cayenchat_irc_core::{Connection, ConnectionConfig};

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
    pub watchdog_stage: u8,
    /// Invalidates the event pump and watchdog of a replaced connection.
    pub generation: u64,
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
            watchdog_stage: 0,
            generation: 0,
        }
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
        if let Some(connection) = self.irc.take() {
            let _ = connection.disconnect();
        }
    }
}
