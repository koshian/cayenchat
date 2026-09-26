//! Alt/F10/hover-revealed menus for platforms without GPUI native menu rendering.
use gpui::{prelude::*, *};
use std::time::Duration;

const HEIGHT: f32 = 28.;
/// Invisible strip at the top of the content that reveals the bar on hover.
/// Alt alone is often an IME toggle, so it cannot be the only way in.
const HOT_ZONE: f32 = 6.;
const HOVER_REVEAL_DELAY: Duration = Duration::from_millis(400);
/// How often a hover-revealed bar checks whether the pointer has left it.
const HOVER_POLL: Duration = Duration::from_millis(150);
/// Extra distance below the bar the pointer may drift before it hides.
const HOVER_SLACK: f32 = 12.;
const REVEAL_ANIMATION: Duration = Duration::from_millis(140);

#[derive(Default)]
pub struct MenuBar {
    visible: bool,
    active: usize,
    open: bool,
    selected: Option<usize>,
    alt_down: bool,
    alt_candidate: bool,
    /// Shown by hovering, so leaving the bar hides it again.
    hover_reveal: bool,
    /// Invalidates pending hover timers when the pointer moves on.
    hover_generation: u64,
    /// Restarts the slide-in animation for each reveal.
    reveals: usize,
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
        // first so Enter/arrows cannot send or edit a draft while using menus,
        // and bound Alt shortcuts also cancel the lone-Alt candidate.
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != handle {
                return;
            }
            let _ = owner.update(cx, |this, cx| {
                let state = access(this);
                state.alt_candidate = false;
                let key = &event.keystroke;
                if key.key == "f10" && !key.modifiers.modified() {
                    if state.visible {
                        state.close();
                    } else {
                        state.reveal(false);
                    }
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
                if self.open
                    && let Some(row) = self.selected
                {
                    if enabled[self.active].contains(&row)
                        && let OwnedMenuItem::Action { action, .. } = &menus[self.active].items[row]
                    {
                        window.dispatch_action(action.boxed_clone(), cx);
                    }
                    self.close();
                } else {
                    self.move_item(false, &enabled[self.active]);
                }
            }
            _ => {
                self.close();
                return false;
            }
        }
        true
    }

    pub fn height(&self) -> Pixels {
        px(if self.visible && !cfg!(target_os = "macos") {
            HEIGHT
        } else {
            0.
        })
    }

    fn close(&mut self) {
        self.visible = false;
        self.open = false;
        self.selected = None;
        self.alt_candidate = false;
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
    }

    fn modifiers_changed(&mut self, modifiers: Modifiers) {
        let other =
            modifiers.control || modifiers.shift || modifiers.platform || modifiers.function;
        if modifiers.alt && !self.alt_down {
            self.alt_candidate = !other;
        }
        if other {
            self.alt_candidate = false;
        }
        if self.alt_down && !modifiers.alt && self.alt_candidate {
            if self.visible {
                self.close();
            } else {
                self.reveal(false);
            }
        }
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
pub fn wrap<V: 'static>(
    state: &MenuBar,
    menus: Vec<OwnedMenu>,
    content: AnyElement,
    access: fn(&mut V) -> &mut MenuBar,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    if cfg!(target_os = "macos") {
        return content;
    }
    wrap_in_window(state, menus, content, access, window, cx)
}

// Kept platform-independent so first-frame rendering is tested on macOS too.
fn wrap_in_window<V: 'static>(
    state: &MenuBar,
    menus: Vec<OwnedMenu>,
    content: AnyElement,
    access: fn(&mut V) -> &mut MenuBar,
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
    if state.visible {
        // The initial frame has no rendered dispatch tree. Alt can reveal the
        // menu only after it has been painted, when availability queries are safe.
        let enabled = enabled_items(&menus, window, cx);
        for (index, menu) in menus.iter().enumerate() {
            let mut label = div()
                .id(("menu-heading", index))
                .relative()
                .px_2()
                .h_full()
                .cursor_pointer()
                .hover(|d| d.bg(theme.hover_strong))
                .when(state.active == index, |d| d.bg(theme.selected))
                .child(menu.name.clone())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        let state = access(this);
                        state.alt_candidate = false;
                        state.hover_reveal = false;
                        state.open = state.active != index || !state.open;
                        state.active = index;
                        state.selected = None;
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
    let bar = bar.with_animation(
        ("window-menu-reveal", state.reveals),
        Animation::new(REVEAL_ANIMATION).with_easing(ease_out_quint()),
        |bar, delta| bar.h(px(HEIGHT * delta)),
    );
    div()
        .id("window-menu-root")
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .text_size(px(13.))
        .line_height(px(26.))
        .text_color(theme.text)
        .capture_any_mouse_down(cx.listener(move |this, _, _, _| {
            access(this).alt_candidate = false;
        }))
        .on_modifiers_changed(
            cx.listener(move |this, event: &ModifiersChangedEvent, _, cx| {
                access(this).modifiers_changed(event.modifiers);
                cx.notify();
            }),
        )
        .when(state.visible, |d| d.child(bar))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .child(content)
                .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                    let state = access(this);
                    state.alt_candidate = false;
                    if state.visible {
                        state.close();
                        cx.stop_propagation();
                        cx.notify();
                    }
                })),
        )
        .when(!state.visible, |d| d.child(hot_zone))
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
    fn first_render_and_alt_reveal_use_a_ready_dispatch_tree(cx: &mut gpui::TestAppContext) {
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
                    .on_action(|_: &crate::CopyDiagnostics, _, _| {})
                    .into_any_element();
                super::wrap_in_window(
                    &self.menu,
                    vec![
                        Menu {
                            name: "View".into(),
                            items: vec![MenuItem::action(
                                "Copy diagnostics",
                                crate::CopyDiagnostics,
                            )],
                        }
                        .owned(),
                    ],
                    content,
                    |this| &mut this.menu,
                    window,
                    cx,
                )
            }
        }

        cx.update(|cx| {
            crate::apply_shortcuts(cayenchat_storage::ChannelNumberModifier::default(), cx);
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
        assert!(view.read_with(cx, |view, _| view.menu.visible));
        cx.update(|window, cx| {
            assert!(window.is_action_available(&crate::CopyDiagnostics, cx));
        });
        cx.simulate_modifiers_change(alt);
        cx.simulate_modifiers_change(Modifiers::default());
        assert!(!view.read_with(cx, |view, _| view.menu.visible));

        // F10 works when Alt alone is taken by an IME toggle.
        cx.simulate_keystrokes("f10");
        assert!(view.read_with(cx, |view, _| view.menu.visible));
        assert_eq!(
            view.read_with(cx, |view, cx| view.input.read(cx).text().to_owned()),
            ""
        );
        cx.simulate_keystrokes("f10");
        assert!(!view.read_with(cx, |view, _| view.menu.visible));

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
    fn alt_alone_toggles_but_chords_do_not() {
        let mut state = MenuBar::default();
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        state.modifiers_changed(alt);
        assert!(!state.visible);
        state.modifiers_changed(Modifiers::default());
        assert!(state.visible);
        state.modifiers_changed(alt);
        state.modifiers_changed(Modifiers::default());
        assert!(!state.visible);
        state.modifiers_changed(alt);
        state.alt_candidate = false; // A normal key or mouse click while Alt is held.
        state.modifiers_changed(Modifiers::default());
        assert!(!state.visible);
        state.modifiers_changed(Modifiers {
            control: true,
            ..alt
        });
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
