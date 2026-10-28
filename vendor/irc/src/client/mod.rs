//! A simple, thread-safe, and async-friendly IRC client library.
//!
//! This API provides the ability to connect to an IRC server via the
//! [`Client`] type. The [`Client`] trait that
//! [`Client`] implements provides methods for communicating with the
//! server.
//!
//! # Examples
//!
//! Using these APIs, we can connect to a server and send a one-off message (in this case,
//! identifying with the server).
//!
//! ```no_run
//! # extern crate irc;
//! use irc::client::prelude::Client;
//!
//! # #[tokio::main]
//! # async fn main() -> irc::error::Result<()> {
//! let client = Client::new("config.toml").await?;
//! client.identify()?;
//! # Ok(())
//! # }
//! ```
//!
//! We can then use functions from [`Client`] to receive messages from the
//! server in a blocking fashion and perform any desired actions in response. The following code
//! performs a simple call-and-response when the bot's name is mentioned in a channel.
//!
//! ```no_run
//! use irc::client::prelude::*;
//! use futures::*;
//!
//! # #[tokio::main]
//! # async fn main() -> irc::error::Result<()> {
//! let mut client = Client::new("config.toml").await?;
//! let mut stream = client.stream()?;
//! client.identify()?;
//!
//! while let Some(message) = stream.next().await.transpose()? {
//!     if let Command::PRIVMSG(channel, message) = message.command {
//!         if message.contains(client.current_nickname()) {
//!             client.send_privmsg(&channel, "beep boop").unwrap();
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "ctcp")]
use chrono::prelude::*;
use futures_util::{
    future::{FusedFuture, Future},
    ready,
    stream::{FusedStream, Stream},
};
use futures_util::{
    sink::Sink as _,
    stream::{SplitSink, SplitStream, StreamExt as _},
};
use parking_lot::{Mutex, RwLock};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::Path,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use crate::{
    client::{
        conn::Connection,
        data::{Config, User},
        queue::{OutgoingReceiver, OutgoingSender},
    },
    error,
    proto::{
        mode::ModeType,
        CapSubCommand::{END, LS, REQ},
        Capability, ChannelMode, Command,
        Command::{
            ChannelMODE, AUTHENTICATE, CAP, INVITE, JOIN, KICK, KILL, NICK, NICKSERV, NOTICE, OPER,
            PART, PASS, PONG, PRIVMSG, QUIT, SAMODE, SANICK, TOPIC, USER,
        },
        Message, Mode, NegotiationVersion, Response,
    },
};

pub mod conn;
pub mod data;
mod mock;
pub mod prelude;
mod queue;
pub mod transport;

macro_rules! pub_state_base {
    () => {
        /// Changes the modes for the specified target.
        pub fn send_mode<S, T>(&self, target: S, modes: &[Mode<T>]) -> error::Result<()>
        where
            S: fmt::Display,
            T: ModeType,
        {
            self.send(T::mode(&target.to_string(), modes))
        }

        /// Joins the specified channel or chanlist.
        pub fn send_join<S>(&self, chanlist: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            self.send(JOIN(chanlist.to_string(), None, None))
        }

        /// Joins the specified channel or chanlist using the specified key or keylist.
        pub fn send_join_with_keys<S1, S2>(&self, chanlist: S1, keylist: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            self.send(JOIN(chanlist.to_string(), Some(keylist.to_string()), None))
        }

        /// Sends a notice to the specified target.
        pub fn send_notice<S1, S2>(&self, target: S1, message: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            let message = message.to_string();
            for line in message.split("\r\n") {
                self.send(NOTICE(target.to_string(), line.to_string()))?
            }
            Ok(())
        }
    };
}

macro_rules! pub_sender_base {
    () => {
        /// Sends a request for a list of server capabilities for a specific IRCv3 version.
        pub fn send_cap_ls(&self, version: NegotiationVersion) -> error::Result<()> {
            self.send(Command::CAP(
                None,
                LS,
                match version {
                    NegotiationVersion::V301 => None,
                    NegotiationVersion::V302 => Some("302".to_owned()),
                },
                None,
            ))
        }

        /// Sends an IRCv3 capabilities request for the specified extensions.
        pub fn send_cap_req(&self, extensions: &[Capability]) -> error::Result<()> {
            let append = |mut s: String, c| {
                s.push_str(c);
                s.push(' ');
                s
            };
            let mut exts = extensions
                .iter()
                .map(|c| c.as_ref())
                .fold(String::new(), append);
            let len = exts.len() - 1;
            exts.truncate(len);
            self.send(CAP(None, REQ, None, Some(exts)))
        }

        /// Sends a SASL AUTHENTICATE message with the specified data.
        pub fn send_sasl<S: fmt::Display>(&self, data: S) -> error::Result<()> {
            self.send(AUTHENTICATE(data.to_string()))
        }

        /// Sends a SASL AUTHENTICATE request to use the PLAIN mechanism.
        pub fn send_sasl_plain(&self) -> error::Result<()> {
            self.send_sasl("PLAIN")
        }

        /// Sends a SASL AUTHENTICATE request to use the EXTERNAL mechanism.
        pub fn send_sasl_external(&self) -> error::Result<()> {
            self.send_sasl("EXTERNAL")
        }

        /// Sends a SASL AUTHENTICATE request to abort authentication.
        pub fn send_sasl_abort(&self) -> error::Result<()> {
            self.send_sasl("*")
        }

        /// Sends a PONG with the specified message.
        pub fn send_pong<S>(&self, msg: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            self.send(PONG(msg.to_string(), None))
        }

        /// Parts the specified channel or chanlist.
        pub fn send_part<S>(&self, chanlist: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            self.send(PART(chanlist.to_string(), None))
        }

        /// Attempts to oper up using the specified username and password.
        pub fn send_oper<S1, S2>(&self, username: S1, password: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            self.send(OPER(username.to_string(), password.to_string()))
        }

        /// Sends a message to the specified target. If the message contains IRC newlines (`\r\n`), it
        /// will automatically be split and sent as multiple separate `PRIVMSG`s to the specified
        /// target. If you absolutely must avoid this behavior, you can do
        /// `client.send(PRIVMSG(target, message))` directly.
        pub fn send_privmsg<S1, S2>(&self, target: S1, message: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            let message = message.to_string();
            for line in message.split("\r\n") {
                self.send(PRIVMSG(target.to_string(), line.to_string()))?
            }
            Ok(())
        }

        /// Sets the topic of a channel or requests the current one.
        /// If `topic` is an empty string, it won't be included in the message.
        pub fn send_topic<S1, S2>(&self, channel: S1, topic: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            let topic = topic.to_string();
            self.send(TOPIC(
                channel.to_string(),
                if topic.is_empty() { None } else { Some(topic) },
            ))
        }

        /// Kills the target with the provided message.
        pub fn send_kill<S1, S2>(&self, target: S1, message: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            self.send(KILL(target.to_string(), message.to_string()))
        }

        /// Kicks the listed nicknames from the listed channels with a comment.
        /// If `message` is an empty string, it won't be included in the message.
        pub fn send_kick<S1, S2, S3>(
            &self,
            chanlist: S1,
            nicklist: S2,
            message: S3,
        ) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
            S3: fmt::Display,
        {
            let message = message.to_string();
            self.send(KICK(
                chanlist.to_string(),
                nicklist.to_string(),
                if message.is_empty() {
                    None
                } else {
                    Some(message)
                },
            ))
        }

        /// Changes the mode of the target by force.
        /// If `modeparams` is an empty string, it won't be included in the message.
        pub fn send_samode<S1, S2, S3>(
            &self,
            target: S1,
            mode: S2,
            modeparams: S3,
        ) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
            S3: fmt::Display,
        {
            let modeparams = modeparams.to_string();
            self.send(SAMODE(
                target.to_string(),
                mode.to_string(),
                if modeparams.is_empty() {
                    None
                } else {
                    Some(modeparams)
                },
            ))
        }

        /// Forces a user to change from the old nickname to the new nickname.
        pub fn send_sanick<S1, S2>(&self, old_nick: S1, new_nick: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            self.send(SANICK(old_nick.to_string(), new_nick.to_string()))
        }

        /// Invites a user to the specified channel.
        pub fn send_invite<S1, S2>(&self, nick: S1, chan: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            self.send(INVITE(nick.to_string(), chan.to_string()))
        }

        /// Quits the server entirely with a message.
        /// This defaults to `Powered by Rust.` if none is specified.
        pub fn send_quit<S>(&self, msg: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            let msg = msg.to_string();
            self.send(QUIT(Some(if msg.is_empty() {
                "Powered by Rust.".to_string()
            } else {
                msg
            })))
        }

        /// Sends a CTCP-escaped message to the specified target.
        /// This requires the CTCP feature to be enabled.
        #[cfg(feature = "ctcp")]
        pub fn send_ctcp<S1, S2>(&self, target: S1, msg: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            let msg = msg.to_string();
            for line in msg.split("\r\n") {
                self.send(PRIVMSG(
                    target.to_string(),
                    format!("\u{001}{}\u{001}", line),
                ))?
            }
            Ok(())
        }

        /// Sends an action command to the specified target.
        /// This requires the CTCP feature to be enabled.
        #[cfg(feature = "ctcp")]
        pub fn send_action<S1, S2>(&self, target: S1, msg: S2) -> error::Result<()>
        where
            S1: fmt::Display,
            S2: fmt::Display,
        {
            self.send_ctcp(target, &format!("ACTION {}", msg.to_string())[..])
        }

        /// Sends a finger request to the specified target.
        /// This requires the CTCP feature to be enabled.
        #[cfg(feature = "ctcp")]
        pub fn send_finger<S: fmt::Display>(&self, target: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            self.send_ctcp(target, "FINGER")
        }

        /// Sends a version request to the specified target.
        /// This requires the CTCP feature to be enabled.
        #[cfg(feature = "ctcp")]
        pub fn send_version<S>(&self, target: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            self.send_ctcp(target, "VERSION")
        }

        /// Sends a source request to the specified target.
        /// This requires the CTCP feature to be enabled.
        #[cfg(feature = "ctcp")]
        pub fn send_source<S>(&self, target: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            self.send_ctcp(target, "SOURCE")
        }

        /// Sends a user info request to the specified target.
        /// This requires the CTCP feature to be enabled.
        #[cfg(feature = "ctcp")]
        pub fn send_user_info<S>(&self, target: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            self.send_ctcp(target, "USERINFO")
        }

        /// Sends a finger request to the specified target.
        /// This requires the CTCP feature to be enabled.
        #[cfg(feature = "ctcp")]
        pub fn send_ctcp_ping<S>(&self, target: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            let time = Local::now();
            self.send_ctcp(target, &format!("PING {}", time.timestamp())[..])
        }

        /// Sends a time request to the specified target.
        /// This requires the CTCP feature to be enabled.
        #[cfg(feature = "ctcp")]
        pub fn send_time<S>(&self, target: S) -> error::Result<()>
        where
            S: fmt::Display,
        {
            self.send_ctcp(target, "TIME")
        }
    };
}

/// A stream of `Messages` received from an IRC server via an `Client`.
///
/// Interaction with this stream relies on the `futures` API, but is only expected for less
/// traditional use cases. To learn more, you can view the documentation for the
/// [`futures`](https://docs.rs/futures/) crate, or the tutorials for
/// [`tokio`](https://tokio.rs/docs/getting-started/futures/).
#[derive(Debug)]
pub struct ClientStream {
    state: Arc<ClientState>,
    stream: SplitStream<Connection>,
    // In case the client stream also handles outgoing messages.
    outgoing: Option<Outgoing>,
}

impl ClientStream {
    /// collect stream and collect all messages available.
    pub async fn collect(mut self) -> error::Result<Vec<Message>> {
        let mut output = Vec::new();

        while let Some(message) = self.next().await {
            match message {
                Ok(message) => output.push(message),
                Err(e) => return Err(e),
            }
        }

        Ok(output)
    }
}

impl FusedStream for ClientStream {
    fn is_terminated(&self) -> bool {
        false
    }
}

impl Stream for ClientStream {
    type Item = Result<Message, error::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(outgoing) = self.as_mut().outgoing.as_mut() {
            match Pin::new(outgoing).poll(cx) {
                Poll::Ready(Ok(())) => {
                    // assure that we wake up again to check the incoming stream.
                    cx.waker().wake_by_ref();
                    return Poll::Ready(None);
                }
                Poll::Ready(Err(e)) => {
                    cx.waker().wake_by_ref();
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Pending => (),
            }
        }

        match ready!(Pin::new(&mut self.as_mut().stream).poll_next(cx)) {
            Some(Ok(msg)) => {
                self.state.handle_message(&msg)?;
                Poll::Ready(Some(Ok(msg)))
            }
            other => Poll::Ready(other),
        }
    }
}

/// Most channels with a member list per connection (local change, see vendor/README.md).
const MAX_LISTED_CHANNELS: usize = 2_000;
/// Most members listed in one channel (local change, see vendor/README.md).
const MAX_MEMBERS_PER_CHANNEL: usize = 50_000;
/// Most members listed over all channels of a connection (local change, see vendor/README.md).
const MAX_MEMBERS_TOTAL: usize = 100_000;

/// Most bytes retained for the members of one channel (local change, see vendor/README.md).
const MAX_BYTES_PER_CHANNEL: usize = 16 * 1024 * 1024;
/// Most bytes retained for the members of all channels of a connection (local change,
/// see vendor/README.md).
const MAX_BYTES_TOTAL: usize = 32 * 1024 * 1024;

/// Bytes charged per member besides its strings: the `User` (its access levels are a set of
/// at most six levels), the slack of a growing list and an index slot.
const ENTRY_OVERHEAD: usize = 128;

/// Members and retained bytes of one or more channel lists.
#[derive(Clone, Copy, Debug, Default)]
struct Usage {
    members: usize,
    bytes: usize,
}

impl Usage {
    /// Bytes charged for a user: a fixed share for the entry and index slot, the nickname
    /// twice (list and `names_index` key) and the optional user and host parts.
    fn of_user(user: &User) -> Usage {
        let bytes = ENTRY_OVERHEAD
            + 2 * user.get_nickname().len()
            + user.get_username().map_or(0, str::len)
            + user.get_hostname().map_or(0, str::len);
        Usage { members: 1, bytes }
    }

    fn plus(self, o: Usage) -> Usage {
        Usage {
            members: self.members + o.members,
            bytes: self.bytes + o.bytes,
        }
    }

    fn minus(self, o: Usage) -> Usage {
        Usage {
            members: self.members.saturating_sub(o.members),
            bytes: self.bytes.saturating_sub(o.bytes),
        }
    }
}

/// Fails when adding `more` to a channel holding `in_channel`, of `in_total` over all
/// channels, would pass a member or byte limit.
fn check_budget(in_channel: Usage, in_total: Usage, more: Usage) -> error::Result<()> {
    let (c, t) = (in_channel.plus(more), in_total.plus(more));
    if c.members > MAX_MEMBERS_PER_CHANNEL {
        Err(error::Error::ChannelListLimit(
            "too many members in a channel",
        ))
    } else if t.members > MAX_MEMBERS_TOTAL {
        Err(error::Error::ChannelListLimit("too many members in total"))
    } else if c.bytes > MAX_BYTES_PER_CHANNEL {
        Err(error::Error::ChannelListLimit(
            "member list of a channel too large",
        ))
    } else if t.bytes > MAX_BYTES_TOTAL {
        Err(error::Error::ChannelListLimit(
            "member lists too large in total",
        ))
    } else {
        Ok(())
    }
}

/// What the channel lists retain, kept up to date by each change to a list so that a line
/// costs constant time however many members there are (local change, see vendor/README.md).
/// Lock it only while holding the `chanlists` lock.
#[derive(Debug, Default)]
struct Ledger {
    channels: HashMap<String, Usage>,
    total: Usage,
}

impl Ledger {
    /// Replaces `freed` of `chan` by `cost`, or fails and changes nothing when that passes
    /// a limit.
    fn charge(&mut self, chan: &str, freed: Usage, cost: Usage) -> error::Result<()> {
        let in_chan = self
            .channels
            .get(chan)
            .copied()
            .unwrap_or_default()
            .minus(freed);
        let total = self.total.minus(freed);
        check_budget(in_chan, total, cost)?;
        self.total = total.plus(cost);
        let new = in_chan.plus(cost);
        match self.channels.get_mut(chan) {
            Some(u) => *u = new,
            None => {
                self.channels.insert(chan.to_owned(), new);
            }
        }
        Ok(())
    }

    /// Gives back `freed` of `chan`.
    fn release(&mut self, chan: &str, freed: Usage) {
        if let Some(u) = self.channels.get_mut(chan) {
            *u = u.minus(freed);
        }
        self.total = self.total.minus(freed);
    }

    /// Gives back everything `chan` holds.
    fn clear(&mut self, chan: &str) {
        if let Some(u) = self.channels.remove(chan) {
            self.total = self.total.minus(u);
        }
    }
}

/// Returns spare capacity of a list that has shrunk well below it; a list keeps the room of
/// its largest size otherwise (local change, see vendor/README.md).
fn release_spare(list: &mut Vec<User>) {
    if list.capacity() > 64 && list.len() < list.capacity() / 4 {
        list.shrink_to_fit();
    }
}

/// Thread-safe internal state for an IRC server connection.
#[derive(Debug)]
struct ClientState {
    sender: Sender,
    /// The configuration used with this connection.
    config: Config,
    /// A thread-safe map of channels to the list of users in them.
    chanlists: RwLock<HashMap<String, Vec<User>>>,
    /// Channels whose NAMES reply ended (RPL_ENDOFNAMES). The next RPL_NAMREPLY
    /// for one of them starts a new reply and rebuilds the list (local change,
    /// see vendor/README.md).
    names_done: RwLock<HashSet<String>>,
    /// Nick to position in the list, for the channel whose NAMES reply is being taken in.
    /// Any message that is not a RPL_NAMREPLY drops it, since the positions may have moved
    /// (local change, see vendor/README.md).
    names_index: RwLock<HashMap<String, HashMap<String, usize>>>,
    /// Members and bytes the lists retain, per channel and in total (local change, see
    /// vendor/README.md).
    ledger: Mutex<Ledger>,
    /// Set by the first end of MOTD; only that one joins the configured channels.
    motd_handled: AtomicBool,
    /// A thread-safe index to track the current alternative nickname being used.
    alt_nick_index: RwLock<usize>,
    /// Our nickname as the server confirmed it (RPL_WELCOME, then our own NICK).
    /// Unlike `current_nickname` this follows nick changes after registration.
    confirmed_nick: RwLock<Option<String>>,
    /// Default ghost sequence to send if one is required but none is configured.
    default_ghost_sequence: Vec<String>,
}

impl ClientState {
    fn new(sender: Sender, config: Config) -> ClientState {
        ClientState {
            sender,
            config,
            chanlists: RwLock::new(HashMap::new()),
            names_done: RwLock::new(HashSet::new()),
            names_index: RwLock::new(HashMap::new()),
            ledger: Mutex::new(Ledger::default()),
            motd_handled: AtomicBool::new(false),
            alt_nick_index: RwLock::new(0),
            confirmed_nick: RwLock::new(None),
            default_ghost_sequence: vec![String::from("GHOST")],
        }
    }

    fn config(&self) -> &Config {
        &self.config
    }

    fn send<M: Into<Message>>(&self, msg: M) -> error::Result<()> {
        let msg = msg.into();
        // Local change (see vendor/README.md): the queue can refuse a message when it is
        // full, so the state change for a sent message happens only once it is queued.
        // A refused PART must not drop the list of a channel we are still in.
        let sent = msg.clone();
        self.sender.send(msg)?;
        self.handle_sent_message(&sent)
    }

    /// Drops the NAMES position index of `chan` (local change, see vendor/README.md). Call
    /// it whenever the positions in a list may move.
    fn forget_names_index(&self, chan: &str) {
        self.names_index.write().remove(chan);
    }

    /// Drops the list of `chan` and the end-of-NAMES mark that goes with it (local change,
    /// see vendor/README.md). Every path that forgets a channel must use this.
    fn drop_channel_list(&self, chan: &str) {
        let mut lists = self.chanlists.write();
        let _ = lists.remove(chan);
        self.ledger.lock().clear(chan);
        drop(lists);
        self.names_done.write().remove(chan);
        self.forget_names_index(chan);
    }

    /// Gets the current nickname in use.
    fn current_nickname(&self) -> &str {
        let alt_nicks = self.config().alternate_nicknames();
        let index = self.alt_nick_index.read();

        match *index {
            0 => self
                .config()
                .nickname()
                .expect("current_nickname should not be callable if nickname is not defined."),
            i => alt_nicks[i - 1].as_str(),
        }
    }

    /// Handles sent messages internally for basic client functionality.
    fn handle_sent_message(&self, msg: &Message) -> error::Result<()> {
        log::trace!("[SENT] {}", msg.to_string());

        if let PART(ref chan, _) = msg.command {
            self.drop_channel_list(chan);
        }

        Ok(())
    }

    /// Handles received messages internally for basic client functionality.
    fn handle_message(&self, msg: &Message) -> error::Result<()> {
        log::trace!("[RECV] {}", msg.to_string());
        match msg.command {
            JOIN(ref chan, _, _) => self.handle_join(msg.source_nickname().unwrap_or(""), chan)?,
            PART(ref chan, _) => self.handle_part(msg.source_nickname().unwrap_or(""), chan),
            KICK(ref chan, ref user, _) => {
                // Local change (see vendor/README.md): being kicked drops the
                // channel's list like a PART we sent does. Otherwise the stale
                // entries stay and the next JOIN and NAMES are appended to them.
                #[cfg(feature = "channel-lists")]
                if self
                    .confirmed_nick
                    .read()
                    .as_deref()
                    .is_some_and(|me| user.eq_ignore_ascii_case(me))
                {
                    self.drop_channel_list(chan);
                    return Ok(());
                }
                self.handle_part(user, chan)
            }
            QUIT(_) => self.handle_quit(msg.source_nickname().unwrap_or("")),
            NICK(ref new_nick) => {
                let old_nick = msg.source_nickname().unwrap_or("");
                {
                    let mut me = self.confirmed_nick.write();
                    if me
                        .as_deref()
                        .is_some_and(|me| old_nick.eq_ignore_ascii_case(me))
                    {
                        *me = Some(new_nick.clone());
                    }
                }
                self.handle_nick_change(old_nick, new_nick)?
            }
            Command::Response(Response::RPL_WELCOME, ref args) => {
                if let Some(nick) = args.first() {
                    *self.confirmed_nick.write() = Some(nick.clone());
                }
            }
            ChannelMODE(ref chan, ref modes) => self.handle_mode(chan, modes),
            PRIVMSG(ref target, ref body) => {
                if body.starts_with('\u{001}') {
                    let tokens: Vec<_> = {
                        let end = if body.ends_with('\u{001}') && body.len() > 1 {
                            body.len() - 1
                        } else {
                            body.len()
                        };
                        body[1..end].split(' ').collect()
                    };
                    if target.starts_with('#') {
                        self.handle_ctcp(target, &tokens)?
                    } else if let Some(user) = msg.source_nickname() {
                        self.handle_ctcp(user, &tokens)?
                    }
                }
            }
            Command::Response(Response::RPL_NAMREPLY, ref args) => self.handle_namreply(args)?,
            Command::Response(Response::RPL_ENDOFMOTD, _)
            | Command::Response(Response::ERR_NOMOTD, _) => {
                // Local change (see vendor/README.md): only the first end of
                // MOTD of a connection auto-joins. A later one (`/quote MOTD`,
                // a server resend) must not join channels again.
                if self.motd_handled.swap(true, Ordering::SeqCst) {
                    return Ok(());
                }
                self.send_nick_password()?;
                self.send_umodes()?;

                let config_chans = self.config().channels();
                for chan in config_chans {
                    match self.config().channel_key(chan) {
                        Some(key) => self.send_join_with_keys::<&str, &str>(chan, key)?,
                        None => self.send_join(chan)?,
                    }
                }
            }
            Command::Response(Response::RPL_ENDOFNAMES, ref args) => self.handle_endofnames(args),
            Command::Response(Response::ERR_NICKNAMEINUSE, _)
            | Command::Response(Response::ERR_ERRONEOUSNICKNAME, _) => {
                let alt_nicks = self.config().alternate_nicknames();
                let mut index = self.alt_nick_index.write();

                if *index >= alt_nicks.len() {
                    return Err(error::Error::NoUsableNick);
                } else {
                    self.send(NICK(alt_nicks[*index].to_owned()))?;
                    *index += 1;
                }
            }
            _ => (),
        }
        Ok(())
    }

    fn send_nick_password(&self) -> error::Result<()> {
        if self.config().nick_password().is_empty() {
            Ok(())
        } else {
            let mut index = self.alt_nick_index.write();

            if self.config().should_ghost() && *index != 0 {
                let seq = match self.config().ghost_sequence() {
                    Some(seq) => seq,
                    None => &*self.default_ghost_sequence,
                };

                for s in seq {
                    self.send(NICKSERV(vec![
                        s.to_string(),
                        self.config().nickname()?.to_string(),
                        self.config().nick_password().to_string(),
                    ]))?;
                }
                *index = 0;
                self.send(NICK(self.config().nickname()?.to_owned()))?
            }

            self.send(NICKSERV(vec![
                "IDENTIFY".to_string(),
                self.config().nick_password().to_string(),
            ]))
        }
    }

    fn send_umodes(&self) -> error::Result<()> {
        if self.config().umodes().is_empty() {
            Ok(())
        } else {
            self.send_mode(
                self.current_nickname(),
                &Mode::as_user_modes(
                    self.config()
                        .umodes()
                        .split(' ')
                        .collect::<Vec<_>>()
                        .as_ref(),
                )
                .map_err(|e| error::Error::InvalidMessage {
                    string: format!(
                        "MODE {} {}",
                        self.current_nickname(),
                        self.config().umodes()
                    ),
                    cause: e,
                })?,
            )
        }
    }

    #[cfg(not(feature = "channel-lists"))]
    fn handle_join(&self, _: &str, _: &str) -> error::Result<()> {
        Ok(())
    }

    #[cfg(feature = "channel-lists")]
    fn handle_join(&self, src: &str, chan: &str) -> error::Result<()> {
        // Local change (see vendor/README.md): our own JOIN is the only thing
        // that creates a list, and it starts a new empty one.
        if self
            .confirmed_nick
            .read()
            .as_deref()
            .is_some_and(|me| src.eq_ignore_ascii_case(me))
        {
            self.names_done.write().remove(chan);
            self.forget_names_index(chan);
            let mut lists = self.chanlists.write();
            if lists.len() >= MAX_LISTED_CHANNELS && !lists.contains_key(chan) {
                return Err(error::Error::ChannelListLimit("too many channels"));
            }
            lists.insert(chan.to_owned(), Vec::new());
            self.ledger.lock().clear(chan);
            return Ok(());
        }
        let mut lists = self.chanlists.write();
        if let Some(vec) = lists.get_mut(chan).filter(|_| !src.is_empty()) {
            let user = User::new(src);
            self.ledger
                .lock()
                .charge(chan, Usage::default(), Usage::of_user(&user))?;
            vec.push(user);
            self.forget_names_index(chan);
        }
        Ok(())
    }

    #[cfg(not(feature = "channel-lists"))]
    fn handle_part(&self, _: &str, _: &str) {}

    #[cfg(feature = "channel-lists")]
    fn handle_part(&self, src: &str, chan: &str) {
        if let Some(vec) = self.chanlists.write().get_mut(&chan.to_owned()) {
            if !src.is_empty() {
                if let Some(n) = vec.iter().position(|x| x.get_nickname() == src) {
                    let gone = vec.swap_remove(n);
                    self.ledger.lock().release(chan, Usage::of_user(&gone));
                    release_spare(vec);
                    self.forget_names_index(chan);
                }
            }
        }
    }

    #[cfg(not(feature = "channel-lists"))]
    fn handle_quit(&self, _: &str) {}

    #[cfg(feature = "channel-lists")]
    fn handle_quit(&self, src: &str) {
        if src.is_empty() {
            return;
        }

        for (chan, vec) in self.chanlists.write().iter_mut() {
            if let Some(p) = vec.iter().position(|x| x.get_nickname() == src) {
                let gone = vec.swap_remove(p);
                self.ledger.lock().release(chan, Usage::of_user(&gone));
                release_spare(vec);
                self.forget_names_index(chan);
            }
        }
    }

    #[cfg(not(feature = "channel-lists"))]
    fn handle_nick_change(&self, _: &str, _: &str) -> error::Result<()> {
        Ok(())
    }

    #[cfg(feature = "channel-lists")]
    fn handle_nick_change(&self, old_nick: &str, new_nick: &str) -> error::Result<()> {
        if old_nick.is_empty() || new_nick.is_empty() {
            return Ok(());
        }

        let mut lists = self.chanlists.write();
        for (chan, vec) in lists.iter_mut() {
            if let Some(n) = vec.iter().position(|x| x.get_nickname() == old_nick) {
                let new_entry = User::new(new_nick);
                // A longer nickname must not grow a list past its budget.
                let freed = Usage::of_user(&vec[n]);
                self.ledger
                    .lock()
                    .charge(chan, freed, Usage::of_user(&new_entry))?;
                vec[n] = new_entry;
                self.forget_names_index(chan);
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "channel-lists"))]
    fn handle_mode(&self, _: &str, _: &[Mode<ChannelMode>]) {}

    #[cfg(feature = "channel-lists")]
    fn handle_mode(&self, chan: &str, modes: &[Mode<ChannelMode>]) {
        for mode in modes {
            match *mode {
                Mode::Plus(_, Some(ref user)) | Mode::Minus(_, Some(ref user)) => {
                    if let Some(vec) = self.chanlists.write().get_mut(chan) {
                        if let Some(n) = vec.iter().position(|x| x.get_nickname() == user) {
                            vec[n].update_access_level(mode)
                        }
                    }
                }
                _ => (),
            }
        }
    }

    #[cfg(not(feature = "channel-lists"))]
    fn handle_namreply(&self, _: &[String]) -> error::Result<()> {
        Ok(())
    }

    #[cfg(feature = "channel-lists")]
    fn handle_namreply(&self, args: &[String]) -> error::Result<()> {
        if args.len() == 4 {
            let chan = &args[2];
            let mut lists = self.chanlists.write();
            // Only a channel we joined has a list.
            let Some(vec) = lists.get_mut(chan) else {
                return Ok(());
            };
            // The first line of a new reply replaces the old list. A new `Vec` also
            // gives back the capacity the old one kept.
            if self.names_done.write().remove(chan) {
                *vec = Vec::new();
                self.ledger.lock().clear(chan);
                self.forget_names_index(chan);
            }
            // One nick appears once; the later entry wins so its modes are kept. The index
            // keeps a reply of N names linear instead of scanning the list per name.
            // The first line of a reply starts from a fresh index.
            let mut indexes = self.names_index.write();
            let index = indexes.entry(chan.clone()).or_insert_with(|| {
                vec.iter()
                    .enumerate()
                    .map(|(i, u)| (u.get_nickname().to_owned(), i))
                    .collect()
            });
            for user in args[3].split(' ') {
                let user = User::new(user);
                // Over a limit the list would be incomplete, so end the connection. A
                // replaced entry is charged only for the difference.
                let old = index.get(user.get_nickname()).copied();
                let freed = old.map_or(Usage::default(), |i| Usage::of_user(&vec[i]));
                self.ledger
                    .lock()
                    .charge(chan, freed, Usage::of_user(&user))?;
                match old {
                    Some(i) => vec[i] = user,
                    None => {
                        index.insert(user.get_nickname().to_owned(), vec.len());
                        vec.push(user);
                    }
                }
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "channel-lists"))]
    fn handle_endofnames(&self, _: &[String]) {}

    #[cfg(feature = "channel-lists")]
    fn handle_endofnames(&self, args: &[String]) {
        if let Some(chan) = args.get(1) {
            if self.chanlists.read().contains_key(chan) {
                self.names_done.write().insert(chan.clone());
            }
        }
    }

    #[cfg(feature = "ctcp")]
    fn handle_ctcp(&self, resp: &str, tokens: &[&str]) -> error::Result<()> {
        if tokens.is_empty() {
            return Ok(());
        }
        if tokens[0].eq_ignore_ascii_case("FINGER") {
            self.send_ctcp_internal(
                resp,
                &format!(
                    "FINGER :{} ({})",
                    self.config().real_name(),
                    self.config().username()
                ),
            )
        } else if tokens[0].eq_ignore_ascii_case("VERSION") {
            self.send_ctcp_internal(resp, &format!("VERSION {}", self.config().version()))
        } else if tokens[0].eq_ignore_ascii_case("SOURCE") {
            self.send_ctcp_internal(resp, &format!("SOURCE {}", self.config().source()))
        } else if tokens[0].eq_ignore_ascii_case("PING") && tokens.len() > 1 {
            self.send_ctcp_internal(resp, &format!("PING {}", tokens[1]))
        } else if tokens[0].eq_ignore_ascii_case("TIME") {
            self.send_ctcp_internal(resp, &format!("TIME :{}", Local::now().to_rfc2822()))
        } else if tokens[0].eq_ignore_ascii_case("USERINFO") {
            self.send_ctcp_internal(resp, &format!("USERINFO :{}", self.config().user_info()))
        } else {
            Ok(())
        }
    }

    #[cfg(feature = "ctcp")]
    fn send_ctcp_internal(&self, target: &str, msg: &str) -> error::Result<()> {
        self.send_notice(target, format!("\u{001}{}\u{001}", msg))
    }

    #[cfg(not(feature = "ctcp"))]
    fn handle_ctcp(&self, _: &str, _: &[&str]) -> error::Result<()> {
        Ok(())
    }

    pub_state_base!();
}

/// Thread-safe sender that can be used with the client.
#[derive(Debug, Clone)]
pub struct Sender {
    tx_outgoing: OutgoingSender,
}

impl Sender {
    /// Send a single message to the outgoing queue, which fails with
    /// `Error::OutgoingQueueFull` when the connection does not drain it.
    pub fn send<M: Into<Message>>(&self, msg: M) -> error::Result<()> {
        self.tx_outgoing.send(msg.into())
    }

    pub_state_base!();
    pub_sender_base!();
}

/// Future to handle outgoing messages.
///
/// Note: this is essentially the same as a version of [SendAll](https://github.com/rust-lang-nursery/futures-rs/blob/master/futures-util/src/sink/send_all.rs) that owns it's sink and stream.
#[derive(Debug)]
pub struct Outgoing {
    sink: SplitSink<Connection, Message>,
    stream: OutgoingReceiver,
    buffered: Option<Message>,
}

impl Outgoing {
    fn try_start_send(
        &mut self,
        cx: &mut Context<'_>,
        message: Message,
    ) -> Poll<Result<(), error::Error>> {
        debug_assert!(self.buffered.is_none());

        match Pin::new(&mut self.sink).poll_ready(cx)? {
            Poll::Ready(()) => Poll::Ready(Pin::new(&mut self.sink).start_send(message)),
            Poll::Pending => {
                self.buffered = Some(message);
                Poll::Pending
            }
        }
    }
}

impl FusedFuture for Outgoing {
    fn is_terminated(&self) -> bool {
        // NB: outgoing stream never terminates.
        // TODO: should it terminate if rx_outgoing is terminated?
        false
    }
}

impl Future for Outgoing {
    type Output = error::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;

        if let Some(message) = this.buffered.take() {
            ready!(this.try_start_send(cx, message))?
        }

        loop {
            match this.stream.poll_recv(cx) {
                Poll::Ready(Some(message)) => ready!(this.try_start_send(cx, message))?,
                Poll::Ready(None) => {
                    ready!(Pin::new(&mut this.sink).poll_flush(cx))?;
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => {
                    ready!(Pin::new(&mut this.sink).poll_flush(cx))?;
                    return Poll::Pending;
                }
            }
        }
    }
}

/// The canonical implementation of a connection to an IRC server.
///
/// For a full example usage, see [`irc::client`].
#[derive(Debug)]
pub struct Client {
    /// The internal, thread-safe server state.
    state: Arc<ClientState>,
    incoming: Option<SplitStream<Connection>>,
    outgoing: Option<Outgoing>,
    sender: Sender,
    #[cfg(test)]
    /// A view of the logs for a mock connection.
    view: Option<self::transport::LogView>,
}

impl Client {
    /// Creates a new `Client` from the configuration at the specified path, connecting
    /// immediately. This function is short-hand for loading the configuration and then calling
    /// `Client::from_config` and consequently inherits its behaviors.
    ///
    /// # Example
    /// ```no_run
    /// # use irc::client::prelude::*;
    /// # #[tokio::main]
    /// # async fn main() -> irc::error::Result<()> {
    /// let client = Client::new("config.toml").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new<P: AsRef<Path>>(config: P) -> error::Result<Client> {
        Client::from_config(Config::load(config)?).await
    }

    /// Creates a `Future` of an `Client` from the specified configuration and on the event loop
    /// corresponding to the given handle. This can be used to set up a number of `Clients` on a
    /// single, shared event loop. It can also be used to take more control over execution and error
    /// handling. Connection will not occur until the event loop is run.
    pub async fn from_config(config: Config) -> error::Result<Client> {
        let (tx_outgoing, rx_outgoing) = queue::channel();
        let conn = Connection::new(&config, tx_outgoing.clone()).await?;

        #[cfg(test)]
        let view = conn.log_view();

        let (sink, incoming) = conn.split();

        let sender = Sender { tx_outgoing };

        Ok(Client {
            sender: sender.clone(),
            state: Arc::new(ClientState::new(sender, config)),
            incoming: Some(incoming),
            outgoing: Some(Outgoing {
                sink,
                stream: rx_outgoing,
                buffered: None,
            }),
            #[cfg(test)]
            view,
        })
    }

    /// Gets the log view from the internal transport. Only used for unit testing.
    #[cfg(test)]
    fn log_view(&self) -> &self::transport::LogView {
        self.view
            .as_ref()
            .expect("there should be a log during testing")
    }

    /// Take the outgoing future in order to drive it yourself.
    ///
    /// Must be called before `stream` if you intend to drive this future
    /// yourself.
    pub fn outgoing(&mut self) -> Option<Outgoing> {
        self.outgoing.take()
    }

    /// Get access to a thread-safe sender that can be used with the client.
    pub fn sender(&self) -> Sender {
        self.sender.clone()
    }

    /// Gets the configuration being used with this `Client`.
    fn config(&self) -> &Config {
        &self.state.config
    }

    /// Gets a stream of incoming messages from the `Client`'s connection. This is only necessary
    /// when trying to set up more complex clients, and requires use of the `futures` crate. Most
    /// You can find some examples of setups using `stream` in the
    /// [GitHub repository](https://github.com/aatxe/irc/tree/stable/examples).
    ///
    /// **Note**: The stream can only be returned once. Subsequent attempts will cause a panic.
    // FIXME: when impl traits stabilize, we should change this return type.
    pub fn stream(&mut self) -> error::Result<ClientStream> {
        let stream = self
            .incoming
            .take()
            .ok_or(error::Error::StreamAlreadyConfigured)?;

        Ok(ClientStream {
            state: Arc::clone(&self.state),
            stream,
            outgoing: self.outgoing.take(),
        })
    }

    /// Gets a list of currently joined channels. This will be `None` if tracking is disabled
    /// altogether by disabling the `channel-lists` feature.
    #[cfg(feature = "channel-lists")]
    pub fn list_channels(&self) -> Option<Vec<String>> {
        Some(
            self.state
                .chanlists
                .read()
                .keys()
                .map(|k| k.to_owned())
                .collect(),
        )
    }

    /// Drops the tracked list of `chan`, like a PART we send does. For a caller that knows
    /// better than the built-in nickname comparison that it was kicked (see vendor/README.md).
    #[cfg(feature = "channel-lists")]
    pub fn forget_channel_list(&self, chan: &str) {
        self.state.drop_channel_list(chan);
    }

    /// Does nothing since `channel-lists` feature is disabled.
    #[cfg(not(feature = "channel-lists"))]
    pub fn forget_channel_list(&self, _: &str) {}

    /// Always returns `None` since `channel-lists` feature is disabled.
    #[cfg(not(feature = "channel-lists"))]
    pub fn list_channels(&self) -> Option<Vec<String>> {
        None
    }

    /// Gets a list of [`Users`] in the specified channel. If the
    /// specified channel hasn't been joined or the `channel-lists` feature is disabled, this function
    /// will return `None`.
    ///
    /// For best results, be sure to request `multi-prefix` support from the server. This will allow
    /// for more accurate tracking of user rank (e.g. oper, half-op, etc.).
    /// # Requesting multi-prefix support
    /// ```no_run
    /// # use irc::client::prelude::*;
    /// use irc::proto::caps::Capability;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> irc::error::Result<()> {
    /// # let client = Client::new("config.toml").await?;
    /// client.send_cap_req(&[Capability::MultiPrefix])?;
    /// client.identify()?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "channel-lists")]
    pub fn list_users(&self, chan: &str) -> Option<Vec<User>> {
        self.state.chanlists.read().get(&chan.to_owned()).cloned()
    }

    /// Always returns `None` since `channel-lists` feature is disabled.
    #[cfg(not(feature = "channel-lists"))]
    pub fn list_users(&self, _: &str) -> Option<Vec<User>> {
        None
    }

    /// Gets the current nickname in use. This may be the primary username set in the configuration,
    /// or it could be any of the alternative nicknames listed as well. As a result, this is the
    /// preferred way to refer to the client's nickname.
    pub fn current_nickname(&self) -> &str {
        self.state.current_nickname()
    }

    /// Sends a [`Command`] as this `Client`. This is the
    /// core primitive for sending messages to the server.
    ///
    /// # Example
    /// ```no_run
    /// # use irc::client::prelude::*;
    /// # #[tokio::main]
    /// # async fn main() {
    /// # let client = Client::new("config.toml").await.unwrap();
    /// client.send(Command::NICK("example".to_owned())).unwrap();
    /// client.send(Command::USER("user".to_owned(), "0".to_owned(), "name".to_owned())).unwrap();
    /// # }
    /// ```
    pub fn send<M: Into<Message>>(&self, msg: M) -> error::Result<()> {
        self.state.send(msg)
    }

    /// Sends a CAP END, NICK and USER to identify.
    pub fn identify(&self) -> error::Result<()> {
        // Send a CAP END to signify that we're IRCv3-compliant (and to end negotiations!).
        self.send(CAP(None, END, None, None))?;
        if self.config().password() != "" {
            self.send(PASS(self.config().password().to_owned()))?;
        }
        self.send(NICK(self.config().nickname()?.to_owned()))?;
        self.send(USER(
            self.config().username().to_owned(),
            "0".to_owned(),
            self.config().real_name().to_owned(),
        ))?;
        Ok(())
    }

    pub_state_base!();
    pub_sender_base!();
}

#[cfg(test)]
mod test {
    use std::{collections::HashMap, default::Default, thread, time::Duration};

    use super::Client;
    #[cfg(feature = "channel-lists")]
    use super::{
        Ledger, Usage, ENTRY_OVERHEAD, MAX_BYTES_PER_CHANNEL, MAX_BYTES_TOTAL, MAX_LISTED_CHANNELS,
        MAX_MEMBERS_PER_CHANNEL, MAX_MEMBERS_TOTAL,
    };
    #[cfg(feature = "channel-lists")]
    use crate::client::data::User;
    use crate::{
        client::data::Config,
        error::Error,
        proto::{
            command::Command::{Raw, PRIVMSG},
            ChannelMode, IrcCodec, Mode, UserMode,
        },
    };
    use anyhow::Result;
    use futures::prelude::*;

    pub fn test_config() -> Config {
        Config {
            owners: vec!["test".to_string()],
            nickname: Some("test".to_string()),
            alt_nicks: vec!["test2".to_string()],
            server: Some("irc.test.net".to_string()),
            channels: vec!["#test".to_string(), "#test2".to_string()],
            user_info: Some("Testing.".to_string()),
            use_mock_connection: true,
            ..Default::default()
        }
    }

    pub fn get_client_value(client: Client) -> String {
        // We sleep here because of synchronization issues.
        // We can't guarantee that everything will have been sent by the time of this call.
        thread::sleep(Duration::from_millis(100));
        client
            .log_view()
            .sent()
            .unwrap()
            .iter()
            .fold(String::new(), |mut acc, msg| {
                // NOTE: we have to sanitize here because sanitization happens in IrcCodec after the
                // messages are converted into strings, but our transport logger catches messages before
                // they ever reach that point.
                acc.push_str(&IrcCodec::sanitize(msg.to_string()));
                acc
            })
    }

    #[tokio::test]
    async fn stream() -> Result<()> {
        let exp = "PRIVMSG test :Hi!\r\nPRIVMSG test :This is a test!\r\n\
                   :test!test@test JOIN #test\r\n";

        let mut client = Client::from_config(Config {
            mock_initial_value: Some(exp.to_owned()),
            ..test_config()
        })
        .await?;

        client.stream()?.collect().await?;
        // assert_eq!(&messages[..], exp);
        Ok(())
    }

    #[tokio::test]
    async fn handle_message() -> Result<()> {
        let value = ":irc.test.net 376 test :End of /MOTD command.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "JOIN #test\r\nJOIN #test2\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn handle_end_motd_with_nick_password() -> Result<()> {
        let value = ":irc.test.net 376 test :End of /MOTD command.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            nick_password: Some("password".to_string()),
            channels: vec!["#test".to_string(), "#test2".to_string()],
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "NICKSERV IDENTIFY password\r\nJOIN #test\r\n\
             JOIN #test2\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn handle_end_motd_with_chan_keys() -> Result<()> {
        let value = ":irc.test.net 376 test :End of /MOTD command\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            nickname: Some("test".to_string()),
            channels: vec!["#test".to_string(), "#test2".to_string()],
            channel_keys: {
                let mut map = HashMap::new();
                map.insert("#test2".to_string(), "password".to_string());
                map
            },
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "JOIN #test\r\nJOIN #test2 password\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn handle_end_motd_with_ghost() -> Result<()> {
        let value = ":irc.test.net 433 * test :Nickname is already in use.\r\n\
                     :irc.test.net 376 test2 :End of /MOTD command.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            nickname: Some("test".to_string()),
            alt_nicks: vec!["test2".to_string()],
            nick_password: Some("password".to_string()),
            channels: vec!["#test".to_string(), "#test2".to_string()],
            should_ghost: true,
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "NICK test2\r\nNICKSERV GHOST test password\r\n\
             NICK test\r\nNICKSERV IDENTIFY password\r\nJOIN #test\r\nJOIN #test2\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn handle_end_motd_with_ghost_seq() -> Result<()> {
        let value = ":irc.test.net 433 * test :Nickname is already in use.\r\n\
                     :irc.test.net 376 test2 :End of /MOTD command.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            nickname: Some("test".to_string()),
            alt_nicks: vec!["test2".to_string()],
            nick_password: Some("password".to_string()),
            channels: vec!["#test".to_string(), "#test2".to_string()],
            should_ghost: true,
            ghost_sequence: Some(vec!["RECOVER".to_string(), "RELEASE".to_string()]),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "NICK test2\r\nNICKSERV RECOVER test password\
             \r\nNICKSERV RELEASE test password\r\nNICK test\r\nNICKSERV IDENTIFY password\
             \r\nJOIN #test\r\nJOIN #test2\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn handle_end_motd_with_umodes() -> Result<()> {
        let value = ":irc.test.net 376 test :End of /MOTD command.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            nickname: Some("test".to_string()),
            umodes: Some("+B".to_string()),
            channels: vec!["#test".to_string(), "#test2".to_string()],
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "MODE test +B\r\nJOIN #test\r\nJOIN #test2\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn nickname_in_use() -> Result<()> {
        let value = ":irc.test.net 433 * test :Nickname is already in use.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "NICK test2\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn ran_out_of_nicknames() -> Result<()> {
        let value = ":irc.test.net 433 * test :Nickname is already in use.\r\n\
                     :irc.test.net 433 * test2 :Nickname is already in use.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        let res = client.stream()?.try_collect::<Vec<_>>().await;
        if let Err(Error::NoUsableNick) = res {
        } else {
            panic!("expected error when no valid nicks were specified")
        }
        Ok(())
    }

    #[tokio::test]
    async fn send() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        assert!(client
            .send(PRIVMSG("#test".to_string(), "Hi there!".to_string()))
            .is_ok());
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG #test :Hi there!\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_no_newline_injection() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        assert!(client
            .send(PRIVMSG(
                "#test".to_string(),
                "Hi there!\r\nJOIN #bad".to_string()
            ))
            .is_ok());
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG #test :Hi there!\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_raw_is_really_raw() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        assert!(client
            .send(Raw("PASS".to_owned(), vec!["password".to_owned()]))
            .is_ok());
        assert!(client
            .send(Raw("NICK".to_owned(), vec!["test".to_owned()]))
            .is_ok());
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PASS password\r\nNICK test\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn channel_tracking_names() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #test\r\n:irc.test.net 353 test = #test :test ~owner &admin\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(client.list_channels().unwrap(), vec!["#test".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn channel_tracking_names_part() -> Result<()> {
        use crate::proto::command::Command::PART;

        let value = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #test\r\n:irc.test.net 353 test = #test :test ~owner &admin\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;

        // Keep the stream alive: once it is dropped the queue is closed and sends fail.
        let mut stream = client.stream()?;
        while stream.next().await.is_some() {}

        assert_eq!(client.list_channels(), Some(vec!["#test".to_owned()]));
        // we ignore the result: the client state is updated once the message is queued.
        let _ = client.send(PART("#test".to_string(), None));
        assert_eq!(client.list_channels(), Some(vec![]));
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn refused_part_keeps_the_channel_list() -> Result<()> {
        use crate::proto::command::Command::PART;

        let value = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #test\r\n:irc.test.net 353 test = #test :test ~owner\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;

        // Keep the stream alive, so the queue stays open but is never drained.
        let mut stream = client.stream()?;
        while stream.next().await.is_some() {}

        let mut refused = false;
        for _ in 0..=super::queue::MAX_QUEUED_MESSAGES {
            if matches!(
                client.send(PRIVMSG("#x".to_string(), "x".to_string())),
                Err(Error::OutgoingQueueFull)
            ) {
                refused = true;
                break;
            }
        }
        assert!(refused);

        assert!(matches!(
            client.send(PART("#test".to_string(), None)),
            Err(Error::OutgoingQueueFull)
        ));
        assert_eq!(client.list_channels(), Some(vec!["#test".to_owned()]));
        assert_eq!(client.list_users("#test").map(|u| u.len()), Some(2));
        drop(stream);
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn user_tracking_names() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #test\r\n:irc.test.net 353 test = #test :test ~owner &admin\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            client.list_users("#test").unwrap(),
            vec![User::new("test"), User::new("~owner"), User::new("&admin")]
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn user_tracking_names_join() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #test\r\n:irc.test.net 353 test = #test :test ~owner &admin\r\n\
                     :test2!test@test JOIN #test\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            client.list_users("#test").unwrap(),
            vec![
                User::new("test"),
                User::new("~owner"),
                User::new("&admin"),
                User::new("test2"),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn user_tracking_names_kick() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #test\r\n:irc.test.net 353 test = #test :test ~owner &admin test2\r\n\
                     :owner!test@test KICK #test test2\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            client.list_users("#test").unwrap(),
            vec![User::new("test"), User::new("~owner"), User::new("&admin")]
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn user_tracking_names_part() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #test\r\n:irc.test.net 353 test = #test :test ~owner &admin\r\n\
                     :owner!test@test PART #test\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            client.list_users("#test").unwrap(),
            vec![User::new("test"), User::new("&admin")]
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn user_tracking_names_mode() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #test\r\n:irc.test.net 353 test = #test :+test ~owner &admin\r\n\
                     :test!test@test MODE #test +o test\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            client.list_users("#test").unwrap(),
            vec![User::new("@test"), User::new("~owner"), User::new("&admin")]
        );
        let mut exp = User::new("@test");
        exp.update_access_level(&Mode::Plus(ChannelMode::Voice, None));
        assert_eq!(
            client.list_users("#test").unwrap()[0].highest_access_level(),
            exp.highest_access_level()
        );
        // The following tests if the maintained user contains the same entries as what is expected
        // but ignores the ordering of these entries.
        let mut levels = client.list_users("#test").unwrap()[0].access_levels();
        levels.retain(|l| exp.access_levels().contains(l));
        assert_eq!(levels.len(), exp.access_levels().len());
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn names_for_unjoined_channel_is_ignored() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n\
                     :irc.test.net 353 test = #ghost :x1 x2\r\n\
                     :irc.test.net 366 test #ghost :End of /NAMES list.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(client.list_channels(), Some(vec![]));
        assert!(client.list_users("#ghost").is_none());
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn names_twice_rebuilds_list() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n\
                     :test!t@h JOIN #test\r\n\
                     :irc.test.net 353 test = #test :@alice bob\r\n\
                     :irc.test.net 353 test = #test :carol\r\n\
                     :irc.test.net 366 test #test :End of /NAMES list.\r\n\
                     :irc.test.net 353 test = #test :@alice bob carol\r\n\
                     :irc.test.net 353 test = #test :bob\r\n\
                     :irc.test.net 366 test #test :End of /NAMES list.\r\n\
                     :alice!a@h MODE #test +o bob\r\n\
                     :carol!c@h PART #test\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            client.list_users("#test").unwrap(),
            vec![User::new("@alice"), User::new("@bob")]
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn names_twice_keeps_roles() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n\
                     :test!t@h JOIN #test\r\n\
                     :irc.test.net 353 test = #test :@alice bob\r\n\
                     :irc.test.net 366 test #test :End of /NAMES list.\r\n\
                     :irc.test.net 353 test = #test :alice @bob\r\n\
                     :irc.test.net 366 test #test :End of /NAMES list.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        let users = client.list_users("#test").unwrap();
        assert_eq!(users.len(), 2);
        // `User`'s equality ignores roles, so check them directly.
        let level = |n: &str| {
            users
                .iter()
                .find(|u| u.get_nickname() == n)
                .unwrap()
                .highest_access_level()
        };
        assert_eq!(level("alice"), crate::client::data::AccessLevel::Member);
        assert_eq!(level("bob"), crate::client::data::AccessLevel::Oper);
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn join_during_names_takes_the_names_role() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n\
                     :test!t@h JOIN #test\r\n\
                     :irc.test.net 353 test = #test :test\r\n\
                     :bob!b@h JOIN #test\r\n\
                     :irc.test.net 353 test = #test :@bob\r\n\
                     :irc.test.net 366 test #test :End of /NAMES list.\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        let users = client.list_users("#test").unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(
            users[1].highest_access_level(),
            crate::client::data::AccessLevel::Oper
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn leaving_channels_leaves_no_state() -> Result<()> {
        let mut value = String::from(":irc.test.net 001 test :Welcome\r\n");
        for i in 0..200 {
            value.push_str(&format!(
                ":test!t@h JOIN #c{i}\r\n:irc.test.net 353 test = #c{i} :test x\r\n\
                 :irc.test.net 366 test #c{i} :End of /NAMES list.\r\n\
                 :x!x@h KICK #c{i} test\r\n"
            ));
        }
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert!(client.state.chanlists.read().is_empty());
        assert!(client.state.names_done.read().is_empty());
        client.forget_channel_list("#c1");
        assert!(client.state.names_done.read().is_empty());
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn a_large_names_reply_is_taken_in_linear_time() -> Result<()> {
        let mut value = String::from(":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #big\r\n");
        let mut nick = 0;
        for _ in 0..2000 {
            let names: Vec<String> = (0..20)
                .map(|_| {
                    nick += 1;
                    format!("n{nick}")
                })
                .collect();
            value.push_str(&format!(
                ":irc.test.net 353 test = #big :{}\r\n",
                names.join(" ")
            ));
        }
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value),
            ..test_config()
        })
        .await?;
        let start = std::time::Instant::now();
        client.stream()?.collect().await?;
        assert_eq!(client.list_users("#big").unwrap().len(), 40_000);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        Ok(())
    }

    #[cfg(feature = "channel-lists")]
    fn names_lines(chan: &str, from: usize, count: usize) -> String {
        let mut out = String::new();
        let mut n = from;
        while n < from + count {
            let end = (n + 20).min(from + count);
            let names: Vec<String> = (n..end).map(|i| format!("n{i}")).collect();
            out.push_str(&format!(
                ":irc.test.net 353 test = {chan} :{}\r\n",
                names.join(" ")
            ));
            n = end;
        }
        out
    }

    #[cfg(feature = "channel-lists")]
    async fn run_lines(value: String) -> (Client, std::result::Result<(), Error>) {
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value),
            ..test_config()
        })
        .await
        .unwrap();
        let res = client.stream().unwrap().collect().await.map(drop);
        (client, res)
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn names_per_channel_limit_ends_the_connection() {
        let head = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #a\r\n";
        // Exactly at the limit, with duplicates ignored, is accepted.
        let at = format!(
            "{head}{}{}",
            names_lines("#a", 0, MAX_MEMBERS_PER_CHANNEL),
            names_lines("#a", 0, 100)
        );
        let (client, res) = run_lines(at).await;
        res.unwrap();
        assert_eq!(
            client.list_users("#a").unwrap().len(),
            MAX_MEMBERS_PER_CHANNEL
        );
        // One more is refused.
        let over = format!(
            "{head}{}",
            names_lines("#a", 0, MAX_MEMBERS_PER_CHANNEL + 1)
        );
        let (client, res) = run_lines(over).await;
        assert!(matches!(res, Err(Error::ChannelListLimit(_))));
        assert!(client.list_users("#a").unwrap().len() <= MAX_MEMBERS_PER_CHANNEL);
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn names_connection_limit_spans_channels_and_a_refresh_is_not_double_counted() {
        let mut value = String::from(":irc.test.net 001 test :Welcome\r\n");
        let chans = MAX_MEMBERS_TOTAL / MAX_MEMBERS_PER_CHANNEL;
        for c in 0..chans {
            value.push_str(&format!(":test!t@h JOIN #c{c}\r\n"));
            value.push_str(&names_lines(&format!("#c{c}"), 0, MAX_MEMBERS_PER_CHANNEL));
        }
        // A refreshed NAMES of a full channel replaces its list.
        value.push_str(":irc.test.net 366 test #c0 :End\r\n");
        value.push_str(&names_lines("#c0", 0, MAX_MEMBERS_PER_CHANNEL));
        let (_, res) = run_lines(value.clone()).await;
        res.unwrap();
        // One member in another channel passes the total.
        value.push_str(":test!t@h JOIN #extra\r\n:x!x@h JOIN #extra\r\n");
        let (_, res) = run_lines(value).await;
        assert!(matches!(res, Err(Error::ChannelListLimit(_))));
    }

    /// NAMES lines of `count` distinct nicknames of `len` bytes each, one per line.
    #[cfg(feature = "channel-lists")]
    fn long_names(chan: &str, from: usize, count: usize, len: usize) -> String {
        (from..from + count)
            .map(|i| {
                let nick = format!("{i:0>len$}");
                format!(":irc.test.net 353 test = {chan} :{nick}\r\n")
            })
            .collect()
    }

    // A long nickname costs `ENTRY_OVERHEAD` + 2 * 400 bytes (see `Usage::of_user`).
    #[cfg(feature = "channel-lists")]
    const LONG_NICK: usize = 400;
    #[cfg(feature = "channel-lists")]
    const LONG_COST: usize = ENTRY_OVERHEAD + 2 * LONG_NICK;

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn names_byte_budget_per_channel_ends_the_connection() {
        let head = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #a\r\n";
        let fit = MAX_BYTES_PER_CHANNEL / LONG_COST;
        // Right up to the budget is accepted.
        let at = format!("{head}{}", long_names("#a", 0, fit, LONG_NICK));
        let (client, res) = run_lines(at.clone()).await;
        res.unwrap();
        assert_eq!(client.list_users("#a").unwrap().len(), fit);
        // One more is refused and the list never passes the budget.
        let over = format!("{at}{}", long_names("#a", fit, 1, LONG_NICK));
        let (client, res) = run_lines(over).await;
        assert!(matches!(res, Err(Error::ChannelListLimit(_))));
        assert_eq!(client.list_users("#a").unwrap().len(), fit);
        // A repeated nickname replaced by a longer one counts the growth.
        let grown = format!("{at}:irc.test.net 353 test = #a :{:0>1000}\r\n", 0);
        let (_, res) = run_lines(grown).await;
        assert!(matches!(res, Err(Error::ChannelListLimit(_))));
        // A refreshed NAMES gives the bytes back.
        let refresh = format!(
            "{at}:irc.test.net 366 test #a :End\r\n{}",
            long_names("#a", 0, fit, LONG_NICK)
        );
        let (_, res) = run_lines(refresh).await;
        res.unwrap();
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn names_byte_budget_spans_channels() {
        let mut value = String::from(":irc.test.net 001 test :Welcome\r\n");
        let per = MAX_BYTES_PER_CHANNEL / LONG_COST;
        let full = MAX_BYTES_TOTAL / MAX_BYTES_PER_CHANNEL;
        for c in 0..full {
            value.push_str(&format!(":test!t@h JOIN #c{c}\r\n"));
            value.push_str(&long_names(&format!("#c{c}"), 0, per, LONG_NICK));
        }
        let (_, res) = run_lines(value.clone()).await;
        res.unwrap();
        // The rest of the total budget is smaller than one more long nickname.
        value.push_str(":test!t@h JOIN #extra\r\n");
        let room = (MAX_BYTES_TOTAL - full * per * LONG_COST) / LONG_COST;
        value.push_str(&long_names("#extra", per, room + 1, LONG_NICK));
        let (_, res) = run_lines(value).await;
        assert!(matches!(res, Err(Error::ChannelListLimit(_))));
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn nick_change_cannot_grow_a_list_past_the_budget() {
        let head = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #a\r\n";
        let fit = MAX_BYTES_PER_CHANNEL / LONG_COST;
        let value = format!(
            "{head}{}:irc.test.net 353 test = #a :short\r\n:short!u@h NICK :{:0>2000}\r\n",
            long_names("#a", 0, fit - 1, LONG_NICK),
            0
        );
        let (_, res) = run_lines(value).await;
        assert!(matches!(res, Err(Error::ChannelListLimit(_))));
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn join_and_channel_count_are_limited() {
        let mut value = String::from(":irc.test.net 001 test :Welcome\r\n");
        for c in 0..MAX_LISTED_CHANNELS {
            value.push_str(&format!(":test!t@h JOIN #c{c}\r\n"));
        }
        let (client, res) = run_lines(value.clone()).await;
        res.unwrap();
        assert_eq!(
            client.list_channels().map_or(0, |c| c.len()),
            MAX_LISTED_CHANNELS
        );
        value.push_str(":test!t@h JOIN #one-too-many\r\n");
        let (_, res) = run_lines(value).await;
        assert!(matches!(res, Err(Error::ChannelListLimit(_))));
    }

    /// What the lists hold, counted from scratch.
    #[cfg(feature = "channel-lists")]
    fn recount(client: &Client) -> (usize, usize) {
        let lists = client.state.chanlists.read();
        lists
            .values()
            .flatten()
            .fold((0, 0), |(m, b), u| (m + 1, b + Usage::of_user(u).bytes))
    }

    #[test]
    #[cfg(feature = "channel-lists")]
    fn ledger_limit_is_exact_and_a_refusal_changes_nothing() {
        let mut ledger = Ledger::default();
        let one = Usage {
            members: 1,
            bytes: 1,
        };
        let full = Usage {
            members: 1,
            bytes: MAX_BYTES_PER_CHANNEL - 1,
        };
        ledger.charge("#a", Usage::default(), full).unwrap();
        // The byte that fills the channel exactly fits, the next one does not.
        ledger.charge("#a", Usage::default(), one).unwrap();
        assert!(ledger.charge("#a", Usage::default(), one).is_err());
        assert_eq!(ledger.channels["#a"].bytes, MAX_BYTES_PER_CHANNEL);
        assert_eq!(ledger.total.bytes, MAX_BYTES_PER_CHANNEL);
        // Giving back makes room again; a channel's removal gives back all of it.
        ledger.release("#a", one);
        ledger.charge("#a", Usage::default(), one).unwrap();
        ledger.clear("#a");
        assert_eq!(ledger.total.bytes, 0);
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn same_nick_with_user_and_host_counts_the_growth() {
        let head = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #a\r\n";
        let fit = MAX_BYTES_PER_CHANNEL / LONG_COST;
        let at = format!("{head}{}", long_names("#a", 0, fit, LONG_NICK));
        let nick = format!("{:0>LONG_NICK$}", 0);
        let grown = format!(
            "{at}:irc.test.net 353 test = #a :{nick}!{:x<600}@{:x<600}\r\n",
            "", ""
        );
        let (_, res) = run_lines(grown).await;
        assert!(matches!(res, Err(Error::ChannelListLimit(_))));
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn refreshed_list_gives_back_its_capacity() {
        let head = ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #a\r\n";
        let value = format!(
            "{head}{}:irc.test.net 366 test #a :End\r\n:irc.test.net 353 test = #a :one\r\n",
            long_names("#a", 0, 5000, 8)
        );
        let (client, res) = run_lines(value).await;
        res.unwrap();
        assert!(client.state.chanlists.read()["#a"].capacity() < 64);
        assert_eq!(client.state.ledger.lock().total.members, 1);
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn leaving_members_give_back_their_capacity() {
        let mut value = format!(
            ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #a\r\n{}",
            long_names("#a", 0, 5000, 8)
        );
        for i in 1..5000 {
            value.push_str(&format!(":{i:0>8}!u@h QUIT :bye\r\n"));
        }
        let (client, res) = run_lines(value).await;
        res.unwrap();
        assert!(client.state.chanlists.read()["#a"].capacity() < 64);
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn access_levels_stay_a_set_however_long_the_prefix() {
        let prefix = "@".repeat(400);
        let value = format!(
            ":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #a\r\n\
             :irc.test.net 353 test = #a :{prefix}nick\r\n\
             :op!o@h MODE #a +o nick\r\n:op!o@h MODE #a +o nick\r\n"
        );
        let (client, res) = run_lines(value).await;
        res.unwrap();
        let users = client.list_users("#a").unwrap();
        assert_eq!(users[0].access_levels().len(), 2);
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn ledger_follows_every_change_to_the_lists() {
        let value = ":irc.test.net 001 test :Welcome\r\n\
             :test!t@h JOIN #a\r\n:test!t@h JOIN #b\r\n\
             :irc.test.net 353 test = #a :@a1 a2 a3\r\n\
             :irc.test.net 353 test = #b :b1 b2\r\n\
             :irc.test.net 353 test = #a :a4 a2!u@h\r\n\
             :x!x@h JOIN #a\r\n:a1!x@h PART #a\r\n:b1!x@h QUIT :bye\r\n\
             :a3!x@h NICK :a3-longer-nick\r\n\
             :irc.test.net 366 test #b :End\r\n:irc.test.net 353 test = #b :b9\r\n\
             :test!t@h JOIN #b\r\n:irc.test.net 353 test = #b :b7 b8\r\n";
        let (client, res) = run_lines(value.to_owned()).await;
        res.unwrap();
        let ledger = client.state.ledger.lock();
        assert_eq!((ledger.total.members, ledger.total.bytes), recount(&client));
        // #a keeps a2, a3-longer-nick, a4 and x; #b was replaced by its own JOIN.
        assert_eq!(ledger.channels["#a"].members, 4);
        assert_eq!(ledger.channels["#b"].members, 2);
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn alternating_names_and_consecutive_joins_keep_linear_time() -> Result<()> {
        let mut value = String::from(
            ":irc.test.net 001 test :Welcome\r\n\
             :test!t@h JOIN #a\r\n:test!t@h JOIN #b\r\n:test!t@h JOIN #c\r\n",
        );
        for i in 0..20_000 {
            value.push_str(&format!(":irc.test.net 353 test = #a :a{i}\r\n"));
            value.push_str(&format!(":irc.test.net 353 test = #b :b{i}\r\n"));
        }
        for i in 0..40_000 {
            value.push_str(&format!(":j{i}!u@h JOIN #c\r\n"));
        }
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value),
            ..test_config()
        })
        .await?;
        let start = std::time::Instant::now();
        client.stream()?.collect().await?;
        assert_eq!(client.list_users("#a").unwrap().len(), 20_000);
        assert_eq!(client.list_users("#c").unwrap().len(), 40_000);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn messages_between_names_lines_keep_linear_time() -> Result<()> {
        let mut value = String::from(":irc.test.net 001 test :Welcome\r\n:test!t@h JOIN #big\r\n");
        let mut nick = 0;
        for _ in 0..2000 {
            let names: Vec<String> = (0..20)
                .map(|_| {
                    nick += 1;
                    format!("n{nick}")
                })
                .collect();
            value.push_str(":irc.test.net NOTICE test :progress\r\n");
            value.push_str(&format!(
                ":irc.test.net 353 test = #big :{}\r\n",
                names.join(" ")
            ));
        }
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value),
            ..test_config()
        })
        .await?;
        let start = std::time::Instant::now();
        client.stream()?.collect().await?;
        assert_eq!(client.list_users("#big").unwrap().len(), 40_000);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn part_between_names_lines_keeps_the_index_right() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n\
                     :test!t@h JOIN #test\r\n\
                     :irc.test.net 353 test = #test :a b c\r\n\
                     :a!a@h PART #test\r\n\
                     :irc.test.net 353 test = #test :@c d\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        let mut users: Vec<String> = client
            .list_users("#test")
            .unwrap()
            .iter()
            .map(|u| u.get_nickname().to_owned())
            .collect();
        users.sort();
        assert_eq!(users, ["b", "c", "d"]);
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn own_join_discards_old_list() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n\
                     :test!t@h JOIN #test\r\n\
                     :irc.test.net 353 test = #test :test old\r\n\
                     :test!t@h JOIN #test\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(client.list_channels(), Some(vec!["#test".to_owned()]));
        assert_eq!(client.list_users("#test"), Some(vec![]));
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "channel-lists")]
    async fn later_end_of_motd_does_not_join() -> Result<()> {
        let value = ":irc.test.net 001 test :Welcome\r\n\
                     :irc.test.net 422 test :MOTD File is missing\r\n\
                     :test!t@h JOIN #test\r\n\
                     :irc.test.net 353 test = #ghost :x1\r\n\
                     :irc.test.net 376 test :End of /MOTD command\r\n\
                     :irc.test.net 422 test :MOTD File is missing\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            nickname: Some("test".to_string()),
            channels: vec!["#test".to_string()],
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "JOIN #test\r\n");
        Ok(())
    }

    #[tokio::test]
    #[cfg(not(feature = "channel-lists"))]
    async fn no_user_tracking() -> Result<()> {
        let value = ":irc.test.net 353 test = #test :test ~owner &admin\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert!(client.list_users("#test").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn handle_single_soh() -> Result<()> {
        let value = ":test!test@test PRIVMSG #test :\u{001}\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            nickname: Some("test".to_string()),
            channels: vec!["#test".to_string(), "#test2".to_string()],
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn finger_response() -> Result<()> {
        let value = ":test!test@test PRIVMSG test :\u{001}FINGER\u{001}\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "NOTICE test :\u{001}FINGER :test (test)\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn version_response() -> Result<()> {
        let value = ":test!test@test PRIVMSG test :\u{001}VERSION\u{001}\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            &format!(
                "NOTICE test :\u{001}VERSION {}\u{001}\r\n",
                crate::VERSION_STR,
            )
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn source_response() -> Result<()> {
        let value = ":test!test@test PRIVMSG test :\u{001}SOURCE\u{001}\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "NOTICE test :\u{001}SOURCE https://github.com/aatxe/irc\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn ctcp_ping_response() -> Result<()> {
        let value = ":test!test@test PRIVMSG test :\u{001}PING test\u{001}\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "NOTICE test :\u{001}PING test\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn time_response() -> Result<()> {
        let value = ":test!test@test PRIVMSG test :\u{001}TIME\u{001}\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        let val = get_client_value(client);
        assert!(val.starts_with("NOTICE test :\u{001}TIME :"));
        assert!(val.ends_with("\u{001}\r\n"));
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn user_info_response() -> Result<()> {
        let value = ":test!test@test PRIVMSG test :\u{001}USERINFO\u{001}\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "NOTICE test :\u{001}USERINFO :Testing.\u{001}\
             \r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn ctcp_ping_no_timestamp() -> Result<()> {
        let value = ":test!test@test PRIVMSG test \u{001}PING\u{001}\r\n";
        let mut client = Client::from_config(Config {
            mock_initial_value: Some(value.to_owned()),
            ..test_config()
        })
        .await?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "");
        Ok(())
    }

    #[tokio::test]
    async fn identify() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.identify()?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "CAP END\r\nNICK test\r\n\
             USER test 0 * test\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn identify_with_password() -> Result<()> {
        let mut client = Client::from_config(Config {
            nickname: Some("test".to_string()),
            password: Some("password".to_string()),
            ..test_config()
        })
        .await?;
        client.identify()?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "CAP END\r\nPASS password\r\nNICK test\r\n\
             USER test 0 * test\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_pong() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_pong("irc.test.net")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "PONG irc.test.net\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_join() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_join("#test,#test2,#test3")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "JOIN #test,#test2,#test3\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_part() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_part("#test")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "PART #test\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_oper() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_oper("test", "test")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "OPER test test\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_privmsg() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_privmsg("#test", "Hi, everybody!")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG #test :Hi, everybody!\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_notice() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_notice("#test", "Hi, everybody!")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "NOTICE #test :Hi, everybody!\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_topic_no_topic() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_topic("#test", "")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "TOPIC #test\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_topic() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_topic("#test", "Testing stuff.")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "TOPIC #test :Testing stuff.\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_kill() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_kill("test", "Testing kills.")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "KILL test :Testing kills.\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_kick_no_message() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_kick("#test", "test", "")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "KICK #test test\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_kick() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_kick("#test", "test", "Testing kicks.")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "KICK #test test :Testing kicks.\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_mode_no_modeparams() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_mode("#test", &[Mode::Plus(ChannelMode::InviteOnly, None)])?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "MODE #test +i\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_mode() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_mode(
            "#test",
            &[
                Mode::Plus(ChannelMode::Oper, Some("test".to_owned())),
                Mode::Minus(ChannelMode::Oper, Some("test2".to_owned())),
            ],
        )?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "MODE #test +o-o test test2\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_umode() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_mode(
            "test",
            &[
                Mode::Plus(UserMode::Invisible, None),
                Mode::Plus(UserMode::MaskedHost, None),
            ],
        )?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "MODE test +i+x\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_samode_no_modeparams() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_samode("#test", "+i", "")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "SAMODE #test +i\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_samode() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_samode("#test", "+o", "test")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "SAMODE #test +o test\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_sanick() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_sanick("test", "test2")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "SANICK test test2\r\n");
        Ok(())
    }

    #[tokio::test]
    async fn send_invite() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_invite("test", "#test")?;
        client.stream()?.collect().await?;
        assert_eq!(&get_client_value(client)[..], "INVITE test #test\r\n");
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn send_ctcp() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_ctcp("test", "LINE1\r\nLINE2\r\nLINE3")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG test \u{001}LINE1\u{001}\r\nPRIVMSG test \u{001}LINE2\u{001}\r\nPRIVMSG test \u{001}LINE3\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn send_action() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_action("test", "tests.")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG test :\u{001}ACTION tests.\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn send_finger() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_finger("test")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG test \u{001}FINGER\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn send_version() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_version("test")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG test \u{001}VERSION\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn send_source() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_source("test")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG test \u{001}SOURCE\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn send_user_info() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_user_info("test")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG test \u{001}USERINFO\u{001}\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn send_ctcp_ping() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_ctcp_ping("test")?;
        client.stream()?.collect().await?;
        let val = get_client_value(client);
        println!("{}", val);
        assert!(val.starts_with("PRIVMSG test :\u{001}PING "));
        assert!(val.ends_with("\u{001}\r\n"));
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "ctcp")]
    async fn send_time() -> Result<()> {
        let mut client = Client::from_config(test_config()).await?;
        client.send_time("test")?;
        client.stream()?.collect().await?;
        assert_eq!(
            &get_client_value(client)[..],
            "PRIVMSG test \u{001}TIME\u{001}\r\n"
        );
        Ok(())
    }
}
