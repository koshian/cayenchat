// SPDX-License-Identifier: GPL-3.0-only
use super::{Navigate, shortcut_bindings};
#[cfg(target_os = "macos")]
use cayenchat_app::Command;
use cayenchat_storage::ChannelNumberModifier;

/// The navigation command a typed key combination runs, and that no other
/// navigation binding takes the same keys.
#[cfg(target_os = "macos")]
fn command_for(keys: &str) -> Option<Command> {
    let typed = gpui::Keystroke::parse_with_os(keys, crate::shortcuts::HOST_OS).unwrap();
    let bindings = gpui::TestAppContext::single()
        .update(|cx| shortcut_bindings(ChannelNumberModifier::Ctrl, &Default::default(), cx));
    let matching: Vec<_> = bindings
        .iter()
        .filter(|binding| binding.match_keystrokes(std::slice::from_ref(&typed)) == Some(false))
        .collect();
    assert!(
        matching.len() <= 1,
        "{keys} is bound {} times",
        matching.len()
    );
    matching.first().and_then(|binding| {
        binding
            .action()
            .as_any()
            .downcast_ref::<Navigate>()
            .map(|navigate| navigate.command)
    })
}

#[cfg(target_os = "macos")]
#[test]
fn the_bracket_keys_move_between_channels_and_servers() {
    // Decided in #72.
    assert_eq!(command_for("cmd-["), Some(Command::PreviousChannel));
    assert_eq!(command_for("cmd-]"), Some(Command::NextChannel));
    // Pressing Cmd+Shift+[ on macOS arrives as `cmd-{` (see the bindings),
    // so that is what must be bound; `cmd-shift-[` never arrives.
    assert_eq!(command_for("cmd-{"), Some(Command::PreviousServer));
    assert_eq!(command_for("cmd-}"), Some(Command::NextServer));
    assert_eq!(command_for("cmd-shift-["), None);
    assert_eq!(command_for("cmd-shift-]"), None);
    // The arrow-based keys stay.
    assert_eq!(command_for("ctrl-up"), Some(Command::PreviousChannel));
    assert_eq!(command_for("ctrl-right"), Some(Command::NextServer));
    assert_eq!(command_for("cmd-up"), Some(Command::PreviousActiveChannel));
    assert_eq!(
        command_for("cmd-alt-down"),
        Some(Command::NextActiveChannel)
    );
}

#[test]
fn no_two_navigation_bindings_share_keys() {
    for modifier in [
        ChannelNumberModifier::Ctrl,
        ChannelNumberModifier::Alt,
        ChannelNumberModifier::Super,
    ] {
        let mut seen = std::collections::HashSet::new();
        let made = gpui::TestAppContext::single()
            .update(|cx| shortcut_bindings(modifier, &Default::default(), cx));
        for binding in made {
            if binding
                .action()
                .as_any()
                .downcast_ref::<Navigate>()
                .is_none()
            {
                continue;
            }
            let keys = format!("{:?}", binding.keystrokes());
            assert!(
                seen.insert(keys.clone()),
                "{keys} is bound twice ({modifier:?})"
            );
        }
    }
}
