// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, ToggleToolbar, Worker, app_menus};
use crate::localization::Localizer;
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use cayenchat_storage::Language;
use gpui::{Action, MenuItem, TestAppContext};

fn view_menu_label(toolbar_shown: bool) -> String {
    let menus = app_menus(
        false,
        toolbar_shown,
        false,
        &Localizer::new(Language::English),
    );
    menus
        .iter()
        .flat_map(|menu| &menu.items)
        .find_map(|item| match item {
            MenuItem::Action { name, action, .. } if action.name() == ToggleToolbar.name() => {
                Some(name.to_string())
            }
            _ => None,
        })
        .expect("the View menu offers the toolbar")
}

#[test]
fn the_view_menu_offers_to_show_or_hide_the_toolbar() {
    assert_eq!(view_menu_label(true), "Hide Toolbar");
    assert_eq!(view_menu_label(false), "Show Toolbar");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn the_view_menu_offers_the_menu_bar_apart_from_the_toolbar() {
    let label = |menu_bar_hidden, toolbar_shown| {
        let menus = app_menus(
            false,
            toolbar_shown,
            menu_bar_hidden,
            &Localizer::new(Language::English),
        );
        menus
            .iter()
            .flat_map(|menu| &menu.items)
            .find_map(|item| match item {
                MenuItem::Action { name, action, .. }
                    if action.name() == super::ToggleMenuBar.name() =>
                {
                    Some(name.to_string())
                }
                _ => None,
            })
            .expect("the View menu offers the menu bar")
    };
    assert_eq!(label(false, true), "Hide Menu Bar");
    assert_eq!(label(true, true), "Show Menu Bar");
    assert_eq!(label(false, false), "Hide Menu Bar");
    assert_eq!(label(true, false), "Show Menu Bar");
}

#[gpui::test]
fn buttons_follow_the_connection_and_the_selection(cx: &mut TestAppContext) {
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
        let state = chat.toolbar_state();
        assert!(
            !state.connected
                && !state.can_join_or_part
                && !state.can_whois
                && !state.can_topic
                && !state.can_mode
                && !state.can_nick
        );
        let events =
            |chat: &mut ChatWindow, events: Vec<Event>, cx: &mut gpui::Context<ChatWindow>| {
                chat.handle_events(NetworkId(1), events, Worker::Running, cx);
            };
        events(
            chat,
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
            ],
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        let state = chat.toolbar_state();
        assert!(
            state.can_join_or_part
                && state.joined
                && state.can_topic
                && state.can_mode
                && state.can_nick
        );
        // WHOIS needs one chosen member.
        assert!(!state.can_whois);

        // The same button follows every way of leaving and of coming back.
        let joined = |chat: &ChatWindow| chat.toolbar_state().joined;
        events(
            chat,
            vec![Event::Parted {
                channel: "#a".into(),
            }],
            cx,
        );
        assert!(!joined(chat), "after PART");
        events(
            chat,
            vec![Event::Joined {
                channel: "#a".into(),
            }],
            cx,
        );
        assert!(joined(chat), "after JOIN completes");
        // A KICK of ours arrives as a part.
        events(
            chat,
            vec![Event::Parted {
                channel: "#a".into(),
            }],
            cx,
        );
        assert!(!joined(chat), "after KICK");
        events(
            chat,
            vec![Event::Joined {
                channel: "#a".into(),
            }],
            cx,
        );
        events(chat, vec![Event::Disconnected("gone".into())], cx);
        let state = chat.toolbar_state();
        assert!(!state.joined, "after the connection ended");
        assert!(!state.can_join_or_part, "nothing to join through");
        assert!(
            !state.can_topic && !state.can_mode && !state.can_nick,
            "unconnected has none of the three"
        );
        // Reconnecting and joining again (auto-join).
        events(
            chat,
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
            ],
            cx,
        );
        let state = chat.toolbar_state();
        assert!(
            state.can_join_or_part
                && state.joined
                && state.can_topic
                && state.can_mode
                && state.can_nick,
            "after auto-join"
        );

        // Back on the server, the channel buttons have nothing to act on,
        // but the nickname is the server's.
        let server = chat.state.networks()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectServer(server));
        let state = chat.toolbar_state();
        assert!(!state.can_join_or_part && !state.can_topic && !state.can_mode && state.can_nick);
        // Selecting the channel again shows its state.
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        assert!(chat.toolbar_state().joined);
    });
}

#[gpui::test]
fn topic_mode_and_nick_open_the_shared_prompt_from_the_toolbar(cx: &mut TestAppContext) {
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
    // A connection to a local listener that never registers: confirmed
    // commands can be queued on it, nothing reaches a server.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config =
        cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
    config.port = listener.local_addr().unwrap().port();
    config.use_tls = false;
    let connection = cayenchat_irc_core::Connection::connect(config).unwrap();
    chat.update(cx, |chat, cx| {
        chat.sessions.get_mut(&NetworkId(1)).unwrap().irc = Some(connection);
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        chat.state.set_topic(NetworkId(1), "#a", "old topic");
    });
    chat.update_in(cx, |chat, window, cx| {
        // Topic: the current topic, all selected, for the selected channel.
        chat.toolbar_topic(window, cx);
        let prompt = chat.member_prompt.as_ref().unwrap();
        assert_eq!(prompt.nickname, "#a");
        assert_eq!(prompt.input.read(cx).text(), "old topic");
        assert_eq!(
            prompt.position,
            Some(ChatWindow::centered_prompt_position(window))
        );
        // A cancel sends nothing and closes the prompt.
        chat.cancel_member_prompt(window, cx);
        assert!(chat.member_prompt.is_none());
        assert!(chat.notices.all().is_empty());

        // Confirming a topic sends it.
        chat.toolbar_topic(window, cx);
        chat.member_prompt
            .as_ref()
            .unwrap()
            .input
            .update(cx, |input, cx| input.set_text("new topic", cx));
        chat.submit_member_prompt(window, cx);
        assert!(chat.member_prompt.is_none());
        assert!(chat.notices.all().is_empty());

        // Mode: an empty string input, sent as /mode only on confirm.
        chat.toolbar_mode(window, cx);
        let prompt = chat.member_prompt.as_ref().unwrap();
        assert!(prompt.input.read(cx).text().is_empty());
        prompt
            .input
            .update(cx, |input, cx| input.set_text("+m", cx));
        chat.submit_member_prompt(window, cx);
        assert!(chat.member_prompt.is_none());
        assert!(chat.notices.all().is_empty());

        // Nick: the own nickname of the selected channel's network.
        chat.toolbar_nick(window, cx);
        let prompt = chat.member_prompt.as_ref().unwrap();
        assert_eq!(prompt.input.read(cx).text(), "me");
        prompt
            .input
            .update(cx, |input, cx| input.set_text("me2", cx));
        chat.submit_member_prompt(window, cx);
        assert!(chat.member_prompt.is_none());
        assert!(chat.notices.all().is_empty());

        // With a server selected the channel buttons have nothing to act on;
        // their handlers do nothing instead of guessing a channel.
        let server = chat.state.networks()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectServer(server));
        chat.toolbar_topic(window, cx);
        chat.toolbar_mode(window, cx);
        assert!(chat.member_prompt.is_none());
    });
}

#[gpui::test]
fn a_toolbar_click_opens_the_prompt_and_an_unavailable_one_ignores_it(cx: &mut TestAppContext) {
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
    cx.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(700.)));
    cx.run_until_parked();
    // Unregistered on a channel: the topic button is drawn but dimmed, and
    // its click does nothing.
    let channel = chat.read_with(cx, |chat, _| chat.state.conversations()[0].id);
    chat.update(cx, |chat, _| {
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel))
    });
    cx.run_until_parked();
    let topic = cx
        .debug_bounds("toolbar-topic")
        .expect("the topic button is drawn");
    cx.simulate_click(topic.center(), gpui::Modifiers::default());
    chat.read_with(cx, |chat, _| assert!(chat.member_prompt.is_none()));

    // Registered and on the channel: the same click opens the shared prompt.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config =
        cayenchat_irc_core::ConnectionConfig::tls("127.0.0.1".into(), "me".into(), Vec::new());
    config.port = listener.local_addr().unwrap().port();
    config.use_tls = false;
    let connection = cayenchat_irc_core::Connection::connect(config).unwrap();
    chat.update(cx, |chat, cx| {
        chat.sessions.get_mut(&NetworkId(1)).unwrap().irc = Some(connection);
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        chat.state.set_topic(NetworkId(1), "#a", "old topic");
    });
    cx.run_until_parked();
    let topic = cx
        .debug_bounds("toolbar-topic")
        .expect("the topic button is drawn");
    cx.simulate_click(topic.center(), gpui::Modifiers::default());
    let input = chat.read_with(cx, |chat, _| {
        let prompt = chat
            .member_prompt
            .as_ref()
            .expect("the click opened the prompt");
        assert_eq!(prompt.nickname, "#a");
        prompt.input.clone()
    });
    assert_eq!(
        input.read_with(cx, |input, _| input.text().to_owned()),
        "old topic"
    );
    assert!(
        cx.debug_bounds("member-prompt").is_some(),
        "the prompt is drawn"
    );
}

#[gpui::test]
fn the_first_button_follows_a_connection_or_a_pending_retry(cx: &mut TestAppContext) {
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
    chat.update(cx, |chat, _| {
        let idle = chat.toolbar_state();
        assert!(!idle.connected && idle.can_connect && !idle.can_reconnect);
        let session = chat.sessions.get_mut(&NetworkId(1)).expect("a session");
        session.retry_pending = true;
        let waiting = chat.toolbar_state();
        assert!(waiting.connected, "a scheduled retry can be cancelled");
        chat.sessions.get_mut(&NetworkId(1)).unwrap().retry_pending = false;
        assert!(!chat.toolbar_state().connected);
    });
}

#[gpui::test]
fn disconnecting_everything_targets_only_connected_or_retrying_servers(cx: &mut TestAppContext) {
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
    chat.update(cx, |chat, _| {
        assert!(chat.disconnectable_networks().is_empty());
        chat.sessions.get_mut(&NetworkId(1)).unwrap().retry_pending = true;
        assert_eq!(chat.disconnectable_networks(), vec![NetworkId(1)]);
    });
    // With nothing to stop, the long press asks nothing and changes nothing.
    chat.update_in(cx, |chat, window, cx| {
        chat.sessions.get_mut(&NetworkId(1)).unwrap().retry_pending = false;
        chat.disconnect_all_action(window, cx);
        assert!(!chat.sessions[&NetworkId(1)].manual_disconnect);
    });
}

#[gpui::test]
fn the_upload_button_explains_a_missing_service_before_asking_for_a_file(cx: &mut TestAppContext) {
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
    // The test platform has no file dialog and panics if one is opened.
    chat.update_in(cx, |chat, window, cx| chat.toolbar_upload(window, cx));
    assert!(cx.has_pending_prompt(), "the missing service is announced");
}

#[test]
fn the_file_dialog_offers_the_images_the_upload_accepts() {
    let filter = crate::image_upload::image_file_filter(&Localizer::new(Language::English));
    assert_eq!(filter.name.as_ref(), "Images");
    let extensions: Vec<&str> = filter.extensions.iter().map(|e| e.as_ref()).collect();
    assert_eq!(
        extensions,
        cayenchat_model::attachment::ImageFormat::FILE_EXTENSIONS
    );
}

#[gpui::test]
fn double_clicking_a_server_leaves_one_waiting_to_retry_alone(cx: &mut TestAppContext) {
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
    chat.update_in(cx, |chat, window, cx| {
        let session = chat.sessions.get_mut(&NetworkId(1)).unwrap();
        session.retry_pending = true;
        let token = session.retry_token;
        chat.toggle_connection(NetworkId(1), window, cx);
        let session = &chat.sessions[&NetworkId(1)];
        assert!(session.retry_pending && !session.manual_disconnect);
        assert_eq!(session.retry_token, token);
    });
}
