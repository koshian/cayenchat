// SPDX-License-Identifier: GPL-3.0-only
//! A simulated wake-up against local servers: which connections are checked,
//! what a silent one turns into, and that it reconnects.
use super::{ChatWindow, Worker};
use cayenchat_app::ConnectionStatus;
use cayenchat_irc_core::{Connection, ConnectionConfig, Event};
use cayenchat_model::NetworkId;
use gpui::TestAppContext;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn config(port: u16) -> ConnectionConfig {
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = port;
    config.use_tls = false;
    config
}

/// Accepts one client and registers it; returns the lines it sends next.
fn accept_registered(
    listener: &TcpListener,
) -> (BufReader<std::net::TcpStream>, std::net::TcpStream) {
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "closed");
        if line.starts_with("USER ") {
            break;
        }
    }
    socket
        .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
        .unwrap();
    (reader, socket)
}

fn next_line(reader: &mut BufReader<std::net::TcpStream>, prefix: &str) -> Option<String> {
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return None,
            Ok(_) if line.starts_with(prefix) => return Some(line),
            Ok(_) => {}
        }
    }
}

#[gpui::test]
fn a_wake_checks_only_live_connections_and_reconnects_the_silent_one(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.add_server("irc.example.org");
    settings.add_server("irc.example.net");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let listeners: Vec<_> = (0..3)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let ports: Vec<u16> = listeners
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect();
    let mut listeners = listeners.into_iter();
    let (silent, answering, manual) = (
        listeners.next().unwrap(),
        listeners.next().unwrap(),
        listeners.next().unwrap(),
    );
    // Network 1 never answers the check, then accepts the reconnection.
    let (reconnected, wait_reconnected) = mpsc::channel();
    let server_silent = thread::spawn(move || {
        let (mut reader, _socket) = accept_registered(&silent);
        let ping = next_line(&mut reader, "PING ").expect("a check");
        assert!(ping.starts_with("PING cayenchat-wake-1"), "{ping:?}");
        let (_second, _socket) = accept_registered(&silent);
        reconnected.send(()).unwrap();
    });
    // Network 2 answers it.
    let server_answering = thread::spawn(move || {
        let (mut reader, mut socket) = accept_registered(&answering);
        let ping = next_line(&mut reader, "PING ").expect("a check");
        assert!(ping.starts_with("PING cayenchat-wake-1"), "{ping:?}");
        socket
            .write_all(b":server PONG server :cayenchat-wake-1\r\n")
            .unwrap();
        // Closing ends the test's wait for this server.
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let _ = next_line(&mut reader, "QUIT");
    });
    // Network 3 was disconnected by the user: nothing is sent to it.
    let (pinged, wait_pinged) = mpsc::channel();
    let server_manual = thread::spawn(move || {
        let (mut reader, _socket) = accept_registered(&manual);
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        pinged.send(next_line(&mut reader, "PING ")).unwrap();
    });

    let networks = [NetworkId(1), NetworkId(2), NetworkId(3)];
    chat.update(cx, |chat, cx| {
        for (network, port) in networks.into_iter().zip(&ports) {
            let session = chat.sessions.get_mut(&network).unwrap();
            session.irc = Some(Connection::connect(config(*port)).unwrap());
            session.active_config = Some(config(*port));
        }
    });

    // Feeds the workers' events in as the event pump does.
    let pump = |chat: &mut ChatWindow, cx: &mut gpui::Context<ChatWindow>| {
        let mut seen = Vec::new();
        for network in networks {
            let mut batch = Vec::new();
            while let Some(event) = chat
                .sessions
                .get_mut(&network)
                .and_then(|session| session.irc.as_mut())
                .and_then(Connection::try_recv)
            {
                match event {
                    Event::Disconnected(_) => seen.push((network, false)),
                    Event::Registered { .. } => seen.push((network, true)),
                    _ => {}
                }
                batch.push(event);
            }
            if !batch.is_empty() {
                chat.handle_events(network, batch, Worker::Running, cx);
            }
        }
        seen
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut registered = 0;
    while registered < networks.len() {
        assert!(Instant::now() < deadline, "not registered");
        registered += chat
            .update(cx, |chat, cx| pump(chat, cx))
            .iter()
            .filter(|(_, registered)| *registered)
            .count();
        thread::sleep(Duration::from_millis(20));
    }
    chat.update(cx, |chat, cx| {
        chat.sessions
            .get_mut(&NetworkId(3))
            .unwrap()
            .manual_disconnect = true;
        // A duplicate notice reaches the same checks, not more of them.
        chat.check_connections_after_wake(cx);
        chat.check_connections_after_wake(cx);
    });
    let mut disconnected = Vec::new();
    while disconnected.is_empty() {
        assert!(Instant::now() < deadline, "no disconnect");
        disconnected = chat
            .update(cx, |chat, cx| pump(chat, cx))
            .into_iter()
            .filter(|(_, registered)| !registered)
            .map(|(network, _)| network)
            .collect();
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(disconnected, [NetworkId(1)]);
    chat.update(cx, |chat, cx| {
        assert!(matches!(
            chat.state.status(NetworkId(1)),
            Some(ConnectionStatus::Disconnected(_))
        ));
        assert!(chat.sessions[&NetworkId(1)].retry_pending);
        assert_eq!(
            chat.state.status(NetworkId(2)),
            Some(&ConnectionStatus::Registered)
        );
        assert!(!chat.sessions[&NetworkId(2)].retry_pending);
        assert!(!chat.sessions[&NetworkId(3)].retry_pending);
        // The retry timer fires: the lost network connects again.
        let generation = chat.sessions[&NetworkId(1)].generation;
        chat.start_reconnect(NetworkId(1), cx);
        assert_eq!(chat.sessions[&NetworkId(1)].generation, generation + 1);
    });
    wait_reconnected
        .recv_timeout(Duration::from_secs(10))
        .expect("the silent network reconnected");
    chat.update(cx, |chat, _| {
        for network in networks {
            if let Some(connection) = chat.sessions.get_mut(&network).and_then(|s| s.irc.take()) {
                let _ = connection.disconnect();
            }
        }
    });
    assert_eq!(
        wait_pinged.recv_timeout(Duration::from_secs(25)).unwrap(),
        None,
        "a manually disconnected network is not checked"
    );
    server_silent.join().unwrap();
    server_answering.join().unwrap();
    server_manual.join().unwrap();
}
