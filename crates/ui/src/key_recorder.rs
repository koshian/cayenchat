//! Recording the next key combination the user presses.
//!
//! Used to assign a shortcut by pressing it (#114). While a recording is
//! active the next keystroke is taken for itself: it reaches neither the
//! application's shortcuts nor a text field. What is recorded is the key the
//! way GPUI reports it, which is also how a binding is matched, so a recorded
//! key always works even where it is not what was typed (on macOS, Shift with a
//! symbol key arrives as the shifted character with Shift cleared:
//! Cmd+Shift+] is `cmd-}`).

use gpui::{App, Context, Keystroke, Subscription, Window};

use crate::shortcuts::{types_text_on, usable_key};

/// What a pressed key comes to while recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// A usable combination, in the form kept in settings (`cmd-}`).
    Key(String),
    /// Escape: recording stops and nothing changes. Escape itself cannot be
    /// assigned (it is how a recording is left).
    Cancelled,
    /// A key that cannot be assigned, with the reason.
    Rejected(Rejection),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// A letter, digit or symbol with no Ctrl, Alt or Cmd/Win held would
    /// stop that character from being typed.
    NeedsModifier,
    /// Option+letter on macOS (it types `å` and the like) or Ctrl+Alt+letter
    /// on Windows (AltGr): assigning it would take that character away.
    TypesText,
}

/// What a keystroke means to a recording, or `None` to keep waiting (a
/// keystroke that only makes part of a character, as an IME does).
/// `altgr` is whether Ctrl+Alt is AltGr on this system (Windows).
pub fn captured(keystroke: &Keystroke, altgr: bool) -> Option<Recorded> {
    if keystroke.is_ime_in_progress() {
        return None;
    }
    let modifiers = &keystroke.modifiers;
    if keystroke.key == "escape" && !modifiers.modified() {
        return Some(Recorded::Cancelled);
    }
    // Option+letter on macOS, and Ctrl+Alt+letter where that is AltGr: the
    // combinations that type a character.
    if types_text_on(keystroke, cfg!(target_os = "macos"), altgr) {
        return Some(Recorded::Rejected(Rejection::TypesText));
    }
    let key = keystroke.unparse();
    Some(match usable_key(&key) {
        Some(_) => Recorded::Key(key),
        None => Recorded::Rejected(Rejection::NeedsModifier),
    })
}

/// Takes the next keystroke of this application for `on_recorded`, until the
/// returned subscription is dropped. The keystroke is kept from every other
/// handler, so recording `cmd-}` does not also move to the next server.
/// Keystrokes that are only part of a character (IME) pass through.
pub fn record<V: 'static>(
    cx: &mut Context<V>,
    on_recorded: impl Fn(&mut V, Recorded, &mut Window, &mut Context<V>) + 'static,
) -> Subscription {
    let view = cx.weak_entity();
    cx.intercept_keystrokes(move |event, window, cx: &mut App| {
        let Some(recorded) = captured(&event.keystroke, cfg!(target_os = "windows")) else {
            return;
        };
        cx.stop_propagation();
        let _ = view.update(cx, |view, cx| on_recorded(view, recorded, window, cx));
    })
}

/// A label for a key as kept in settings, such as `Cmd+}` for `cmd-}`: the
/// modifiers in a fixed order, then the key by its name.
///
/// A symbol is shown as the character that arrives. On macOS that is the
/// shifted character with Shift cleared (Cmd+Shift+] is labelled `Cmd+}`),
/// which is right whatever the layout; guessing which key it is on would be
/// wrong on layouts that place symbols differently (JIS, for one).
pub fn describe(key: &str) -> String {
    let Ok(keystroke) = Keystroke::parse(key) else {
        return key.to_owned();
    };
    let modifiers = &keystroke.modifiers;
    let shift = modifiers.shift;
    let name = keystroke.key.clone();
    // The order the shortcut tables use (SHORTCUTS.md): Cmd, Ctrl, Opt, Shift.
    let mut parts: Vec<String> = Vec::new();
    if modifiers.platform {
        parts.push(
            if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Win"
            }
            .into(),
        );
    }
    if modifiers.control {
        parts.push("Ctrl".into());
    }
    if modifiers.alt {
        parts.push(
            if cfg!(target_os = "macos") {
                "Opt"
            } else {
                "Alt"
            }
            .into(),
        );
    }
    if shift {
        parts.push("Shift".into());
    }
    if modifiers.function {
        parts.push("Fn".into());
    }
    parts.push(key_name(&name));
    parts.join("+")
}

fn key_name(key: &str) -> String {
    match key {
        "up" => "Up".into(),
        "down" => "Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "tab" => "Tab".into(),
        "space" => "Space".into(),
        "enter" => "Enter".into(),
        "escape" => "Esc".into(),
        "backspace" => "Backspace".into(),
        "delete" => "Delete".into(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        // Function keys and any other name.
        key => {
            let mut chars = key.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key as a real press delivers it: a plain character comes with its
    /// text (without it, the IME would be taken to be composing).
    fn press(source: &str) -> Keystroke {
        Keystroke::parse(source).unwrap().with_simulated_ime()
    }

    #[test]
    fn a_modified_key_is_recorded_in_the_form_that_is_kept() {
        // The platform key is spelled as the platform does (`cmd` on macOS,
        // `super` on Linux, `win` on Windows): the kept form is what
        // `unparse` makes of the keystroke.
        assert_eq!(
            captured(&press("cmd-}"), false),
            Some(Recorded::Key(press("cmd-}").unparse()))
        );
        // Spelled another way, it is kept in the canonical order.
        assert_eq!(
            captured(&press("ctrl-alt-n"), false),
            captured(&press("alt-ctrl-n"), false)
        );
        assert_eq!(
            captured(&press("f5"), false),
            Some(Recorded::Key("f5".into()))
        );
        assert_eq!(
            captured(&press("alt-shift-space"), false),
            Some(Recorded::Key("alt-shift-space".into()))
        );
    }

    #[test]
    fn escape_cancels_and_a_bare_key_is_refused() {
        assert_eq!(captured(&press("escape"), false), Some(Recorded::Cancelled));
        // With a modifier Escape is just a key (and usable).
        assert_eq!(
            captured(&press("cmd-escape"), false),
            Some(Recorded::Key(press("cmd-escape").unparse()))
        );
        for bare in ["a", "shift-a", "1", "home", "space"] {
            assert_eq!(
                captured(&press(bare), false),
                Some(Recorded::Rejected(Rejection::NeedsModifier)),
                "{bare}"
            );
        }
    }

    #[test]
    fn altgr_typing_is_refused_where_altgr_exists() {
        // Ctrl+Alt+N typing "ń" on a Polish layout (the text comes with it,
        // but even without it the combination is how that letter is typed).
        let typed = Keystroke {
            key_char: Some("ń".into()),
            ..press("ctrl-alt-n")
        };
        for keystroke in [&typed, &press("ctrl-alt-n")] {
            assert_eq!(
                captured(keystroke, true),
                Some(Recorded::Rejected(Rejection::TypesText))
            );
        }
        // Where Ctrl+Alt is not AltGr, it is an ordinary combination.
        assert_eq!(
            captured(&typed, false),
            Some(Recorded::Key("ctrl-alt-n".into()))
        );
        // A named key with Ctrl+Alt types nothing, AltGr or not.
        assert_eq!(
            captured(&press("ctrl-alt-pageup"), true),
            Some(Recorded::Key("ctrl-alt-pageup".into()))
        );
    }

    #[test]
    fn option_letter_is_refused_on_macos_only() {
        let option_a = captured(&press("alt-a"), false);
        if cfg!(target_os = "macos") {
            assert_eq!(option_a, Some(Recorded::Rejected(Rejection::TypesText)));
        } else {
            assert_eq!(option_a, Some(Recorded::Key("alt-a".into())));
        }
        // With Cmd (or Ctrl) held it types nothing.
        assert_eq!(
            captured(&press("ctrl-alt-a"), false),
            Some(Recorded::Key("ctrl-alt-a".into()))
        );
    }

    #[test]
    fn a_key_in_the_middle_of_an_ime_composition_is_not_a_recording() {
        // A plain character with no text yet: the IME is composing.
        let composing = Keystroke {
            modifiers: Default::default(),
            key: "a".into(),
            key_char: None,
        };
        assert_eq!(captured(&composing, false), None);
    }

    #[test]
    fn labels_show_what_to_press() {
        #[cfg(target_os = "macos")]
        {
            // macOS reports Cmd+Shift+] as `cmd-}`; the label is the symbol
            // that arrives, which holds on any layout.
            assert_eq!(describe("cmd-}"), "Cmd+}");
            assert_eq!(describe("cmd-{"), "Cmd+{");
            assert_eq!(describe("cmd-["), "Cmd+[");
            assert_eq!(describe("ctrl-alt-up"), "Ctrl+Opt+Up");
            assert_eq!(describe("cmd-alt-left"), "Cmd+Opt+Left");
            assert_eq!(describe("cmd-ctrl-1"), "Cmd+Ctrl+1");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(describe("ctrl-alt-pagedown"), "Ctrl+Alt+PageDown");
            assert_eq!(describe("alt-shift-space"), "Alt+Shift+Space");
        }
        assert_eq!(describe("ctrl-tab"), "Ctrl+Tab");
        assert_eq!(describe("ctrl-shift-tab"), "Ctrl+Shift+Tab");
        assert_eq!(describe("f5"), "F5");
        assert_eq!(describe("ctrl-a"), "Ctrl+A");
        // What cannot be read back is shown as it is.
        assert_eq!(describe(""), describe(""));
    }

    struct Probe {
        recorded: Vec<Recorded>,
        shortcut_fired: usize,
        subscription: Option<Subscription>,
        focus: gpui::FocusHandle,
    }

    impl gpui::Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            use gpui::{InteractiveElement, ParentElement, Styled};
            gpui::div()
                .size_full()
                .key_context("Probe")
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &crate::CopyDiagnostics, _, _| {
                    this.shortcut_fired += 1;
                }))
                .child("recorder")
        }
    }

    #[gpui::test]
    fn while_recording_the_key_is_taken_and_does_not_run_its_shortcut(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::KeyBinding;

        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new(
                "ctrl-alt-n",
                crate::CopyDiagnostics,
                Some("Probe"),
            )]);
        });
        let (probe, cx) = cx.add_window_view(|window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus);
            Probe {
                recorded: Vec::new(),
                shortcut_fired: 0,
                subscription: None,
                focus,
            }
        });
        let fired = |cx: &mut gpui::VisualTestContext| probe.read_with(cx, |p, _| p.shortcut_fired);

        // Not recording: the shortcut works.
        cx.simulate_keystrokes("ctrl-alt-n");
        assert_eq!(fired(cx), 1);

        // Recording: the key is recorded and its shortcut does not run.
        probe.update(cx, |probe, cx| {
            probe.subscription = Some(record(cx, |probe, recorded, _, _| {
                probe.recorded.push(recorded);
            }));
        });
        cx.simulate_keystrokes("ctrl-alt-n");
        assert_eq!(fired(cx), 1, "the key was taken, not run");
        cx.simulate_keystrokes("a");
        cx.simulate_keystrokes("escape");
        let recorded = probe.read_with(cx, |p, _| p.recorded.clone());
        assert_eq!(
            recorded,
            [
                Recorded::Key("ctrl-alt-n".into()),
                Recorded::Rejected(Rejection::NeedsModifier),
                Recorded::Cancelled,
            ]
        );

        // Done recording (the subscription is dropped): keys work again.
        probe.update(cx, |probe, _| probe.subscription = None);
        cx.simulate_keystrokes("ctrl-alt-n");
        assert_eq!(fired(cx), 2);
        assert_eq!(probe.read_with(cx, |p, _| p.recorded.len()), 3);
    }
}
