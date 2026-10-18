// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, MemberMenu, Worker};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use gpui::{TestAppContext, point, px};

#[gpui::test]
fn right_click_on_self_closes_the_peer_menu(cx: &mut TestAppContext) {
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
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            network,
            vec![Event::Registered {
                nickname: "alice".into(),
            }],
            Worker::Running,
            cx,
        );
        chat.member_menu = Some(MemberMenu {
            position: point(px(0.), px(0.)),
            network,
            nickname: "bob".into(),
            channel: "bob".into(),
            private: true,
            group: Vec::new(),
            ctcp_open: false,
        });
        // The peer keeps their menu; a channel is never ours to refuse.
        assert!(!chat.right_click_on_self(true, network, "bob"));
        assert!(!chat.right_click_on_self(false, network, "alice"));
        assert!(chat.member_menu.is_some());
        // Our own row closes it.
        assert!(chat.right_click_on_self(true, network, "Alice"));
        assert!(chat.member_menu.is_none());
    });
}

#[gpui::test]
fn ctcp_submenu_opens_and_closes_without_closing_the_menu(cx: &mut TestAppContext) {
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
        chat.member_menu = Some(MemberMenu {
            position: point(px(0.), px(0.)),
            network: NetworkId(1),
            nickname: "bob".into(),
            channel: "#a".into(),
            private: false,
            group: Vec::new(),
            ctcp_open: false,
        });
        chat.set_member_ctcp_open(true, cx);
        assert!(chat.member_menu.as_ref().unwrap().ctcp_open);
        chat.set_member_ctcp_open(false, cx);
        assert!(!chat.member_menu.as_ref().unwrap().ctcp_open);
    });
    assert_eq!(
        super::MEMBER_CTCP_ITEMS.map(|(_, tag)| tag),
        ["PING", "TIME", "VERSION", "USERINFO", "CLIENTINFO"]
    );
}
