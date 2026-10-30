// SPDX-License-Identifier: GPL-3.0-only
//! The settings of one channel (#680, #781): joining it automatically, its
//! encodings and its numeric character reference mode, in one dialog that
//! the chat window (channel menu) and the settings window (auto-join list)
//! both show. The dialog only edits a copy: it reports Save or Cancel, and
//! the host writes the values with [`apply_charsets`] and
//! [`apply_auto_join`]. Saving never joins or parts the channel.

use cayenchat_model::{NetworkId, display, names::CaseMapping};
use cayenchat_storage::{
    AutoJoinEntry, CharRefMode, ServerProfile, TextEncoding, format_auto_join,
};
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, FocusNextField, FocusPreviousField, localization::Localizer, settings_file,
    settings_theme,
};

/// Widest the panel gets; a smaller window keeps [`MARGIN`] around it.
const PANEL_WIDTH: Pixels = px(480.);
/// Space kept free around the panel.
const MARGIN: Pixels = px(16.);
/// Tallest the open encoding list gets before it scrolls.
const MENU_HEIGHT: Pixels = px(176.);

/// How the dialog ended.
pub(crate) enum ChannelSettingsEvent {
    Save,
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncodingSide {
    Decode,
    Encode,
}

/// What the dialog holds for a channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Choices {
    pub(crate) enabled: bool,
    pub(crate) decode: Option<TextEncoding>,
    pub(crate) encode: Option<TextEncoding>,
    pub(crate) char_refs: Option<CharRefMode>,
}

impl Choices {
    /// What `profile` holds for `channel` now.
    pub(crate) fn of(profile: &ServerProfile, channel: &str, mapping: CaseMapping) -> Self {
        let same = |left: &str, right: &str| mapping.same(left, right);
        let saved = profile.channel_charset(channel, same);
        Self {
            enabled: profile
                .auto_join_entries()
                .iter()
                .any(|entry| entry.enabled && same(&entry.name, channel)),
            decode: saved.and_then(|entry| entry.decode),
            encode: saved.and_then(|entry| entry.encode),
            char_refs: saved.and_then(|entry| entry.char_refs),
        }
    }
}

/// The choices of a numeric character reference mode, with the label key of
/// each. `inherit` adds the channel's "follow the server" choice.
pub(crate) fn char_ref_choices(inherit: bool) -> Vec<(Option<CharRefMode>, &'static str)> {
    let mut choices = Vec::new();
    if inherit {
        choices.push((None, "channel_settings_inherit"));
    }
    choices.extend([
        (Some(CharRefMode::Auto), "char_refs_auto"),
        (Some(CharRefMode::Iso2022Jp), "char_refs_iso2022jp"),
        (Some(CharRefMode::Off), "char_refs_off"),
    ]);
    choices
}

/// Whether a UTF8ONLY server accepts these encodings.
pub(crate) fn channel_encodings_allowed(
    utf8_only: bool,
    decode: Option<TextEncoding>,
    encode: Option<TextEncoding>,
) -> bool {
    !utf8_only
        || [decode, encode]
            .into_iter()
            .flatten()
            .all(|encoding| encoding == TextEncoding::Utf8)
}

/// Writes the encodings and the reference mode of `channel` into `profile`.
/// An error leaves the profile as it was.
pub(crate) fn apply_charsets(
    profile: &mut ServerProfile,
    channel: &str,
    mapping: CaseMapping,
    choices: Choices,
    utf8_only: bool,
    i18n: &Localizer,
) -> Result<(), String> {
    if !channel_encodings_allowed(utf8_only, choices.decode, choices.encode) {
        return Err(i18n.text("charset_utf8only"));
    }
    profile.set_channel_charset(
        channel,
        choices.decode,
        choices.encode,
        choices.char_refs,
        |left, right| mapping.same(left, right),
    )
}

/// Writes the auto-join choice of `channel` into the profile's channel list
/// and returns the list's text. A channel that was never listed is not added
/// just to be off.
pub(crate) fn apply_auto_join(
    profile: &mut ServerProfile,
    channel: &str,
    mapping: CaseMapping,
    enabled: bool,
) -> String {
    let mut entries = profile.auto_join_entries();
    let found = entries
        .iter_mut()
        .filter(|entry| mapping.same(&entry.name, channel))
        .map(|entry| entry.enabled = enabled)
        .count();
    if found == 0 && enabled {
        entries.push(AutoJoinEntry {
            name: channel.to_owned(),
            enabled: true,
        });
    }
    let text = format_auto_join(&entries);
    profile.channels = text.trim().to_owned();
    text
}

/// The dialog. Its controls, in Tab order.
const AUTO_JOIN: usize = 0;
const DECODE: usize = 1;
const ENCODE: usize = 2;
const CHAR_REFS: usize = 3;
const CHAR_REF_COUNT: usize = 4;
const RESET: usize = CHAR_REFS + CHAR_REF_COUNT;
const CANCEL: usize = RESET + 1;
const SAVE: usize = CANCEL + 1;
const CONTROLS: usize = SAVE + 1;

pub(crate) struct ChannelSettingsView {
    channel: String,
    mapping: CaseMapping,
    /// The server's name as shown under the title.
    server: String,
    pub(crate) choices: Choices,
    /// The encoding list that is open, with the option the keyboard is on
    /// (0 is "follow the server").
    pub(crate) open_list: Option<EncodingSide>,
    highlight: usize,
    pub(crate) error: Option<String>,
    controls: Vec<FocusHandle>,
    /// The first control takes the focus on the first draw.
    focus_pending: bool,
    body_scroll: ScrollHandle,
    menu_scroll: ScrollHandle,
    i18n: Localizer,
}

impl EventEmitter<ChannelSettingsEvent> for ChannelSettingsView {}

impl ChannelSettingsView {
    pub(crate) fn new(
        channel: &str,
        mapping: CaseMapping,
        server: String,
        choices: Choices,
        i18n: Localizer,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            channel: channel.to_owned(),
            mapping,
            server,
            choices,
            open_list: None,
            highlight: 0,
            error: None,
            controls: (0..CONTROLS)
                .map(|_| cx.focus_handle().tab_stop(true))
                .collect(),
            focus_pending: true,
            body_scroll: ScrollHandle::new(),
            menu_scroll: ScrollHandle::new(),
            i18n,
        }
    }

    pub(crate) fn channel(&self) -> &str {
        &self.channel
    }

    pub(crate) fn mapping(&self) -> CaseMapping {
        self.mapping
    }

    pub(crate) fn set_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.error = error;
        cx.notify();
    }

    /// Reset to default: the encodings and the reference mode follow the
    /// server again, once saved. The auto-join choice is not one of them.
    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        self.choices.decode = None;
        self.choices.encode = None;
        self.choices.char_refs = None;
        self.open_list = None;
        self.error = None;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        cx.emit(ChannelSettingsEvent::Save);
    }

    /// Escape closes an open list first, then the dialog (as Cancel).
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) {
        if self.open_list.take().is_some() {
            cx.notify();
        } else {
            cx.emit(ChannelSettingsEvent::Cancel);
        }
    }

    fn encoding(&self, side: EncodingSide) -> Option<TextEncoding> {
        match side {
            EncodingSide::Decode => self.choices.decode,
            EncodingSide::Encode => self.choices.encode,
        }
    }

    fn toggle_list(&mut self, side: EncodingSide, cx: &mut Context<Self>) {
        if self.open_list == Some(side) {
            self.open_list = None;
        } else {
            self.open_list = Some(side);
            self.highlight = self
                .encoding(side)
                .and_then(|current| TextEncoding::ALL.iter().position(|e| *e == current))
                .map_or(0, |at| at + 1);
            self.menu_scroll.scroll_to_item(self.highlight);
        }
        cx.notify();
    }

    fn choose(&mut self, side: EncodingSide, option: Option<TextEncoding>, cx: &mut Context<Self>) {
        match side {
            EncodingSide::Decode => self.choices.decode = option,
            EncodingSide::Encode => self.choices.encode = option,
        }
        self.open_list = None;
        self.error = None;
        cx.notify();
    }

    fn set_char_refs(&mut self, mode: Option<CharRefMode>, cx: &mut Context<Self>) {
        self.choices.char_refs = mode;
        self.error = None;
        cx.notify();
    }

    fn toggle_enabled(&mut self, cx: &mut Context<Self>) {
        self.choices.enabled = !self.choices.enabled;
        cx.notify();
    }

    /// A click on a selector. Enter and Space reach it as keyboard clicks (on
    /// key-up), so they are not handled in `key_down`; with the list open they
    /// choose the highlighted entry, a mouse click closes the list.
    fn click_selector(&mut self, side: EncodingSide, keyboard: bool, cx: &mut Context<Self>) {
        if keyboard && self.open_list == Some(side) {
            let option = self
                .highlight
                .checked_sub(1)
                .map(|at| TextEncoding::ALL[at]);
            self.choose(side, option, cx);
        } else {
            self.toggle_list(side, cx);
        }
    }

    /// Up and Down walk the open list.
    fn move_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.open_list.is_none() {
            return;
        }
        let last = TextEncoding::ALL.len();
        self.highlight = if down {
            (self.highlight + 1).min(last)
        } else {
            self.highlight.saturating_sub(1)
        };
        self.menu_scroll.scroll_to_item(self.highlight);
        cx.notify();
    }

    /// Tab / Shift+Tab: only the dialog's own controls are reachable.
    fn step_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let step = if forward { 1 } else { CONTROLS - 1 };
        let at = self
            .controls
            .iter()
            .position(|handle| handle.is_focused(window));
        let next = at.map_or(0, |at| (at + step) % CONTROLS);
        // An open list belongs to its selector.
        self.open_list = None;
        window.focus(&self.controls[next]);
        // The body's children: the hint, then one per control group.
        match next {
            AUTO_JOIN => self.body_scroll.scroll_to_item(1),
            DECODE => self.body_scroll.scroll_to_item(2),
            ENCODE => self.body_scroll.scroll_to_item(3),
            CHAR_REFS..RESET => self.body_scroll.scroll_to_item(4),
            _ => {}
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() || keystroke.is_ime_in_progress() {
            return;
        }
        match keystroke.key.as_str() {
            "escape" => self.escape(cx),
            "up" => self.move_highlight(false, cx),
            "down" => self.move_highlight(true, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// One labelled row of the form: the label above its control.
    fn field(label: String, palette: &crate::theme::Theme) -> Div {
        div().flex().flex_col().gap_1().child(
            div()
                .text_color(palette.text)
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
    }

    fn selector(
        &self,
        side: EncodingSide,
        label: String,
        ring: Rgba,
        palette: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let (id, control) = match side {
            EncodingSide::Decode => ("channel-decode", DECODE),
            EncodingSide::Encode => ("channel-encode", ENCODE),
        };
        let inherit = self.i18n.text("channel_settings_inherit");
        let current = self.encoding(side);
        let open = self.open_list == Some(side);
        let mut selector = Self::field(label, palette).child(
            div()
                .id(id)
                .debug_selector(|| id.into())
                .track_focus(&self.controls[control])
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px_2()
                .py_1()
                .bg(palette.surface)
                .border_1()
                .border_color(if open { ring } else { palette.border })
                .focus(move |style| style.border_color(ring))
                .rounded(px(4.))
                .cursor_pointer()
                .child(
                    div()
                        .min_w_0()
                        .child(current.map_or_else(|| inherit.clone(), |e| e.label().to_owned())),
                )
                .child(div().text_color(palette.text_secondary).child("▾"))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.click_selector(side, event.is_keyboard(), cx)
                })),
        );
        if open {
            let hover = palette.hover;
            let selected = palette.selected;
            let mut menu = div()
                .id(("channel-encoding-list", control))
                .debug_selector(|| "channel-encoding-list".into())
                .track_scroll(&self.menu_scroll)
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                .max_h(MENU_HEIGHT)
                .overflow_y_scroll()
                .bg(palette.surface)
                .border_1()
                .border_color(palette.border)
                .rounded(px(4.));
            let options = std::iter::once(None).chain(TextEncoding::ALL.into_iter().map(Some));
            for (index, option) in options.enumerate() {
                menu = menu.child(
                    div()
                        .id((id, index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(move |d| d.bg(hover))
                        .when(current == option, |d| d.bg(selected))
                        .when(self.highlight == index, |d| d.bg(hover))
                        .child(option.map_or(inherit.clone(), |e| e.label().to_owned()))
                        .on_click(cx.listener(move |this, _, _, cx| this.choose(side, option, cx))),
                );
            }
            selector = selector.child(menu);
        }
        selector
    }
}

impl Render for ChannelSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_pending {
            self.focus_pending = false;
            window.focus(&self.controls[AUTO_JOIN]);
        }
        let palette = settings_theme::palette(cx);
        let ring = settings_theme::current(cx).map_or(palette.text, |native| {
            settings_theme::color(native.defaults.focus_ring_color)
        });
        let heading = self.i18n.format(
            "channel_settings_heading",
            &[("channel", &display::neutralize_bidi(&self.channel))],
        );
        let server = self.i18n.format(
            "channel_settings_server",
            &[("server", &display::neutralize_bidi(&self.server))],
        );

        let auto_join = div()
            .id("channel-settings-auto-join")
            .debug_selector(|| "channel-settings-auto-join".into())
            .track_focus(&self.controls[AUTO_JOIN])
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .rounded(px(4.))
            .border_1()
            .border_color(gpui::transparent_black())
            .focus(move |style| style.border_color(ring))
            .cursor_pointer()
            .child(settings_theme::checkbox(self.choices.enabled, true, cx))
            .child(self.i18n.text("channel_settings_auto_join"))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_enabled(cx)));

        let mut modes = div().flex().flex_wrap().gap_1();
        for (index, (mode, key)) in char_ref_choices(true).into_iter().enumerate() {
            let selected = self.choices.char_refs == mode;
            modes = modes.child(
                settings_theme::button(("channel-char-refs", index), selected, cx)
                    .track_focus(&self.controls[CHAR_REFS + index])
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text(key))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_char_refs(mode, cx))),
            );
        }
        let char_refs = Self::field(self.i18n.text("channel_settings_char_refs"), &palette)
            .child(modes)
            .child(
                div()
                    .text_sm()
                    .text_color(palette.text_secondary)
                    .child(self.i18n.text("iso2022jp_char_refs_hint")),
            );

        let body = div()
            .id("channel-settings-body")
            .track_scroll(&self.body_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .px_4()
            .py_3()
            .child(
                div()
                    .text_sm()
                    .text_color(palette.text_secondary)
                    .child(self.i18n.text("channel_settings_hint")),
            )
            .child(auto_join)
            .child(self.selector(
                EncodingSide::Decode,
                self.i18n.text("channel_settings_decode"),
                ring,
                &palette,
                cx,
            ))
            .child(self.selector(
                EncodingSide::Encode,
                self.i18n.text("channel_settings_encode"),
                ring,
                &palette,
                cx,
            ))
            .child(char_refs);

        // Outside the scrolling body, so a rejected save is always visible.
        let error = self.error.clone().map(|error| {
            div()
                .id("channel-settings-error")
                .debug_selector(|| "channel-settings-error".into())
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(palette.border)
                .text_color(palette.warning)
                .child(error)
        });

        let footer = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(palette.border)
            .child(
                settings_theme::button("channel-settings-reset", false, cx)
                    .track_focus(&self.controls[RESET])
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text("channel_settings_reset"))
                    .on_click(cx.listener(|this, _, _, cx| this.reset(cx))),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        settings_theme::button("channel-settings-cancel", false, cx)
                            .track_focus(&self.controls[CANCEL])
                            .focus(move |style| style.border_color(ring))
                            .child(self.i18n.text("channel_settings_cancel"))
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ChannelSettingsEvent::Cancel)),
                            ),
                    )
                    .child(
                        settings_theme::button("channel-settings-save", true, cx)
                            .track_focus(&self.controls[SAVE])
                            .focus(move |style| style.border_color(ring))
                            .child(self.i18n.text("channel_settings_save"))
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ChannelSettingsEvent::Save)),
                            ),
                    ),
            );

        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .id("channel-settings-title")
                    .text_size(rems(1.1))
                    .font_weight(FontWeight::BOLD)
                    .child(heading),
            )
            .child(
                div()
                    .id("channel-settings-server")
                    .text_sm()
                    .text_color(palette.text_secondary)
                    .child(server),
            );

        let panel = settings_theme::dialog_panel(cx)
            .id("channel-settings-dialog")
            .debug_selector(|| "channel-settings-dialog".into())
            .occlude()
            .w(PANEL_WIDTH)
            .max_w_full()
            .max_h_full()
            .child(header)
            .child(body)
            .children(error)
            .child(footer);
        // The backdrop takes every click and key, so nothing behind the
        // dialog changes; a click outside the panel does not close it.
        div()
            .id("channel-settings-backdrop")
            .debug_selector(|| "channel-settings-backdrop".into())
            .key_context("ChannelSettingsDialog")
            .on_action(
                cx.listener(|this, _: &FocusNextField, window, cx| {
                    this.step_focus(true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &FocusPreviousField, window, cx| {
                this.step_focus(false, window, cx)
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.key_down(event, cx)))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p(MARGIN)
            .bg(gpui::black().opacity(0.4))
            .child(panel)
    }
}

/// The dialog shown over the chat window, opened from the channel menu.
pub(crate) struct ChannelSettingsPrompt {
    network: NetworkId,
    profile_id: String,
    view: Entity<ChannelSettingsView>,
    previous: Option<FocusHandle>,
    _subscription: Subscription,
}

impl ChannelSettingsPrompt {
    pub(crate) fn network(&self) -> NetworkId {
        self.network
    }

    pub(crate) fn view(&self) -> &Entity<ChannelSettingsView> {
        &self.view
    }
}

impl ChatWindow {
    /// Shows the settings of `channel` of `network` over the chat window. The
    /// values are those of the settings file, which a settings window or
    /// `/charset` may have updated since this window last saved.
    pub(crate) fn show_channel_settings(
        &mut self,
        network: NetworkId,
        channel: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile_id) = self
            .sessions
            .get(&network)
            .map(|session| session.profile_id.clone())
        else {
            return;
        };
        if self.channel_settings.is_some() {
            return;
        }
        let mapping = self.state.casemapping(network);
        let latest = match settings_file::load() {
            Ok(Some(settings)) if settings.profile(&profile_id).is_some() => Some(settings),
            _ => None,
        };
        let Some(profile) = latest.as_ref().unwrap_or(&self.saved).profile(&profile_id) else {
            return;
        };
        let choices = Choices::of(profile, channel, mapping);
        let server = self.prompt_server_label(network);
        let i18n = self.i18n.clone();
        let view =
            cx.new(|cx| ChannelSettingsView::new(channel, mapping, server, choices, i18n, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                ChannelSettingsEvent::Save => this.save_channel_settings_prompt(window, cx),
                ChannelSettingsEvent::Cancel => this.close_channel_settings_prompt(window, cx),
            });
        settings_theme::refresh(cx);
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        self.channel_settings = Some(ChannelSettingsPrompt {
            network,
            profile_id,
            view,
            previous: window.focused(cx),
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Closes the dialog without saving, putting the focus back.
    pub(crate) fn close_channel_settings_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = self.channel_settings.take()
            && let Some(handle) = prompt.previous
        {
            window.focus(&handle);
        }
        cx.notify();
    }

    /// Save: writes the channel's settings into the settings file, starting
    /// from the file, and applies them as the settings window's save does;
    /// nothing is joined or parted. A failure keeps the dialog open with its
    /// reason.
    fn save_channel_settings_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.channel_settings.as_ref() else {
            return;
        };
        let (network, profile_id, view) = (
            prompt.network,
            prompt.profile_id.clone(),
            prompt.view.clone(),
        );
        let (channel, mapping, choices) = {
            let view = view.read(cx);
            (view.channel().to_owned(), view.mapping(), view.choices)
        };
        let fail = |error: String, cx: &mut Context<Self>| {
            view.update(cx, |view, cx| view.set_error(Some(error), cx));
        };
        // The server may have been removed while the dialog was open.
        if self.sessions.get(&network).map(|s| s.profile_id.as_str()) != Some(profile_id.as_str()) {
            self.close_channel_settings_prompt(window, cx);
            return;
        }
        let mut settings = match settings_file::load() {
            Ok(Some(settings)) if settings.profile(&profile_id).is_some() => settings,
            Ok(_) => self.saved.clone(),
            Err(error) => return fail(error, cx),
        };
        let utf8_only = self.channel_settings_context(&profile_id).1;
        let Some(profile) = settings.profile_mut(&profile_id) else {
            self.close_channel_settings_prompt(window, cx);
            return;
        };
        if let Err(error) =
            apply_charsets(profile, &channel, mapping, choices, utf8_only, &self.i18n)
        {
            return fail(error, cx);
        }
        apply_auto_join(profile, &channel, mapping, choices.enabled);
        let profile = profile.clone();
        if let Err(error) = settings_file::save(&settings) {
            return fail(error, cx);
        }
        let previous = self.saved.clone();
        self.apply_settings_change(&previous, settings, true, window, cx);
        if let Some(handle) = self.settings_window {
            let _ = handle.update(cx, |form, _, cx| {
                form.follow_channel_save(&profile, &channel, mapping, choices.enabled, cx)
            });
        }
        self.close_channel_settings_prompt(window, cx);
    }
}
