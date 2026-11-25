// SPDX-License-Identifier: GPL-3.0-only
//! The settings window drawn by the GPUI Kit's `Settings` (#622).
//!
//! The Kit supplies the sidebar with its search field, the pages, the groups
//! under headings and the rows (name, one-line description, control). Its
//! `switch`, `dropdown` and `number_input` fields draw the matching settings.
//! What the Kit has no part for goes into a Kit row through
//! `SettingField::render` or `SettingItem::render`: the color swatches and
//! the light/dark columns, the sliders, the wallpapers, the theme library
//! and the tabs that are not rebuilt yet (each is one item holding its
//! previous panel, so what it shows and does is unchanged).
//!
//! The Kit's setters get only `&mut App`, so a setter reaches the window
//! through a weak handle and does what the previous control did: change the
//! form, which the existing autosave then validates and stores.

use std::rc::Rc;

use cayenchat_storage::{MAX_FONT_SIZE, MIN_FONT_SIZE};
use gpui::{prelude::*, *};
use gpui_component::{
    select::{SearchableVec, Select, SelectDelegate, SelectEvent, SelectItem, SelectState},
    setting::{
        NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage,
        Settings,
    },
};

use crate::{
    SettingsWindow,
    input::TextInput,
    settings_items::{self, Control, Item},
    settings_theme,
    settings_window::{FontTarget, SettingsTab},
};

/// A row whose control `build` draws, with `description` under the name.
/// `build` runs with the window, so it may use `cx.listener` as the
/// previous controls did.
pub(crate) fn field_item(
    this: &WeakEntity<SettingsWindow>,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    build: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) -> AnyElement
    + 'static,
) -> SettingItem {
    let this = this.clone();
    SettingItem::new(
        title,
        SettingField::render(move |_, window, cx| {
            this.update(cx, |this, cx| build(this, window, cx))
                .unwrap_or_else(|_| div().into_any_element())
        }),
    )
    .description(description.into())
}

/// An item that is drawn whole by `build` (a Kit row without its own name).
pub(crate) fn element_item(
    this: &WeakEntity<SettingsWindow>,
    build: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) -> AnyElement
    + 'static,
) -> SettingItem {
    let this = this.clone();
    SettingItem::render(move |_, window, cx| {
        this.update(cx, |this, cx| build(this, window, cx))
            .unwrap_or_else(|_| div().into_any_element())
    })
}

/// One entry of a font dropdown: a font name (empty for the default font) and
/// the text shown for it.
#[derive(Clone)]
pub(crate) struct FontChoice {
    pub(crate) name: SharedString,
    label: SharedString,
}

impl SelectItem for FontChoice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &SharedString {
        &self.name
    }
}

/// The Kit's searchable `Select` of the fonts (#622).
pub(crate) type FontSelect = Entity<SelectState<SearchableVec<FontChoice>>>;

/// The entries of a font dropdown: the default font first, the font held by
/// the form when the system does not list it (so its name is shown and
/// kept), then the system's fonts.
pub(crate) fn font_choices(
    default_label: String,
    held: &str,
    fonts: &[SharedString],
) -> SearchableVec<FontChoice> {
    // The name is kept as it is (it is the value saved); only the shown
    // label is made safe, since a settings file may hold bidirectional
    // controls that would reorder the text.
    let entry = |name: &SharedString| FontChoice {
        name: name.clone(),
        label: cayenchat_model::display::neutralize_bidi(name)
            .into_owned()
            .into(),
    };
    let mut choices = vec![FontChoice {
        name: SharedString::default(),
        label: default_label.into(),
    }];
    if !held.is_empty() && !fonts.iter().any(|font| font == held) {
        choices.push(entry(&SharedString::from(held.to_owned())));
    }
    choices.extend(fonts.iter().map(entry));
    SearchableVec::new(choices)
}

impl SettingsWindow {
    /// The dropdown of the font name of `target`, made when first drawn. The
    /// form's text field keeps the value (saving and Reset read it), so the
    /// dropdown follows the field and a pick is written to it.
    fn font_select(
        &mut self,
        target: FontTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FontSelect {
        let held = self.font_input(target).read(cx).text().trim().to_owned();
        let held_name = SharedString::from(held.clone());
        if self.fonts.is_empty() {
            // Listing the system's fonts takes a while (many seconds in a
            // debug build), so it waits until a list is drawn instead of
            // slowing every opening of the settings window.
            let mut fonts = window.text_system().all_font_names();
            fonts.sort_unstable();
            fonts.dedup();
            self.fonts = fonts.into_iter().map(SharedString::from).collect();
        }
        let default_label = self.i18n.text(if target == FontTarget::Time {
            "default_monospace"
        } else {
            "system_default"
        });
        let choices = font_choices(default_label.clone(), &held, &self.fonts);
        if let Some((select, label)) = self.font_selects[target as usize].clone() {
            // The field changed (Reset, the other theme's fonts) or the
            // language did (the default entry's label): show it.
            if label != default_label || select.read(cx).selected_value() != Some(&held_name) {
                select.update(cx, |select, cx| {
                    select.set_items(choices, window, cx);
                    select.set_selected_value(&held_name, window, cx);
                });
                self.font_selects[target as usize] = Some((select.clone(), default_label));
            }
            return select;
        }
        let at = choices.position(&held_name);
        let select = cx.new(|cx| SelectState::new(choices, at, window, cx).searchable(true));
        cx.subscribe_in(
            &select,
            window,
            move |this, _, event: &SelectEvent<SearchableVec<FontChoice>>, _, cx| {
                let SelectEvent::Confirm(name) = event;
                let name = name.clone().unwrap_or_default();
                this.font_input(target)
                    .update(cx, |field, cx| field.set_text(&name, cx));
                cx.notify();
            },
        )
        .detach();
        self.font_selects[target as usize] = Some((select.clone(), default_label));
        select
    }
}

/// The item at the end of a page: the status message and the reset button
/// of `tab`.
pub(crate) fn footer_item(this: &WeakEntity<SettingsWindow>, tab: SettingsTab) -> SettingItem {
    element_item(this, move |this, _, cx| {
        let theme = settings_theme::palette(cx);
        div()
            .flex()
            .flex_col()
            .when_some(this.status_message(), |d, feedback| {
                d.child(div().text_color(theme.warning).child(feedback))
            })
            .child(this.reset_footer(tab, cx))
            .into_any_element()
    })
}

/// A group of rows under a heading.
pub(crate) fn group(
    title: String,
    description: Option<String>,
    items: Vec<SettingItem>,
) -> SettingGroup {
    let group = SettingGroup::new().title(title).items(items);
    match description {
        Some(description) => group.description(description),
        None => group,
    }
}

impl SettingsWindow {
    /// The Kit's `Settings`: a page for each tab.
    pub(crate) fn kit_settings(&mut self, cx: &mut Context<Self>) -> Settings {
        let this = cx.entity().downgrade();
        let tabs = Self::settings_tabs();
        let selected = tabs
            .iter()
            .position(|(tab, ..)| *tab == self.tab)
            .unwrap_or(0);
        let pages: Vec<SettingPage> = tabs
            .iter()
            .map(|&(tab, _, label_key)| {
                let title = self.i18n.text(label_key);
                let page = match tab {
                    SettingsTab::Appearance => self.appearance_page(title, &this, cx),
                    SettingsTab::Notifications => self.notifications_page(title, &this),
                    SettingsTab::Application => self.application_page(title, &this, cx),
                    SettingsTab::Experimental => self.experimental_page(title, &this),
                    SettingsTab::Keyboard => self.keyboard_page(title, &this),
                    SettingsTab::Credentials => self.credentials_page(title, &this),
                    SettingsTab::ImageUpload => self.image_upload_page(title, &this),
                    _ => self.panel_page(tab, title, &this),
                };
                // The page being drawn is the tab being shown.
                let follow = this.clone();
                page.title_suffix(move |_, cx| {
                    let _ = follow.update(cx, |this, cx| this.follow_kit_page(tab, cx));
                    div()
                })
            })
            .collect();
        Settings::new(SharedString::from(format!(
            "settings-pages-{}",
            self.kit_generation
        )))
        .sidebar_width(px(190.))
        .sidebar_size_range(px(140.)..px(300.))
        .default_selected_index(SelectIndex {
            page_ix: selected,
            group_ix: None,
        })
        .pages(pages)
    }

    /// The Notifications tab: when a desktop notification is shown, then the
    /// keywords that make a message count as one.
    fn notifications_page(&self, title: String, this: &WeakEntity<Self>) -> SettingPage {
        let i18n = &self.i18n;
        let platform_hint = if cfg!(target_os = "macos") {
            "notifications_hint_macos"
        } else {
            "notifications_hint"
        };
        SettingPage::new(title).groups([
            group(
                i18n.text("group_notify_desktop"),
                Some(i18n.text(platform_hint)),
                self.kit_table(settings_items::NOTIFICATIONS_DESKTOP, this),
            ),
            group(
                i18n.text("group_notify_keywords"),
                None,
                self.kit_table(settings_items::NOTIFICATIONS_KEYWORDS, this),
            ),
            SettingGroup::new().item(footer_item(this, SettingsTab::Notifications)),
        ])
    }

    /// A tab not rebuilt yet: its panel as one item of one group.
    fn panel_page(
        &mut self,
        tab: SettingsTab,
        title: String,
        this: &WeakEntity<Self>,
    ) -> SettingPage {
        SettingPage::new(title).group(
            SettingGroup::new().item(element_item(this, move |this, _, cx| {
                this.render_tab_panel(tab, cx)
            })),
        )
    }

    /// The panel the tab drew before the Kit's pages.
    fn render_tab_panel(&mut self, tab: SettingsTab, cx: &mut Context<Self>) -> AnyElement {
        match tab {
            SettingsTab::Connection => self.render_connection_settings(cx).into_any_element(),
            // Drawn as groups by `application_page`.
            SettingsTab::Application => div().into_any_element(),
            // Drawn as groups by `appearance_page`.
            SettingsTab::Appearance => div().into_any_element(),
            // Drawn as groups by `keyboard_page`.
            SettingsTab::Keyboard => div().into_any_element(),
            // Drawn as groups by `notifications_page`.
            SettingsTab::Notifications => div().into_any_element(),
            SettingsTab::Ircv3 => self.render_ircv3_settings(cx).into_any_element(),
            // Drawn as groups by `image_upload_page`.
            SettingsTab::ImageUpload => div().into_any_element(),
            // Drawn as groups by `credentials_page`.
            SettingsTab::Credentials => div().into_any_element(),
            // Drawn as groups by `experimental_page`.
            SettingsTab::Experimental => div().into_any_element(),
        }
    }

    /// Kit items for a table of declared items: the setters change the form
    /// as the previous controls did.
    pub(crate) fn kit_table(
        &self,
        table: &'static [Item],
        this: &WeakEntity<Self>,
    ) -> Vec<SettingItem> {
        let on_toggle = {
            let this = this.clone();
            Rc::new(move |item: &'static Item, on: bool, cx: &mut App| {
                if let Control::Toggle { set, .. } = &item.control {
                    let _ = this.update(cx, |this, cx| {
                        set(&mut this.settings.values, on);
                        cx.notify();
                    });
                }
            })
        };
        let on_choose = {
            let this = this.clone();
            Rc::new(move |item: &'static Item, index: usize, cx: &mut App| {
                if let Control::Choice(options) | Control::Select(options) = &item.control {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(option) = options.get(index) {
                            (option.select)(&mut this.settings.values);
                            this.feedback = None;
                            cx.notify();
                        }
                    });
                }
            })
        };
        let slider = {
            let this = this.clone();
            Rc::new(move |item: &'static Item, cx: &mut App| {
                this.update(cx, |this, cx| this.slider_control(item, cx))
                    .unwrap_or_else(|_| div().into_any_element())
            })
        };
        settings_items::kit_items(
            table,
            &self.settings.values,
            &self.i18n,
            |id| self.settings.item_field(id).cloned(),
            on_toggle,
            on_choose,
            slider,
        )
    }

    /// One color row: the light and dark values side by side, each a swatch
    /// that opens the picker and a `#RRGGBB` field. The picker for either
    /// opens in an item under the row.
    fn color_items(
        &self,
        this: &WeakEntity<Self>,
        label_key: &str,
        hint_key: &str,
        light: &Entity<TextInput>,
        dark: &Entity<TextInput>,
    ) -> Vec<SettingItem> {
        let (light, dark) = (light.clone(), dark.clone());
        let picker_here = self
            .color_picker
            .as_ref()
            .is_some_and(|open| open.field == light || open.field == dark);
        let mut items = vec![field_item(
            this,
            self.i18n.text(label_key),
            self.i18n.text(hint_key),
            move |this, _, cx| {
                div()
                    .flex()
                    .gap_4()
                    .child(this.color_input(&light, cx))
                    .child(this.color_input(&dark, cx))
                    .into_any_element()
            },
        )];
        if picker_here {
            items.push(element_item(this, |this, _, cx| {
                this.render_color_panel(cx).into_any_element()
            }));
        }
        items
    }

    /// The Light/Dark headings above the color rows of a group, lined up with
    /// the cells of `color_input`.
    fn color_heading_item(&self) -> SettingItem {
        let (light, dark) = (
            self.i18n.text("colors_light"),
            self.i18n.text("colors_dark"),
        );
        SettingItem::render(move |options, _, _| {
            let cell = |text: &String| {
                div()
                    .w(px(crate::settings_window::COLOR_CELL_WIDTH))
                    .flex_shrink_0()
                    .child(text.clone())
            };
            div()
                .flex()
                .gap_4()
                .when(options.layout() == Axis::Horizontal, |d| d.justify_end())
                .child(cell(&light))
                .child(cell(&dark))
        })
    }

    /// The font of `target`: its name field with the list of the system's
    /// fonts, and its size.
    fn font_items(
        &self,
        this: &WeakEntity<Self>,
        target: FontTarget,
        cx: &App,
    ) -> Vec<SettingItem> {
        let (name_key, hint_key) = match target {
            FontTarget::ChannelLog => ("channel_log", "font_hint_channel_log"),
            FontTarget::CombinedLog => ("combined_log", "font_hint_combined_log"),
            FontTarget::MemberList => ("member_list", "font_hint_member_list"),
            FontTarget::ChannelTree => ("channel_tree", "font_hint_channel_tree"),
            FontTarget::Input => ("draft_input", "font_hint_input"),
            FontTarget::Time => ("timestamp_monospace", "font_hint_time"),
        };
        let name = self.i18n.text(name_key);
        let size = f64::from(self.settings.font_size(target, cx));
        let setter = this.clone();
        vec![
            field_item(
                this,
                self.i18n.format("font_family_label", &[("name", &name)]),
                self.i18n.text(hint_key),
                move |this, window, cx| {
                    let select = this.font_select(target, window, cx);
                    Select::new(&select)
                        .w(px(crate::settings_window::FONT_FIELD_WIDTH))
                        .menu_width(px(crate::settings_window::FONT_FIELD_WIDTH + 80.))
                        .into_any_element()
                },
            ),
            SettingItem::new(
                self.i18n.format("font_size_label", &[("name", &name)]),
                SettingField::number_input(
                    NumberFieldOptions {
                        min: f64::from(MIN_FONT_SIZE),
                        max: f64::from(MAX_FONT_SIZE),
                        step: 1.,
                    },
                    move |_| size,
                    move |value, cx| {
                        let _ = setter.update(cx, |this, cx| {
                            this.settings.set_font_size(target, value as u16, cx);
                            cx.notify();
                        });
                    },
                ),
            )
            .description(self.i18n.text("font_size_hint")),
        ]
    }

    /// The Appearance tab: groups of rows, each with a one-line description.
    fn appearance_page(
        &mut self,
        title: String,
        this: &WeakEntity<Self>,
        cx: &mut Context<Self>,
    ) -> SettingPage {
        let s = &self.settings;
        let mut pane_colors = vec![self.color_heading_item()];
        for (label, hint, light, dark) in [
            (
                "member_list_background",
                "member_list_background_hint",
                &s.member_list_background,
                &s.dark_member_list_background,
            ),
            (
                "channel_tree_background",
                "channel_tree_background_hint",
                &s.channel_tree_background,
                &s.dark_channel_tree_background,
            ),
            (
                "channel_log",
                "channel_log_hint",
                &s.channel_log_background,
                &s.dark_channel_log_background,
            ),
            (
                "channel_log_alternate",
                "channel_log_alternate_hint",
                &s.channel_log_alternate,
                &s.dark_channel_log_alternate,
            ),
            (
                "combined_log",
                "combined_log_hint",
                &s.combined_log_background,
                &s.dark_combined_log_background,
            ),
            (
                "combined_log_alternate",
                "combined_log_alternate_hint",
                &s.combined_log_alternate,
                &s.dark_combined_log_alternate,
            ),
        ] {
            pane_colors.extend(self.color_items(this, label, hint, light, dark));
        }
        let mut text_colors = vec![self.color_heading_item()];
        for (label, hint, light, dark) in [
            (
                "channel_event_color",
                "channel_event_color_hint",
                &s.channel_event_color,
                &s.dark_channel_event_color,
            ),
            (
                "notice_color",
                "notice_color_hint",
                &s.notice_color,
                &s.dark_notice_color,
            ),
            (
                "highlight_color",
                "highlight_color_hint",
                &s.highlight_color,
                &s.dark_highlight_color,
            ),
            (
                "link_color",
                "link_color_hint",
                &s.link_color,
                &s.dark_link_color,
            ),
            (
                "link_background",
                "link_background_hint",
                &s.link_background,
                &s.dark_link_background,
            ),
        ] {
            text_colors.extend(self.color_items(this, label, hint, light, dark));
        }
        let mut popup_colors = vec![self.color_heading_item()];
        for (label, hint, light, dark) in [
            (
                "url_tooltip_color",
                "url_tooltip_color_hint",
                &s.url_tooltip_color,
                &s.dark_url_tooltip_color,
            ),
            (
                "url_tooltip_link_color",
                "url_tooltip_link_color_hint",
                &s.url_tooltip_link_color,
                &s.dark_url_tooltip_link_color,
            ),
            (
                "url_tooltip_text_color",
                "url_tooltip_text_color_hint",
                &s.url_tooltip_text_color,
                &s.dark_url_tooltip_text_color,
            ),
        ] {
            popup_colors.extend(self.color_items(this, label, hint, light, dark));
        }

        let mut theme_rows = self.kit_table(settings_items::APPEARANCE_THEME, this);
        theme_rows.extend(self.theme_items(this, cx));

        let mut transparency = Vec::new();
        for table in [
            settings_items::OPACITY_MEMBER_LIST,
            settings_items::OPACITY_CHANNEL_TREE,
            settings_items::OPACITY_CHANNEL_LOG,
            settings_items::OPACITY_COMBINED_LOG,
            settings_items::OPACITY_URL_TOOLTIP,
        ] {
            transparency.extend(self.kit_table(table, this));
        }
        transparency.push(self.background_blur_item(this));
        if self.blur_supported && self.blur_radius_supported {
            transparency.extend(self.kit_table(settings_items::BLUR_RADIUS, this));
        }

        let mut fonts = vec![self.split_fonts_item(this)];
        if self.settings.values.appearance.split_fonts {
            fonts.push(self.font_side_item(this));
        }
        for target in [
            FontTarget::ChannelLog,
            FontTarget::CombinedLog,
            FontTarget::MemberList,
            FontTarget::ChannelTree,
            FontTarget::Input,
            FontTarget::Time,
        ] {
            fonts.extend(self.font_items(this, target, cx));
        }
        fonts.extend(self.kit_table(settings_items::LINE_HEIGHT_CHANNEL_LOG, this));
        fonts.extend(self.kit_table(settings_items::LINE_HEIGHT_COMBINED_LOG, this));

        let display = self.kit_table(settings_items::APPEARANCE_ROWS, this);
        let footer = footer_item(this, SettingsTab::Appearance);
        let i18n = &self.i18n;
        SettingPage::new(title).groups([
            group(
                i18n.text("theme_heading"),
                Some(i18n.text("theme_hint")),
                theme_rows,
            ),
            group(
                i18n.text("group_pane_colors"),
                Some(i18n.text("colors_note")),
                pane_colors,
            ),
            group(i18n.text("group_text_colors"), None, text_colors),
            group(i18n.text("group_popup_colors"), None, popup_colors),
            group(i18n.text("group_transparency"), None, transparency),
            group(
                i18n.text("group_fonts"),
                Some(i18n.text("fonts_note")),
                fonts,
            ),
            group(
                i18n.text("wallpaper_heading"),
                Some(i18n.text("wallpaper_hint")),
                self.wallpaper_items(this, cx),
            ),
            group(i18n.text("group_display"), None, display),
            SettingGroup::new().item(footer),
        ])
    }

    /// The window-wide background blur switch (#1018). It only does
    /// something where the platform blurs behind the window, so elsewhere it
    /// is disabled and the description gives the reason; the stored value is
    /// kept.
    fn background_blur_item(&self, this: &WeakEntity<Self>) -> SettingItem {
        let supported = self.blur_supported;
        let hint = if supported && self.blur_radius_supported {
            "background_blur_hint_adjustable"
        } else if supported {
            "background_blur_hint"
        } else {
            "background_blur_unsupported"
        };
        let on = self.settings.values.appearance.background_blur;
        let this = this.clone();
        SettingItem::new(
            self.i18n.text("background_blur"),
            SettingField::switch(
                move |_| on,
                move |value, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.settings.values.appearance.background_blur = value;
                        cx.notify();
                    });
                },
            ),
        )
        .description(self.i18n.text(hint))
        .disabled(!supported)
    }

    /// The switch for separate light and dark fonts.
    fn split_fonts_item(&self, this: &WeakEntity<Self>) -> SettingItem {
        let on = self.settings.values.appearance.split_fonts;
        let this = this.clone();
        SettingItem::new(
            self.i18n.text("split_fonts"),
            SettingField::switch(
                move |_| on,
                move |value, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.settings.set_split_fonts(value, cx);
                        cx.notify();
                    });
                },
            ),
        )
        .description(self.i18n.text("split_fonts_hint"))
    }

    /// While the fonts are split: the theme whose fonts the rows below show.
    fn font_side_item(&self, this: &WeakEntity<Self>) -> SettingItem {
        let dark = self.settings.font_side_dark;
        let labels = [false, true]
            .map(|side| {
                (
                    SharedString::from(u8::from(side).to_string()),
                    SharedString::from(self.i18n.text(if side {
                        "theme_kind_dark"
                    } else {
                        "theme_kind_light"
                    })),
                )
            })
            .to_vec();
        let this = this.clone();
        SettingItem::new(
            self.i18n.text("wallpaper_editing"),
            SettingField::dropdown(
                labels,
                move |_| u8::from(dark).to_string().into(),
                move |value, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.settings.select_font_side(value == "1", cx);
                        cx.notify();
                    });
                },
            ),
        )
        .description(self.i18n.text("font_editing_hint"))
    }
}
