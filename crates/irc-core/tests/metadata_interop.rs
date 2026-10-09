//! Interoperability check of the avatar metadata subset against a real,
//! independent server (spec/development.md, "IRC metadata interoperability").
//! Ignored by default; it needs a disposable local server, for example the
//! pinned Ergo that `scripts/ergo-metadata-interop.sh` builds and runs:
//!
//! ```text
//! CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 \
//!   cargo test --locked -p cayenchat-irc-core --test metadata_interop -- --ignored --nocapture
//! ```
//!
//! CayenChat's own connection code is the client under test; a minimal raw
//! client plays the other users. Nicknames and the channel carry a per-run
//! suffix, and every URL points at example.com: nothing is fetched.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cayenchat_irc_core::{
    AvatarRequestFailure, Connection, ConnectionConfig, Event, Ircv3Options, WireDirection,
};

const TIMEOUT: Duration = Duration::from_secs(20);

fn server() -> Option<(String, u16)> {
    let address = std::env::var("CAYENCHAT_INTEROP_IRC").ok()?;
    let (host, port) = address.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

/// Another user, speaking raw IRC with `batch` and `draft/metadata-2`.
struct Peer {
    name: String,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Peer {
    fn connect(host: &str, port: u16, nick: &str) -> Self {
        let writer = TcpStream::connect((host, port)).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut peer = Self {
            name: nick.to_owned(),
            reader: BufReader::new(writer.try_clone().unwrap()),
            writer,
        };
        for line in [
            "CAP LS 302".to_owned(),
            format!("NICK {nick}"),
            format!("USER {nick} 0 * :{nick}"),
            "CAP REQ :batch draft/metadata-2".into(),
            "CAP END".into(),
        ] {
            peer.send(&line);
        }
        peer.expect(|line| line.contains(" 376 ") || line.contains(" 422 "));
        peer.send("METADATA * SUB avatar");
        peer.expect(|line| line.contains(" 770 "));
        peer
    }

    fn send(&mut self, line: &str) {
        println!("{} >> {line}", self.name);
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .unwrap();
    }

    /// Reads until a line matches, answering PINGs.
    fn expect(&mut self, matches: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "{}: expected line did not arrive",
                self.name
            );
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => panic!("{}: server closed the connection", self.name),
                Ok(_) => {}
                Err(_) => continue,
            }
            let line = line.trim_end().to_owned();
            println!("{} << {line}", self.name);
            if let Some(token) = line.strip_prefix("PING ") {
                self.send(&format!("PONG {token}"));
                continue;
            }
            if matches(&line) {
                return line;
            }
        }
    }

    /// Reads for `period`, returning every line.
    fn drain(&mut self, period: Duration) -> Vec<String> {
        let deadline = Instant::now() + period;
        let mut lines = Vec::new();
        while Instant::now() < deadline {
            let mut line = String::new();
            if let Ok(n) = self.reader.read_line(&mut line)
                && n > 0
            {
                let line = line.trim_end().to_owned();
                println!("{} << {line}", self.name);
                if let Some(token) = line.strip_prefix("PING ") {
                    self.send(&format!("PONG {token}"));
                }
                lines.push(line);
            }
        }
        lines
    }
}

/// CayenChat's connection and everything it reported.
struct Client {
    connection: Connection,
    events: Vec<Event>,
}

impl Client {
    fn connect(host: &str, port: u16, nick: &str, channel: &str) -> Self {
        let mut config = ConnectionConfig::tls(host.into(), nick.into(), vec![channel.into()]);
        config.port = port;
        config.use_tls = false;
        config.ircv3 = Ircv3Options {
            batch: true,
            metadata: true,
            ..Ircv3Options::default()
        };
        let connection = Connection::connect(config).unwrap();
        // The checks read IRC lines sent after registration.
        connection.set_transcript(true);
        Self {
            connection,
            events: Vec::new(),
        }
    }

    /// Waits for an event, keeping every event seen.
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
            self.events.push(event.clone());
            if matches(&event) {
                return event;
            }
        }
    }

    /// Waits for the answer to our request `request`.
    fn own(&mut self, request: u64) -> Event {
        self.wait("own avatar answer", |event| {
            matches!(event, Event::OwnAvatar { request: Some(r), .. } | Event::OwnAvatarFailed { request: r, .. } if *r == request)
        })
    }

    fn sent(&self, prefix: &str) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, Event::Wire { direction: WireDirection::Sent, line, .. } if line.starts_with(prefix)))
            .count()
    }
}

fn avatar(nickname: &str, url: Option<&str>) -> Event {
    Event::UserAvatar {
        nickname: nickname.into(),
        url: url.map(str::to_owned),
    }
}

#[test]
#[ignore = "needs a disposable local metadata server (CAYENCHAT_INTEROP_IRC)"]
fn avatar_metadata_against_a_real_server() {
    let Some((host, port)) = server() else {
        panic!("set CAYENCHAT_INTEROP_IRC=host:port");
    };
    let run = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 100_000;
    let (me, bob, carol, dave) = (
        format!("cc{run}"),
        format!("bob{run}"),
        format!("carol{run}"),
        format!("dave{run}"),
    );
    let channel = format!("#cc-interop-{run}");
    let url = |name: &str| format!("https://example.com/{name}.png");

    // Bob is there first, with an avatar.
    let mut peer = Peer::connect(&host, port, &bob);
    peer.send(&format!("METADATA * SET avatar :{}", url("bob")));
    peer.expect(|line| line.contains(" 761 "));
    peer.send(&format!("JOIN {channel}"));
    peer.expect(|line| line.contains(" 366 "));

    // Negotiation, subscription, our own state and initial delivery.
    let mut client = Client::connect(&host, port, &me, &channel);
    client.wait("MetadataReady", |event| *event == Event::MetadataReady);
    assert_eq!(
        client.wait("own state", |event| matches!(
            event,
            Event::OwnAvatar { .. }
        )),
        Event::OwnAvatar {
            url: None,
            request: None
        }
    );
    client.wait("bob's avatar on join", |event| {
        *event == avatar(&bob, Some(&url("bob")))
    });
    peer.expect(|line| line.contains(&format!(":{me}!")) && line.contains("JOIN"));

    // Publish, confirmed by the server and seen by the other user.
    client
        .connection
        .set_own_avatar(1, Some(&url("me")))
        .unwrap();
    assert_eq!(
        client.own(1),
        Event::OwnAvatar {
            url: Some(url("me")),
            request: Some(1)
        }
    );
    peer.expect(|line| line.contains(&format!(" {me} avatar ")) && line.contains(&url("me")));

    // Change it.
    client
        .connection
        .set_own_avatar(2, Some(&url("me2")))
        .unwrap();
    assert!(matches!(
        client.own(2),
        Event::OwnAvatar {
            request: Some(2),
            ..
        }
    ));
    peer.expect(|line| line.contains(&url("me2")));

    // Bob changes his; we follow.
    peer.send(&format!("METADATA * SET avatar :{}", url("bob2")));
    client.wait("bob's change", |event| {
        *event == avatar(&bob, Some(&url("bob2")))
    });

    // Carol joins after us, with an avatar set beforehand: the server does
    // not announce it, so CayenChat looks her up once.
    let mut later = Peer::connect(&host, port, &carol);
    later.send(&format!("METADATA * SET avatar :{}", url("carol")));
    later.expect(|line| line.contains(" 761 "));
    later.send(&format!("JOIN {channel}"));
    client.wait("carol's avatar after joining", |event| {
        *event == avatar(&carol, Some(&url("carol")))
    });
    assert_eq!(client.sent(&format!("METADATA {carol} GET avatar")), 1);

    // Nickname change moves it; quitting ends it.
    later.send(&format!("NICK {dave}"));
    client.wait("move", |event| {
        *event
            == Event::AvatarMoved {
                from: carol.clone(),
                to: dave.clone(),
            }
    });
    later.send("QUIT :bye");
    client.wait("dave quit", |event| *event == avatar(&dave, None));

    // Someone else takes the name, without an avatar: looked up, none.
    let mut reuse = Peer::connect(&host, port, &dave);
    reuse.send(&format!("JOIN {channel}"));
    client.wait("lookup of the new dave", |event| {
        matches!(event, Event::Wire { direction: WireDirection::Sent, line, .. } if *line == format!("METADATA {dave} GET avatar"))
    });
    client.wait("answer for the new dave", |event| {
        matches!(event, Event::Wire { direction: WireDirection::Received, line, .. } if line.contains(&format!(" 766 {me} {dave} avatar")))
    });
    thread::sleep(Duration::from_millis(500));
    while let Some(event) = client.connection.try_recv() {
        client.events.push(event);
    }
    let reused: Vec<_> = client
        .events
        .iter()
        .filter(|event| matches!(event, Event::UserAvatar { nickname, url: Some(_) } if *nickname == dave))
        .collect();
    assert!(reused.is_empty(), "new occupant got an avatar: {reused:?}");

    // Remove ours: only the avatar key, confirmed, seen by Bob.
    client.connection.set_own_avatar(3, None).unwrap();
    assert_eq!(
        client.own(3),
        Event::OwnAvatar {
            url: None,
            request: Some(3)
        }
    );
    peer.expect(|line| line.contains(" 766 ") && line.contains(&format!(" {me} avatar")));

    // Rejection: Ergo limits key + value to 350 bytes.
    let long = format!("https://example.com/{}.png", "a".repeat(340));
    client.connection.set_own_avatar(4, Some(&long)).unwrap();
    let rejected = client.own(4);
    println!("long value: {rejected:?}");
    assert!(matches!(
        rejected,
        Event::OwnAvatarFailed {
            failure: AvatarRequestFailure::Rejected { .. },
            ..
        }
    ));

    // Rate limit: Ergo allows 10 changes per 2 minutes by default.
    let mut limited = None;
    for request in 5..20 {
        client
            .connection
            .set_own_avatar(request, Some(&url(&format!("r{request}"))))
            .unwrap();
        if let Event::OwnAvatarFailed {
            failure: failure @ AvatarRequestFailure::RateLimited { .. },
            ..
        } = client.own(request)
        {
            limited = Some(failure);
            break;
        }
    }
    println!("rate limit: {limited:?}");
    assert!(limited.is_some(), "no RATE_LIMITED within 15 changes");

    // Reconnect: our draft is not republished; the server reports what it
    // kept, and Bob's avatar arrives again.
    let published = client.sent("METADATA * SET");
    client.connection.disconnect().unwrap();
    client.wait("disconnect", |event| matches!(event, Event::Closed(_)));
    let mut again = Client::connect(&host, port, &me, &channel);
    again.wait("MetadataReady", |event| *event == Event::MetadataReady);
    let kept = again.wait("own state", |event| {
        matches!(event, Event::OwnAvatar { .. })
    });
    println!("after reconnect: {kept:?} (published {published} SETs before)");
    again.wait("bob again", |event| {
        *event == avatar(&bob, Some(&url("bob2")))
    });
    assert_eq!(again.sent("METADATA * SET"), 0, "nothing republished");
    peer.drain(Duration::from_millis(300));
    reuse.send("QUIT :bye");
    peer.send("QUIT :bye");
    again.connection.disconnect().unwrap();
}

/// Request counts for a burst of joiners and repeated updates: lookups stay
/// one per new user, spaced out, and updates cause no requests at all.
#[test]
#[ignore = "needs a disposable local metadata server (CAYENCHAT_INTEROP_IRC)"]
fn join_bursts_and_repeated_updates_stay_bounded() {
    let Some((host, port)) = server() else {
        panic!("set CAYENCHAT_INTEROP_IRC=host:port");
    };
    let run = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 100_000;
    let channel = format!("#cc-burst-{run}");
    let mut client = Client::connect(&host, port, &format!("cb{run}"), &channel);
    client.wait("MetadataReady", |event| *event == Event::MetadataReady);
    client.wait(
        "joined",
        |event| matches!(event, Event::Names { users, .. } if !users.is_empty()),
    );

    const JOINERS: usize = 24;
    let mut peers: Vec<Peer> = (0..JOINERS)
        .map(|n| {
            let mut peer = Peer::connect(&host, port, &format!("j{n}x{run}"));
            if n % 2 == 0 {
                peer.send(&format!(
                    "METADATA * SET avatar :https://example.com/j{n}.png"
                ));
            }
            peer
        })
        .collect();
    let started = Instant::now();
    for peer in &mut peers {
        peer.send(&format!("JOIN {channel}"));
    }
    // Every even joiner's avatar arrives through a lookup, in any order.
    let joined_with_avatar = |events: &[Event]| {
        events
            .iter()
            .filter(|event| matches!(event, Event::UserAvatar { nickname, url: Some(_) } if nickname.starts_with('j')))
            .count()
    };
    while joined_with_avatar(&client.events) < JOINERS / 2 {
        client.wait("joiner avatars", |event| {
            matches!(event, Event::UserAvatar { .. })
        });
    }
    let elapsed = started.elapsed();
    let lookups = client.sent("METADATA j");
    println!("{JOINERS} joiners: {lookups} lookups in {elapsed:?}");
    assert_eq!(lookups, JOINERS, "one lookup per joiner, no repeats");
    // At most two per second after the 2 s pause.
    assert!(elapsed >= Duration::from_millis(2000 + 500 * (JOINERS as u64 / 2 - 1)));

    // Repeated updates by one member: followed, and no request sent.
    let before = client.sent("METADATA");
    for version in 0..8 {
        peers[0].send(&format!(
            "METADATA * SET avatar :https://example.com/v{version}.png"
        ));
    }
    let nick = format!("j0x{run}");
    client.wait("last update", |event| {
        matches!(event, Event::UserAvatar { nickname, url: Some(url) } if *nickname == nick && url.ends_with("v7.png"))
    });
    assert_eq!(client.sent("METADATA"), before, "updates cost no requests");
    // A NAMES refresh asks nothing either.
    client
        .connection
        .send_command(&format!("/names {channel}"), None)
        .ok();
    thread::sleep(Duration::from_secs(3));
    while let Some(event) = client.connection.try_recv() {
        client.events.push(event);
    }
    assert_eq!(client.sent("METADATA"), before, "NAMES triggers no lookups");
    for peer in &mut peers {
        peer.send("QUIT :bye");
    }
    client.connection.disconnect().unwrap();
}
