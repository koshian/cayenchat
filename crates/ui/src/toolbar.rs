// SPDX-License-Identifier: GPL-3.0-only
//! The toolbar below the menu bar: the everyday actions as icon buttons.
//!
//! A button does what its menu item or shortcut does (it calls the same
//! handler of the chat window), so there is no second implementation. Whether
//! a button can be pressed is worked out in [`State`] when the chat state
//! changes; drawing only reads it. The bar is a cached view of its own, so
//! typing and new messages do not lay it out again.

use std::borrow::Cow;

use gpui::{prelude::*, *};

use crate::{ChatWindow, Connect, Disconnect, OpenSettings, Reconnect, tooltip::TextTooltip};
use crate::{key_recorder, shortcuts, theme::Theme};
use cayenchat_app::Command;

/// Height of the bar: the 28 px buttons with a little room around them.
pub const HEIGHT: Pixels = px(34.);
const BUTTON: f32 = 28.;
const ICON: f32 = 16.;
/// How long the Disconnect button is held to ask about every server.
const LONG_PRESS: std::time::Duration = std::time::Duration::from_millis(600);

/// What the buttons can do right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The selected server has a connection, one still opening or a retry
    /// waiting: the first button disconnects instead of connecting.
    pub connected: bool,
    pub can_connect: bool,
    /// The selected server has been connected in this run.
    pub can_reconnect: bool,
    /// The selected channel is joined: the button parts instead of joining.
    pub joined: bool,
    pub can_join_or_part: bool,
    pub can_next_unread: bool,
    /// The selected public channel's topic or mode can be changed, and the
    /// selected network's own nickname (#370).
    pub can_topic: bool,
    pub can_mode: bool,
    pub can_nick: bool,
    pub can_whois: bool,
    pub can_upload: bool,
    /// The windows are kept on top (Windows only).
    pub always_on_top: bool,
}

#[derive(Clone, Copy)]
enum Click {
    Connect,
    Disconnect,
    Reconnect,
    JoinOrPart,
    Topic,
    Mode,
    Nick,
    NextUnread,
    Whois,
    Upload,
    Search,
    AlwaysOnTop,
    Settings,
}

/// Where the shortcut shown in a tooltip comes from.
#[derive(Clone, Copy)]
enum Keys {
    None,
    /// A navigation action, whose keys the user can change.
    Action(&'static str),
    /// A shortcut that cannot be changed.
    Fixed(&'static str),
}

struct Button {
    id: &'static str,
    icon: &'static str,
    /// Locale key of the action's name.
    label: &'static str,
    keys: Keys,
    enabled: bool,
    /// For a button that switches something on and off: whether it is on.
    /// It is drawn pressed in, and the tooltip says which.
    toggled: Option<bool>,
    click: Click,
}

impl State {
    /// The buttons in order, with `None` for a separator between groups.
    /// The next notification joins the groups when that action exists (#369).
    fn layout(&self) -> Vec<Option<Button>> {
        self.layout_with(cfg!(target_os = "windows"))
    }

    /// [`layout`](Self::layout), saying whether the Windows-only "keep on
    /// top" button is among them.
    fn layout_with(&self, always_on_top_button: bool) -> Vec<Option<Button>> {
        let button = |id, icon, label, keys, enabled, click| Button {
            id,
            icon,
            label,
            keys,
            enabled,
            toggled: None,
            click,
        };
        let mut buttons = vec![
            // One button connects or disconnects, whichever the server needs.
            Some(if self.connected {
                button(
                    "connect",
                    "unplug",
                    "disconnect",
                    Keys::None,
                    true,
                    Click::Disconnect,
                )
            } else {
                button(
                    "connect",
                    "plug-zap",
                    "connect",
                    Keys::None,
                    self.can_connect,
                    Click::Connect,
                )
            }),
            Some(button(
                "reconnect",
                "refresh-cw",
                "reconnect",
                Keys::None,
                self.can_reconnect,
                Click::Reconnect,
            )),
            None,
            Some(if self.joined {
                button(
                    "join-part",
                    "log-out",
                    "toolbar_part",
                    Keys::None,
                    self.can_join_or_part,
                    Click::JoinOrPart,
                )
            } else {
                button(
                    "join-part",
                    "log-in",
                    "toolbar_join",
                    Keys::None,
                    self.can_join_or_part,
                    Click::JoinOrPart,
                )
            }),
            None,
            Some(button(
                "topic",
                "message-square-text",
                "toolbar_topic",
                Keys::None,
                self.can_topic,
                Click::Topic,
            )),
            Some(button(
                "mode",
                "sliders-horizontal",
                "toolbar_mode",
                Keys::None,
                self.can_mode,
                Click::Mode,
            )),
            Some(button(
                "nick",
                "user-pen",
                "toolbar_nick",
                Keys::None,
                self.can_nick,
                Click::Nick,
            )),
            None,
            Some(button(
                "next-unread",
                "chevrons-down",
                "shortcut_next_unread_channel",
                Keys::Action("next_unread_channel"),
                self.can_next_unread,
                Click::NextUnread,
            )),
            None,
            Some(button(
                "whois",
                "user-search",
                "toolbar_whois",
                Keys::None,
                self.can_whois,
                Click::Whois,
            )),
            Some(button(
                "upload",
                "image-up",
                "toolbar_upload",
                Keys::None,
                self.can_upload,
                Click::Upload,
            )),
            Some(button(
                "search",
                "search",
                "toolbar_search",
                Keys::Fixed(if cfg!(target_os = "macos") {
                    "cmd-f"
                } else {
                    "ctrl-f"
                }),
                true,
                Click::Search,
            )),
            None,
        ];
        if always_on_top_button {
            buttons.push(Some(Button {
                toggled: Some(self.always_on_top),
                ..button(
                    "always-on-top",
                    "bring-to-front",
                    "toolbar_always_on_top",
                    Keys::None,
                    true,
                    Click::AlwaysOnTop,
                )
            }));
        }
        buttons.push(Some(button(
            "settings",
            "settings",
            "menu_settings",
            Keys::Fixed(if cfg!(target_os = "macos") {
                "cmd-,"
            } else {
                "ctrl-,"
            }),
            true,
            Click::Settings,
        )));
        buttons
    }
}

/// The tooltip: the action's name and the keys that run it.
fn tooltip_text(name: &str, keys: &[String]) -> String {
    if keys.is_empty() {
        name.to_owned()
    } else {
        format!("{name} ({})", keys.join(", "))
    }
}

/// The tooltip for shortcut action `id` with the keys it has now.
pub(crate) fn action_tooltip(name: &str, id: &'static str, cx: &gpui::App) -> String {
    let overrides = cx
        .try_global::<crate::ShortcutPrefs>()
        .map(|prefs| prefs.overrides.clone())
        .unwrap_or_default();
    tooltip_text(name, &shown_keys(Keys::Action(id), &overrides))
}

/// The keys of `keys` now, labelled for display.
fn shown_keys(keys: Keys, overrides: &shortcuts::Overrides) -> Vec<String> {
    let keys: Vec<String> = match keys {
        Keys::None => Vec::new(),
        Keys::Fixed(key) => vec![key.to_owned()],
        Keys::Action(id) => shortcuts::ACTIONS
            .iter()
            .find(|action| action.id == id)
            .map(|action| shortcuts::keys(action, overrides))
            .unwrap_or_default(),
    };
    keys.iter().map(|key| key_recorder::describe(key)).collect()
}

/// A press on the Disconnect button, which becomes a long press once it has
/// been held for `LONG_PRESS`.
#[derive(Default)]
struct Hold {
    /// Counts the press down; dropped on release.
    timer: Option<Task<()>>,
    /// The press ran out, so the click that follows the release is not a
    /// plain Disconnect.
    fired: bool,
}

impl Hold {
    fn begin(&mut self, timer: Task<()>) {
        self.fired = false;
        self.timer = Some(timer);
    }

    fn fire(&mut self) {
        self.timer = None;
        self.fired = true;
    }

    /// The button was released on the button; its click follows.
    fn release(&mut self) {
        self.timer = None;
    }

    /// The button was released elsewhere, so no click follows.
    fn cancel(&mut self) {
        self.timer = None;
        self.fired = false;
    }

    /// Whether the click that arrived ends a long press, which has been
    /// handled already.
    fn take_click(&mut self) -> bool {
        std::mem::take(&mut self.fired)
    }
}

struct Toolbar {
    chat: WeakEntity<ChatWindow>,
    state: State,
    hold: Hold,
    _subscriptions: [Subscription; 2],
}

/// The toolbar's view, showing `state` until the chat window says otherwise.
pub fn view(state: State, cx: &mut Context<ChatWindow>) -> AnyView {
    let chat = cx.entity();
    cx.new(|cx: &mut Context<Toolbar>| Toolbar {
        chat: chat.downgrade(),
        state,
        hold: Hold::default(),
        _subscriptions: [
            // The state is worked out when the chat window changes, and the
            // bar redraws only if a button's availability or name did.
            cx.observe(&chat, |this, chat, cx| {
                let state = chat.read(cx).toolbar_state();
                if state != this.state {
                    this.state = state;
                    cx.notify();
                }
            }),
            // Light or dark, or other colors.
            cx.observe_global::<Theme>(|_, cx| cx.notify()),
        ],
    })
    .into()
}

impl Toolbar {
    fn button(&self, spec: Button, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let Button {
            id,
            icon,
            label,
            keys,
            enabled,
            toggled,
            click,
        } = spec;
        let chat = self.chat.clone();
        let pressed = toggled == Some(true);
        let color = if !enabled {
            theme.text_muted
        } else if pressed {
            theme.link
        } else {
            theme.text
        };
        div()
            .id(id)
            .debug_selector(|| format!("toolbar-{id}"))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(BUTTON))
            .rounded(px(4.))
            .when(pressed, |button| button.bg(theme.selected))
            .child(
                svg()
                    .path(format!("icons/ui/{icon}.svg"))
                    .size(px(ICON))
                    .text_color(color),
            )
            .when(enabled, |button| {
                let hover = theme.hover_strong;
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .when(matches!(click, Click::Disconnect), |button| {
                        // Holding the button asks about disconnecting every
                        // server; a plain click only ends the selected one.
                        button
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    let timer = cx.spawn_in(window, async move |this, cx| {
                                        cx.background_executor().timer(LONG_PRESS).await;
                                        let _ = this.update_in(cx, |this, window, cx| {
                                            this.hold.fire();
                                            let _ = this.chat.update(cx, |chat, cx| {
                                                chat.disconnect_all_action(window, cx)
                                            });
                                        });
                                    });
                                    this.hold.begin(timer);
                                }),
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, _| this.hold.release()),
                            )
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(|this, _, _, _| this.hold.cancel()),
                            )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Only the Disconnect button holds; another button's
                        // click is never swallowed by a stale long press.
                        if matches!(click, Click::Disconnect) && this.hold.take_click() {
                            return;
                        }
                        let _ = this.chat.update(cx, |chat, cx| match click {
                            Click::Connect => chat.connect_action(&Connect, window, cx),
                            Click::Disconnect => chat.disconnect_action(&Disconnect, window, cx),
                            Click::Reconnect => chat.reconnect_action(&Reconnect, window, cx),
                            Click::AlwaysOnTop => chat.toolbar_always_on_top(window, cx),
                            Click::JoinOrPart => chat.toolbar_join_or_part(cx),
                            Click::Topic => chat.toolbar_topic(window, cx),
                            Click::Mode => chat.toolbar_mode(window, cx),
                            Click::Nick => chat.toolbar_nick(window, cx),
                            Click::NextUnread => {
                                chat.dispatch(Command::NextUnreadChannel, window, cx)
                            }
                            Click::Whois => chat.toolbar_whois(cx),
                            Click::Upload => chat.toolbar_upload(window, cx),
                            Click::Search => chat.toggle_search(window, cx),
                            Click::Settings => chat.open_settings(&OpenSettings, window, cx),
                        });
                    }))
            })
            // Built when the pointer rests on the button, so a changed
            // shortcut or language shows without redrawing the bar.
            .tooltip(move |_, cx| {
                let (name, state) = chat
                    .read_with(cx, |chat, _| {
                        let state = toggled.map(|on| {
                            chat.i18n
                                .text(if on { "toolbar_on" } else { "toolbar_off" })
                        });
                        (chat.i18n.text(label), state)
                    })
                    .unwrap_or_default();
                let overrides = cx
                    .try_global::<crate::ShortcutPrefs>()
                    .map(|prefs| prefs.overrides.clone())
                    .unwrap_or_default();
                // The search key follows the key theme (F3 under Emacs).
                let keys = match keys {
                    Keys::Fixed("ctrl-f" | "cmd-f") => Keys::Fixed(crate::find_in_log_key(cx)),
                    other => other,
                };
                let mut text = tooltip_text(&name, &shown_keys(keys, &overrides));
                if let Some(state) = state {
                    text = format!("{text}: {state}");
                }
                cx.new(|_| TextTooltip(text.into())).into()
            })
    }
}

impl Render for Toolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::current(cx);
        let mut bar = div()
            .id("toolbar")
            .flex()
            .items_center()
            .gap(px(2.))
            .w_full()
            .h(HEIGHT)
            .px(px(6.))
            .flex_shrink_0()
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border);
        for spec in self.state.layout() {
            bar = match spec {
                Some(spec) => bar.child(self.button(spec, &theme, cx)),
                None => bar.child(
                    div()
                        .w(px(1.))
                        .h(px(ICON))
                        .mx(px(4.))
                        .flex_shrink_0()
                        .bg(theme.separator),
                ),
            };
        }
        bar
    }
}

/// The icons, compiled in: no files are read at run time.
pub struct Icons;

const ICONS: &[(&str, &[u8])] = &[
    (
        "icons/ui/arrow-down-to-line.svg",
        include_bytes!("../../../assets/icons/ui/arrow-down-to-line.svg"),
    ),
    (
        "icons/ui/bring-to-front.svg",
        include_bytes!("../../../assets/icons/ui/bring-to-front.svg"),
    ),
    (
        "icons/ui/chevron-down.svg",
        include_bytes!("../../../assets/icons/ui/chevron-down.svg"),
    ),
    (
        "icons/ui/chevron-up.svg",
        include_bytes!("../../../assets/icons/ui/chevron-up.svg"),
    ),
    (
        "icons/ui/circle-x.svg",
        include_bytes!("../../../assets/icons/ui/circle-x.svg"),
    ),
    (
        "icons/ui/ellipsis.svg",
        include_bytes!("../../../assets/icons/ui/ellipsis.svg"),
    ),
    (
        "icons/ui/unlink.svg",
        include_bytes!("../../../assets/icons/ui/unlink.svg"),
    ),
    (
        "icons/ui/link.svg",
        include_bytes!("../../../assets/icons/ui/link.svg"),
    ),
    (
        "icons/ui/search.svg",
        include_bytes!("../../../assets/icons/ui/search.svg"),
    ),
    (
        "icons/ui/x.svg",
        include_bytes!("../../../assets/icons/ui/x.svg"),
    ),
    (
        "icons/ui/chevrons-down.svg",
        include_bytes!("../../../assets/icons/ui/chevrons-down.svg"),
    ),
    (
        "icons/ui/image-up.svg",
        include_bytes!("../../../assets/icons/ui/image-up.svg"),
    ),
    (
        "icons/ui/log-in.svg",
        include_bytes!("../../../assets/icons/ui/log-in.svg"),
    ),
    (
        "icons/ui/log-out.svg",
        include_bytes!("../../../assets/icons/ui/log-out.svg"),
    ),
    (
        "icons/ui/message-square-text.svg",
        include_bytes!("../../../assets/icons/ui/message-square-text.svg"),
    ),
    (
        "icons/ui/plug-zap.svg",
        include_bytes!("../../../assets/icons/ui/plug-zap.svg"),
    ),
    (
        "icons/ui/sliders-horizontal.svg",
        include_bytes!("../../../assets/icons/ui/sliders-horizontal.svg"),
    ),
    (
        "icons/ui/refresh-cw.svg",
        include_bytes!("../../../assets/icons/ui/refresh-cw.svg"),
    ),
    (
        "icons/ui/settings.svg",
        include_bytes!("../../../assets/icons/ui/settings.svg"),
    ),
    (
        "icons/ui/unplug.svg",
        include_bytes!("../../../assets/icons/ui/unplug.svg"),
    ),
    (
        "icons/ui/user-pen.svg",
        include_bytes!("../../../assets/icons/ui/user-pen.svg"),
    ),
    (
        "icons/ui/user-search.svg",
        include_bytes!("../../../assets/icons/ui/user-search.svg"),
    ),
];

impl AssetSource for Icons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Button, Click, Icons, Keys, State, key_recorder, shortcuts, shown_keys, tooltip_text,
    };
    use gpui::{AssetSource, Task};

    #[test]
    fn a_long_press_swallows_the_click_that_follows_its_release() {
        let mut hold = super::Hold::default();
        assert!(!hold.take_click(), "a plain click goes through");
        hold.begin(Task::ready(()));
        hold.release();
        assert!(!hold.take_click(), "released before the time ran out");
        hold.begin(Task::ready(()));
        hold.fire();
        assert!(hold.timer.is_none());
        hold.release();
        assert!(hold.take_click(), "the click after a long press is ignored");
        assert!(!hold.take_click(), "and only that one");
    }

    #[test]
    fn a_long_press_released_elsewhere_leaves_nothing_behind() {
        let mut hold = super::Hold::default();
        hold.begin(Task::ready(()));
        hold.fire();
        hold.cancel();
        assert!(!hold.take_click(), "no click follows, so the next is real");
        // A fresh press also forgets a flag the release never cleared.
        hold.begin(Task::ready(()));
        hold.fire();
        hold.begin(Task::ready(()));
        assert!(!hold.take_click());
    }

    fn buttons(state: State) -> Vec<Button> {
        state.layout().into_iter().flatten().collect()
    }

    #[test]
    fn tooltips_name_the_action_and_its_keys() {
        assert_eq!(tooltip_text("Disconnect", &[]), "Disconnect");
        assert_eq!(
            tooltip_text(
                "Next unread channel",
                &["Ctrl+Tab".into(), "Alt+Space".into()]
            ),
            "Next unread channel (Ctrl+Tab, Alt+Space)"
        );
    }

    #[test]
    fn a_changed_navigation_shortcut_shows_in_the_tooltip() {
        let mut overrides = shortcuts::Overrides::new();
        overrides.insert("next_unread_channel".into(), "ctrl-j".into());
        assert_eq!(
            shown_keys(Keys::Action("next_unread_channel"), &overrides),
            vec![key_recorder::describe("ctrl-j")]
        );
        assert_ne!(
            shown_keys(
                Keys::Action("next_unread_channel"),
                &shortcuts::Overrides::new()
            ),
            shown_keys(Keys::Action("next_unread_channel"), &overrides)
        );
        assert!(shown_keys(Keys::None, &overrides).is_empty());
    }

    #[test]
    fn one_button_connects_or_disconnects() {
        let first = |state| buttons(state).remove(0);
        let idle = first(State {
            can_connect: true,
            ..State::default()
        });
        assert_eq!((idle.icon, idle.label), ("plug-zap", "connect"));
        assert!(idle.enabled && matches!(idle.click, Click::Connect));
        // While connecting or connected the same place disconnects.
        let busy = first(State {
            connected: true,
            can_connect: true,
            ..State::default()
        });
        assert_eq!((busy.icon, busy.label), ("unplug", "disconnect"));
        assert!(busy.enabled && matches!(busy.click, Click::Disconnect));
        // No server to act on: connect is there but cannot be pressed.
        assert!(!first(State::default()).enabled);
    }

    #[test]
    fn reconnect_is_its_own_button_for_a_server_connected_before() {
        let reconnect = |can_reconnect, connected| {
            buttons(State {
                can_reconnect,
                connected,
                ..State::default()
            })
            .into_iter()
            .find(|button| button.id == "reconnect")
            .map(|button| {
                (
                    button.icon,
                    button.enabled,
                    matches!(button.click, Click::Reconnect),
                )
            })
        };
        assert_eq!(reconnect(false, false), Some(("refresh-cw", false, true)));
        assert_eq!(reconnect(true, false), Some(("refresh-cw", true, true)));
        // The caller greys it out while a connection is live.
        assert_eq!(reconnect(false, true), Some(("refresh-cw", false, true)));
    }

    #[test]
    fn the_channel_button_joins_or_parts() {
        let channel = |joined| {
            buttons(State {
                joined,
                ..State::default()
            })
            .into_iter()
            .find(|button| button.id == "join-part")
            .map(|button| (button.icon, button.label))
        };
        assert_eq!(channel(false), Some(("log-in", "toolbar_join")));
        assert_eq!(channel(true), Some(("log-out", "toolbar_part")));
    }

    #[test]
    fn the_keep_on_top_button_is_a_toggle_beside_settings() {
        let state = State {
            always_on_top: true,
            ..State::default()
        };
        let ids = |shown| {
            state
                .layout_with(shown)
                .into_iter()
                .flatten()
                .map(|button| button.id)
                .collect::<Vec<_>>()
        };
        assert!(!ids(false).contains(&"always-on-top"));
        let ids = ids(true);
        assert_eq!(&ids[ids.len() - 2..], ["always-on-top", "settings"]);
        let toggled = |always_on_top| {
            State {
                always_on_top,
                ..State::default()
            }
            .layout_with(true)
            .into_iter()
            .flatten()
            .find(|button| button.id == "always-on-top")
            .map(|button| (button.icon, button.enabled, button.toggled))
        };
        assert_eq!(toggled(false), Some(("bring-to-front", true, Some(false))));
        assert_eq!(toggled(true), Some(("bring-to-front", true, Some(true))));
    }

    #[test]
    fn unavailable_buttons_are_disabled_and_settings_and_search_never_are() {
        let state = State::default();
        for shown in [false, true] {
            for button in state.layout_with(shown).into_iter().flatten() {
                let always_enabled = ["settings", "always-on-top", "search"].contains(&button.id);
                assert_eq!(button.enabled, always_enabled, "{}", button.id);
            }
        }
    }

    #[test]
    fn the_search_button_names_the_shortcut_that_opens_the_same_bar() {
        let search = State::default()
            .layout_with(false)
            .into_iter()
            .flatten()
            .find(|button| button.id == "search")
            .unwrap();
        assert_eq!(search.icon, "search");
        let keys = shown_keys(search.keys, &shortcuts::Overrides::new());
        let expected = if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        };
        assert_eq!(keys, [key_recorder::describe(expected)]);
    }

    #[test]
    fn every_icon_used_is_compiled_in() {
        let icons = Icons;
        for state in [
            State::default(),
            State {
                connected: true,
                joined: true,
                ..State::default()
            },
        ] {
            for button in state.layout_with(true).into_iter().flatten() {
                let loaded = icons
                    .load(&format!("icons/ui/{}.svg", button.icon))
                    .unwrap();
                assert!(loaded.is_some(), "{}", button.icon);
            }
        }
    }
}
