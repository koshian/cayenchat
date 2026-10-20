// SPDX-License-Identifier: MPL-2.0
//! CTCP AVATAR pictures tied to a user mask (D058) through a real,
//! independent server. Ignored by default; it needs a disposable local
//! server, for example the pinned Ergo that `scripts/ergo-metadata-interop.sh`
//! builds and runs:
//!
//! ```text
//! CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 \
//!   cargo test --locked -p cayenchat-irc-core --test peer_avatar_interop -- --ignored --nocapture
//! ```
//!
//! CayenChat's own connection code is the client under test; a minimal raw
//! client plays a bot that marks its realname and answers CTCP AVATAR.
//! Nicknames and the channel carry a per-run suffix.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cayenchat_irc_core::{
    ChannelActivityKind, Connection, ConnectionConfig, Event, Ircv3Options, PeerAvatarCache,
    WireDirection,
};

const TIMEOUT: Duration = Duration::from_secs(20);
const PICTURE: &str = "https://example.com/bot.png";

fn server() -> Option<(String, u16)> {
    let address = std::env::var("CAYENCHAT_INTEROP_IRC").ok()?;
    let (host, port) = address.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

/// A bot speaking raw IRC whose realname carries KVIrc's avatar mark.
struct Bot {
    name: String,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Bot {
    fn connect(host: &str, port: u16, nick: &str, user: &str, channel: &str) -> Self {
        let writer = TcpStream::connect((host, port)).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut bot = Self {
            name: nick.to_owned(),
            reader: BufReader::new(writer.try_clone().unwrap()),
            writer,
        };
        bot.send(&format!("NICK {nick}"));
        bot.send(&format!("USER {user} 0 * :\u{3}4\u{f}Bot"));
        bot.expect(|line| line.contains(" 376 ") || line.contains(" 422 "));
        bot.send(&format!("JOIN {channel}"));
        bot.expect(|line| line.contains(" 366 "));
        bot
    }

    fn send(&mut self, line: &str) {
        println!("{} >> {line}", self.name);
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .unwrap();
    }

    fn read(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => panic!("{}: server closed the connection", self.name),
            Ok(_) => {}
            Err(_) => return None,
        }
        let line = line.trim_end().to_owned();
        println!("{} << {line}", self.name);
        if let Some(token) = line.strip_prefix("PING ") {
            self.send(&format!("PONG {token}"));
        }
        Some(line)
    }

    fn expect(&mut self, matches: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "{}: expected line did not arrive",
                self.name
            );
            if let Some(line) = self.read()
                && matches(&line)
            {
                return line;
            }
        }
    }
}

/// CayenChat's connection and everything it reported.
struct Client {
    connection: Connection,
    events: Vec<Event>,
}

impl Client {
    fn connect(host: &str, port: u16, nick: &str, channel: &str, cache: &PeerAvatarCache) -> Self {
        let mut config = ConnectionConfig::tls(host.into(), nick.into(), vec![channel.into()]);
        config.port = port;
        config.use_tls = false;
        config.ircv3 = Ircv3Options {
            peer_avatars: true,
            ..Ircv3Options::default()
        };
        config.peer_avatar_cache = cache.clone();
        let connection = Connection::connect(config).unwrap();
        connection.set_transcript(true);
        Self {
            connection,
            events: Vec::new(),
        }
    }

    fn wait(&mut self, what: &str, matches: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "CayenChat: {what} did not arrive"
            );
            let Some(event) = self.connection.try_recv() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            match &event {
                Event::Wire {
                    direction, line, ..
                } => println!(
                    "cayenchat {} {line}",
                    if *direction == WireDirection::Sent {
                        ">>"
                    } else {
                        "<<"
                    }
                ),
                Event::Diagnostic { .. } => {}
                other => println!("cayenchat event {other:?}"),
            }
            assert!(
                !matches!(event, Event::Disconnected(_)),
                "CayenChat was disconnected: {event:?}"
            );
            self.events.push(event.clone());
            if matches(&event) {
                return event;
            }
        }
    }

    /// Lines sent so far that start with `prefix`.
    fn sent(&self, prefix: &str) -> usize {
        self.events
            .iter()
            .filter(|event| {
                matches!(event, Event::Wire { direction: WireDirection::Sent, line, .. }
                    if line.starts_with(prefix))
            })
            .count()
    }
}

#[test]
#[ignore = "needs a disposable local IRC server (CAYENCHAT_INTEROP_IRC)"]
fn a_picture_follows_the_mask_through_a_real_server() {
    let Some((host, port)) = server() else {
        panic!("set CAYENCHAT_INTEROP_IRC=host:port");
    };
    let run = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 100_000;
    let me = format!("cc{run}");
    let channel = format!("#avatar{run}");
    let bot_nick = format!("bot{run}");
    let user = format!("u{run}");
    let cache = PeerAvatarCache::default();
    let mut client = Client::connect(&host, port, &me, &channel, &cache);
    client.wait("join", |event| matches!(event, Event::Joined { .. }));

    // The bot's first line comes before anything is known about it: the
    // picture arrives afterwards, for the application to show on that line.
    let mut bot = Bot::connect(&host, port, &bot_nick, &user, &channel);
    bot.send(&format!("PRIVMSG {channel} :first line"));
    client.wait(
        "first line",
        |event| matches!(event, Event::ChannelMessage { text, .. } if text == "first line"),
    );
    assert!(
        client
            .events
            .iter()
            .all(|e| !matches!(e, Event::PeerAvatar { .. }))
    );
    bot.expect(|line| line.contains("\u{1}AVATAR\u{1}"));
    bot.send(&format!("NOTICE {me} :\u{1}AVATAR {PICTURE} M\u{1}"));
    let event = client.wait("the picture", |event| {
        matches!(event, Event::PeerAvatar { .. })
    });
    assert_eq!(
        event,
        Event::PeerAvatar {
            nickname: bot_nick.clone(),
            url: Some(PICTURE.into())
        }
    );
    assert_eq!(cache.len(), 1);
    let asked = client.sent("PRIVMSG ");
    assert_eq!(asked, 1, "one CTCP AVATAR query");

    // The bot leaves and comes back with the same nick and username: the
    // mask is remembered, so the join brings the picture and nothing is asked.
    bot.send("QUIT :back soon");
    client.wait("quit", |event| {
        matches!(event, Event::ChannelActivity { actor, kind: ChannelActivityKind::Quit { .. }, .. }
            if *actor == bot_nick)
    });
    let who = client.sent("WHO ");
    let mut bot = Bot::connect(&host, port, &bot_nick, &user, &channel);
    let before = client.events.len();
    client.wait(
        "the remembered picture",
        |event| matches!(event, Event::PeerAvatar { nickname, .. } if *nickname == bot_nick),
    );
    bot.send(&format!("PRIVMSG {channel} :second line"));
    client.wait(
        "second line",
        |event| matches!(event, Event::ChannelMessage { text, .. } if text == "second line"),
    );
    thread::sleep(Duration::from_secs(3));
    while let Some(event) = client.connection.try_recv() {
        client.events.push(event);
    }
    assert!(client.events.len() > before);
    assert_eq!(client.sent("WHO "), who, "no lookup for a remembered mask");
    assert_eq!(client.sent("PRIVMSG "), asked, "no query either");

    // Another nickname is another key: it is looked up afresh.
    let other_nick = format!("{bot_nick}_");
    let mut other = Bot::connect(&host, port, &other_nick, &user, &channel);
    other.send(&format!("PRIVMSG {channel} :new name"));
    client.wait(
        "new name",
        |event| matches!(event, Event::ChannelMessage { sender, .. } if *sender == other_nick),
    );
    client.wait("a lookup", |event| {
        matches!(event, Event::Wire { direction: WireDirection::Sent, line, .. }
            if *line == format!("WHO {other_nick}"))
    });
    other.send("QUIT");
    bot.send("QUIT");
}
