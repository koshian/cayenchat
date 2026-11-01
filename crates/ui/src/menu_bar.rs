// SPDX-License-Identifier: GPL-3.0-only
//! Alt/hover-revealed menus for platforms without GPUI native menu rendering.
use gpui::{prelude::*, *};
use std::time::Duration;

const HEIGHT: f32 = 28.;
/// Invisible strip at the top of the content that reveals the bar on hover.
/// Alt alone is not treated as a menu request; Alt+letter and hover remain available.
pub const HOT_ZONE: f32 = 6.;
const HOVER_REVEAL_DELAY: Duration = Duration::from_millis(400);
/// How often a hover-revealed bar checks whether the pointer has left it.
const HOVER_POLL: Duration = Duration::from_millis(150);
/// Extra distance below the bar the pointer may drift before it hides.
const HOVER_SLACK: f32 = 12.;
const REVEAL_ANIMATION: Duration = Duration::from_millis(140);

/// Access keys of the top-level menus, in menu order (Alt+key opens one).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessKeys {
    /// Lower-case key of each menu; `None` for a menu without one.
    pub keys: Vec<Option<char>>,
    /// Show the key as "Name(K)" always (Japanese convention) instead of
    /// underlining it while Alt or the bar is in use.
    pub suffix: bool,
}

impl AccessKeys {
    fn menu_for(&self, key: &Keystroke) -> Option<usize> {
        let modifiers = key.modifiers;
        // Exactly Alt: Ctrl+Alt is AltGr on Windows and Shift+Alt is a layout
        // switch, neither is a menu key.
        if !modifiers.alt || modifiers.control || modifiers.shift || modifiers.platform {
            return None;
        }
        let mut chars = key.key.chars();
        let (Some(pressed), None) = (chars.next(), chars.next()) else {
            return None;
        };
        let pressed = pressed.to_ascii_lowercase();
        self.keys.iter().position(|k| *k == Some(pressed))
    }
}

#[derive(Default)]
pub struct MenuBar {
    /// The bar is on screen all the time (the default). `visible` then only
    /// says that it is being used from the keyboard or an open menu.
    always: bool,
    visible: bool,
    active: usize,
    open: bool,
    selected: Option<usize>,
    alt_down: bool,
    /// Shown by hovering, so leaving the bar hides it again.
    hover_reveal: bool,
    /// Invalidates pending hover timers when the pointer moves on.
    hover_generation: u64,
    /// Restarts the slide-in animation for each reveal.
    reveals: usize,
    /// The current reveal slides in. Fixed for the whole reveal: switching
    /// the bar between animated and plain would restart the animation.
    slide: bool,
    _keys: Option<Subscription>,
}

impl MenuBar {
    pub fn new<V: 'static>(
        window: &Window,
        cx: &mut Context<V>,
        access: fn(&mut V) -> &mut MenuBar,
    ) -> Self {
        if cfg!(target_os = "macos") {
            return Self::default();
        }
        Self::new_in_window(window, cx, access)
    }

    fn new_in_window<V: 'static>(
        window: &Window,
        cx: &mut Context<V>,
        access: fn(&mut V) -> &mut MenuBar,
    ) -> Self {
        let handle = window.window_handle();
        let owner = cx.weak_entity();
        // GPUI dispatches key bindings before element key listeners. Intercept
        // first so Enter/arrows cannot send or edit a draft while using menus.
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != handle {
                return;
            }
            let _ = owner.update(cx, |this, cx| {
                let state = access(this);
                let key = &event.keystroke;
                if key.key == "f10" && !key.modifiers.modified() {
                    // Same as View → Hide/Show Menu Bar, which also saves it.
                    state.close();
                    window.dispatch_action(crate::ToggleMenuBar.boxed_clone(), cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !state.visible {
                    return;
                }
                let menus = cx.get_menus().unwrap_or_default();
                let enabled = enabled_items(&menus, window, cx);
                if state.key_down(&event.keystroke, &menus, &enabled, window, cx) {
                    cx.stop_propagation();
                }
                cx.notify();
            });
        });
        Self {
            _keys: Some(keys),
            ..Self::default()
        }
    }

    fn key_down(
        &mut self,
        key: &Keystroke,
        menus: &[OwnedMenu],
        enabled: &[Vec<usize>],
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        if key.modifiers.modified() || menus.is_empty() {
            self.close();
            return false;
        }
        match key.key.as_str() {
            "escape" => self.close(),
            "left" | "right" => self.move_menu(key.key == "left", menus.len()),
            "up" | "down" => self.move_item(key.key == "up", &enabled[self.active]),
            "enter" | "space" => {
                // Revealing the bar alone does not select a menu item: leave
                // Enter/Space to the input instead of opening a menu.
                let (true, Some(row)) = (self.open, self.selected) else {
                    self.close();
                    return false;
                };
                if enabled[self.active].contains(&row)
                    && let OwnedMenuItem::Action { action, .. } = &menus[self.active].items[row]
                {
                    window.dispatch_action(action.boxed_clone(), cx);
                }
                self.close();
            }
            _ => {
                self.close();
                return false;
            }
        }
        true
    }

    /// Shows the bar all the time (`true`) or only on request (`false`).
    pub fn set_always(&mut self, always: bool) {
        self.always = always;
    }

    /// Whether the bar is shown all the time.
    pub fn always(&self) -> bool {
        self.always
    }

    /// Whether the bar takes room on screen.
    fn shown(&self) -> bool {
        self.visible || self.always
    }

    pub fn height(&self) -> Pixels {
        px(if self.shown() && !cfg!(target_os = "macos") {
            HEIGHT
        } else {
            0.
        })
    }

    fn close(&mut self) {
        self.visible = false;
        self.open = false;
        self.selected = None;
        self.hover_reveal = false;
        self.hover_generation += 1;
    }

    fn reveal(&mut self, by_hover: bool) {
        self.visible = true;
        self.hover_reveal = by_hover;
        self.active = 0;
        self.open = false;
        self.selected = None;
        self.reveals += 1;
        self.slide = true;
    }

    /// Alt held together with a menu's letter opens that menu. Only reached
    /// by keys no shortcut took. Releasing Alt first does not count: an IME
    /// toggle can look like a lone Alt, and typed letters must not open menus.
    fn access_key(
        &mut self,
        key: &Keystroke,
        keys: &AccessKeys,
        menu_count: usize,
        composing: bool,
    ) -> bool {
        let Some(index) = keys.menu_for(key).filter(|index| *index < menu_count) else {
            return false;
        };
        if composing {
            return false;
        }
        self.hover_reveal = false;
        self.hover_generation += 1;
        if !self.visible {
            self.reveal(false);
            // The popup lives inside the clipped bar, so it would flicker
            // while the bar grows.
            self.slide = false;
        }
        self.active = index;
        self.open = true;
        self.selected = None;
        true
    }

    fn modifiers_changed(&mut self, modifiers: Modifiers) {
        self.alt_down = modifiers.alt;
    }

    fn move_menu(&mut self, backwards: bool, count: usize) {
        self.active = (self.active + if backwards { count - 1 } else { 1 }) % count;
        self.selected = None;
    }

    fn move_item(&mut self, backwards: bool, items: &[usize]) {
        self.open = true;
        if items.is_empty() {
            self.selected = None;
            return;
        }
        let position = self
            .selected
            .and_then(|item| items.iter().position(|ix| *ix == item));
        let next = match position {
            Some(position) => {
                (position + if backwards { items.len() - 1 } else { 1 }) % items.len()
            }
            None if backwards => items.len() - 1,
            None => 0,
        };
        self.selected = Some(items[next]);
    }
}

fn enabled_items(menus: &[OwnedMenu], window: &Window, cx: &mut App) -> Vec<Vec<usize>> {
    menus
        .iter()
        .map(|menu| {
            menu.items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| match item {
                    OwnedMenuItem::Action { action, .. }
                        if window.is_action_available(action.as_ref(), cx) =>
                    {
                        Some(index)
                    }
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// Keep focus in the original text field so Edit actions use its selection.
/// MenuBar::new intercepts menu keys before that field handles them.
#[allow(clippy::too_many_arguments)] // Each is one independent hook of the caller's view.
pub fn wrap<V: 'static>(
    state: &MenuBar,
    menus: Vec<OwnedMenu>,
    access_keys: AccessKeys,
    content: AnyElement,
    access: fn(&mut V) -> &mut MenuBar,
    composing: fn(&V, &App) -> bool,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    if cfg!(target_os = "macos") {
        return content;
    }
    wrap_in_window(
        state,
        menus,
        access_keys,
        content,
        access,
        composing,
        window,
        cx,
    )
}

/// The heading text: the name with its access key as a "(K)" suffix, or with
/// the key underlined while Alt or the bar is in use.
fn heading(name: &str, key: Option<char>, suffix: bool, underline: bool) -> Div {
    let label = div().flex().whitespace_nowrap();
    let Some(key) = key else {
        return label.child(name.to_owned());
    };
    if suffix {
        return label.child(format!("{name}({})", key.to_ascii_uppercase()));
    }
    let found = name
        .char_indices()
        .find(|(_, c)| c.to_ascii_lowercase() == key);
    match found {
        Some((at, c)) if underline => {
            let end = at + c.len_utf8();
            label
                .child(name[..at].to_owned())
                .child(div().underline().child(name[at..end].to_owned()))
                .child(name[end..].to_owned())
        }
        _ => label.child(name.to_owned()),
    }
}

// Kept platform-independent so first-frame rendering is tested on macOS too.
#[allow(clippy::too_many_arguments)]
fn wrap_in_window<V: 'static>(
    state: &MenuBar,
    menus: Vec<OwnedMenu>,
    access_keys: AccessKeys,
    content: AnyElement,
    access: fn(&mut V) -> &mut MenuBar,
    composing: fn(&V, &App) -> bool,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = crate::theme::current(cx);
    let mut bar = div()
        .id("window-menu-bar")
        .flex()
        .h(px(HEIGHT))
        .flex_shrink_0()
        .overflow_hidden()
        .bg(theme.surface)
        .border_b_1()
        .border_color(theme.border);
    if state.shown() {
        // The initial frame has no rendered dispatch tree, and a bar that is
        // always shown is drawn in it. Availability is therefore asked only
        // while a menu is open, which needs a click or key after painting.
        let enabled = if state.open {
            enabled_items(&menus, window, cx)
        } else {
            Vec::new()
        };
        for (index, menu) in menus.iter().enumerate() {
            let mut label = div()
                .id(("menu-heading", index))
                .relative()
                .px_2()
                .h_full()
                .cursor_pointer()
                // While the bar is in use only the active heading is lit, so
                // a hover highlight never adds a second one.
                .when(!state.visible, |d| d.hover(|d| d.bg(theme.hover_strong)))
                .when(state.visible && state.active == index, |d| {
                    d.bg(theme.selected)
                })
                // With a menu open, pointing at another heading moves to it.
                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                    let state = access(this);
                    if state.open && state.active != index {
                        state.active = index;
                        state.selected = None;
                        cx.notify();
                    }
                }))
                .child(heading(
                    &menu.name,
                    access_keys.keys.get(index).copied().flatten(),
                    access_keys.suffix,
                    state.alt_down || state.visible,
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        let state = access(this);
                        if state.open && state.active == index {
                            // Clicking the open menu again closes it and
                            // leaves no heading selected.
                            state.close();
                        } else {
                            state.hover_reveal = false;
                            // Using the bar by mouse also lets the keyboard
                            // move around it, as when it was revealed.
                            state.visible = true;
                            state.open = true;
                            state.active = index;
                            state.selected = None;
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                );
            if state.open && state.active == index {
                let mut popup = div()
                    .id("window-menu-popup")
                    .absolute()
                    .top(px(HEIGHT))
                    .left_0()
                    .min_w(px(240.))
                    .p_1()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_md()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
                for (row, item) in menu.items.iter().enumerate() {
                    match item {
                        OwnedMenuItem::Separator => {
                            popup =
                                popup.child(div().my_1().border_t_1().border_color(theme.border));
                        }
                        OwnedMenuItem::Action { name, action, .. } => {
                            let available = enabled[index].contains(&row);
                            let action = action.boxed_clone();
                            popup = popup.child(
                                div()
                                    .id(("window-menu-item", row))
                                    .px_2()
                                    .py_1()
                                    .whitespace_nowrap()
                                    .child(name.clone())
                                    .when(state.selected == Some(row), |d| d.bg(theme.selected))
                                    .when(!available, |d| d.text_color(theme.text_muted))
                                    .when(available, |d| {
                                        d.cursor_pointer()
                                            .hover(|d| d.bg(theme.hover_strong))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                access(this).close();
                                                window.dispatch_action(action.boxed_clone(), cx);
                                                cx.stop_propagation();
                                                cx.notify();
                                            }))
                                    }),
                            );
                        }
                        // The shared app menus only contain actions/separators
                        // on Linux and Windows. OS-managed submenus are macOS-only.
                        _ => {}
                    }
                }
                label = label.child(deferred(popup).with_priority(10));
            }
            bar = bar.child(label);
        }
    }
    let hot_zone = div()
        .id("window-menu-hot-zone")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(HOT_ZONE))
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            let state = access(this);
            state.hover_generation += 1;
            if *hovered {
                reveal_on_hover(state.hover_generation, access, window, cx);
            }
        }));
    // A bar that is always there does not slide in (and cannot stop half way).
    // Neither does one revealed by an access key (see `access_key`).
    let bar = if state.always || !state.slide {
        bar.into_any_element()
    } else {
        bar.with_animation(
            ("window-menu-reveal", state.reveals),
            Animation::new(REVEAL_ANIMATION).with_easing(ease_out_quint()),
            |bar, delta| bar.h(px(HEIGHT * delta)),
        )
        .into_any_element()
    };
    let menu_count = menus.len();
    div()
        .id("window-menu-root")
        // Bubble phase: a key that a shortcut took never gets here, so a
        // user's Alt+letter shortcut wins over the access key.
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
            if event.is_held {
                return;
            }
            let composing = composing(this, cx);
            if access(this).access_key(&event.keystroke, &access_keys, menu_count, composing) {
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .text_size(px(13.))
        .line_height(px(26.))
        .text_color(theme.text)
        .on_modifiers_changed(
            cx.listener(move |this, event: &ModifiersChangedEvent, _, cx| {
                access(this).modifiers_changed(event.modifiers);
                cx.notify();
            }),
        )
        .when(state.shown(), |d| d.child(bar))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .child(content)
                .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                    let state = access(this);
                    // Only a click that dismisses an open menu is spent. A bar
                    // that is merely shown stays: closing it on press would
                    // shift the content before the release and cancel the click.
                    if state.visible && state.open {
                        state.close();
                        cx.stop_propagation();
                        cx.notify();
                    }
                })),
        )
        .when(!state.shown(), |d| d.child(hot_zone))
        .into_any_element()
}

/// Pointer height relative to the top of the menu area.
fn pointer_depth(window: &Window) -> Pixels {
    window.mouse_position().y - crate::decorations::content_origin(window).y
}

/// Reveals the bar if the pointer is still resting in the hot zone after the
/// delay, then hides it again once the pointer moves away from the bar.
fn reveal_on_hover<V: 'static>(
    generation: u64,
    access: fn(&mut V) -> &mut MenuBar,
    window: &Window,
    cx: &mut Context<V>,
) {
    cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(HOVER_REVEAL_DELAY).await;
        let revealed = this.update_in(cx, |this, window, cx| {
            let state = access(this);
            if state.hover_generation != generation
                || state.visible
                || pointer_depth(window) >= px(HOT_ZONE)
            {
                return false;
            }
            state.reveal(true);
            cx.notify();
            true
        });
        if !revealed.unwrap_or(false) {
            return;
        }
        loop {
            cx.background_executor().timer(HOVER_POLL).await;
            let keep = this.update_in(cx, |this, window, cx| {
                let state = access(this);
                if !state.visible || !state.hover_reveal {
                    return false;
                }
                if state.open || pointer_depth(window) <= px(HEIGHT + HOVER_SLACK) {
                    return true;
                }
                state.close();
                cx.notify();
                false
            });
            if !keep.unwrap_or(false) {
                return;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{MenuBar, Modifiers};
    use gpui::px;

    #[gpui::test]
    fn an_always_shown_bar_is_drawn_on_the_first_frame_and_opens_by_click(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{
            Context, Focusable, IntoElement, Menu, MenuItem, MouseButton, Render, Window, div,
            prelude::*,
        };

        struct View {
            menu: MenuBar,
            input: gpui::Entity<crate::input::TextInput>,
        }
        impl Render for View {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let content = div()
                    .key_context("ChatWindow")
                    .child(self.input.clone())
                    .on_action(|_: &crate::ToggleDebug, _, _| {})
                    .into_any_element();
                super::wrap_in_window(
                    &self.menu,
                    vec![
                        Menu {
                            name: "View".into(),
                            items: vec![MenuItem::action("Toggle diagnostics", crate::ToggleDebug)],
                            menu_type: None,
                        }
                        .owned(),
                    ],
                    super::AccessKeys {
                        keys: vec![Some('v')],
                        suffix: false,
                    },
                    content,
                    |this| &mut this.menu,
                    |_, _| false,
                    window,
                    cx,
                )
            }
        }

        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ))
        });
        // The first frame has no dispatch tree: drawing the bar must not ask
        // which actions are available (that used to panic on startup).
        let (view, cx) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| crate::input::TextInput::new_live("Draft", cx));
            window.focus(&input.focus_handle(cx));
            let mut menu = MenuBar::new_in_window(window, cx, |this: &mut View| &mut this.menu);
            menu.set_always(true);
            View { menu, input }
        });
        let state = |cx: &mut gpui::VisualTestContext| {
            view.read_with(cx, |view, _| {
                (view.menu.shown(), view.menu.visible, view.menu.open)
            })
        };
        assert_eq!(state(cx), (true, false, false));

        // Clicking the heading opens its menu; Escape closes it, and the bar
        // stays.
        let at = gpui::point(px(10.), px(10.));
        cx.simulate_mouse_move(at, None, Modifiers::default());
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(state(cx), (true, true, true));
        cx.simulate_keystrokes("escape");
        assert_eq!(state(cx), (true, false, false));

        // A second click on the same heading closes it and leaves nothing
        // selected (#645).
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(state(cx), (true, true, true));
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(state(cx), (true, false, false));

        // Alt may underline access keys, but releasing it never opens menus.
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        cx.simulate_modifiers_change(alt);
        cx.simulate_modifiers_change(Modifiers::default());
        assert_eq!(state(cx), (true, false, false));
        cx.simulate_modifiers_change(alt);
        cx.simulate_modifiers_change(Modifiers::default());
        assert_eq!(state(cx), (true, false, false));

        // Auto-hide restores the old behaviour: no room until asked.
        view.update(cx, |view, _| view.menu.set_always(false));
        assert!(!state(cx).0);
    }

    #[gpui::test]
    fn a_click_on_content_passes_through_a_shown_bar_but_not_an_open_menu(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{
            Context, Focusable, IntoElement, Menu, MenuItem, Render, Window, div, prelude::*,
        };

        struct View {
            menu: MenuBar,
            input: gpui::Entity<crate::input::TextInput>,
            clicks: usize,
            toggles: usize,
        }
        impl Render for View {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let content = div()
                    .size_full()
                    .key_context("ChatWindow")
                    .child(
                        div()
                            .id("button")
                            .h(px(40.))
                            .on_click(cx.listener(|this, _, _, _| this.clicks += 1)),
                    )
                    .child(self.input.clone())
                    .on_action(cx.listener(|this, _: &crate::ToggleMenuBar, _, _| {
                        this.toggles += 1;
                    }))
                    .into_any_element();
                super::wrap_in_window(
                    &self.menu,
                    vec![
                        Menu {
                            name: "View".into(),
                            items: vec![MenuItem::action("Toggle diagnostics", crate::ToggleDebug)],
                            menu_type: None,
                        }
                        .owned(),
                    ],
                    super::AccessKeys {
                        keys: vec![Some('v')],
                        suffix: false,
                    },
                    content,
                    |this| &mut this.menu,
                    |_, _| false,
                    window,
                    cx,
                )
            }
        }

        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ))
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| crate::input::TextInput::new_live("Draft", cx));
            window.focus(&input.focus_handle(cx));
            View {
                menu: MenuBar::new_in_window(window, cx, |this: &mut View| &mut this.menu),
                input,
                clicks: 0,
                toggles: 0,
            }
        });
        let state = |cx: &mut gpui::VisualTestContext| {
            view.read_with(cx, |view, _| {
                (view.menu.visible, view.menu.open, view.clicks)
            })
        };
        // The button sits at the top of the content: the bar pushes it down.
        let button_at = gpui::point(px(300.), px(super::HEIGHT + 6.));

        // Shown by resting the pointer at the top, with no menu open: a whole
        // click (press and release) on the button below the bar works and the
        // bar stays until the pointer leaves.
        cx.simulate_mouse_move(gpui::point(px(300.), px(80.)), None, Modifiers::default());
        cx.simulate_mouse_move(gpui::point(px(300.), px(2.)), None, Modifiers::default());
        cx.executor().advance_clock(super::HOVER_REVEAL_DELAY * 2);
        assert_eq!(state(cx), (true, false, 0));
        cx.simulate_mouse_move(button_at, None, Modifiers::default());
        cx.simulate_click(button_at, Modifiers::default());
        assert_eq!(state(cx), (true, false, 1));

        // With a menu open, the outside click only dismisses it.
        view.update(cx, |view, _| view.menu.open = true);
        assert_eq!(state(cx), (true, true, 1));
        cx.simulate_click(button_at, Modifiers::default());
        assert_eq!(state(cx), (false, false, 1));

        // F10 asks for the same toggle as View → Hide/Show Menu Bar.
        cx.simulate_keystrokes("f10");
        assert_eq!(view.read_with(cx, |view, _| view.toggles), 1);
        assert_eq!(state(cx), (false, false, 1));
    }

    #[gpui::test]
    fn first_render_and_menu_reveal_use_a_ready_dispatch_tree(cx: &mut gpui::TestAppContext) {
        use gpui::{
            Context, Entity, Focusable, IntoElement, Menu, MenuItem, Render, Window, div,
            prelude::*,
        };

        struct TestView {
            menu: MenuBar,
            input: Entity<crate::input::TextInput>,
        }
        impl Render for TestView {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let content = div()
                    .key_context("ChatWindow")
                    .child(self.input.clone())
                    .on_action(|_: &crate::ToggleDebug, _, _| {})
                    .into_any_element();
                super::wrap_in_window(
                    &self.menu,
                    vec![
                        Menu {
                            name: "View".into(),
                            items: vec![MenuItem::action("Toggle diagnostics", crate::ToggleDebug)],
                            menu_type: None,
                        }
                        .owned(),
                    ],
                    super::AccessKeys {
                        keys: vec![Some('v')],
                        suffix: false,
                    },
                    content,
                    |this| &mut this.menu,
                    |_, _| false,
                    window,
                    cx,
                )
            }
        }

        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ))
        });
        // add_window_view performs the initial render before a dispatch tree
        // exists. Querying action availability here used to panic on startup.
        let (view, cx) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| crate::input::TextInput::new_live("Draft", cx));
            window.focus(&input.focus_handle(cx));
            TestView {
                menu: MenuBar::new_in_window(window, cx, |this: &mut TestView| &mut this.menu),
                input,
            }
        });
        assert!(!view.read_with(cx, |view, _| view.menu.visible));
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        cx.simulate_modifiers_change(alt);
        cx.simulate_modifiers_change(Modifiers::default());
        assert!(!view.read_with(cx, |view, _| view.menu.visible));
        cx.update(|window, cx| {
            assert!(window.is_action_available(&crate::ToggleDebug, cx));
        });
        // F10 toggles the bar setting; it never reveals the bar itself.
        cx.simulate_keystrokes("f10");
        assert!(!view.read_with(cx, |view, _| view.menu.visible));
        assert_eq!(
            view.read_with(cx, |view, cx| view.input.read(cx).text().to_owned()),
            ""
        );

        // Alt+letter opens that menu and shows the bar; releasing Alt first and
        // then typing the letter does not.
        cx.simulate_keystrokes("alt-v");
        assert!(view.read_with(cx, |view, _| view.menu.visible && view.menu.open));
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |view, _| !view.menu.visible && !view.menu.open));
        cx.simulate_modifiers_change(alt);
        cx.simulate_modifiers_change(Modifiers::default());
        cx.simulate_keystrokes("v");
        assert!(view.read_with(cx, |view, _| !view.menu.visible && !view.menu.open));
        assert_eq!(
            view.read_with(cx, |view, cx| view.input.read(cx).text().to_owned()),
            "v"
        );
        // A shortcut on the same chord is used instead of the menu.
        cx.update(|_, cx| cx.bind_keys([gpui::KeyBinding::new("alt-v", crate::ToggleDebug, None)]));
        cx.simulate_keystrokes("alt-v");
        assert!(view.read_with(cx, |view, _| !view.menu.open));

        // Resting in the hot zone reveals it; moving away hides it again.
        cx.simulate_mouse_move(gpui::point(px(40.), px(80.)), None, Modifiers::default());
        cx.simulate_mouse_move(gpui::point(px(40.), px(2.)), None, Modifiers::default());
        cx.executor().advance_clock(super::HOVER_REVEAL_DELAY / 2);
        assert!(!view.read_with(cx, |view, _| view.menu.visible));
        cx.executor().advance_clock(super::HOVER_REVEAL_DELAY);
        assert!(view.read_with(cx, |view, _| view.menu.visible));
        cx.simulate_mouse_move(gpui::point(px(40.), px(20.)), None, Modifiers::default());
        cx.executor().advance_clock(super::HOVER_POLL * 2);
        assert!(view.read_with(cx, |view, _| view.menu.visible));
        cx.simulate_mouse_move(gpui::point(px(40.), px(200.)), None, Modifiers::default());
        cx.executor().advance_clock(super::HOVER_POLL * 2);
        assert!(!view.read_with(cx, |view, _| view.menu.visible));

        // Passing through the hot zone without resting does not reveal it.
        cx.simulate_mouse_move(gpui::point(px(40.), px(2.)), None, Modifiers::default());
        cx.simulate_mouse_move(gpui::point(px(40.), px(80.)), None, Modifiers::default());
        cx.executor().advance_clock(super::HOVER_REVEAL_DELAY * 2);
        assert!(!view.read_with(cx, |view, _| view.menu.visible));
    }

    #[test]
    fn access_keys_need_exactly_alt_and_no_composition() {
        let keys = super::AccessKeys {
            keys: vec![Some('f'), None, Some('e')],
            suffix: false,
        };
        let press = |text: &str| gpui::Keystroke::parse(text).unwrap();
        let mut state = MenuBar::default();
        assert!(!state.access_key(&press("e"), &keys, 3, false));
        assert!(!state.access_key(&press("ctrl-alt-e"), &keys, 3, false));
        assert!(!state.access_key(&press("alt-shift-e"), &keys, 3, false));
        assert!(!state.access_key(&press("alt-x"), &keys, 3, false));
        assert!(!state.access_key(&press("alt-e"), &keys, 3, true));
        assert!(!state.access_key(&press("alt-e"), &keys, 2, false));
        assert!(!state.visible);
        assert!(state.access_key(&press("alt-e"), &keys, 3, false));
        assert!((state.visible, state.open, state.active) == (true, true, 2));
    }

    #[test]
    fn slide_choice_is_fixed_for_a_whole_reveal() {
        let mut state = MenuBar::default();
        state.reveal(false);
        assert!(state.slide);
        // Opening and closing menus must not switch the bar's element shape.
        state.open = true;
        assert!(state.slide);
        state.open = false;
        assert!(state.slide);
        state.close();
        let keys = super::AccessKeys {
            keys: vec![Some('f')],
            suffix: false,
        };
        let alt_f = gpui::Keystroke::parse("alt-f").unwrap();
        assert!(state.access_key(&alt_f, &keys, 1, false));
        assert!(!state.slide);
    }

    #[test]
    fn alt_alone_does_not_reveal_menu() {
        let mut state = MenuBar::default();
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        state.modifiers_changed(alt);
        state.modifiers_changed(Modifiers::default());
        assert!(!state.visible);
    }

    #[test]
    fn menu_navigation_wraps_and_skips_separators_and_disabled_items() {
        let mut state = MenuBar::default();
        state.move_menu(true, 5);
        assert_eq!(state.active, 4);
        state.move_menu(false, 5);
        assert_eq!(state.active, 0);
        let selectable = [0, 2, 5];
        state.move_item(true, &selectable);
        assert_eq!(state.selected, Some(5));
        state.move_item(false, &selectable);
        assert_eq!(state.selected, Some(0));
        state.move_item(false, &selectable);
        assert_eq!(state.selected, Some(2));
        state.move_item(false, &[]);
        assert_eq!(state.selected, None);
    }
}
