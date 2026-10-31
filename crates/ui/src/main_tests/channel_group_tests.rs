// SPDX-License-Identifier: GPL-3.0-only
use cayenchat_app::{Command, MoveTo, Parent, Selection};
use gpui::{Entity, TestAppContext, VisualTestContext};

use super::{ChatWindow, TreeRow};

fn open_chat(cx: &mut TestAppContext) -> (Entity<ChatWindow>, &mut VisualTestContext) {
    crate::support::install_test_globals(cx);
    let mut settings = crate::settings_with_channels("#a,#b");
    settings
        .add_server(cayenchat_storage::PRESETS[1].host)
        .channels = "#a,#c".into();
    cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx))
}

fn open_menu(chat: &mut ChatWindow, name: &str, network: u32) -> cayenchat_model::ConversationId {
    let id = chat
        .state
        .channel_id(cayenchat_model::NetworkId(network), name)
        .unwrap();
    chat.channel_menu = Some(crate::ChannelMenu {
        position: gpui::point(gpui::px(0.), gpui::px(0.)),
        network: cayenchat_model::NetworkId(network),
        conversation: id,
        channel: name.into(),
        joined: true,
        private: false,
        grouped: false,
        groups_open: true,
    });
    id
}

fn channel_rows(chat: &ChatWindow) -> Vec<String> {
    chat.tree_rows
        .iter()
        .map(|row| match *row {
            TreeRow::Server(_) => "server".into(),
            TreeRow::Group(..) => "group".into(),
            TreeRow::NewGroup => "new".into(),
            TreeRow::Channel(id, at) => {
                format!("{}@{at}", chat.state.conversation(id).unwrap().name)
            }
        })
        .collect()
}

#[gpui::test]
fn a_new_group_is_named_in_the_tree_and_cancel_changes_nothing(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let a = open_menu(chat, "#a", 1);
        chat.begin_new_group(window, cx);
        // Nothing exists before Enter.
        assert!(chat.state.groups().is_empty());
        assert!(chat.group_edit.is_some());
        chat.cancel_group_edit(window, cx);
        assert!(chat.group_edit.is_none());
        assert!(chat.state.groups().is_empty());
        assert_eq!(
            chat.state.display_parent(a),
            Some(Parent::Server(cayenchat_model::NetworkId(1)))
        );

        open_menu(chat, "#a", 1);
        chat.begin_new_group(window, cx);
        // An empty name keeps the field open.
        chat.commit_group_edit(window, cx);
        assert!(chat.group_edit.is_some());
        chat.group_edit
            .as_ref()
            .unwrap()
            .input
            .update(cx, |input, cx| input.set_text("daily", cx));
        chat.commit_group_edit(window, cx);
        assert!(chat.group_edit.is_none());
        assert_eq!(chat.state.groups()[0].name, "daily");
        assert!(matches!(
            chat.state.display_parent(a),
            Some(Parent::Group(_))
        ));
    });
}

#[gpui::test]
fn groups_mix_servers_and_the_numbers_follow_the_rows(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("channel-order.json");
    chat.update_in(cx, |chat, _, cx| {
        chat.order_file = Some(file.clone());
        let group = chat.state.create_group("daily").unwrap();
        let c = chat
            .state
            .channel_id(cayenchat_model::NetworkId(2), "#c")
            .unwrap();
        let a = chat
            .state
            .channel_id(cayenchat_model::NetworkId(1), "#a")
            .unwrap();
        chat.state.move_to_group(c, group, None);
        chat.state.move_to_group(a, group, None);
        chat.save_channel_layout(None, cx);

        chat.state.dispatch(Command::SelectChannel(c));
        chat.state.dispatch(Command::SelectChannelAt(1));
        assert_eq!(chat.state.selection(), Selection::Channel(a));
        chat.move_conversation(a, MoveTo::First, cx);
        chat.state.dispatch(Command::SelectChannelAt(0));
        assert_eq!(chat.state.selection(), Selection::Channel(a));
    });
    cx.update(|window, _| window.refresh());
    chat.read_with(cx, |chat, _| {
        assert_eq!(
            channel_rows(chat),
            ["group", "#a@0", "#c@1", "server", "#b@0", "server", "#a@0"]
        );
    });
    // A restart reads the same place back, for the channel of either server.
    chat.update(cx, |chat, _| {
        let saved = cayenchat_storage::order::load_orders_from(&file);
        assert_eq!(saved.groups.len(), 1);
        assert_eq!(saved.groups[0].members.len(), 2);
        chat.channel_orders = saved;
        chat.state.set_groups(Vec::new());
        chat.restore_channel_groups();
        let group = &chat.state.groups()[0];
        assert_eq!(group.name, "daily");
        assert_eq!(group.members.len(), 2);
        assert_eq!(group.members[0].key, "#a");
    });
}

#[gpui::test]
fn deleting_a_group_asks_when_it_holds_channels_and_returns_them(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    chat.update_in(cx, |chat, window, cx| {
        let group = chat.state.create_group("g").unwrap();
        let a = chat
            .state
            .channel_id(cayenchat_model::NetworkId(1), "#a")
            .unwrap();
        // An empty group goes at once.
        chat.open_group_menu(group, gpui::point(gpui::px(0.), gpui::px(0.)), window);
        chat.delete_group_from_menu(cx);
        assert!(chat.state.groups().is_empty());

        let group = chat.state.create_group("g").unwrap();
        chat.state.move_to_group(a, group, None);
        chat.open_group_menu(group, gpui::point(gpui::px(0.), gpui::px(0.)), window);
        chat.delete_group_from_menu(cx);
        assert!(chat.group_menu.as_ref().unwrap().confirming);
        assert_eq!(chat.state.groups().len(), 1);
        chat.delete_group_from_menu(cx);
        assert!(chat.state.groups().is_empty());
        assert_eq!(
            chat.state.display_parent(a),
            Some(Parent::Server(cayenchat_model::NetworkId(1)))
        );
    });
}

/// `storage` cannot depend on `app`, so each keeps its own group limits; they
/// must stay equal.
#[test]
fn storage_and_app_share_the_group_limits() {
    assert_eq!(
        cayenchat_storage::order::MAX_GROUPS,
        cayenchat_app::groups::MAX_GROUPS
    );
    assert_eq!(
        cayenchat_storage::order::MAX_GROUP_MEMBERS,
        cayenchat_app::groups::MAX_GROUP_MEMBERS
    );
    assert_eq!(
        cayenchat_storage::order::MAX_GROUP_NAME_CHARS,
        cayenchat_app::groups::MAX_GROUP_NAME_CHARS
    );
}
