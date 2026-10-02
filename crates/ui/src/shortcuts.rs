//! Navigation shortcuts as named actions.
//!
//! Each action has an id, the command it runs and the keys it has by default
//! on this platform. The user can replace an action's keys (#114): the setting
//! is the id → key pairs they changed, so a later change of a default keeps
//! the meaning of what they chose, and an action they did not touch follows
//! the defaults.
//!
//! A key is kept the way GPUI reports it when pressed. That is not always the
//! way it is typed: on macOS, Shift with a symbol key arrives as the shifted
//! character with Shift cleared (Cmd+Shift+[ is `cmd-{`), so matching and
//! saving by what arrives is what keeps a saved key working.

use std::collections::BTreeMap;

use cayenchat_app::Command;
use gpui::{KeyBinding, Keystroke};

/// What the user changed: action id → key.
pub type Overrides = BTreeMap<String, String>;

pub struct ShortcutAction {
    pub id: &'static str,
    pub command: Command,
    /// The keys it has until the user chooses another; all of them go.
    pub defaults: &'static [&'static str],
}

const fn action(
    id: &'static str,
    command: Command,
    defaults: &'static [&'static str],
) -> ShortcutAction {
    ShortcutAction {
        id,
        command,
        defaults,
    }
}

/// `mac` on macOS, `other` on Windows and Linux.
#[cfg(target_os = "macos")]
macro_rules! per_platform {
    ($mac:expr, $other:expr) => {
        $mac
    };
}
#[cfg(not(target_os = "macos"))]
macro_rules! per_platform {
    ($mac:expr, $other:expr) => {
        $other
    };
}

/// The actions the keys can be changed for. Decided in #72: on macOS the
/// bracket keys move between channels and (with Shift) servers; the arrow
/// keys stay for layouts where `[` and `]` are awkward.
pub const ACTIONS: &[ShortcutAction] = &[
    action(
        "previous_channel",
        Command::PreviousChannel,
        per_platform!(&["cmd-[", "ctrl-up"], &["alt-up"]),
    ),
    action(
        "next_channel",
        Command::NextChannel,
        per_platform!(&["cmd-]", "ctrl-down"], &["alt-down"]),
    ),
    action(
        "previous_active_channel",
        Command::PreviousActiveChannel,
        per_platform!(&["cmd-up", "cmd-alt-up"], &["ctrl-pageup"]),
    ),
    action(
        "next_active_channel",
        Command::NextActiveChannel,
        per_platform!(&["cmd-down", "cmd-alt-down"], &["ctrl-pagedown"]),
    ),
    // `cmd-{` and `cmd-}`: how macOS reports Cmd+Shift+[ and Cmd+Shift+].
    action(
        "previous_server",
        Command::PreviousServer,
        per_platform!(&["cmd-{", "ctrl-left"], &["alt-pageup"]),
    ),
    action(
        "next_server",
        Command::NextServer,
        per_platform!(&["cmd-}", "ctrl-right"], &["alt-pagedown"]),
    ),
    action(
        "previous_active_server",
        Command::PreviousActiveServer,
        per_platform!(&["cmd-alt-left"], &["ctrl-alt-pageup"]),
    ),
    action(
        "next_active_server",
        Command::NextActiveServer,
        per_platform!(&["cmd-alt-right"], &["ctrl-alt-pagedown"]),
    ),
    action(
        "previous_unread_channel",
        Command::PreviousUnreadChannel,
        per_platform!(&["ctrl-shift-tab", "alt-shift-space"], &["ctrl-shift-tab"]),
    ),
    action(
        "next_unread_channel",
        Command::NextUnreadChannel,
        per_platform!(&["ctrl-tab", "alt-space"], &["ctrl-tab"]),
    ),
    action(
        "previous_selected_channel",
        Command::PreviousSelectedChannel,
        per_platform!(&["alt-tab"], &["alt-left"]),
    ),
];

/// Whether pressing `keystroke` is how a character is typed on that system:
/// Option+letter on macOS (`å`, `ø`) and Ctrl+Alt+letter on Windows, where it
/// is AltGr. A shortcut on it would be matched before the character reaches
/// the draft and take that character away. `macos` and `windows` say which
/// system's rules apply.
pub fn types_text_on(keystroke: &Keystroke, macos: bool, windows: bool) -> bool {
    let modifiers = &keystroke.modifiers;
    let one_character = keystroke.key.chars().count() == 1;
    one_character
        && !modifiers.platform
        && ((macos && modifiers.alt && !modifiers.control)
            || (windows && modifiers.alt && modifiers.control))
}

/// [`types_text_on`] for the system this is running on.
pub fn types_text(keystroke: &Keystroke) -> bool {
    types_text_on(
        keystroke,
        cfg!(target_os = "macos"),
        cfg!(target_os = "windows"),
    )
}

/// A key a user may give an action, or `None`. One keystroke (no chords),
/// parsed as GPUI parses a binding, carrying a modifier or being a function
/// key: a bare letter would take the characters out of the draft, and a
/// modifier on its own is not a key. A combination that types a character
/// ([`types_text`]) is not usable either.
pub fn usable_key(key: &str) -> Option<Keystroke> {
    if key.is_empty() || key.chars().any(char::is_whitespace) {
        return None;
    }
    let keystroke = Keystroke::parse(key).ok()?;
    if types_text(&keystroke) {
        return None;
    }
    let modifier_alone = matches!(
        keystroke.key.as_str(),
        "" | "shift" | "control" | "alt" | "platform" | "function"
    );
    let modifiers = &keystroke.modifiers;
    let carries_modifier =
        modifiers.control || modifiers.alt || modifiers.platform || modifiers.function;
    let function_key = keystroke
        .key
        .strip_prefix('f')
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()));
    (!modifier_alone && (carries_modifier || function_key)).then_some(keystroke)
}

/// The keys `action` has now: the one the user chose, if it is usable, else
/// the defaults.
pub fn keys(action: &ShortcutAction, overrides: &Overrides) -> Vec<String> {
    match overrides.get(action.id) {
        Some(key) if usable_key(key).is_some() => vec![key.clone()],
        _ => action
            .defaults
            .iter()
            .map(|key| (*key).to_owned())
            .collect(),
    }
}

/// The navigation bindings for the defaults and what the user changed.
/// Entries for unknown actions and unusable keys are ignored.
pub fn bindings(overrides: &Overrides) -> Vec<KeyBinding> {
    ACTIONS
        .iter()
        .flat_map(|action| {
            keys(action, overrides)
                .into_iter()
                .map(|key| crate::navigation_binding(&key, action.command))
        })
        .collect()
}

/// Who holds a key. Conflicts are logged at startup and shown as warnings by
/// the shortcuts tab (#118).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Action(&'static str),
    /// A shortcut that cannot be changed (Send, Tab, settings, the digits).
    Fixed,
}

/// A key that more than one owner would take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub key: String,
    pub owners: Vec<Owner>,
}

/// The keys more than one owner takes, for the defaults and `overrides`.
/// `fixed` are the bindings that cannot be changed; a key matches one the way
/// GPUI would match it when pressed.
pub fn conflicts(overrides: &Overrides, fixed: &[KeyBinding]) -> Vec<Conflict> {
    let mut by_key: Vec<(String, Keystroke, Vec<Owner>)> = Vec::new();
    for action in ACTIONS {
        for key in keys(action, overrides) {
            let Some(keystroke) = usable_key(&key) else {
                continue;
            };
            let canonical = keystroke.unparse();
            match by_key
                .iter_mut()
                .find(|(existing, _, _)| *existing == canonical)
            {
                Some((_, _, owners)) => {
                    if !owners.contains(&Owner::Action(action.id)) {
                        owners.push(Owner::Action(action.id));
                    }
                }
                None => by_key.push((canonical, keystroke, vec![Owner::Action(action.id)])),
            }
        }
    }
    by_key
        .into_iter()
        .filter_map(|(key, keystroke, mut owners)| {
            if fixed
                .iter()
                .any(|binding| binding.match_keystrokes(&[keystroke.clone()]) == Some(false))
            {
                owners.push(Owner::Fixed);
            }
            (owners.len() > 1).then_some(Conflict { key, owners })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Navigate;

    fn overrides(pairs: &[(&str, &str)]) -> Overrides {
        pairs
            .iter()
            .map(|(id, key)| ((*id).to_owned(), (*key).to_owned()))
            .collect()
    }

    /// (key, command) of every navigation binding made.
    fn navigation(overrides: &Overrides) -> Vec<(String, Command)> {
        bindings(overrides)
            .iter()
            .map(|binding| {
                let key = binding.keystrokes()[0].unparse();
                let command = binding
                    .action()
                    .as_any()
                    .downcast_ref::<Navigate>()
                    .expect("a navigation binding")
                    .command;
                (key, command)
            })
            .collect()
    }

    #[test]
    fn the_defaults_are_the_keys_the_application_had_before_they_could_change() {
        let made = navigation(&Overrides::new());
        #[cfg(target_os = "macos")]
        let expected: &[(&str, Command)] = &[
            ("cmd-[", Command::PreviousChannel),
            ("ctrl-up", Command::PreviousChannel),
            ("cmd-]", Command::NextChannel),
            ("ctrl-down", Command::NextChannel),
            ("cmd-up", Command::PreviousActiveChannel),
            ("alt-cmd-up", Command::PreviousActiveChannel),
            ("cmd-down", Command::NextActiveChannel),
            ("alt-cmd-down", Command::NextActiveChannel),
            ("cmd-{", Command::PreviousServer),
            ("ctrl-left", Command::PreviousServer),
            ("cmd-}", Command::NextServer),
            ("ctrl-right", Command::NextServer),
            ("alt-cmd-left", Command::PreviousActiveServer),
            ("alt-cmd-right", Command::NextActiveServer),
            ("ctrl-shift-tab", Command::PreviousUnreadChannel),
            ("alt-shift-space", Command::PreviousUnreadChannel),
            ("ctrl-tab", Command::NextUnreadChannel),
            ("alt-space", Command::NextUnreadChannel),
            ("alt-tab", Command::PreviousSelectedChannel),
        ];
        #[cfg(not(target_os = "macos"))]
        let expected: &[(&str, Command)] = &[
            ("alt-up", Command::PreviousChannel),
            ("alt-down", Command::NextChannel),
            ("ctrl-pageup", Command::PreviousActiveChannel),
            ("ctrl-pagedown", Command::NextActiveChannel),
            ("alt-pageup", Command::PreviousServer),
            ("alt-pagedown", Command::NextServer),
            ("ctrl-alt-pageup", Command::PreviousActiveServer),
            ("ctrl-alt-pagedown", Command::NextActiveServer),
            ("ctrl-shift-tab", Command::PreviousUnreadChannel),
            ("ctrl-tab", Command::NextUnreadChannel),
            ("alt-left", Command::PreviousSelectedChannel),
        ];
        let expected: Vec<(String, Command)> = expected
            .iter()
            .map(|(key, command)| ((*key).to_owned(), *command))
            .collect();
        assert_eq!(made.len(), expected.len(), "{made:?}");
        for pair in &expected {
            assert!(made.contains(pair), "missing {pair:?} in {made:?}");
        }
    }

    #[test]
    fn a_chosen_key_replaces_all_of_the_actions_defaults_and_only_its_own() {
        let changed = navigation(&overrides(&[("next_channel", "ctrl-shift-n")]));
        let next: Vec<_> = changed
            .iter()
            .filter(|(_, command)| *command == Command::NextChannel)
            .collect();
        assert_eq!(next.len(), 1, "{next:?}");
        assert_eq!(next[0].0, "ctrl-shift-n");
        // The other actions keep their defaults.
        let defaults = navigation(&Overrides::new());
        for pair in defaults
            .iter()
            .filter(|(_, command)| *command != Command::NextChannel)
        {
            assert!(changed.contains(pair), "{pair:?} was lost");
        }
    }

    #[test]
    fn unusable_entries_are_ignored_and_the_action_keeps_its_defaults() {
        let defaults = navigation(&Overrides::new());
        for (id, key) in [
            ("no_such_action", "ctrl-x"),
            ("next_channel", ""),
            ("next_channel", "banana-"),
            ("next_channel", "a"),
            ("next_channel", "shift-a"),
            ("next_channel", "shift"),
            ("next_channel", "ctrl"),
            ("next_channel", "cmd-k cmd-j"),
            ("next_channel", " cmd-k"),
        ] {
            assert_eq!(
                navigation(&overrides(&[(id, key)])),
                defaults,
                "{id} = {key:?}"
            );
        }
        // A function key needs no modifier; a modified key does.
        assert!(usable_key("f5").is_some());
        assert!(usable_key("ctrl-a").is_some());
        assert!(usable_key("cmd-}").is_some());
        assert!(usable_key("alt-shift-space").is_some());
        assert!(usable_key("home").is_none());
    }

    #[test]
    fn a_combination_that_types_a_character_is_not_usable() {
        let alt_a = Keystroke::parse("alt-a").unwrap();
        let ctrl_alt_n = Keystroke::parse("ctrl-alt-n").unwrap();
        let cmd_alt_a = Keystroke::parse("cmd-alt-a").unwrap();
        let alt_space = Keystroke::parse("alt-space").unwrap();
        let ctrl_alt_pageup = Keystroke::parse("ctrl-alt-pageup").unwrap();
        // macOS: Option+letter types å, ø and the like.
        assert!(types_text_on(&alt_a, true, false));
        assert!(!types_text_on(&alt_a, false, false));
        // Windows: Ctrl+Alt is AltGr.
        assert!(types_text_on(&ctrl_alt_n, false, true));
        assert!(
            !types_text_on(&ctrl_alt_n, true, false),
            "Ctrl stops Option typing"
        );
        assert!(!types_text_on(&ctrl_alt_n, false, false));
        // With Cmd it is a shortcut, not typing; named keys never type text.
        assert!(!types_text_on(&cmd_alt_a, true, true));
        assert!(!types_text_on(&alt_space, true, true));
        assert!(!types_text_on(&ctrl_alt_pageup, true, true));
        // `usable_key` follows the system this runs on.
        assert_eq!(usable_key("alt-a").is_none(), cfg!(target_os = "macos"));
        assert_eq!(
            usable_key("ctrl-alt-n").is_none(),
            cfg!(target_os = "windows")
        );
        assert!(usable_key("cmd-alt-a").is_some());
    }

    #[test]
    fn a_changed_shortcut_in_the_saved_settings_counts_as_a_change_to_rebind() {
        use crate::ShortcutPrefs;
        let mut settings = cayenchat_storage::Settings::default();
        let before = ShortcutPrefs::from(&settings);
        assert_eq!(before, ShortcutPrefs::from(&settings));
        settings
            .keybindings
            .insert("next_channel".into(), "ctrl-shift-n".into());
        let after = ShortcutPrefs::from(&settings);
        assert_ne!(before, after, "the settings save rebinds only on a change");
        assert_eq!(after.overrides["next_channel"], "ctrl-shift-n");
    }

    #[test]
    fn the_defaults_conflict_with_nothing() {
        let fixed = crate::fixed_bindings(cayenchat_storage::ChannelNumberModifier::Ctrl);
        assert_eq!(conflicts(&Overrides::new(), &fixed), Vec::new());
    }

    #[test]
    fn two_actions_on_one_key_and_a_fixed_shortcut_are_conflicts() {
        let fixed = crate::fixed_bindings(cayenchat_storage::ChannelNumberModifier::Ctrl);
        let shared = conflicts(
            &overrides(&[
                ("next_channel", "ctrl-shift-n"),
                ("next_server", "ctrl-shift-n"),
            ]),
            &fixed,
        );
        assert_eq!(
            shared,
            [Conflict {
                key: "ctrl-shift-n".into(),
                owners: vec![Owner::Action("next_channel"), Owner::Action("next_server")],
            }]
        );
        // Settings (Cmd+, / Ctrl+,) cannot be taken.
        let taken = conflicts(&overrides(&[("next_channel", "secondary-,")]), &fixed);
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert_eq!(
            taken[0].owners,
            [Owner::Action("next_channel"), Owner::Fixed]
        );
        // The same key spelled another way is the same key.
        let respelled = conflicts(
            &overrides(&[
                ("next_channel", "shift-ctrl-n"),
                ("next_server", "ctrl-shift-n"),
            ]),
            &fixed,
        );
        assert_eq!(respelled.len(), 1, "{respelled:?}");
        // A key that is not usable is not a conflict (it is ignored).
        assert!(conflicts(&overrides(&[("next_channel", "a")]), &fixed).is_empty());
    }
}
