// SPDX-License-Identifier: GPL-3.0-only
use std::sync::Arc;

use cayenchat_app::{Command, Selection};
use cayenchat_storage::{CredentialBackendKind, CredentialStore, credentials::MemoryBackend};
use gpui::{Entity, TestAppContext, VisualTestContext, px};

use super::{ChatWindow, secrets, settings_with_channels};

const CHANNELS: usize = 150;

fn open_chat(cx: &mut TestAppContext) -> (Entity<ChatWindow>, &mut VisualTestContext) {
    let store =
        CredentialStore::with_backend(Arc::new(MemoryBackend::new(CredentialBackendKind::System)));
    cx.update(|cx| {
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(secrets::Credentials(store));
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ))
    });
    let channels = (0..CHANNELS)
        .map(|n| format!("#c{n:03}"))
        .collect::<Vec<_>>()
        .join(",");
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings_with_channels(&channels), None, window, cx)
    });
    settle(cx);
    (chat, cx)
}

fn settle(cx: &mut VisualTestContext) {
    // One draw only: further frames must come from the implementation.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// Whether the selected row lies fully inside the tree's viewport.
fn selection_visible(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) -> bool {
    chat.read_with(cx, |chat, _| {
        let selection = chat.state.selection();
        let at = chat
            .tree_rows
            .iter()
            .position(|row| match *row {
                crate::TreeRow::Server(id) => selection == Selection::Server(id),
                crate::TreeRow::Channel(id) => selection == Selection::Channel(id),
            })
            .expect("selected row");
        let view = chat.tree_list.state.viewport_bounds();
        chat.tree_list.state.bounds_for_item(at).is_some_and(|b| {
            b.top() >= view.top() - px(0.5) && b.bottom() <= view.bottom() + px(0.5)
        })
    })
}

fn select(chat: &Entity<ChatWindow>, command: Command, cx: &mut VisualTestContext) {
    chat.update(cx, |chat, cx| {
        chat.state.dispatch(command);
        cx.notify();
    });
    settle(cx);
}

fn top(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext) -> usize {
    chat.read_with(cx, |chat, _| {
        chat.tree_list.state.logical_scroll_top().item_ix
    })
}

/// A shortcut to a row beyond the laid-out rows still ends with that row in
/// view, and so does the way back.
#[gpui::test]
fn shortcut_reveals_a_row_that_was_never_laid_out(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    select(&chat, Command::SelectChannelAt(CHANNELS - 1), cx);
    assert!(selection_visible(&chat, cx), "last channel");
    assert!(top(&chat, cx) > 0);
    select(&chat, Command::SelectChannelAt(0), cx);
    assert!(selection_visible(&chat, cx), "first channel");
}

/// A selection that is already in view does not move the list, and scrolling
/// by hand is not undone while the selection stays.
#[gpui::test]
fn visible_selection_and_manual_scroll_are_left_alone(cx: &mut TestAppContext) {
    let (chat, cx) = open_chat(cx);
    let start = top(&chat, cx);
    select(&chat, Command::SelectChannelAt(1), cx);
    assert_eq!(top(&chat, cx), start);
    select(&chat, Command::SelectChannelAt(CHANNELS - 1), cx);
    let before = top(&chat, cx);
    chat.read_with(cx, |chat, _| chat.tree_list.state.scroll_by(px(-100.)));
    settle(cx);
    let scrolled = top(&chat, cx);
    assert!(scrolled < before);
    settle(cx);
    assert_eq!(top(&chat, cx), scrolled);
}

/// A row right after a row taller than the viewport still gets revealed, and
/// the follow-up passes stop.
#[gpui::test]
fn reveal_stops_behind_a_row_taller_than_the_viewport(cx: &mut TestAppContext) {
    let store =
        CredentialStore::with_backend(Arc::new(MemoryBackend::new(CredentialBackendKind::System)));
    cx.update(|cx| {
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(secrets::Credentials(store));
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ))
    });
    let channels = format!("#{},#last", "abcdefghij".repeat(200));
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings_with_channels(&channels), None, window, cx)
    });
    settle(cx);
    select(&chat, Command::SelectChannelAt(1), cx);
    assert!(selection_visible(&chat, cx), "row after the tall one");
    assert!(chat.read_with(cx, |chat, _| !chat.tree_reveal_pending));
}
