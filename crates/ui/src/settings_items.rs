// SPDX-License-Identifier: GPL-3.0-only
//! Settings items declared as data (#262).
//!
//! A tab lists its items in a table: the control (a switch or a text field),
//! the label, the hint, and how the value is read from and written to
//! [`Settings`]. The shared code in this module and in `settings_window`
//! builds the text fields, reads and validates them for the autosave
//! snapshot, resets them and draws the rows, so a new item is one more entry
//! in a table. Screens with their own behaviour (auto-join list, avatar
//! editor, color picker, shortcut recorder, secrets) stay separate widgets.
//!
//! Validation lives in `store` and runs on the typed text each time a
//! snapshot is taken; the autosave, "Restore Defaults" and the localised
//! placeholders all go through the same table.

use cayenchat_app::notifications;
use cayenchat_storage::{
    ChannelNumberModifier, LinuxDisplay, NoticeKey, SendKey, Settings, TextKeyTheme,
    UnreadLogPosition,
};
use gpui::{prelude::*, *};

use crate::{SettingsWindow, input::TextInput, settings_theme, settings_window::SettingsForm};

/// How an item reads and writes its value.
pub(crate) enum Control {
    /// A checkbox row.
    Toggle {
        get: fn(&Settings) -> bool,
        set: fn(&mut Settings, bool),
    },
    /// A text field. `show` is the text for a value, `store` parses typed
    /// text into the settings and says why it cannot (nothing is changed on
    /// an error).
    Text {
        placeholder_key: &'static str,
        show: fn(&Settings) -> String,
        store: fn(&mut Settings, &str) -> Result<(), String>,
    },
    /// A row of mutually exclusive choices (an enum setting).
    Choice(&'static [ChoiceOption]),
    /// Explanatory text under the row above.
    Note,
}

/// One choice of a [`Control::Choice`]: its label and how it is read from
/// and written to [`Settings`].
pub(crate) struct ChoiceOption {
    pub label_key: &'static str,
    pub is_selected: fn(&Settings) -> bool,
    pub select: fn(&mut Settings),
}

/// `option!("label_key", field.path = Value)` declares a [`ChoiceOption`]
/// that is selected while the field equals the value.
macro_rules! option {
    ($key:literal, $($field:ident).+ = $value:expr) => {
        ChoiceOption {
            label_key: $key,
            is_selected: |s| s.$($field).+ == $value,
            select: |s| s.$($field).+ = $value,
        }
    };
}

/// One declared item.
pub(crate) struct Item {
    /// Stable id of the row; also keys the text field of a `Text` control.
    pub id: &'static str,
    /// Localization key of the label (the note's text for `Note`).
    pub label_key: &'static str,
    pub control: Control,
    /// Whether the platform shows it.
    pub visible: bool,
    /// When set, the row is usable only while this holds.
    pub enabled_when: Option<fn(&Settings) -> bool>,
}

impl Item {
    const fn new(id: &'static str, label_key: &'static str, control: Control) -> Self {
        Self {
            id,
            label_key,
            control,
            visible: true,
            enabled_when: None,
        }
    }

    const fn toggle(
        id: &'static str,
        label_key: &'static str,
        get: fn(&Settings) -> bool,
        set: fn(&mut Settings, bool),
    ) -> Self {
        Self::new(id, label_key, Control::Toggle { get, set })
    }

    const fn text(
        id: &'static str,
        label_key: &'static str,
        placeholder_key: &'static str,
        show: fn(&Settings) -> String,
        store: fn(&mut Settings, &str) -> Result<(), String>,
    ) -> Self {
        Self::new(
            id,
            label_key,
            Control::Text {
                placeholder_key,
                show,
                store,
            },
        )
    }

    const fn choice(
        id: &'static str,
        label_key: &'static str,
        options: &'static [ChoiceOption],
    ) -> Self {
        Self::new(id, label_key, Control::Choice(options))
    }

    const fn note(id: &'static str, text_key: &'static str) -> Self {
        Self::new(id, text_key, Control::Note)
    }

    const fn only_if(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    const fn enabled_when(mut self, condition: fn(&Settings) -> bool) -> Self {
        self.enabled_when = Some(condition);
        self
    }

    pub(crate) fn enabled(&self, settings: &Settings) -> bool {
        self.enabled_when
            .is_none_or(|condition| condition(settings))
    }
}

fn notifications_on(settings: &Settings) -> bool {
    settings.notifications.enabled
}

/// The Notifications tab.
pub(crate) const NOTIFICATIONS: &[Item] = &[
    Item::toggle(
        "notifications-enabled",
        "notifications_enabled",
        |s| s.notifications.enabled,
        |s, on| s.notifications.enabled = on,
    ),
    Item::toggle(
        "notify-mentions",
        "notify_mentions",
        |s| s.notifications.mentions,
        |s, on| s.notifications.mentions = on,
    )
    .enabled_when(notifications_on),
    Item::toggle(
        "notify-private-messages",
        "notify_private_messages",
        |s| s.notifications.private_messages,
        |s, on| s.notifications.private_messages = on,
    )
    .enabled_when(notifications_on),
    Item::toggle(
        "notify-keywords",
        "notify_keywords",
        |s| s.notifications.keyword_alerts,
        |s, on| s.notifications.keyword_alerts = on,
    )
    .enabled_when(notifications_on),
    Item::text(
        "keywords",
        "keywords",
        "keywords_placeholder",
        |s| s.notifications.keywords.join(", "),
        |s, text| {
            s.notifications.keywords = notifications::parse_keywords(text);
            Ok(())
        },
    ),
    Item::note("keywords-hint", "keywords_hint"),
    Item::text(
        "exclude-keywords",
        "exclude_keywords",
        "exclude_keywords_placeholder",
        |s| s.notifications.exclude_keywords.join(", "),
        |s, text| {
            s.notifications.exclude_keywords = notifications::parse_keywords(text);
            Ok(())
        },
    ),
    Item::note("exclude-keywords-hint", "exclude_keywords_hint"),
    Item::toggle(
        "notify-sound",
        "notify_sound",
        |s| s.notifications.sound,
        |s, on| s.notifications.sound = on,
    )
    .enabled_when(notifications_on)
    .only_if(cfg!(target_os = "windows")),
    Item::note("notifications-hint-macos", "notifications_hint_macos")
        .only_if(cfg!(target_os = "macos")),
    Item::note("notifications-hint", "notifications_hint").only_if(!cfg!(target_os = "macos")),
];

/// The Application tab, from "Always on top" to the double-click
/// confirmations. The language list, the autostart row and the option rows
/// around the tables are their own widgets.
pub(crate) const APPLICATION_WINDOW: &[Item] = &[
    Item::toggle(
        "always-on-top",
        "always_on_top",
        |s| s.always_on_top,
        |s, on| s.always_on_top = on,
    )
    .only_if(cfg!(target_os = "windows")),
    Item::toggle(
        "restore-window-layout",
        "restore_window_layout",
        |s| s.restore_window_layout,
        |s, on| s.restore_window_layout = on,
    ),
    Item::toggle(
        "confirm-quit",
        "confirm_quit",
        |s| s.confirm_quit,
        |s, on| s.confirm_quit = on,
    ),
    Item::toggle(
        "shorten-amazon-urls",
        "shorten_amazon_urls",
        |s| s.shorten_amazon_urls,
        |s, on| s.shorten_amazon_urls = on,
    ),
    Item::note("shorten-amazon-urls-hint", "shorten_amazon_urls_hint"),
    Item::toggle(
        "confirm-multiline-paste",
        "confirm_multiline_paste",
        |s| s.confirm_multiline_paste,
        |s, on| s.confirm_multiline_paste = on,
    ),
    Item::note(
        "confirm-multiline-paste-hint",
        "confirm_multiline_paste_hint",
    ),
    Item::toggle(
        "confirm-server-dblclick-disconnect",
        "confirm_server_dblclick_disconnect",
        |s| s.confirm_server_dblclick_disconnect,
        |s, on| s.confirm_server_dblclick_disconnect = on,
    ),
    Item::toggle(
        "confirm-channel-dblclick-part",
        "confirm_channel_dblclick_part",
        |s| s.confirm_channel_dblclick_part,
        |s, on| s.confirm_channel_dblclick_part = on,
    ),
    Item::choice(
        "unread_log_position",
        "unread_log_position",
        &[
            option!(
                "unread_log_position_previous",
                unread_log_position = UnreadLogPosition::Previous
            ),
            option!(
                "unread_log_position_latest",
                unread_log_position = UnreadLogPosition::Latest
            ),
        ],
    ),
    Item::note("unread-log-position-hint", "unread_log_position_hint"),
];

/// The menu bar row (macOS has its own menu bar that is always shown) and
/// the display server row (Linux) of the Application tab.
pub(crate) const APPLICATION_MENU: &[Item] = &[
    Item::toggle(
        "menu-bar-auto-hide",
        "menu_bar_auto_hide",
        |s| s.menu_bar_auto_hide,
        |s, on| s.menu_bar_auto_hide = on,
    )
    .only_if(!cfg!(target_os = "macos")),
    Item::choice(
        "linux_display",
        "linux_display",
        &[
            option!(
                "linux_display_wayland",
                linux_display = LinuxDisplay::Wayland
            ),
            option!("linux_display_x11", linux_display = LinuxDisplay::X11),
        ],
    )
    .only_if(cfg!(unix_desktop)),
    Item::note("linux-display-hint", "linux_display_hint").only_if(cfg!(unix_desktop)),
];

/// The send and notice key rows of the Keyboard tab. The clash warning
/// depends on both and is drawn between this table and `KEYBOARD_MODIFIERS`.
pub(crate) const KEYBOARD_SEND: &[Item] = &[
    Item::choice(
        "send_key",
        "send_key",
        &[
            option!("send_key_enter", send_key = SendKey::Enter),
            option!("send_key_ctrl_enter", send_key = SendKey::CtrlEnter),
            option!("send_key_double_enter", send_key = SendKey::DoubleEnter),
        ],
    ),
    Item::note("send-key-hint", "send_key_hint"),
    Item::choice(
        "notice_key",
        "notice_key",
        &[
            option!("notice_key_off", notice_key = NoticeKey::Off),
            option!("notice_key_ctrl_enter", notice_key = NoticeKey::CtrlEnter),
            option!("notice_key_alt_enter", notice_key = NoticeKey::AltEnter),
        ],
    ),
    Item::note("notice-key-hint", "notice_key_hint"),
];

/// The channel number modifier (not macOS) and text key theme (Linux and
/// other Unix desktops) rows of the Keyboard tab. The shortcut recorder
/// rows below are their own widgets.
pub(crate) const KEYBOARD_MODIFIERS: &[Item] = &[
    Item::choice(
        "channel_number_modifier",
        "channel_number_modifier",
        &[
            option!(
                "channel_number_modifier_ctrl",
                channel_number_modifier = ChannelNumberModifier::Ctrl
            ),
            option!(
                "channel_number_modifier_alt",
                channel_number_modifier = ChannelNumberModifier::Alt
            ),
            option!(
                "channel_number_modifier_super",
                channel_number_modifier = ChannelNumberModifier::Super
            ),
        ],
    )
    .only_if(!cfg!(target_os = "macos")),
    Item::note(
        "channel-number-modifier-hint",
        "channel_number_modifier_hint",
    )
    .only_if(!cfg!(target_os = "macos")),
    Item::choice(
        "text_key_theme",
        "text_key_theme",
        &[
            option!("text_key_theme_auto", text_key_theme = TextKeyTheme::Auto),
            option!(
                "text_key_theme_standard",
                text_key_theme = TextKeyTheme::Standard
            ),
            option!("text_key_theme_emacs", text_key_theme = TextKeyTheme::Emacs),
        ],
    )
    .only_if(cfg!(unix_desktop)),
    Item::note("text-key-theme-hint", "text_key_theme_hint").only_if(cfg!(unix_desktop)),
];

/// The image preview row of the Application tab.
pub(crate) const APPLICATION_PREVIEWS: &[Item] = &[
    Item::toggle(
        "image-previews",
        "image_previews",
        |s| s.appearance.image_previews,
        |s, on| s.appearance.image_previews = on,
    ),
    Item::note("image-previews-hint", "image_previews_hint"),
];

/// The URL and character reference rows of the Application tab.
pub(crate) const APPLICATION_TEXT: &[Item] = &[
    Item::toggle(
        "compact-urls",
        "compact_urls",
        |s| s.appearance.compact_urls,
        |s, on| s.appearance.compact_urls = on,
    ),
    Item::note("compact-urls-hint", "compact_urls_hint"),
    Item::toggle(
        "decode-char-refs",
        "decode_char_refs",
        |s| s.appearance.decode_char_refs,
        |s, on| s.appearance.decode_char_refs = on,
    ),
    Item::note("decode-char-refs-hint", "decode_char_refs_hint"),
];

/// The automatic phrase row of the Experimental tab. The debug log rows
/// below it choose a file and are their own widgets.
pub(crate) const EXPERIMENTAL: &[Item] = &[
    Item::toggle(
        "auto-phrase-toggle",
        "auto_phrase",
        |s| s.experimental.auto_phrase,
        |s, on| s.experimental.auto_phrase = on,
    ),
    Item::note("auto-phrase-hint", "auto_phrase_hint"),
];

/// Every table; the shared code visits the text fields of all of them.
const TABLES: &[&[Item]] = &[
    NOTIFICATIONS,
    EXPERIMENTAL,
    APPLICATION_WINDOW,
    APPLICATION_MENU,
    APPLICATION_PREVIEWS,
    APPLICATION_TEXT,
    KEYBOARD_SEND,
    KEYBOARD_MODIFIERS,
];

/// The text items of every table, in declaration order.
fn text_items() -> impl Iterator<Item = &'static Item> {
    TABLES
        .iter()
        .flat_map(|table| table.iter())
        .filter(|item| matches!(item.control, Control::Text { .. }))
}

impl SettingsForm {
    /// Creates the text fields of every declared text item.
    pub(crate) fn item_fields(
        values: &Settings,
        i18n: &crate::localization::Localizer,
        cx: &mut App,
    ) -> Vec<(&'static str, Entity<TextInput>)> {
        text_items()
            .filter_map(|item| {
                let Control::Text {
                    placeholder_key,
                    show,
                    ..
                } = &item.control
                else {
                    return None;
                };
                let text = show(values);
                let placeholder = i18n.text(placeholder_key);
                Some((
                    item.id,
                    cx.new(|cx| TextInput::new_settings_field(&placeholder, &text, false, cx)),
                ))
            })
            .collect()
    }

    /// The text field of the declared item `id`.
    pub(crate) fn item_field(&self, id: &str) -> Option<&Entity<TextInput>> {
        self.item_fields
            .iter()
            .find_map(|(field_id, field)| (*field_id == id).then_some(field))
    }

    /// Parses every declared text field into `settings`.
    pub(crate) fn read_item_fields(&self, settings: &mut Settings, cx: &App) -> Result<(), String> {
        for item in text_items() {
            if let (Control::Text { store, .. }, Some(field)) =
                (&item.control, self.item_field(item.id))
            {
                store(settings, field.read(cx).text())?;
            }
        }
        Ok(())
    }
}

impl SettingsWindow {
    /// Shows the values of `table`'s text fields again, from `values` (used
    /// by "Restore Defaults").
    pub(crate) fn show_item_fields(
        &mut self,
        table: &'static [Item],
        values: &Settings,
        cx: &mut Context<Self>,
    ) {
        for item in table {
            if let (Control::Text { show, .. }, Some(field)) =
                (&item.control, self.settings.item_field(item.id))
            {
                let text = show(values);
                field.update(cx, |field, cx| field.set_text(&text, cx));
            }
        }
    }

    /// Refreshes the placeholders of the declared text fields after the
    /// language changed.
    pub(crate) fn translate_item_fields(&mut self, cx: &mut Context<Self>) {
        for item in text_items() {
            if let (
                Control::Text {
                    placeholder_key, ..
                },
                Some(field),
            ) = (&item.control, self.settings.item_field(item.id))
            {
                let placeholder = self.i18n.text(placeholder_key);
                field.update(cx, |field, cx| field.set_placeholder(&placeholder, cx));
            }
        }
    }

    /// Draws the rows of `table` below one another.
    pub(crate) fn render_items(
        &self,
        table: &'static [Item],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = settings_theme::palette(cx);
        table
            .iter()
            .filter(|item| item.visible)
            .filter_map(|item| match &item.control {
                Control::Toggle { get, set } => {
                    let enabled = item.enabled(&self.settings.values);
                    let checked = get(&self.settings.values);
                    let set = *set;
                    Some(
                        div()
                            .id(item.id)
                            .ml(settings_theme::LABEL_INDENT)
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(!enabled, |d| d.text_color(theme.text_secondary))
                            .when(enabled, |d| d.cursor_pointer())
                            .child(settings_theme::checkbox(checked, enabled, cx))
                            .child(self.i18n.text(item.label_key))
                            .when(enabled, |d| {
                                d.on_click(cx.listener(move |this, _, _, cx| {
                                    let on = !get(&this.settings.values);
                                    set(&mut this.settings.values, on);
                                    cx.notify();
                                }))
                            })
                            .into_any_element(),
                    )
                }
                Control::Text { .. } => {
                    let field = self.settings.item_field(item.id)?.clone();
                    Some(
                        crate::settings_window::settings_field(
                            &self.i18n.text(item.label_key),
                            field,
                        )
                        .into_any_element(),
                    )
                }
                Control::Choice(options) => {
                    let options = options
                        .iter()
                        .map(|option| {
                            (
                                option.label_key,
                                (option.is_selected)(&self.settings.values),
                                Box::new(option.select) as Box<dyn Fn(&mut Settings)>,
                            )
                        })
                        .collect();
                    Some(
                        self.choice_row(item.label_key, options, cx)
                            .into_any_element(),
                    )
                }
                Control::Note => Some(
                    div()
                        .ml(settings_theme::LABEL_INDENT)
                        .text_color(theme.text_secondary)
                        .child(self.i18n.text(item.label_key))
                        .into_any_element(),
                ),
            })
            .collect()
    }
}

/// Called with the item and its new value when a Kit switch is flipped.
pub(crate) type ToggleHandler = std::rc::Rc<dyn Fn(&'static Item, bool, &mut App)>;

/// Called with the item and the index of the option when a Kit dropdown changes.
pub(crate) type ChoiceHandler = std::rc::Rc<dyn Fn(&'static Item, usize, &mut App)>;

/// Converts the visible items of `table` into Kit `setting` items (the thin
/// adapter to `gpui-component::setting`).
///
/// A switch becomes `SettingField::switch`, a text item wraps the existing
/// `TextInput` (found by `field`; an item without one is skipped) in
/// `SettingField::render`, and a note becomes the description of the item
/// above it. `on_toggle` receives the item and the new value when the Kit
/// switch is flipped, `on_choose` the item and option index when a choice
/// (a dropdown) changes. Kit offers no standalone item renderer (`Settings`
/// always draws its sidebar and search), so the screen still draws through
/// `render_items`; the Kit render path is taken when #622/#623 need it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn kit_items(
    table: &'static [Item],
    values: &Settings,
    i18n: &crate::localization::Localizer,
    field: impl Fn(&str) -> Option<Entity<TextInput>>,
    on_toggle: ToggleHandler,
    on_choose: ChoiceHandler,
) -> Vec<gpui_component::setting::SettingItem> {
    use gpui_component::setting::{SettingField, SettingItem};

    let mut out: Vec<SettingItem> = Vec::new();
    for item in table.iter().filter(|item| item.visible) {
        let disabled = !item.enabled(values);
        match &item.control {
            Control::Toggle { get, .. } => {
                let checked = get(values);
                let on_toggle = on_toggle.clone();
                out.push(
                    SettingItem::new(
                        i18n.text(item.label_key),
                        SettingField::switch(
                            move |_| checked,
                            move |on, cx| on_toggle(item, on, cx),
                        ),
                    )
                    .disabled(disabled),
                );
            }
            Control::Text { .. } => {
                let Some(input) = field(item.id) else {
                    continue;
                };
                out.push(
                    SettingItem::new(
                        i18n.text(item.label_key),
                        SettingField::render(move |_, _, _| input.clone()),
                    )
                    .disabled(disabled),
                );
            }
            Control::Choice(options) => {
                // The dropdown's values are the option indexes.
                let labels = options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| {
                        (index.to_string().into(), i18n.text(option.label_key).into())
                    })
                    .collect();
                let selected = options
                    .iter()
                    .position(|option| (option.is_selected)(values))
                    .unwrap_or(0)
                    .to_string();
                let on_choose = on_choose.clone();
                out.push(
                    SettingItem::new(
                        i18n.text(item.label_key),
                        SettingField::dropdown(
                            labels,
                            move |_| selected.clone().into(),
                            move |value, cx| {
                                if let Ok(index) = value.parse() {
                                    on_choose(item, index, cx);
                                }
                            },
                        ),
                    )
                    .disabled(disabled),
                );
            }
            Control::Note => {
                if let Some(SettingItem::Item { description, .. }) = out.last_mut() {
                    *description = Some(i18n.text(item.label_key).into());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Control, KEYBOARD_SEND, NOTIFICATIONS, Settings, TABLES, kit_items};
    use gpui_component::setting::SettingItem;
    use std::rc::Rc;

    fn convert(settings: &Settings) -> Vec<SettingItem> {
        let i18n = crate::localization::Localizer::new(Default::default());
        kit_items(
            NOTIFICATIONS,
            settings,
            &i18n,
            |_| None,
            Rc::new(|_, _, _| {}),
            Rc::new(|_, _, _| {}),
        )
    }

    #[test]
    fn kit_items_follow_the_declared_toggles() {
        // Text fields need an app context and are skipped here.
        let settings = Settings::default();
        let items = convert(&settings);
        let toggles = NOTIFICATIONS
            .iter()
            .filter(|item| item.visible && matches!(item.control, Control::Toggle { .. }))
            .count();
        assert_eq!(items.len(), toggles);
    }

    #[test]
    fn kit_items_disable_dependent_toggles() {
        let mut settings = Settings::default();
        settings.notifications.enabled = false;
        let disabled: Vec<bool> = convert(&settings)
            .iter()
            .map(|item| matches!(item, SettingItem::Item { disabled: true, .. }))
            .collect();
        // The master switch stays usable; the dependent ones are disabled.
        assert_eq!(disabled.first(), Some(&false));
        assert!(disabled.iter().skip(1).all(|d| *d));
    }

    #[test]
    fn notes_become_descriptions_of_the_item_above() {
        let items = convert(&Settings::default());
        // The trailing platform hint attaches to the last toggle.
        let Some(SettingItem::Item { description, .. }) = items.last() else {
            panic!("an item");
        };
        assert!(description.is_some());
    }

    #[test]
    fn ids_are_unique_across_tables() {
        let mut ids: Vec<&str> = TABLES
            .iter()
            .flat_map(|table| table.iter())
            .map(|item| item.id)
            .collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn toggles_write_what_they_read() {
        for item in TABLES.iter().flat_map(|table| table.iter()) {
            if let Control::Toggle { get, set } = &item.control {
                let mut settings = Settings::default();
                let before = get(&settings);
                set(&mut settings, !before);
                assert_eq!(get(&settings), !before, "{}", item.id);
            }
        }
    }

    #[test]
    fn keywords_round_trip_through_their_text() {
        let mut settings = Settings::default();
        settings.notifications.keywords = vec!["cat".into(), "dog".into()];
        for item in NOTIFICATIONS {
            if let Control::Text { show, store, .. } = &item.control {
                let text = show(&settings);
                let mut other = Settings::default();
                assert!(store(&mut other, &text).is_ok());
                assert_eq!(show(&other), text, "{}", item.id);
            }
        }
    }

    #[test]
    fn dependent_toggles_follow_the_master_switch() {
        let mut settings = Settings::default();
        settings.notifications.enabled = false;
        let mentions = NOTIFICATIONS
            .iter()
            .find(|item| item.id == "notify-mentions")
            .expect("declared");
        assert!(!mentions.enabled(&settings));
        settings.notifications.enabled = true;
        assert!(mentions.enabled(&settings));
    }

    #[test]
    fn choices_select_exactly_one_option_and_write_what_they_read() {
        for item in TABLES.iter().flat_map(|table| table.iter()) {
            let Control::Choice(options) = &item.control else {
                continue;
            };
            for (index, option) in options.iter().enumerate() {
                let mut settings = Settings::default();
                (option.select)(&mut settings);
                let selected: Vec<usize> = options
                    .iter()
                    .enumerate()
                    .filter(|(_, other)| (other.is_selected)(&settings))
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(selected, [index], "{} option {index}", item.id);
            }
        }
    }

    #[test]
    fn kit_items_turn_choices_into_dropdowns() {
        let items = kit_items(
            KEYBOARD_SEND,
            &Settings::default(),
            &crate::localization::Localizer::new(Default::default()),
            |_| None,
            Rc::new(|_, _, _| {}),
            Rc::new(|_, _, _| {}),
        );
        // Two choices; their hints become descriptions.
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| matches!(
            item,
            SettingItem::Item {
                description: Some(_),
                ..
            }
        )));
    }
}
