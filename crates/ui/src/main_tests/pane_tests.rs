// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, LogPosition, LogSelection, Selection, Worker};
use cayenchat_model::NetworkId;
use cayenchat_storage::Settings;
use gpui::EntityInputHandler as _;
use gpui::{Focusable, TestAppContext};

#[gpui::test]
fn the_title_shows_the_member_count_of_a_channel(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

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
        // Before any connection the title names no server or channel.
        assert!(
            chat.window_title().starts_with("Not connected — ")
                || chat.window_title().starts_with("未接続 — "),
            "{}",
            chat.window_title()
        );
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
        // Without a roster there is no count to show.
        assert!(
            chat.window_title().starts_with("#a @ "),
            "{}",
            chat.window_title()
        );
        let members = ["@op", "alice", "bob"].map(String::from).to_vec();
        chat.state.set_members(NetworkId(1), "#a", members);
        assert!(
            chat.window_title().starts_with("#a (3) @ "),
            "{}",
            chat.window_title()
        );
        chat.state.set_topic(NetworkId(1), "#a", "Welcome");
        assert!(
            chat.window_title().starts_with("#a (3) @ ")
                && chat.window_title().contains(": Welcome"),
            "{}",
            chat.window_title()
        );
    });
}

#[gpui::test]
fn rosters_around_a_case_mapping_change_update_both_conversations(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;
    use cayenchat_model::names::CaseMapping;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#foo[1],#foo{1}");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let names = |channel: &str, user: &str| Event::Names {
        channel: channel.into(),
        users: vec![user.into()],
    };
    let rosters = |chat: &ChatWindow| {
        chat.state
            .conversations()
            .iter()
            .map(|c| (c.name.clone(), c.members.clone()))
            .collect::<Vec<_>>()
    };
    // Servers announce ASCII before the batch under test, so the two
    // names are different conversations at its start.
    let setup = || {
        vec![
            Event::Registered {
                nickname: "me".into(),
            },
            Event::CaseMapping(CaseMapping::Ascii),
            Event::Joined {
                channel: "#foo[1]".into(),
            },
            Event::Joined {
                channel: "#foo{1}".into(),
            },
        ]
    };
    let events = || {
        vec![
            names("#foo{1}", "before"),
            Event::CaseMapping(CaseMapping::Rfc1459),
            names("#foo{1}", "after"),
        ]
    };
    // As ASCII the two names are different conversations, so the first
    // roster must not be dropped in favour of the last. Applying the
    // events one batch at a time is the reference.
    let (separate, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    separate.update(cx, |chat, cx| {
        chat.handle_events(NetworkId(1), setup(), Worker::Running, cx);
        for event in events() {
            chat.handle_events(NetworkId(1), vec![event], Worker::Running, cx);
        }
    });
    let expected = separate.read_with(cx, |chat, _| rosters(chat));
    chat.update(cx, |chat, cx| {
        chat.handle_events(NetworkId(1), setup(), Worker::Running, cx);
        chat.handle_events(NetworkId(1), events(), Worker::Running, cx);
        assert_eq!(rosters(chat), expected);
    });
    assert!(
        expected.iter().any(|(_, m)| m == &["before"]),
        "{expected:?}"
    );
    assert!(
        expected.iter().any(|(_, m)| m == &["after"]),
        "{expected:?}"
    );
}

#[gpui::test]
fn clicking_members_selects_them_and_the_menu_keeps_a_chosen_group(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;
    use gpui::{Modifiers, MouseButton, point, px};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    // A menu bar above the panes would move the rows clicked below.
    let mut settings = crate::settings_with_channels("#a");
    settings.menu_bar_auto_hide = true;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
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
        let members = ["@op", "alice", "bob", "carol"].map(String::from).to_vec();
        chat.state.set_members(NetworkId(1), "#a", members);
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        cx.notify();
    });
    cx.run_until_parked();
    // The member list is the top of the 240 px column at the right edge;
    // its rows are 22 px tall (a 20 px line and 1 px padding each side).
    let viewport = cx.update(|window, _| window.viewport_size());
    let x = f32::from(viewport.width) - 240. + 30.;
    let toolbar = f32::from(crate::toolbar::HEIGHT);
    let row = |index: usize| point(px(x), px(toolbar + 22. * index as f32 + 11.));
    let none = Modifiers::none();
    let chosen = |chat: &gpui::Entity<ChatWindow>, cx: &mut gpui::VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            let channel = chat.state.selected_channel().unwrap();
            chat.member_selection
                .nicknames(channel.id, &channel.members)
        })
    };
    let click = |cx: &mut gpui::VisualTestContext, at, modifiers| {
        cx.simulate_mouse_move(at, None, modifiers);
        cx.simulate_mouse_down(at, MouseButton::Left, modifiers);
        cx.simulate_mouse_up(at, MouseButton::Left, modifiers);
        cx.run_until_parked();
    };

    click(cx, row(1), none);
    assert_eq!(chosen(&chat, cx), ["alice"]);
    click(cx, row(3), Modifiers::secondary_key());
    assert_eq!(chosen(&chat, cx), ["alice", "carol"]);
    // The range starts at the member clicked last (carol).
    click(cx, row(0), Modifiers::shift());
    assert_eq!(chosen(&chat, cx), ["op", "alice", "bob", "carol"]);
    click(cx, row(2), none);
    assert_eq!(chosen(&chat, cx), ["bob"]);
    click(cx, row(0), Modifiers::shift());
    assert_eq!(chosen(&chat, cx), ["op", "alice", "bob"]);

    // A menu on a chosen member keeps the group; on another member it
    // acts on that member alone.
    click(cx, row(1), none);
    click(cx, row(3), Modifiers::secondary_key());
    let right_click = |cx: &mut gpui::VisualTestContext, at| {
        cx.simulate_mouse_move(at, None, none);
        cx.simulate_mouse_down(at, MouseButton::Right, none);
        cx.run_until_parked();
    };
    right_click(cx, row(3));
    assert_eq!(chosen(&chat, cx), ["alice", "carol"]);
    assert!(chat.read_with(cx, |chat, _| chat.member_menu.is_some()));
    // On one of several chosen members the menu acts on all of them.
    let group = |chat: &gpui::Entity<ChatWindow>, cx: &mut gpui::VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            chat.member_menu.as_ref().map(|menu| menu.group.clone())
        })
    };
    assert_eq!(
        group(&chat, cx),
        Some(vec!["alice".to_owned(), "carol".to_owned()])
    );
    right_click(cx, row(2));
    assert_eq!(chosen(&chat, cx), ["bob"]);
    assert_eq!(
        chat.read_with(cx, |chat, _| chat
            .member_menu
            .as_ref()
            .map(|menu| menu.nickname.clone())),
        Some("bob".to_owned())
    );
    // On a member who was not chosen it is the menu for that one member.
    assert_eq!(group(&chat, cx), Some(Vec::new()));
    // A single chosen member is not a group either.
    click(cx, row(1), none);
    right_click(cx, row(1));
    assert_eq!(group(&chat, cx), Some(Vec::new()));

    // Choosing a mode closes the menu; without a connection it says so
    // and sends nothing. An open menu blocks the rows under it, so close
    // it first.
    chat.update(cx, |chat, _| {
        chat.dismiss_menus();
    });
    click(cx, row(1), none);
    click(cx, row(3), Modifiers::secondary_key());
    right_click(cx, row(3));
    assert_eq!(group(&chat, cx).map(|group| group.len()), Some(2));
    chat.update(cx, |chat, cx| {
        chat.member_modes(cayenchat_irc_core::MemberMode::Op, cx);
        assert!(chat.member_menu.is_none());
        assert!(
            chat.notices
                .all()
                .iter()
                .any(|n| n.detail.contains(&chat.i18n.text("not_connected")))
        );
    });
}

use gpui::{Context, IntoElement, Window, div, prelude::*, px};

/// Two rows one above the other, each noting whether it is hovered.
struct TwoRows {
    hovered: std::rc::Rc<std::cell::Cell<[bool; 2]>>,
    exclusive: bool,
}

impl gpui::Render for TwoRows {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let row = |index: usize| {
            let hovered = self.hovered.clone();
            let row = div()
                .id(("row", index))
                .h(px(22.))
                .on_hover(move |on, _, _| {
                    let mut now = hovered.get();
                    now[index] = *on;
                    hovered.set(now);
                });
            if self.exclusive {
                super::exclusive_row(row)
            } else {
                row
            }
        };
        div().size_full().child(row(0)).child(row(1))
    }
}

#[gpui::test]
fn only_one_row_is_hovered_on_the_line_between_two(cx: &mut TestAppContext) {
    use gpui::{Modifiers, point};

    for (exclusive, expected) in [(false, [true, true]), (true, [false, true])] {
        let hovered = std::rc::Rc::new(std::cell::Cell::new([false; 2]));
        let (_view, cx) = cx.add_window_view(|_, _| TwoRows {
            hovered: hovered.clone(),
            exclusive,
        });
        cx.run_until_parked();
        // Rows are 22 px tall: the line at 22 belongs to both.
        cx.simulate_mouse_move(point(px(10.), px(22.)), None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(hovered.get(), expected, "exclusive: {exclusive}");
        // Inside a row, only that row.
        cx.simulate_mouse_move(point(px(10.), px(11.)), None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(hovered.get(), [true, false], "exclusive: {exclusive}");
    }
}

#[gpui::test]
fn member_rows_do_not_overlap(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    for avatars in [false, true] {
        cx.update(|cx| {
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        settings.menu_bar_auto_hide = true;
        for server in &mut settings.servers {
            server.user_avatars = avatars;
        }
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![
                    Event::Registered {
                        nickname: "me".into(),
                    },
                    Event::Joined {
                        channel: "#a".into(),
                    },
                    Event::Names {
                        channel: "#a".into(),
                        // The middle one is too long for the list and
                        // has places where a line could break.
                        users: vec![
                            "@op".into(),
                            "a-very-long-nickname-that-does-not-fit|in[the]narrow-list".into(),
                            "bob".into(),
                        ],
                    },
                ],
                Worker::Running,
                cx,
            );
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            cx.notify();
        });
        cx.run_until_parked();
        let rows: Vec<_> = ["member-row-0", "member-row-1", "member-row-2"]
            .into_iter()
            .map(|selector| cx.debug_bounds(selector).expect("row drawn"))
            .collect();
        for pair in rows.windows(2) {
            assert!(
                pair[0].bottom() <= pair[1].top(),
                "avatars {avatars}: {:?} overlaps {:?}",
                pair[0],
                pair[1]
            );
        }
    }
}

#[gpui::test]
fn focused_lists_take_the_standard_movement_keys(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.menu_bar_auto_hide = true;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        let mut events = vec![Event::Registered {
            nickname: "me".into(),
        }];
        for channel in ["#a", "#b"] {
            events.push(Event::Joined {
                channel: channel.into(),
            });
            events.push(Event::Names {
                channel: channel.into(),
                users: vec!["@op".into(), "alice".into(), "bob".into()],
            });
        }
        chat.handle_events(NetworkId(1), events, Worker::Running, cx);
        let first = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(first));
        cx.notify();
    });
    cx.run_until_parked();
    let selected = |cx: &mut gpui::VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            chat.state.selected_channel().map(|c| c.name.clone())
        })
    };
    // The draft keeps J/K as text and Up/Down as history.
    cx.simulate_keystrokes("j");
    assert_eq!(selected(cx).as_deref(), Some("#a"));

    chat.update_in(cx, |chat, window, _| window.focus(&chat.tree_focus));
    cx.simulate_keystrokes("down");
    assert_eq!(selected(cx).as_deref(), Some("#b"));
    assert!(
        chat.update_in(cx, |chat, window, _| chat.tree_focus.is_focused(window)),
        "moving keeps focus on the tree"
    );
    cx.simulate_keystrokes("up");
    assert_eq!(selected(cx).as_deref(), Some("#a"));
    cx.simulate_keystrokes("end");
    assert_eq!(selected(cx).as_deref(), Some("#b"));
    cx.simulate_keystrokes("home");
    // The first row is the server.
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.state.selection()),
        Selection::Server(NetworkId(1))
    );
    cx.simulate_keystrokes("right");
    assert_eq!(selected(cx).as_deref(), Some("#a"));
    cx.simulate_keystrokes("left");
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.state.selection()),
        Selection::Server(NetworkId(1))
    );
    cx.simulate_keystrokes("down enter");
    assert!(
        chat.update_in(cx, |chat, window, cx| chat.inputs[&chat.state.selection()]
            .focus_handle(cx)
            .is_focused(window)),
        "Enter activates the channel and returns to the draft"
    );

    chat.update_in(cx, |chat, window, _| window.focus(&chat.members_focus));
    cx.simulate_keystrokes("down down");
    let chosen = |cx: &mut gpui::VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            let channel = chat.state.selected_channel().unwrap();
            chat.member_selection
                .nicknames(channel.id, &channel.members)
        })
    };
    // Moving from nothing starts at the first row.
    assert_eq!(chosen(cx), ["alice"]);
    cx.simulate_keystrokes("end");
    assert_eq!(chosen(cx), ["bob"]);
    cx.simulate_keystrokes("up");
    assert_eq!(chosen(cx), ["alice"]);
    // Focus moving to another pane ends the choice (#294).
    // Focus events are only sent to an active window, and from a frame
    // drawn with the list focused.
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear());
    chat.update_in(cx, |chat, window, _| window.focus(&chat.log_focus));
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(chosen(cx).is_empty());
}

#[gpui::test]
fn typing_in_the_log_goes_to_the_draft(cx: &mut TestAppContext) {
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
    chat.update_in(cx, |chat, window, _| window.focus(&chat.log_focus));
    cx.simulate_keystrokes("h i");
    let (text, focused) = chat.update_in(cx, |chat, window, cx| {
        let input = chat.inputs[&chat.state.selection()].clone();
        (
            input.read(cx).text().to_owned(),
            input.focus_handle(cx).is_focused(window),
        )
    });
    assert_eq!(text, "hi");
    assert!(focused);
}

#[gpui::test]
fn composing_in_the_log_goes_to_the_draft(cx: &mut TestAppContext) {
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
        window.focus(&chat.log_focus);
        chat.replace_and_mark_text_in_range(None, "にほ", None, window, cx);
        chat.replace_text_in_range(None, "日本", window, cx);
    });
    let (text, focused) = chat.update_in(cx, |chat, window, cx| {
        let input = chat.inputs[&chat.state.selection()].clone();
        (
            input.read(cx).text().to_owned(),
            input.focus_handle(cx).is_focused(window),
        )
    });
    assert_eq!(text, "日本");
    assert!(focused);
}

#[gpui::test]
fn the_log_input_handler_mirrors_a_non_empty_draft(cx: &mut TestAppContext) {
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
        let input = chat.inputs[&chat.state.selection()].clone();
        input.update(cx, |input, cx| input.set_text("abc", cx));
        window.focus(&chat.log_focus);
        // The cursor and composition are the draft's, not an empty log's.
        let selection = chat.selected_text_range(false, window, cx).unwrap();
        assert_eq!(selection.range, 3..3);
        assert_eq!(chat.marked_text_range(window, cx), None);
        chat.replace_and_mark_text_in_range(None, "にほ", None, window, cx);
        assert_eq!(chat.marked_text_range(window, cx), Some(3..5));
        let mut actual = None;
        assert_eq!(
            chat.text_for_range(0..5, &mut actual, window, cx)
                .as_deref(),
            Some("abcにほ")
        );
        chat.unmark_text(window, cx);
        assert_eq!(chat.marked_text_range(window, cx), None);
        // An explicit UTF-16 range replaces inside the draft.
        chat.replace_text_in_range(Some(0..1), "X", window, cx);
        let text = input.read(cx).text().to_owned();
        assert_eq!(text, "Xbcにほ");
    });
}

#[gpui::test]
fn the_list_panes_forward_typing_and_candidate_position(cx: &mut TestAppContext) {
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
    for pane in ["members", "tree"] {
        chat.update_in(cx, |chat, window, cx| {
            let input = chat.inputs[&chat.state.selection()].clone();
            input.update(cx, |input, cx| input.set_text("", cx));
            match pane {
                "members" => window.focus(&chat.members_focus),
                _ => window.focus(&chat.tree_focus),
            }
        });
        cx.simulate_keystrokes("h i");
        let (text, focused) = chat.update_in(cx, |chat, window, cx| {
            let input = chat.inputs[&chat.state.selection()].clone();
            (
                input.read(cx).text().to_owned(),
                input.focus_handle(cx).is_focused(window),
            )
        });
        assert_eq!(text, "hi", "typing in the {pane} pane");
        assert!(focused, "focus moves to the draft from the {pane} pane");
    }
    // The candidate window follows the draft, not the pane that forwards.
    chat.update_in(cx, |chat, window, cx| {
        let input = chat.inputs[&chat.state.selection()].clone();
        let own = input.read(cx).last_bounds().expect("the draft was painted");
        let stray = gpui::Bounds::new(gpui::point(px(900.), px(5.)), gpui::size(px(10.), px(10.)));
        let forwarded = chat.bounds_for_range(0..0, stray, window, cx).unwrap();
        let direct = input
            .update(cx, |input, cx| {
                input.bounds_for_range(0..0, own, window, cx)
            })
            .unwrap();
        assert_eq!(forwarded, direct);
    });
}

#[gpui::test]
fn member_keys_scroll_only_when_the_row_leaves_the_view(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.menu_bar_auto_hide = true;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        let users = (0..200).map(|n| format!("user{n:03}")).collect();
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::Names {
                    channel: "#a".into(),
                    users,
                },
            ],
            Worker::Running,
            cx,
        );
        let first = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(first));
        cx.notify();
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, _| window.focus(&chat.members_focus));
    let top = |cx: &mut gpui::VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            let handle = chat.members_scroll.0.borrow();
            (-f32::from(handle.base_handle.offset().y) / 22.).round() as usize
        })
    };
    let chosen = |cx: &mut gpui::VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            let channel = chat.state.selected_channel().unwrap();
            chat.member_selection
                .anchor_index(channel.id, &channel.members)
        })
    };

    // Rows still in view do not scroll the list.
    cx.simulate_keystrokes("down down down down");
    cx.run_until_parked();
    assert_eq!(chosen(cx), Some(3));
    assert_eq!(top(cx), 0, "a visible row does not move to the top");

    // Paging past the view keeps the chosen row in view, at the bottom.
    cx.simulate_keystrokes("pagedown pagedown pagedown pagedown");
    cx.run_until_parked();
    let (row, first) = (chosen(cx).unwrap(), top(cx));
    assert!(row > 3 && first > 0 && first <= row, "{first} {row}");
    let visible = chat.read_with(cx, |chat, _| {
        (f32::from(
            chat.members_scroll
                .0
                .borrow()
                .base_handle
                .bounds()
                .size
                .height,
        ) / 22.) as usize
    });
    assert!(row < first + visible + 1, "{first} {row} {visible}");

    // Moving up inside the view leaves the scroll position alone.
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(top(cx), first);

    // After the wheel moves the view away from the chosen row, a key
    // brings the new row in at the nearest edge, not the far one.
    let wheel_to = |cx: &mut gpui::VisualTestContext, row: usize| {
        chat.update(cx, |chat, _| {
            let handle = chat.members_scroll.0.borrow();
            let mut offset = handle.base_handle.offset();
            offset.y = px(-(row as f32) * 22.);
            handle.base_handle.set_offset(offset);
        });
        cx.run_until_parked();
    };
    let row = chosen(cx).unwrap();
    wheel_to(cx, row + 50);
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(chosen(cx), Some(row - 1));
    assert_eq!(top(cx), row - 1, "a row above the view lands at the top");

    cx.simulate_keystrokes("end");
    wheel_to(cx, 0);
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    let (row, first) = (chosen(cx).unwrap(), top(cx));
    assert_eq!(row, 198);
    assert!(
        first > 100 && row < first + visible + 1,
        "a row below the view lands at the bottom: {first} {row}"
    );
}

#[gpui::test]
fn a_member_who_leaves_stays_unchosen_when_the_nickname_is_taken_again(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;
    use gpui::{Modifiers, MouseButton, point, px};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.menu_bar_auto_hide = true;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let names = |users: &[&str]| Event::Names {
        channel: "#a".into(),
        users: users.iter().map(|user| (*user).to_owned()).collect(),
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                names(&["@op", "alice", "bob"]),
            ],
            Worker::Running,
            cx,
        );
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        cx.notify();
    });
    cx.run_until_parked();
    let chosen = |chat: &gpui::Entity<ChatWindow>, cx: &mut gpui::VisualTestContext| {
        chat.read_with(cx, |chat, _| {
            let channel = chat.state.selected_channel().unwrap();
            chat.member_selection
                .nicknames(channel.id, &channel.members)
        })
    };
    let width = f32::from(cx.update(|window, _| window.viewport_size()).width);
    // Rows are 22 px tall; the second row is alice.
    let toolbar = f32::from(crate::toolbar::HEIGHT);
    let alice = point(px(width - 240. + 30.), px(toolbar + 22. + 11.));
    let none = Modifiers::none();
    cx.simulate_mouse_move(alice, None, none);
    cx.simulate_mouse_down(alice, MouseButton::Left, none);
    cx.simulate_mouse_up(alice, MouseButton::Left, none);
    cx.run_until_parked();
    assert_eq!(chosen(&chat, cx), ["alice"]);

    // She quits, and someone else uses the nickname: not chosen.
    for users in [&["@op", "bob"][..], &["@op", "alice", "bob"][..]] {
        chat.update(cx, |chat, cx| {
            chat.handle_events(NetworkId(1), vec![names(users)], Worker::Running, cx)
        });
        cx.run_until_parked();
    }
    assert!(chosen(&chat, cx).is_empty());

    // The same when both rosters arrive in one batch: only the last is
    // applied, but the one before still ends her choice.
    cx.simulate_mouse_move(alice, None, none);
    cx.simulate_mouse_down(alice, MouseButton::Left, none);
    cx.simulate_mouse_up(alice, MouseButton::Left, none);
    cx.run_until_parked();
    assert_eq!(chosen(&chat, cx), ["alice"]);
    chat.update(cx, |chat, cx| {
        let batch = vec![
            names(&["@op", "bob"]),
            names(&["@op", "alice", "bob", "carol"]),
        ];
        chat.handle_events(NetworkId(1), batch, Worker::Running, cx)
    });
    cx.run_until_parked();
    assert!(chosen(&chat, cx).is_empty());
    chat.read_with(cx, |chat, _| {
        let members = &chat.state.selected_channel().unwrap().members;
        assert_eq!(members, &["@op", "alice", "bob", "carol"]);
    });
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![names(&["@op", "alice", "bob"])],
            Worker::Running,
            cx,
        )
    });
    cx.run_until_parked();

    // The same while a group menu is open: it keeps the nicknames it was
    // opened with, but what it acts on is who is still chosen when an item
    // is clicked.
    let toolbar = f32::from(crate::toolbar::HEIGHT);
    let bob = point(px(width - 240. + 30.), px(toolbar + 22. * 2. + 11.));
    cx.simulate_mouse_move(alice, None, none);
    cx.simulate_mouse_down(alice, MouseButton::Left, none);
    cx.simulate_mouse_up(alice, MouseButton::Left, none);
    let secondary = Modifiers::secondary_key();
    cx.simulate_mouse_move(bob, None, secondary);
    cx.simulate_mouse_down(bob, MouseButton::Left, secondary);
    cx.simulate_mouse_up(bob, MouseButton::Left, secondary);
    cx.simulate_mouse_move(bob, None, none);
    cx.simulate_mouse_down(bob, MouseButton::Right, none);
    cx.run_until_parked();
    let (menu_group, now) = chat.read_with(cx, |chat, _| {
        let menu = chat.member_menu.as_ref().expect("group menu open");
        (menu.group.clone(), chat.menu_group_now(menu))
    });
    assert_eq!(menu_group, ["alice", "bob"]);
    assert_eq!(now, ["alice", "bob"]);
    for users in [&["@op", "bob"][..], &["@op", "alice", "bob"][..]] {
        chat.update(cx, |chat, cx| {
            chat.handle_events(NetworkId(1), vec![names(users)], Worker::Running, cx)
        });
        cx.run_until_parked();
    }
    let (menu_group, now) = chat.read_with(cx, |chat, _| {
        let menu = chat.member_menu.as_ref().expect("the menu stays open");
        (menu.group.clone(), chat.menu_group_now(menu))
    });
    assert_eq!(menu_group, ["alice", "bob"], "opened with both");
    assert_eq!(now, ["bob"], "the newcomer named alice is not acted on");
}

#[gpui::test]
fn the_color_picker_and_palette_edit_the_color_fields(cx: &mut TestAppContext) {
    use crate::color_picker::{ColorChanged, format_hex};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a");
    let owner =
        cx.add_window(|window, cx| ChatWindow::with_settings(settings.clone(), None, window, cx));
    let (form, cx) = cx.add_window_view(|window, cx| {
        let mut form = super::SettingsWindow::new(owner, settings.clone(), window, cx);
        form.tab = super::SettingsTab::Appearance;
        form
    });
    let field = form.read_with(cx, |form, _| form.settings.main_log_background.clone());
    let text = |cx: &mut gpui::VisualTestContext| field.read_with(cx, |f, _| f.text().to_owned());
    let before = text(cx);

    // Opening the picker for a field starts from that field's color;
    // opening it again closes it.
    form.update(cx, |form, cx| form.toggle_color_picker(&field, cx));
    cx.run_until_parked();
    let picker = form
        .read_with(cx, |form, _| {
            form.color_picker.as_ref().map(|open| open.picker.clone())
        })
        .expect("open");
    assert_eq!(
        format_hex(picker.read_with(cx, |p, _| p.color())),
        before.to_uppercase()
    );

    // Picking writes the color into the field.
    picker.update(cx, |_, cx| cx.emit(ColorChanged(0xAA0000)));
    cx.run_until_parked();
    assert_eq!(text(cx), "#AA0000");
    // Typing in the field moves the picker.
    field.update(cx, |field, cx| field.set_text("#112233", cx));
    cx.run_until_parked();
    assert_eq!(picker.read_with(cx, |p, _| p.color()), 0x112233);

    // The palette keeps colors, applies one, and forgets one.
    let palette = |form: &gpui::Entity<super::SettingsWindow>, cx: &mut gpui::VisualTestContext| {
        form.read_with(cx, |form, _| {
            form.settings.values.appearance.saved_colors.clone()
        })
    };
    form.update(cx, |form, cx| {
        let _ = form.settings.values.appearance.save_color("#112233");
        let _ = form.settings.values.appearance.save_color("#FFEE00");
        form.apply_palette_color("#FFEE00", cx);
    });
    cx.run_until_parked();
    assert_eq!(palette(&form, cx), ["#112233", "#FFEE00"]);
    assert_eq!(text(cx), "#FFEE00");
    assert_eq!(picker.read_with(cx, |p, _| p.color()), 0xFFEE00);
    form.update(cx, |form, _| {
        form.settings
            .values
            .appearance
            .remove_saved_color("#112233")
    });
    assert_eq!(palette(&form, cx), ["#FFEE00"]);

    // The field is what gets saved, palette included.
    let saved = form
        .update(cx, |form, cx| form.settings.snapshot(cx))
        .unwrap();
    assert_eq!(saved.appearance.main_log_background, "#FFEE00");
    assert_eq!(saved.appearance.saved_colors, ["#FFEE00"]);

    // The same swatch closes it; another field's swatch moves it.
    let other = form.read_with(cx, |form, _| form.settings.dark_main_log_background.clone());
    form.update(cx, |form, cx| form.toggle_color_picker(&other, cx));
    assert!(
        form.read_with(cx, |form, _| form.color_picker.as_ref().unwrap().field
            == other)
    );
    form.update(cx, |form, cx| form.toggle_color_picker(&other, cx));
    assert!(form.read_with(cx, |form, _| form.color_picker.is_none()));

    // Leaving the tab closes an open picker; coming back finds it closed.
    form.update(cx, |form, cx| form.toggle_color_picker(&field, cx));
    assert!(form.read_with(cx, |form, _| form.color_picker.is_some()));
    form.update(cx, |form, cx| {
        form.show_tab(super::SettingsTab::Connection, cx)
    });
    assert!(form.read_with(cx, |form, _| form.color_picker.is_none()));
    // The picker opened here has no field of its own (the row has one),
    // and still follows what is typed in the row.
    form.update(cx, |form, cx| form.toggle_color_picker(&field, cx));
    let picker = form
        .read_with(cx, |form, _| {
            form.color_picker.as_ref().map(|open| open.picker.clone())
        })
        .expect("open");
    assert!(!picker.read_with(cx, |picker, _| picker.shows_hex_field()));
    field.update(cx, |field, cx| field.set_text("#123456", cx));
    cx.run_until_parked();
    assert_eq!(picker.read_with(cx, |picker, _| picker.color()), 0x123456);
}

#[gpui::test]
fn login_startup_is_set_without_a_server_and_follows_the_system(cx: &mut TestAppContext) {
    use crate::autostart::{AutostartStatus, fake};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    *fake::STATE.lock().unwrap() = Ok(AutostartStatus::Disabled);
    *fake::FAIL_CHANGES.lock().unwrap() = false;
    // No server is registered, so the server form is not drawn at all.
    let settings = Settings::default();
    assert!(settings.selected_profile().is_none());
    let owner = cx.add_window(|window, cx| {
        ChatWindow::with_settings(crate::settings_with_channels("#a"), None, window, cx)
    });
    let (form, cx) = cx.add_window_view(|window, cx| {
        let mut form = super::SettingsWindow::new(owner, settings.clone(), window, cx);
        form.tab = super::SettingsTab::Application;
        form
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("autostart").is_some(),
        "shown without a server"
    );
    let shown = |form: &gpui::Entity<super::SettingsWindow>, cx: &mut gpui::VisualTestContext| {
        form.read_with(cx, |form, _| form.autostart.clone())
    };
    assert_eq!(shown(&form, cx), Some(Ok(AutostartStatus::Disabled)));

    // A click changes the system and the checkbox follows it. The
    // checkbox ignores clicks until the change has finished.
    form.update(cx, |form, cx| {
        form.toggle_autostart(cx);
        assert!(form.autostart_busy);
        form.toggle_autostart(cx);
    });
    cx.run_until_parked();
    assert_eq!(shown(&form, cx), Some(Ok(AutostartStatus::Enabled)));
    assert!(!form.read_with(cx, |form, _| form.autostart_busy));

    // A refused change leaves the real state on screen, with the error.
    *fake::FAIL_CHANGES.lock().unwrap() = true;
    form.update(cx, |form, cx| form.toggle_autostart(cx));
    cx.run_until_parked();
    assert_eq!(shown(&form, cx), Some(Ok(AutostartStatus::Enabled)));
    assert!(form.read_with(cx, |form, _| form.feedback.is_some()));
    *fake::FAIL_CHANGES.lock().unwrap() = false;

    // A change made in the system is seen when a tab is shown again.
    *fake::STATE.lock().unwrap() = Ok(AutostartStatus::DisabledByUser);
    form.update(cx, |form, cx| {
        form.show_tab(super::SettingsTab::Appearance, cx)
    });
    cx.run_until_parked();
    assert_eq!(shown(&form, cx), Some(Ok(AutostartStatus::DisabledByUser)));
    *fake::STATE.lock().unwrap() = Ok(AutostartStatus::Disabled);
}

#[gpui::test]
fn dragging_the_splitters_resizes_the_right_column_within_limits(cx: &mut TestAppContext) {
    use super::{LEFT_MIN_WIDTH, RIGHT_PANE_MIN_HEIGHT, RIGHT_WIDTH_MIN};
    use gpui::{Modifiers, MouseButton, point, px};

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
    cx.run_until_parked();
    let none = Modifiers::none();
    let viewport = cx.update(|window, _| window.viewport_size());
    let width = f32::from(viewport.width);
    let drag = |cx: &mut gpui::VisualTestContext, from: (f32, f32), steps: &[(f32, f32)]| {
        let at = |(x, y): (f32, f32)| point(px(x), px(y));
        cx.simulate_mouse_move(at(from), None, none);
        cx.simulate_mouse_down(at(from), MouseButton::Left, none);
        for step in steps {
            cx.simulate_mouse_move(at(*step), MouseButton::Left, none);
        }
        cx.run_until_parked();
        cx.simulate_mouse_up(at(*steps.last().unwrap()), MouseButton::Left, none);
    };

    // The handle sits just left of the 240 px column; pulling it left
    // widens the column, and the window keeps room for the left column.
    let handle_x = width - 240. - 2.;
    drag(
        cx,
        (handle_x, 300.),
        &[(handle_x - 10., 300.), (handle_x - 60., 300.)],
    );
    let widened = chat.read_with(cx, |chat, _| chat.right_width);
    assert!((295.0..=305.0).contains(&widened), "{widened}");
    let x = width - widened - 2.;
    drag(cx, (x, 300.), &[(x - 10., 300.), (-500., 300.)]);
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.right_width),
        width - LEFT_MIN_WIDTH
    );
    let x = width - (width - LEFT_MIN_WIDTH) - 2.;
    drag(cx, (x, 300.), &[(x + 10., 300.), (width + 500., 300.)]);
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.right_width),
        RIGHT_WIDTH_MIN
    );

    // The member list and channel tree share the column; the boundary
    // starts in the middle and cannot squeeze either below its minimum.
    assert_eq!(chat.read_with(cx, |chat, _| chat.members_height), None);
    let column = chat
        .read_with(cx, |chat, _| chat.right_column_bounds.get())
        .expect("the column was laid out");
    let (left, top, height) = (
        f32::from(column.origin.x) + 20.,
        f32::from(column.origin.y),
        f32::from(column.size.height),
    );
    let y = top + height / 2. + 2.;
    drag(cx, (left, y), &[(left, y + 10.), (left, y + 40.)]);
    let members = chat
        .read_with(cx, |chat, _| chat.members_height)
        .expect("dragged");
    assert!(
        (height / 2. + 33.0..=height / 2. + 43.0).contains(&members),
        "{members}"
    );
    let y = top + members + 2.;
    // The list really is that tall: the handle sits right below it.
    let handle = cx.debug_bounds("members-split").expect("handle drawn");
    assert!(
        (f32::from(handle.origin.y) - (top + members)).abs() < 1.,
        "{handle:?} for a list of {members}"
    );
    drag(cx, (left, y), &[(left, y - 10.), (left, -300.)]);
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.members_height),
        Some(RIGHT_PANE_MIN_HEIGHT)
    );
}

#[gpui::test]
fn the_window_layout_is_saved_after_changes_and_applied_at_startup(cx: &mut TestAppContext) {
    use super::{DEFAULT_LOG_SPLIT, DEFAULT_RIGHT_WIDTH, LAYOUT_SAVE_DELAY, LOG_SPLIT_LIMITS};
    use cayenchat_storage::layout::{Layout, load_layout_from};
    use gpui::{Modifiers, MouseButton, point, px};

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
    cx.run_until_parked();
    // Tests never write the user's real layout file: the path is given
    // here, and without one nothing is written.
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("window.json");
    // Saving needs no window at hand, as when the application quits.
    let save = |cx: &mut gpui::VisualTestContext| chat.update(cx, |chat, _| chat.save_layout_now());
    save(cx);
    assert!(!file.exists(), "no path, no file");

    // Applying a saved layout takes over the pane sizes; a split from
    // damage is held within limits.
    chat.update(cx, |chat, _| {
        chat.layout_file = Some(file.clone());
        chat.apply_layout(&Layout {
            right_width: Some(300.),
            members_height: Some(200.),
            log_split: Some(5.0),
            ..Layout::default()
        });
        assert_eq!(chat.right_width, 300.);
        assert_eq!(chat.members_height, Some(200.));
        assert_eq!(chat.log_split, LOG_SPLIT_LIMITS.1);
        chat.apply_layout(&Layout::default());
        assert_eq!(chat.right_width, 300., "nothing saved leaves it alone");
        chat.right_width = DEFAULT_RIGHT_WIDTH;
        chat.members_height = None;
        chat.log_split = DEFAULT_LOG_SPLIT;
    });

    // Saving records the window and the pane sizes.
    save(cx);
    let saved = load_layout_from(&file);
    assert!(saved.window.is_some());
    assert_eq!(saved.right_width, Some(DEFAULT_RIGHT_WIDTH));
    assert_eq!(saved.members_height, None);
    assert_eq!(saved.log_split, Some(DEFAULT_LOG_SPLIT));

    // A resize is noted, so a later write without the window has it.
    cx.simulate_resize(gpui::size(px(1111.), px(777.)));
    cx.run_until_parked();
    save(cx);
    let resized = load_layout_from(&file).window.expect("window saved");
    assert_eq!((resized.width, resized.height), (1111., 777.));

    // Switched off, nothing is written.
    std::fs::remove_file(&file).unwrap();
    chat.update(cx, |chat, _| chat.restore_layout = false);
    save(cx);
    assert!(!file.exists());
    chat.update(cx, |chat, _| chat.restore_layout = true);

    // Dragging the splitter writes it shortly after the last move.
    let none = Modifiers::none();
    let width = f32::from(cx.update(|window, _| window.viewport_size()).width);
    let handle = point(px(width - DEFAULT_RIGHT_WIDTH - 2.), px(300.));
    cx.simulate_mouse_move(handle, None, none);
    cx.simulate_mouse_down(handle, MouseButton::Left, none);
    for step in [10., 60.] {
        cx.simulate_mouse_move(
            point(handle.x - px(step), handle.y),
            MouseButton::Left,
            none,
        );
    }
    cx.run_until_parked();
    cx.simulate_mouse_up(handle, MouseButton::Left, none);
    assert!(!file.exists(), "not before the pause is over");
    cx.executor().advance_clock(LAYOUT_SAVE_DELAY * 2);
    cx.run_until_parked();
    let dragged = load_layout_from(&file);
    let widened = dragged.right_width.expect("saved after the drag");
    assert!((295.0..=305.0).contains(&widened), "{widened}");
}

#[gpui::test]
fn an_older_layout_write_never_replaces_a_newer_one(cx: &mut TestAppContext) {
    use cayenchat_storage::layout::load_layout_from;

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
    cx.run_until_parked();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("window.json");
    let (older, newer) = chat.update(cx, |chat, _| {
        chat.layout_file = Some(file.clone());
        chat.right_width = 250.;
        let older = chat.take_layout_write().expect("kept");
        chat.right_width = 350.;
        let newer = chat.take_layout_write().expect("kept");
        (older, newer)
    });
    // The newer write lands first, as when the older one was left waiting.
    newer();
    older();
    assert_eq!(load_layout_from(&file).right_width, Some(350.));
    // Saving at quit returns with the file written.
    chat.update(cx, |chat, _| {
        chat.right_width = 400.;
        chat.save_layout_now();
    });
    assert_eq!(load_layout_from(&file).right_width, Some(400.));
}

#[gpui::test]
fn server_prompts_open_where_the_menu_was_and_need_a_connection(cx: &mut TestAppContext) {
    use super::{MemberPromptKind, ServerMenu};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.servers[0].nickname = "alice".into();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    for kind in [MemberPromptKind::Join, MemberPromptKind::Nick] {
        chat.update_in(cx, |chat, window, cx| {
            chat.server_menu = Some(ServerMenu {
                position: gpui::point(gpui::px(10.), gpui::px(10.)),
                network: NetworkId(1),
            });
            chat.open_server_prompt(kind, window, cx);
            assert!(chat.server_menu.is_none());
            let prompt = chat.member_prompt.as_ref().expect("prompt opened");
            assert_eq!(prompt.network, NetworkId(1));
            // A new nickname starts from the current one; a channel from nothing.
            let initial = prompt.input.read(cx).text().to_owned();
            match kind {
                MemberPromptKind::Nick => {
                    let saved = chat.saved.servers[0].nickname.clone();
                    assert!(!saved.is_empty());
                    assert_eq!(initial, saved);
                }
                _ => assert_eq!(initial, ""),
            }
            let prompt = chat.member_prompt.as_ref().expect("prompt opened");
            prompt
                .input
                .update(cx, |input, cx| input.set_text("#b", cx));
            chat.submit_member_prompt(window, cx);
            // Not connected: kept open with the reason.
            assert!(chat.member_prompt.is_some());
            assert_eq!(chat.prompt_feedback, Some(chat.i18n.text("not_connected")));
        });
    }
}

#[gpui::test]
fn ircv3_choices_are_per_server_and_wait_for_the_next_connection(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Ircv3Options;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.servers[0].nickname = "me".into();
    settings.servers[0].username = "me".into();
    settings.add_server("irc.example.org");
    settings.servers[1].nickname = "me".into();
    settings.servers[1].username = "me".into();
    settings.servers[1].ircv3.server_time = true;
    settings.servers[1].ircv3.batch = true;
    settings.servers[1].ircv3.peer_avatars = true;
    let config = |settings: &Settings, index: usize| {
        crate::connection_config(
            &settings.servers[index],
            cayenchat_storage::Language::English,
            None,
            None,
        )
        .unwrap()
    };
    // Avatar metadata is always asked for; the rest follows the options.
    assert_eq!(
        config(&settings, 0).ircv3,
        Ircv3Options {
            metadata: true,
            ..Ircv3Options::default()
        }
    );
    assert_eq!(
        config(&settings, 1).ircv3,
        Ircv3Options {
            message_tags: false,
            server_time: true,
            batch: true,
            metadata: true,
            peer_avatars: true,
            chathistory: false,
            confirmed_sending: false,
            accounts: false,
        }
    );
    // Peer avatars alone share nothing and leave the realname unmarked.
    assert_eq!(config(&settings, 1).shared_avatar, None);
    assert!(!config(&settings, 1).advertises_avatar());
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let (first, second) = (NetworkId(1), NetworkId(2));
    chat.update(cx, |chat, cx| {
        // Both servers were used in this run (as after a connection attempt).
        chat.sessions.get_mut(&first).unwrap().active_config = Some(config(&settings, 0));
        chat.sessions.get_mut(&second).unwrap().active_config = Some(config(&settings, 1));
        let generation = chat.sessions[&first].generation;
        let mut next = settings.clone();
        next.servers[0].ircv3.message_tags = true;
        chat.apply_servers(next, cx);
        let ircv3 = |network| {
            chat.sessions[&network]
                .active_config
                .as_ref()
                .unwrap()
                .ircv3
        };
        assert_eq!(
            ircv3(first),
            Ircv3Options {
                message_tags: true,
                server_time: false,
                batch: false,
                metadata: true,
                peer_avatars: false,
                chathistory: false,
                confirmed_sending: false,
                accounts: false,
            },
            "reconnects use the new choice; batch stays off here"
        );
        assert_eq!(
            ircv3(second),
            config(&settings, 1).ircv3,
            "other server unchanged"
        );
        assert_eq!(chat.sessions[&first].generation, generation, "no reconnect");
        assert!(chat.sessions[&first].irc.is_none());
    });
}

#[gpui::test]
fn disconnect_is_offered_only_while_there_is_something_to_stop(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update(cx, |chat, cx| {
        let network = NetworkId(1);
        assert!(!chat.can_disconnect(network), "never connected");
        // A failed connection waiting to retry can still be stopped.
        chat.sessions.get_mut(&network).unwrap().retry_pending = true;
        assert!(chat.can_disconnect(network));
        chat.disconnect(network, cx);
        let session = &chat.sessions[&network];
        assert!(session.manual_disconnect && !session.retry_pending);
        assert!(!chat.can_disconnect(network));
    });
}

#[gpui::test]
fn a_connection_the_user_ended_is_not_reconnected(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{ConnectionConfig, Event};
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update(cx, |chat, cx| {
        let network = NetworkId(1);
        let connected = |chat: &mut ChatWindow| {
            let session = chat.sessions.get_mut(&network).unwrap();
            session.active_config = Some(ConnectionConfig::tls(
                "irc.example".into(),
                "alice".into(),
                vec!["#a".into()],
            ));
            session.manual_disconnect = false;
            session.retry_pending = false;
        };
        connected(chat);
        chat.handle_events(
            network,
            vec![Event::Disconnected("gone".into())],
            Worker::Running,
            cx,
        );
        assert!(chat.sessions[&network].retry_pending, "a lost link retries");
        // `/quit` reaches the worker as a command; it reports the end as Closed.
        connected(chat);
        chat.handle_events(
            network,
            vec![Event::Closed("bye".into())],
            Worker::Running,
            cx,
        );
        let session = &chat.sessions[&network];
        assert!(session.manual_disconnect && !session.retry_pending);
    });
}

#[gpui::test]
fn repeated_message_ids_are_shown_and_notified_once(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_790_553_511_123);
    let mention = |msgid: &str, replayed| Event::ChannelMessage {
        channel: "#b".into(),
        sender: "bob".into(),
        text: "alice: look".into(),
        notice: false,
        server_time: Some(stamp),
        msgid: Some(msgid.into()),
        account: None,
        replayed,
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::Joined {
                    channel: "#b".into(),
                },
                mention("m1", false),
                // A bouncer or history request delivering it again.
                mention("m1", true),
                mention("m1", false),
                mention("m2", false),
            ],
            Worker::Running,
            cx,
        );
        assert_eq!(chat.notifier.shown.len(), 2);
        let b = &chat.state.conversations()[1];
        assert_eq!(b.messages.len(), 2);
        let ids: Vec<_> = b
            .messages
            .iter()
            .map(|m| m.native_id.as_ref().map(|id| id.as_str()))
            .collect();
        assert_eq!(ids, [Some("m1"), Some("m2")]);
        assert!(b.messages.iter().all(|m| {
            m.timestamp
                .is_some_and(|t| t.as_millis() == 1_790_553_511_123)
        }));
    });
}

#[gpui::test]
fn requested_history_is_inserted_quietly_before_newer_lines(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{Event, HistoryMessage};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let old = |text: &str, msgid: &str| HistoryMessage {
        sender: "bob".into(),
        text: text.into(),
        notice: false,
        server_time: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_550_000)),
        msgid: Some(msgid.into()),
        account: None,
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::Joined {
                    channel: "#b".into(),
                },
                Event::HistoryRequested {
                    channel: "#b".into(),
                    resumed: false,
                },
                Event::ChannelMessage {
                    channel: "#b".into(),
                    sender: "bob".into(),
                    text: "alice: live".into(),
                    notice: false,
                    server_time: None,
                    msgid: Some("live1".into()),
                    account: None,
                    replayed: false,
                },
                Event::ChannelHistory {
                    channel: "#b".into(),
                    incomplete: false,
                    messages: vec![
                        old("alice: from yesterday", "old1"),
                        HistoryMessage {
                            notice: true,
                            ..old("maintenance", "old2")
                        },
                        old("alice: live", "live1"),
                    ],
                },
            ],
            Worker::Running,
            cx,
        );
        assert_eq!(chat.notifier.shown.len(), 1, "only the live mention");
        let b = &chat.state.conversations()[1];
        let texts: Vec<_> = b.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "alice: from yesterday",
                "[NOTICE] maintenance",
                "alice: live"
            ]
        );
        assert!(b.messages[0].is_history());
        assert!(b.messages[0].highlights.is_empty());
        let live = &b.messages[2];
        let shown: Vec<_> = live
            .highlights
            .as_slice()
            .iter()
            .map(|r| &live.text[r.clone()])
            .collect();
        assert_eq!(shown, ["alice"]);
        // The combined log shows the live line only.
        let rows = super::newest_lines(chat.state.conversations(), None, 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, b.messages[2].sequence);
    });
}

#[gpui::test]
fn discovered_direct_messages_open_quietly_and_overlap_is_dropped(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{Event, HistoryMessage};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let line = |sender: &str, text: &str, secs: u64, msgid: &str| HistoryMessage {
        sender: sender.into(),
        text: text.into(),
        notice: false,
        server_time: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
        msgid: Some(msgid.into()),
        account: None,
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                // A conversation we already know, with one live line.
                Event::PrivateMessage {
                    sender: "Bob".into(),
                    text: "live".into(),
                    notice: false,
                    server_time: Some(
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_550_100),
                    ),
                    msgid: Some("d2".into()),
                    account: None,
                    replayed: false,
                },
                // Discovery: a new peer, and Bob again in another case.
                Event::HistoryRequested {
                    channel: "carol".into(),
                    resumed: false,
                },
                Event::ChannelHistory {
                    channel: "carol".into(),
                    incomplete: false,
                    messages: vec![
                        line("carol", "call me", 1_790_550_000, "c1"),
                        line("alice", "ok", 1_790_550_050, "c2"),
                    ],
                },
                Event::HistoryRequested {
                    channel: "BOB".into(),
                    resumed: false,
                },
                Event::ChannelHistory {
                    channel: "BOB".into(),
                    incomplete: false,
                    messages: vec![
                        line("Bob", "missed", 1_790_550_000, "d1"),
                        line("Bob", "live", 1_790_550_100, "d2"),
                    ],
                },
            ],
            Worker::Running,
            cx,
        );
        let key = |nick: &str| cayenchat_irc_core::text::nickname_key(nick);
        let carol = chat.state.private_id(NetworkId(1), &key("carol")).unwrap();
        let bob = chat.state.private_id(NetworkId(1), &key("bob")).unwrap();
        let texts = |id| -> Vec<String> {
            chat.state
                .conversations()
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect()
        };
        assert_eq!(texts(carol), ["call me", "ok"]);
        // No second Bob conversation, and the repeated live line is dropped.
        assert_eq!(
            chat.state
                .conversations()
                .iter()
                .filter(|c| c.is_private())
                .count(),
            2
        );
        // Logs keep arrival order: the reserved block sits where the
        // request was made, after the live line that was already there.
        assert_eq!(texts(bob), ["live", "missed"]);
        // History is context: no unread mark, notification or highlight.
        assert!(!chat.state.is_unread(carol));
        assert_eq!(chat.notifier.shown.len(), 1, "only Bob's live line");
        let messages = &chat
            .state
            .conversations()
            .iter()
            .find(|c| c.id == carol)
            .unwrap()
            .messages;
        assert!(messages.iter().all(|m| m.is_history()));
    });
}

#[gpui::test]
fn scrolling_to_the_top_loads_one_older_page_quietly(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{
        Connection, ConnectionConfig, Event, HistoryMessage, OlderHistoryStatus,
    };

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let at = |secs: u64| Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
    let line = |text: &str, secs: u64, msgid: &str| HistoryMessage {
        sender: "bob".into(),
        text: text.into(),
        notice: false,
        server_time: at(secs),
        msgid: Some(msgid.into()),
        account: None,
    };
    // A connection to a local listener that never registers: requests
    // can be queued on it, nothing reaches a server.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = listener.local_addr().unwrap().port();
    config.use_tls = false;
    let connection = Connection::connect(config).unwrap();
    let (b, main_rows) = chat.update(cx, |chat, cx| {
        chat.sessions.get_mut(&NetworkId(1)).unwrap().irc = Some(connection);
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::HistoryAvailable(true),
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::Joined {
                    channel: "#b".into(),
                },
                Event::HistoryRequested {
                    channel: "#b".into(),
                    resumed: false,
                },
                Event::ChannelHistory {
                    channel: "#b".into(),
                    incomplete: false,
                    messages: vec![line("recent", 1_790_550_000, "r1")],
                },
            ],
            Worker::Running,
            cx,
        );
        let b = chat.state.channel_id(NetworkId(1), "#b").unwrap();
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(b));
        chat.sync_log_lists();
        (
            b,
            chat.main_lists[&Selection::Channel(b)].state.item_count(),
        )
    });
    cx.run_until_parked();

    let request = chat.update(cx, |chat, _| {
        // Far from the top, or another conversation: nothing is asked.
        chat.scrolled_main_log(b, 40);
        let a = chat.state.channel_id(NetworkId(1), "#a").unwrap();
        chat.scrolled_main_log(a, 0);
        assert!(chat.state.older_history_in_flight(b).is_none());
        assert!(chat.state.older_history_in_flight(a).is_none());
        // Scrolled to the top: one page is asked for, and only one.
        chat.scrolled_main_log(b, 0);
        let request = chat.state.older_history_in_flight(b).expect("asked");
        chat.scrolled_main_log(b, 1);
        assert_eq!(chat.state.older_history_in_flight(b), Some(request));
        request
    });
    chat.update(cx, |chat, cx| {
        // The user has selected text in the line that is on screen.
        chat.log_selection = Some(LogSelection {
            source: Selection::Channel(b),
            anchor: LogPosition { row: 0, byte: 0 },
            cursor: LogPosition { row: 0, byte: 3 },
        });
        let before = chat.notifier.shown.len();
        chat.handle_events(
            NetworkId(1),
            vec![
                // A live line while the page is on its way.
                Event::ChannelMessage {
                    channel: "#b".into(),
                    sender: "carol".into(),
                    text: "live".into(),
                    notice: false,
                    server_time: at(1_790_560_000),
                    msgid: Some("l1".into()),
                    account: None,
                    replayed: false,
                },
                Event::OlderChannelHistory {
                    channel: "#b".into(),
                    request,
                    messages: vec![
                        line("alice: older mention", 1_790_540_000, "o1"),
                        line("recent", 1_790_550_000, "r1"),
                    ],
                    status: OlderHistoryStatus::Beginning,
                },
            ],
            Worker::Running,
            cx,
        );
        assert_eq!(chat.notifier.shown.len(), before, "history never notifies");
        let texts: Vec<_> = chat.state.conversations()[1]
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(texts, ["alice: older mention", "recent", "live"]);
        let older = &chat.state.conversations()[1].messages[0];
        assert!(older.is_history());
        assert!(older.highlights.is_empty());
        assert!(!chat.state.is_highlighted(b));
        let selection = chat.log_selection.unwrap();
        assert_eq!(
            (selection.anchor.row, selection.cursor.row),
            (1, 1),
            "still on \"recent\""
        );
        chat.sync_log_lists();
        assert_eq!(
            chat.main_lists[&Selection::Channel(b)].state.item_count(),
            main_rows + 2
        );
        // The beginning was reached: scrolling asks nothing more.
        chat.scrolled_main_log(b, 0);
        assert!(chat.state.older_history_in_flight(b).is_none());
        // A page answering an old request is ignored.
        chat.handle_events(
            NetworkId(1),
            vec![Event::OlderChannelHistory {
                channel: "#b".into(),
                request,
                messages: vec![line("stale", 1_790_530_000, "s1")],
                status: OlderHistoryStatus::More,
            }],
            Worker::Running,
            cx,
        );
        assert_eq!(chat.state.conversations()[1].messages.len(), 3);
    });
}

#[gpui::test]
fn sent_messages_are_confirmed_in_place_and_unconfirmed_ones_are_marked(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let accepted = |local_id, text: &str| Event::OutgoingAccepted {
        local_id,
        channel: "#a".into(),
        text: text.into(),
        notice: false,
        action: None,
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                // Shown at once, before any confirmation.
                accepted(Some(1), "hello  world"),
                accepted(Some(2), "rejected"),
                accepted(Some(3), "never answered"),
                accepted(None, "no echo-message here"),
            ],
            Worker::Running,
            cx,
        );
        let id = chat.state.channel_id(NetworkId(1), "#a").unwrap();
        let lines = |chat: &ChatWindow| {
            chat.state
                .conversations()
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .messages
                .iter()
                .map(|m| (m.text.clone(), m.delivery_failed))
                .collect::<Vec<_>>()
        };
        assert_eq!(chat.sessions[&NetworkId(1)].pending_sends.len(), 3);
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::OutgoingConfirmed {
                    local_id: 1,
                    text: Some("hello world".into()),
                    action: None,
                    msgid: Some("m1".into()),
                    server_time: None,
                },
                Event::OutgoingFailed {
                    local_id: 2,
                    reason: "Cannot send to channel".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        // Replaced in place, not added again.
        assert_eq!(
            lines(chat)
                .iter()
                .filter(|(text, _)| text.starts_with("hello"))
                .collect::<Vec<_>>(),
            [&("hello world".to_owned(), false)]
        );
        assert!(lines(chat).contains(&("rejected".to_owned(), true)));
        assert_eq!(chat.sessions[&NetworkId(1)].pending_sends.len(), 1);
        // The rejection is also said in the server log.
        assert!(
            chat.state
                .server_messages(NetworkId(1))
                .iter()
                .any(|m| m.text.contains("Cannot send to channel"))
        );
        // A link that ends with a message unconfirmed marks it.
        chat.handle_events(
            NetworkId(1),
            vec![Event::Disconnected("gone".into())],
            Worker::Running,
            cx,
        );
        assert!(lines(chat).contains(&("never answered".to_owned(), true)));
        assert!(lines(chat).contains(&("no echo-message here".to_owned(), false)));
        assert!(chat.sessions[&NetworkId(1)].pending_sends.is_empty());
        // The channel log shows where the connection was lost, once.
        let lost = |chat: &ChatWindow| {
            lines(chat)
                .iter()
                .filter(|(text, _)| text.starts_with("Disconnected"))
                .count()
        };
        assert_eq!(lost(chat), 1);
        chat.handle_events(
            NetworkId(1),
            vec![Event::Disconnected("still gone".into())],
            Worker::Running,
            cx,
        );
        assert_eq!(lost(chat), 1);
    });
}

#[gpui::test]
fn tracked_accounts_fill_whois_and_are_forgotten_with_the_connection(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{Event, WhoisInfo};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::UserAccount {
                    nickname: "Bob".into(),
                    account: Some("bob-acct".into()),
                    realname: Some("Bob Builder".into()),
                },
                Event::UserAccount {
                    nickname: "Eve".into(),
                    account: None,
                    realname: None,
                },
                Event::UserAccountForgotten {
                    nickname: "EVE".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        let accounts = |chat: &ChatWindow| chat.sessions[&NetworkId(1)].user_accounts.clone();
        assert_eq!(accounts(chat).len(), 1, "casemapped removal");
        let tracked = accounts(chat);
        let mut info = WhoisInfo {
            nickname: "bob".into(),
            username: Some("u".into()),
            ..WhoisInfo::default()
        };
        super::complete_whois(&mut info, &tracked);
        assert_eq!(info.account.as_deref(), Some("bob-acct"));
        assert_eq!(info.realname.as_deref(), Some("Bob Builder"));
        let mut said = WhoisInfo {
            nickname: "BOB".into(),
            account: Some("server-acct".into()),
            ..WhoisInfo::default()
        };
        super::complete_whois(&mut said, &tracked);
        assert_eq!(said.account.as_deref(), Some("server-acct"));
        chat.handle_events(
            NetworkId(1),
            vec![Event::Disconnected("gone".into())],
            Worker::Running,
            cx,
        );
        assert!(accounts(chat).is_empty());
    });
}

#[gpui::test]
fn a_reconnect_recovers_missed_lines_where_the_log_was_cut_off(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{ConnectionConfig, Event, HistoryMessage};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let at = |secs: u64| Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
    let message = |text: &str, secs: u64, msgid: &str| Event::ChannelMessage {
        channel: "#a".into(),
        sender: "bob".into(),
        text: text.into(),
        notice: false,
        server_time: at(secs),
        msgid: Some(msgid.into()),
        account: None,
        replayed: false,
    };
    let history = |text: &str, secs: u64, msgid: &str| HistoryMessage {
        sender: "bob".into(),
        text: text.into(),
        notice: false,
        server_time: at(secs),
        msgid: Some(msgid.into()),
        account: None,
    };
    let session = |events: Vec<Event>| {
        let mut all = vec![
            Event::Registered {
                nickname: "alice".into(),
            },
            Event::HistoryAvailable(true),
            Event::Joined {
                channel: "#a".into(),
            },
        ];
        all.extend(events);
        all
    };
    chat.update(cx, |chat, cx| {
        chat.sessions.get_mut(&NetworkId(1)).unwrap().active_config = Some(ConnectionConfig::tls(
            "irc.example".into(),
            "alice".into(),
            vec!["#a".into()],
        ));
        chat.handle_events(
            NetworkId(1),
            session(vec![
                message("A", 1_790_550_000, "m1"),
                Event::Disconnected("connection reset".into()),
            ]),
            Worker::Running,
            cx,
        );
        // The next connection resumes #a after its last line.
        let config = chat.reconnect_config(NetworkId(1)).unwrap();
        assert_eq!(config.resume_history.len(), 1);
        assert_eq!(config.resume_history[0].channel, "#a");
        assert_eq!(config.resume_history[0].after.msgid.as_deref(), Some("m1"));
        assert_eq!(config.resume_history[0].after.time, at(1_790_550_000));
        // The saved configuration itself is not changed.
        assert!(
            chat.sessions[&NetworkId(1)]
                .active_config
                .as_ref()
                .unwrap()
                .resume_history
                .is_empty()
        );
        let shown = chat.notifier.shown.len();
        chat.handle_events(
            NetworkId(1),
            session(vec![
                Event::HistoryRequested {
                    channel: "#a".into(),
                    resumed: true,
                },
                message("D", 1_790_560_000, "m4"),
                Event::ChannelHistory {
                    channel: "#a".into(),
                    messages: vec![
                        history("alice: while you were away", 1_790_555_000, "m3"),
                        history("D", 1_790_560_000, "m4"),
                    ],
                    incomplete: true,
                },
            ]),
            Worker::Running,
            cx,
        );
        assert_eq!(
            chat.notifier.shown.len(),
            shown,
            "recovered lines never notify"
        );
        let texts: Vec<_> = chat.state.conversations()[0]
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(
            texts,
            [
                "A",
                super::HISTORY_GAP_NOTE,
                "alice: while you were away",
                "Disconnected: connection reset",
                "D"
            ]
        );
        assert!(chat.state.history_resume(NetworkId(1)).is_empty());
        assert!(
            chat.reconnect_config(NetworkId(1))
                .unwrap()
                .resume_history
                .is_empty()
        );
    });
}

#[gpui::test]
fn private_messages_get_their_own_conversations(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    settings.add_server("two.example");
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let pm = |sender: &str, text: &str, notice, replayed| Event::PrivateMessage {
        sender: sender.into(),
        text: text.into(),
        notice,
        server_time: None,
        msgid: None,
        account: None,
        replayed,
    };
    chat.update(cx, |chat, cx| {
        let (one, two) = (chat.state.networks()[0].id, chat.state.networks()[1].id);
        for network in [one, two] {
            chat.handle_events(
                network,
                vec![Event::Registered {
                    nickname: "alice".into(),
                }],
                Worker::Running,
                cx,
            );
        }
        chat.handle_events(
            one,
            vec![
                Event::Joined {
                    channel: "#a".into(),
                },
                pm("Bob", "hello", false, false),
                pm("bob", "again", false, false),
                // Services' notices stay in the server log.
                pm("NickServ", "This nickname is registered", true, false),
                pm("carol", "old", false, true),
                Event::OutgoingAccepted {
                    local_id: None,
                    channel: "BOB".into(),
                    text: "hi bob".into(),
                    notice: false,
                    action: None,
                },
                Event::OwnPrivateMessage {
                    target: "bob".into(),
                    text: "from my phone".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    replayed: false,
                },
                // A notice from someone we already talk to joins them.
                pm("bob", "psst", true, false),
                Event::ChannelMessage {
                    channel: "#a".into(),
                    sender: "bob".into(),
                    text: "in the channel".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        // The same nickname on the other server is someone else.
        chat.handle_events(
            two,
            vec![pm("bob", "other server", false, false)],
            Worker::Running,
            cx,
        );

        let conversations = chat.state.conversations();
        let names: Vec<_> = conversations
            .iter()
            .map(|c| (c.network, c.name.as_str(), c.is_private()))
            .collect();
        assert_eq!(
            names,
            [
                (one, "#a", false),
                (one, "bob", true),
                (one, "carol", true),
                (two, "bob", true),
            ]
        );
        let texts = |index: usize| -> Vec<(String, String)> {
            chat.state.conversations()[index]
                .messages
                .iter()
                .map(|m| (m.sender.clone(), m.text.clone()))
                .collect()
        };
        assert_eq!(texts(0), [("bob".into(), "in the channel".into())]);
        assert_eq!(
            texts(1),
            [
                ("Bob".into(), "hello".into()),
                ("bob".into(), "again".into()),
                ("alice".into(), "hi bob".into()),
                ("alice".into(), "from my phone".into()),
                ("bob".into(), "[NOTICE] psst".into()),
            ]
        );
        assert!(chat.state.conversations()[2].messages[0].is_history());
        assert!(
            chat.state
                .server_messages(one)
                .iter()
                .any(|m| m.text == "-NickServ- This nickname is registered")
        );
        assert!(
            !chat
                .state
                .server_messages(one)
                .iter()
                .any(|m| m.text.contains("hello")),
            "not duplicated in the server log"
        );
        let bob = chat.state.conversations()[1].id;
        assert!(chat.state.is_unread(bob) && chat.state.is_highlighted(bob));
        // Two live PRIVMSGs from bob and one from the other server's bob;
        // the replayed one and the notices do not notify.
        assert_eq!(chat.notifier.shown.len(), 3);

        // Reading the conversation in the focused window silences it.
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(bob));
        chat.window_active = true;
        chat.handle_events(
            one,
            vec![pm("bob", "seen", false, false)],
            Worker::Running,
            cx,
        );
        assert_eq!(chat.notifier.shown.len(), 3);
        assert!(!chat.state.is_highlighted(bob));

        // Nick changes follow the peer; a quit leaves a boundary line.
        chat.handle_events(
            one,
            vec![
                Event::UserNickChanged {
                    from: "Bob".into(),
                    to: "robert".into(),
                },
                Event::UserQuit {
                    nickname: "robert".into(),
                    reason: Some("bye".into()),
                },
                pm("bob", "a new bob", false, false),
            ],
            Worker::Running,
            cx,
        );
        let robert = chat
            .state
            .conversations()
            .iter()
            .find(|c| c.id == bob)
            .unwrap();
        assert_eq!(robert.name, "robert");
        let tail: Vec<_> = robert
            .messages
            .iter()
            .rev()
            .take(2)
            .map(|m| (m.activity, m.text.as_str()))
            .collect();
        assert_eq!(
            tail,
            [
                (true, "robert has quit (bye)"),
                (true, "Bob is now known as robert")
            ]
        );
        let new_bob = chat.state.private_id(one, "bob").unwrap();
        assert_ne!(new_bob, bob);

        // Closing drops the conversation and its draft.
        chat.channel_menu = Some(crate::ChannelMenu {
            position: gpui::point(gpui::px(0.), gpui::px(0.)),
            network: one,
            conversation: new_bob,
            channel: "bob".into(),
            joined: true,
            private: true,
        });
        chat.close_private_conversation(cx);
        assert!(chat.state.private_id(one, "bob").is_none());
        assert!(!chat.inputs.contains_key(&Selection::Channel(new_bob)));
    });
}

#[test]
fn shortcut_digits_follow_the_numbered_shortcuts() {
    let digits: String = (0..12).filter_map(crate::shortcut_digit).collect();
    assert_eq!(digits, "1234567890");
}

#[test]
fn a_window_with_older_settings_follows_the_servers_order_in_the_file() {
    let profile = |id: &str| {
        let mut profile = cayenchat_storage::ServerProfile::default();
        profile.id = id.to_owned();
        profile
    };
    let ids = |servers: &[cayenchat_storage::ServerProfile]| -> Vec<String> {
        servers.iter().map(|p| p.id.clone()).collect()
    };
    let file = [profile("two"), profile("one")];
    let mut held = vec![profile("one"), profile("two"), profile("new")];
    assert!(crate::settings_window::follow_server_order(
        &mut held, &file
    ));
    assert_eq!(ids(&held), ["two", "one", "new"]);
    // Nothing to follow once they agree, or when the file lacks servers.
    assert!(!crate::settings_window::follow_server_order(
        &mut held, &file
    ));
    assert!(!crate::settings_window::follow_server_order(
        &mut held,
        &[profile("one")]
    ));
    assert_eq!(ids(&held), ["two", "one", "new"]);
}

#[gpui::test]
fn moving_channels_is_saved_per_server_and_restored(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a,#b,#c");
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("channel-order.json");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        let profile = chat.saved.selected_profile().unwrap().id.clone();
        let c = chat.state.conversations()[2].id;
        // Without a path nothing is written, but the tree still moves.
        chat.move_conversation(c, cayenchat_app::MoveTo::First, cx);
        assert!(!file.exists());
        chat.order_file = Some(file.clone());
        chat.move_conversation(c, cayenchat_app::MoveTo::Last, cx);
        let names = |chat: &ChatWindow| {
            chat.state
                .conversations()
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(chat), ["#a", "#b", "#c"]);
        let a = chat.state.conversations()[0].id;
        chat.move_conversation(a, cayenchat_app::MoveTo::Down, cx);
        assert_eq!(names(chat), ["#b", "#a", "#c"]);
        let saved = cayenchat_storage::order::load_orders_from(&file);
        assert_eq!(saved.order(&profile), ["#b", "#a", "#c"]);
    });
}

#[gpui::test]
fn the_channel_menu_disables_or_adds_auto_join_without_deleting(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a,-#b");
    let file = crate::settings_file::TestFile::with(&settings);
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update(cx, |chat, cx| {
        let network = chat.state.networks()[0].id;
        let menu = |chat: &mut ChatWindow, channel: &str| {
            chat.channel_menu = Some(crate::ChannelMenu {
                position: gpui::point(gpui::px(0.), gpui::px(0.)),
                network,
                conversation: chat.state.conversations()[0].id,
                channel: channel.into(),
                joined: false,
                private: false,
            });
        };
        assert!(chat.auto_join_enabled(network, "#A"));
        assert!(!chat.auto_join_enabled(network, "#b"));
        let saved = |chat: &ChatWindow| chat.saved.selected_profile().unwrap().channels.clone();
        chat.sessions.get_mut(&network).unwrap().active_config =
            Some(cayenchat_irc_core::ConnectionConfig::tls(
                "irc.example".into(),
                "alice".into(),
                vec!["#a".into()],
            ));

        menu(chat, "#a");
        chat.toggle_auto_join(false, cx);
        assert_eq!(saved(chat), "-#a,-#b");
        // The next reconnect uses the saved list, without a JOIN/PART now.
        assert!(chat.reconnect_config(network).unwrap().channels.is_empty());
        assert!(!chat.auto_join_enabled(network, "#a"));
        assert!(chat.channel_menu.is_none());

        // A disabled entry is enabled again in place; a new one is appended.
        menu(chat, "#b");
        chat.toggle_auto_join(true, cx);
        menu(chat, "#c");
        chat.toggle_auto_join(true, cx);
        assert_eq!(saved(chat), "-#a,#b,#c");
        let file = crate::settings_file::load().unwrap().unwrap();
        assert_eq!(file.selected_profile().unwrap().channels, "-#a,#b,#c");

        // `[]` and `{}` are the same channel under RFC 1459 case mapping.
        menu(chat, "#[x]");
        chat.toggle_auto_join(true, cx);
        assert!(chat.auto_join_enabled(network, "#{x}"));
        menu(chat, "#{x}");
        chat.toggle_auto_join(true, cx);
        assert_eq!(saved(chat), "-#a,#b,#c,#[x]");
        menu(chat, "#{X}");
        chat.toggle_auto_join(false, cx);
        assert_eq!(saved(chat), "-#a,#b,#c,-#[x]");
    });
    drop(file);
}

#[gpui::test]
fn colored_mentions_highlight_where_they_are_drawn(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.notifications.keywords = vec!["deploy".into()];
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                // A client that colors nicknames in replies.
                Event::ChannelMessage {
                    channel: "#a".into(),
                    sender: "bob".into(),
                    text: "\u{3}04alice\u{3}: \u{2}deploy\u{2}?".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        let message = chat.state.conversations()[0].messages.last().unwrap();
        let shown: Vec<_> = message
            .highlights
            .as_slice()
            .iter()
            .map(|range| &message.text[range.clone()])
            .collect();
        assert_eq!(shown, ["alice", "deploy"], "{:?}", message.text);
    });
}

#[gpui::test]
fn notices_are_shown_without_highlight_or_unread(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.notifications.keywords = vec!["deploy".into()];
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update(cx, |chat, cx| {
        let notice_channel = |text: &str| Event::ChannelMessage {
            channel: "#a".into(),
            sender: "bob".into(),
            text: text.into(),
            notice: true,
            server_time: None,
            msgid: None,
            account: None,
            replayed: false,
        };
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::Joined {
                    channel: "#b".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        // Select the other channel so #a counts as unselected.
        let b = chat.state.conversations()[1].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(b));
        chat.handle_events(
            NetworkId(1),
            vec![
                notice_channel("\u{3}04alice\u{3}: \u{2}deploy\u{2}?"),
                // A private NOTICE without a conversation goes to the
                // server log behind the sender.
                Event::PrivateMessage {
                    sender: "bot".into(),
                    text: "alice: deploy".into(),
                    notice: true,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        let a = chat.state.conversations()[0].id;
        let channel = &chat.state.conversations()[0];
        let server = chat.state.server_messages(NetworkId(1));
        for message in [channel.messages.last(), server.last()] {
            let message = message.unwrap();
            assert!(
                message.highlights.as_slice().is_empty(),
                "{:?}",
                message.text
            );
        }
        assert!(!chat.state.is_unread(a) && !chat.state.is_highlighted(a));

        // An existing private conversation shows the NOTICE without unread.
        chat.handle_events(
            NetworkId(1),
            vec![Event::PrivateMessage {
                sender: "carol".into(),
                text: "hi".into(),
                notice: false,
                server_time: None,
                msgid: None,
                account: None,
                replayed: false,
            }],
            Worker::Running,
            cx,
        );
        let carol = chat.state.conversations().last().unwrap().id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(carol));
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(b));
        let before = chat.state.conversations().last().unwrap().messages.len();
        chat.handle_events(
            NetworkId(1),
            vec![Event::PrivateMessage {
                sender: "carol".into(),
                text: "alice: deploy".into(),
                notice: true,
                server_time: None,
                msgid: None,
                account: None,
                replayed: false,
            }],
            Worker::Running,
            cx,
        );
        let conversation = chat.state.conversations().last().unwrap();
        assert_eq!(conversation.messages.len(), before + 1);
        assert!(
            conversation
                .messages
                .last()
                .unwrap()
                .highlights
                .as_slice()
                .is_empty()
        );
        assert!(!chat.state.is_unread(carol) && !chat.state.is_highlighted(carol));
    });
}

#[gpui::test]
fn action_verbs_and_senders_are_not_keywords_and_old_lines_stay(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.notifications.keywords = vec!["action".into(), "bob".into(), "deploy".into()];
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let message = |text: &str| Event::ChannelMessage {
        channel: "#a".into(),
        sender: "bob".into(),
        text: text.into(),
        notice: false,
        server_time: None,
        msgid: None,
        account: None,
        replayed: false,
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                // Neither the verb nor the sender is searched.
                message("\u{1}ACTION waves\u{1}"),
                message("\u{1}ACTION deploys\u{1}"),
            ],
            Worker::Running,
            cx,
        );
        let shown = |chat: &ChatWindow, index: usize| -> Vec<String> {
            let message = &chat.state.conversations()[0].messages[index];
            message
                .highlights
                .as_slice()
                .iter()
                .map(|range| message.text[range.clone()].to_owned())
                .collect()
        };
        let waves = chat.state.conversations()[0].messages.len() - 2;
        assert!(shown(chat, waves).is_empty());
        assert_eq!(shown(chat, waves + 1), ["deploy"]);

        // New keywords and a new nickname apply to later lines only.
        chat.notification_rules.keywords = vec!["hello".into()];
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::NickChanged {
                    nickname: "carol".into(),
                },
                message("hello carol, deploy"),
            ],
            Worker::Running,
            cx,
        );
        assert_eq!(shown(chat, waves + 1), ["deploy"]);
        let last = chat.state.conversations()[0].messages.len() - 1;
        assert_eq!(shown(chat, last), ["hello", "carol"]);
    });
}

#[gpui::test]
fn a_nickname_inside_a_link_is_not_a_mention(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(crate::settings_with_channels("#a"), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        let found = |text: &str| {
            let found = chat.find_mentions(NetworkId(1), "bob", text, false);
            let shown: Vec<_> = found
                .ranges
                .iter()
                .map(|r| text[r.clone()].to_owned())
                .collect();
            (found.mentioned, shown)
        };
        assert_eq!(
            found("https://example.org/users/alice/profile"),
            (false, vec![])
        );
        assert_eq!(
            found("see \u{3}04https://example.org/?u=alice\u{3}"),
            (false, vec![])
        );
        assert_eq!(
            found("alice: これ見て https://example.org/users/alice/"),
            (true, vec!["alice".to_owned()])
        );
        assert_eq!(
            found("https://example.org/alice alice"),
            (true, vec!["alice".to_owned()])
        );
        // Many links, each holding the nickname, still leave only the outside one.
        let many = format!("{}alice", "https://x/alice ".repeat(1000));
        let (mentioned, shown) = found(&many);
        assert!(mentioned);
        assert_eq!(shown, vec!["alice".to_owned()]);
    });
}

#[gpui::test]
fn excluded_text_cancels_overlapping_mentions_and_keywords(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.notifications.keywords = vec!["Dalk".into(), "ビルド".into()];
    settings.notifications.exclude_keywords = vec!["<dalk>".into(), "[ビルド]".into()];
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "dalk".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        let found = |text: &str| {
            let found = chat.find_mentions(NetworkId(1), "bob", text, false);
            let shown: Vec<_> = found
                .ranges
                .iter()
                .map(|r| text[r.clone()].to_owned())
                .collect();
            (found.mentioned, found.keyword, shown)
        };
        assert_eq!(found("Dalk、ちょっと聞いて").2, ["Dalk", "Dalk"]);
        assert_eq!(found("<dalk> こんにちは"), (false, false, vec![]));
        let (mentioned, keyword, shown) = found("<dalk> Dalk、返事して");
        assert!(mentioned && keyword);
        assert_eq!(shown, ["Dalk", "Dalk"]);
        assert_eq!(found("<DALK> Dalk!").2, ["Dalk", "Dalk"]);
        // Formatting codes do not hide the excluded text.
        assert_eq!(found("\u{2}<dalk>\u{2} \u{3}04hi"), (false, false, vec![]));
        assert_eq!(found("[ビルド] 失敗"), (false, false, vec![]));
        assert!(found("[ビルド] ビルド失敗").1);
    });
}

#[gpui::test]
fn highlights_and_private_messages_notify_unless_visible(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.language = cayenchat_storage::Language::English;
    settings.notifications.keywords = vec!["deploy".into()];
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let message = |channel: &str, text: &str| Event::ChannelMessage {
        channel: channel.into(),
        sender: "bob".into(),
        text: text.into(),
        notice: false,
        server_time: None,
        msgid: None,
        account: None,
        replayed: false,
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::Joined {
                    channel: "#b".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        chat.state.dispatch(cayenchat_app::Command::SelectChannel(
            chat.state.conversations()[0].id,
        ));
        chat.window_active = true;
        chat.handle_events(
            NetworkId(1),
            vec![
                message("#a", "alice: visible already"),
                message("#b", "hello"),
                message("#b", "\u{2}alice\u{2}: ping"),
                message("#b", "Deploy done"),
                message("#b", "\u{1}ACTION deploys\u{1}"),
                Event::ChannelMessage {
                    channel: "#b".into(),
                    sender: "bob".into(),
                    text: "alice: deploy from the backlog".into(),
                    notice: false,
                    replayed: true,
                    server_time: None,
                    msgid: None,
                    account: None,
                },
                Event::PrivateMessage {
                    sender: "carol".into(),
                    text: "old psst".into(),
                    notice: false,
                    replayed: true,
                    server_time: None,
                    msgid: None,
                    account: None,
                },
                Event::PrivateMessage {
                    sender: "carol".into(),
                    text: "psst".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
                Event::PrivateMessage {
                    sender: "NickServ".into(),
                    text: "notice".into(),
                    notice: true,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        chat.window_active = false;
        chat.handle_events(
            NetworkId(1),
            vec![message("#a", "alice: away now")],
            Worker::Running,
            cx,
        );
        assert!(
            chat.notifier.shown.iter().all(|n| !n.sound),
            "sound is off by default"
        );
        chat.notification_rules.mentions = false;
        chat.handle_events(
            NetworkId(1),
            vec![message("#a", "alice: ignored")],
            Worker::Running,
            cx,
        );
        chat.notification_burst = cayenchat_app::notifications::BurstLimiter::default();
        chat.notification_rules.sound = true;
        chat.notification_rules.mentions = true;
        chat.handle_events(
            NetworkId(1),
            vec![message("#a", "alice: with sound")],
            Worker::Running,
            cx,
        );
        let loud = chat.notifier.shown.pop().expect("sound notification");
        assert_eq!(loud.body, "alice: with sound");
        assert!(loud.sound);
        let summaries: Vec<_> = chat
            .notifier
            .shown
            .iter()
            .map(|n| (n.summary.as_str(), n.body.as_str()))
            .collect();
        assert_eq!(
            summaries,
            [
                ("bob in #b", "alice: ping"),
                ("bob in #b", "Deploy done"),
                ("bob in #b", "* bob deploys"),
                ("carol (private message)", "psst"),
                ("bob in #a", "alice: away now"),
            ]
        );
        let (a, b) = (
            &chat.state.conversations()[0],
            &chat.state.conversations()[1],
        );
        assert!(!chat.state.is_highlighted(a.id));
        assert!(chat.state.is_highlighted(b.id));
        // The highlighted parts of each message, with their offsets.
        let highlights = |index: usize| -> Vec<(usize, &str)> {
            let message = &b.messages[index];
            let ranges = message.highlights.as_slice().iter();
            ranges
                .map(|r| (r.start, &message.text[r.clone()]))
                .collect()
        };
        assert!(highlights(0).is_empty());
        assert_eq!(highlights(1), [(1, "alice")]);
        assert_eq!(highlights(2), [(0, "Deploy")]);
        // The line holds the action's own text, which is what is searched.
        assert!(b.messages[3].action);
        assert_eq!(b.messages[3].text, "deploys");
        assert_eq!(highlights(3), [(0, "deploy")]);
        assert!(b.messages[4].is_history());
        assert!(highlights(4).is_empty());

        // Replayed history leaves an unselected channel unmarked.
        let (a, b) = (a.id, b.id);
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(b));
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(a));
        chat.handle_events(
            NetworkId(1),
            vec![Event::ChannelMessage {
                channel: "#b".into(),
                sender: "tiarra".into(),
                text: "12:34 <bob> alice: deploy".into(),
                notice: true,
                replayed: true,
                server_time: None,
                msgid: None,
                account: None,
            }],
            Worker::Running,
            cx,
        );
        assert!(!chat.state.is_highlighted(b));
    });
}

#[gpui::test]
fn old_server_times_display_locally_and_still_notify(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a,#b");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let old = UNIX_EPOCH + Duration::from_secs(1_319_042_451);
    let local = |time: SystemTime| {
        use chrono::Timelike;
        let time = chrono::DateTime::<chrono::Local>::from(time);
        cayenchat_model::TimeOfDay::new(time.hour() as u8, time.minute() as u8)
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::Joined {
                    channel: "#b".into(),
                },
            ],
            Worker::Running,
            cx,
        );
        chat.state.dispatch(cayenchat_app::Command::SelectChannel(
            chat.state.conversations()[0].id,
        ));
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::ChannelMessage {
                    channel: "#b".into(),
                    sender: "bob".into(),
                    text: "now".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
                Event::ChannelMessage {
                    channel: "#b".into(),
                    sender: "bob".into(),
                    text: "alice: from years ago".into(),
                    notice: false,
                    server_time: Some(old),
                    msgid: None,
                    account: None,
                    replayed: false,
                },
            ],
            Worker::Running,
            cx,
        );
        let b = &chat.state.conversations()[1];
        assert_eq!(b.messages[1].text, "alice: from years ago");
        assert!(b.messages[0].sequence < b.messages[1].sequence);
        assert_eq!(b.messages[1].time, local(old));
        assert!(chat.state.is_unread(b.id));
        assert_eq!(
            chat.notifier.shown.len(),
            1,
            "an old timestamp does not mute"
        );
    });
}

#[gpui::test]
fn safe_channel_events_keep_messages_and_members_in_the_channel(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("");
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    let channel = "!ABCDEtest";
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: channel.into(),
                },
                Event::Names {
                    channel: channel.into(),
                    users: vec!["@alice".into(), "bob".into()],
                },
                Event::ChannelMessage {
                    channel: channel.into(),
                    sender: "bob".into(),
                    text: "hello".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
                Event::OutgoingAccepted {
                    local_id: None,
                    channel: channel.into(),
                    text: "reply".into(),
                    notice: false,
                    action: None,
                },
                Event::OutgoingAccepted {
                    local_id: None,
                    channel: channel.into(),
                    text: "notice".into(),
                    notice: true,
                    action: None,
                },
            ],
            Worker::Running,
            cx,
        );
        let conversations = chat.state.conversations();
        assert_eq!(conversations.len(), 1);
        let conversation = &conversations[0];
        assert_eq!(conversation.name, channel);
        assert!(chat.state.is_active_channel(conversation.id));
        assert_eq!(conversation.members, ["@alice", "bob"]);
        assert_eq!(conversation.messages.len(), 3);
        assert_eq!(conversation.messages[0].text, "hello");
        assert_eq!(conversation.messages[1].text, "reply");
        assert_eq!(conversation.messages[1].sender, "alice");
        assert_eq!(conversation.messages[2].text, "[NOTICE] notice");
        assert!(
            chat.inputs
                .contains_key(&Selection::Channel(conversation.id))
        );
        let id = conversation.id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(id));
        chat.handle_events(
            NetworkId(1),
            vec![Event::Parted {
                channel: channel.into(),
            }],
            Worker::Running,
            cx,
        );
        assert!(!chat.state.is_active_channel(id));
        assert!(chat.state.conversations()[0].members.is_empty());
    });
    cx.run_until_parked();
    // The server and the joined channel.
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.tree_list.state.item_count()),
        2
    );
}

#[gpui::test]
fn events_stay_with_their_server_and_removed_servers_disappear(cx: &mut TestAppContext) {
    use cayenchat_app::ConnectionStatus;
    use cayenchat_irc_core::{Event, WireDirection};
    use std::time::Duration;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.add_server("");
    settings.selected_profile_mut().unwrap().host = "irc.example.org".into();
    settings.selected_profile_mut().unwrap().channels = "#a".into();
    let custom = settings.selected_server.clone();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    // Tree order: the order the servers were added.
    let (ircnet, custom_net) = (NetworkId(1), NetworkId(2));
    let session_events = |channel: &str, text: &str| {
        vec![
            Event::Registered {
                nickname: "me".into(),
            },
            Event::Joined {
                channel: channel.into(),
            },
            Event::Wire {
                elapsed: Duration::ZERO,
                direction: WireDirection::Received,
                line: text.into(),
            },
            Event::ChannelMessage {
                channel: channel.into(),
                sender: "bob".into(),
                text: text.into(),
                notice: false,
                server_time: None,
                msgid: None,
                account: None,
                replayed: false,
            },
        ]
    };
    chat.update(cx, |chat, cx| {
        assert_eq!(chat.state.networks().len(), 2);
        assert_eq!(chat.sessions[&custom_net].profile_id, custom);
        chat.handle_events(
            custom_net,
            session_events("#a", "custom"),
            Worker::Running,
            cx,
        );
        chat.handle_events(ircnet, session_events("#a", "ircnet"), Worker::Running, cx);
        fn texts(chat: &ChatWindow, network: NetworkId) -> Vec<String> {
            chat.state
                .conversations()
                .iter()
                .filter(|c| c.network == network)
                .flat_map(|c| c.messages.iter().map(|m| m.text.clone()))
                .collect()
        }
        // Same channel name on two servers: separate conversations.
        assert_eq!(texts(chat, custom_net), ["custom"]);
        assert_eq!(texts(chat, ircnet), ["ircnet"]);
        assert_eq!(chat.sessions[&custom_net].diagnostics.len(), 1);
        assert_eq!(chat.sessions[&ircnet].diagnostics.len(), 1);

        // A disconnect on one server leaves the other registered.
        chat.handle_events(
            custom_net,
            vec![Event::Disconnected("gone".into())],
            Worker::Running,
            cx,
        );
        assert!(matches!(
            chat.state.status(custom_net),
            Some(ConnectionStatus::Disconnected(_))
        ));
        assert_eq!(
            chat.state.status(ircnet),
            Some(&ConnectionStatus::Registered)
        );

        // Removing the server from the settings drops it and its state.
        let removed: Vec<_> = chat
            .state
            .conversations()
            .iter()
            .filter(|c| c.network == custom_net)
            .map(|c| c.id)
            .collect();
        let mut next = settings.clone();
        next.remove_selected_server();
        chat.apply_servers(next, cx);
        assert_eq!(chat.state.networks().len(), 1);
        assert!(!chat.sessions.contains_key(&custom_net));
        assert!(
            removed
                .iter()
                .all(|id| !chat.inputs.contains_key(&Selection::Channel(*id)))
        );
        assert_eq!(texts(chat, ircnet), ["ircnet"]);
    });
    cx.run_until_parked();
}

#[gpui::test]
fn each_server_that_rejects_a_nickname_gets_its_own_field(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.add_server("");
    settings.selected_profile_mut().unwrap().host = "irc.example.org".into();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    let (first, second) = (NetworkId(1), NetworkId(2));
    let rejected = |nickname: &str| {
        vec![Event::NicknameRejected {
            nickname: nickname.into(),
        }]
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(first, rejected("alice"), Worker::Running, cx);
        chat.handle_events(second, rejected("bob"), Worker::Running, cx);
    });
    cx.run_until_parked();
    let rows = |chat: &ChatWindow, cx: &gpui::App| {
        chat.nick_prompts
            .iter()
            .map(|prompt| {
                (
                    prompt.network,
                    prompt.rejected.clone(),
                    prompt.input.read(cx).text().to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        chat.read_with(cx, |chat, cx| rows(chat, cx)),
        [
            (first, "alice".into(), "alice_".into()),
            (second, "bob".into(), "bob_".into())
        ]
    );
    // The first new row has focus; typing goes there, not into the draft.
    cx.simulate_input("x");
    assert_eq!(
        chat.read_with(cx, |chat, cx| rows(chat, cx))[0].2,
        "alice_x"
    );

    // A second rejection on the same server updates its row.
    chat.update(cx, |chat, cx| {
        chat.handle_events(first, rejected("alice_x"), Worker::Running, cx)
    });
    let after = chat.read_with(cx, |chat, cx| rows(chat, cx));
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], (first, "alice_x".into(), "alice_x_".into()));

    // Registration on one server closes only its row.
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            first,
            vec![Event::Registered {
                nickname: "alice_x_".into(),
            }],
            Worker::Running,
            cx,
        )
    });
    assert_eq!(
        chat.read_with(cx, |chat, cx| rows(chat, cx)),
        [(second, "bob".into(), "bob_".into())]
    );
}

#[gpui::test]
fn starts_without_servers_and_shows_one_once_added(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(Settings::default(), None, window, cx)
    });
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert!(chat.state.networks().is_empty());
        assert_eq!(chat.state.selection(), Selection::None);
        assert_eq!(chat.tree_list.state.item_count(), 0);
        assert!(chat.startup_connections.is_empty());
    });
    // The draft still accepts text and Enter reports that nothing is connected.
    cx.simulate_input("hello");
    cx.dispatch_action(super::SendMessage);
    chat.read_with(cx, |chat, _| assert!(!chat.notices.all().is_empty()));

    let settings = crate::settings_with_channels("#a");
    chat.update(cx, |chat, cx| chat.apply_servers(settings, cx));
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.state.networks().len(), 1);
        assert_eq!(chat.state.networks()[0].name, "irc.ircnet.com");
        assert_eq!(chat.tree_list.state.item_count(), 2);
    });
}

#[gpui::test]
fn typing_reuses_panes_and_new_messages_redraw_them(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ))
    });
    let settings = crate::settings_with_channels("#a,#b");
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    cx.run_until_parked();
    let (channel, input) = chat.update(cx, |chat, _| {
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
        (channel, chat.inputs[&Selection::Channel(channel)].clone())
    });
    cx.update(|window, cx| {
        window.focus(&input.focus_handle(cx));
        window.refresh();
    });
    cx.run_until_parked();
    let rendered = chat.read_with(cx, |chat, _| chat.pane_renders);
    assert!(rendered >= 4, "panes rendered {rendered} times");
    // The server row and the two configured channels.
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.tree_list.state.item_count()),
        3
    );

    cx.simulate_input("hello");
    cx.run_until_parked();
    assert_eq!(
        input.read_with(cx, |input, _| input.text().to_owned()),
        "hello"
    );
    assert_eq!(chat.read_with(cx, |chat, _| chat.pane_renders), rendered);

    chat.update(cx, |chat, cx| {
        let name = chat.state.conversations()[0].name.clone();
        chat.state.append_channel_message(
            NetworkId(1),
            &name,
            "bob",
            "hi",
            cayenchat_app::Kind::Message,
            cayenchat_model::Provenance::Live,
        );
        cx.notify();
    });
    cx.run_until_parked();
    assert!(chat.read_with(cx, |chat, _| chat.pane_renders) > rendered);
    let rows = chat.read_with(cx, |chat, _| {
        chat.main_lists[&Selection::Channel(channel)]
            .state
            .item_count()
    });
    assert!(rows >= 1);
}

#[gpui::test]
fn the_window_lays_out_at_its_smallest_size_and_recovers(cx: &mut TestAppContext) {
    use super::MIN_WINDOW_SIZE;
    use gpui::px;

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
    cx.run_until_parked();
    cx.simulate_resize(gpui::size(px(MIN_WINDOW_SIZE.0), px(MIN_WINDOW_SIZE.1)));
    cx.run_until_parked();
    let column = chat
        .read_with(cx, |chat, _| chat.right_column_bounds.get())
        .expect("the column was laid out");
    assert!(f32::from(column.size.width) >= 0. && f32::from(column.size.height) > 0.);
    cx.simulate_resize(gpui::size(px(960.), px(600.)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("members-split").is_some());
}

#[gpui::test]
fn switching_settings_tabs_resets_only_the_pane_scroll(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let settings = crate::settings_with_channels("#a");
    let owner =
        cx.add_window(|window, cx| ChatWindow::with_settings(settings.clone(), None, window, cx));
    let (form, cx) = cx.add_window_view(|window, cx| {
        let mut form = super::SettingsWindow::new(owner, settings.clone(), window, cx);
        form.tab = super::SettingsTab::Appearance;
        form
    });
    let scrolled = gpui::point(gpui::px(0.), gpui::px(-120.));
    form.update(cx, |form, _| {
        form.pane_scroll.set_offset(scrolled);
        form.nav_scroll.set_offset(scrolled);
    });

    // The same tab keeps its place.
    form.update(cx, |form, cx| {
        form.show_tab(super::SettingsTab::Appearance, cx)
    });
    assert_eq!(form.read_with(cx, |f, _| f.pane_scroll.offset()), scrolled);

    // Another tab starts at the top, and so does coming back; the nav stays.
    for tab in [
        super::SettingsTab::Connection,
        super::SettingsTab::Appearance,
    ] {
        form.update(cx, |form, cx| {
            form.pane_scroll.set_offset(scrolled);
            form.show_tab(tab, cx)
        });
        assert_eq!(
            form.read_with(cx, |f, _| f.pane_scroll.offset().y),
            gpui::px(0.)
        );
        assert_eq!(form.read_with(cx, |f, _| f.nav_scroll.offset()), scrolled);
    }
}

/// Runs the pending work and draws the window, so that what a change put on
/// screen can be found and reached by keys.
fn settle(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear());
}

#[gpui::test]
fn quitting_asks_first_only_when_switched_on(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    assert!(!settings.confirm_quit);
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();

    // Off: closing the window goes straight on, with no question.
    assert!(cx.simulate_close());
    assert!(!cx.has_pending_prompt());

    chat.update(cx, |chat, _| chat.confirm_quit = true);
    let input = chat.read_with(cx, |chat, _| chat.inputs[&chat.state.selection()].clone());
    cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
    cx.simulate_input("unsent");

    // On: the window stays and one question opens, however often asked.
    assert!(!cx.simulate_close());
    settle(cx);
    assert!(
        !cx.has_pending_prompt(),
        "the question is not a native prompt"
    );
    chat.read_with(cx, |chat, _| assert!(chat.quit_prompt_open));
    assert!(cx.debug_bounds("quit-dialog").is_some());
    assert!(!cx.simulate_close());
    cx.dispatch_action(super::Quit);
    settle(cx);
    // Escape cancels.
    cx.simulate_keystrokes("escape");
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(!chat.quit_prompt_open, "asked once"));

    // Cancelled: the draft and the focus are where they were, and the next
    // request asks again.
    input.read_with(cx, |input, _| assert_eq!(input.text(), "unsent"));
    cx.update(|window, cx| assert!(input.focus_handle(cx).is_focused(window)));
    chat.read_with(cx, |chat, _| assert!(!chat.quit_prompt_open));
    cx.dispatch_action(super::Quit);
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(chat.quit_prompt_open));
    cx.simulate_keystrokes("escape");
    settle(cx);
}

#[gpui::test]
fn quit_from_another_window_asks_in_the_chat_window(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    settings.confirm_quit = true;
    let app = cx.clone();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();

    // The settings window has no Quit handler, so the action ends up here.
    app.update(crate::request_quit_anywhere);
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| assert!(chat.quit_prompt_open));
    app.update(crate::request_quit_anywhere);
    settle(cx);
    cx.simulate_keystrokes("escape");
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(!chat.quit_prompt_open));
}

#[gpui::test]
fn closing_another_window_during_the_question_keeps_quitting_possible(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    settings.confirm_quit = true;
    let app = cx.clone();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();
    // A second window, opened last so that it is the active one, as the
    // settings window is when the user presses Ctrl+Q there.
    let other = app.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), |_, cx| {
            cx.new(|_| gpui::Empty)
        })
        .unwrap()
    });

    app.update(crate::request_quit_anywhere);
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| assert!(chat.quit_prompt_open));
    // The other window goes away before the question is answered.
    other
        .update(&mut app.clone(), |_, window, _| window.remove_window())
        .unwrap();
    app.update(|_| {});
    settle(cx);
    cx.simulate_keystrokes("escape");
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(!chat.quit_prompt_open));

    // The next request asks again instead of being ignored.
    app.update(crate::request_quit_anywhere);
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(chat.quit_prompt_open));
    cx.simulate_keystrokes("escape");
    settle(cx);
}

#[gpui::test]
fn quit_requested_while_the_chat_window_is_busy_still_asks(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    settings.confirm_quit = true;
    let app = cx.clone();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();

    // The global handler runs inside the chat window's own event handling
    // when the focused fallback prompt passes Quit on; the window is then
    // borrowed and cannot be updated.
    app.update(|cx| {
        let handle = cx.windows()[0].downcast::<ChatWindow>().unwrap();
        handle
            .update(cx, |_, _, cx| crate::request_quit_anywhere(cx))
            .unwrap();
    });
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert!(chat.quit_prompt_open, "asked after the event")
    });

    // Asking again while the question is open and the window is busy neither
    // quits nor opens a second question.
    app.update(|cx| {
        let handle = cx.windows()[0].downcast::<ChatWindow>().unwrap();
        handle
            .update(cx, |_, _, cx| crate::request_quit_anywhere(cx))
            .unwrap();
    });
    settle(cx);
    cx.simulate_keystrokes("escape");
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(!chat.quit_prompt_open));
}

fn quit_dialog_window(
    cx: &mut TestAppContext,
) -> (gpui::Entity<ChatWindow>, &mut gpui::VisualTestContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    settings.confirm_quit = true;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();
    cx.dispatch_action(super::Quit);
    settle(cx);
    (chat, cx)
}

#[gpui::test]
fn quit_question_keeps_enter_and_space_on_cancel(cx: &mut TestAppContext) {
    let (chat, cx) = quit_dialog_window(cx);
    let input = chat.read_with(cx, |chat, _| chat.inputs[&chat.state.selection()].clone());
    // The question is already open; cancel it, then focus the input, which
    // is where the focus must come back to.
    cx.simulate_keystrokes("escape");
    settle(cx);
    cx.update(|window, cx| window.focus(&input.focus_handle(cx)));

    for key in ["enter", "space"] {
        cx.dispatch_action(super::Quit);
        settle(cx);
        chat.read_with(cx, |chat, _| assert!(chat.quit_prompt_open));
        cx.simulate_keystrokes(key);
        settle(cx);
        chat.read_with(cx, |chat, _| {
            assert!(!chat.quit_prompt_open, "{key} cancels")
        });
        cx.update(|window, cx| assert!(input.focus_handle(cx).is_focused(window)));
    }
}

#[gpui::test]
fn quit_question_quits_only_through_the_confirm_button(cx: &mut TestAppContext) {
    let (chat, cx) = quit_dialog_window(cx);
    let input = chat.read_with(cx, |chat, _| chat.inputs[&chat.state.selection()].clone());
    // Tab goes to the confirm button and a second Tab back to Cancel, where
    // Enter cancels.
    cx.simulate_keystrokes("tab tab enter");
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(!chat.quit_prompt_open));

    // Clicking Cancel cancels; clicking the confirm button quits.
    cx.dispatch_action(super::Quit);
    settle(cx);
    let cancel = cx.debug_bounds("quit-dialog-cancel").unwrap();
    cx.simulate_click(cancel.center(), gpui::Modifiers::none());
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(!chat.quit_prompt_open));
    cx.update(|window, cx| window.focus(&input.focus_handle(cx)));

    cx.dispatch_action(super::Quit);
    settle(cx);
    let confirm = cx.debug_bounds("quit-dialog-confirm").unwrap();
    cx.simulate_click(confirm.center(), gpui::Modifiers::none());
    settle(cx);
    chat.read_with(cx, |chat, _| assert!(!chat.quit_prompt_open));
    // The test platform ignores `quit`; the focus is the visible difference:
    // cancelling gives it back to the input, quitting does not.
    cx.update(|window, cx| assert!(!input.focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn quit_question_lays_out_confirm_and_cancel_per_os(cx: &mut TestAppContext) {
    let (_chat, cx) = quit_dialog_window(cx);
    let confirm = cx.debug_bounds("quit-dialog-confirm").unwrap();
    let cancel = cx.debug_bounds("quit-dialog-cancel").unwrap();
    // Windows and Linux: confirm left of Cancel; macOS: the other way round.
    assert_eq!(
        confirm.origin.x < cancel.origin.x,
        !crate::button_order::cancel_first()
    );

    // Both orders, whatever OS the test runs on.
    struct Row(bool);
    impl gpui::Render for Row {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            crate::button_order::row_with(div().flex(), self.0)
                .child(div().debug_selector(|| "first".into()).child("first"))
                .child(div().debug_selector(|| "second".into()).child("second"))
        }
    }
    for cancel_first in [false, true] {
        let (_, cx) = cx.add_window_view(|_, _| Row(cancel_first));
        let first = cx.debug_bounds("first").unwrap();
        let second = cx.debug_bounds("second").unwrap();
        assert_eq!(first.origin.x > second.origin.x, cancel_first);
    }
}

#[gpui::test]
fn actions_are_kept_as_their_own_text_on_every_path(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{Event, HistoryMessage};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(crate::settings_with_channels("#a"), None, window, cx)
    });
    let channel_message = |text: &str, notice: bool| Event::ChannelMessage {
        channel: "#a".into(),
        sender: "bob".into(),
        text: text.into(),
        notice,
        server_time: None,
        msgid: None,
        account: None,
        replayed: false,
    };
    chat.update(cx, |chat, cx| {
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
                Event::HistoryRequested {
                    channel: "#a".into(),
                    resumed: false,
                },
                channel_message("\u{1}ACTION waves\u{1}", false),
                // No closing U+0001 is still an action.
                channel_message("\u{1}ACTION nods", false),
                // These are not, and keep being shown as they are.
                channel_message("\u{1}ACTION\u{1}", false),
                channel_message("\u{1}ACTIONwaves\u{1}", false),
                channel_message("\u{1}ACTION waves\u{1}", true),
                Event::PrivateMessage {
                    sender: "carol".into(),
                    text: "\u{1}ACTION sighs\u{1}".into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                },
                Event::OutgoingAccepted {
                    local_id: Some(1),
                    channel: "#a".into(),
                    text: "* dances".into(),
                    notice: false,
                    action: Some("dances".into()),
                },
                Event::OutgoingConfirmed {
                    local_id: 1,
                    text: None,
                    action: Some("dances!".into()),
                    msgid: None,
                    server_time: None,
                },
                Event::ChannelHistory {
                    channel: "#a".into(),
                    messages: vec![HistoryMessage {
                        sender: "bob".into(),
                        text: "\u{1}ACTION left\u{1}".into(),
                        notice: false,
                        server_time: None,
                        msgid: None,
                        account: None,
                    }],
                    incomplete: false,
                },
            ],
            Worker::Running,
            cx,
        );
        let shown = |conversation: usize| -> Vec<(bool, bool, String)> {
            chat.state.conversations()[conversation]
                .messages
                .iter()
                .filter(|message| !message.activity)
                .map(|message| (message.action, message.notice, message.text.clone()))
                .collect()
        };
        assert_eq!(
            shown(0),
            [
                (true, false, "left".to_owned()),
                (true, false, "waves".to_owned()),
                (true, false, "nods".to_owned()),
                (false, false, "\u{1}ACTION\u{1}".to_owned()),
                (false, false, "\u{1}ACTIONwaves\u{1}".to_owned()),
                (false, true, "[NOTICE] \u{1}ACTION waves\u{1}".to_owned()),
                (true, false, "dances!".to_owned()),
            ]
        );
        let private = chat.state.conversations().len() - 1;
        assert_eq!(shown(private), [(true, false, "sighs".to_owned())]);
    });
}

#[gpui::test]
fn member_prompt_buttons_follow_the_os_order(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        let network = chat.selected_network_id().unwrap();
        chat.show_member_prompt(
            network,
            String::new(),
            super::MemberPromptKind::Join,
            gpui::point(gpui::px(50.), gpui::px(50.)),
            window,
            cx,
        );
    });
    settle(cx);
    let submit = cx.debug_bounds("member-prompt-submit").unwrap();
    let cancel = cx.debug_bounds("member-prompt-cancel").unwrap();
    assert_eq!(
        submit.origin.x < cancel.origin.x,
        !crate::button_order::cancel_first()
    );
}

#[gpui::test]
fn quit_question_box_is_no_wider_than_its_content(cx: &mut TestAppContext) {
    let (_chat, cx) = quit_dialog_window(cx);
    let boxed = cx.debug_bounds("quit-dialog-box").unwrap();
    let cancel = cx.debug_bounds("quit-dialog-cancel").unwrap();
    let confirm = cx.debug_bounds("quit-dialog-confirm").unwrap();
    assert!(boxed.size.width <= gpui::px(360.));
    // The short English detail is narrower than the 360 px cap, so the
    // box is narrower than the cap rather than padded out to it.
    assert!(boxed.size.width < gpui::px(360.), "{:?}", boxed.size);
    let buttons_right = cancel.right().max(confirm.right());
    assert!(buttons_right <= boxed.right());
}

#[gpui::test]
fn quit_question_keeps_wrapped_connected_text_inside_its_box(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{Connection, ConnectionConfig};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    settings.confirm_quit = true;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    // A connection to a local listener that never registers: only the
    // "connected" wording of the question depends on it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = ConnectionConfig::tls("127.0.0.1".into(), "alice".into(), Vec::new());
    config.port = listener.local_addr().unwrap().port();
    config.use_tls = false;
    let connection = Connection::connect(config).unwrap();
    chat.update(cx, |chat, _| {
        chat.sessions.get_mut(&NetworkId(1)).unwrap().irc = Some(connection);
    });
    // Narrow enough that the long connected detail cannot stay on one line.
    cx.simulate_resize(gpui::size(gpui::px(300.), gpui::px(400.)));
    cx.run_until_parked();
    cx.dispatch_action(super::Quit);
    settle(cx);

    let boxed = cx.debug_bounds("quit-dialog-box").unwrap();
    let detail = cx.debug_bounds("quit-dialog-detail").unwrap();
    let cancel = cx.debug_bounds("quit-dialog-cancel").unwrap();
    let confirm = cx.debug_bounds("quit-dialog-confirm").unwrap();
    let overlay = cx.debug_bounds("quit-dialog").unwrap();
    assert!(
        boxed.size.width <= overlay.size.width,
        "{:?} in {:?}",
        boxed.size,
        overlay.size
    );
    // The text wrapped onto several lines, and the box grew with it.
    assert!(detail.size.height > gpui::px(30.), "{:?}", detail.size);
    for (name, inner) in [("detail", detail), ("cancel", cancel), ("confirm", confirm)] {
        assert!(
            inner.left() >= boxed.left()
                && inner.right() <= boxed.right()
                && inner.top() >= boxed.top()
                && inner.bottom() <= boxed.bottom(),
            "{name} {inner:?} outside {boxed:?}"
        );
    }
}

#[gpui::test]
fn quit_question_sits_near_the_top_of_the_window(cx: &mut TestAppContext) {
    let (_chat, cx) = quit_dialog_window(cx);
    let overlay = cx.debug_bounds("quit-dialog").unwrap();
    let boxed = cx.debug_bounds("quit-dialog-box").unwrap();
    assert!(boxed.origin.y - overlay.origin.y <= gpui::px(24.));
}

#[gpui::test]
fn the_title_leads_with_the_known_modes_of_the_selected_channel(cx: &mut TestAppContext) {
    use cayenchat_irc_core::Event;

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
        let modes = |flags: &str| Event::ChannelModes {
            channel: "#a".into(),
            flags: flags.into(),
        };
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
        // Unknown modes are left out rather than shown as `[+]`.
        assert!(chat.window_title().starts_with("#a @ "));
        chat.handle_events(NetworkId(1), vec![modes("snt")], Worker::Running, cx);
        assert!(
            chat.window_title().starts_with("[+snt] #a @ "),
            "{}",
            chat.window_title()
        );
        // Known and empty: no brackets.
        chat.handle_events(NetworkId(1), vec![modes("")], Worker::Running, cx);
        assert!(chat.window_title().starts_with("#a @ "));
        // Leaving forgets the modes.
        chat.handle_events(NetworkId(1), vec![modes("m")], Worker::Running, cx);
        chat.handle_events(
            NetworkId(1),
            vec![Event::Parted {
                channel: "#a".into(),
            }],
            Worker::Running,
            cx,
        );
        assert!(
            !chat.window_title().contains("[+"),
            "{}",
            chat.window_title()
        );
        // A late reply for a channel that is no longer joined is ignored.
        chat.handle_events(NetworkId(1), vec![modes("m")], Worker::Running, cx);
        assert!(
            !chat.window_title().contains("[+"),
            "{}",
            chat.window_title()
        );
    });
}

#[gpui::test]
fn server_settings_open_the_clicked_server_not_the_selected_one(cx: &mut TestAppContext) {
    use super::ServerMenu;

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.add_server("two.example");
    let first = settings.servers[0].id.clone();
    let second = settings.servers[1].id.clone();
    settings.selected_server = first.clone();
    let _file = crate::settings_file::TestFile::with(&settings);
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    for (index, expected) in [(1, &second), (0, &first), (1, &second)] {
        chat.update_in(cx, |chat, window, cx| {
            let before = chat.state.selection();
            let network = chat.state.networks()[index].id;
            chat.server_menu = Some(ServerMenu {
                position: gpui::point(gpui::px(10.), gpui::px(10.)),
                network,
            });
            chat.open_server_settings(window, cx);
            assert!(chat.server_menu.is_none());
            assert_eq!(chat.state.selection(), before);
        });
        let (tab, profile) = chat.read_with(cx, |chat, _| {
            let handle = chat.settings_window.unwrap();
            handle
                .read_with(&*cx, |settings, _| {
                    (
                        settings.tab,
                        settings.settings.values.selected_server.clone(),
                    )
                })
                .unwrap()
        });
        assert!(tab == crate::settings_window::SettingsTab::Connection);
        assert_eq!(&profile, expected);
    }
}

#[gpui::test]
fn the_regain_setting_reaches_an_automatic_reconnect(cx: &mut TestAppContext) {
    use cayenchat_irc_core::{ConnectionConfig, Event};

    cx.update(|cx| {
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ));
    });
    let mut settings = crate::settings_with_channels("#a");
    settings.language = cayenchat_storage::Language::English;
    let on = settings.clone();
    let mut off = settings.clone();
    off.servers[0].regain_ops = false;
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));
    chat.update(cx, |chat, cx| {
        let mut config = ConnectionConfig::tls("irc.example".into(), "alice".into(), Vec::new());
        config.regain_ops = true;
        chat.sessions.get_mut(&NetworkId(1)).unwrap().active_config = Some(config);
        chat.handle_events(
            NetworkId(1),
            vec![
                Event::Registered {
                    nickname: "alice".into(),
                },
                Event::Disconnected("connection reset".into()),
            ],
            Worker::Running,
            cx,
        );
        let config = chat.reconnect_config(NetworkId(1)).unwrap();
        assert!(config.regain_ops);
        // Switched off in the settings while connected: the reconnect obeys.
        chat.apply_servers(off, cx);
        let config = chat.reconnect_config(NetworkId(1)).unwrap();
        assert!(!config.regain_ops);
        chat.apply_servers(on, cx);
        let config = chat.reconnect_config(NetworkId(1)).unwrap();
        assert!(config.regain_ops);
    });
}
