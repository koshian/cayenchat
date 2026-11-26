// SPDX-License-Identifier: GPL-3.0-only
//! The Connection tab as Kit groups (#622): the server, how to sign in, who
//! you are, the messages, the character encoding, what happens on connect
//! and the avatars. Each row has a name and a one-sentence description; the
//! switches and dropdowns are the Kit's. What the Kit has no part for (the
//! password and key controls, the auto-join summary, the
//! avatar section and the Back/Save/Connect buttons) sits in a Kit row
//! through `SettingField::render` / `SettingItem::render`. Saving,
//! validation and the secrets stay with the form, as before.

use gpui::{Axis, EntityInputHandler};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_component::{
    Sizable,
    input::{Input, InputEvent, InputState},
};

use super::*;
use crate::settings_kit::{element_item, field_item, group, page};

/// Value of the dropdown entry that adds a server.
const ADD_SERVER: &str = "\u{1}add";

impl SettingsWindow {
    /// The Connection tab: the shown server's settings in groups.
    pub(crate) fn connection_page(
        &mut self,
        title: String,
        this: &WeakEntity<Self>,
        cx: &mut Context<Self>,
    ) -> SettingPage {
        let connected = self.selected_connected(cx);
        self.connected_shown = connected;
        let profile = self.settings.values.selected_profile().cloned();
        // A server not in the saved list is a draft being added: only Save
        // registers it, and only a registered server can be connected. A
        // registered server's changes are saved as they are made (#634).
        let adding = profile
            .as_ref()
            .is_some_and(|profile| self.saved.profile(&profile.id).is_none());
        let intro = self.i18n.text(if adding {
            "adding_server_intro"
        } else {
            "connection_intro"
        });
        let mut groups = vec![group(
            self.i18n.text("group_conn_server"),
            Some(intro),
            self.server_items(profile.as_ref(), adding, this, cx),
        )];
        if let Some(profile) = &profile {
            groups.extend([
                group(
                    self.i18n.text("group_conn_auth"),
                    None,
                    self.auth_items(profile, this),
                ),
                group(
                    self.i18n.text("group_conn_identity"),
                    None,
                    self.identity_items(profile, this),
                ),
                group(
                    self.i18n.text("group_conn_messages"),
                    None,
                    vec![
                        self.text_item(this, "quit_message", |form| &form.quit_message),
                        self.text_item(this, "sleep_quit_message", |form| &form.sleep_quit_message),
                        self.text_item(this, "part_message", |form| &form.part_message),
                    ],
                ),
                group(
                    self.i18n.text("group_conn_encoding"),
                    None,
                    self.encoding_items(profile, this),
                ),
                group(
                    self.i18n.text("group_conn_behavior"),
                    None,
                    self.behavior_items(profile, this, cx),
                ),
                group(
                    self.i18n.text("group_conn_avatars"),
                    None,
                    self.avatar_items(profile, this),
                ),
            ]);
        }
        groups.push(SettingGroup::new().item(self.connection_footer(adding, connected, this)));
        page(title, groups, cx)
    }

    /// A row with the Kit's input for a text field of the form, with the Kit's
    /// layout (beside the name, below it in a narrow window). The form's field keeps the text (saving and Reset read
    /// it): the input shows it and a change is written to it.
    ///
    /// This is the Kit's `Input` with the sync of `SettingField::input` done
    /// here, for two reasons. The Kit keeps its getter and setter from the
    /// first draw on, while a reload of the settings file replaces the form's
    /// fields: `field` is looked up in the window's current form each time.
    /// And the Kit resets the input to the form's text on every draw, which
    /// ends an IME composition (marked text raises no change event, so the
    /// form still holds the old text): a composing input is left alone.
    fn text_item(
        &self,
        this: &WeakEntity<Self>,
        key: &str,
        field: fn(&SettingsForm) -> &Entity<TextInput>,
    ) -> SettingItem {
        let title = self.i18n.text(key);
        let aria = title.clone();
        let window_entity = this.clone();
        let id = SharedString::from(format!("kit-text-{key}"));
        SettingItem::new(
            title,
            SettingField::render(move |options, window, cx| {
                let shown = window_entity
                    .read_with(cx, |this, cx| {
                        field(&this.settings).read(cx).text().to_owned()
                    })
                    .unwrap_or_default();
                let state = window.use_keyed_state(id.clone(), cx, {
                    let (shown, target) = (shown.clone(), window_entity.clone());
                    move |window, cx| {
                        let input = cx.new(|cx| InputState::new(window, cx).default_value(shown));
                        let _subscription =
                            cx.subscribe(&input, move |_, input, event: &InputEvent, cx| {
                                if !matches!(event, InputEvent::Change) {
                                    return;
                                }
                                let value = input.read(cx).value();
                                let _ = target.update(cx, |this, cx| {
                                    let field = field(&this.settings).clone();
                                    if field.read(cx).text() == value.as_ref() {
                                        return;
                                    }
                                    field.update(cx, |field, cx| field.set_text(&value, cx));
                                    this.watch_kit_input(cx);
                                });
                            });
                        KitTextState {
                            input,
                            _subscription,
                        }
                    }
                });
                let input = state.read(cx).input.clone();
                input.update(cx, |input, cx| {
                    if input.value() != shown && input.marked_text_range(window, cx).is_none() {
                        input.set_value(shown, window, cx);
                    }
                });
                let input = Input::new(&input)
                    .aria_label(aria.clone())
                    .disabled(options.is_disabled())
                    .with_size(options.size());
                // One width for every text field of a row (a Kit row would squeeze a
                // wider one when its description is long).
                if options.layout() == Axis::Horizontal {
                    input
                        .w(px(crate::settings_items::TEXT_FIELD_WIDTH))
                        .into_any_element()
                } else {
                    input.w_full().into_any_element()
                }
            }),
        )
        .description(self.i18n.text(&format!("{key}_desc")))
    }

    /// A line of text under a row, in the warning color or not.
    pub(crate) fn note_item(
        &self,
        this: &WeakEntity<Self>,
        key: &str,
        warning: bool,
    ) -> SettingItem {
        self.note_text_item(this, self.i18n.text(key), warning)
    }

    /// A line of already localized text under a row.
    pub(crate) fn note_text_item(
        &self,
        this: &WeakEntity<Self>,
        text: String,
        warning: bool,
    ) -> SettingItem {
        element_item(this, move |_, _, cx| {
            let theme = settings_theme::palette(cx);
            div()
                .text_color(if warning {
                    theme.warning
                } else {
                    theme.text_secondary
                })
                .child(text.clone())
                .into_any_element()
        })
    }

    /// A Kit switch that sets one field of the shown server.
    fn profile_switch(
        &self,
        this: &WeakEntity<Self>,
        key: &str,
        on: bool,
        set: fn(&mut ServerProfile, bool),
    ) -> SettingItem {
        let target = this.clone();
        SettingItem::new(
            self.i18n.text(key),
            SettingField::switch(
                move |_| on,
                move |value, cx| {
                    let _ = target.update(cx, |this, cx| {
                        if let Some(profile) = this.settings.values.selected_profile_mut() {
                            set(profile, value);
                        }
                        cx.notify();
                    });
                },
            ),
        )
        .description(self.i18n.text(&format!("{key}_desc")))
    }

    /// A Kit switch whose change is made by `toggle` (a flip that may refuse).
    fn toggle_switch(
        &self,
        this: &WeakEntity<Self>,
        key: &str,
        on: bool,
        toggle: fn(&mut Self, &mut Context<Self>),
    ) -> SettingItem {
        let target = this.clone();
        SettingItem::new(
            self.i18n.text(key),
            SettingField::switch(
                move |_| on,
                move |_, cx| {
                    let _ = target.update(cx, toggle);
                },
            ),
        )
        .description(self.i18n.text(&format!("{key}_desc")))
    }

    /// A Kit dropdown over `labels`; `pick` gets the index chosen.
    fn index_dropdown(
        &self,
        this: &WeakEntity<Self>,
        key: &str,
        labels: Vec<String>,
        current: usize,
        pick: impl Fn(&mut Self, usize, &mut Context<Self>) + 'static,
    ) -> SettingItem {
        let target = this.clone();
        let options = labels
            .into_iter()
            .enumerate()
            .map(|(index, label)| {
                (
                    SharedString::from(index.to_string()),
                    SharedString::from(label),
                )
            })
            .collect::<Vec<_>>();
        SettingItem::new(
            self.i18n.text(key),
            SettingField::dropdown(
                options,
                move |_| current.to_string().into(),
                move |value, cx| {
                    if let Ok(index) = value.parse::<usize>() {
                        let _ = target.update(cx, |this, cx| pick(this, index, cx));
                    }
                },
            ),
        )
        .description(self.i18n.text(&format!("{key}_desc")))
    }

    /// The server to edit, and its address.
    fn server_items(
        &self,
        profile: Option<&ServerProfile>,
        adding: bool,
        this: &WeakEntity<Self>,
        cx: &App,
    ) -> Vec<SettingItem> {
        let i18n = &self.i18n;
        let current_host = self.settings.custom_host.read(cx).text().trim().to_owned();
        let (options, current): (Vec<(SharedString, SharedString)>, SharedString) = if adding {
            // Adding offers only suggestions; existing servers stay out of
            // the way so the list cannot be mistaken for editing one.
            let options = cayenchat_storage::PRESETS
                .iter()
                .map(|preset| {
                    (
                        SharedString::from(preset.host),
                        SharedString::from(i18n.format(
                            "preset_option",
                            &[("name", preset.name), ("host", preset.host)],
                        )),
                    )
                })
                .collect();
            let known = cayenchat_storage::PRESETS
                .iter()
                .any(|preset| preset.host == current_host);
            (
                options,
                if known {
                    current_host.into()
                } else {
                    SharedString::default()
                },
            )
        } else {
            let mut options: Vec<_> = self
                .saved
                .ordered_servers()
                .map(|server| {
                    (
                        SharedString::from(server.id.clone()),
                        SharedString::from(
                            display::neutralize_bidi(&ircv3_settings::server_label(server, i18n))
                                .into_owned(),
                        ),
                    )
                })
                .collect();
            options.push((ADD_SERVER.into(), i18n.text("add_server").into()));
            (
                options,
                profile
                    .map(|profile| SharedString::from(profile.id.clone()))
                    .unwrap_or_default(),
            )
        };
        let target = this.clone();
        let mut items = vec![
            SettingItem::new(
                i18n.text("destination"),
                SettingField::scrollable_dropdown(
                    options,
                    move |_| current.clone(),
                    move |value, cx| {
                        let _ = target.update(cx, |this, cx| {
                            if adding {
                                this.choose_preset(&value, cx);
                            } else if value.as_ref() == ADD_SERVER {
                                this.add_server(cayenchat_storage::PRESETS[0].host, cx);
                            } else {
                                this.select_server(value.to_string(), cx);
                            }
                        });
                    },
                ),
            )
            .description(i18n.text("destination_desc")),
        ];
        let Some(profile) = profile else {
            items.push(self.note_item(this, "no_servers_hint", false));
            return items;
        };
        items.push(self.text_item(this, "display_name", |form| &form.display_name));
        items.push(self.text_item(this, "host", |form| &form.custom_host));
        items.push(self.text_item(this, "port", |form| &form.port));
        items.push(self.toggle_switch(this, "tls", profile.use_tls, Self::toggle_tls));
        if profile.use_tls {
            items.push(self.toggle_switch(
                this,
                "verify_certificates",
                profile.verify_tls_certificates,
                Self::toggle_certificate_verification,
            ));
            if !profile.verify_tls_certificates {
                items.push(self.note_item(this, "certificate_warning", true));
            }
        }
        let registered = self.saved.profile(&profile.id).is_some();
        items.push(field_item(
            this,
            i18n.text(if registered {
                "remove_server"
            } else {
                "cancel_add_server"
            }),
            i18n.text(if registered {
                "remove_server_desc"
            } else {
                "cancel_add_server_desc"
            }),
            move |this, _, cx| {
                settings_theme::button("remove-server", false, cx)
                    .text_color(settings_theme::palette(cx).warning)
                    .child(this.i18n.text(if registered {
                        "remove_server_button"
                    } else {
                        "cancel"
                    }))
                    .on_click(cx.listener(|this, _, window, cx| this.remove_server(window, cx)))
                    .into_any_element()
            },
        ));
        items
    }

    /// The server password and SASL.
    fn auth_items(&self, profile: &ServerProfile, this: &WeakEntity<Self>) -> Vec<SettingItem> {
        let i18n = &self.i18n;
        let mut items = vec![self.credential_item(this, Credential::Server, "server_password")];
        if !profile.use_tls {
            let target = this.clone();
            let window = self.window;
            items.push(
                SettingItem::new(
                    i18n.text("plaintext_pass"),
                    SettingField::switch(
                        {
                            let on = profile.allow_plaintext_pass;
                            move |_| on
                        },
                        move |_, cx| {
                            // Turning it on asks first, which needs the window.
                            let target = target.clone();
                            cx.defer(move |cx| {
                                let _ = window.update(cx, |_, window, cx| {
                                    let _ = target.update(cx, |this, cx| {
                                        this.toggle_plaintext_pass(window, cx)
                                    });
                                });
                            });
                        },
                    ),
                )
                .description(i18n.text("plaintext_pass_desc")),
            );
            if profile.allow_plaintext_pass {
                items.push(self.note_item(this, "plaintext_pass_warning", true));
            }
        }
        items.push(self.toggle_switch(this, "sasl", profile.sasl_enabled, Self::toggle_sasl));
        let mechanisms = SaslMechanism::ALL;
        items.push(
            self.index_dropdown(
                this,
                "sasl_mechanism",
                mechanisms
                    .iter()
                    .map(|mechanism| mechanism.name().to_owned())
                    .collect(),
                mechanisms
                    .iter()
                    .position(|mechanism| *mechanism == profile.sasl_mechanism)
                    .unwrap_or(0),
                move |this, index, cx| {
                    if let Some(mechanism) = mechanisms.get(index) {
                        this.choose_sasl_mechanism(*mechanism, cx);
                    }
                },
            ),
        );
        if profile.sasl_enabled {
            items.push(self.note_item(this, "sasl_tls_note", false));
            items.push(self.text_item(this, "sasl_account", |form| &form.sasl_username));
            if profile.sasl_mechanism.uses_key() {
                items.extend(self.sasl_key_items(profile.sasl_key_saved, this));
            } else {
                items.push(self.credential_item(this, Credential::Sasl, "sasl_password"));
            }
        }
        items
    }

    /// The nickname, the nicknames to fall back on, the USER and real name.
    fn identity_items(&self, profile: &ServerProfile, this: &WeakEntity<Self>) -> Vec<SettingItem> {
        let collisions = NickCollision::ALL;
        vec![
            self.text_item(this, "nickname", |form| &form.nickname),
            self.index_dropdown(
                this,
                "nick_collision",
                collisions
                    .iter()
                    .map(|choice| self.i18n.text(nick_collision_key(*choice)))
                    .collect(),
                collisions
                    .iter()
                    .position(|choice| *choice == profile.nick_collision)
                    .unwrap_or(0),
                move |this, index, cx| {
                    if let (Some(choice), Some(profile)) = (
                        collisions.get(index),
                        this.settings.values.selected_profile_mut(),
                    ) {
                        profile.nick_collision = *choice;
                    }
                    cx.notify();
                },
            ),
            self.text_item(this, "alt_nicks", |form| &form.alt_nicks),
            self.note_item(this, "nick_collision_hint", false),
            self.text_item(this, "username", |form| &form.username),
            self.text_item(this, "realname", |form| &form.realname),
        ]
    }

    /// The server's encoding, and the character references of ISO-2022-JP.
    fn encoding_items(&self, profile: &ServerProfile, this: &WeakEntity<Self>) -> Vec<SettingItem> {
        let encodings = TextEncoding::ALL;
        vec![
            self.index_dropdown(
                this,
                "encoding",
                encodings
                    .iter()
                    .map(|encoding| encoding.label().to_owned())
                    .collect(),
                encodings
                    .iter()
                    .position(|encoding| *encoding == profile.encoding)
                    .unwrap_or(0),
                move |this, index, cx| {
                    if let (Some(encoding), Some(profile)) = (
                        encodings.get(index),
                        this.settings.values.selected_profile_mut(),
                    ) {
                        profile.encoding = *encoding;
                    }
                    cx.notify();
                },
            ),
            self.note_item(this, "encoding_hint", false),
            self.index_dropdown(
                this,
                "iso2022jp_char_refs",
                CHAR_REF_CHOICES
                    .iter()
                    .map(|(_, key)| self.i18n.text(key))
                    .collect(),
                CHAR_REF_CHOICES
                    .iter()
                    .position(|(mode, _)| *mode == profile.char_refs)
                    .unwrap_or(0),
                |this, index, cx| {
                    if let (Some((mode, _)), Some(profile)) = (
                        CHAR_REF_CHOICES.get(index),
                        this.settings.values.selected_profile_mut(),
                    ) {
                        profile.char_refs = *mode;
                    }
                    cx.notify();
                },
            ),
            self.note_item(this, "iso2022jp_char_refs_hint", false),
        ]
    }

    /// What happens when the app starts and when the server is joined.
    fn behavior_items(
        &self,
        profile: &ServerProfile,
        this: &WeakEntity<Self>,
        cx: &App,
    ) -> Vec<SettingItem> {
        let summary = self.auto_join_summary(cx);
        vec![
            field_item(
                this,
                self.i18n.text("auto_join_channels"),
                self.i18n.text("auto_join_channels_desc"),
                move |this, _, cx| {
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(div().id("auto-join-summary").child(summary.clone()))
                        .child(
                            settings_theme::button("auto-join-edit", false, cx)
                                .child(this.i18n.text("auto_join_edit"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_auto_join(window, cx)
                                })),
                        )
                        .into_any_element()
                },
            ),
            self.profile_switch(
                this,
                "connect_on_startup",
                profile.connect_on_startup,
                |profile, on| profile.connect_on_startup = on,
            ),
            self.profile_switch(this, "regain_ops", profile.regain_ops, |profile, on| {
                profile.regain_ops = on
            }),
            self.note_item(this, "regain_ops_hint", false),
        ]
    }

    /// The server's avatars, the icon this client publishes and the style of
    /// the log.
    fn avatar_items(&self, profile: &ServerProfile, this: &WeakEntity<Self>) -> Vec<SettingItem> {
        let reiwa = self.settings.values.appearance.reiwa_mode;
        let target = this.clone();
        vec![
            self.profile_switch(this, "user_avatars", profile.user_avatars, |profile, on| {
                profile.user_avatars = on
            }),
            self.note_item(this, "user_avatars_hint", false),
            element_item(this, |this, _, cx| {
                let profile = this.settings.values.selected_profile().cloned();
                match profile {
                    Some(profile) => this.render_quick_avatar(&profile, cx).into_any_element(),
                    None => div().into_any_element(),
                }
            }),
            // App-wide, so outside what belongs to this server alone.
            SettingItem::new(
                self.i18n.text("reiwa_mode"),
                SettingField::switch(
                    move |_| reiwa,
                    move |value, cx| {
                        let _ = target.update(cx, |this, cx| {
                            this.settings.values.appearance.reiwa_mode = value;
                            cx.notify();
                        });
                    },
                ),
            )
            .description(self.i18n.text("reiwa_mode_global_desc")),
        ]
    }

    /// The status message and the Back, Save and Connect buttons.
    fn connection_footer(
        &self,
        adding: bool,
        connected: bool,
        this: &WeakEntity<Self>,
    ) -> SettingItem {
        let no_server = self.settings.values.selected_profile().is_none();
        element_item(this, move |this, _, cx| {
            let theme = settings_theme::palette(cx);
            div()
                .flex()
                .flex_col()
                .gap_2()
                .when_some(this.status_message(), |d, feedback| {
                    d.child(div().text_color(theme.warning).child(feedback))
                })
                .when(connected && !adding, |d| {
                    d.child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(this.i18n.text("ircv3_next_connection")),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            settings_theme::button("back-button", false, cx)
                                .debug_selector(|| "back-button".into())
                                .child(this.i18n.text("back"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_settings(window, cx);
                                })),
                        )
                        .child(div().flex_1())
                        .when(adding, |d| {
                            d.child(
                                settings_theme::button("save-button", false, cx)
                                    .debug_selector(|| "save-button".into())
                                    .child(this.i18n.text("save"))
                                    .on_click(cx.listener(|this, _, _, cx| this.save_servers(cx))),
                            )
                        })
                        .when(!no_server && !adding, |d| {
                            // One button in one place: Disconnect while the
                            // server being edited is connected, being
                            // connected or waiting to retry, Connect
                            // otherwise.
                            d.child(
                                settings_theme::button("connection-button", !connected, cx)
                                    .debug_selector(|| "connection-button".into())
                                    .child(this.i18n.text(if connected {
                                        "disconnect"
                                    } else {
                                        "connect"
                                    }))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if !connected {
                                            this.connect_from_settings(window, cx);
                                            return;
                                        }
                                        // Disconnects the server being edited.
                                        let profile = this.settings.values.selected_server.clone();
                                        let _ = this.owner.update(cx, |owner, _, cx| {
                                            if let Some(network) =
                                                owner.network_of_profile(&profile)
                                            {
                                                owner.disconnect(network, cx);
                                            }
                                        });
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .into_any_element()
        })
    }

    /// A password of the shown server as one row: a "save password" box
    /// beside the field of a draft server (its own Save writes it). Of a
    /// registered server: Save beside an empty field while none is saved;
    /// Change and Delete beside "Saved" once one is; Save and Cancel beside a
    /// field while it is being changed. A saved value is never shown.
    fn credential_item(
        &self,
        this: &WeakEntity<Self>,
        which: Credential,
        key: &'static str,
    ) -> SettingItem {
        field_item(
            this,
            self.i18n.text(key),
            self.i18n.text(&format!("{key}_desc")),
            move |this, _, cx| this.credential_controls(which, cx).into_any_element(),
        )
    }

    fn credential_controls(&self, which: Credential, cx: &mut Context<Self>) -> Div {
        let (saved, changing) = self.settings.credential_state(which);
        let slot = which as usize;
        let row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap_2();
        if saved && !changing {
            return row
                .child(self.i18n.text("credential_saved"))
                .child(
                    settings_theme::button(("credential-change", slot), false, cx)
                        .child(self.i18n.text("credential_change"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.change_credential(which, window, cx)
                        })),
                )
                .child(
                    settings_theme::button(("credential-delete", slot), false, cx)
                        .child(self.i18n.text("credential_delete"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.delete_credential(which, cx)),
                        ),
                );
        }
        let row = row.child(
            div()
                .w(px(crate::settings_items::TEXT_FIELD_WIDTH))
                .child(self.settings.credential_field(which).clone()),
        );
        if !changing && !self.shown_server_registered() {
            // A draft server: the box makes its own Save write the password.
            return row.child(
                div()
                    .id(("credential-remember", slot))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(settings_theme::checkbox(
                        self.settings.remember(which),
                        true,
                        cx,
                    ))
                    .child(self.i18n.text("credential_remember"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_remember(which, window, cx)
                    })),
            );
        }
        row.child(
            settings_theme::button(("credential-save", slot), true, cx)
                .child(self.i18n.text("credential_save"))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.save_credential(which, window, cx)),
                ),
        )
        .when(changing, |row| {
            row.child(
                settings_theme::button(("credential-cancel", slot), false, cx)
                    .child(self.i18n.text("cancel"))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.cancel_credential_change(which, cx)),
                    ),
            )
        })
    }

    /// The key management that takes the place of the password row for
    /// ECDSA-NIST256P-CHALLENGE: make, load, show and delete. The private key
    /// is never shown, copied or exported.
    fn sasl_key_items(&self, saved: bool, this: &WeakEntity<Self>) -> Vec<SettingItem> {
        let mut items = vec![field_item(
            this,
            self.i18n.text("sasl_key"),
            self.i18n.text("sasl_key_desc"),
            move |this, _, cx| {
                let button = |id: &'static str, label: &'static str, cx: &mut Context<Self>| {
                    settings_theme::button(id, false, cx).child(this.i18n.text(label))
                };
                let mut actions =
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(this.i18n.text(if saved {
                            "credential_saved"
                        } else {
                            "sasl_key_none"
                        }))
                        .child(
                            button("sasl-key-generate", "sasl_key_generate", cx).on_click(
                                cx.listener(|this, _, window, cx| this.generate_key(window, cx)),
                            ),
                        )
                        .child(button("sasl-key-import", "sasl_key_import", cx).on_click(
                            cx.listener(|this, _, window, cx| this.import_key(window, cx)),
                        ));
                if saved {
                    actions = actions
                        .child(
                            button("sasl-key-show", "sasl_key_show_public", cx)
                                .on_click(cx.listener(|this, _, _, cx| this.show_public_key(cx))),
                        )
                        .child(button("sasl-key-delete", "credential_delete", cx).on_click(
                            cx.listener(|this, _, window, cx| this.delete_key(window, cx)),
                        ));
                }
                actions.into_any_element()
            },
        )];
        if self.settings.sasl_public_key.is_some() {
            items.push(element_item(this, |this, _, cx| {
                let theme = settings_theme::palette(cx);
                let Some(public) = this.settings.sasl_public_key.clone() else {
                    return div().into_any_element();
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(this.i18n.text("sasl_key_public")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(div().min_w_0().child(public))
                            .child(
                                settings_theme::button("sasl-key-copy", false, cx)
                                    .child(this.i18n.text("sasl_key_copy"))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.copy_public_key(cx)),
                                    ),
                            ),
                    )
                    .into_any_element()
            }));
        }
        items.push(self.note_item(this, "sasl_key_note", false));
        items
    }
}

/// The text of a way to react when the nickname is in use.
fn nick_collision_key(choice: NickCollision) -> &'static str {
    match choice {
        NickCollision::Underscore => "nick_collision_underscore",
        NickCollision::Underscores => "nick_collision_underscores",
        NickCollision::Digits => "nick_collision_digits",
        NickCollision::Alternates => "nick_collision_alternates",
        NickCollision::Dialog => "nick_collision_dialog",
    }
}

/// The Kit input of a text row (`text_item`), kept across draws.
struct KitTextState {
    input: Entity<InputState>,
    _subscription: Subscription,
}
#[cfg(test)]
mod tests {
    use gpui::Entity;

    use super::{SettingsTab, SettingsWindow};

    /// Settings with one server shown on the Connection tab, with the
    /// production Tab bindings.
    fn connection_form(
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<SettingsWindow>, &mut gpui::VisualTestContext) {
        crate::support::install_test_globals(cx);
        cx.update(|cx| {
            crate::input::bind_keys(false, cx);
            cx.bind_keys(crate::shortcut_bindings(
                cayenchat_storage::ChannelNumberModifier::Ctrl,
                &Default::default(),
                cx,
            ));
        });
        let settings = crate::settings_with_channels("#a");
        let owner = cx.add_window(|window, cx| {
            crate::ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let (form, cx) = cx.add_window_view(|window, cx| {
            let mut form = SettingsWindow::new(owner, settings.clone(), window, cx);
            form.tab = SettingsTab::Connection;
            form
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (form, cx)
    }

    /// Draws frames until the Tab being followed has settled.
    fn settle(cx: &mut gpui::VisualTestContext) {
        for _ in 0..40 {
            // The test platform asks for no further frame; the app does.
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn tab_goes_on_into_the_groups_the_page_has_not_painted(cx: &mut gpui::TestAppContext) {
        let (form, cx) = connection_form(cx);
        form.update_in(cx, |form, window, cx| window.focus(&form.nav_focus, cx));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("connection-button").is_none(),
            "the last group is already painted"
        );
        let mut reached = false;
        for _ in 0..80 {
            cx.simulate_keystrokes("tab");
            settle(cx);
            if cx.debug_bounds("connection-button").is_some() {
                reached = true;
                break;
            }
        }
        assert!(reached, "Tab never reached the groups below");

        // Shift+Tab goes back up through the groups above.
        let mut back = false;
        for _ in 0..120 {
            cx.simulate_keystrokes("shift-tab");
            settle(cx);
            if cx.debug_bounds("connection-button").is_none() {
                back = true;
                break;
            }
        }
        assert!(back, "Shift+Tab never left the groups below");
    }
}
