// SPDX-License-Identifier: MPL-2.0
use super::*;
use base64::Engine;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    time::Instant,
};

#[test]
fn topic_lines_become_topic_events() {
    let topic = |line: &str| topic_event(&line.parse::<IrcMessage>().unwrap(), None);
    let event = |channel: &str, text: &str, changed_by: Option<&str>| {
        Some(Event::Topic {
            channel: channel.into(),
            topic: text.into(),
            changed_by: changed_by.map(str::to_owned),
            server_time: None,
        })
    };
    assert_eq!(
        topic(":irc.example 332 me #test :Welcome, all"),
        event("#test", "Welcome, all", None)
    );
    assert_eq!(
        topic(":irc.example 331 me #test :No topic is set"),
        event("#test", "", None)
    );
    assert_eq!(
        topic(":alice!u@h TOPIC #test :New topic"),
        event("#test", "New topic", Some("alice"))
    );
    assert_eq!(
        topic(":alice!u@h TOPIC #test :"),
        event("#test", "", Some("alice"))
    );
    assert_eq!(topic(":alice!u@h PRIVMSG #test :hi"), None);
}

#[test]
fn member_commands_validate_targets_and_build_expected_irc_lines() {
    let wire = |action| match member_outgoing("Alice", action).unwrap() {
        Outgoing::Raw(message) => message.to_string().trim_end().to_owned(),
        other => panic!("expected raw command: {other:?}"),
    };
    assert_eq!(wire(MemberCommand::Whois), "WHOIS Alice");
    assert_eq!(
        wire(MemberCommand::Invite {
            channel: "#test".into()
        }),
        "INVITE Alice #test"
    );
    assert_eq!(
        wire(MemberCommand::GiveOp {
            channel: "#test".into()
        }),
        "MODE #test +o Alice"
    );
    assert_eq!(
        wire(MemberCommand::Deop {
            channel: "#test".into()
        }),
        "MODE #test -o Alice"
    );
    assert!(member_outgoing("bad nick", MemberCommand::Whois).is_err());
    assert!(member_outgoing("Alice,Bob", MemberCommand::Whois).is_err());
}

#[test]
fn private_conversations_are_targets_for_me_and_msg_but_not_channel_commands() {
    let target = |line: &str| match parse_slash_command(line, Some("bob")) {
        Ok(Outgoing::Message { target, .. }) => Ok(target),
        Ok(other) => Err(format!("{other:?}")),
        Err(error) => Err(error),
    };
    assert_eq!(target("/me waves").as_deref(), Ok("bob"));
    assert_eq!(target("/msg :hello").as_deref(), Ok("bob"));
    assert_eq!(target("/msg carol hi").as_deref(), Ok("carol"));
    assert!(parse_slash_command("/part", Some("bob")).is_err());
    assert!(parse_slash_command("/topic new", Some("bob")).is_err());
}

#[test]
fn service_credentials_are_not_echoed_into_conversations() {
    assert_eq!(
        echo_text("NickServ", "IDENTIFY hunter2".into()),
        "[redacted]"
    );
    assert_eq!(
        echo_text("nickserv@services.example", "identify a b".into()),
        "[redacted]"
    );
    assert_eq!(echo_text("NickServ", "INFO bob".into()), "INFO bob");
    assert_eq!(
        echo_text("bob", "IDENTIFY hunter2".into()),
        "IDENTIFY hunter2"
    );
    // Other networks' services.
    for (target, body) in [
        ("Q@CServe.quakenet.org", "AUTH alice hunter2"),
        ("Q", "NEWPASS old new new"),
        ("X@channels.undernet.org", "login alice hunter2"),
        ("AuthServ", "auth alice hunter2"),
        ("AuthServ@Services.GameSurge.net", "PASS old new"),
    ] {
        assert_eq!(echo_text(target, body.into()), "[redacted]", "{target}");
    }
    assert_eq!(echo_text("Q", "WHOIS #chan".into()), "WHOIS #chan");
}

#[test]
fn credentials_to_other_networks_services_stay_out_of_the_transcript() {
    let line = |command| redacted_wire_line(&IrcMessage::from(command));
    assert_eq!(
        line(IrcCommand::PRIVMSG(
            "Q@CServe.quakenet.org".into(),
            "AUTH alice hunter2".into()
        )),
        "PRIVMSG Q@CServe.quakenet.org :[redacted]"
    );
    assert_eq!(
        line(IrcCommand::Raw(
            "AUTH".into(),
            vec!["alice".into(), "hunter2".into()]
        )),
        "AUTH [redacted]"
    );
    assert_eq!(
        line(IrcCommand::Raw(
            "AUTHSERV".into(),
            vec!["AUTH".into(), "alice".into(), "hunter2".into()]
        )),
        "AUTHSERV [redacted]"
    );
    // `/quote AUTHSERV :AUTH alice hunter2` is one trailing argument.
    assert_eq!(
        line(IrcCommand::Raw(
            "AUTHSERV".into(),
            vec!["AUTH alice hunter2".into()]
        )),
        "AUTHSERV [redacted]"
    );
}

#[test]
fn account_tags_ride_along_with_channel_and_private_messages() {
    let private =
        |line: &str| private_message(&line.parse::<IrcMessage>().unwrap(), "me", false, None);
    let account = |event: Option<Event>| match event {
        Some(Event::PrivateMessage { account, .. }) => account,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        account(private("@account=alice :alice!u@h PRIVMSG me :hi")),
        Some("alice".into())
    );
    // Not logged in: the tag is absent; `*` and empty values are not accounts.
    assert_eq!(account(private(":alice!u@h PRIVMSG me :hi")), None);
    assert_eq!(
        account(private("@account=* :alice!u@h PRIVMSG me :hi")),
        None
    );
    assert_eq!(
        account(private("@account= :alice!u@h PRIVMSG me :hi")),
        None
    );
    // The account can differ between messages of one nickname.
    assert_eq!(
        account(private("@account=alice2 :alice!u@h PRIVMSG me :again")),
        Some("alice2".into())
    );
}

#[test]
fn private_messages_come_only_from_users_to_our_nickname() {
    let translate =
        |line: &str| private_message(&line.parse::<IrcMessage>().unwrap(), "Me", false, None);
    assert_eq!(
        translate(":alice!u@h PRIVMSG me :hello"),
        Some(Event::PrivateMessage {
            sender: "alice".into(),
            text: "hello".into(),
            notice: false,
            server_time: None,
            msgid: None,
            account: None,
            replayed: false,
        })
    );
    assert_eq!(
        translate(":alice!u@h NOTICE Me :psst"),
        Some(Event::PrivateMessage {
            sender: "alice".into(),
            text: "psst".into(),
            notice: true,
            server_time: None,
            msgid: None,
            account: None,
            replayed: false,
        })
    );
    assert!(matches!(
        translate(":alice!u@h PRIVMSG Me :\u{1}ACTION waves\u{1}"),
        Some(Event::PrivateMessage { .. })
    ));
    assert_eq!(translate(":alice!u@h PRIVMSG Me :\u{1}VERSION\u{1}"), None);
    assert_eq!(translate(":irc.example NOTICE Me :*** Looking up"), None);
    assert_eq!(translate(":alice!u@h PRIVMSG other :hello"), None);
    // RFC 1459 case mapping: `[` and `{` are the same letter.
    let message = ":alice!u@h PRIVMSG m{e} :x".parse::<IrcMessage>().unwrap();
    assert!(private_message(&message, "M[E]", false, None).is_some());
    // Our own line to someone, relayed by a bouncer (another client of
    // ours, or its playback).
    let own = |line: &str| private_message(&line.parse::<IrcMessage>().unwrap(), "Me", true, None);
    assert_eq!(
        own("@msgid=x1 :me!u@h PRIVMSG bob :sent elsewhere"),
        Some(Event::OwnPrivateMessage {
            target: "bob".into(),
            text: "sent elsewhere".into(),
            notice: false,
            server_time: None,
            msgid: Some("x1".into()),
            replayed: true,
        })
    );
    assert!(matches!(
        own(":me!u@h PRIVMSG NickServ :IDENTIFY hunter2"),
        Some(Event::OwnPrivateMessage { text, .. }) if text == "[redacted]"
    ));
    assert_eq!(own(":me!u@h PRIVMSG #chan :channel"), None);
    assert_eq!(own(":me!u@h PRIVMSG bob :\u{1}VERSION\u{1}"), None);
    assert_eq!(translate(":alice!u@h PRIVMSG #chan :hello"), None);
}

#[test]
fn whois_collector_bounds_unfinished_replies() {
    let mut collector = WhoisCollector::default();
    for index in 0..MAX_PENDING_WHOIS + 10 {
        let line = format!(":srv 311 me nick{index} u h * :real");
        assert_eq!(collector.observe(&line.parse().unwrap()), None);
    }
    assert_eq!(collector.pending.len(), MAX_PENDING_WHOIS);
    for _ in 0..MAX_WHOIS_ITEMS {
        let line = ":srv 319 me nick0 :#a #b";
        collector.observe(&line.parse().unwrap());
        let line = ":srv 671 me nick0 :is using a secure connection";
        collector.observe(&line.parse().unwrap());
    }
    let info = collector
        .observe(&":srv 318 me nick0 :End".parse().unwrap())
        .unwrap();
    assert_eq!(info.channels.len(), MAX_WHOIS_ITEMS);
    assert_eq!(info.extra.len(), MAX_WHOIS_ITEMS);
}

#[test]
fn whois_realname_hides_both_avatar_marks() {
    for (wire, shown, marked) in [
        ("\\x034\\x03Alice", "Alice", true),
        ("\u{3}4\u{f}Alice", "Alice", true),
        ("\u{3}4\u{f}\\x034\\x03Alice", "Alice", true),
        ("\\x031\\x03Alice", "Alice", false),
        ("Alice", "Alice", false),
    ] {
        let mut collector = WhoisCollector::default();
        let mut feed = |line: &str| collector.observe(&line.parse::<IrcMessage>().unwrap());
        feed(&format!(":srv 311 me Alice u h * :{wire}"));
        let info = feed(":srv 318 me Alice :End of WHOIS list.").unwrap();
        assert_eq!(info.realname.as_deref(), Some(shown), "{wire:?}");
        assert_eq!(info.avatar_mark, marked, "{wire:?}");
    }
}

#[test]
fn whois_collector_merges_replies_until_end_of_whois() {
    let mut collector = WhoisCollector::default();
    let mut feed = |line: &str| collector.observe(&line.parse::<IrcMessage>().unwrap());
    assert_eq!(feed(":srv 301 me Alice :before whois"), None);
    assert_eq!(
        feed(":srv 311 me Alice ~alice example.org * :Alice Liddell"),
        None
    );
    assert_eq!(feed(":srv 319 me Alice :@#one +#two"), None);
    assert_eq!(feed(":srv 319 me Alice :#three"), None);
    assert_eq!(
        feed(":srv 312 me Alice irc.example.org :Example server"),
        None
    );
    assert_eq!(feed(":srv 301 me Alice :gone fishing"), None);
    assert_eq!(feed(":srv 330 me Alice alice :is logged in as"), None);
    assert_eq!(
        feed(":srv 671 me Alice :is using a secure connection"),
        None
    );
    assert_eq!(
        feed(":srv 317 me Alice 665 1788066240 :seconds idle, signon time"),
        None
    );
    let info = feed(":srv 318 me Alice :End of WHOIS list.").unwrap();
    assert!(info.found());
    assert_eq!(info.nickname, "Alice");
    assert_eq!(info.username.as_deref(), Some("~alice"));
    assert_eq!(info.host.as_deref(), Some("example.org"));
    assert_eq!(info.realname.as_deref(), Some("Alice Liddell"));
    assert_eq!(info.channels, ["@#one", "+#two", "#three"]);
    assert_eq!(info.server.as_deref(), Some("irc.example.org"));
    assert_eq!(info.server_info.as_deref(), Some("Example server"));
    assert_eq!(info.away.as_deref(), Some("gone fishing"));
    assert_eq!(info.account.as_deref(), Some("alice"));
    assert_eq!(info.idle_seconds, Some(665));
    assert_eq!(info.signon, Some(1788066240));
    assert_eq!(info.extra, ["is using a secure connection"]);
    assert_eq!(feed(":srv 301 me Alice :after whois"), None);

    assert_eq!(feed(":srv 401 me Nobody :No such nick/channel"), None);
    let missing = feed(":srv 318 me Nobody :End of WHOIS list.").unwrap();
    assert_eq!(missing.nickname, "Nobody");
    assert!(!missing.found());
}

#[test]
fn tls_provider_is_explicit_when_both_backends_are_enabled() {
    ensure_tls_crypto_provider().unwrap();
    assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    let _ = rustls::ClientConfig::builder();
}

#[test]
fn closed_worker_keeps_buffered_diagnostics_readable() {
    let (commands, _) = mpsc::channel(1);
    let (event_tx, events) = mpsc::channel(1);
    event_tx
        .try_send(Event::Diagnostic {
            elapsed: Duration::ZERO,
            message: "TLS failed".into(),
        })
        .unwrap();
    drop(event_tx);
    let mut connection = Connection {
        commands,
        cancel: Arc::new(Notify::new()),
        quit: Arc::default(),
        events: Some(Events(events)),
        encoding: "UTF-8".into(),
        charsets: Arc::new(ChannelCharsets::new(&[]).unwrap()),
        part_message: None,
        modes_per_line: Arc::new(AtomicUsize::new(0)),
        relay_prefix: Arc::new(AtomicUsize::new(0)),
        closed: None,
        transcript: Arc::default(),
    };
    assert!(!connection.is_closed());
    assert!(matches!(
        connection.try_recv(),
        Some(Event::Diagnostic { .. })
    ));
    assert!(connection.is_closed());
}

#[test]
fn member_modes_are_split_to_the_announced_limit_and_validated_first() {
    let (commands, mut queued) = mpsc::channel(8);
    let (_event_tx, events) = mpsc::channel(1);
    let connection = Connection {
        commands,
        cancel: Arc::new(Notify::new()),
        quit: Arc::default(),
        events: Some(Events(events)),
        encoding: "UTF-8".into(),
        charsets: Arc::new(ChannelCharsets::new(&[]).unwrap()),
        part_message: None,
        modes_per_line: Arc::new(AtomicUsize::new(2)),
        relay_prefix: Arc::new(AtomicUsize::new(0)),
        closed: None,
        transcript: Arc::default(),
    };
    let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    connection
        .send_member_modes("#t", MemberMode::Voice, &names(&["a", "b", "c"]))
        .unwrap();
    let mut sent = Vec::new();
    while let Ok(Outgoing::Raw(message)) = queued.try_recv() {
        sent.push(message.to_string().trim_end().to_owned());
    }
    assert_eq!(sent, ["MODE #t +vv a b", "MODE #t +v c"]);
    for (channel, list) in [
        ("#t", names(&["a", "bad nick"])),
        ("#t", names(&[])),
        ("nochannel", names(&["a"])),
    ] {
        assert!(
            connection
                .send_member_modes(channel, MemberMode::Op, &list)
                .is_err()
        );
    }
    assert!(queued.try_recv().is_err(), "nothing queued on error");
}

#[test]
fn member_modes_are_not_sent_in_part_when_the_queue_is_nearly_full() {
    // Room for two commands; three are needed (one mode per line).
    let (commands, mut queued) = mpsc::channel(2);
    let (_event_tx, events) = mpsc::channel(1);
    let connection = Connection {
        commands,
        cancel: Arc::new(Notify::new()),
        quit: Arc::default(),
        events: Some(Events(events)),
        encoding: "UTF-8".into(),
        charsets: Arc::new(ChannelCharsets::new(&[]).unwrap()),
        part_message: None,
        modes_per_line: Arc::new(AtomicUsize::new(1)),
        relay_prefix: Arc::new(AtomicUsize::new(0)),
        closed: None,
        transcript: Arc::default(),
    };
    let names = ["a", "b", "c"].map(str::to_owned).to_vec();
    assert!(
        connection
            .send_member_modes("#t", MemberMode::Op, &names)
            .is_err()
    );
    assert!(queued.try_recv().is_err(), "not even the first line went");
    // With enough room the same request goes through whole.
    connection
        .send_member_modes("#t", MemberMode::Op, &names[..2])
        .unwrap();
    assert!(queued.try_recv().is_ok() && queued.try_recv().is_ok());
}

#[test]
fn taken_events_can_be_awaited_until_the_worker_ends() {
    let (commands, _) = mpsc::channel(1);
    let (event_tx, events) = mpsc::channel(1);
    let mut connection = Connection {
        commands,
        cancel: Arc::new(Notify::new()),
        quit: Arc::default(),
        events: Some(Events(events)),
        encoding: "UTF-8".into(),
        charsets: Arc::new(ChannelCharsets::new(&[]).unwrap()),
        part_message: None,
        modes_per_line: Arc::new(AtomicUsize::new(0)),
        relay_prefix: Arc::new(AtomicUsize::new(0)),
        closed: None,
        transcript: Arc::default(),
    };
    let mut events = connection.take_events().unwrap();
    assert!(connection.take_events().is_none());
    assert!(connection.try_recv().is_none());
    event_tx.try_send(Event::TransportConnected).unwrap();
    drop(event_tx);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(matches!(
            events.recv().await,
            Some(Event::TransportConnected)
        ));
        assert!(events.recv().await.is_none());
    });
    assert!(events.is_closed());
}

#[test]
fn diagnostic_wire_preserves_messages_and_redacts_credentials() {
    let line = |command| redacted_wire_line(&IrcMessage::from(command));
    assert_eq!(
        line(IrcCommand::PASS("server-secret".into())),
        "PASS [redacted]"
    );
    assert_eq!(
        line(IrcCommand::AUTHENTICATE("base64-secret".into())),
        "AUTHENTICATE [redacted]"
    );
    assert_eq!(
        line(IrcCommand::AUTHENTICATE("PLAIN".into())),
        "AUTHENTICATE PLAIN"
    );
    assert_eq!(
        line(IrcCommand::OPER("alice".into(), "oper-secret".into())),
        "OPER alice [redacted]"
    );
    assert_eq!(
        line(IrcCommand::JOIN(
            "#test".into(),
            Some("channel-key".into()),
            None
        )),
        "JOIN #test [redacted]"
    );
    assert_eq!(
        line(IrcCommand::PRIVMSG(
            "NickServ".into(),
            "IDENTIFY account-secret".into()
        )),
        "PRIVMSG NickServ :[redacted]"
    );
    assert_eq!(
        line(IrcCommand::CHANSERV("IDENTIFY channel-secret".into())),
        "CHANSERV [redacted]"
    );
    assert_eq!(
        line(IrcCommand::PRIVMSG(
            "#test".into(),
            "hello from the channel".into()
        )),
        "PRIVMSG #test :hello from the channel"
    );
    let error = irc::error::Error::CodecFailed {
        codec: "ISO-2022-JP",
        data: "PASS server-secret".into(),
    };
    assert!(!stream_error_detail(&error).contains("server-secret"));
}

#[test]
fn library_config_sets_the_ping_timeout() {
    let config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
    let timeout = library_config(&config).ping_timeout();
    assert_eq!(timeout, PING_TIMEOUT_SECS);
    assert!(timeout > 60);
}

#[test]
fn certificate_verification_is_opt_out_for_tls_only() {
    let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
    assert!(!library_config(&config).dangerously_accept_invalid_certs());
    config.verify_tls_certificates = false;
    assert!(library_config(&config).dangerously_accept_invalid_certs());
    config.use_tls = false;
    assert!(!library_config(&config).dangerously_accept_invalid_certs());
}

#[test]
fn debug_output_redacts_credentials_and_username_is_validated() {
    let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
    config.server_password = Some("server-secret".into());
    config.sasl = Some(SaslCredentials {
        username: "account".into(),
        password: "sasl-secret".into(),
    });
    let debug = format!("{config:?}");
    assert!(!debug.contains("server-secret"), "{debug}");
    assert!(!debug.contains("sasl-secret"), "{debug}");
    assert!(debug.contains("account"));

    assert_eq!(config.username, "alice");
    config.username = "ident".into();
    config.validate().unwrap();
    assert_eq!(config.nickname, "alice");
    assert_eq!(library_config(&config).username.as_deref(), Some("ident"));
    assert_eq!(library_config(&config).nickname.as_deref(), Some("alice"));
    for bad in ["", "two words", "a@b", "bad\r\nQUIT"] {
        config.username = bad.into();
        assert!(config.validate().is_err(), "{bad:?}");
    }
}

#[test]
fn config_and_outgoing_reject_protocol_control_characters() {
    let mut config = ConnectionConfig::tls(
        "irc.example.org".into(),
        "alice".into(),
        vec!["#test".into()],
    );
    assert!(config.validate().is_ok());
    config.nickname = "bad\r\nNICK".into();
    assert!(config.validate().is_err());
    assert!(!valid_channel("#bad\nJOIN"));
    config.nickname = "alice".into();
    config.sasl = Some(SaslCredentials {
        username: "account".into(),
        password: "secret".into(),
    });
    config.use_tls = false;
    assert!(config.validate().is_err());
    assert!(validate_wire("PRIVMSG #test :🙂\r\n", "ISO-2022-JP").is_err());
    let japanese = "あ".repeat(200);
    let outgoing = parse_slash_command(&format!("/msg #test {japanese}"), None).unwrap();
    assert!(validate_outgoing(&outgoing, "ISO-2022-JP").is_ok());
    assert!(validate_outgoing(&outgoing, "UTF-8").is_err());
}

#[test]
fn utf8_only_isupport_is_detected() {
    let message: IrcMessage = ":server 005 alice UTF8ONLY CHANTYPES=# :are supported"
        .parse()
        .unwrap();
    assert!(requires_utf8(&message));
    let message: IrcMessage = ":server 005 alice CHANTYPES=# :are supported"
        .parse()
        .unwrap();
    assert!(!requires_utf8(&message));
}

#[test]
fn casemapping_is_read_from_isupport_and_withdrawn_by_its_removal() {
    let read = |line: &str| {
        let message: IrcMessage = line.parse().unwrap();
        advertised_casemapping(&message)
    };
    assert_eq!(
        read(":srv 005 alice CHANTYPES=# CASEMAPPING=ascii :are supported"),
        Some(CaseMapping::Ascii)
    );
    assert_eq!(
        read(":srv 005 alice casemapping=rfc1459-strict :are supported"),
        Some(CaseMapping::Rfc1459Strict)
    );
    assert_eq!(
        read(":srv 005 alice CASEMAPPING=rfc7613 :are supported"),
        Some(CaseMapping::Ascii)
    );
    assert_eq!(
        read(":srv 005 alice -CASEMAPPING :are supported"),
        Some(CaseMapping::Rfc1459)
    );
    assert_eq!(read(":srv 005 alice CHANTYPES=# :are supported"), None);
    assert_eq!(read(":srv 001 alice :CASEMAPPING=ascii"), None);
}

/// IRC lines reach the transcript during registration, and afterwards
/// only while the UI asks for them.
#[test]
fn the_transcript_follows_registration_and_the_ui() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (next, wait) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        loop {
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).unwrap();
            if line.starts_with(b"USER ") {
                break;
            }
        }
        socket
            .write_all(b":server 001 alice :Welcome\r\n:bob!u@h PRIVMSG alice :one\r\n")
            .unwrap();
        wait.recv().unwrap();
        socket
            .write_all(b":bob!u@h PRIVMSG alice :two\r\n")
            .unwrap();
        wait.recv().unwrap();
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let until_message = |connection: &mut Connection, text: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut wire = Vec::new();
        loop {
            assert!(Instant::now() < deadline, "no message {text}");
            match connection.try_recv() {
                Some(Event::Wire { line, .. }) => wire.push(line),
                Some(Event::PrivateMessage { text: got, .. }) if got == text => break wire,
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
    };
    let before = until_message(&mut connection, "one");
    assert!(before.iter().any(|line| line.starts_with("NICK alice")));
    assert!(before.iter().any(|line| line.contains(" 001 alice ")));
    assert!(!before.iter().any(|line| line.contains("PRIVMSG")));
    connection.set_transcript(true);
    next.send(()).unwrap();
    let after = until_message(&mut connection, "two");
    assert_eq!(after, [":bob!u@h PRIVMSG alice two"]);
    next.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn iso_2022_jp_channel_name_and_message_round_trip() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        loop {
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).unwrap();
            if line.starts_with(b"USER ") {
                break;
            }
        }
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        let mut join = Vec::new();
        reader.read_until(b'\n', &mut join).unwrap();
        // The encoded comma is part of が, not a channel-list separator.
        assert_eq!(join, b"JOIN #\x1b$B$,$,\x1b(B\r\n");
        socket.write_all(b":alice!u@h JOIN #\x1b$B$,$,\x1b(B\r\n:alice!u@h PRIVMSG #\x1b$B$,$,\x1b(B :\x1b$B$3$s$K$A$O\x1b(B\r\n").unwrap();
        let reply = loop {
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).unwrap();
            if line.starts_with(b"PRIVMSG ") {
                break line;
            }
        };
        assert_eq!(reply, b"PRIVMSG #\x1b$B$,$,\x1b(B \x1b$BJV;v\x1b(B\r\n");
    });
    let mut config =
        ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#がが".into()]);
    config.port = port;
    config.use_tls = false;
    config.encoding = "ISO-2022-JP".into();
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut joined = false;
    let mut received = false;
    while Instant::now() < deadline && !(joined && received) {
        match connection.try_recv() {
            Some(Event::Joined { channel }) => joined = channel == "#がが",
            Some(Event::ChannelMessage { channel, text, .. }) => {
                received = channel == "#がが" && text == "こんにちは";
            }
            Some(Event::Disconnected(reason)) => panic!("unexpected disconnect: {reason}"),
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(joined && received, "Japanese channel event was not decoded");
    assert!(connection.send_message("#がが", "🙂", false).is_err());
    connection.send_message("#がが", "返事", false).unwrap();
    server.join().unwrap();
}

/// Reads lines until one starts with `prefix`.
fn read_line_starting(reader: &mut BufReader<std::net::TcpStream>, prefix: &[u8]) -> Vec<u8> {
    loop {
        let mut line = Vec::new();
        assert_ne!(reader.read_until(b'\n', &mut line).unwrap(), 0, "closed");
        if line.starts_with(prefix) {
            return line;
        }
    }
}

/// The next chat message event: its channel and text.
fn next_channel_message(connection: &mut Connection) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "no channel message");
        match connection.try_recv() {
            Some(Event::ChannelMessage { channel, text, .. }) => return (channel, text),
            Some(Event::Disconnected(reason)) => panic!("unexpected disconnect: {reason}"),
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn registered_server(
    listener: TcpListener,
    extra: &'static [u8],
) -> BufReader<std::net::TcpStream> {
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    read_line_starting(&mut reader, b"USER ");
    socket
        .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
        .unwrap();
    socket.write_all(extra).unwrap();
    reader
}

/// Two channels of one UTF-8 server use different encodings for their text,
/// in both directions, while the server's default stays for everything else
/// and for a side that is not overridden.
#[test]
fn channels_of_one_server_use_their_own_text_encodings() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen, wait) = std_mpsc::channel();
    let server = thread::spawn(move || {
        let mut reader = registered_server(
            listener,
            b":bob!u@h PRIVMSG #jp :\x1b$B$3$s$K$A$O\x1b(B\r\n\
              :bob!u@h PRIVMSG #utf :\xe3\x81\x93\xe3\x82\x93\r\n\
              :bob!u@h PRIVMSG #JP :\x1b$B$3$s\x1b(B\r\n\
              :bob!u@h PRIVMSG #euc :\xa4\xb3\xa4\xf3\r\n",
        );
        assert_eq!(
            read_line_starting(&mut reader, b"PRIVMSG #jp"),
            b"PRIVMSG #jp \x1b$B$3$s\x1b(B\r\n"
        );
        assert_eq!(
            read_line_starting(&mut reader, b"PRIVMSG #utf"),
            "PRIVMSG #utf こん\r\n".as_bytes()
        );
        // Only the received side of #euc is overridden: it sends UTF-8.
        assert_eq!(
            read_line_starting(&mut reader, b"PRIVMSG #euc"),
            "PRIVMSG #euc こん\r\n".as_bytes()
        );
        // The action is converted like any text.
        assert_eq!(
            read_line_starting(&mut reader, b"PRIVMSG #jp"),
            b"PRIVMSG #jp :\x01ACTION \x1b$B$3\x1b(B\x01\r\n"
        );
        seen.send(()).unwrap();
        // After a reset the server's encoding applies again.
        assert_eq!(
            read_line_starting(&mut reader, b"PRIVMSG #jp"),
            "PRIVMSG #jp こ\r\n".as_bytes()
        );
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = port;
    config.use_tls = false;
    config.channel_encodings = vec![
        ChannelEncoding {
            channel: "#jp".into(),
            decode: Some("ISO-2022-JP".into()),
            encode: Some("ISO-2022-JP".into()),
        },
        ChannelEncoding {
            channel: "#euc".into(),
            decode: Some("EUC-JP".into()),
            encode: None,
        },
    ];
    let mut connection = Connection::connect(config).unwrap();
    for expected in [
        ("#jp", "こんにちは"),
        ("#utf", "こん"),
        // The same channel under the server's case mapping.
        ("#JP", "こん"),
        ("#euc", "こん"),
    ] {
        assert_eq!(
            next_channel_message(&mut connection),
            (expected.0.to_owned(), expected.1.to_owned())
        );
    }
    connection.send_message("#jp", "こん", false).unwrap();
    connection.send_message("#utf", "こん", false).unwrap();
    connection.send_message("#euc", "こん", false).unwrap();
    connection.send_command("/me こ", Some("#jp")).unwrap();
    // A character the channel's encoding cannot hold is refused, naming it.
    let error = connection.send_message("#jp", "🙂", false).unwrap_err();
    assert!(error.contains("ISO-2022-JP"), "{error}");
    assert!(connection.send_message("#utf", "🙂", false).is_ok());
    // The worker writes queued lines later; the override goes once it has.
    wait.recv_timeout(Duration::from_secs(5)).unwrap();
    connection.set_channel_encoding("#jp", None, None).unwrap();
    connection.send_message("#jp", "こ", false).unwrap();
    server.join().unwrap();
}

/// The channel name keeps the server's encoding, ISO-2022-JP here, while the
/// channel's text uses another one.
#[test]
fn a_channel_override_leaves_the_channel_name_in_the_server_encoding() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut reader = registered_server(
            listener,
            b":bob!u@h PRIVMSG #\x1b$B$,$,\x1b(B :\xe3\x81\x93\xe3\x82\x93\r\n",
        );
        let join = read_line_starting(&mut reader, b"JOIN");
        assert_eq!(join, b"JOIN #\x1b$B$,$,\x1b(B\r\n");
        assert_eq!(
            read_line_starting(&mut reader, b"PRIVMSG"),
            b"PRIVMSG #\x1b$B$,$,\x1b(B \x82\xb1\x82\xf1\r\n"
        );
    });
    let mut config =
        ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#がが".into()]);
    config.port = port;
    config.use_tls = false;
    config.encoding = "ISO-2022-JP".into();
    config.channel_encodings = vec![ChannelEncoding {
        channel: "#がが".into(),
        decode: Some("UTF-8".into()),
        encode: Some("Shift_JIS".into()),
    }];
    let mut connection = Connection::connect(config).unwrap();
    assert_eq!(
        next_channel_message(&mut connection),
        ("#がが".to_owned(), "こん".to_owned())
    );
    connection.send_message("#がが", "こん", false).unwrap();
    server.join().unwrap();
}

#[test]
fn utf8_only_switches_off_other_channel_encodings() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut reader = registered_server(
            listener,
            b":server 005 alice UTF8ONLY :are supported\r\n\
              :bob!u@h PRIVMSG #jp :\xe3\x81\x93\r\n",
        );
        assert_eq!(
            read_line_starting(&mut reader, b"PRIVMSG"),
            "PRIVMSG #jp こ\r\n".as_bytes()
        );
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = port;
    config.use_tls = false;
    config.channel_encodings = vec![ChannelEncoding {
        channel: "#jp".into(),
        decode: Some("ISO-2022-JP".into()),
        encode: Some("ISO-2022-JP".into()),
    }];
    let mut connection = Connection::connect(config).unwrap();
    assert_eq!(
        next_channel_message(&mut connection),
        ("#jp".to_owned(), "こ".to_owned())
    );
    assert!(connection.utf8_only());
    connection.send_message("#jp", "こ", false).unwrap();
    // Nothing but UTF-8 can be set from now on.
    assert!(
        connection
            .set_channel_encoding("#x", Some("Shift_JIS"), None)
            .is_err()
    );
    assert!(
        connection
            .set_channel_encoding("#x", Some("utf-8"), None)
            .is_ok()
    );
    server.join().unwrap();
}

/// Every encoding the application offers is accepted by the connection and
/// by a channel override, and its text survives the wire in both directions.
#[test]
fn every_offered_encoding_round_trips() {
    use cayenchat_storage::TextEncoding;
    let sample = |encoding: TextEncoding| match encoding {
        TextEncoding::Utf8 => "日本語 🙂",
        TextEncoding::Iso2022Jp | TextEncoding::ShiftJis | TextEncoding::EucJp => "こんにちは",
        TextEncoding::EucKr => "한국어",
        TextEncoding::Windows1254 => "Çağrı şİ",
        TextEncoding::Windows1252 => "café €",
        TextEncoding::Gbk | TextEncoding::Big5 => "你好",
        TextEncoding::Koi8U => "Привіт ґ",
        _ => "Привет",
    };
    for encoding in TextEncoding::ALL {
        let label = encoding.label();
        let text = sample(encoding);
        let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
        config.encoding = label.into();
        config.channel_encodings = vec![ChannelEncoding {
            channel: "#c".into(),
            decode: Some(label.into()),
            encode: Some(label.into()),
        }];
        config
            .validate()
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        let line = format!("PRIVMSG #c :{text}\r\n");
        let codec = ChannelCharsets::new(&config.channel_encodings).unwrap();
        for charsets in [None, Some(&codec as &dyn BodyCharsets)] {
            let wire = Wire { label, charsets };
            let bytes =
                encode_strict(&line, wire).unwrap_or_else(|error| panic!("{label}: {error}"));
            let encoding_rs = charsets::resolve(label).unwrap();
            let back = irc::proto::charsets::decode_line(&bytes, encoding_rs, charsets);
            assert_eq!(back, line, "{label}");
        }
    }
    // Names other than the canonical one stay input aliases only.
    assert!(
        charsets::resolve("ISO-8859-1").is_ok(),
        "a WHATWG label, not an offered name"
    );
    assert_eq!(TextEncoding::parse("ISO-8859-1"), None);
}

#[test]
fn channel_encodings_are_validated() {
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    let entry = |channel: &str, decode: Option<&str>| ChannelEncoding {
        channel: channel.into(),
        decode: decode.map(str::to_owned),
        encode: None,
    };
    config.channel_encodings = vec![entry("#a", Some("Big5")), entry("#b", Some("windows-1251"))];
    assert!(config.validate().is_ok());
    config.channel_encodings = vec![entry("#a", Some("klingon"))];
    assert!(config.validate().unwrap_err().contains("klingon"));
    // Encodings that cannot be written are no encodings here.
    config.channel_encodings = vec![entry("#a", Some("utf-16le"))];
    assert!(config.validate().is_err());
    config.channel_encodings = vec![entry("alice", Some("Big5"))];
    assert!(config.validate().is_err());
}

/// A server that never ends its line is disconnected once the line passes
/// the codec's limit, instead of being buffered without bound.
#[test]
fn an_endless_line_disconnects_with_the_reason() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let chunk = [b'a'; 4096];
        // Stops when the client hangs up.
        while socket.write_all(&chunk).is_ok() {}
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let reason = loop {
        assert!(Instant::now() < deadline, "the client kept reading");
        match connection.try_recv() {
            Some(Event::Disconnected(reason)) => break reason,
            _ => thread::sleep(Duration::from_millis(10)),
        }
    };
    assert!(reason.contains("longer than 16384 bytes"), "{reason}");
    drop(connection);
    server.join().unwrap();
}

#[test]
fn slash_commands_infer_selected_channel_and_reject_injection() {
    let wire =
        |line: &str, selected: Option<&str>| match parse_slash_command(line, selected).unwrap() {
            Outgoing::Raw(message) => message.to_string().trim_end().to_owned(),
            other => panic!("expected raw command: {other:?}"),
        };
    assert_eq!(wire("/part", Some("#test")), "PART #test");
    assert_eq!(
        wire("/part Leaving now", Some("#test")),
        "PART #test :Leaving now"
    );
    assert_eq!(
        wire("/topic New topic", Some("#test")),
        "TOPIC #test :New topic"
    );
    assert_eq!(wire("/mode +o bob", Some("#test")), "MODE #test +o bob");
    assert_eq!(
        wire("/kick bob goodbye", Some("#test")),
        "KICK #test bob goodbye"
    );
    assert_eq!(wire("/invite bob", Some("#test")), "INVITE bob #test");
    assert_eq!(wire("/names", Some("#test")), "NAMES #test");
    assert_eq!(wire("/join #other", None), "JOIN #other");
    assert_eq!(wire("/raw WHOIS bob", None), "WHOIS bob");
    assert!(parse_slash_command("/part", None).is_err());
    assert!(parse_slash_command("/raw PRIVMSG #test :hi\r\nQUIT", Some("#test")).is_err());
    match parse_slash_command("/ctcp alice version", None).unwrap() {
        Outgoing::Ctcp { nick, tag, args } => {
            assert_eq!(
                (nick.as_str(), tag.as_str(), args.as_str()),
                ("alice", "VERSION", "")
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    match parse_slash_command("/ping alice", Some("#test")).unwrap() {
        Outgoing::Ctcp { nick, tag, args } => {
            assert_eq!(
                (nick.as_str(), tag.as_str(), args.as_str()),
                ("alice", "PING", "")
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    for line in [
        "/ping",
        "/ping alice bob",
        "/ping #test",
        "/ctcp alice",
        "/ctcp #a VERSION",
        "/ctcp alice DCC SEND",
    ] {
        assert!(parse_slash_command(line, None).is_err(), "{line}");
    }
    match parse_slash_command("/me waves", Some("#test")).unwrap() {
        Outgoing::Message {
            target,
            text,
            display_text,
            ..
        } => {
            assert_eq!(target, "#test");
            assert_eq!(text, "\u{1}ACTION waves\u{1}");
            assert_eq!(display_text, "* waves");
        }
        other => panic!("expected action: {other:?}"),
    }
    match parse_slash_command("/msg :hello everyone", Some("#test")).unwrap() {
        Outgoing::Message { target, text, .. } => {
            assert_eq!(target, "#test");
            assert_eq!(text, "hello everyone");
        }
        other => panic!("expected message: {other:?}"),
    }
}

#[test]
fn safe_channel_targets_and_commands_keep_the_full_name() {
    for channel in ["!test", "!ABCDEtest", "!ABCDE日本語"] {
        let config = ConnectionConfig::tls(
            "irc.example.org".into(),
            "alice".into(),
            vec![channel.into()],
        );
        assert!(config.validate().is_ok());
        assert!(!valid_nickname(channel));
        for (line, expected) in [
            (format!("/join {channel}"), format!("JOIN {channel}")),
            ("/part".into(), format!("PART {channel}")),
            (
                format!("/part {channel} bye"),
                format!("PART {channel} bye"),
            ),
            (
                "/topic new topic".into(),
                format!("TOPIC {channel} :new topic"),
            ),
            (
                format!("/topic {channel} topic"),
                format!("TOPIC {channel} topic"),
            ),
            ("/mode +o bob".into(), format!("MODE {channel} +o bob")),
            (
                format!("/mode {channel} +o bob"),
                format!("MODE {channel} +o bob"),
            ),
            ("/kick bob bye".into(), format!("KICK {channel} bob bye")),
            (
                format!("/kick {channel} bob bye"),
                format!("KICK {channel} bob bye"),
            ),
            ("/invite bob".into(), format!("INVITE bob {channel}")),
            (
                format!("/invite bob {channel}"),
                format!("INVITE bob {channel}"),
            ),
            ("/names".into(), format!("NAMES {channel}")),
        ] {
            let Outgoing::Raw(message) = parse_slash_command(&line, Some(channel)).unwrap() else {
                panic!("expected a raw command for {line}");
            };
            assert_eq!(message.to_string().trim_end(), expected);
        }
        let Outgoing::Message { target, .. } =
            parse_slash_command("/me waves", Some(channel)).unwrap()
        else {
            panic!("expected an action");
        };
        assert_eq!(target, channel);
        for action in [
            MemberCommand::Invite {
                channel: channel.into(),
            },
            MemberCommand::GiveOp {
                channel: channel.into(),
            },
            MemberCommand::Deop {
                channel: channel.into(),
            },
        ] {
            assert!(member_outgoing("bob", action).is_ok());
        }
    }
    for invalid in ["!", "!bad name", "!bad,other", "!bad\r\nJOIN", "!bad\0"] {
        assert!(!valid_channel(invalid));
    }
    // Accepting safe channels must not mistake mode flags for channel names.
    assert!(!valid_channel("+o"));
}

#[test]
fn safe_channel_join_names_messages_and_part_round_trip() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
        let read = |lines: &mut std::io::Lines<BufReader<std::net::TcpStream>>| {
            loop {
                let line = lines.next().expect("client closed the connection").unwrap();
                if !is_mode_query(&line) {
                    break line;
                }
            }
        };
        while !read(&mut lines).starts_with("USER ") {}
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        assert_eq!(read(&mut lines), "JOIN !test");
        // IRCnet resolves the short name to the server-assigned full name.
        socket.write_all(b":alice!u@h JOIN !ABCDEtest\r\n:server 353 alice = !ABCDEtest :@alice bob\r\n:server 366 alice !ABCDEtest :End of NAMES\r\n:bob!u@h PRIVMSG !ABCDEtest :hello\r\n:bob!u@h NOTICE !ABCDEtest :notice\r\n").unwrap();
        for expected in [
            "PRIVMSG !ABCDEtest reply",
            "NOTICE !ABCDEtest notice",
            "INVITE bob !ABCDEtest",
            "MODE !ABCDEtest +o bob",
            "PART !ABCDEtest",
        ] {
            assert_eq!(read(&mut lines), expected);
        }
        socket.write_all(b":alice!u@h PART !ABCDEtest\r\n").unwrap();
    });
    let mut config =
        ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["!test".into()]);
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut joined, mut names, mut message, mut notice, mut parted) =
        (false, false, false, false, false);
    let mut sent = false;
    while Instant::now() < deadline && !parted {
        match connection.try_recv() {
            Some(Event::Joined { channel }) => {
                assert_eq!(channel, "!ABCDEtest");
                joined = true;
            }
            Some(Event::Names { channel, users }) if users.len() == 2 => {
                assert_eq!(channel, "!ABCDEtest");
                assert!(users.contains(&"@alice".into()));
                assert!(users.contains(&"bob".into()));
                names = true;
            }
            Some(Event::ChannelMessage {
                channel,
                sender,
                text,
                notice: is_notice,
                ..
            }) => {
                assert_eq!(channel, "!ABCDEtest");
                assert_eq!(sender, "bob");
                assert_eq!(text, if is_notice { "notice" } else { "hello" });
                if is_notice {
                    notice = true;
                } else {
                    message = true;
                }
            }
            Some(Event::Parted { channel }) => {
                assert_eq!(channel, "!ABCDEtest");
                parted = true;
            }
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(5)),
        }
        if joined && names && message && notice && !sent {
            connection
                .send_message("!ABCDEtest", "reply", false)
                .unwrap();
            connection
                .send_message("!ABCDEtest", "notice", true)
                .unwrap();
            connection
                .send_member_command(
                    "bob",
                    MemberCommand::Invite {
                        channel: "!ABCDEtest".into(),
                    },
                )
                .unwrap();
            connection
                .send_member_command(
                    "bob",
                    MemberCommand::GiveOp {
                        channel: "!ABCDEtest".into(),
                    },
                )
                .unwrap();
            connection
                .send_command("/part", Some("!ABCDEtest"))
                .unwrap();
            sent = true;
        }
    }
    assert!(
        joined && names && message && notice && parted,
        "safe channel lifecycle incomplete"
    );
    server.join().unwrap();
}

#[test]
fn rejoining_after_a_kick_lists_each_member_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
        while !lines.next().unwrap().unwrap().starts_with("USER ") {}
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n:server 353 alice = #test :@alice bob\r\n:server 366 alice #test :End of NAMES\r\n:bob!u@h KICK #test alice :bye\r\n:alice!u@h JOIN #test\r\n:server 353 alice = #test :alice @bob\r\n:server 366 alice #test :End of NAMES\r\n").unwrap();
        // The mode query of each join; the socket must outlive them.
        for _ in 0..2 {
            assert_eq!(lines.next().unwrap().unwrap(), "MODE #test");
        }
    });
    let mut config =
        ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#test".into()]);
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut rosters = Vec::new();
    while Instant::now() < deadline && rosters.len() < 4 {
        match connection.try_recv() {
            Some(Event::Names { users, .. }) => rosters.push(users),
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    // Each JOIN also publishes a snapshot; the last one follows the final NAMES.
    assert_eq!(rosters.len(), 4, "{rosters:?}");
    let mut last = rosters[3].clone();
    last.sort();
    assert_eq!(last, ["@bob", "alice"], "{rosters:?}");
    server.join().unwrap();
}

#[test]
fn joining_queries_the_channel_modes_and_follows_changes_without_arguments() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
        while !lines.next().unwrap().unwrap().starts_with("USER ") {}
        socket
            .write_all(b":server 001 me :Welcome\r\n:server 376 me :End of MOTD\r\n")
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "JOIN #a");
        socket.write_all(b":me!u@h JOIN #a\r\n").unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "MODE #a");
        socket
            .write_all(b":server 324 me #a +sntkl secret 50\r\n:op!o@h MODE #a +m-s\r\n:op!o@h MODE #a +ov me me\r\n")
            .unwrap();
        // Keep the socket open until the client has read everything.
        let _ = lines.next();
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec!["#a".into()]);
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut flags = Vec::new();
    let mut logged = Vec::new();
    while Instant::now() < deadline && flags.len() < 3 {
        match connection.try_recv() {
            Some(Event::ChannelModes { channel, flags: f }) => {
                assert_eq!(channel, "#a");
                flags.push(f);
            }
            Some(Event::ChannelActivity {
                kind: ChannelActivityKind::ModeChanged { modes, .. },
                ..
            }) => logged.push(modes),
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    assert_eq!(flags, ["sntkl", "ntklm", "ntklm"]);
    // Only the MODE lines are activity; the 324 is not a mode change.
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert!(logged.iter().all(|modes| !modes.contains("secret")));
    drop(connection);
    server.join().unwrap();
}

#[test]
fn a_join_burst_sends_only_a_few_mode_queries_until_they_are_answered() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        while !lines.next().unwrap().unwrap().starts_with("USER ") {}
        socket
            .write_all(b":server 001 me :Welcome\r\n:server 376 me :End of MOTD\r\n")
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "JOIN #c0");
        let burst: String = (0..10).map(|n| format!(":me!u@h JOIN #c{n}\r\n")).collect();
        socket.write_all(burst.as_bytes()).unwrap();
        // Further lines from the server do not release more queries.
        socket
            .write_all(b":server 366 me #c0 :End of NAMES\r\n:server 366 me #c1 :End of NAMES\r\n")
            .unwrap();
        let mut asked = Vec::new();
        for _ in 0..4 {
            asked.push(lines.next().unwrap().unwrap());
        }
        assert_eq!(asked, ["MODE #c0", "MODE #c1", "MODE #c2", "MODE #c3"]);
        // A slow machine may let the 10 s give-up time pass while we wait;
        // a query then is a legitimate timeout, not a burst.
        let sent = Instant::now();
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let line = lines.next().unwrap();
        assert!(
            line.is_err() || sent.elapsed() > Duration::from_secs(8),
            "a fifth query went out"
        );
        if line.is_ok() {
            return;
        }
        // One answer frees one place.
        socket.write_all(b":server 324 me #c2 +n\r\n").unwrap();
        let answered = Instant::now();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "MODE #c4");
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let line = lines.next().unwrap();
        assert!(
            line.is_err() || answered.elapsed() > Duration::from_secs(8),
            "more than one query went out"
        );
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec!["#c0".into()]);
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !server.is_finished() {
        if connection.try_recv().is_none() {
            thread::sleep(Duration::from_millis(5));
        }
    }
    server.join().unwrap();
}

#[test]
fn modes_after_rejoining_from_a_kick_reach_the_roster() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
        while !lines.next().unwrap().unwrap().starts_with("USER ") {}
        socket
            .write_all(b":server 001 me :Welcome\r\n:server 376 me :End of MOTD\r\n")
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "JOIN #a");
        // carol left while we were out of the channel and is never reported.
        socket.write_all(b":me!u@h JOIN #a\r\n:server 353 me = #a :@op bob alice carol me\r\n:server 366 me #a :End of NAMES\r\n:op!o@h KICK #a me :bye\r\n:me!u@h JOIN #a\r\n:server 353 me = #a :@op bob alice me\r\n:server 366 me #a :End of NAMES\r\n:op!o@h MODE #a +o alice\r\n:op!o@h MODE #a +v bob\r\n").unwrap();
        // The mode query of each join; the socket must outlive them.
        for _ in 0..2 {
            assert_eq!(lines.next().unwrap().unwrap(), "MODE #a");
        }
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec!["#a".into()]);
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = Vec::new();
    let mut modes = 0;
    while Instant::now() < deadline && modes < 2 {
        match connection.try_recv() {
            Some(Event::Names { users, .. }) => last = users,
            Some(Event::ChannelActivity {
                kind: ChannelActivityKind::ModeChanged { .. },
                ..
            }) => modes += 1,
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    // A roster snapshot may follow the last mode change.
    thread::sleep(Duration::from_millis(100));
    while let Some(event) = connection.try_recv() {
        if let Event::Names { users, .. } = event {
            last = users;
        }
    }
    last.sort();
    assert_eq!(last, ["+bob", "@alice", "@op", "me"], "modes: {modes}");
    server.join().unwrap();
}

/// Connects to a server that sends `script` after the JOIN and returns the last roster.
fn roster_after_script(script: &'static [u8], settle_events: usize) -> Vec<String> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
        while !lines.next().unwrap().unwrap().starts_with("USER ") {}
        socket
            .write_all(b":server 001 me :Welcome\r\n:server 376 me :End of MOTD\r\n")
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "JOIN #a");
        socket.write_all(script).unwrap();
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "me".into(), vec!["#a".into()]);
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = Vec::new();
    let mut seen = 0;
    while Instant::now() < deadline && seen < settle_events {
        match connection.try_recv() {
            Some(Event::Names { users, .. }) => {
                last = users;
                seen += 1;
            }
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    thread::sleep(Duration::from_millis(100));
    while let Some(event) = connection.try_recv() {
        if let Event::Names { users, .. } = event {
            last = users;
        }
    }
    last.sort();
    server.join().unwrap();
    last
}

#[test]
fn a_kick_after_our_nick_changed_drops_the_old_roster() {
    let last = roster_after_script(
        b":server 353 me = #a :@op bob alice me\r\n:server 366 me #a :End of NAMES\r\n:me!u@h NICK renamed\r\n:op!o@h KICK #a renamed :bye\r\n:renamed!u@h JOIN #a\r\n:server 353 renamed = #a :@op bob alice renamed\r\n:server 366 renamed #a :End of NAMES\r\n:op!o@h MODE #a +o renamed\r\n:op!o@h MODE #a +v bob\r\n",
        3,
    );
    assert_eq!(last, ["+bob", "@op", "@renamed", "alice"]);
}

#[test]
fn a_kick_of_someone_using_our_old_nick_keeps_the_roster() {
    let last = roster_after_script(
        b":server 353 me = #a :@op bob me\r\n:server 366 me #a :End of NAMES\r\n:me!u@h NICK renamed\r\n:me!x@y JOIN #a\r\n:op!o@h KICK #a me :bye\r\n:op!o@h MODE #a +v bob\r\n",
        2,
    );
    assert_eq!(last, ["+bob", "@op", "renamed"]);
}

#[test]
fn kicks_and_refused_joins_reach_the_channel_log() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
        while !lines.next().unwrap().unwrap().starts_with("USER ") {}
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n:server 353 alice = #test :@alice bob carol\r\n:server 366 alice #test :End of NAMES\r\n:op!u@h KICK #test bob :spam\r\n:op!u@h KICK #test alice :bye\r\n:server 474 alice #test :Cannot join channel (+b)\r\n").unwrap();
    });
    let mut config =
        ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#test".into()]);
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut kinds = Vec::new();
    while Instant::now() < deadline && kinds.len() < 3 {
        match connection.try_recv() {
            Some(Event::ChannelActivity {
                actor,
                kind,
                channel,
                ..
            }) if kind.is_alert() => kinds.push((channel, actor, kind)),
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    assert_eq!(
        kinds,
        [
            (
                "#test".to_owned(),
                "op".to_owned(),
                ChannelActivityKind::Kicked {
                    target: "bob".into(),
                    reason: Some("spam".into()),
                    own: false,
                },
            ),
            (
                "#test".to_owned(),
                "op".to_owned(),
                ChannelActivityKind::Kicked {
                    target: "alice".into(),
                    reason: Some("bye".into()),
                    own: true,
                },
            ),
            (
                "#test".to_owned(),
                "server".to_owned(),
                ChannelActivityKind::JoinFailed {
                    banned: true,
                    bad_key: false,
                    reason: "Cannot join channel (+b)".into(),
                },
            ),
        ]
    );
    server.join().unwrap();
}

#[test]
fn local_server_registration_join_receive_and_send() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        let mut registration = Vec::new();
        loop {
            line.clear();
            lines.read_line(&mut line).unwrap();
            registration.push(line.trim_end().to_owned());
            if line.starts_with("USER ") {
                break;
            }
        }
        // Without credentials there is no PASS, and USER carries the
        // configured username rather than the nickname.
        assert!(!registration.iter().any(|line| line.starts_with("PASS")));
        assert!(registration.contains(&"NICK alice".to_owned()));
        assert!(
            registration.last().unwrap().starts_with("USER ident1 0 * "),
            "{registration:?}"
        );
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        loop {
            line.clear();
            lines.read_line(&mut line).unwrap();
            if line.starts_with("JOIN #test") {
                break;
            }
        }
        socket.write_all(b"PING :ping-token\r\n").unwrap();
        loop {
            line.clear();
            lines.read_line(&mut line).unwrap();
            if line.starts_with("PONG ") {
                assert_eq!(line.trim_end(), "PONG ping-token");
                break;
            }
        }
        socket.write_all(
                b":alice!u@h JOIN #test\r\n:alice!u@h JOIN #other\r\n:server 353 alice = #other :alice\r\n:server 366 alice #other :End of NAMES\r\n:alice!u@h PRIVMSG #test :hello\r\n:server 353 alice = #test :@alice  bob \r\n:server 366 alice #test :End of NAMES\r\n:charlie!u@h JOIN #test\r\n:alice!u@h MODE #test +o charlie\r\n:bob!u@h PART #test\r\n:charlie!u@h NICK dave\r\n:dave!u@h QUIT :bye\r\n"
            ).unwrap();
        let mut outgoing = Vec::new();
        while outgoing.len() < 7 {
            line.clear();
            lines.read_line(&mut line).unwrap();
            if line.starts_with("PRIVMSG ")
                || line.starts_with("NOTICE ")
                || line.starts_with("WHOIS ")
                || line.starts_with("INVITE ")
                || (line.starts_with("MODE ") && !is_mode_query(&line))
            {
                outgoing.push(line.trim_end().to_owned());
            }
        }
        outgoing
    });

    let mut config = ConnectionConfig::tls(
        "127.0.0.1".into(),
        "alice".into(),
        vec!["#test".into(), "#other".into()],
    );
    config.username = "ident1".into();
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    // The transcript is checked after registration too.
    connection.set_transcript(true);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen_registered = false;
    let mut seen_joined = false;
    let mut seen_message = false;
    let mut seen_names = false;
    let mut seen_roster_updated = false;
    let mut seen_nick_update = false;
    let mut seen_quit_update = false;
    let mut activities = Vec::new();
    let mut people = Vec::new();
    let mut rosters = Vec::new();
    let mut transcript = Vec::new();
    while Instant::now() < deadline
        && !(seen_registered
            && seen_joined
            && seen_message
            && seen_names
            && seen_roster_updated
            && seen_nick_update
            && seen_quit_update)
    {
        if let Some(event) = connection.try_recv() {
            match event {
                Event::Registered { nickname } => seen_registered = nickname == "alice",
                Event::Joined { channel } => seen_joined |= channel == "#test",
                Event::ChannelMessage {
                    channel,
                    sender,
                    text,
                    ..
                } => {
                    seen_message = channel == "#test" && sender == "alice" && text == "hello";
                }
                Event::Names { channel, users } => {
                    // charlie only ever joins #test, so its NICK and QUIT
                    // must not republish #other.
                    assert!(
                        channel != "#other"
                            || !activities.iter().any(|(actor, _)| actor == "charlie"),
                        "#other roster republished for an unrelated user"
                    );
                    // Extra spaces in NAMES must not become members.
                    assert!(
                        users.iter().all(|user| !display_nickname(user).is_empty()),
                        "empty roster entry in {users:?}"
                    );
                    rosters.push(users.clone());
                    seen_names |= channel == "#test"
                        && users.contains(&"@alice".to_owned())
                        && users.contains(&"bob".to_owned());
                    seen_roster_updated |= channel == "#test"
                        && users.contains(&"@alice".to_owned())
                        && users.contains(&"@charlie".to_owned())
                        && !users.contains(&"bob".to_owned());
                    seen_nick_update |= channel == "#test"
                        && users.contains(&"@dave".to_owned())
                        && !users.contains(&"@charlie".to_owned());
                    seen_quit_update |= channel == "#test" && users == ["@alice"];
                }
                Event::ChannelActivity {
                    channel,
                    actor,
                    kind,
                    ..
                } if channel == "#test" => activities.push((actor, kind)),
                Event::ChannelActivity {
                    channel,
                    actor,
                    kind: ChannelActivityKind::Quit { .. },
                    ..
                } => panic!("{actor} quit shown in {channel} without being a member"),
                Event::Wire {
                    direction, line, ..
                } => transcript.push((direction, line)),
                Event::Disconnected(reason) => panic!("unexpected disconnect: {reason}"),
                event @ (Event::UserNickChanged { .. } | Event::UserQuit { .. }) => {
                    people.push(event)
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }
    // Other users' renames and quits, once each, for private
    // conversations.
    assert_eq!(
        people,
        [
            Event::UserNickChanged {
                from: "charlie".into(),
                to: "dave".into()
            },
            Event::UserQuit {
                nickname: "dave".into(),
                reason: Some("bye".into())
            },
        ]
    );
    assert!(
        seen_registered
            && seen_joined
            && seen_message
            && seen_names
            && seen_roster_updated
            && seen_nick_update
            && seen_quit_update,
        "rosters: {rosters:?}"
    );
    for expected in [
        (
            "alice",
            ChannelActivityKind::Joined {
                mask: Some("u@h".into()),
            },
        ),
        (
            "charlie",
            ChannelActivityKind::Joined {
                mask: Some("u@h".into()),
            },
        ),
        (
            "alice",
            ChannelActivityKind::ModeChanged {
                modes: "+o charlie".into(),
                privileges: vec![PrivilegeChange {
                    granted: true,
                    mode: 'o',
                    target: "charlie".into(),
                }],
                replayed: false,
            },
        ),
        ("bob", ChannelActivityKind::Left { reason: None }),
        (
            "charlie",
            ChannelActivityKind::NickChanged { to: "dave".into() },
        ),
        (
            "dave",
            ChannelActivityKind::Quit {
                reason: Some("bye".into()),
            },
        ),
    ] {
        assert!(
            activities
                .iter()
                .any(|(actor, kind)| actor == expected.0 && kind == &expected.1),
            "missing {expected:?}: {activities:?}"
        );
    }
    connection.send_message("#test", "outgoing", false).unwrap();
    connection.send_message("#test", "notice", true).unwrap();
    connection
        .send_private_message("charlie", "hello", false)
        .unwrap();
    connection
        .send_member_command("charlie", MemberCommand::Whois)
        .unwrap();
    connection
        .send_member_command(
            "charlie",
            MemberCommand::Invite {
                channel: "#other".into(),
            },
        )
        .unwrap();
    connection
        .send_member_command(
            "charlie",
            MemberCommand::GiveOp {
                channel: "#test".into(),
            },
        )
        .unwrap();
    connection
        .send_member_command(
            "charlie",
            MemberCommand::Deop {
                channel: "#test".into(),
            },
        )
        .unwrap();
    let outgoing = server.join().unwrap();
    for expected in [
        "PRIVMSG charlie hello",
        "WHOIS charlie",
        "INVITE charlie #other",
        "MODE #test +o charlie",
        "MODE #test -o charlie",
    ] {
        assert!(
            outgoing.iter().any(|line| line == expected),
            "missing {expected}: {outgoing:?}"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline
        && !transcript.iter().any(|(direction, line)| {
            *direction == WireDirection::Sent && line == "NOTICE #test notice"
        })
    {
        if let Some(Event::Wire {
            direction, line, ..
        }) = connection.try_recv()
        {
            transcript.push((direction, line));
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }
    for (direction, expected) in [
        (WireDirection::Sent, "CAP END"),
        (WireDirection::Sent, "JOIN #test"),
        (WireDirection::Received, "PING ping-token"),
        (WireDirection::Sent, "PONG ping-token"),
        (WireDirection::Sent, "PRIVMSG #test outgoing"),
        (WireDirection::Sent, "NOTICE #test notice"),
    ] {
        assert!(
            transcript
                .iter()
                .any(|(actual_direction, line)| *actual_direction == direction && line == expected),
            "missing {direction:?} {expected}: {transcript:?}"
        );
    }
    assert!(
        transcript.iter().any(|(direction, line)| {
            *direction == WireDirection::Received
                && line.starts_with(":alice!u@h PRIVMSG #test")
                && line.ends_with("hello")
        }),
        "{transcript:?}"
    );
    assert!(
        outgoing.contains(&"PRIVMSG #test outgoing".to_owned()),
        "{outgoing:?}"
    );
    assert!(
        outgoing.contains(&"NOTICE #test notice".to_owned()),
        "{outgoing:?}"
    );
}

#[test]
fn disconnect_during_tls_handshake_ends_immediately() {
    // The listener accepts but never answers the TLS ClientHello, so the
    // worker would otherwise wait for the full transport timeout.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || listener.accept().map(|(socket, _)| socket));

    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
    config.port = port;
    let mut connection = Connection::connect(config).unwrap();
    let _socket = server.join().unwrap().unwrap();
    connection.disconnect().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut reason = None;
    while Instant::now() < deadline && reason.is_none() {
        match connection.try_recv() {
            Some(Event::Closed(detail)) => reason = Some(detail),
            Some(Event::Disconnected(detail) | Event::Refused(detail)) => {
                panic!("not a user disconnect: {detail}")
            }
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert_eq!(reason.as_deref(), Some(USER_DISCONNECT));
}

#[test]
fn plaintext_server_password_requires_opt_in_and_sasl_still_requires_tls() {
    let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
    config.use_tls = false;
    config.server_password = Some("user/network:secret".into());
    assert!(config.validate().is_err());
    config.allow_plaintext_pass = true;
    config.validate().unwrap();
    config.sasl = Some(SaslCredentials {
        username: "account".into(),
        password: "secret".into(),
    });
    assert!(config.validate().is_err());
}

#[test]
fn opted_in_plaintext_connection_sends_server_password() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut received = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            lines.read_line(&mut line).unwrap();
            received.push(line.trim_end().to_owned());
            if line.starts_with("USER ") {
                break;
            }
        }
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        received
    });

    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
    config.port = port;
    config.use_tls = false;
    config.server_password = Some("alice/net:secret".into());
    config.allow_plaintext_pass = true;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut registered = false;
    while Instant::now() < deadline && !registered {
        match connection.try_recv() {
            Some(Event::Registered { .. }) => registered = true,
            Some(Event::Disconnected(reason) | Event::Refused(reason)) => {
                panic!("unexpected disconnect: {reason}")
            }
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(registered, "client did not register");
    let received = server.join().unwrap();
    assert!(
        received.contains(&"PASS alice/net:secret".to_owned()),
        "{received:?}"
    );
}

#[test]
fn the_quit_message_defaults_and_is_validated() {
    let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
    assert_eq!(config.wire_quit_message(), "Leaving CayenChat");
    config.quit_message = "   ".into();
    assert_eq!(config.wire_quit_message(), "Leaving CayenChat");
    config.quit_message = " Gone fishing ".into();
    assert_eq!(config.wire_quit_message(), "Gone fishing");
    config.validate().unwrap();
    config.quit_message = "two\nlines".into();
    assert!(config.validate().is_err());
    config.quit_message = "a\0b".into();
    assert!(config.validate().is_err());
    config.quit_message = "x".repeat(600);
    assert!(config.validate().is_err());
    config.quit_message = "さよなら".into();
    config.encoding = "iso-8859-1".into();
    assert!(config.validate().is_err());
    config.encoding = "shift_jis".into();
    config.validate().unwrap();
}

/// Connects to a local server, disconnects, and returns the raw `QUIT`
/// line the server received (without its line ending).
fn quit_line_sent_with(configure: impl FnOnce(&mut ConnectionConfig)) -> Vec<u8> {
    quit_line_after(configure, |connection| connection.disconnect().unwrap())
}

/// The QUIT line a server receives after `end` ends a registered
/// connection and the worker has finished.
fn quit_line_after(
    configure: impl FnOnce(&mut ConnectionConfig),
    end: impl FnOnce(&mut Connection),
) -> Vec<u8> {
    quit_line_after_flood(configure, end, 0, Duration::ZERO)
}

/// As [`quit_line_after`], with the server sending `flood` NOTICE lines
/// after registering that nobody drains from the connection, and not
/// reading anything from the client for `read_delay` after that.
fn quit_line_after_flood(
    configure: impl FnOnce(&mut ConnectionConfig),
    end: impl FnOnce(&mut Connection),
    flood: usize,
    read_delay: Duration,
) -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut line = Vec::new();
        loop {
            line.clear();
            lines.read_until(b'\n', &mut line).unwrap();
            if line.starts_with(b"USER ") {
                break;
            }
        }
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        for index in 0..flood {
            socket
                .write_all(format!(":server NOTICE alice :flood {index}\r\n").as_bytes())
                .unwrap();
        }
        thread::sleep(read_delay);
        loop {
            line.clear();
            lines.read_until(b'\n', &mut line).unwrap();
            if line.starts_with(b"QUIT ") {
                while line
                    .last()
                    .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
                {
                    line.pop();
                }
                // Like an IRC server: more lines, ERROR, then it closes. A client
                // that drops the socket with these unread sends RST, which shows
                // here as an error instead of a clean end.
                for index in 0..100 {
                    let _ = socket
                        .write_all(format!(":server NOTICE alice :late {index}\r\n").as_bytes());
                }
                let _ = socket.write_all(b"ERROR :Closing Link: alice (Quit)\r\n");
                let reset = "client reset the connection instead of closing it";
                socket.shutdown(std::net::Shutdown::Write).expect(reset);
                let mut rest = Vec::new();
                std::io::Read::read_to_end(&mut lines, &mut rest).expect(reset);
                return line;
            }
        }
    });

    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
    config.port = port;
    config.use_tls = false;
    configure(&mut config);
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut registered = false;
    while Instant::now() < deadline {
        if let Some(Event::Registered { .. }) = connection.try_recv() {
            registered = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(registered, "client did not register");
    end(&mut connection);
    assert!(connection.wait_closed(Duration::from_secs(5)));
    server.join().unwrap()
}

#[test]
fn the_quit_command_sends_the_default_or_the_given_reason() {
    let line = quit_line_after(
        |config| config.quit_message = "Back soon".into(),
        |connection| connection.send_command("/quit", None).unwrap(),
    );
    assert_eq!(line, b"QUIT :Back soon".to_vec());
    let line = quit_line_after(
        |_| {},
        |connection| connection.send_command("/QUIT :Bye now", None).unwrap(),
    );
    assert_eq!(line, b"QUIT :Bye now".to_vec());
}

#[test]
fn shutdown_sends_quit_while_the_event_queue_is_full() {
    // More lines than EVENT_CAPACITY, and the test never drains them.
    let line = quit_line_after_flood(
        |_| {},
        |connection| {
            thread::sleep(Duration::from_millis(500));
            connection.shutdown();
            // The same deadline the application waits for on exit.
            assert!(connection.wait_closed(QUIT_WAIT));
        },
        EVENT_CAPACITY * 4,
        Duration::ZERO,
    );
    assert_eq!(line, b"QUIT :Leaving CayenChat".to_vec());
}

#[test]
fn shutdown_writes_quit_behind_a_send_backlog_while_lines_arrive() {
    // The server reads nothing for a while, so the socket fills up and
    // the writer stalls with QUIT queued behind the messages.
    let text = "x".repeat(400);
    let line = quit_line_after_flood(
        |_| {},
        |connection| {
            let stop = Instant::now() + Duration::from_secs(3);
            let mut queued = 0;
            while queued < 20_000 && Instant::now() < stop {
                while connection.try_recv().is_some() {}
                match connection.send_message("#test", &text, false) {
                    Ok(()) => queued += 1,
                    Err(_) => thread::sleep(Duration::from_millis(1)),
                }
            }
            assert!(queued > 10_000, "only {queued} messages were queued");
            connection.shutdown();
            // The server reads QUIT and the worker ends within the time
            // the application waits on exit.
            assert!(connection.wait_closed(QUIT_WAIT));
        },
        EVENT_CAPACITY * 4,
        Duration::from_millis(800),
    );
    assert_eq!(line, b"QUIT :Leaving CayenChat".to_vec());
}

#[test]
fn disconnect_flushes_quit_to_server() {
    assert_eq!(
        quit_line_sent_with(|_| {}),
        b"QUIT :Leaving CayenChat".to_vec()
    );
}

#[test]
fn disconnect_sends_the_configured_quit_message() {
    let line = quit_line_sent_with(|config| config.quit_message = " Back soon ".into());
    assert_eq!(line, b"QUIT :Back soon".to_vec());
    // Blank keeps the default.
    let line = quit_line_sent_with(|config| config.quit_message = "  ".into());
    assert_eq!(line, b"QUIT :Leaving CayenChat".to_vec());
}

#[test]
fn sleep_sends_the_sleep_message_and_closes_within_the_limit() {
    let started = Instant::now();
    let line = quit_line_after(
        |config| {
            config.quit_message = "Bye".into();
            config.sleep_quit_message = " Sleeping ".into();
        },
        |connection| connection.sleep("Sleeping"),
    );
    assert_eq!(line, b"QUIT Sleeping".to_vec());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_silent_server_does_not_hold_a_sleep_quit_past_the_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (done, wait) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        while !line.starts_with("USER ") {
            line.clear();
            lines.read_line(&mut line).unwrap();
        }
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        // Reads QUIT but never closes.
        while !line.starts_with("QUIT ") {
            line.clear();
            lines.read_line(&mut line).unwrap();
        }
        wait.recv_timeout(Duration::from_secs(10)).ok();
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
    config.port = port;
    config.use_tls = false;
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "not registered");
        if let Some(Event::Registered { .. }) = connection.try_recv() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    connection.sleep("Zzz");
    assert!(connection.wait_closed(Duration::from_secs(3)));
    assert!(started.elapsed() < Duration::from_millis(1800));
    let mut closed = None;
    while let Some(event) = connection.try_recv() {
        if let Event::Closed(reason) = event {
            closed = Some(reason);
        }
    }
    assert_eq!(closed.as_deref(), Some("Disconnected before sleep."));
    done.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn the_sleep_quit_message_falls_back_and_is_validated() {
    let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
    assert_eq!(config.wire_sleep_quit_message(), "Leaving CayenChat");
    config.quit_message = "Bye".into();
    config.sleep_quit_message = "  ".into();
    assert_eq!(config.wire_sleep_quit_message(), "Bye");
    config.sleep_quit_message = " Zzz ".into();
    assert_eq!(config.wire_sleep_quit_message(), "Zzz");
    config.validate().unwrap();
    config.sleep_quit_message = "two\nlines".into();
    assert!(config.validate().is_err());
    config.sleep_quit_message = "a\0b".into();
    assert!(config.validate().is_err());
    config.sleep_quit_message = "x".repeat(600);
    assert!(config.validate().is_err());
}

#[test]
fn the_quit_message_follows_the_connection_encoding() {
    let line = quit_line_sent_with(|config| {
        config.encoding = "ISO-2022-JP".into();
        config.quit_message = "さよなら".into();
    });
    // The whole line goes through the codec; a message without spaces
    // needs no leading colon.
    let mut want = encode_strict("QUIT さよなら\r\n", "ISO-2022-JP")
        .unwrap()
        .into_owned();
    assert!(want.contains(&0x1b), "ISO-2022-JP uses escape sequences");
    want.truncate(want.len() - 2);
    assert_eq!(line, want);
}

#[test]
fn the_part_message_is_validated_independently_of_quit() {
    let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
    config.validate().unwrap();
    assert_eq!(config.wire_part_message(), None);
    config.part_message = "   ".into();
    assert_eq!(config.wire_part_message(), None);
    config.part_message = " またね ".into();
    assert_eq!(config.wire_part_message().as_deref(), Some("またね"));
    config.validate().unwrap();
    config.encoding = "iso-8859-1".into();
    assert!(config.validate().is_err());
    config.encoding = "UTF-8".into();
    config.part_message = "two\nlines".into();
    assert!(config.validate().is_err());
    config.part_message = "a\0b".into();
    assert!(config.validate().is_err());
    config.part_message = "x".repeat(600);
    assert!(config.validate().is_err());
    // A bad PART message does not touch QUIT, and the reverse.
    config.part_message = String::new();
    config.quit_message = "Bye".into();
    config.validate().unwrap();
    assert_eq!(config.wire_quit_message(), "Bye");
}

#[test]
fn the_default_part_reason_applies_only_without_an_explicit_one() {
    let wire = |line: &str, default: Option<&str>| {
        let outgoing = parse_slash_command(line, Some("#example")).unwrap();
        match with_default_part_reason(outgoing, default) {
            Outgoing::Raw(message) => message.to_string().trim_end().to_owned(),
            other => panic!("expected raw command: {other:?}"),
        }
    };
    assert_eq!(wire("/part", Some("またね")), "PART #example またね");
    assert_eq!(
        wire("/part #example", Some("またね")),
        "PART #example またね"
    );
    assert_eq!(wire("/part #example :", Some("bye")), "PART #example bye");
    assert_eq!(
        wire("/part #example :休憩", Some("またね")),
        "PART #example 休憩"
    );
    assert_eq!(wire("/part #example", None), "PART #example");
    assert_eq!(wire("/topic hello", Some("またね")), "TOPIC #example hello");
}

#[test]
fn the_quit_line_may_use_the_whole_512_bytes() {
    // "QUIT :" is 6 bytes and CRLF 2, leaving 504 for the message.
    let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), vec![]);
    config.quit_message = "x".repeat(504);
    config.validate().unwrap();
    config.quit_message = "x".repeat(505);
    assert!(config.validate().is_err());
}

#[test]
fn sasl_plain_waits_for_success_before_ending_cap_negotiation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let read = |reader: &mut BufReader<std::net::TcpStream>| {
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if !line.starts_with("PING ") && !is_mode_query(&line) {
                    break line.trim_end().to_owned();
                }
            }
        };
        let initial = (0..4).map(|_| read(&mut lines)).collect::<Vec<_>>();
        assert!(initial.contains(&"CAP LS 302".to_owned()), "{initial:?}");
        assert!(
            initial.contains(&"PASS server-secret".to_owned()),
            "{initial:?}"
        );
        assert!(initial.contains(&"NICK alice".to_owned()), "{initial:?}");
        assert!(
            initial.iter().any(|line| line.starts_with("USER alice ")),
            "{initial:?}"
        );
        socket
            .write_all(b":server CAP alice LS :sasl=PLAIN,EXTERNAL\r\n")
            .unwrap();
        assert_eq!(read(&mut lines), "CAP REQ sasl");
        socket
            .write_all(b":server CAP alice ACK :sasl\r\n")
            .unwrap();
        assert_eq!(read(&mut lines), "AUTHENTICATE PLAIN");
        socket.write_all(b"AUTHENTICATE +\r\n").unwrap();
        let expected = base64::engine::general_purpose::STANDARD.encode(b"\0account\0secret");
        assert_eq!(read(&mut lines), format!("AUTHENTICATE {expected}"));
        socket
            .write_all(b":server 903 alice :SASL authentication successful\r\n")
            .unwrap();
        assert_eq!(read(&mut lines), "CAP END");
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        assert_eq!(read(&mut lines), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
        assert_eq!(read(&mut lines), "QUIT :Leaving CayenChat");
    });

    let mut config =
        ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#test".into()]);
    config.port = port;
    // This local fixture exercises the SASL protocol without a certificate. The public
    // constructor rejects this combination; production credentials require TLS.
    config.use_tls = false;
    config.server_password = Some("server-secret".into());
    config.sasl = Some(SaslCredentials {
        username: "account".into(),
        password: "secret".into(),
    });
    let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(config, command_rx, event_tx));
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut registered = false;
    let mut joined = false;
    let mut transcript = Vec::new();
    while Instant::now() < deadline && !(registered && joined) {
        match event_rx.try_recv() {
            Ok(Event::Registered { nickname }) => registered = nickname == "alice",
            Ok(Event::Joined { channel }) => joined = channel == "#test",
            Ok(Event::Wire {
                direction, line, ..
            }) => transcript.push((direction, line)),
            Ok(Event::Disconnected(reason)) => panic!("unexpected disconnect: {reason}"),
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(registered && joined);
    for (direction, expected) in [
        (WireDirection::Sent, "CAP LS 302"),
        (WireDirection::Sent, "PASS [redacted]"),
        (
            WireDirection::Received,
            ":server CAP alice LS sasl=PLAIN,EXTERNAL",
        ),
        (WireDirection::Sent, "CAP REQ sasl"),
        (WireDirection::Sent, "AUTHENTICATE PLAIN"),
        (WireDirection::Received, "AUTHENTICATE +"),
        (WireDirection::Sent, "AUTHENTICATE [redacted]"),
        (WireDirection::Sent, "CAP END"),
        (WireDirection::Sent, "JOIN #test"),
    ] {
        assert!(
            transcript
                .iter()
                .any(|(actual_direction, line)| *actual_direction == direction && line == expected),
            "missing {direction:?} {expected}: {transcript:?}"
        );
    }
    assert!(!format!("{transcript:?}").contains("server-secret"));
    assert!(!format!("{transcript:?}").contains("account"));
    assert!(!format!("{transcript:?}").contains("secret"));
    command_tx.try_send(Outgoing::Quit).unwrap();
    server.join().unwrap();
    worker.join().unwrap();
}

#[test]
fn sasl_failure_stops_registration() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            lines.read_line(&mut line).unwrap();
            if line.starts_with("USER ") {
                break;
            }
        }
        socket
            .write_all(b":server CAP alice LS :sasl=PLAIN\r\n")
            .unwrap();
        line.clear();
        lines.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "CAP REQ sasl");
        socket
            .write_all(b":server CAP alice ACK :sasl\r\n")
            .unwrap();
        line.clear();
        lines.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "AUTHENTICATE PLAIN");
        socket.write_all(b"AUTHENTICATE +\r\n").unwrap();
        line.clear();
        lines.read_line(&mut line).unwrap();
        assert!(line.starts_with("AUTHENTICATE "));
        socket
            .write_all(b":server 904 alice :Authentication failed\r\n")
            .unwrap();
    });

    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
    config.port = port;
    config.use_tls = false; // The private worker is tested without a certificate.
    config.sasl = Some(SaslCredentials {
        username: "account".into(),
        password: "wrong".into(),
    });
    let (_commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(config, command_rx, event_tx));
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut failure = None;
    while Instant::now() < deadline {
        match event_rx.try_recv() {
            Ok(Event::Refused(reason)) => {
                failure = Some(reason);
                break;
            }
            Ok(Event::Disconnected(reason)) => panic!("retryable SASL failure: {reason}"),
            Ok(Event::Registered { .. }) => panic!("registered after SASL failure"),
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert_eq!(failure.as_deref(), Some("SASL authentication failed."));
    server.join().unwrap();
    worker.join().unwrap();
}

#[test]
fn password_mismatch_is_refused_instead_of_retryable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            lines.read_line(&mut line).unwrap();
            if line.starts_with("USER ") {
                break;
            }
        }
        socket
            .write_all(b":server 464 alice :Password incorrect\r\nERROR :Closing link\r\n")
            .unwrap();
    });

    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
    config.port = port;
    config.use_tls = false; // The private worker is tested without a certificate.
    config.server_password = Some("wrong".into());
    let (_commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(config, command_rx, event_tx));
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut failure = None;
    while Instant::now() < deadline {
        match event_rx.try_recv() {
            Ok(Event::Refused(reason)) => {
                failure = Some(reason);
                break;
            }
            Ok(Event::Disconnected(reason)) => panic!("retryable 464: {reason}"),
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert_eq!(
        failure.as_deref(),
        Some("Server rejected the password (464).")
    );
    server.join().unwrap();
    worker.join().unwrap();
}

#[test]
fn nickname_in_use_waits_for_another_nickname() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            lines.read_line(&mut line).unwrap();
            if line.starts_with("USER ") {
                break;
            }
        }
        socket
            .write_all(b":server 433 * alice :Nickname is already in use\r\n")
            .unwrap();
        line.clear();
        lines.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "NICK alice_");
        socket
            .write_all(b":server 001 alice_ :Welcome\r\n")
            .unwrap();
        line.clear();
        let _ = lines.read_line(&mut line);
    });

    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
    config.port = port;
    config.use_tls = false; // The private worker is tested without a certificate.
    let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(config, command_rx, event_tx));
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut rejected = None;
    let mut registered = None;
    while Instant::now() < deadline && registered.is_none() {
        match event_rx.try_recv() {
            Ok(Event::NicknameRejected { nickname }) => {
                rejected = Some(nickname);
                commands
                    .try_send(checked_command(IrcCommand::NICK("alice_".into())).unwrap())
                    .unwrap();
            }
            Ok(Event::Registered { nickname }) => registered = Some(nickname),
            Ok(Event::Disconnected(reason) | Event::Refused(reason)) => {
                panic!("disconnected after 433: {reason}")
            }
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert_eq!(rejected.as_deref(), Some("alice"));
    assert_eq!(registered.as_deref(), Some("alice_"));
    commands.try_send(Outgoing::Quit).unwrap();
    server.join().unwrap();
    worker.join().unwrap();
}

/// Whether a client line is the query of a channel's modes sent after a join.
fn is_mode_query(line: &str) -> bool {
    line.trim_end()
        .strip_prefix("MODE ")
        .is_some_and(valid_channel)
}

/// Reads client lines from a fixture socket, skipping PINGs and the mode
/// query after each join.
fn read_client_line(reader: &mut BufReader<std::net::TcpStream>) -> String {
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if !line.starts_with("PING ") && !is_mode_query(&line) {
            break line.trim_end().to_owned();
        }
    }
}

/// Runs one connection against a fixture until `done` sees its events,
/// then quits. Returns every event received.
fn run_fixture(config: ConnectionConfig, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
    let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(config, command_rx, event_tx));
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = Vec::new();
    while Instant::now() < deadline && !done(&received) {
        match event_rx.try_recv() {
            Ok(event) => {
                if let Event::Disconnected(reason) | Event::Refused(reason) = &event {
                    panic!("unexpected end: {reason}");
                }
                received.push(event);
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(done(&received), "{received:?}");
    command_tx.try_send(Outgoing::Quit).unwrap();
    worker.join().unwrap();
    received
}

fn plain_config(port: u16, ircv3: Ircv3Options) -> ConnectionConfig {
    let mut config =
        ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#test".into()]);
    config.port = port;
    config.use_tls = false;
    config.ircv3 = ircv3;
    config
}

fn channel_messages(events: &[Event]) -> Vec<(String, Option<SystemTime>)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ChannelMessage {
                text, server_time, ..
            } => Some((text.clone(), *server_time)),
            _ => None,
        })
        .collect()
}

const TAGGED_TRAFFIC: &[u8] =
    b"@time=2011-10-19T16:40:51.620Z;msgid=Ab1 :bob!u@h PRIVMSG #test :stamped\r\n\
@time=not-a-time;msgid= :bob!u@h PRIVMSG #test :invalid\r\n\
@+typing=active;time=2011-10-19T16:40:52.000Z :bob!u@h TAGMSG #test\r\n\
@+typing=active :bob!u@h TAGMSG alice\r\n\
@time=2011-10-19T16:40:53.000Z :bob!u@h PART #test :bye\r\n\
@time=2011-10-19T16:40:54.000Z :irc.example NOTICE alice :tagged server line\r\n\
:bob!u@h PRIVMSG #test :plain\r\n";

fn history_options() -> Ircv3Options {
    Ircv3Options {
        chathistory: true,
        ..Ircv3Options::default()
    }
}

/// Answers registration for a server offering `offer`, acknowledging
/// every request, then accepts the JOIN of #test.
fn register_with(
    socket: &mut std::net::TcpStream,
    lines: &mut BufReader<std::net::TcpStream>,
    offer: &str,
    isupport: &str,
) -> Vec<String> {
    assert_eq!(read_client_line(lines), "CAP LS 302");
    let _nick = read_client_line(lines);
    let _user = read_client_line(lines);
    socket
        .write_all(format!(":srv CAP * LS :{offer}\r\n").as_bytes())
        .unwrap();
    let mut requested = Vec::new();
    loop {
        let line = read_client_line(lines);
        if line == "CAP END" {
            break;
        }
        let name = line.strip_prefix("CAP REQ ").unwrap().to_owned();
        socket
            .write_all(format!(":srv CAP * ACK :{name}\r\n").as_bytes())
            .unwrap();
        requested.push(name);
    }
    socket
            .write_all(
                format!(":srv 001 alice :Welcome\r\n:srv 005 alice {isupport} :are supported\r\n:srv 376 alice :End\r\n")
                    .as_bytes(),
            )
            .unwrap();
    if offer.contains("draft/chathistory") {
        // Once per connection, as soon as registration completes.
        assert!(read_client_line(lines).starts_with("CHATHISTORY TARGETS timestamp="),);
        socket
            .write_all(b":srv BATCH +tg draft/chathistory-targets\r\n:srv BATCH -tg\r\n")
            .unwrap();
    }
    assert_eq!(read_client_line(lines), "JOIN #test");
    socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
    requested
}

#[test]
fn chathistory_requests_the_latest_lines_on_join_and_reports_them_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let requested = register_with(
            &mut socket,
            &mut lines,
            "batch server-time message-tags draft/chathistory draft/event-playback",
            "CHATHISTORY=20 MSGREFTYPES=msgid,timestamp",
        );
        assert_eq!(
            requested,
            ["message-tags", "server-time", "batch", "draft/chathistory"]
        );
        assert_eq!(
            read_client_line(&mut lines),
            "CHATHISTORY LATEST #test * 20"
        );
        // A live line arrives before the reply and is also in it.
        socket
                .write_all(
                    b"@time=2026-09-27T23:58:31.000Z;msgid=live1 :bob!u@h PRIVMSG #test :alice: live during request\r\n\
@draft/chathistory-end :srv BATCH +r1 chathistory #test\r\n\
@batch=r1;time=2026-09-27T23:50:00.000Z;msgid=old1 :bob!u@h PRIVMSG #test :alice: old\r\n\
@batch=r1;time=2026-09-27T23:58:31.000Z;msgid=live1 :bob!u@h PRIVMSG #test :alice: live during request\r\n\
:srv BATCH -r1\r\n\
@time=2026-09-27T23:59:00.000Z;msgid=after1 :bob!u@h PRIVMSG #test :after\r\n",
                )
                .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let events = run_fixture(plain_config(port, history_options()), |events| {
        channel_messages(events).len() == 2
            && events
                .iter()
                .any(|event| matches!(event, Event::ChannelHistory { .. }))
    });
    server.join().unwrap();
    let requested = events
            .iter()
            .position(|e| matches!(e, Event::HistoryRequested { channel, resumed: false } if channel == "#test"))
            .expect("request reported");
    let joined = events
        .iter()
        .position(|e| matches!(e, Event::Joined { .. }))
        .unwrap();
    assert!(joined < requested, "the channel exists before its request");
    let history: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::ChannelHistory {
                channel,
                messages,
                incomplete: false,
            } => Some((channel, messages)),
            _ => None,
        })
        .collect();
    assert_eq!(history.len(), 1);
    let (channel, messages) = history[0];
    assert_eq!(channel, "#test");
    let lines: Vec<_> = messages
        .iter()
        .map(|m| (m.text.as_str(), m.msgid.as_deref()))
        .collect();
    assert_eq!(
        lines,
        [
            ("alice: old", Some("old1")),
            ("alice: live during request", Some("live1"))
        ]
    );
    assert!(messages.iter().all(|m| m.server_time.is_some()));
    // Reply lines never become live messages or server lines.
    let live: Vec<_> = channel_messages(&events)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert_eq!(live, ["alice: live during request", "after"]);
    assert!(!events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("BATCH") || line.contains("old"))));
}

fn sending_options() -> Ircv3Options {
    Ircv3Options {
        confirmed_sending: true,
        ..Ircv3Options::default()
    }
}

#[test]
fn labeled_messages_are_confirmed_by_their_echo_batch_and_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let requested = register_with(
            &mut socket,
            &mut lines,
            "echo-message labeled-response batch message-tags server-time",
            "NETWORK=t",
        );
        assert_eq!(
            requested,
            [
                "message-tags",
                "server-time",
                "batch",
                "echo-message",
                "labeled-response"
            ]
        );
        let label = |line: &str| -> String {
            let tag = line.strip_prefix("@label=").expect("labeled").to_owned();
            tag.split_once(' ').unwrap().0.to_owned()
        };
        // 1: echoed with a rewritten text and the server's id and time.
        let first = read_client_line(&mut lines);
        assert!(first.ends_with("PRIVMSG #test :hello  world"), "{first}");
        let l1 = label(&first);
        assert!(l1.len() <= 64);
        // 2: a multi-message labeled response (batch), 3: an error.
        let second = read_client_line(&mut lines);
        let third = read_client_line(&mut lines);
        let (l2, l3) = (label(&second), label(&third));
        assert!(l1 != l2 && l2 != l3 && l1 != l3, "labels are distinct");
        // Another client's message as us: no label, not a confirmation.
        socket
            .write_all(
                format!(
                    "@time=2026-09-28T10:00:00.000Z :alice!u@h PRIVMSG #test :from my phone\r\n\
@label={l1};msgid=m1;time=2026-09-28T10:00:01.000Z :alice!u@h PRIVMSG #test :hello world\r\n\
@label={l2} :srv BATCH +b labeled-response\r\n\
@batch=b;msgid=m2;time=2026-09-28T10:00:02.000Z :alice!u@h PRIVMSG bob :second\r\n\
:srv BATCH -b\r\n\
@label={l3} :srv 404 alice #closed :Cannot send to channel\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let mut connection = Connection::connect(plain_config(port, sending_options())).unwrap();
    wait_for(&mut connection, |e| matches!(e, Event::Joined { .. }));
    connection
        .send_message("#test", "hello  world", false)
        .unwrap();
    connection
        .send_private_message("bob", "second", false)
        .unwrap();
    connection.send_message("#test", "third", false).unwrap();
    let mut accepted = Vec::new();
    let mut settled = Vec::new();
    let mut live = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && settled.len() < 3 {
        match connection.try_recv() {
            Some(Event::OutgoingAccepted { local_id, .. }) => accepted.push(local_id),
            Some(Event::OutgoingConfirmed {
                local_id,
                text,
                msgid,
                server_time,
                ..
            }) => {
                assert!(server_time.is_some());
                settled.push(format!("{local_id} ok {text:?} {msgid:?}"));
            }
            Some(Event::OutgoingFailed { local_id, reason }) => {
                settled.push(format!("{local_id} failed {reason}"))
            }
            Some(Event::ChannelMessage { text, .. }) => live.push(text),
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    connection.disconnect().unwrap();
    server.join().unwrap();
    assert_eq!(accepted, [Some(1), Some(2), Some(3)]);
    assert_eq!(
        settled,
        [
            "1 ok Some(\"hello world\") Some(\"m1\")",
            "2 ok None Some(\"m2\")",
            "3 failed Cannot send to channel"
        ]
    );
    // Only the other client's line is a live message; no duplicates.
    assert_eq!(live, ["from my phone"]);
}

#[test]
fn a_label_does_not_count_against_the_512_byte_message_limit() {
    // The longest one-word text that still fits once the server has added
    // our prefix and the colon (`PRIVMSG #test ` is 14 bytes, CRLF 2) is
    // sent with a label as well. The `@label=...` tag has a limit of its own.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (expected_tx, expected_rx) = std_mpsc::channel::<String>();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(
            &mut socket,
            &mut lines,
            "echo-message labeled-response batch message-tags server-time",
            "NETWORK=t",
        );
        let sent = read_client_line(&mut lines);
        let (tags, message) = sent.split_once(' ').expect("labeled");
        assert!(tags.starts_with("@label="), "{tags}");
        // The whole text arrives, in a message within its own limit
        // (a single word is sent without the colon).
        let expected = expected_rx.recv().unwrap();
        assert!(message.ends_with(&expected), "{message}");
        assert!(message.len() + 2 <= 512, "{} bytes", message.len() + 2);
        let _ = read_client_line(&mut lines);
    });
    let mut connection = Connection::connect(plain_config(port, sending_options())).unwrap();
    wait_for(&mut connection, |e| matches!(e, Event::Joined { .. }));
    let prefix = relay::published(&connection.relay_prefix);
    let text = "x".repeat(512 - prefix - 16 - 1);
    expected_tx.send(text.clone()).unwrap();
    assert!(
        connection
            .send_message("#test", &format!("{text}x"), false)
            .is_err(),
        "one byte more does not fit"
    );
    connection.send_message("#test", &text, false).unwrap();
    let event = wait_for(&mut connection, |e| {
        matches!(e, Event::OutgoingAccepted { .. } | Event::ServerLine(_))
    });
    assert!(
        matches!(
            event,
            Event::OutgoingAccepted {
                local_id: Some(1),
                ..
            }
        ),
        "a message that fits without a label is sent with one: {event:?}"
    );
    connection.disconnect().unwrap();
    server.join().unwrap();

    // One byte more does not fit, with or without a label.
    assert!(
        validate_wire(&format!("PRIVMSG #test {}\r\n", "x".repeat(497)), "UTF-8").is_err(),
        "the message part stays limited to 512 bytes"
    );
}

#[test]
fn tagged_messages_have_separate_limits_for_tags_and_message() {
    let tagged = |label: &str, text: &str| {
        echo::with_label(
            IrcMessage::from(IrcCommand::PRIVMSG("#test".into(), text.into())),
            Some(label.into()),
        )
    };
    // A text of one word is sent without the colon, so 496 bytes fill
    // the 512 bytes and 497 do not.
    assert!(validate_tagged_wire(&tagged("c1", &"x".repeat(496)), "UTF-8").is_ok());
    // The message part is limited as before, tags aside.
    assert!(validate_tagged_wire(&tagged("c1", &"x".repeat(497)), "UTF-8").is_err());
    // The tags are limited on their own: 4094 bytes without `@` and the
    // space, so a value of 4094 - "label=".len() fits and one more does not.
    let fits = "l".repeat(4094 - "label=".len());
    assert!(validate_tagged_wire(&tagged(&fits, "hi"), "UTF-8").is_ok());
    let too_long = "l".repeat(4094 - "label=".len() + 1);
    assert!(
        validate_tagged_wire(&tagged(&too_long, "hi"), "UTF-8")
            .unwrap_err()
            .contains("tags")
    );
    // A character the encoding cannot hold is still refused.
    assert!(validate_tagged_wire(&tagged("c1", "🙂"), "ISO-2022-JP").is_err());
}

#[test]
fn withdrawing_labeled_response_keeps_echo_message_and_settles_pending_messages() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let requested = register_with(
            &mut socket,
            &mut lines,
            "echo-message labeled-response batch message-tags server-time",
            "NETWORK=t",
        );
        assert!(requested.contains(&"labeled-response".to_owned()));
        // Sent with a label, as labeled-response was on.
        let first = read_client_line(&mut lines);
        assert!(first.starts_with("@label="), "{first}");
        // The server withdraws labeled-response but keeps echo-message.
        // Its echo of the pending message comes without a label.
        socket
            .write_all(
                b":srv CAP alice DEL :labeled-response\r\n\
@msgid=m1;time=2026-09-28T10:00:01.000Z :alice!u@h PRIVMSG #test :still delivered\r\n",
            )
            .unwrap();
        // A message sent now goes out without a label.
        let second = read_client_line(&mut lines);
        assert_eq!(second, "PRIVMSG #test later");
        socket
            .write_all(b"@msgid=m2 :alice!u@h PRIVMSG #test :later\r\n")
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let mut connection = Connection::connect(plain_config(port, sending_options())).unwrap();
    wait_for(&mut connection, |e| matches!(e, Event::Joined { .. }));
    connection
        .send_message("#test", "still delivered", false)
        .unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut sent_second = false;
    while Instant::now() < deadline {
        match connection.try_recv() {
            Some(Event::OutgoingConfirmed {
                local_id, msgid, ..
            }) => {
                seen.push(format!("confirmed {local_id} {msgid:?}"));
                if !sent_second {
                    sent_second = true;
                    connection.send_message("#test", "later", false).unwrap();
                } else {
                    break;
                }
            }
            Some(Event::OutgoingFailed { local_id, .. }) => {
                seen.push(format!("failed {local_id}"));
            }
            Some(Event::ChannelMessage { text, .. }) => seen.push(format!("line {text}")),
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    connection.disconnect().unwrap();
    server.join().unwrap();
    // Each message is confirmed once; no echo is shown as another line.
    assert_eq!(
        seen,
        [
            "confirmed 1 Some(\"m1\")".to_owned(),
            "confirmed 2 Some(\"m2\")".to_owned()
        ]
    );
}

#[test]
fn without_labels_an_identical_echo_confirms_and_without_echo_nothing_is_tracked() {
    // echo-message only: no label tag on the wire, FIFO by text.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let requested = register_with(&mut socket, &mut lines, "echo-message", "NETWORK=t");
        assert_eq!(requested, ["echo-message"]);
        // No label tag: labeled-response was not negotiated.
        assert_eq!(read_client_line(&mut lines), "PRIVMSG #test hi");
        socket
            .write_all(b"@msgid=e1 :alice!u@h PRIVMSG #test :hi\r\n")
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let mut connection = Connection::connect(plain_config(port, sending_options())).unwrap();
    wait_for(&mut connection, |e| matches!(e, Event::Joined { .. }));
    connection.send_message("#test", "hi", false).unwrap();
    let event = wait_for(&mut connection, |e| {
        matches!(e, Event::OutgoingConfirmed { .. })
    });
    assert!(matches!(
        event,
        Event::OutgoingConfirmed { local_id: 1, text: None, msgid: Some(ref id), .. } if id == "e1"
    ));
    connection.disconnect().unwrap();
    server.join().unwrap();

    // A server without echo-message: the local line is all there is.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let requested = register_with(&mut socket, &mut lines, "batch", "NETWORK=t");
        assert!(requested.is_empty());
        assert_eq!(read_client_line(&mut lines), "PRIVMSG #test hi");
        let _ = read_client_line(&mut lines);
    });
    let mut connection = Connection::connect(plain_config(port, sending_options())).unwrap();
    wait_for(&mut connection, |e| matches!(e, Event::Joined { .. }));
    connection.send_message("#test", "hi", false).unwrap();
    let event = wait_for(&mut connection, |e| {
        matches!(e, Event::OutgoingAccepted { .. })
    });
    assert!(matches!(
        event,
        Event::OutgoingAccepted { local_id: None, .. }
    ));
    connection.disconnect().unwrap();
    server.join().unwrap();
}

#[test]
fn user_accounts_follow_joins_account_changes_and_one_whox_query() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let requested = register_with(
            &mut socket,
            &mut lines,
            "account-notify extended-join",
            "WHOX",
        );
        assert_eq!(requested, ["account-notify", "extended-join"]);
        socket
            .write_all(b":srv 353 alice = #test :alice bob carol\r\n:srv 366 alice #test :End\r\n")
            .unwrap();
        // Members present before us: one WHOX query, its reply consumed.
        assert_eq!(read_client_line(&mut lines), "WHO #test %tnar,1");
        socket
            .write_all(
                b":srv 354 alice 1 bob bob-acct :Bob Builder\r\n\
:srv 354 alice 1 carol 0 :Carol\r\n\
:srv 315 alice #test :End of /WHO list\r\n\
:dave!u@h JOIN #test dave-acct :Dave D\r\n\
:bob!u@h ACCOUNT *\r\n\
:carol!u@h ACCOUNT carol-acct\r\n\
:carol!u@h NICK carla\r\n\
:dave!u@h QUIT :bye\r\n",
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let options = Ircv3Options {
        accounts: true,
        ..Ircv3Options::default()
    };
    let events = run_fixture(plain_config(port, options), |events| {
        events
            .iter()
            .any(|e| matches!(e, Event::UserAccountForgotten { nickname } if nickname == "dave"))
    });
    server.join().unwrap();
    let tracked: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            Event::UserAccount {
                nickname,
                account,
                realname,
            } => Some(format!(
                "{nickname} {} {}",
                account.as_deref().unwrap_or("-"),
                realname.as_deref().unwrap_or("-")
            )),
            Event::UserAccountForgotten { nickname } => Some(format!("{nickname} gone")),
            _ => None,
        })
        .collect();
    assert_eq!(
        tracked,
        [
            "bob bob-acct Bob Builder",
            "carol - Carol",
            "dave dave-acct Dave D",
            "bob - Bob Builder",
            "carol carol-acct Carol",
            "carol gone",
            "carla carol-acct Carol",
            "dave gone",
        ]
    );
    // The WHOX reply and account lines are not server lines.
    assert!(!events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("354") || line.contains("Bob Builder"))));
}

#[test]
fn a_whox_reply_that_arrives_after_a_user_left_does_not_bring_them_back() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(
            &mut socket,
            &mut lines,
            "account-notify extended-join",
            "WHOX=ct",
        );
        // The query goes out on our join; bob leaves before its answer,
        // and the member list (NAMES) is not even published yet.
        assert_eq!(read_client_line(&mut lines), "WHO #test %tnar,1");
        socket
            .write_all(
                b":bob!u@h PART #test\r\n\
:srv 354 alice 1 bob bob-acct :Bob Builder\r\n\
:srv 354 alice 1 carol carol-acct :Carol\r\n\
:srv 315 alice #test :End of /WHO list\r\n\
:srv 353 alice = #test :alice carol\r\n:srv 366 alice #test :End\r\n\
:carol!u@h PRIVMSG #test :done\r\n",
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let options = Ircv3Options {
        accounts: true,
        ..Ircv3Options::default()
    };
    let events = run_fixture(plain_config(port, options), |events| {
        channel_messages(events).len() == 1
    });
    server.join().unwrap();
    let tracked: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            Event::UserAccount {
                nickname, account, ..
            } => Some(format!("{nickname} {}", account.as_deref().unwrap_or("-"))),
            Event::UserAccountForgotten { nickname } => Some(format!("{nickname} gone")),
            _ => None,
        })
        .collect();
    // Only carol is recorded; bob, who left first, is not resurrected.
    assert_eq!(tracked, ["carol carol-acct"]);
}

#[test]
fn user_accounts_are_not_tracked_unless_asked_for() {
    let (port, server) = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            // Nothing to negotiate: registration goes without CAP.
            assert_eq!(read_client_line(&mut lines), "CAP END");
            let _ = (read_client_line(&mut lines), read_client_line(&mut lines));
            socket
                    .write_all(b":srv 001 alice :Welcome\r\n:srv 005 alice WHOX :ok\r\n:srv 376 alice :End\r\n")
                    .unwrap();
            assert_eq!(read_client_line(&mut lines), "JOIN #test");
            socket
                    .write_all(b":alice!u@h JOIN #test\r\n:bob!u@h JOIN #test b :B\r\n:bob!u@h PRIVMSG #test :hi\r\n")
                    .unwrap();
            // No WHO follows: the next line is the QUIT.
            assert!(read_client_line(&mut lines).starts_with("QUIT"));
        });
        (port, server)
    };
    let events = run_fixture(plain_config(port, Ircv3Options::default()), |events| {
        channel_messages(events).len() == 1
    });
    server.join().unwrap();
    assert!(!events.iter().any(|e| matches!(
        e,
        Event::UserAccount { .. } | Event::UserAccountForgotten { .. }
    )));
}

#[test]
fn targets_discover_direct_messages_and_their_latest_lines_follow() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        // Registration by hand: the TARGETS reply is not empty here.
        assert_eq!(read_client_line(&mut lines), "CAP LS 302");
        let _ = (read_client_line(&mut lines), read_client_line(&mut lines));
        socket
            .write_all(b":srv CAP * LS :batch server-time message-tags draft/chathistory\r\n")
            .unwrap();
        loop {
            let line = read_client_line(&mut lines);
            if line == "CAP END" {
                break;
            }
            let name = line.strip_prefix("CAP REQ ").unwrap();
            socket
                .write_all(format!(":srv CAP * ACK :{name}\r\n").as_bytes())
                .unwrap();
        }
        socket
            .write_all(b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n")
            .unwrap();
        let asked = read_client_line(&mut lines);
        assert!(
            asked.starts_with("CHATHISTORY TARGETS timestamp=")
                && asked.ends_with(" 16")
                && asked.matches("timestamp=").count() == 2,
            "{asked}"
        );
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket
            .write_all(
                b":alice!u@h JOIN #test\r\n\
:srv BATCH +t draft/chathistory-targets\r\n\
@batch=t :srv CHATHISTORY TARGETS #test 2026-09-27T23:59:00.000Z\r\n\
@batch=t :srv CHATHISTORY TARGETS Bob 2026-09-27T23:58:00.000Z\r\n\
@batch=t :srv CHATHISTORY TARGETS bob 2026-09-27T23:57:00.000Z\r\n\
:srv BATCH -t\r\n",
            )
            .unwrap();
        // The channel asks on join; the direct message follows, one at a time.
        assert_eq!(
            read_client_line(&mut lines),
            "CHATHISTORY LATEST #test * 50"
        );
        socket
            .write_all(b":srv BATCH +c chathistory #test\r\n:srv BATCH -c\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "CHATHISTORY LATEST Bob * 50");
        socket
            .write_all(
                b":srv BATCH +d chathistory Bob\r\n\
@batch=d;time=2026-09-27T23:57:00.000Z;msgid=d1 :Bob!u@h PRIVMSG alice :did you get this?\r\n\
@batch=d;time=2026-09-27T23:58:00.000Z;msgid=d2 :alice!u@h PRIVMSG Bob :not yet\r\n\
:srv BATCH -d\r\n",
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let mut config = plain_config(port, history_options());
    config.history_targets_since = Some(SystemTime::now() - Duration::from_secs(3600));
    let events = run_fixture(config, |events| {
        events
            .iter()
            .any(|event| matches!(event, Event::ChannelHistory { channel, .. } if channel == "Bob"))
    });
    server.join().unwrap();
    let requested: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::HistoryRequested { channel, resumed } => Some((channel.as_str(), *resumed)),
            _ => None,
        })
        .collect();
    assert_eq!(requested, [("#test", false), ("Bob", false)]);
    let dm = events
        .iter()
        .find_map(|event| match event {
            Event::ChannelHistory {
                channel, messages, ..
            } if channel == "Bob" => Some(messages),
            _ => None,
        })
        .unwrap();
    let lines: Vec<_> = dm
        .iter()
        .map(|m| (m.sender.as_str(), m.text.as_str(), m.msgid.as_deref()))
        .collect();
    assert_eq!(
        lines,
        [
            ("Bob", "did you get this?", Some("d1")),
            ("alice", "not yet", Some("d2"))
        ]
    );
    // Discovery is history, not live traffic.
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::PrivateMessage { .. } | Event::OwnPrivateMessage { .. }
    )));
    assert!(!events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("TARGETS") || line.contains("BATCH") || line.contains("Bob"))));
}

/// Like [`run_fixture`], but `step` may queue commands as events arrive;
/// it returns `true` once everything expected has been received.
fn run_fixture_driven(
    config: ConnectionConfig,
    mut step: impl FnMut(&[Event], &mpsc::Sender<Outgoing>) -> bool,
) -> Vec<Event> {
    let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(config, command_rx, event_tx));
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = Vec::new();
    let mut done = false;
    while Instant::now() < deadline && !done {
        match event_rx.try_recv() {
            Ok(event) => {
                if let Event::Disconnected(reason) | Event::Refused(reason) = &event {
                    panic!("unexpected end: {reason}");
                }
                received.push(event);
                done = step(&received, &command_tx);
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(done, "{received:?}");
    command_tx.try_send(Outgoing::Quit).unwrap();
    worker.join().unwrap();
    received
}

fn older_request(request: u64, msgid: &str) -> Outgoing {
    Outgoing::OlderHistory {
        channel: "#test".into(),
        request,
        reference: MessageReference {
            msgid: Some(msgid.into()),
            time: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_550_000)),
        },
        limit: 100,
    }
}

#[test]
fn older_history_asks_before_the_oldest_message_and_reports_the_page_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(
            &mut socket,
            &mut lines,
            "batch server-time message-tags draft/chathistory",
            "CHATHISTORY=20 MSGREFTYPES=msgid,timestamp",
        );
        assert_eq!(
            read_client_line(&mut lines),
            "CHATHISTORY LATEST #test * 20"
        );
        socket
            .write_all(
                b":srv BATCH +r chathistory #test
@batch=r;time=2026-09-27T23:50:00.000Z;msgid=m5 :bob!u@h PRIVMSG #test :five
:srv BATCH -r
",
            )
            .unwrap();
        assert_eq!(
            read_client_line(&mut lines),
            "CHATHISTORY BEFORE #test msgid=m5 20"
        );
        // A live line arrives while the page is on its way.
        socket
                .write_all(
                    b"@time=2026-09-27T23:59:00.000Z;msgid=live :carol!u@h PRIVMSG #test :live meanwhile
@draft/chathistory-end :srv BATCH +o chathistory #test
@batch=o;time=2026-09-27T23:40:00.000Z;msgid=m3 :bob!u@h PRIVMSG #test :three
@batch=o;time=2026-09-27T23:45:00.000Z;msgid=m4 :bob!u@h NOTICE #test :four
:srv BATCH -o
",
                )
                .unwrap();
        assert!(read_client_line(&mut lines).starts_with("QUIT"));
    });
    let mut asked = false;
    let events = run_fixture_driven(plain_config(port, history_options()), |events, commands| {
        if !asked
            && events
                .iter()
                .any(|e| matches!(e, Event::ChannelHistory { .. }))
        {
            asked = true;
            commands.try_send(older_request(42, "m5")).unwrap();
        }
        channel_messages(events).len() == 1
            && events
                .iter()
                .any(|e| matches!(e, Event::OlderChannelHistory { .. }))
    });
    server.join().unwrap();
    let available = events
        .iter()
        .position(|e| matches!(e, Event::HistoryAvailable(true)))
        .expect("availability reported");
    let joined = events
        .iter()
        .position(|e| matches!(e, Event::Joined { .. }))
        .unwrap();
    assert!(available < joined);
    let pages: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::OlderChannelHistory {
                channel,
                request,
                messages,
                status,
            } => Some((channel, *request, messages, *status)),
            _ => None,
        })
        .collect();
    assert_eq!(pages.len(), 1);
    let (channel, request, messages, status) = &pages[0];
    assert_eq!((channel.as_str(), *request), ("#test", 42));
    assert_eq!(*status, OlderHistoryStatus::Beginning);
    let lines: Vec<_> = messages
        .iter()
        .map(|m| (m.text.as_str(), m.notice, m.msgid.as_deref()))
        .collect();
    assert_eq!(
        lines,
        [("three", false, Some("m3")), ("four", true, Some("m4"))]
    );
    // The live line stayed live; the page never did; no second reservation.
    assert_eq!(
        channel_messages(&events)
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>(),
        ["live meanwhile"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::HistoryRequested { .. }))
            .count(),
        1
    );
}

#[test]
fn older_history_fails_at_once_without_chathistory() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(&mut socket, &mut lines, "batch server-time", "CHANTYPES=#");
        // Nothing is asked of the server: the next line is QUIT.
        assert!(read_client_line(&mut lines).starts_with("QUIT"));
    });
    let mut asked = false;
    let events = run_fixture_driven(plain_config(port, history_options()), |events, commands| {
        if !asked && events.iter().any(|e| matches!(e, Event::Joined { .. })) {
            asked = true;
            commands.try_send(older_request(7, "m1")).unwrap();
        }
        events
            .iter()
            .any(|e| matches!(e, Event::OlderChannelHistory { .. }))
    });
    server.join().unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::HistoryAvailable(_)))
    );
    assert!(events.iter().any(|e| matches!(e,
            Event::OlderChannelHistory { request: 7, status: OlderHistoryStatus::Failed, messages, .. }
            if messages.is_empty())));
}

#[test]
fn a_reconnect_asks_only_for_the_lines_after_the_cut() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(
            &mut socket,
            &mut lines,
            "batch server-time message-tags draft/chathistory",
            "CHATHISTORY=2 MSGREFTYPES=msgid,timestamp",
        );
        assert_eq!(
            read_client_line(&mut lines),
            "CHATHISTORY LATEST #test msgid=m5 2"
        );
        // Live traffic after our JOIN, then the missed lines (the
        // server repeats the live one), and exactly as many as asked.
        socket
            .write_all(
                b"@time=2026-09-28T00:10:00.000Z;msgid=m8 :bob!u@h PRIVMSG #test :live
:srv BATCH +r chathistory #test
@batch=r;time=2026-09-28T00:05:00.000Z;msgid=m7 :bob!u@h PRIVMSG #test :missed
@batch=r;time=2026-09-28T00:10:00.000Z;msgid=m8 :bob!u@h PRIVMSG #test :live
:srv BATCH -r
",
            )
            .unwrap();
        assert!(read_client_line(&mut lines).starts_with("QUIT"));
    });
    let mut config = plain_config(port, history_options());
    config.resume_history = vec![HistoryResume {
        channel: "#TEST".into(),
        after: MessageReference {
            msgid: Some("m5".into()),
            time: None,
        },
    }];
    let events = run_fixture(config, |events| {
        events
            .iter()
            .any(|event| matches!(event, Event::ChannelHistory { .. }))
    });
    server.join().unwrap();
    assert!(events.iter().any(|event| matches!(event,
            Event::HistoryRequested { channel, resumed: true } if channel == "#test")));
    let Some(Event::ChannelHistory {
        messages,
        incomplete,
        ..
    }) = events
        .iter()
        .find(|event| matches!(event, Event::ChannelHistory { .. }))
    else {
        unreachable!()
    };
    let texts: Vec<_> = messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, ["missed", "live"]);
    assert!(
        *incomplete,
        "as many lines as asked for: more may be missing"
    );
    assert_eq!(
        channel_messages(&events)
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>(),
        ["live"]
    );
}

#[test]
fn chathistory_is_not_requested_from_servers_without_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let requested = register_with(
            &mut socket,
            &mut lines,
            "batch server-time message-tags",
            "CHANTYPES=#",
        );
        assert!(requested.is_empty(), "{requested:?}");
        socket
            .write_all(b":bob!u@h PRIVMSG #test :hello\r\n")
            .unwrap();
        // The next thing the client sends is its QUIT, not a request.
        assert!(read_client_line(&mut lines).starts_with("QUIT"));
    });
    let events = run_fixture(plain_config(port, history_options()), |events| {
        channel_messages(events).len() == 1
    });
    server.join().unwrap();
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::HistoryRequested { .. } | Event::ChannelHistory { .. }
    )));
}

#[test]
fn a_reply_cut_off_by_a_disconnect_reports_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(&mut socket, &mut lines, "batch draft/chathistory", "");
        assert_eq!(
            read_client_line(&mut lines),
            "CHATHISTORY LATEST #test * 50"
        );
        socket
            .write_all(
                b":srv BATCH +r chathistory #test\r\n@batch=r :bob!u@h PRIVMSG #test :partial\r\n",
            )
            .unwrap();
        drop(socket);
    });
    let (_command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel(EVENT_CAPACITY);
    let config = plain_config(port, history_options());
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(config, command_rx, event_tx));
    });
    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match event_rx.try_recv() {
            Ok(event) => {
                let end = matches!(event, Event::Disconnected(_));
                events.push(event);
                if end {
                    break;
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    worker.join().unwrap();
    server.join().unwrap();
    assert!(
        matches!(events.last(), Some(Event::Disconnected(_))),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::HistoryRequested { .. }))
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::ChannelHistory { .. } | Event::ChannelMessage { .. }
    )));
}

#[test]
fn disabled_ircv3_sends_plain_cap_end_and_ignores_tags() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let initial: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
        assert_eq!(initial[0], "CAP END", "no CAP LS when nothing is enabled");
        assert!(!initial.iter().any(|line| line.contains("CAP REQ")));
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
        socket.write_all(TAGGED_TRAFFIC).unwrap();
        let _ = read_client_line(&mut lines);
    });
    let events = run_fixture(plain_config(port, Ircv3Options::default()), |events| {
        channel_messages(events).len() == 3
    });
    server.join().unwrap();
    let messages = channel_messages(&events);
    assert!(
        messages.iter().all(|(_, time)| time.is_none()),
        "{messages:?}"
    );
    assert_tagmsg_invisible(&events);
}

fn assert_tagmsg_invisible(events: &[Event]) {
    for event in events {
        match event {
            Event::Wire { .. } | Event::Diagnostic { .. } => {}
            other => assert!(
                !format!("{other:?}").contains("TAGMSG")
                    && !format!("{other:?}").contains("typing"),
                "TAGMSG leaked into {other:?}"
            ),
        }
    }
    assert!(events.iter().any(|event| matches!(event,
            Event::Wire { direction: WireDirection::Received, line, .. } if line.contains("TAGMSG"))));
}

#[test]
fn server_time_is_negotiated_per_connection_and_reset_on_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        // First connection: server-time offered across continuation lines.
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let initial: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
        assert_eq!(initial[0], "CAP LS 302");
        socket
                .write_all(b":server CAP * LS * :multi-prefix sasl=PLAIN\r\n:server CAP * LS :server-time message-tags\r\n")
                .unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP REQ server-time");
        socket
            .write_all(b":server CAP * ACK :server-time\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP END");
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
        socket.write_all(TAGGED_TRAFFIC).unwrap();
        let _ = read_client_line(&mut lines);
        drop(socket);

        // Reconnect: a server without CAP that still sends time tags.
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        assert_eq!(read_client_line(&mut lines), "CAP LS 302");
        socket
            .write_all(b":server 421 * CAP :Unknown command\r\n")
            .unwrap();
        let _nick = read_client_line(&mut lines);
        let _user = read_client_line(&mut lines);
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
        socket.write_all(TAGGED_TRAFFIC).unwrap();
        let _ = read_client_line(&mut lines);
    });
    let options = Ircv3Options {
        message_tags: false,
        server_time: true,
        batch: false,
        metadata: false,
        peer_avatars: false,
        chathistory: false,
        confirmed_sending: false,
        accounts: false,
    };
    let events = run_fixture(plain_config(port, options), |events| {
        channel_messages(events).len() == 3
    });
    let stamp = |millis: u64| Some(SystemTime::UNIX_EPOCH + Duration::from_millis(millis));
    assert_eq!(
        channel_messages(&events),
        [
            ("stamped".to_owned(), stamp(1_319_042_451_620)),
            ("invalid".to_owned(), None),
            ("plain".to_owned(), None),
        ]
    );
    // An old timestamp alone does not make a live line replayed history.
    assert!(events.iter().any(|event| matches!(event,
            Event::ChannelMessage { text, replayed: false, server_time: Some(_), .. }
                if text == "stamped")));
    // The message ID travels with the event; an empty one is absent.
    assert!(events.iter().any(|event| matches!(event,
            Event::ChannelMessage { text, msgid: Some(id), .. } if text == "stamped" && id == "Ab1")));
    assert!(events.iter().any(|event| matches!(event,
            Event::ChannelMessage { text, msgid: None, .. } if text == "invalid")));
    assert!(events.iter().any(|event| matches!(event,
            Event::ChannelActivity { kind: ChannelActivityKind::Left { .. }, server_time, .. }
                if *server_time == stamp(1_319_042_453_000))));
    assert!(events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line == ":irc.example NOTICE alice :tagged server line")));
    assert_tagmsg_invisible(&events);

    let events = run_fixture(plain_config(port, options), |events| {
        channel_messages(events).len() == 3
    });
    server.join().unwrap();
    assert!(
        channel_messages(&events)
            .iter()
            .all(|(_, time)| time.is_none()),
        "a new connection does not inherit the previous negotiation"
    );
}

#[test]
fn legacy_encoding_keeps_server_time_and_skips_message_tags() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Encoded with the line ending so the encoder returns to ASCII.
    let body = encode_strict("日本語の本文\r\n", "iso-2022-jp")
        .unwrap()
        .into_owned();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        assert_eq!(read_client_line(&mut lines), "CAP LS 302");
        socket
            .write_all(b":server CAP * LS :message-tags server-time\r\n")
            .unwrap();
        let requests: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
        assert!(
            requests.contains(&"CAP REQ server-time".to_owned()),
            "{requests:?}"
        );
        assert!(
            !requests.iter().any(|line| line.contains("message-tags")),
            "{requests:?}"
        );
        socket
            .write_all(b":server CAP * ACK :server-time\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP END");
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
        let mut line =
            b"@time=2011-10-19T16:40:51.620Z;msgid=abc :bob!u@h PRIVMSG #test :".to_vec();
        line.extend_from_slice(&body);
        socket.write_all(&line).unwrap();
        let _ = read_client_line(&mut lines);
    });
    let mut config = plain_config(
        port,
        Ircv3Options {
            message_tags: true,
            server_time: true,
            batch: false,
            metadata: false,
            peer_avatars: false,
            chathistory: false,
            confirmed_sending: false,
            accounts: false,
        },
    );
    config.encoding = "ISO-2022-JP".into();
    let events = run_fixture(config, |events| channel_messages(events).len() == 1);
    server.join().unwrap();
    assert_eq!(
        channel_messages(&events),
        [(
            "日本語の本文".to_owned(),
            Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_319_042_451_620))
        )]
    );
    assert!(events.iter().any(|event| matches!(event,
            Event::Diagnostic { message, .. } if message.contains("Message tags are not requested"))));
}

/// Channel message texts with their replayed flags, in arrival order.
fn replay_flags(events: &[Event]) -> Vec<(String, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ChannelMessage { text, replayed, .. } => Some((text.clone(), *replayed)),
            _ => None,
        })
        .collect()
}

fn batch_server_lines(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ServerLine(line) if line.contains("BATCH") => Some(line.clone()),
            _ => None,
        })
        .collect()
}

const BATCH_TRAFFIC: &[u8] = b":srv BATCH +H1 chathistory #test\r\n\
@batch=H1;time=2011-10-19T16:40:51.620Z :bob!u@h PRIVMSG #test :alice: old\r\n\
:bob!u@h PRIVMSG #test :alice: live between\r\n\
@batch=H1 :srv BATCH +n1 example.com/nested\r\n\
@batch=n1 :bob!u@h PRIVMSG #test :alice: nested old\r\n\
:srv BATCH +u1 example.com/unknown\r\n\
@batch=u1 :bob!u@h PRIVMSG #test :alice: unknown type\r\n\
@batch=h1 :bob!u@h PRIVMSG #test :alice: other case\r\n\
@batch=H1 :srv BATCH -n1\r\n\
:srv BATCH -u1\r\n\
:srv BATCH -H1\r\n\
@batch=H1 :bob!u@h PRIVMSG #test :alice: after end\r\n";

#[test]
fn negotiated_batch_marks_history_until_it_is_withdrawn() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        assert_eq!(read_client_line(&mut lines), "CAP LS 302");
        socket
            .write_all(b":srv CAP * LS :batch server-time message-tags draft/chathistory\r\n")
            .unwrap();
        let requests: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
        assert!(
            requests.contains(&"CAP REQ batch".to_owned()),
            "{requests:?}"
        );
        assert!(
            requests
                .iter()
                .all(|line| !line.starts_with("CAP REQ") || line == "CAP REQ batch"),
            "only the opted-in capability is requested: {requests:?}"
        );
        socket.write_all(b":srv CAP * ACK :batch\r\n").unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP END");
        socket
            .write_all(b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
        socket.write_all(BATCH_TRAFFIC).unwrap();
        // Withdrawing batch forgets a history batch left open.
        socket
            .write_all(
                b":srv BATCH +h2 chathistory #test\r\n\
@batch=h2 :bob!u@h PRIVMSG #test :alice: before del\r\n\
:srv CAP alice DEL :batch\r\n\
@batch=h2 :bob!u@h PRIVMSG #test :alice: after del\r\n",
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
        drop(socket);

        // Reconnect: batch is no longer offered; the old reference is
        // meaningless on the new connection.
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        assert_eq!(read_client_line(&mut lines), "CAP LS 302");
        socket.write_all(b":srv CAP * LS :server-time\r\n").unwrap();
        let requests: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
        assert!(requests.contains(&"CAP END".to_owned()), "{requests:?}");
        assert!(!requests.iter().any(|line| line.starts_with("CAP REQ")));
        socket
            .write_all(b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket
                .write_all(b":alice!u@h JOIN #test\r\n@batch=h2 :bob!u@h PRIVMSG #test :alice: stale reference\r\n")
                .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let options = Ircv3Options {
        batch: true,
        ..Ircv3Options::default()
    };
    let events = run_fixture(plain_config(port, options), |events| {
        replay_flags(events).len() == 8
    });
    let owned = |pairs: &[(&str, bool)]| -> Vec<(String, bool)> {
        pairs
            .iter()
            .map(|(text, replayed)| ((*text).to_owned(), *replayed))
            .collect()
    };
    assert_eq!(
        replay_flags(&events),
        owned(&[
            ("alice: old", true),
            ("alice: live between", false),
            ("alice: nested old", true),
            ("alice: unknown type", false),
            ("alice: other case", false),
            ("alice: after end", false),
            ("alice: before del", true),
            ("alice: after del", false),
        ])
    );
    // server-time was not requested, so its tag is ignored.
    assert!(
        channel_messages(&events)
            .iter()
            .all(|(_, time)| time.is_none())
    );
    // Negotiated framing stays out of the server log but in the transcript.
    let before_del = events
        .iter()
        .position(|event| matches!(event, Event::ServerLine(line) if line.contains("DEL")))
        .unwrap_or(events.len());
    assert!(batch_server_lines(&events[..before_del]).is_empty());
    assert!(events.iter().any(|event| matches!(event,
            Event::Wire { direction: WireDirection::Received, line, .. } if line.contains("BATCH +H1"))));

    let events = run_fixture(plain_config(port, options), |events| {
        replay_flags(events).len() == 1
    });
    server.join().unwrap();
    assert_eq!(
        replay_flags(&events),
        owned(&[("alice: stale reference", false)])
    );
}

#[test]
fn unsolicited_history_batches_keep_their_compatibility_handling() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let initial: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
        assert_eq!(initial[0], "CAP END", "batch off: nothing is negotiated");
        socket
            .write_all(b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket.write_all(b":alice!u@h JOIN #test\r\n").unwrap();
        socket.write_all(BATCH_TRAFFIC).unwrap();
        let _ = read_client_line(&mut lines);
    });
    let events = run_fixture(plain_config(port, Ircv3Options::default()), |events| {
        replay_flags(events).len() == 6
    });
    server.join().unwrap();
    // Same classification as before the option existed.
    assert_eq!(
        replay_flags(&events),
        [
            ("alice: old", true),
            ("alice: live between", false),
            ("alice: nested old", true),
            ("alice: unknown type", false),
            ("alice: other case", false),
            ("alice: after end", false),
        ]
        .map(|(text, replayed)| (text.to_owned(), replayed))
    );
    // Unsolicited framing lines still reach the server log as before.
    assert_eq!(
        batch_server_lines(&events),
        [
            ":srv BATCH +H1 CHATHISTORY #test",
            ":srv BATCH +n1 EXAMPLE.COM/NESTED",
            ":srv BATCH +u1 EXAMPLE.COM/UNKNOWN",
            ":srv BATCH -n1",
            ":srv BATCH -u1",
            ":srv BATCH -H1",
        ]
    );
}

fn avatar_events(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::UserAvatar { nickname, url } => {
                Some(format!("{nickname}={}", url.as_deref().unwrap_or("-")))
            }
            Event::AvatarMoved { from, to } => Some(format!("{from}->{to}")),
            Event::AvatarsReset => Some("reset".into()),
            _ => None,
        })
        .collect()
}

#[test]
fn metadata_avatars_follow_subscription_updates_and_withdrawal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        assert_eq!(read_client_line(&mut lines), "CAP LS 302");
        socket
            .write_all(b":srv CAP * LS :batch draft/metadata-2=max-subs=10 metadata-notify\r\n")
            .unwrap();
        let requests: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
        assert!(
            requests.contains(&"CAP REQ batch".to_owned()),
            "{requests:?}"
        );
        assert!(
            !requests.iter().any(|line| line.contains("metadata")),
            "metadata waits for batch: {requests:?}"
        );
        socket.write_all(b":srv CAP * ACK :batch\r\n").unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP REQ draft/metadata-2");
        socket
            .write_all(b":srv CAP * ACK :draft/metadata-2\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP END");
        // Our own metadata arrives in the registration burst.
        socket
            .write_all(
                b":srv 001 alice :Welcome\r\n\
:srv BATCH +r metadata alice\r\n\
@batch=r :srv METADATA alice avatar * :https://example.com/me.png\r\n\
:srv BATCH -r\r\n\
:srv 376 alice :End\r\n",
            )
            .unwrap();
        // The subscription goes out at 001, before the configured JOIN.
        assert_eq!(read_client_line(&mut lines), "METADATA * SUB avatar");
        assert_eq!(read_client_line(&mut lines), "METADATA * GET avatar");
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket
            .write_all(
                b":srv 770 alice avatar\r\n\
:alice!u@h JOIN #test\r\n\
:srv 353 alice = #test :alice bob @carol\r\n\
:srv 366 alice #test :End\r\n\
:srv BATCH +m metadata #test\r\n\
@batch=m :srv METADATA bob avatar * :https://example.com/bob/{size}\r\n\
@batch=m :srv METADATA carol display-name * :Carol\r\n\
@batch=m :srv METADATA #test avatar * :https://example.com/room.png\r\n\
:srv BATCH -m\r\n\
:srv 774 alice #test 1\r\n",
            )
            .unwrap();
        // After the server's delay the client asks once.
        assert_eq!(read_client_line(&mut lines), "METADATA #test SYNC");
        socket
            .write_all(
                b":srv BATCH +s metadata #test\r\n\
@batch=s :srv METADATA carol avatar * :https://example.com/c.png\r\n\
:srv BATCH -s\r\n\
:bob!u@h METADATA bob avatar * :https://example.com/bob2.png\r\n\
:bob!u@h NICK bobby\r\n\
:carol!u@h PART #test :bye\r\n\
:bobby!u@h QUIT :gone\r\n\
:srv CAP alice DEL :batch\r\n",
            )
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP REQ -draft/metadata-2");
        socket
            .write_all(
                b":srv CAP alice ACK :-draft/metadata-2\r\n\
:srv METADATA dave avatar * :https://example.com/late.png\r\n\
:dave!u@h PRIVMSG #test :after\r\n",
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let options = Ircv3Options {
        batch: true,
        metadata: true,
        ..Ircv3Options::default()
    };
    let events = run_fixture(plain_config(port, options), |events| {
        channel_messages(events).len() == 1
    });
    server.join().unwrap();
    assert_eq!(
        avatar_events(&events),
        [
            "alice=https://example.com/me.png",
            "bob=https://example.com/bob/{size}",
            "carol=https://example.com/c.png",
            "bob=https://example.com/bob2.png",
            "bob->bobby",
            "carol=-",
            "bobby=-",
            "reset",
        ]
    );
    // Metadata is neither chat nor server log; only the transcript has it.
    assert_eq!(channel_messages(&events).len(), 1);
    let reset = events
        .iter()
        .position(|event| matches!(event, Event::AvatarsReset))
        .unwrap();
    for event in &events[..reset] {
        if let Event::ServerLine(line) = event {
            assert!(
                !line.contains("METADATA") && !line.contains(" 770 ") && !line.contains(" 774 "),
                "{line}"
            );
        }
    }
    // After withdrawal a METADATA line is ordinary server traffic again.
    assert!(events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("late.png"))));
}

#[test]
fn kvirc_peer_avatars_are_exchanged_by_url_only() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // The peer's lines are what KVIrc 5.2 sends (see peer_avatar.rs).
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        // No capability is needed.
        assert_eq!(read_client_line(&mut lines), "CAP END");
        assert_eq!(read_client_line(&mut lines), "NICK alice");
        let user = read_client_line(&mut lines);
        assert!(user.starts_with("USER alice 0 * "), "{user:?}");
        assert!(user.ends_with("\u{3}4\u{f}CayenChat"), "{user:?}");
        socket
            .write_all(b":srv.example 001 alice :Welcome\r\n:srv.example 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        // NAMES has no realname: nobody is asked about until they speak.
        socket
            .write_all(
                b":alice!u@h JOIN #test\r\n\
:srv.example 353 alice = #test :alice kv @bob carol\r\n\
:srv.example 366 alice #test :End\r\n\
:kv!u@h PRIVMSG #test :hello\r\n\
:kv!u@h PRIVMSG alice :\x01AVATAR\x01\r\n",
            )
            .unwrap();
        let mut sent = [read_client_line(&mut lines), read_client_line(&mut lines)];
        sent.sort();
        assert_eq!(
            sent,
            [
                "NOTICE kv :\u{1}AVATAR https://example.com/alice.png\u{1}".to_owned(),
                "WHO kv".to_owned(),
            ]
        );
        socket
            .write_all(
                b":srv.example 352 alice #test ~kv host srv.example kv H :0 \x034\x0fKVIrc 5.2\r\n\
:srv.example 315 alice kv :End of /WHO list.\r\n",
            )
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "PRIVMSG kv \u{1}AVATAR\u{1}");
        socket
            .write_all(
                b":kv!u@h NOTICE alice :\x01AVATAR https://example.com/kv.png M\x01\r\n\
:bob!u@h NOTICE #test :\x01AVATAR bob.png 20480\x01\r\n\
:carol!u@h NOTICE alice :\x01AVATAR\x01\r\n\
:bob!u@h PRIVMSG #test :after\r\n",
            )
            .unwrap();
        // A file offer is never fetched by DCC.
        let next = read_client_line(&mut lines);
        assert!(
            !next.contains("DCC") && !next.contains("bob.png"),
            "{next:?}"
        );
    });
    let mut config = plain_config(
        port,
        Ircv3Options {
            peer_avatars: true,
            ..Ircv3Options::default()
        },
    );
    config.shared_avatar = Some("https://example.com/alice.png".into());
    let events = run_fixture(config, |events| {
        channel_messages(events).len() == 2 && !avatar_events(events).is_empty()
    });
    server.join().unwrap();
    assert_eq!(avatar_events(&events), ["kv=https://example.com/kv.png"]);
    assert_eq!(
        channel_messages(&events)
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>(),
        ["hello", "after"]
    );
    // Protocol traffic is neither chat nor server log.
    for event in &events {
        match event {
            Event::PrivateMessage { text, .. } => panic!("private row: {text:?}"),
            Event::ServerLine(line) => assert!(
                !line.contains("AVATAR") && !line.contains(" 352 ") && !line.contains(" 315 "),
                "{line}"
            ),
            _ => {}
        }
    }
}

#[test]
fn ctcp_requests_are_answered_privately_and_shown_readably() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        assert_eq!(read_client_line(&mut lines), "CAP END");
        assert_eq!(read_client_line(&mut lines), "NICK alice");
        read_client_line(&mut lines);
        socket
            .write_all(b":srv.example 001 alice :Welcome\r\n:srv.example 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket
            .write_all(
                b":alice!u@h JOIN #test\r\n\
:srv.example 353 alice = #test :alice bob carol\r\n\
:srv.example 366 alice #test :End\r\n\
:bob!u@h PRIVMSG alice :\x01VERSION\x01\r\n\
:carol!u@h PRIVMSG #test :\x01TIME\x01\r\n\
:bob!u@h PRIVMSG alice :\x01PING 1727490000\x01\r\n\
:bob!u@h PRIVMSG #test :\x01ACTION waves\x01\r\n\
:bob!u@h PRIVMSG #test :hello\r\n",
            )
            .unwrap();
        assert_eq!(
            read_client_line(&mut lines),
            concat!(
                "NOTICE bob :\u{1}VERSION CayenChat ",
                env!("CARGO_PKG_VERSION"),
                "\u{1}"
            )
        );
        assert_eq!(
            read_client_line(&mut lines),
            "NOTICE bob :\u{1}PING 1727490000\u{1}"
        );
        // The channel request is not answered.
        let next = read_client_line(&mut lines);
        assert!(next.is_empty() || next.starts_with("QUIT"), "{next:?}");
    });
    let config = plain_config(port, Ircv3Options::default());
    let events = run_fixture(config, |events| channel_messages(events).len() == 2);
    server.join().unwrap();
    assert_eq!(
        channel_messages(&events)
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>(),
        ["\u{1}ACTION waves\u{1}", "hello"]
    );
    let shown: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::ServerLine(line) if line.contains("CTCP") => Some(line.as_str()),
            Event::PrivateMessage { text, .. } => panic!("private row: {text:?}"),
            _ => None,
        })
        .collect();
    assert_eq!(
        shown,
        [
            "CTCP VERSION request from bob",
            "CTCP TIME request from carol to #test (not answered)",
            "CTCP PING request from bob",
        ]
    );
}

#[test]
fn peer_avatars_off_leaves_ctcp_avatar_unanswered() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        assert_eq!(read_client_line(&mut lines), "CAP END");
        assert_eq!(read_client_line(&mut lines), "NICK alice");
        // No mark, even with a URL configured.
        let user = read_client_line(&mut lines);
        assert!(user.ends_with(" CayenChat"), "{user:?}");
        socket
            .write_all(b":srv.example 001 alice :Welcome\r\n:srv.example 376 alice :End\r\n")
            .unwrap();
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        socket
            .write_all(
                b":alice!u@h JOIN #test\r\n\
:srv.example 353 alice = #test :alice kv\r\n\
:srv.example 366 alice #test :End\r\n\
:kv!u@h PRIVMSG alice :\x01AVATAR\x01\r\n\
:kv!u@h PRIVMSG #test :hello\r\n",
            )
            .unwrap();
        // Nothing is answered or looked up.
        let next = read_client_line(&mut lines);
        assert!(next.is_empty() || next.starts_with("QUIT"), "{next:?}");
    });
    let mut config = plain_config(port, Ircv3Options::default());
    config.shared_avatar = Some("https://example.com/alice.png".into());
    let events = run_fixture(config, |events| channel_messages(events).len() == 1);
    server.join().unwrap();
    assert!(avatar_events(&events).is_empty());
    assert!(events.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line == "CTCP AVATAR request from kv (not answered)")));
}

#[test]
fn the_configured_realname_is_registered_with_the_avatar_mark_only_on_the_wire() {
    let mut config = plain_config(6667, Ircv3Options::default());
    assert_eq!(
        config.wire_realname(),
        "CayenChat",
        "unset keeps the default"
    );
    config.realname = "   ".into();
    assert_eq!(config.wire_realname(), "CayenChat", "blank is unset");
    config.realname = " Alice Liddell ".into();
    assert_eq!(config.wire_realname(), "Alice Liddell");
    config.ircv3.peer_avatars = true;
    config.shared_avatar = Some("https://example.com/a.png".into());
    assert_eq!(config.wire_realname(), "\u{3}4\u{f}Alice Liddell");
    assert_eq!(
        config.realname, " Alice Liddell ",
        "the setting stays clean"
    );
    assert_eq!(
        peer_avatar::without_mark(&config.wire_realname()),
        "Alice Liddell"
    );
    assert!(config.validate().is_ok());
    config.encoding = "ISO-2022-JP".into();
    assert!(
        !config.advertises_avatar(),
        "SI does not survive ISO-2022-JP"
    );
    assert_eq!(config.wire_realname(), "Alice Liddell");
    assert!(config.validate().is_ok());
    config.realname = "\\x034\\x03Alice".into();
    assert_eq!(config.wire_realname(), "\\x034\\x03Alice", "sent as typed");
    config.encoding = "UTF-8".into();
    config.realname = "a\r\nQUIT".into();
    assert!(config.validate().is_err());
    assert!(
        Connection::connect(config)
            .err()
            .is_some_and(|error| error.contains("line breaks"))
    );
}

/// Serves registration with the capabilities in `offer`, then runs
/// `script` and quits. Returns the client's USER line.
fn setname_server(
    offer: &'static str,
    script: impl FnOnce(&mut dyn FnMut(&str), &mut BufReader<std::net::TcpStream>) + Send + 'static,
) -> (u16, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut send = |text: &str| socket.write_all(text.as_bytes()).unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP LS 302");
        send(&format!(":srv CAP * LS :{offer}\r\n"));
        let registration: Vec<String> = (0..2).map(|_| read_client_line(&mut lines)).collect();
        let mut user = registration
            .iter()
            .find(|line| line.starts_with("USER "))
            .cloned();
        loop {
            let line = read_client_line(&mut lines);
            if let Some(cap) = line.strip_prefix("CAP REQ ") {
                send(&format!(":srv CAP * ACK :{cap}\r\n"));
            } else if line == "CAP END" {
                break;
            } else if line.starts_with("USER ") {
                user = Some(line);
            }
        }
        send(":srv 001 alice :Welcome\r\n");
        script(&mut send, &mut lines);
        user.expect("USER was sent")
    });
    (port, server)
}

fn wait_for(connection: &mut Connection, want: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match connection.try_recv() {
            Some(event) if want(&event) => return event,
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("event did not arrive");
}

fn setname_options() -> Ircv3Options {
    Ircv3Options {
        server_time: true,
        ..Ircv3Options::default()
    }
}

#[test]
fn setname_changes_the_realname_and_the_server_confirms_it() {
    let (port, server) = setname_server("server-time setname", |send, lines| {
        assert_eq!(
            read_client_line(lines),
            "SETNAME :\u{3}4\u{f}Bob Builder",
            "the avatar mark of this connection is kept"
        );
        send(":alice!u@h SETNAME :\u{3}4\u{f}Bob Builder\r\n");
        // Another user's change is not ours and not shown.
        send(":bob!u@h SETNAME :Other\r\n");
        assert_eq!(read_client_line(lines), "SETNAME \u{3}4\u{f}Again");
        send(":alice!u@h SETNAME :\u{3}4\u{f}Again\r\n");
        let _ = read_client_line(lines);
    });
    let mut config = plain_config(port, setname_options());
    config.realname = "Alice".into();
    config.ircv3.peer_avatars = true;
    config.shared_avatar = Some("https://example.com/a.png".into());
    let mut connection = Connection::connect(config).unwrap();
    wait_for(&mut connection, |event| {
        matches!(event, Event::Registered { .. })
    });
    connection.set_real_name("Bob Builder").unwrap();
    assert_eq!(
        wait_for(&mut connection, |event| matches!(
            event,
            Event::RealNameChanged { .. }
        )),
        Event::RealNameChanged {
            realname: "Bob Builder".into()
        }
    );
    connection.set_real_name("Again").unwrap();
    assert_eq!(
        wait_for(&mut connection, |event| matches!(
            event,
            Event::RealNameChanged { .. }
        )),
        Event::RealNameChanged {
            realname: "Again".into()
        }
    );
    connection.disconnect().unwrap();
    assert_eq!(server.join().unwrap(), "USER alice 0 * \u{3}4\u{f}Alice");
}

#[test]
fn setname_without_the_capability_is_reported_at_once() {
    let (port, server) = setname_server("server-time", |_, lines| {
        // Nothing may reach the server; the next line is the QUIT.
        assert!(read_client_line(lines).starts_with("QUIT"));
    });
    let mut config = plain_config(port, setname_options());
    config.realname = "Alice".into();
    let mut connection = Connection::connect(config).unwrap();
    wait_for(&mut connection, |event| {
        matches!(event, Event::Registered { .. })
    });
    connection.set_real_name("Bob").unwrap();
    assert_eq!(
        wait_for(&mut connection, |event| matches!(
            event,
            Event::RealNameFailed(_)
        )),
        Event::RealNameFailed(RealNameFailure::Unsupported)
    );
    connection.disconnect().unwrap();
    assert_eq!(server.join().unwrap(), "USER alice 0 * Alice");
}

#[test]
fn a_rejected_setname_carries_the_servers_reason() {
    let (port, server) = setname_server("setname", |send, lines| {
        assert_eq!(read_client_line(lines), "SETNAME Bob");
        send("FAIL SETNAME INVALID_REALNAME :Real name is not valid\r\n");
        assert!(read_client_line(lines).starts_with("QUIT"));
    });
    let mut connection = Connection::connect(plain_config(port, setname_options())).unwrap();
    wait_for(&mut connection, |event| {
        matches!(event, Event::Registered { .. })
    });
    assert!(connection.set_real_name("a\nb").is_err());
    connection.set_real_name("Bob").unwrap();
    assert_eq!(
        wait_for(&mut connection, |event| matches!(
            event,
            Event::RealNameFailed(_)
        )),
        Event::RealNameFailed(RealNameFailure::Rejected("Real name is not valid".into()))
    );
    connection.disconnect().unwrap();
    assert_eq!(server.join().unwrap(), "USER alice 0 * CayenChat");
}

#[test]
fn own_avatar_requests_and_later_joiners_follow_the_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // The server answers like Ergo 2.19 does (see spec/development.md).
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut send = |text: &str| socket.write_all(text.as_bytes()).unwrap();
        assert_eq!(read_client_line(&mut lines), "CAP LS 302");
        send(":srv CAP * LS :batch draft/metadata-2=before-connect,max-subs=10\r\n");
        let _: Vec<String> = (0..3).map(|_| read_client_line(&mut lines)).collect();
        send(":srv CAP * ACK :batch\r\n");
        assert_eq!(read_client_line(&mut lines), "CAP REQ draft/metadata-2");
        send(":srv CAP * ACK :draft/metadata-2\r\n");
        assert_eq!(read_client_line(&mut lines), "CAP END");
        send(
            ":srv 001 alice :Welcome\r\n:srv BATCH +r metadata alice\r\n:srv BATCH -r\r\n:srv 376 alice :End\r\n",
        );
        assert_eq!(read_client_line(&mut lines), "METADATA * SUB avatar");
        assert_eq!(read_client_line(&mut lines), "METADATA * GET avatar");
        assert_eq!(read_client_line(&mut lines), "JOIN #test");
        send(
            ":srv 770 alice avatar\r\n:srv 774 * *ALL 0 :Try again later\r\n\
:srv BATCH +g metadata alice\r\n@batch=g :srv 766 alice alice avatar :Key is not set\r\n:srv BATCH -g\r\n\
:alice!u@h JOIN #test\r\n:srv 353 alice = #test :alice bob\r\n:srv 366 alice #test :End\r\n",
        );
        // Publishing, confirmed with the value the server kept.
        assert_eq!(
            read_client_line(&mut lines),
            "METADATA * SET avatar https://example.com/me.png"
        );
        send(":srv 761 alice alice avatar * :https://example.com/me.png\r\n");
        // A user who joins later is looked up after a pause; members
        // from NAMES are not.
        send(":dave!u@h JOIN #test\r\n");
        assert_eq!(read_client_line(&mut lines), "METADATA dave GET avatar");
        send(
            ":srv BATCH +d metadata dave\r\n@batch=d :srv 761 alice dave avatar * :https://example.com/d.png\r\n:srv BATCH -d\r\n",
        );
        // Removal touches only the avatar key.
        assert_eq!(read_client_line(&mut lines), "METADATA * SET avatar");
        send(":srv 766 alice alice avatar :Key deleted\r\n");
        assert_eq!(
            read_client_line(&mut lines),
            "METADATA * SET avatar https://example.com/second.png"
        );
        send("FAIL METADATA INVALID_VALUE avatar :Value is too long\r\n");
        send(":bob!u@h PRIVMSG #test :done\r\n");
        let _ = read_client_line(&mut lines);
    });
    let mut connection = Connection::connect(plain_config(
        port,
        Ircv3Options {
            batch: true,
            metadata: true,
            ..Ircv3Options::default()
        },
    ))
    .unwrap();
    // Refused on this connection until registration and the capability.
    connection
        .set_own_avatar(1, Some("https://example.com/early.png"))
        .unwrap();
    assert!(connection.set_own_avatar(9, Some("bad\r\nQUIT")).is_err());
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = Vec::new();
    let mut names = false;
    let mut ready = false;
    while Instant::now() < deadline {
        let Some(event) = connection.try_recv() else {
            thread::sleep(Duration::from_millis(10));
            continue;
        };
        match &event {
            Event::MetadataReady => ready = true,
            Event::Names { .. } if ready && !names => {
                names = true;
                connection
                    .set_own_avatar(2, Some("https://example.com/me.png"))
                    .unwrap();
            }
            Event::UserAvatar { nickname, .. } if nickname == "dave" => {
                connection.set_own_avatar(3, None).unwrap();
            }
            Event::OwnAvatar {
                request: Some(3), ..
            } => connection
                .set_own_avatar(4, Some("https://example.com/second.png"))
                .unwrap(),
            Event::ChannelMessage { .. } => {
                seen.push(event);
                break;
            }
            _ => {}
        }
        seen.push(event);
    }
    connection.disconnect().unwrap();
    server.join().unwrap();
    let own: Vec<_> = seen
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::OwnAvatar { .. } | Event::OwnAvatarFailed { .. }
            )
        })
        .cloned()
        .collect();
    assert_eq!(
        own,
        [
            Event::OwnAvatarFailed {
                request: 1,
                failure: AvatarRequestFailure::Unavailable
            },
            Event::OwnAvatar {
                url: None,
                request: None
            },
            Event::OwnAvatar {
                url: Some("https://example.com/me.png".into()),
                request: Some(2)
            },
            Event::OwnAvatar {
                url: None,
                request: Some(3)
            },
            Event::OwnAvatarFailed {
                request: 4,
                failure: AvatarRequestFailure::Rejected {
                    code: "INVALID_VALUE".into(),
                    description: "Value is too long".into()
                }
            },
        ]
    );
    assert_eq!(
        avatar_events(&seen),
        [
            "alice=https://example.com/me.png",
            "dave=https://example.com/d.png",
            "alice=-",
        ]
    );
    // Metadata never becomes a chat row or a server-log line.
    assert!(!seen.iter().any(|event| matches!(event,
            Event::ServerLine(line) if line.contains("METADATA") || line.contains(" 76"))));
}

#[test]
fn metadata_without_batch_brings_batch_along_or_stays_off() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        for metadata in [false, true] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut lines = BufReader::new(socket.try_clone().unwrap());
            let first = read_client_line(&mut lines);
            if !metadata {
                // Everything off: the plain registration of old.
                assert_eq!(first, "CAP END");
            } else {
                assert_eq!(first, "CAP LS 302");
                socket
                    .write_all(b":srv CAP * LS :batch draft/metadata-2 server-time\r\n")
                    .unwrap();
                let requests: Vec<String> = (0..4).map(|_| read_client_line(&mut lines)).collect();
                assert!(
                    requests.contains(&"CAP REQ server-time".to_owned()),
                    "{requests:?}"
                );
                assert!(
                    requests.contains(&"CAP REQ batch".to_owned()),
                    "{requests:?}"
                );
                // Batch declined: metadata is never requested alone.
                socket
                    .write_all(b":srv CAP * ACK :server-time\r\n:srv CAP * NAK :batch\r\n")
                    .unwrap();
                assert_eq!(read_client_line(&mut lines), "CAP END");
            }
            socket
                .write_all(
                    b":srv 001 alice :Welcome\r\n:srv 376 alice :End\r\n\
:srv METADATA bob avatar * :https://example.com/b.png\r\n\
:bob!u@h PRIVMSG #test :hi\r\n",
                )
                .unwrap();
            loop {
                let next = read_client_line(&mut lines);
                assert!(!next.starts_with("METADATA"), "{next}");
                if next == "JOIN #test" {
                    break;
                }
            }
            let _ = read_client_line(&mut lines);
        }
    });
    for options in [
        Ircv3Options::default(),
        // Metadata with batch off: batch is asked for as its
        // prerequisite; declined, so no metadata either.
        Ircv3Options {
            server_time: true,
            metadata: true,
            ..Ircv3Options::default()
        },
    ] {
        let events = run_fixture(plain_config(port, options), |events| {
            channel_messages(events).len() == 1
        });
        assert!(avatar_events(&events).is_empty());
    }
    server.join().unwrap();
}

#[test]
fn messages_are_limited_by_the_line_the_recipient_gets() {
    let message = |text: &str| Outgoing::Message {
        target: "#test".into(),
        text: text.into(),
        display_text: text.into(),
        notice: false,
        encoder: None,
    };
    // `PRIVMSG #test ` is 14 bytes and CRLF 2. A one-word text goes without
    // the colon, which the server adds when relaying, so it counts as a byte.
    let prefix = relay::OwnMask::default().prefix_bytes("alice", "UTF-8");
    let room = 512 - prefix - 16;
    let exact = "x".repeat(room - 1);
    assert!(validate_outgoing_relayed(&message(&exact), "UTF-8", prefix).is_ok());
    let error =
        validate_outgoing_relayed(&message(&format!("{exact}x")), "UTF-8", prefix).unwrap_err();
    assert!(error.contains("too long"), "{error}");
    // With a space the colon is on the wire already, so the limit is the same.
    let spaced = format!("x {}", "x".repeat(room - 3));
    assert_eq!(spaced.len(), room - 1);
    assert!(validate_outgoing_relayed(&message(&spaced), "UTF-8", prefix).is_ok());
    assert!(validate_outgoing_relayed(&message(&format!("{spaced}x")), "UTF-8", prefix).is_err());
    // NOTICE is a byte shorter than PRIVMSG.
    let notice = |text: &str| Outgoing::Message {
        target: "#test".into(),
        text: text.into(),
        display_text: text.into(),
        notice: true,
        encoder: None,
    };
    assert!(validate_outgoing_relayed(&notice(&"x".repeat(room)), "UTF-8", prefix).is_ok());
    assert!(validate_outgoing_relayed(&notice(&"x".repeat(room + 1)), "UTF-8", prefix).is_err());
    // Without the prefix only the command itself is limited.
    assert!(validate_outgoing(&message(&format!("{exact}x")), "UTF-8").is_ok());
    assert!(validate_outgoing(&message(&"x".repeat(495)), "UTF-8").is_ok());
    assert!(validate_outgoing(&message(&"x".repeat(496)), "UTF-8").is_err());

    // A known, shorter mask leaves more room.
    let mut mask = relay::OwnMask::default();
    mask.observe(&":alice!u@h.example JOIN #test".parse().unwrap(), "alice");
    let known = mask.prefix_bytes("alice", "UTF-8");
    assert!(known < prefix);
    let longer = "x".repeat(512 - known - 17);
    assert!(validate_outgoing_relayed(&message(&longer), "UTF-8", known).is_ok());
    assert!(validate_outgoing_relayed(&message(&longer), "UTF-8", prefix).is_err());

    // ISO-2022-JP counts the converted text: each kana takes 2 bytes plus
    // the escape sequences, fewer than UTF-8's 3.
    let kana = "あ".repeat(150);
    assert!(validate_outgoing_relayed(&message(&kana), "ISO-2022-JP", prefix).is_ok());
    assert!(validate_outgoing_relayed(&message(&kana), "UTF-8", prefix).is_err());
}

#[test]
fn a_queued_message_keeps_the_encoding_it_was_accepted_with() {
    let kana = "あ".repeat(145);
    let charsets = ChannelCharsets::new(&[ChannelEncoding {
        channel: "#test".into(),
        decode: None,
        encode: Some("Shift_JIS".into()),
    }])
    .unwrap();
    let prefix = relay::OwnMask::default().prefix_bytes("alice", "UTF-8");
    let accepted = |encoder| Outgoing::Message {
        target: "#test".into(),
        text: kana.clone(),
        display_text: kana.clone(),
        notice: false,
        encoder,
    };
    let live = |outgoing: &Outgoing| {
        let wire = Wire {
            label: "UTF-8",
            charsets: Some(&charsets),
        };
        validate_outgoing_relayed(outgoing, wire, prefix)
    };
    // Accepted as Shift_JIS (290 bytes), then the channel goes back to the
    // server's UTF-8: the message is still judged, and written, as accepted.
    let pinned = accepted(charsets.encoder("#test"));
    charsets.set("#test", None, None).unwrap();
    assert!(live(&pinned).is_ok());
    // Accepted as UTF-8 (435 bytes), then Shift_JIS is set: still too long.
    charsets.set("#test", None, Some("Shift_JIS")).unwrap();
    assert!(live(&accepted(None)).is_err());
}

#[test]
fn a_replayed_line_with_an_old_mask_does_not_change_the_prefix() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let long_host = "h".repeat(100);
    let host = long_host.clone();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(
            &mut socket,
            &mut lines,
            "echo-message labeled-response batch message-tags server-time",
            "NETWORK=t",
        );
        // The host we have now, then history and playback written with an
        // old, short mask of ours, then live traffic to wait for.
        socket
            .write_all(
                format!(
                    ":srv 396 alice {host} :is now your displayed host\r\n\
                     :srv BATCH +h chathistory #test\r\n\
                     @batch=h :alice!old@short PRIVMSG #test :earlier\r\n\
                     :srv BATCH -h\r\n\
                     :srv BATCH +p znc.in/playback\r\n\
                     @batch=p :alice!old@short PRIVMSG #test :played\r\n\
                     :srv BATCH -p\r\n\
                     :bob!b@c PRIVMSG #test :live\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let mut connection = Connection::connect(plain_config(port, sending_options())).unwrap();
    wait_for(&mut connection, |e| matches!(e, Event::Joined { .. }));
    wait_for(
        &mut connection,
        |e| matches!(e, Event::ChannelMessage { text, .. } if text == "live"),
    );
    let prefix = relay::published(&connection.relay_prefix);
    assert_eq!(prefix, 4 + "alice".len() + "u".len() + long_host.len());
    connection.disconnect().unwrap();
    server.join().unwrap();
}

#[test]
fn channel_keys_stay_out_of_the_transcript_and_mode_activity() {
    let parse = |line: &str| line.parse::<IrcMessage>().unwrap();
    for (line, expected) in [
        (
            ":op!u@h MODE #chan +k review-key",
            ":op!u@h MODE #chan +k [redacted]",
        ),
        (
            ":op!u@h MODE #chan +ok bob review-key",
            ":op!u@h MODE #chan +o+k [redacted] [redacted]",
        ),
        (
            ":op!u@h MODE #chan -k review-key",
            ":op!u@h MODE #chan -k [redacted]",
        ),
        (
            ":srv 324 me #chan +nk review-key",
            ":srv 324 me #chan +nk [redacted]",
        ),
        (":op!u@h MODE #chan +o bob", ":op!u@h MODE #chan +o bob"),
    ] {
        let shown = redacted_wire_line(&parse(line));
        assert_eq!(shown, expected, "{line}");
        assert!(!shown.contains("review-key"), "{line}");
    }
    let sent = IrcMessage::from(IrcCommand::Raw(
        "MODE".into(),
        vec!["#chan".into(), "+k".into(), "review-key".into()],
    ));
    assert_eq!(redacted_wire_line(&sent), "MODE #chan +k [redacted]");
    let modes = [
        Mode::Plus(ChannelMode::Oper, Some("bob".into())),
        Mode::Plus(ChannelMode::Key, Some("review-key".into())),
    ];
    let shown: Vec<_> = redact_key_modes(&modes)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(shown, ["+o [redacted]", "+k [redacted]"]);
}

#[test]
fn received_channel_keys_reach_no_event() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(&mut socket, &mut lines, "message-tags", "NETWORK=t");
        socket
            .write_all(
                b":alice!u@h JOIN #test\r\n\
                  :op!u@h MODE #test +k review-key\r\n\
                  :op!u@h MODE #test -l+k review-key\r\n\
                  :op!u@h MODE #test +ok bob review-key\r\n\
                  :op!u@h MODE #test -k review-key\r\n\
                  :srv 324 alice #test +nk review-key\r\n\
                  :srv NOTICE alice :done\r\n",
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let events = run_fixture(plain_config(port, sending_options()), |events| {
        events
            .iter()
            .any(|event| matches!(event, Event::PrivateMessage { text, .. } if text == "done"))
    });
    server.join().unwrap();
    for event in &events {
        assert!(!format!("{event:?}").contains("review-key"), "{event:?}");
    }
    assert!(events.iter().any(|event| matches!(event,
        Event::ServerLine(line) if line == ":srv 324 alice #test +nk [redacted]")));
    assert!(events.iter().any(|event| matches!(event,
        Event::ChannelActivity { kind: ChannelActivityKind::ModeChanged { modes, .. }, .. }
            if modes.contains("[redacted]"))));
}

fn privileges_of(line: &str) -> Vec<String> {
    let message: IrcMessage = line.parse().unwrap();
    let IrcCommand::ChannelMODE(_, modes) = message.command else {
        panic!("not a channel MODE: {line}");
    };
    privilege_changes(&modes)
        .iter()
        .map(|change| {
            let sign = if change.granted { '+' } else { '-' };
            format!("{sign}{} {}", change.mode, change.target)
        })
        .collect()
}

#[test]
fn privilege_changes_pair_arguments_with_their_modes() {
    assert_eq!(privileges_of("MODE #c +o alice"), ["+o alice"]);
    assert_eq!(
        privileges_of("MODE #c +ov-v alice bob carol"),
        ["+o alice", "+v bob", "-v carol"]
    );
    // `-l` takes no argument, though the parser gives it one.
    assert_eq!(
        privileges_of("MODE #c -l+ov alice bob"),
        ["+o alice", "+v bob"]
    );
    assert_eq!(privileges_of("MODE #c +nt-o alice"), ["-o alice"]);
    // Keys, lists and limits are paired but not kept.
    assert_eq!(
        privileges_of("MODE #c +bok *!*@h alice"),
        Vec::<String>::new()
    );
    assert_eq!(
        privileges_of("MODE #c +kov key alice bob"),
        ["+o alice", "+v bob"]
    );
    assert_eq!(privileges_of("MODE #c +lo 5 alice"), ["+o alice"]);
}

#[test]
fn privilege_changes_are_dropped_when_pairing_is_uncertain() {
    // Unknown letters have an unknown argument count.
    assert_eq!(
        privileges_of("MODE #c +Xob private-mask alice *!*@h"),
        Vec::<String>::new()
    );
    // `q` is a quiet list on some servers, `h` and `a` differ per server.
    assert_eq!(
        privileges_of("MODE #c +q *!*@quiet.invalid"),
        Vec::<String>::new()
    );
    assert_eq!(
        privileges_of("MODE #c +hao alice bob carol"),
        Vec::<String>::new()
    );
    // A missing argument, a leftover one, or a sign-less string.
    assert_eq!(privileges_of("MODE #c +ov alice"), Vec::<String>::new());
    assert_eq!(privileges_of("MODE #c o alice"), Vec::<String>::new());
    assert_eq!(privileges_of("MODE #c +s"), Vec::<String>::new());
}

#[test]
fn channel_mode_in_a_history_batch_is_marked_replayed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        register_with(&mut socket, &mut lines, "batch message-tags", "NETWORK=t");
        socket
            .write_all(
                b":alice!u@h JOIN #test\r\n\
                  :srv BATCH +past znc.in/playback #test\r\n\
                  @batch=past :op!u@h MODE #test +o old\r\n\
                  :srv BATCH -past\r\n\
                  :op!u@h MODE #test +o live\r\n\
                  :srv MODE #test +v svc\r\n\
                  :srv NOTICE alice :done\r\n",
            )
            .unwrap();
        let _ = read_client_line(&mut lines);
    });
    let events = run_fixture(plain_config(port, sending_options()), |events| {
        events
            .iter()
            .any(|event| matches!(event, Event::PrivateMessage { text, .. } if text == "done"))
    });
    server.join().unwrap();
    let seen: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::ChannelActivity {
                kind:
                    ChannelActivityKind::ModeChanged {
                        privileges,
                        replayed,
                        ..
                    },
                ..
            } => Some((privileges[0].target.as_str(), *replayed)),
            _ => None,
        })
        .collect();
    assert_eq!(seen, [("old", true), ("live", false), ("svc", false)]);
}

/// Messages accepted in a channel's Shift_JIS before the server announces
/// `UTF8ONLY` (the worker is busy delivering events) are never written in it.
#[test]
fn utf8_only_announced_after_acceptance_keeps_legacy_bytes_off_the_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (flooded, flood_sent) = std_mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        read_line_starting(&mut reader, b"USER ");
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        for index in 0..EVENT_CAPACITY * 2 {
            socket
                .write_all(format!(":server NOTICE alice :flood {index}\r\n").as_bytes())
                .unwrap();
        }
        socket
            .write_all(b":server 005 alice UTF8ONLY :are supported\r\n")
            .unwrap();
        flooded.send(()).unwrap();
        let mut sent = Vec::new();
        loop {
            let line = read_line_starting(&mut reader, b"PRIVMSG");
            if line.ends_with(b"end\r\n") {
                return sent;
            }
            sent.push(line);
        }
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = port;
    config.use_tls = false;
    config.channel_encodings = vec![ChannelEncoding {
        channel: "#jp".into(),
        decode: None,
        encode: Some("Shift_JIS".into()),
    }];
    let mut connection = Connection::connect(config).unwrap();
    flood_sent.recv_timeout(Duration::from_secs(10)).unwrap();
    // Events are not read, so the worker has not seen UTF8ONLY yet.
    thread::sleep(Duration::from_millis(500));
    assert!(!connection.utf8_only());
    for _ in 0..60 {
        connection.send_message("#jp", "あ", false).unwrap();
    }
    // The worker handles UTF8ONLY and the queued messages in either order:
    // what it accepted before the announcement may go out as accepted,
    // nothing after it may.
    let mut announced = false;
    let mut sent_end = false;
    let mut accepted = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "events did not arrive");
        match connection.try_recv() {
            Some(Event::Diagnostic { message, .. }) if message.contains("UTF8ONLY") => {
                announced = true;
            }
            Some(Event::OutgoingAccepted { text, .. }) if text == "end" => break,
            Some(Event::OutgoingAccepted { .. }) => {
                assert!(!announced, "accepted after UTF8ONLY");
                accepted += 1;
            }
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(10)),
        }
        if announced && !sent_end {
            sent_end = true;
            connection.send_message("#jp", "end", false).unwrap();
        }
    }
    let sent = server.join().unwrap();
    assert_eq!(sent.len(), accepted, "{sent:?}");
    assert!(sent.iter().all(|line| line.contains(&0x82)));
}

#[test]
fn channel_keys_are_sent_with_the_join_and_475_is_told_apart() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap()).lines();
        while !lines.next().unwrap().unwrap().starts_with("USER ") {}
        socket
            .write_all(b":server 001 alice :Welcome\r\n:server 376 alice :End of MOTD\r\n")
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "JOIN #secret old-key");
        socket
            .write_all(b":server 475 alice #secret :Cannot join channel (+k)\r\n:server 471 alice #full :Cannot join channel (+l)\r\n")
            .unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "JOIN #secret new-key");
        socket.write_all(b":alice!u@h JOIN #secret\r\n").unwrap();
    });
    let mut config =
        ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec!["#secret".into()]);
    config.port = port;
    config.use_tls = false;
    config
        .channel_keys
        .insert("#secret".into(), "old-key".into());
    assert!(!format!("{config:?}").contains("old-key"));
    let mut connection = Connection::connect(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut refused = Vec::new();
    let mut retried = false;
    let mut joined = false;
    while Instant::now() < deadline && !joined {
        match connection.try_recv() {
            Some(Event::ChannelActivity {
                channel,
                kind: ChannelActivityKind::JoinFailed { bad_key, .. },
                ..
            }) => {
                refused.push((channel, bad_key));
                if refused.len() == 2 && !retried {
                    retried = true;
                    connection.join_channel("#secret", Some("new-key")).unwrap();
                }
            }
            Some(Event::Joined { channel }) => joined = channel == "#secret",
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    assert!(joined);
    assert_eq!(
        refused,
        [("#secret".to_owned(), true), ("#full".to_owned(), false)]
    );
    server.join().unwrap();
}

#[test]
fn unusable_channel_keys_are_refused() {
    let mut config = ConnectionConfig::tls("irc.example.org".into(), "alice".into(), Vec::new());
    for key in ["", "has space", "a,b", "tab\t", ":lead", "nl\n"] {
        config.channel_keys.insert("#c".into(), key.into());
        assert!(config.validate().is_err(), "{key:?}");
    }
    config.channel_keys.insert("#c".into(), "fine:key".into());
    assert_eq!(config.validate(), Ok(()));
    assert!(!valid_channel_key("x y"));
    assert!(valid_channel_key("p@ss-w0rd"));
}

/// Events until `found` accepts one, panicking on a disconnect it did not ask for.
fn wait_for_event<T>(
    connection: &mut Connection,
    what: &str,
    mut found: impl FnMut(&Event) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "no {what}");
        match connection.try_recv() {
            Some(event) => {
                if let Some(value) = found(&event) {
                    return value;
                }
                if let Event::Disconnected(reason) = event {
                    panic!("unexpected disconnect while waiting for {what}: {reason}");
                }
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn alive_check_connection(port: u16) -> Connection {
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = port;
    config.use_tls = false;
    Connection::connect(config).unwrap()
}

/// A server that answers the check keeps the connection, and what was typed
/// meanwhile is written after the answer, not before.
#[test]
fn an_answered_alive_check_keeps_the_connection_and_releases_held_commands() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (pinged, ping_seen) = std_mpsc::channel();
    let (typed, wait_typed) = std_mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let mut reader = registered_server(listener, b"");
        let mut socket = reader.get_ref().try_clone().unwrap();
        let ping = read_line_starting(&mut reader, b"PING ");
        assert!(ping.starts_with(b"PING cayenchat-wake-1"), "{ping:?}");
        pinged.send(()).unwrap();
        wait_typed.recv().unwrap();
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut line = Vec::new();
        let held = reader.read_until(b'\n', &mut line);
        assert!(held.is_err(), "written before the answer: {line:?}");
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .write_all(b":server PONG server :cayenchat-wake-1\r\n")
            .unwrap();
        let sent = read_line_starting(&mut reader, b"PRIVMSG ");
        assert_eq!(sent, b"PRIVMSG #c held\r\n");
    });
    let mut connection = alive_check_connection(port);
    wait_for_event(&mut connection, "registration", |event| {
        matches!(event, Event::Registered { .. }).then_some(())
    });
    connection.check_alive().unwrap();
    ping_seen.recv_timeout(Duration::from_secs(5)).unwrap();
    connection.send_message("#c", "held", false).unwrap();
    typed.send(()).unwrap();
    wait_for_event(&mut connection, "held message", |event| {
        matches!(event, Event::OutgoingAccepted { text, .. } if text == "held").then_some(())
    });
    server.join().unwrap();
}

/// A second request during a check neither sends another PING nor extends the
/// deadline; any line from the server answers, and a later check starts anew.
#[test]
fn alive_checks_do_not_overlap_and_any_line_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (pinged, ping_seen) = std_mpsc::channel();
    let (again, wait_again) = std_mpsc::channel::<()>();
    let (finished, wait_finished) = std_mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let mut reader = registered_server(listener, b"");
        let mut socket = reader.get_ref().try_clone().unwrap();
        read_line_starting(&mut reader, b"PING cayenchat-wake-1");
        pinged.send(()).unwrap();
        // The duplicate request produces no second PING.
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut line = Vec::new();
        assert!(reader.read_until(b'\n', &mut line).is_err(), "{line:?}");
        socket.write_all(b":bob!u@h PRIVMSG alice :hi\r\n").unwrap();
        wait_again.recv().unwrap();
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        read_line_starting(&mut reader, b"PING cayenchat-wake-2");
        pinged.send(()).unwrap();
        socket.write_all(b":server PONG server :x\r\n").unwrap();
        wait_finished.recv().unwrap();
    });
    let mut connection = alive_check_connection(port);
    wait_for_event(&mut connection, "registration", |event| {
        matches!(event, Event::Registered { .. }).then_some(())
    });
    connection
        .queue_alive_check(Duration::from_secs(2))
        .unwrap();
    connection
        .queue_alive_check(Duration::from_secs(2))
        .unwrap();
    ping_seen.recv_timeout(Duration::from_secs(5)).unwrap();
    wait_for_event(&mut connection, "private message", |event| {
        matches!(event, Event::PrivateMessage { .. }).then_some(())
    });
    // The 2 s deadline has passed by the time the connection is still there.
    thread::sleep(Duration::from_millis(2200));
    assert!(!connection.is_closed());
    connection
        .queue_alive_check(Duration::from_secs(2))
        .unwrap();
    again.send(()).unwrap();
    ping_seen.recv_timeout(Duration::from_secs(5)).unwrap();
    thread::sleep(Duration::from_millis(300));
    while let Some(event) = connection.try_recv() {
        assert!(!matches!(event, Event::Disconnected(_)), "{event:?}");
    }
    finished.send(()).unwrap();
    server.join().unwrap();
}

/// A silent server ends the connection with a reason after the deadline, and
/// what was held is reported instead of being written into the dead link.
#[test]
fn an_unanswered_alive_check_disconnects_and_drops_held_commands() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut reader = registered_server(listener, b"");
        read_line_starting(&mut reader, b"PING cayenchat-wake-1");
        // Nothing more is written to the client; read until it closes.
        let mut rest = Vec::new();
        while reader.read_until(b'\n', &mut rest).unwrap() > 0 {}
        assert!(
            !String::from_utf8_lossy(&rest).contains("PRIVMSG"),
            "{rest:?}"
        );
    });
    let mut connection = alive_check_connection(port);
    wait_for_event(&mut connection, "registration", |event| {
        matches!(event, Event::Registered { .. }).then_some(())
    });
    connection
        .queue_alive_check(Duration::from_millis(300))
        .unwrap();
    connection.send_message("#c", "lost", false).unwrap();
    let mut not_sent = false;
    let reason = loop {
        match connection.try_recv() {
            Some(Event::ServerLine(line)) => not_sent |= line.contains("not sent"),
            Some(Event::Disconnected(reason)) => break reason,
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    assert_eq!(reason, ALIVE_CHECK_FAILED);
    assert!(not_sent);
    server.join().unwrap();
}

/// A request the application accepted still gets its outcome event when the
/// hold is full, and the check keeps the connection once the server answers.
#[test]
fn a_full_hold_still_completes_avatar_and_history_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (refused, wait_refused) = std_mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let mut reader = registered_server(listener, b"");
        let mut socket = reader.get_ref().try_clone().unwrap();
        read_line_starting(&mut reader, b"PING cayenchat-wake-1");
        wait_refused.recv().unwrap();
        socket
            .write_all(b":server PONG server :cayenchat-wake-1\r\n")
            .unwrap();
        for _ in 0..MAX_HELD_COMMANDS {
            read_line_starting(&mut reader, b"PRIVMSG ");
        }
    });
    let mut connection = alive_check_connection(port);
    wait_for_event(&mut connection, "registration", |event| {
        matches!(event, Event::Registered { .. }).then_some(())
    });
    connection.check_alive().unwrap();
    for _ in 0..MAX_HELD_COMMANDS {
        // The worker drains the queue; a momentarily full one is retried.
        while connection.send_message("#c", "held", false).is_err() {
            thread::sleep(Duration::from_millis(5));
        }
    }
    let reference = MessageReference {
        msgid: Some("m1".into()),
        time: None,
    };
    let (mut avatar, mut history) = (false, false);
    // The worker may still be taking in the messages; keep asking until
    // both requests were queued after the hold filled.
    while connection.set_own_avatar(42, None).is_err() {
        thread::sleep(Duration::from_millis(5));
    }
    while connection
        .request_older_history("#c", 43, reference.clone(), 10)
        .is_err()
    {
        thread::sleep(Duration::from_millis(5));
    }
    while !(avatar && history) {
        match wait_for_event(&mut connection, "request outcome", |event| match event {
            Event::OwnAvatarFailed { request: 42, .. } => Some(1),
            Event::OlderChannelHistory {
                request: 43,
                status: OlderHistoryStatus::Failed,
                ..
            } => Some(2),
            _ => None,
        }) {
            1 => avatar = true,
            _ => history = true,
        }
    }
    refused.send(()).unwrap();
    server.join().unwrap();
    assert!(!connection.is_closed());
}

#[test]
fn sleep_sends_quit_while_the_event_queue_is_full() {
    // More lines than EVENT_CAPACITY, and the test never drains them.
    let started = Instant::now();
    let line = quit_line_after_flood(
        |_| {},
        |connection| {
            thread::sleep(Duration::from_millis(500));
            connection.sleep("Sleeping");
            assert!(connection.wait_closed(Duration::from_millis(1500)));
        },
        EVENT_CAPACITY * 4,
        Duration::ZERO,
    );
    assert_eq!(line, b"QUIT Sleeping".to_vec());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn sleep_closes_an_unregistered_connection_while_the_event_queue_is_full() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut lines = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        while !line.starts_with("USER ") {
            line.clear();
            lines.read_line(&mut line).unwrap();
        }
        // Never sends 001; floods notices instead.
        for _ in 0..EVENT_CAPACITY * 4 {
            if socket.write_all(b":server NOTICE * :wait\r\n").is_err() {
                return;
            }
        }
        // The client closes without QUIT.
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut rest = String::new();
        let mut quit = false;
        while let Ok(n) = lines.read_line(&mut rest) {
            if n == 0 {
                break;
            }
            quit |= rest.starts_with("QUIT ");
            rest.clear();
        }
        assert!(!quit, "QUIT sent before registration");
    });
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), vec![]);
    config.port = port;
    config.use_tls = false;
    let connection = Connection::connect(config).unwrap();
    thread::sleep(Duration::from_millis(500));
    let started = Instant::now();
    connection.sleep("Sleeping");
    assert!(connection.wait_closed(Duration::from_millis(1500)));
    assert!(started.elapsed() < Duration::from_millis(1500));
    server.join().unwrap();
}
