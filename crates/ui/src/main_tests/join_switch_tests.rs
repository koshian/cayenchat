// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use cayenchat_irc_core::{ChannelActivityKind, Event};
use cayenchat_model::NetworkId;
use gpui::{TestAppContext, VisualTestContext};

fn install_globals(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
}

fn joined(channel: &str) -> Event {
    Event::Joined {
        channel: channel.into(),
    }
}

fn events(
    chat: &gpui::Entity<ChatWindow>,
    cx: &mut VisualTestContext,
    network: NetworkId,
    batch: Vec<Event>,
) {
    chat.update(cx, |chat, cx| {
        chat.handle_events(network, batch, Worker::Running, cx);
    });
    cx.run_until_parked();
}

fn typed(
    chat: &gpui::Entity<ChatWindow>,
    cx: &mut VisualTestContext,
    network: NetworkId,
    command: &str,
) {
    chat.update_in(cx, |chat, window, cx| {
        chat.note_join_switch(network, command, window, cx);
    });
}

fn selected_name(chat: &gpui::Entity<ChatWindow>, cx: &mut VisualTestContext) -> Option<String> {
    chat.update(cx, |chat, _| {
        chat.state.selected_channel().map(|c| c.name.clone())
    })
}

fn setup<'a>(
    cx: &'a mut TestAppContext,
    channels: &'a str,
) -> (gpui::Entity<ChatWindow>, &'a mut VisualTestContext) {
    install_globals(cx);
    let settings = crate::settings_with_channels(channels);
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    events(&chat, cx, NetworkId(1), vec![joined("#a")]);
    chat.update(cx, |chat, _| {
        let a = chat.state.channel_id(NetworkId(1), "#a").unwrap();
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(a));
    });
    (chat, cx)
}

#[gpui::test]
fn a_typed_join_selects_the_channel_once_the_join_is_confirmed(cx: &mut TestAppContext) {
    let (chat, cx) = setup(cx, "#a");
    typed(&chat, cx, NetworkId(1), "/join #new");
    // Sending alone changes nothing.
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#a"));
    events(&chat, cx, NetworkId(1), vec![joined("#NEW")]);
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#NEW"));
}

#[gpui::test]
fn a_refused_join_keeps_the_view(cx: &mut TestAppContext) {
    let (chat, cx) = setup(cx, "#a");
    typed(&chat, cx, NetworkId(1), "/join #full");
    events(
        &chat,
        cx,
        NetworkId(1),
        vec![Event::ChannelActivity {
            channel: "#full".into(),
            actor: String::new(),
            kind: ChannelActivityKind::JoinFailed {
                reason: "full".into(),
                bad_key: false,
                banned: false,
            },
            server_time: None,
        }],
    );
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#a"));
    // A later confirmation (say, an auto-join) no longer selects it.
    events(&chat, cx, NetworkId(1), vec![joined("#full")]);
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#a"));
}

#[gpui::test]
fn joins_nobody_asked_for_never_switch(cx: &mut TestAppContext) {
    let (chat, cx) = setup(cx, "#a");
    events(&chat, cx, NetworkId(1), vec![joined("#auto")]);
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#a"));
    // A command that is not a join does not arm anything either.
    typed(&chat, cx, NetworkId(1), "/part #a");
    typed(&chat, cx, NetworkId(1), "hello #b");
    events(&chat, cx, NetworkId(1), vec![joined("#b")]);
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#a"));
}

#[gpui::test]
fn a_join_of_a_channel_already_joined_selects_it_at_once(cx: &mut TestAppContext) {
    let (chat, cx) = setup(cx, "#a,#b");
    events(&chat, cx, NetworkId(1), vec![joined("#b")]);
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#a"));
    typed(&chat, cx, NetworkId(1), "/join #B");
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#b"));
    assert!(chat.update(cx, |chat, _| {
        chat.sessions[&NetworkId(1)].join_switch.is_none()
    }));
}

#[gpui::test]
fn several_channels_end_on_the_last_one_joined(cx: &mut TestAppContext) {
    let (chat, cx) = setup(cx, "#a");
    typed(&chat, cx, NetworkId(1), "/join #x,#y,#z");
    events(&chat, cx, NetworkId(1), vec![joined("#x"), joined("#y")]);
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#y"));
    // #z is refused: the view stays on the last one that worked.
    events(
        &chat,
        cx,
        NetworkId(1),
        vec![Event::ChannelActivity {
            channel: "#z".into(),
            actor: String::new(),
            kind: ChannelActivityKind::JoinFailed {
                reason: "banned".into(),
                bad_key: false,
                banned: false,
            },
            server_time: None,
        }],
    );
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#y"));
    assert!(chat.update(cx, |chat, _| {
        chat.sessions[&NetworkId(1)].join_switch.is_none()
    }));
}

#[gpui::test]
fn a_view_the_user_chose_meanwhile_is_kept(cx: &mut TestAppContext) {
    let (chat, cx) = setup(cx, "#a,#b");
    typed(&chat, cx, NetworkId(1), "/join #new");
    chat.update(cx, |chat, _| {
        let b = chat.state.channel_id(NetworkId(1), "#b").unwrap();
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(b));
    });
    events(&chat, cx, NetworkId(1), vec![joined("#new")]);
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#b"));
}

#[gpui::test]
fn a_reconnect_forgets_the_request(cx: &mut TestAppContext) {
    let (chat, cx) = setup(cx, "#a");
    typed(&chat, cx, NetworkId(1), "/join #new");
    events(
        &chat,
        cx,
        NetworkId(1),
        vec![Event::Registered {
            nickname: "me".into(),
        }],
    );
    events(&chat, cx, NetworkId(1), vec![joined("#new")]);
    assert_eq!(selected_name(&chat, cx).as_deref(), Some("#a"));
}

#[gpui::test]
fn the_same_channel_name_on_another_network_is_not_confused(cx: &mut TestAppContext) {
    install_globals(cx);
    let mut settings = crate::settings_with_channels("#a");
    settings.add_server("irc.example.org").channels = "#a".into();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    events(&chat, cx, NetworkId(1), vec![joined("#a")]);
    events(&chat, cx, NetworkId(2), vec![joined("#a")]);
    let before = chat.update(cx, |chat, _| chat.state.selection());
    typed(&chat, cx, NetworkId(1), "/join #shared");
    // The other network joining a channel of that name is not our request.
    events(&chat, cx, NetworkId(2), vec![joined("#shared")]);
    assert_eq!(chat.update(cx, |chat, _| chat.state.selection()), before);
    events(&chat, cx, NetworkId(1), vec![joined("#shared")]);
    let (network, name) = chat.update(cx, |chat, _| {
        let channel = chat.state.selected_channel().unwrap();
        (channel.network, channel.name.clone())
    });
    assert_eq!((network, name.as_str()), (NetworkId(1), "#shared"));
}
