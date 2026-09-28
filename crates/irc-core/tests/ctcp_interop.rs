//! CTCP answers (D029) through a real, independent server (spec/development.md,
//! "CTCP answers"). Ignored by default; it needs a disposable local server,
//! for example the pinned Ergo that `scripts/ergo-metadata-interop.sh`
//! builds and runs:
//!
//! ```text
//! CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 \
//!   cargo test --locked -p cayenchat-irc-core --test ctcp_interop -- --ignored --nocapture
//! ```
//!
//! CayenChat's own connection code is the client under test; minimal raw
//! clients play the other users. Nicknames and the channel carry a per-run
//! suffix.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cayenchat_irc_core::{Connection, ConnectionConfig, Event, WireDirection};

const TIMEOUT: Duration = Duration::from_secs(20);
/// The rate limit's window, plus a margin.
const WINDOW: Duration = Duration::from_secs(11);

fn server() -> Option<(String, u16)> {
    let address = std::env::var("CAYENCHAT_INTEROP_IRC").ok()?;
    let (host, port) = address.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

/// Another user, speaking raw IRC.
struct Peer {
    name: String,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Peer {
    fn connect(host: &str, port: u16, nick: &str, channel: &str) -> Self {
        let writer = TcpStream::connect((host, port)).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut peer = Self {
            name: nick.to_owned(),
            reader: BufReader::new(writer.try_clone().unwrap()),
            writer,
        };
        peer.send(&format!("NICK {nick}"));
        peer.send(&format!("USER {nick} 0 * :{nick}"));
        peer.expect(|line| line.contains(" 376 ") || line.contains(" 422 "));
        peer.send(&format!("JOIN {channel}"));
        peer.expect(|line| line.contains(" 366 "));
        peer
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

    /// Reads until a line matches, answering PINGs.
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

    /// CTCP NOTICEs received during `period`.
    fn notices(&mut self, period: Duration) -> Vec<String> {
        let deadline = Instant::now() + period;
        let mut notices = Vec::new();
        while Instant::now() < deadline {
            if let Some(line) = self.read()
                && let Some((_, text)) = line.split_once(&format!(" NOTICE {} :", self.name))
                && text.starts_with('\u{1}')
            {
                notices.push(text.to_owned());
            }
        }
        notices
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
        Self {
            connection: Connection::connect(config).unwrap(),
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

    fn server_line(&mut self, expected: &str) {
        self.wait(
            expected,
            |event| matches!(event, Event::ServerLine(line) if line == expected),
        );
    }

    fn lines(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::ServerLine(line) if line.contains("CTCP") => Some(line.as_str()),
                _ => None,
            })
            .collect()
    }
}

#[test]
#[ignore = "needs a disposable local IRC server (CAYENCHAT_INTEROP_IRC)"]
fn ctcp_answers_through_a_real_server() {
    let Some((host, port)) = server() else {
        panic!("set CAYENCHAT_INTEROP_IRC=host:port");
    };
    let run = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 100_000;
    let me = format!("cc{run}");
    let channel = format!("#ctcp{run}");
    let mut client = Client::connect(&host, port, &me, &channel);
    client.wait("join", |event| matches!(event, Event::Joined { .. }));
    // Ergo's default channel modes include +C (no CTCP); allow it here.
    client
        .connection
        .send_command(&format!("/MODE {channel} -C"), None)
        .unwrap();
    client.wait("MODE -C", |event| {
        matches!(event, Event::Wire { direction: WireDirection::Received, line, .. }
            if line.contains(" MODE ") && line.contains("-C"))
    });
    let mut bob = Peer::connect(&host, port, &format!("bob{run}"), &channel);
    let mut carol = Peer::connect(&host, port, &format!("carol{run}"), &channel);
    let mut dave = Peer::connect(&host, port, &format!("dave{run}"), &channel);

    // Private requests are answered by NOTICE to the sender.
    bob.send(&format!("PRIVMSG {me} :\u{1}VERSION\u{1}"));
    bob.send(&format!("PRIVMSG {me} :\u{1}PING 1727490000 42\u{1}"));
    let version = bob.expect(|line| line.contains("\u{1}VERSION "));
    assert!(
        version.ends_with(concat!(
            ":\u{1}VERSION CayenChat ",
            env!("CARGO_PKG_VERSION"),
            "\u{1}"
        )),
        "{version:?}"
    );
    let ping = bob.expect(|line| line.contains("\u{1}PING"));
    assert!(ping.ends_with(":\u{1}PING 1727490000 42\u{1}"), "{ping:?}");
    carol.send(&format!("PRIVMSG {me} :\u{1}TIME\u{1}"));
    carol.send(&format!("PRIVMSG {me} :\u{1}CLIENTINFO\u{1}"));
    let time = carol.expect(|line| line.contains("\u{1}TIME "));
    assert!(time.len() > format!(":{me}!").len() + 30, "{time:?}");
    let info = carol.expect(|line| line.contains("\u{1}CLIENTINFO "));
    assert!(
        info.ends_with(":\u{1}CLIENTINFO ACTION CLIENTINFO PING TIME VERSION\u{1}"),
        "{info:?}"
    );
    // Channel requests and USERINFO are shown but not answered.
    dave.send(&format!("PRIVMSG {channel} :\u{1}VERSION\u{1}"));
    client.server_line(&format!(
        "CTCP VERSION request from dave{run} to {channel} (not answered)"
    ));
    for peer in [&mut bob, &mut carol, &mut dave] {
        assert!(peer.notices(Duration::from_secs(1)).is_empty());
    }
    thread::sleep(WINDOW);
    dave.send(&format!("PRIVMSG {me} :\u{1}USERINFO\u{1}"));
    client.server_line(&format!(
        "CTCP USERINFO request from dave{run} (not answered)"
    ));
    assert!(dave.notices(Duration::from_secs(1)).is_empty());

    // A flood from several users: at most five answers, two per user, one
    // line saying the rest are ignored, and the connection stays up.
    thread::sleep(WINDOW);
    for index in 0..6 {
        for peer in [&mut bob, &mut carol, &mut dave] {
            peer.send(&format!("PRIVMSG {me} :\u{1}PING {index}\u{1}"));
        }
    }
    let answers: Vec<usize> = [&mut bob, &mut carol, &mut dave]
        .into_iter()
        .map(|peer| peer.notices(Duration::from_secs(4)).len())
        .collect();
    println!("answers per peer: {answers:?}");
    assert!(answers.iter().all(|count| *count <= 2), "{answers:?}");
    assert_eq!(answers.iter().sum::<usize>(), 5, "{answers:?}");
    bob.send(&format!("PRIVMSG {channel} :still here"));
    client.wait(
        "chat after the flood",
        |event| matches!(event, Event::ChannelMessage { text, .. } if text == "still here"),
    );
    let lines = client.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("Too many CTCP requests"))
            .count(),
        1,
        "{lines:#?}"
    );
    // No CTCP became a chat row or a raw line.
    for event in &client.events {
        match event {
            Event::ChannelMessage { text, .. } | Event::PrivateMessage { text, .. } => {
                assert!(!text.contains('\u{1}'), "{text:?}")
            }
            Event::ServerLine(line) => assert!(!line.contains('\u{1}'), "{line:?}"),
            _ => {}
        }
    }
    client.connection.disconnect().ok();
}
