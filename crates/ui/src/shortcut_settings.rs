//! The Shortcuts tab: the navigation shortcuts by action, with the keys each
//! has now. A key is changed by pressing the new combination (#114); a change
//! can be undone for one action or for all of them.

use gpui::{prelude::*, *};

use crate::{
    SettingsWindow, account_settings,
    ircv3_settings::TextTooltip,
    key_recorder::{self, Recorded, Rejection},
    settings_theme,
    shortcuts::{self, Caution, Owner, ShortcutAction},
};

/// A key being recorded for one action.
pub(crate) struct ShortcutRecording {
    action: &'static str,
    /// Why the last key pressed was refused, as a text key.
    notice: Option<&'static str>,
    /// Ends the recording when dropped.
    _subscription: Subscription,
}

impl SettingsWindow {
    /// The text of an action's name.
    fn shortcut_name(&self, id: &str) -> String {
        self.i18n.text(&format!("shortcut_{id}"))
    }

    /// Warnings for the keys `action` has now: keys shared with another action
    /// or a fixed shortcut, and combinations better avoided.
    pub(crate) fn shortcut_warnings(&self, action: &ShortcutAction) -> Vec<String> {
        let overrides = &self.settings.values.keybindings;
        let fixed = crate::fixed_bindings(self.settings.values.channel_number_modifier);
        let mut warnings: Vec<String> = Vec::new();
        for conflict in shortcuts::conflicts(overrides, &fixed) {
            if !conflict.owners.contains(&Owner::Action(action.id)) {
                continue;
            }
            for owner in &conflict.owners {
                match owner {
                    Owner::Action(other) if *other != action.id => warnings.push(self.i18n.format(
                        "shortcut_conflict_action",
                        &[("other", &self.shortcut_name(other))],
                    )),
                    Owner::Fixed => warnings.push(self.i18n.text("shortcut_conflict_fixed")),
                    Owner::Action(_) => {}
                }
            }
        }
        for key in shortcuts::keys(action, overrides) {
            let Some(keystroke) = shortcuts::usable_key(&key) else {
                continue;
            };
            match shortcuts::caution(&keystroke) {
                Some(Caution::VoiceOver) => warnings.push(self.i18n.text("shortcut_voiceover")),
                Some(Caution::AltGr) => warnings.push(self.i18n.text("shortcut_altgr")),
                None => {}
            }
        }
        warnings.dedup();
        warnings
    }

    /// Starts recording a key for `action`; again on the same action stops.
    pub(crate) fn toggle_shortcut_recording(
        &mut self,
        action: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self
            .shortcut_recording
            .as_ref()
            .is_some_and(|recording| recording.action == action)
        {
            self.shortcut_recording = None;
        } else {
            let subscription = key_recorder::record(cx, move |this, recorded, _, cx| {
                this.shortcut_recorded(action, recorded, cx)
            });
            self.shortcut_recording = Some(ShortcutRecording {
                action,
                notice: None,
                _subscription: subscription,
            });
        }
        cx.notify();
    }

    /// What the recording came to. A refused key keeps recording, with the
    /// reason shown; a key or Escape ends it.
    pub(crate) fn shortcut_recorded(
        &mut self,
        action: &'static str,
        recorded: Recorded,
        cx: &mut Context<Self>,
    ) {
        match recorded {
            Recorded::Key(key) => {
                self.shortcut_recording = None;
                // The action's only default, chosen again, is no change.
                let default = shortcuts::ACTIONS
                    .iter()
                    .find(|candidate| candidate.id == action)
                    .filter(|candidate| candidate.defaults.len() == 1)
                    .and_then(|candidate| shortcuts::usable_key(candidate.defaults[0]))
                    .map(|keystroke| keystroke.unparse());
                if default.as_deref() == Some(key.as_str()) {
                    self.settings.values.keybindings.remove(action);
                } else {
                    self.settings
                        .values
                        .keybindings
                        .insert(action.to_owned(), key);
                }
            }
            Recorded::Cancelled => self.shortcut_recording = None,
            Recorded::Rejected(reason) => {
                if let Some(recording) = &mut self.shortcut_recording {
                    recording.notice = Some(match reason {
                        Rejection::NeedsModifier => "shortcut_needs_modifier",
                        Rejection::TypesText => "shortcut_types_text",
                    });
                }
            }
        }
        cx.notify();
    }

    fn shortcut_row(
        &self,
        index: usize,
        action: &'static ShortcutAction,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = settings_theme::palette(cx);
        let overrides = &self.settings.values.keybindings;
        let changed = overrides.contains_key(action.id);
        let keys = shortcuts::keys(action, overrides)
            .iter()
            .map(|key| key_recorder::describe(key))
            .collect::<Vec<_>>()
            .join(", ");
        let recording = self
            .shortcut_recording
            .as_ref()
            .filter(|recording| recording.action == action.id);
        let warnings = self.shortcut_warnings(action);
        let id = action.id;
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(230.))
                    .flex_shrink_0()
                    .child(self.shortcut_name(id)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(changed && recording.is_none(), |d| {
                        d.font_weight(FontWeight::BOLD)
                    })
                    .child(match recording {
                        Some(recording) => {
                            div()
                                .text_color(theme.text_secondary)
                                .child(match recording.notice {
                                    Some(key) => self.i18n.text(key),
                                    None => self.i18n.text("shortcut_press"),
                                })
                        }
                        None => div().child(keys),
                    }),
            )
            .when(!warnings.is_empty(), |row| {
                let text: SharedString = warnings.join("\n").into();
                row.child(
                    div()
                        .id(("shortcut-warning", index))
                        .flex_shrink_0()
                        .text_color(theme.warning)
                        .child("⚠")
                        .tooltip(move |_, cx| {
                            let text = text.clone();
                            cx.new(|_| TextTooltip(text)).into()
                        }),
                )
            })
            .child(
                settings_theme::button(("shortcut-change", index), recording.is_some(), cx)
                    .debug_selector(move || format!("shortcut-change-{id}"))
                    .child(self.i18n.text("shortcut_change"))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_shortcut_recording(id, cx)),
                    ),
            )
            .child(div().w(px(90.)).flex_shrink_0().when(changed, |slot| {
                slot.child(
                    settings_theme::button(("shortcut-reset", index), false, cx)
                        .debug_selector(move || format!("shortcut-reset-{id}"))
                        .child(self.i18n.text("shortcut_reset"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.values.keybindings.remove(id);
                            if this
                                .shortcut_recording
                                .as_ref()
                                .is_some_and(|recording| recording.action == id)
                            {
                                this.shortcut_recording = None;
                            }
                            cx.notify();
                        })),
                )
            }))
    }

    pub(crate) fn render_shortcut_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = settings_theme::palette(cx);
        let any_changed = !self.settings.values.keybindings.is_empty();
        let mut panel = account_settings::panel(cx).child(
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::BOLD)
                .child(self.i18n.text("shortcuts_tab")),
        );
        for (index, action) in shortcuts::ACTIONS.iter().enumerate() {
            panel = panel.child(self.shortcut_row(index, action, cx));
        }
        panel
            .when(any_changed, |panel| {
                panel.child(
                    settings_theme::button("shortcut-reset-all", false, cx)
                        .debug_selector(|| "shortcut-reset-all".into())
                        .child(self.i18n.text("shortcut_reset_all"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.values.keybindings.clear();
                            this.shortcut_recording = None;
                            cx.notify();
                        })),
                )
            })
            .when_some(self.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
    }
}
