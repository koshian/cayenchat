//! Alt-revealed menus for platforms without GPUI native menu rendering.
use gpui::{prelude::*, *};

const HEIGHT: f32 = 28.;

#[derive(Default)]
pub struct MenuBar {
    visible: bool,
    active: usize,
    open: bool,
    selected: Option<usize>,
    alt_down: bool,
    alt_candidate: bool,
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
                self.visible = true;
                self.active = 0;
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
    let theme = crate::theme::current(cx);
    let enabled = enabled_items(&menus, window, cx);
    let mut bar = div()
        .flex()
        .h(px(HEIGHT))
        .flex_shrink_0()
        .bg(theme.surface)
        .border_b_1()
        .border_color(theme.border);
    if state.visible {
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
    div()
        .id("window-menu-root")
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
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{MenuBar, Modifiers};

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
