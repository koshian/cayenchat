// SPDX-License-Identifier: GPL-3.0-only
//! Channel groups in the channel tree (D075): display-only parents that hold
//! channels of several servers.
//!
//! Everything here changes where a channel row is shown and in which order.
//! Nothing sends to a server, so no JOIN, PART or QUIT leaves from a group
//! operation, and the connection, the log and the unread state of a channel
//! stay with its real server.

use cayenchat_app::{GroupId, GroupMember, GroupSpec, Parent};
use cayenchat_model::{ConversationId, NetworkId, display};
use cayenchat_storage::order::{StoredGroup, StoredMember};
use gpui::{prelude::*, *};

use crate::{ChatWindow, DraggedChannel, button_order, input::TextInput, theme};

/// The menu of a right click on a group heading.
pub(crate) struct GroupMenu {
    pub(crate) position: Point<Pixels>,
    pub(crate) group: GroupId,
    /// Deleting a group that holds channels asks first and says what happens.
    pub(crate) confirming: bool,
}

pub(crate) enum GroupEditTarget {
    /// A group being created for a channel; nothing exists until Enter.
    New(ConversationId),
    Rename(GroupId),
}

/// The name field in the tree: a new group's row, or a renamed heading.
pub(crate) struct GroupEdit {
    pub(crate) target: GroupEditTarget,
    pub(crate) input: Entity<TextInput>,
    /// Leaving the field cancels the edit.
    _blur: Subscription,
}

/// A heading a dragged channel would be dropped on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadingTarget {
    Group(GroupId),
    Server(NetworkId),
}

impl ChatWindow {
    /// Gives the groups saved for the configured servers back to the tree.
    pub(crate) fn restore_channel_groups(&mut self) {
        let profiles: Vec<(NetworkId, String)> = self
            .sessions
            .iter()
            .map(|(id, session)| (*id, session.profile_id.clone()))
            .collect();
        let specs = self
            .channel_orders
            .groups
            .iter()
            .map(|group| GroupSpec {
                name: group.name.clone(),
                collapsed: group.collapsed,
                members: group
                    .members
                    .iter()
                    .filter_map(|member| {
                        let (network, _) = profiles.iter().find(|(_, id)| *id == member.server)?;
                        Some(GroupMember {
                            network: *network,
                            key: member.channel.clone(),
                        })
                    })
                    .collect(),
            })
            .collect();
        self.state.set_groups(specs);
    }

    /// Saves the channel order of `network` (when it changed) and the groups.
    pub(crate) fn save_channel_layout(
        &mut self,
        network: Option<NetworkId>,
        cx: &mut Context<Self>,
    ) {
        if let Some(network) = network
            && let Some(profile_id) = self.sessions.get(&network).map(|s| s.profile_id.clone())
        {
            self.channel_orders
                .set(&profile_id, self.state.channel_order(network));
        }
        self.channel_orders.groups = self
            .state
            .groups()
            .iter()
            .map(|group| StoredGroup {
                name: group.name.clone(),
                collapsed: group.collapsed,
                members: group
                    .members
                    .iter()
                    .filter_map(|member| {
                        Some(StoredMember {
                            server: self.sessions.get(&member.network)?.profile_id.clone(),
                            channel: member.key.clone(),
                        })
                    })
                    .collect(),
            })
            .collect();
        self.channel_orders
            .retain_servers(|id| self.sessions.values().any(|s| s.profile_id == id));
        if let Some(path) = &self.order_file
            && let Err(error) = cayenchat_storage::order::save_orders_to(path, &self.channel_orders)
        {
            self.raise_error(network, error, crate::notices::Origin::User, cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_group(&mut self, id: GroupId, cx: &mut Context<Self>) {
        let collapsed = self.state.group(id).is_some_and(|group| group.collapsed);
        if self.state.set_group_collapsed(id, !collapsed) {
            self.save_channel_layout(None, cx);
        }
    }

    /// Whether the user folded the channels of `network` away (#866).
    pub(crate) fn server_collapsed(&self, network: NetworkId) -> bool {
        self.sessions.get(&network).is_some_and(|s| {
            self.channel_orders
                .collapsed_servers
                .contains(&s.profile_id)
        })
    }

    /// Folds or opens one server's channels. Only the tree changes.
    pub(crate) fn toggle_server(&mut self, network: NetworkId, cx: &mut Context<Self>) {
        let Some(profile_id) = self.sessions.get(&network).map(|s| s.profile_id.clone()) else {
            return;
        };
        if !self.channel_orders.collapsed_servers.remove(&profile_id) {
            self.channel_orders.collapsed_servers.insert(profile_id);
        }
        self.save_channel_layout(None, cx);
    }

    /// The channel menu's "Move to group": an existing group.
    pub(crate) fn move_menu_channel_to_group(&mut self, group: GroupId, cx: &mut Context<Self>) {
        let Some(menu) = self.channel_menu.take() else {
            return;
        };
        if self.state.move_to_group(menu.conversation, group, None) {
            self.save_channel_layout(Some(menu.network), cx);
        }
        cx.notify();
    }

    pub(crate) fn return_menu_channel_to_server(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.channel_menu.take() else {
            return;
        };
        if self.state.return_to_server(menu.conversation) {
            self.save_channel_layout(Some(menu.network), cx);
        }
        cx.notify();
    }

    /// Drop of a channel on a group heading: to the end of the group.
    pub(crate) fn drop_on_group(
        &mut self,
        dragged: &DraggedChannel,
        group: GroupId,
        cx: &mut Context<Self>,
    ) {
        self.drop_heading = None;
        if self.state.move_to_group(dragged.id, group, None) {
            self.save_channel_layout(Some(dragged.network), cx);
        }
        cx.notify();
    }

    /// Drop of a channel on a server heading: back to its own server only.
    pub(crate) fn drop_on_server(
        &mut self,
        dragged: &DraggedChannel,
        server: NetworkId,
        cx: &mut Context<Self>,
    ) {
        self.drop_heading = None;
        if dragged.network == server && self.state.return_to_server(dragged.id) {
            self.save_channel_layout(Some(server), cx);
        }
        cx.notify();
    }

    /// Follows the pointer over a heading during a channel drag.
    pub(crate) fn set_heading_target(
        &mut self,
        target: HeadingTarget,
        inside: bool,
        cx: &mut Context<Self>,
    ) {
        if inside && self.drop_heading != Some(target) {
            self.drop_heading = Some(target);
            cx.notify();
        } else if !inside && self.drop_heading == Some(target) {
            self.drop_heading = None;
            cx.notify();
        }
    }

    /// Opens the name field for a group that will be created for the channel.
    pub(crate) fn begin_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.channel_menu.take() else {
            return;
        };
        self.begin_group_edit(GroupEditTarget::New(menu.conversation), "", window, cx);
    }

    pub(crate) fn begin_rename_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.group_menu.take() else {
            return;
        };
        let name = self
            .state
            .group(menu.group)
            .map(|group| group.name.clone())
            .unwrap_or_default();
        self.begin_group_edit(GroupEditTarget::Rename(menu.group), &name, window, cx);
    }

    fn begin_group_edit(
        &mut self,
        target: GroupEditTarget,
        initial: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = self.i18n.text("group_name_placeholder");
        let input = cx.new(|cx| TextInput::new_field(&placeholder, initial, false, cx));
        input.update(cx, |input, cx| input.select_everything(cx));
        let handle = input.focus_handle(cx);
        let blur = cx.on_blur(&handle, window, |this, _, cx| {
            if this.group_edit.take().is_some() {
                cx.notify();
            }
        });
        self.group_edit = Some(GroupEdit {
            target,
            input,
            _blur: blur,
        });
        window.focus(&handle);
        self.group_edit_reveal = true;
        cx.notify();
    }

    /// Whether the group name field has the keyboard focus.
    pub(crate) fn group_edit_focused(&self, window: &Window, cx: &App) -> bool {
        self.group_edit
            .as_ref()
            .is_some_and(|edit| edit.input.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Enter in the name field. An unusable or taken name keeps the field.
    pub(crate) fn commit_group_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.group_edit.as_ref() else {
            return;
        };
        if edit.input.read(cx).is_composing() {
            return;
        }
        let text = edit.input.read(cx).text().to_owned();
        let done = match edit.target {
            GroupEditTarget::New(conversation) => match self.state.create_group(&text) {
                Some(group) => {
                    let network = self.state.conversation(conversation).map(|c| c.network);
                    self.state.move_to_group(conversation, group, None);
                    self.save_channel_layout(network, cx);
                    true
                }
                None => false,
            },
            GroupEditTarget::Rename(group) => {
                let renamed = self.state.rename_group(group, &text);
                if renamed {
                    self.save_channel_layout(None, cx);
                }
                renamed
            }
        };
        if done {
            self.group_edit = None;
            self.focus_draft(window, cx);
        }
        cx.notify();
    }

    /// Escape in the name field: no group is created, nothing moves.
    pub(crate) fn cancel_group_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.group_edit.take().is_some() {
            self.focus_draft(window, cx);
            cx.notify();
        }
    }

    fn focus_draft(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.inputs.get(&self.state.selection()) {
            window.focus(&input.focus_handle(cx));
        }
    }

    pub(crate) fn open_group_menu(
        &mut self,
        group: GroupId,
        position: Point<Pixels>,
        window: &Window,
    ) {
        let viewport = window.viewport_size();
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        self.member_prompt = None;
        self.group_menu = Some(GroupMenu {
            position: point(
                position.x.min((viewport.width - px(240.)).max(px(0.))),
                position.y.min((viewport.height - px(110.)).max(px(0.))),
            ),
            group,
            confirming: false,
        });
    }

    /// Delete in the group menu: at once for an empty group, otherwise the
    /// menu first says what happens to the channels.
    pub(crate) fn delete_group_from_menu(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.group_menu.as_mut() else {
            return;
        };
        let group = menu.group;
        let holds_channels = self
            .state
            .group(group)
            .is_some_and(|group| !group.members.is_empty());
        if holds_channels && !menu.confirming {
            menu.confirming = true;
        } else {
            self.group_menu = None;
            if self.state.delete_group(group) {
                self.save_channel_layout(None, cx);
            }
        }
        cx.notify();
    }

    /// The group menu: Rename and Delete, or the question before a delete.
    pub(crate) fn render_group_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.group_menu.as_ref()?;
        let group = self.state.group(menu.group)?;
        let theme = theme::current(cx);
        let popup = div()
            .id("group-context-menu")
            .occlude()
            .w(px(240.))
            .p_1()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .shadow_md();
        if menu.confirming {
            let message = self.i18n.format(
                "group_delete_message",
                &[
                    ("name", &display::neutralize_bidi(&group.name)),
                    ("count", &group.members.len().to_string()),
                ],
            );
            return Some(
                popup
                    .child(div().id("group-delete-message").p_2().child(message))
                    .child(
                        button_order::row(div().flex().gap_2().p_2())
                            .child(
                                div()
                                    .id("group-delete-confirm")
                                    .px_2()
                                    .py_1()
                                    .bg(theme.selected)
                                    .cursor_pointer()
                                    .child(self.i18n.text("group_delete_confirm"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.delete_group_from_menu(cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id("group-delete-cancel")
                                    .px_2()
                                    .py_1()
                                    .border_1()
                                    .border_color(theme.border)
                                    .cursor_pointer()
                                    .child(self.i18n.text("cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.group_menu = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }
        Some(
            popup
                .child(
                    div()
                        .id("group-menu-rename")
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover_strong))
                        .child(self.i18n.text("group_rename"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.begin_rename_group(window, cx)),
                        ),
                )
                .child(
                    div()
                        .id("group-menu-delete")
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.hover_strong))
                        .child(self.i18n.text("group_delete"))
                        .on_click(cx.listener(|this, _, _, cx| this.delete_group_from_menu(cx))),
                )
                .into_any_element(),
        )
    }

    /// The channel menu once "Move to group" is open: the groups, the group
    /// the channel is in marked, and a new group.
    pub(crate) fn render_group_choices(
        &self,
        popup: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme::current(cx);
        let current = self
            .channel_menu
            .as_ref()
            .and_then(|menu| self.state.display_parent(menu.conversation));
        let mut popup = popup;
        for group in self.state.groups() {
            let id = group.id;
            let here = current == Some(Parent::Group(id));
            popup = popup.child(
                div()
                    .id(("channel-menu-group", id.0))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.hover_strong))
                    .child(format!(
                        "{}{}",
                        if here { "✓ " } else { "" },
                        display::neutralize_bidi(&group.name)
                    ))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.move_menu_channel_to_group(id, cx)),
                    ),
            );
        }
        popup
            .child(div().my_1().border_t_1().border_color(theme.separator))
            .child(
                div()
                    .id("channel-menu-new-group")
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.hover_strong))
                    .child(self.i18n.text("group_new"))
                    .on_click(cx.listener(|this, _, window, cx| this.begin_new_group(window, cx))),
            )
            .into_any_element()
    }

    /// A group heading, or the heading of the group being renamed.
    pub(crate) fn render_group_row(
        &mut self,
        id: GroupId,
        (unread, highlighted): (bool, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme::current(cx);
        let Some(group) = self.state.group(id) else {
            return div().into_any_element();
        };
        let collapsed = group.collapsed;
        let name = display::neutralize_bidi(&group.name).into_owned();
        let renaming = self
            .group_edit
            .as_ref()
            .filter(|edit| matches!(edit.target, GroupEditTarget::Rename(group) if group == id));
        let target = self.drop_heading == Some(HeadingTarget::Group(id));
        let row = div()
            .id(("group", id.0))
            .px_2()
            .pt_2()
            .pb_1()
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .hover(|d| d.bg(theme.hover_strong))
            .when(target, |d| d.bg(theme.selected))
            .when(highlighted, |d| d.text_color(theme.panes.highlight));
        if let Some(edit) = renaming {
            return self.group_edit_row(row, edit.input.clone(), cx);
        }
        row.child(format!(
            "{} {}{}",
            if collapsed { "▶" } else { "▼" },
            if unread { "● " } else { "" },
            name
        ))
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<DraggedChannel>, _, cx| {
                let inside =
                    event.bounds.contains(&event.event.position) && !event.drag(cx).private;
                this.set_heading_target(HeadingTarget::Group(id), inside, cx);
            }),
        )
        .on_drop(cx.listener(move |this, dragged: &DraggedChannel, _, cx| {
            this.dragging_channel = None;
            this.drop_target = None;
            if !dragged.private {
                this.drop_on_group(dragged, id, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.open_group_menu(id, event.position, window);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(id, cx)))
        .into_any_element()
    }

    /// The row of a group being created, with its name field.
    pub(crate) fn render_new_group_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(input) = self
            .group_edit
            .as_ref()
            .filter(|edit| matches!(edit.target, GroupEditTarget::New(_)))
            .map(|edit| edit.input.clone())
        else {
            return div().into_any_element();
        };
        let row = div().id("group-new").px_2().pt_2().pb_1();
        self.group_edit_row(row, input, cx)
    }

    /// Enter commits and Escape cancels, wherever the field's key goes.
    fn group_edit_row(
        &self,
        row: Stateful<Div>,
        input: Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        row.child(input)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_group_edit(window, cx);
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }
}

/// The server a conversation belongs to, as the tree row of a server heading
/// accepts a drop: only a channel of that server that a group holds.
pub(crate) fn server_accepts(dragged: &DraggedChannel, server: NetworkId, grouped: bool) -> bool {
    dragged.network == server && !dragged.private && grouped
}
