// SPDX-License-Identifier: MPL-2.0
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
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cayenchat_irc_core::{Event, WireDirection};

mod common;
use common::{Raw, TIMEOUT, server};

/// The rate limit's window, plus a margin.
const WINDOW: Duration = Duration::from_secs(11);

/// Another user, speaking raw IRC.
type Peer = Raw;

impl Peer {
    fn connect(host: &str, port: u16, nick: &str, channel: &str) -> Self {
        let mut peer = Raw::open(host, port, nick, TIMEOUT);
        peer.send(&format!("NICK {nick}"));
        peer.send(&format!("USER {nick} 0 * :{nick}"));
        peer.expect(|line| line.contains(" 376 ") || line.contains(" 422 "));
        peer.send(&format!("JOIN {channel}"));
        peer.expect(|line| line.contains(" 366 "));
        peer
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
type Client = common::Client;

impl Client {
    fn connect(host: &str, port: u16, nick: &str, channel: &str) -> Self {
        Self::start(Self::config(host, port, nick, vec![channel.into()]))
    }

    fn server_line(&mut self, expected: &str) {
        self.wait(
            expected,
            |event| matches!(event, Event::CtcpLine(line) if line == expected),
        );
    }

    fn lines(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::CtcpLine(line) => Some(line.as_str()),
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
        info.ends_with(
            ":\u{1}CLIENTINFO ACTION CLIENTINFO ERRMSG FINGER PING SOURCE TIME USERINFO VERSION\u{1}"
        ),
        "{info:?}"
    );
    // Channel requests are shown but not answered.
    dave.send(&format!("PRIVMSG {channel} :\u{1}VERSION\u{1}"));
    client.server_line(&format!(
        "CTCP VERSION request from dave{run} to {channel} (not answered)"
    ));
    for peer in [&mut bob, &mut carol, &mut dave] {
        assert!(peer.notices(Duration::from_secs(1)).is_empty());
    }
    // The other fixed answers, ERRMSG for a unknown query, and no answer to
    // DCC (never interpreted) or to a reply, so ERRMSG cannot loop.
    thread::sleep(WINDOW);
    for (peer, request) in [
        (0, "USERINFO"),
        (0, "FINGER"),
        (1, "SOURCE"),
        (1, "ERRMSG PING :x"),
        (2, "FROBNICATE"),
    ] {
        [&mut dave, &mut bob, &mut carol][peer].send(&format!("PRIVMSG {me} :\u{1}{request}\u{1}"));
    }
    let mut answers = Vec::new();
    for peer in [&mut bob, &mut carol, &mut dave] {
        answers.extend(peer.notices(Duration::from_secs(2)));
    }
    let ends = |text: &str| answers.iter().any(|answer| answer.ends_with(text));
    assert!(ends("\u{1}USERINFO CayenChat user\u{1}"), "{answers:?}");
    assert!(ends("\u{1}FINGER CayenChat user\u{1}"), "{answers:?}");
    assert!(
        ends("\u{1}SOURCE https://github.com/koshian/cayenchat\u{1}"),
        "{answers:?}"
    );
    assert!(ends("\u{1}ERRMSG PING :x :No error\u{1}"), "{answers:?}");
    assert!(
        ends("\u{1}ERRMSG FROBNICATE :Unknown query\u{1}"),
        "{answers:?}"
    );
    assert_eq!(answers.len(), 5, "{answers:?}");
    thread::sleep(WINDOW);
    dave.send(&format!("PRIVMSG {me} :\u{1}DCC SEND f.txt 1 2 3\u{1}"));
    dave.send(&format!(
        "NOTICE {me} :\u{1}ERRMSG FROBNICATE :Unknown query\u{1}"
    ));
    assert!(dave.notices(Duration::from_secs(1)).is_empty());

    // Our own requests: /ctcp shows the reply in the server log, and /ping
    // reports a round trip only for the reply that echoes its token.
    thread::sleep(WINDOW);
    let bob_nick = format!("bob{run}");
    client
        .connection
        .send_command(&format!("/ctcp {bob_nick} VERSION"), None)
        .unwrap();
    let asked = bob.expect(|line| line.contains("PRIVMSG") && line.contains("\u{1}VERSION"));
    assert!(
        asked.ends_with(&format!("PRIVMSG {bob_nick} :\u{1}VERSION\u{1}")),
        "{asked:?}"
    );
    bob.send(&format!("NOTICE {me} :\u{1}VERSION fakeclient 1.0\u{1}"));
    client.server_line(&format!(
        "CTCP VERSION reply from {bob_nick}: fakeclient 1.0"
    ));
    client
        .connection
        .send_command(&format!("/ping {bob_nick}"), None)
        .unwrap();
    let asked = bob.expect(|line| line.contains("PRIVMSG") && line.contains("\u{1}PING "));
    let token = asked
        .rsplit_once("\u{1}PING ")
        .and_then(|(_, rest)| rest.strip_suffix('\u{1}'))
        .unwrap()
        .to_owned();
    // Another user echoing the token, and a wrong token, are no measurement.
    carol.send(&format!("NOTICE {me} :\u{1}PING {token}\u{1}"));
    bob.send(&format!("NOTICE {me} :\u{1}PING 0000\u{1}"));
    bob.send(&format!("NOTICE {me} :\u{1}PING {token}\u{1}"));
    let round_trip = client.wait(
        "ping round trip",
        |event| matches!(event, Event::CtcpLine(line) if line.contains(" ms")),
    );
    println!("{round_trip:?}");
    let Event::CtcpLine(line) = round_trip else {
        unreachable!()
    };
    assert!(
        line.starts_with(&format!("CTCP PING reply from {bob_nick} on {host}: ")),
        "{line}"
    );
    assert_eq!(
        client
            .events
            .iter()
            .filter(|event| matches!(event, Event::CtcpLine(line) if line.contains(" ms")))
            .count(),
        1
    );
    // A request that would inject a command is refused before the wire.
    assert!(
        client
            .connection
            .send_command(&format!("/ctcp {bob_nick} PING x\r\nQUIT"), None)
            .is_err()
    );

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
            Event::ServerLine(line) | Event::CtcpLine(line) => {
                assert!(!line.contains('\u{1}'), "{line:?}")
            }
            _ => {}
        }
    }
    client.connection.disconnect().ok();
}
