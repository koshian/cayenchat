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
        assert!(ping.starts_with("PING cayenchat-wake-"), "{ping:?}");
        let (_second, _socket) = accept_registered(&silent);
        reconnected.send(()).unwrap();
    });
    // Network 2 answers it.
    let server_answering = thread::spawn(move || {
        let (mut reader, mut socket) = accept_registered(&answering);
        // Closing ends the test's wait for this server.
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        // Every check is answered: if the first PONG arrives before the
        // duplicate wake notice is handled, that one starts a second check.
        while let Some(ping) = next_line(&mut reader, "PING ") {
            assert!(ping.starts_with("PING cayenchat-wake-"), "{ping:?}");
            let token = ping
                .trim_end()
                .trim_start_matches("PING ")
                .trim_start_matches(':');
            socket
                .write_all(format!(":server PONG server :{token}\r\n").as_bytes())
                .unwrap();
        }
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
    chat.update(cx, |chat, _| {
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
    let generation = chat.update(cx, |chat, cx| {
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
        chat.start_reconnect(NetworkId(1), false, cx);
        generation
    });
    // The reconnect reads its passwords first.
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
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

#[gpui::test]
fn sleep_quits_the_live_servers_and_the_wake_reconnects_only_those(cx: &mut TestAppContext) {
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
    let (quitting, silent, manual) = (
        listeners.next().unwrap(),
        listeners.next().unwrap(),
        listeners.next().unwrap(),
    );
    // Network 1 takes the QUIT, closes, and is connected to again.
    let (quit_seen, wait_quit) = mpsc::channel();
    let (reconnected, wait_reconnected) = mpsc::channel();
    let server_quitting = thread::spawn(move || {
        let (mut reader, socket) = accept_registered(&quitting);
        quit_seen.send(next_line(&mut reader, "QUIT")).unwrap();
        drop(socket);
        let (_second, _socket) = accept_registered(&quitting);
        reconnected.send(()).unwrap();
    });
    // Network 2 reads the QUIT and never closes: the wait for it is bounded.
    let (silent_quit, wait_silent) = mpsc::channel();
    let server_silent = thread::spawn(move || {
        let (mut reader, socket) = accept_registered(&silent);
        silent_quit.send(next_line(&mut reader, "QUIT")).unwrap();
        let (_second, _socket) = accept_registered(&silent);
        drop(socket);
    });
    // Network 3 was disconnected by the user: nothing happens to it.
    let (heard, wait_heard) = mpsc::channel();
    let server_manual = thread::spawn(move || {
        let (mut reader, _socket) = accept_registered(&manual);
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        heard.send(next_line(&mut reader, "QUIT")).unwrap();
    });

    let networks = [NetworkId(1), NetworkId(2), NetworkId(3)];
    chat.update(cx, |chat, _| {
        for (network, port) in networks.into_iter().zip(&ports) {
            let mut config = config(*port);
            config.quit_message = "Normal".into();
            if network == NetworkId(1) {
                config.sleep_quit_message = "Sleeping now".into();
            }
            let session = chat.sessions.get_mut(&network).unwrap();
            session.irc = Some(Connection::connect(config.clone()).unwrap());
            session.active_config = Some(config);
        }
    });
    let pump = |chat: &mut ChatWindow, cx: &mut gpui::Context<ChatWindow>| {
        let mut registered = 0;
        let mut closed = Vec::new();
        for network in networks {
            let mut batch = Vec::new();
            while let Some(event) = chat
                .sessions
                .get_mut(&network)
                .and_then(|session| session.irc.as_mut())
                .and_then(Connection::try_recv)
            {
                match event {
                    Event::Registered { .. } => registered += 1,
                    Event::Closed(_) | Event::Disconnected(_) => closed.push(network),
                    _ => {}
                }
                batch.push(event);
            }
            if !batch.is_empty() {
                chat.handle_events(network, batch, Worker::Running, cx);
            }
        }
        (registered, closed)
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut registered = 0;
    while registered < networks.len() {
        assert!(Instant::now() < deadline, "not registered");
        registered += chat.update(cx, |chat, cx| pump(chat, cx)).0;
        thread::sleep(Duration::from_millis(20));
    }
    chat.update(cx, |chat, _| {
        chat.sessions
            .get_mut(&NetworkId(3))
            .unwrap()
            .manual_disconnect = true;
    });

    // The sleep notice: both live servers are quit at once, within the limit.
    let started = Instant::now();
    chat.update(cx, |chat, cx| chat.prepare_for_sleep(cx));
    assert!(started.elapsed() < Duration::from_millis(2000));
    assert_eq!(
        wait_quit
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .as_deref(),
        Some("QUIT :Sleeping now\r\n")
    );
    // Without a sleep message of its own, the normal one is sent.
    assert_eq!(
        wait_silent
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .as_deref(),
        Some("QUIT Normal\r\n")
    );
    let mut closed = Vec::new();
    while !closed.contains(&NetworkId(1)) {
        assert!(Instant::now() < deadline, "no close");
        closed.extend(chat.update(cx, |chat, cx| pump(chat, cx)).1);
        thread::sleep(Duration::from_millis(20));
    }
    chat.update(cx, |chat, _| {
        // Asleep is neither the user's doing nor a reason to retry.
        for network in [NetworkId(1), NetworkId(2)] {
            let session = &chat.sessions[&network];
            assert!(session.asleep, "{network:?}");
            assert!(!session.manual_disconnect, "{network:?}");
            assert!(!session.retry_pending, "{network:?}");
        }
        assert!(!chat.sessions[&NetworkId(3)].asleep);
        assert!(chat.sessions[&NetworkId(3)].manual_disconnect);
        assert!(!chat.sessions[&NetworkId(1)].retry_pending);
    });

    // The wake: the networks ended for sleep connect again, the other not.
    let generations = chat.update(cx, |chat, cx| {
        let generations = [NetworkId(1), NetworkId(2)].map(|n| chat.sessions[&n].generation);
        chat.check_connections_after_wake(cx);
        // A duplicate notice reconnects nothing more.
        chat.check_connections_after_wake(cx);
        generations
    });
    // The reconnects read their passwords first.
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert_eq!(chat.sessions[&NetworkId(1)].generation, generations[0] + 1);
        assert_eq!(chat.sessions[&NetworkId(2)].generation, generations[1] + 1);
        assert_eq!(chat.sessions[&NetworkId(3)].generation, 0);
        assert!(!chat.sessions[&NetworkId(1)].asleep);
    });
    wait_reconnected
        .recv_timeout(Duration::from_secs(10))
        .expect("the quit network connected again");
    // Its server sees nothing from us until its own read gives up.
    assert_eq!(
        wait_heard.recv_timeout(Duration::from_secs(15)).unwrap(),
        None,
        "a manually disconnected network is not touched"
    );
    chat.update(cx, |chat, _| {
        for network in networks {
            if let Some(connection) = chat.sessions.get_mut(&network).and_then(|s| s.irc.take()) {
                let _ = connection.disconnect();
            }
        }
    });
    server_quitting.join().unwrap();
    server_silent.join().unwrap();
    server_manual.join().unwrap();
}

#[gpui::test]
fn sleep_holds_back_a_pending_retry_until_the_wake(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let network = NetworkId(1);
    let generation = chat.update(cx, |chat, cx| {
        let session = chat.sessions.get_mut(&network).unwrap();
        session.active_config = Some(config(1));
        session.retry_pending = true;
        let token = session.retry_token;
        chat.prepare_for_sleep(cx);
        let session = &chat.sessions[&network];
        // The retry timer's condition no longer holds.
        assert!(session.asleep);
        assert!(!session.retry_pending);
        assert_ne!(session.retry_token, token);
        assert!(session.irc.is_none());
        // The wake reconnects it, as it does the connections quit for sleep.
        let generation = session.generation;
        chat.check_connections_after_wake(cx);
        generation
    });
    cx.run_until_parked();
    chat.update(cx, |chat, _| {
        assert_eq!(chat.sessions[&network].generation, generation + 1);
        assert!(!chat.sessions[&network].asleep);
        if let Some(connection) = chat.sessions.get_mut(&network).and_then(|s| s.irc.take()) {
            let _ = connection.disconnect();
        }
    });
}

#[gpui::test]
fn saved_leaving_messages_reach_the_open_connection(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        chat.sessions.get_mut(&NetworkId(1)).unwrap().active_config = Some(config(1));
        let mut next = chat.saved.clone();
        next.servers[0].quit_message = "Bye all".into();
        next.servers[0].sleep_quit_message = "Zzz".into();
        chat.apply_servers(next.clone(), cx);
        let config = chat.sessions[&NetworkId(1)].active_config.as_ref().unwrap();
        assert_eq!(config.checked_sleep_quit_message(), "Zzz");
        // Values that settings saving lets through are not taken over: the
        // previous one stays, so the QUIT is valid and so is the reconnect.
        for bad in ["a\nb", "a\0b", &"x".repeat(600)] {
            next.servers[0].sleep_quit_message = bad.into();
            next.servers[0].quit_message = bad.into();
            chat.apply_servers(next.clone(), cx);
            let config = chat.sessions[&NetworkId(1)].active_config.as_ref().unwrap();
            assert_eq!(config.checked_sleep_quit_message(), "Zzz");
            assert_eq!(config.quit_message, "Bye all");
            assert!(config.validate().is_ok());
        }
        next.servers[0].quit_message = "Bye all".into();
        next.servers[0].sleep_quit_message = "  ".into();
        chat.apply_servers(next, cx);
        let config = chat.sessions[&NetworkId(1)].active_config.as_ref().unwrap();
        assert_eq!(config.checked_sleep_quit_message(), "Bye all");
    });
}
