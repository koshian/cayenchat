// SPDX-License-Identifier: GPL-3.0-only
use super::{CompleteNickname, field_traversal, shortcut_bindings};
use crate::input::TextInput;
use cayenchat_storage::ChannelNumberModifier;
use gpui::{
    Context, Entity, Focusable, Render, TestAppContext, VisualTestContext, Window, div, prelude::*,
};

struct Settings {
    fields: Vec<Entity<TextInput>>,
}
impl Render for Settings {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        field_traversal(div().id("settings"))
            .key_context("SettingsWindow")
            .children(self.fields.clone())
    }
}

struct Chat {
    draft: Entity<TextInput>,
    completions: usize,
}
impl Render for Chat {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("ChatWindow")
            .on_action(cx.listener(|this, _: &CompleteNickname, _, _| this.completions += 1))
            .child(self.draft.clone())
    }
}

fn bind(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::theme::apply(
            cayenchat_storage::ThemeMode::Light,
            &cayenchat_storage::Appearance::default(),
            cx,
        );
        crate::input::bind_keys(false, cx);
        cx.bind_keys(shortcut_bindings(
            ChannelNumberModifier::Ctrl,
            &Default::default(),
            cx,
        ));
    });
}

fn focused(view: &Entity<Settings>, cx: &mut VisualTestContext) -> Option<usize> {
    cx.update(|window, cx| {
        view.read(cx)
            .fields
            .iter()
            .position(|field| field.focus_handle(cx).is_focused(window))
    })
}

#[gpui::test]
fn tab_moves_between_settings_fields_without_chat_commands(cx: &mut TestAppContext) {
    bind(cx);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let fields: Vec<_> = ["Host", "Port", "Password"]
            .into_iter()
            .map(|name| {
                cx.new(|cx| TextInput::new_settings_field(name, "", name == "Password", cx))
            })
            .collect();
        window.focus(&fields[0].focus_handle(cx), cx);
        Settings { fields }
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    assert_eq!(focused(&view, cx), Some(1));
    cx.simulate_keystrokes("tab");
    assert_eq!(focused(&view, cx), Some(2));
    cx.simulate_keystrokes("tab");
    assert_eq!(focused(&view, cx), Some(0), "wraps like platform forms");
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(focused(&view, cx), Some(2));
    // Typing, including Enter, still edits the field it lands in.
    cx.simulate_input("secret");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        view.read_with(cx, |view, cx| view.fields[2].read(cx).text().to_owned()),
        "secret"
    );
}

#[gpui::test]
fn tab_still_completes_nicknames_in_chat(cx: &mut TestAppContext) {
    bind(cx);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let draft = cx.new(|cx| TextInput::new_live("Draft", cx));
        window.focus(&draft.focus_handle(cx), cx);
        Chat {
            draft,
            completions: 0,
        }
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |view, _| view.completions), 1);
    assert!(cx.update(|window, cx| view.read(cx).draft.focus_handle(cx).is_focused(window)));
}
