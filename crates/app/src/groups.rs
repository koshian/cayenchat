// SPDX-License-Identifier: MPL-2.0
//! Channel groups: display-only parents in the channel tree (D075).
//!
//! A group only says where channels are shown and in which order. It is not a
//! network: connections, joins, logs and unread state stay with the real
//! server of each channel, and nothing here sends anything. A channel is
//! identified by its server and its folded name (ASCII lowercase, as the
//! saved channel order does), not by its conversation, so it keeps its place
//! through a reconnect, a part and a rejoin, and a restart.

use std::collections::{HashMap, HashSet};

use cayenchat_model::{Conversation, ConversationId, NetworkId};

use crate::{AppState, MoveTo};

/// Groups the user can have, and channels one group remembers; beyond them a
/// file is damage.
pub const MAX_GROUPS: usize = 64;
pub const MAX_GROUP_MEMBERS: usize = 2_000;
/// The longest group name, in characters.
pub const MAX_GROUP_NAME_CHARS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GroupId(pub u32);

/// Where a channel is shown: under a real server or under a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Parent {
    Server(NetworkId),
    Group(GroupId),
}

/// A channel of a group: its server and folded name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMember {
    pub network: NetworkId,
    pub key: String,
}

impl GroupMember {
    fn of(conversation: &Conversation) -> Self {
        Self {
            network: conversation.network,
            key: conversation.name.to_ascii_lowercase(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelGroup {
    pub id: GroupId,
    pub name: String,
    pub collapsed: bool,
    /// In display order, including channels that are not present now.
    pub members: Vec<GroupMember>,
}

/// A group as saved: the same without an ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSpec {
    pub name: String,
    pub collapsed: bool,
    pub members: Vec<GroupMember>,
}

/// The channel tree's shape: the groups, then the servers, each with the
/// conversations shown under it in order. Navigation, numbered shortcuts and
/// drawing all read this one order.
#[derive(Clone, Debug, Default)]
pub struct DisplayTree {
    pub groups: Vec<(GroupId, Vec<ConversationId>)>,
    pub servers: Vec<(NetworkId, Vec<ConversationId>)>,
}

impl DisplayTree {
    pub fn children(&self, parent: Parent) -> &[ConversationId] {
        let children = match parent {
            Parent::Group(id) => self.groups.iter().find(|(g, _)| *g == id).map(|(_, c)| c),
            Parent::Server(id) => self.servers.iter().find(|(s, _)| *s == id).map(|(_, c)| c),
        };
        children.map_or(&[], Vec::as_slice)
    }

    pub fn parent_of(&self, id: ConversationId) -> Option<Parent> {
        let group = self
            .groups
            .iter()
            .find(|(_, children)| children.contains(&id))
            .map(|(group, _)| Parent::Group(*group));
        group.or_else(|| {
            self.servers
                .iter()
                .find(|(_, children)| children.contains(&id))
                .map(|(server, _)| Parent::Server(*server))
        })
    }

    /// Every conversation in the order the tree shows them.
    pub fn conversations(&self) -> impl Iterator<Item = ConversationId> + '_ {
        let groups = self.groups.iter().flat_map(|(_, c)| c.iter().copied());
        groups.chain(self.servers.iter().flat_map(|(_, c)| c.iter().copied()))
    }
}

/// A usable group name: trimmed, not empty, without control characters and
/// not longer than [`MAX_GROUP_NAME_CHARS`].
pub fn clean_group_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && !name.chars().any(char::is_control))
        .then(|| name.chars().take(MAX_GROUP_NAME_CHARS).collect())
}

impl AppState {
    pub fn groups(&self) -> &[ChannelGroup] {
        &self.groups
    }

    pub fn group(&self, id: GroupId) -> Option<&ChannelGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    /// The groups to save.
    pub fn group_specs(&self) -> Vec<GroupSpec> {
        self.groups
            .iter()
            .map(|group| GroupSpec {
                name: group.name.clone(),
                collapsed: group.collapsed,
                members: group.members.clone(),
            })
            .collect()
    }

    /// Adopts saved groups, replacing the present ones. Damage is repaired:
    /// unusable names, repeated names, channels of unknown servers and
    /// channels listed twice are dropped, and the counts are bounded.
    pub fn set_groups(&mut self, specs: Vec<GroupSpec>) {
        self.groups.clear();
        let mut seen = HashSet::new();
        for spec in specs {
            if self.groups.len() >= MAX_GROUPS {
                break;
            }
            let Some(name) = clean_group_name(&spec.name) else {
                continue;
            };
            if self.group_named(&name).is_some() {
                continue;
            }
            let members = spec
                .members
                .into_iter()
                .filter(|m| self.networks.iter().any(|n| n.id == m.network))
                .filter(|m| !m.key.is_empty() && seen.insert((m.network, m.key.clone())))
                .take(MAX_GROUP_MEMBERS)
                .collect();
            let id = self.new_group_id();
            self.groups.push(ChannelGroup {
                id,
                name,
                collapsed: spec.collapsed,
                members,
            });
        }
    }

    fn new_group_id(&mut self) -> GroupId {
        self.next_group_id += 1;
        GroupId(self.next_group_id)
    }

    fn group_named(&self, name: &str) -> Option<GroupId> {
        self.groups
            .iter()
            .find(|group| group.name.to_lowercase() == name.to_lowercase())
            .map(|group| group.id)
    }

    /// Adds an empty group. A name already in use returns that group; `None`
    /// for an unusable name or too many groups.
    pub fn create_group(&mut self, name: &str) -> Option<GroupId> {
        let name = clean_group_name(name)?;
        if let Some(existing) = self.group_named(&name) {
            return Some(existing);
        }
        if self.groups.len() >= MAX_GROUPS {
            return None;
        }
        let id = self.new_group_id();
        self.groups.push(ChannelGroup {
            id,
            name,
            collapsed: false,
            members: Vec::new(),
        });
        Some(id)
    }

    /// Whether `name` renamed the group: false for an unusable name or one
    /// another group has.
    pub fn rename_group(&mut self, id: GroupId, name: &str) -> bool {
        let Some(name) = clean_group_name(name) else {
            return false;
        };
        if self.group_named(&name).is_some_and(|other| other != id) {
            return false;
        }
        match self.groups.iter_mut().find(|group| group.id == id) {
            Some(group) => {
                group.name = name;
                true
            }
            None => false,
        }
    }

    /// Removes a group. Its channels are shown under their servers again, in
    /// the order their servers kept for them.
    pub fn delete_group(&mut self, id: GroupId) -> bool {
        let before = self.groups.len();
        self.groups.retain(|group| group.id != id);
        self.groups.len() != before
    }

    pub fn set_group_collapsed(&mut self, id: GroupId, collapsed: bool) -> bool {
        match self.groups.iter_mut().find(|group| group.id == id) {
            Some(group) if group.collapsed != collapsed => {
                group.collapsed = collapsed;
                true
            }
            _ => false,
        }
    }

    /// Whether any channel is grouped.
    fn any_grouped(&self) -> bool {
        self.groups.iter().any(|group| !group.members.is_empty())
    }

    /// The tree as shown. Without groups it is the plain per-server order.
    pub fn display_tree(&self) -> DisplayTree {
        let mut tree = DisplayTree::default();
        let mut grouped = HashSet::new();
        if self.any_grouped() {
            let index: HashMap<(NetworkId, String), ConversationId> = self
                .conversations
                .iter()
                .filter(|c| !c.is_private())
                .map(|c| {
                    let member = GroupMember::of(c);
                    ((member.network, member.key), c.id)
                })
                .collect();
            for group in &self.groups {
                let present: Vec<ConversationId> = group
                    .members
                    .iter()
                    .filter_map(|m| index.get(&(m.network, m.key.clone())).copied())
                    .collect();
                grouped.extend(present.iter().copied());
                tree.groups.push((group.id, present));
            }
        } else {
            tree.groups = self.groups.iter().map(|g| (g.id, Vec::new())).collect();
        }
        for network in &self.networks {
            let children = self
                .conversations
                .iter()
                .filter(|c| c.network == network.id && !grouped.contains(&c.id))
                .map(|c| c.id)
                .collect();
            tree.servers.push((network.id, children));
        }
        tree
    }

    /// Where the conversation is shown now.
    pub fn display_parent(&self, id: ConversationId) -> Option<Parent> {
        self.display_tree().parent_of(id)
    }

    /// The group that holds the channel, by its identity.
    fn group_index_of(&self, conversation: &Conversation) -> Option<usize> {
        let member = GroupMember::of(conversation);
        self.groups
            .iter()
            .position(|group| group.members.contains(&member))
    }

    /// Takes the channel out of the group that holds it. Whether it was in one.
    fn leave_group(&mut self, conversation: &Conversation) -> bool {
        let member = GroupMember::of(conversation);
        let mut found = false;
        for group in &mut self.groups {
            let before = group.members.len();
            group.members.retain(|m| *m != member);
            found |= group.members.len() != before;
        }
        found
    }

    /// Shows the channel in `group`, before `before` (a channel of that
    /// group) or at the end. Nothing is sent: only the display changes.
    pub fn move_to_group(
        &mut self,
        id: ConversationId,
        group: GroupId,
        before: Option<ConversationId>,
    ) -> bool {
        let Some(conversation) = self.conversation(id).cloned() else {
            return false;
        };
        if conversation.is_private() || self.group(group).is_none() {
            return false;
        }
        let anchor = before
            .and_then(|b| self.conversation(b))
            .map(GroupMember::of);
        let member = GroupMember::of(&conversation);
        let full = self
            .group(group)
            .is_some_and(|g| g.members.len() >= MAX_GROUP_MEMBERS && !g.members.contains(&member));
        if full {
            return false;
        }
        self.leave_group(&conversation);
        let Some(target) = self.groups.iter_mut().find(|g| g.id == group) else {
            return false;
        };
        let at = anchor
            .and_then(|a| target.members.iter().position(|m| *m == a))
            .unwrap_or(target.members.len());
        target.members.insert(at, member);
        true
    }

    /// Shows the channel under its own server again. Whether it was grouped.
    pub fn return_to_server(&mut self, id: ConversationId) -> bool {
        match self.conversation(id).cloned() {
            Some(conversation) => self.leave_group(&conversation),
            None => false,
        }
    }

    /// Whether the channel is shown in a group.
    pub fn is_grouped(&self, id: ConversationId) -> bool {
        self.conversation(id)
            .is_some_and(|c| self.group_index_of(c).is_some())
    }

    /// Whether any channel of the group is unread, and whether any of them
    /// holds a mention: what a collapsed heading shows.
    /// `children` are the group's conversations from the display tree the
    /// caller already has.
    pub fn group_unread(&self, children: &[ConversationId]) -> (bool, bool) {
        (
            children.iter().any(|c| self.is_unread(*c)),
            children.iter().any(|c| self.is_highlighted(*c)),
        )
    }

    /// The conversations the numbered shortcuts reach: those shown under the
    /// selected conversation's parent, or under the selected server.
    pub(crate) fn numbered_targets(&self) -> Vec<ConversationId> {
        let tree = self.display_tree();
        let parent = match self.selected {
            crate::Selection::Channel(id) => tree.parent_of(id),
            crate::Selection::Server(id) => Some(Parent::Server(id)),
            crate::Selection::None => None,
        };
        parent.map_or_else(Vec::new, |parent| tree.children(parent).to_vec())
    }

    /// Moves a channel from one parent to another for `MoveTo::Place`: into
    /// the group of `other`, or back to the server of `other`, which must be
    /// the channel's own server. The channel goes before `other`.
    pub(crate) fn relocate(&mut self, id: ConversationId, other: ConversationId) -> bool {
        let tree = self.display_tree();
        let (Some(from), Some(to)) = (tree.parent_of(id), tree.parent_of(other)) else {
            return false;
        };
        if from == to {
            return false;
        }
        let (Some(moved), Some(target)) = (self.conversation(id), self.conversation(other)) else {
            return false;
        };
        if moved.is_private() != target.is_private() {
            return false;
        }
        match to {
            Parent::Group(group) => self.move_to_group(id, group, Some(other)),
            Parent::Server(network) => {
                if moved.network != network {
                    return false;
                }
                self.return_to_server(id);
                let from = self.conversations.iter().position(|c| c.id == id);
                let to = self.conversations.iter().position(|c| c.id == other);
                if let (Some(from), Some(to)) = (from, to) {
                    let conversation = self.conversations.remove(from);
                    let at = if from < to { to - 1 } else { to };
                    self.conversations.insert(at, conversation);
                    let network = self.conversations[at].network;
                    let order = self.merged_channel_order(network);
                    self.store_channel_order(network, order);
                }
                true
            }
        }
    }

    /// Reorders within a group for [`AppState::move_conversation`]. `None`
    /// when the channel is not in a group or nothing moves.
    pub(crate) fn move_in_group(&mut self, id: ConversationId, to: MoveTo) -> Option<bool> {
        let conversation = self.conversation(id)?.clone();
        let at = self.group_index_of(&conversation)?;
        let tree = self.display_tree();
        let siblings = tree.children(Parent::Group(self.groups[at].id));
        let from = siblings.iter().position(|c| *c == id)?;
        let last = siblings.len() - 1;
        let target = match to {
            MoveTo::Up => siblings[from.saturating_sub(1)],
            MoveTo::Down => siblings[(from + 1).min(last)],
            MoveTo::First => siblings[0],
            MoveTo::Last => siblings[last],
            MoveTo::Place(other) => *siblings.iter().find(|c| **c == other)?,
        };
        if target == id {
            return Some(false);
        }
        let target = GroupMember::of(self.conversation(target)?);
        let members = &mut self.groups[at].members;
        let from = members
            .iter()
            .position(|m| *m == GroupMember::of(&conversation))?;
        let to = members.iter().position(|m| *m == target)?;
        if from < to {
            members[from..=to].rotate_left(1);
        } else {
            members[to..=from].rotate_right(1);
        }
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, MoveTo, NetworkConfig, Selection};

    fn state() -> AppState {
        let config = |id, name: &str, channels: &[&str]| NetworkConfig {
            id: NetworkId(id),
            name: name.into(),
            channels: channels.iter().map(|c| c.to_string()).collect(),
        };
        AppState::with_networks(vec![
            config(1, "ircnet", &["#a", "#b"]),
            config(2, "friend", &["#a", "#c"]),
            config(3, "work", &["#w"]),
        ])
    }

    fn id(state: &AppState, network: u32, name: &str) -> ConversationId {
        state.channel_id(NetworkId(network), name).unwrap()
    }

    fn shown(state: &AppState, parent: Parent) -> Vec<(u32, String)> {
        state
            .display_tree()
            .children(parent)
            .iter()
            .map(|c| {
                let c = state.conversation(*c).unwrap();
                (c.network.0, c.name.clone())
            })
            .collect()
    }

    fn mixed(state: &mut AppState) -> GroupId {
        let group = state.create_group("daily").unwrap();
        for (network, name) in [(2, "#c"), (1, "#a"), (2, "#a")] {
            let channel = id(state, network, name);
            assert!(state.move_to_group(channel, group, None));
        }
        group
    }

    #[test]
    fn without_groups_the_tree_is_the_server_order() {
        let state = state();
        let tree = state.display_tree();
        assert!(tree.groups.is_empty());
        assert_eq!(shown(&state, Parent::Server(NetworkId(1))).len(), 2);
        assert_eq!(tree.conversations().count(), state.conversations().len());
    }

    #[test]
    fn a_moved_channel_is_shown_once_and_stays_with_its_server() {
        let mut state = state();
        let group = mixed(&mut state);
        assert_eq!(
            shown(&state, Parent::Group(group)),
            [(2, "#c".into()), (1, "#a".into()), (2, "#a".into())]
        );
        assert_eq!(
            shown(&state, Parent::Server(NetworkId(1))),
            [(1, "#b".into())]
        );
        assert!(shown(&state, Parent::Server(NetworkId(2))).is_empty());
        // The conversation is the one of its real server.
        let a = id(&state, 1, "#a");
        assert_eq!(state.conversation(a).unwrap().network, NetworkId(1));
        assert_eq!(state.display_parent(a), Some(Parent::Group(group)));
        // A new join of an ungrouped channel appears under its server.
        state.joined_channel(NetworkId(2), "#new");
        assert_eq!(
            shown(&state, Parent::Server(NetworkId(2))),
            [(2, "#new".into())]
        );
    }

    #[test]
    fn numbers_and_cycling_follow_the_displayed_parent() {
        let mut state = state();
        let group = mixed(&mut state);
        let first = id(&state, 2, "#c");
        state.dispatch(Command::SelectChannel(first));
        state.dispatch(Command::SelectChannelAt(1));
        assert_eq!(state.selected_channel().unwrap().name, "#a");
        assert_eq!(state.selected_channel().unwrap().network, NetworkId(1));
        state.dispatch(Command::SelectChannelAt(2));
        assert_eq!(state.selected_channel().unwrap().network, NetworkId(2));
        // No fourth item, and no leaking to a server.
        state.dispatch(Command::SelectChannelAt(3));
        assert_eq!(state.selected_channel().unwrap().network, NetworkId(2));
        // A selected server counts its own remaining channels only.
        state.dispatch(Command::SelectServerAt(0));
        state.dispatch(Command::SelectChannelAt(0));
        assert_eq!(state.selected_channel().unwrap().name, "#b");
        state.dispatch(Command::SelectChannelAt(1));
        assert_eq!(state.selected_channel().unwrap().name, "#b");
        // Next channel walks the group, then the servers.
        state.dispatch(Command::SelectChannel(id(&state, 2, "#a")));
        state.dispatch(Command::NextChannel);
        assert_eq!(state.selected_channel().unwrap().name, "#b");
        state.dispatch(Command::PreviousChannel);
        assert_eq!(state.selected_channel().unwrap().name, "#a");
        assert_eq!(state.selected_channel().unwrap().network, NetworkId(2));
        // Server numbers do not count the group.
        state.dispatch(Command::SelectServerAt(1));
        assert_eq!(state.selection(), Selection::Server(NetworkId(2)));
        assert!(state.group(group).is_some());
    }

    #[test]
    fn channels_reorder_in_a_group_across_servers_and_come_back() {
        let mut state = state();
        let group = mixed(&mut state);
        let (c, a1, a2) = (
            id(&state, 2, "#c"),
            id(&state, 1, "#a"),
            id(&state, 2, "#a"),
        );
        assert_eq!(
            state.move_conversation(a2, MoveTo::First),
            Some(NetworkId(2))
        );
        assert_eq!(state.move_conversation(c, MoveTo::Last), Some(NetworkId(2)));
        assert_eq!(
            shown(&state, Parent::Group(group)),
            [(2, "#a".into()), (1, "#a".into()), (2, "#c".into())]
        );
        // From a server into a group goes before the target.
        let b = id(&state, 1, "#b");
        assert_eq!(
            state.move_conversation(b, MoveTo::Place(a1)),
            Some(NetworkId(1))
        );
        assert_eq!(shown(&state, Parent::Group(group))[1], (1, "#b".into()));
        // Back to the own server only.
        let w = id(&state, 3, "#w");
        assert_eq!(
            state.move_conversation(w, MoveTo::Place(c)),
            Some(NetworkId(3))
        );
        assert!(state.return_to_server(w));
        assert_eq!(state.move_conversation(c, MoveTo::Place(w)), None);
        assert_eq!(state.display_parent(c), Some(Parent::Group(group)));
        let other = id(&state, 2, "#a");
        let _ = other;
        assert!(state.return_to_server(a1));
        assert_eq!(state.display_parent(a1), Some(Parent::Server(NetworkId(1))));
        // Deleting returns the rest.
        assert!(state.delete_group(group));
        assert_eq!(state.display_parent(c), Some(Parent::Server(NetworkId(2))));
        assert_eq!(shown(&state, Parent::Server(NetworkId(2))).len(), 2);
    }

    #[test]
    fn a_server_drop_target_must_be_the_own_server() {
        let mut state = state();
        let group = state.create_group("g").unwrap();
        let a1 = id(&state, 1, "#a");
        let a2 = id(&state, 2, "#a");
        let b = id(&state, 1, "#b");
        state.move_to_group(a1, group, None);
        assert_eq!(state.move_conversation(a1, MoveTo::Place(a2)), None);
        assert_eq!(state.display_parent(a1), Some(Parent::Group(group)));
        assert_eq!(
            state.move_conversation(a1, MoveTo::Place(b)),
            Some(NetworkId(1))
        );
        assert_eq!(
            shown(&state, Parent::Server(NetworkId(1))),
            [(1, "#a".into()), (1, "#b".into())]
        );
    }

    #[test]
    fn the_group_keeps_its_channels_through_a_reset_and_a_part() {
        let mut state = state();
        let group = mixed(&mut state);
        state.parted_channel(NetworkId(1), "#a");
        assert_eq!(shown(&state, Parent::Group(group)).len(), 3);
        state.reset_network(NetworkId(1), vec!["#a".into()]);
        assert_eq!(shown(&state, Parent::Group(group)).len(), 3);
        // A rejoin with other letter case lands in the same place.
        state.reset_network(NetworkId(2), vec![]);
        assert_eq!(shown(&state, Parent::Group(group)), [(1, "#a".into())]);
        state.joined_channel(NetworkId(2), "#C");
        assert_eq!(shown(&state, Parent::Group(group))[0], (2, "#C".into()));
    }

    #[test]
    fn groups_round_trip_and_damage_is_repaired() {
        let mut state = state();
        let group = mixed(&mut state);
        state.set_group_collapsed(group, true);
        let specs = state.group_specs();
        let mut next = self::state();
        next.set_groups(specs.clone());
        assert_eq!(next.group_specs(), specs);
        assert!(next.groups()[0].collapsed);
        let member = |network, key: &str| GroupMember {
            network: NetworkId(network),
            key: key.into(),
        };
        let spec = |name: &str, members| GroupSpec {
            name: name.into(),
            collapsed: false,
            members,
        };
        next.set_groups(vec![
            spec("x", vec![member(1, "#a"), member(9, "#z"), member(1, "#a")]),
            spec("X", vec![member(1, "#b")]),
            spec("  ", vec![]),
            spec("y", vec![member(1, "#a"), member(1, "#b")]),
        ]);
        let groups = next.group_specs();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].members, [member(1, "#a")]);
        assert_eq!(groups[1].members, [member(1, "#b")]);
    }

    #[test]
    fn names_are_cleaned_and_not_repeated() {
        let mut state = state();
        let a = state.create_group("  Daily ").unwrap();
        assert_eq!(state.group(a).unwrap().name, "Daily");
        assert_eq!(state.create_group("daily"), Some(a));
        assert_eq!(state.create_group("a\u{7}b"), None);
        assert_eq!(state.create_group(" "), None);
        let b = state.create_group("Other").unwrap();
        assert!(!state.rename_group(b, "DAILY"));
        assert!(state.rename_group(a, "daily"));
        assert!(state.rename_group(b, "Other 2"));
        for n in 0..MAX_GROUPS {
            state.create_group(&format!("g{n}"));
        }
        assert_eq!(state.groups().len(), MAX_GROUPS);
    }

    #[test]
    fn a_collapsed_group_still_reports_unread() {
        let mut state = state();
        let group = mixed(&mut state);
        state.set_group_collapsed(group, true);
        let a = id(&state, 1, "#a");
        state.dispatch(crate::Command::SelectServer(NetworkId(3)));
        state.mark_unread(a);
        let children =
            |state: &AppState| state.display_tree().children(Parent::Group(group)).to_vec();
        assert_eq!(state.group_unread(&children(&state)), (true, false));
        state.mark_highlighted(NetworkId(2), "#c");
        assert_eq!(state.group_unread(&children(&state)), (true, true));
    }

    #[test]
    fn display_parent_agrees_with_the_tree() {
        let mut state = state();
        mixed(&mut state);
        let query = id(&state, 1, "#a");
        state.dispatch(Command::SelectServer(NetworkId(3)));
        // A private conversation and a grouped one beside plain channels.
        state.private_conversation(NetworkId(1), "bob", "Bob", crate::Missing::Create);
        let tree = state.display_tree();
        let all: Vec<ConversationId> = state.conversations().iter().map(|c| c.id).collect();
        assert!(all.len() > 5);
        for conversation in all {
            assert_eq!(
                state.display_parent(conversation),
                tree.parent_of(conversation)
            );
        }
        assert!(matches!(
            state.display_parent(query),
            Some(Parent::Group(_))
        ));
        assert_eq!(
            state.display_parent(id(&state, 1, "#b")),
            Some(Parent::Server(NetworkId(1)))
        );
        assert_eq!(state.display_parent(ConversationId(u32::MAX)), None);
    }

    #[test]
    fn group_unread_agrees_with_the_tree() {
        let mut state = state();
        let group = mixed(&mut state);
        state.dispatch(crate::Command::SelectServer(NetworkId(3)));
        let children =
            |state: &AppState| state.display_tree().children(Parent::Group(group)).to_vec();
        assert_eq!(state.group_unread(&children(&state)), (false, false));
        state.mark_unread(id(&state, 2, "#a"));
        assert_eq!(state.group_unread(&children(&state)), (true, false));
        state.mark_highlighted(NetworkId(1), "#a");
        assert_eq!(state.group_unread(&children(&state)), (true, true));
        assert_eq!(state.group_unread(&[]), (false, false));
    }

    #[test]
    fn removing_a_server_drops_its_channels_from_groups() {
        let mut state = state();
        let group = mixed(&mut state);
        let keep: Vec<NetworkConfig> = [1, 3]
            .into_iter()
            .map(|n| NetworkConfig {
                id: NetworkId(n),
                name: format!("n{n}"),
                channels: vec![],
            })
            .collect();
        state.sync_networks(&keep);
        assert_eq!(state.group(group).unwrap().members.len(), 1);
    }
}
