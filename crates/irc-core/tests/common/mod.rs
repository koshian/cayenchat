// SPDX-License-Identifier: MPL-2.0
//! Harness shared by the interop tests: where the server is, a raw IRC
//! socket that plays another user, and CayenChat's own connection with the
//! events it reported. Each test file keeps its own scripts and checks.

#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    thread,
    time::{Duration, Instant},
};

use cayenchat_irc_core::{Connection, ConnectionConfig, Event, WireDirection};

/// How long `Client::wait` and a default `Raw::expect` wait.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// The server under test, from `CAYENCHAT_INTEROP_IRC` (`host:port`).
pub fn server() -> Option<(String, u16)> {
    let address = std::env::var("CAYENCHAT_INTEROP_IRC").ok()?;
    let (host, port) = address.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

/// Another user, speaking raw IRC.
pub struct Raw {
    pub name: String,
    pub reader: BufReader<TcpStream>,
    pub writer: TcpStream,
    /// How long `expect` waits for a line.
    pub timeout: Duration,
}

impl Raw {
    /// Opens the socket; registering and joining is up to the caller.
    pub fn open(host: &str, port: u16, name: &str, timeout: Duration) -> Self {
        let writer = TcpStream::connect((host, port)).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        Self {
            name: name.to_owned(),
            reader: BufReader::new(writer.try_clone().unwrap()),
            writer,
            timeout,
        }
    }

    pub fn send(&mut self, line: &str) {
        println!("{} >> {line}", self.name);
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .unwrap();
    }

    /// One line, if one arrives within the socket's read timeout.
    /// Answers PINGs.
    pub fn read(&mut self) -> Option<String> {
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
    pub fn expect(&mut self, matches: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + self.timeout;
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
pub struct Client {
    pub connection: Connection,
    pub events: Vec<Event>,
}

impl Client {
    /// A plain-text configuration for the server at `host:port`.
    pub fn config(host: &str, port: u16, nick: &str, channels: Vec<String>) -> ConnectionConfig {
        let mut config = ConnectionConfig::tls(host.into(), nick.into(), channels);
        config.port = port;
        config.use_tls = false;
        config
    }

    pub fn start(config: ConnectionConfig) -> Self {
        let connection = Connection::connect(config).unwrap();
        // The checks read IRC lines sent after registration.
        connection.set_transcript(true);
        Self {
            connection,
            events: Vec::new(),
        }
    }

    /// Waits for an event, keeping every event seen.
    pub fn wait(&mut self, what: &str, matches: impl Fn(&Event) -> bool) -> Event {
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
}
