//! Interoperability check of recent channel history (`draft/chathistory`)
//! against a real, independent server (spec/development.md, "IRC
//! interoperability with Ergo"). Ignored by default; it needs a disposable
//! local server, for example the pinned Ergo that
//! `scripts/ergo-metadata-interop.sh` builds and runs:
//!
//! ```text
//! CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 \
//!   cargo test --locked -p cayenchat-irc-core --test chathistory_interop -- --ignored --nocapture
//! ```
//!
//! A minimal raw client speaks first; CayenChat's own connection code then
//! joins and asks for history. Nicknames and channels carry a per-run suffix.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cayenchat_irc_core::{
    Connection, ConnectionConfig, Event, HISTORY_LIMIT, HistoryMessage, Ircv3Options,
    MessageReference, OlderHistoryStatus, WireDirection,
};

const TIMEOUT: Duration = Duration::from_secs(20);
/// Ergo's default fakelag lets a client send about two lines a second after
/// a short burst, so filling a channel takes a while.
const FILL_TIMEOUT: Duration = Duration::from_secs(60);

fn server() -> Option<(String, u16)> {
    let address = std::env::var("CAYENCHAT_INTEROP_IRC").ok()?;
    let (host, port) = address.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

fn suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{:x}", nanos % 0xffffff)
}

/// Another user speaking raw IRC without any capability.
struct Peer {
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
            reader: BufReader::new(writer.try_clone().unwrap()),
            writer,
        };
        peer.send(&format!("NICK {nick}"));
        peer.send(&format!("USER {nick} 0 * :{nick}"));
        peer.expect(|line| line.contains(" 376 ") || line.contains(" 422 "));
        peer
    }

    fn send(&mut self, line: &str) {
        println!("peer >> {line}");
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .unwrap();
    }

    fn expect(&mut self, matches: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + FILL_TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "peer: expected line did not arrive"
            );
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => panic!("peer: server closed the connection"),
                Ok(_) => {}
                Err(_) => continue,
            }
            let line = line.trim_end().to_owned();
            println!("peer << {line}");
            if let Some(token) = line.strip_prefix("PING ") {
                self.send(&format!("PONG {token}"));
                continue;
            }
            if matches(&line) {
                return line;
            }
        }
    }
}

struct Client {
    connection: Connection,
    events: Vec<Event>,
}

impl Client {
    fn connect(host: &str, port: u16, nick: &str, channels: Vec<String>) -> Self {
        let mut config = ConnectionConfig::tls(host.into(), nick.into(), channels);
        config.port = port;
        config.use_tls = false;
        config.ircv3 = Ircv3Options {
            chathistory: true,
            ..Ircv3Options::default()
        };
        Self {
            connection: Connection::connect(config).unwrap(),
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
                Event::Diagnostic { message, .. } => println!("cayenchat note {message}"),
                other => println!("cayenchat event {other:?}"),
            }
            self.events.push(event.clone());
            if matches(&event) {
                return event;
            }
        }
    }

    fn older_page(
        &mut self,
        channel: &str,
        request: u64,
        reference: MessageReference,
    ) -> (Vec<HistoryMessage>, OlderHistoryStatus) {
        self.connection
            .request_older_history(channel, request, reference, HISTORY_LIMIT)
            .unwrap();
        match self.wait(
            "older page",
            |event| matches!(event, Event::OlderChannelHistory { request: r, .. } if *r == request),
        ) {
            Event::OlderChannelHistory {
                messages, status, ..
            } => (messages, status),
            _ => unreachable!(),
        }
    }

    fn history(&mut self, channel: &str) -> Vec<HistoryMessage> {
        match self.wait("history reply", |event| {
            matches!(event, Event::ChannelHistory { channel: c, .. } if c.eq_ignore_ascii_case(channel))
        }) {
            Event::ChannelHistory { messages, .. } => messages,
            _ => unreachable!(),
        }
    }
}

#[test]
#[ignore = "needs a disposable local IRC server with chathistory (CAYENCHAT_INTEROP_IRC)"]
fn recent_channel_history_against_a_real_server() {
    let (host, port) = server().expect("set CAYENCHAT_INTEROP_IRC=host:port");
    let run = suffix();
    let busy = format!("#hist{run}");
    let quiet = format!("#quiet{run}");
    let mut peer = Peer::connect(&host, port, &format!("pe{run}"));
    peer.send(&format!("JOIN {busy}"));
    peer.expect(|line| line.contains(" 366 "));
    peer.send(&format!("JOIN {quiet}"));
    peer.expect(|line| line.contains(" 366 "));
    // More lines than CayenChat asks for.
    let total = HISTORY_LIMIT + 10;
    for index in 0..total {
        peer.send(&format!("PRIVMSG {busy} :line {index}"));
    }
    // Let the server store them before the client joins.
    peer.send(&format!("PING :stored{run}"));
    peer.expect(|line| line.contains(&format!("stored{run}")));

    let mut client = Client::connect(
        &host,
        port,
        &format!("cc{run}"),
        vec![busy.clone(), quiet.clone()],
    );
    client.wait("chathistory negotiated", |event| {
        matches!(event, Event::Diagnostic { message, .. } if message.contains("draft/chathistory enabled"))
    });
    let busy_history = client.history(&busy);
    assert!(
        busy_history.len() <= HISTORY_LIMIT,
        "{}",
        busy_history.len()
    );
    // Without event-playback Ergo 2.19 reports events (our own JOIN) as
    // PRIVMSGs from HistServ; they count against the limit and are shown
    // like any other history line.
    let (events, lines): (Vec<_>, Vec<_>) =
        busy_history.iter().partition(|m| m.sender == "HistServ");
    assert!(
        events.iter().all(|m| m.text.contains("joined")),
        "{events:?}"
    );
    let texts: Vec<_> = lines.iter().map(|m| m.text.clone()).collect();
    assert!(
        texts.len() + events.len() == HISTORY_LIMIT,
        "{}",
        texts.len()
    );
    let expected: Vec<_> = (total - texts.len()..total)
        .map(|index| format!("line {index}"))
        .collect();
    assert_eq!(texts, expected);
    assert!(
        busy_history
            .iter()
            .all(|m| m.msgid.is_some() && m.server_time.is_some())
    );
    // Nobody spoke: only the joins that Ergo reports through HistServ.
    let quiet_history = client.history(&quiet);
    assert!(
        quiet_history.iter().all(|m| m.sender == "HistServ"),
        "{quiet_history:?}"
    );
    // Requests went out one at a time, with our limit.
    let requests: Vec<_> = client
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Wire {
                direction: WireDirection::Sent,
                line,
                ..
            } if line.starts_with("CHATHISTORY") => Some(line.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        requests,
        [
            format!("CHATHISTORY LATEST {busy} * {HISTORY_LIMIT}"),
            format!("CHATHISTORY LATEST {quiet} * {HISTORY_LIMIT}"),
        ]
    );
    // History never arrived as live messages; a new line does.
    peer.send(&format!("PRIVMSG {busy} :live after history"));
    client.wait("live line", |event| {
        matches!(event, Event::ChannelMessage { text, replayed: false, .. } if text == "live after history")
    });
    let live = client
        .events
        .iter()
        .filter(|event| matches!(event, Event::ChannelMessage { .. }))
        .count();
    assert_eq!(live, 1);
}

#[test]
#[ignore = "needs a disposable local IRC server with chathistory (CAYENCHAT_INTEROP_IRC)"]
fn older_channel_history_pages_against_a_real_server() {
    let (host, port) = server().expect("set CAYENCHAT_INTEROP_IRC=host:port");
    let run = suffix();
    let channel = format!("#page{run}");
    let mut peer = Peer::connect(&host, port, &format!("pp{run}"));
    peer.send(&format!("JOIN {channel}"));
    peer.expect(|line| line.contains(" 366 "));
    // More than one page, fewer than two.
    let total = HISTORY_LIMIT + 20;
    for index in 0..total {
        peer.send(&format!("PRIVMSG {channel} :line {index}"));
    }
    peer.send(&format!("PING :stored{run}"));
    peer.expect(|line| line.contains(&format!("stored{run}")));

    let mut client = Client::connect(&host, port, &format!("cp{run}"), vec![channel.clone()]);
    client.wait("history available", |event| {
        matches!(event, Event::HistoryAvailable(true))
    });
    let recent = client.history(&channel);
    let oldest = recent
        .iter()
        .min_by_key(|m| m.server_time)
        .expect("recent history")
        .clone();

    // One page before the oldest line, by msgid.
    let (page, status) = client.older_page(
        &channel,
        1,
        MessageReference {
            msgid: oldest.msgid.clone(),
            time: oldest.server_time,
        },
    );
    println!("first older page: {} lines, {status:?}", page.len());
    let texts: Vec<_> = page
        .iter()
        .filter(|m| m.sender != "HistServ")
        .map(|m| m.text.clone())
        .collect();
    let first_recent = recent
        .iter()
        .find(|m| m.sender != "HistServ")
        .map(|m| m.text.clone())
        .unwrap();
    let first_recent: usize = first_recent.trim_start_matches("line ").parse().unwrap();
    let expected: Vec<_> = (0..first_recent).map(|i| format!("line {i}")).collect();
    assert_eq!(
        texts, expected,
        "the page ends right before the recent lines"
    );
    assert!(page.iter().all(|m| m.server_time < oldest.server_time));
    assert!(page.len() <= HISTORY_LIMIT);

    // Before the page's own oldest line, by timestamp only: nothing is left.
    let oldest = page.iter().min_by_key(|m| m.server_time).unwrap().clone();
    let (rest, status) = client.older_page(
        &channel,
        2,
        MessageReference {
            msgid: None,
            time: oldest.server_time,
        },
    );
    println!("second older page: {} lines, {status:?}", rest.len());
    assert!(rest.iter().all(|m| m.sender == "HistServ"), "{rest:?}");
    let requests: Vec<_> = client
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Wire {
                direction: WireDirection::Sent,
                line,
                ..
            } if line.starts_with("CHATHISTORY BEFORE") => Some(line.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains(" msgid="), "{requests:?}");
    assert!(requests[1].contains(" timestamp="), "{requests:?}");
    // No history line arrived as live traffic.
    assert!(
        !client
            .events
            .iter()
            .any(|event| matches!(event, Event::ChannelMessage { .. }))
    );
}
