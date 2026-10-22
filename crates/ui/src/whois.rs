// SPDX-License-Identifier: GPL-3.0-only
use crate::{ChatWindow, localization::Localizer, os_text::WindowOsText};
use cayenchat_irc_core::WhoisInfo;
use cayenchat_model::{NetworkId, display, names::CaseMapping};
use gpui::{prelude::*, *};
use std::{collections::HashSet, sync::Arc};

/// Side of the avatar box in logical pixels.
const AVATAR_SLOT: f32 = 48.;

/// Pixels a side of the avatar to draw in the box on a window with
/// `scale_factor` device pixels per logical pixel, so it is never enlarged.
pub fn avatar_pixels(scale_factor: f32) -> u32 {
    (AVATAR_SLOT * scale_factor.max(1.)).ceil() as u32
}

/// The channels we are in on a network, folded under the network's case
/// mapping so a WHOIS entry matches however the server cases it.
#[derive(Clone, Default, PartialEq)]
pub struct JoinedChannels {
    pub mapping: CaseMapping,
    pub names: HashSet<String>,
}

impl JoinedChannels {
    fn contains(&self, channel: &str) -> bool {
        self.names.contains(&self.mapping.fold(channel))
    }
}

pub struct WhoisWindow {
    owner: WindowHandle<ChatWindow>,
    /// Server the reply came from; actions go back to the same server.
    network: NetworkId,
    info: WhoisInfo,
    // Joined channel names pushed by the owner; reading the owner
    // while rendering would re-enter it during its own update.
    joined: JoinedChannels,
    selected_channel: usize,
    channel_menu_open: bool,
    details_scroll: ScrollHandle,
    menu_scroll: ScrollHandle,
    status: Option<String>,
    /// The user's avatar image, pushed by the owner (it owns the cache).
    avatar: Option<Arc<RenderImage>>,
    /// The nickname's default icon, drawn while `avatar` is empty and the
    /// owner says no image is coming.
    default_avatar: Option<Arc<gpui::Image>>,
    /// Images this window replaced; their tiles in its own atlas are freed
    /// at the next render, which no longer draws them.
    retired: Vec<Arc<RenderImage>>,
    /// Whether the server exchanges avatars, so the user can be asked.
    can_request_avatar: bool,
    i18n: Localizer,
    focus: FocusHandle,
    // Refreshed at the start of each render for row helpers.
    theme: crate::theme::Theme,
}

impl WhoisWindow {
    pub fn open(
        owner: WindowHandle<ChatWindow>,
        network: NetworkId,
        info: WhoisInfo,
        joined: JoinedChannels,
        i18n: Localizer,
        cx: &mut App,
    ) -> Result<WindowHandle<Self>, String> {
        let bounds = Bounds::centered(None, size(px(460.), px(480.)), cx);
        let title = window_title(&info);
        cx.open_window(
            WindowOptions {
                app_id: Some(crate::APP_ID.into()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(360.), px(300.))),
                titlebar: Some(TitlebarOptions {
                    // A fixed title: the nickname is server text and reaches
                    // the OS only through the gate below (`os_set_title`).
                    title: Some("CayenChat".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                // Gives X11 the UTF-8 title too (issue #143).
                window.os_set_title(&title);
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    window.focus(&focus);
                    Self {
                        owner,
                        network,
                        info,
                        joined,
                        selected_channel: 0,
                        channel_menu_open: false,
                        details_scroll: ScrollHandle::new(),
                        menu_scroll: ScrollHandle::new(),
                        status: None,
                        avatar: None,
                        default_avatar: None,
                        retired: Vec::new(),
                        can_request_avatar: false,
                        i18n,
                        focus,
                        theme: crate::theme::current(cx),
                    }
                })
            },
        )
        .map_err(|error| error.to_string())
    }

    /// Replaces the shown reply; a missing nickname keeps the last known details.
    pub fn set_info(&mut self, info: WhoisInfo, window: &mut Window, cx: &mut Context<Self>) {
        if info.found() {
            window.os_set_title(&window_title(&info));
            let selected = self.info.channels.get(self.selected_channel).cloned();
            self.selected_channel = selected
                .and_then(|entry| info.channels.iter().position(|other| *other == entry))
                .unwrap_or(0);
            self.channel_menu_open = false;
            self.info = info;
            self.status = None;
        } else {
            self.status = Some(self.i18n.format_nickname("whois_not_found", &info.nickname));
        }
        cx.notify();
    }

    pub fn set_joined(&mut self, joined: JoinedChannels, cx: &mut Context<Self>) {
        if self.joined != joined {
            self.joined = joined;
            cx.notify();
        }
    }

    /// The avatar to draw and whether it can be asked for; redrawn only on
    /// a change.
    pub fn set_avatar(
        &mut self,
        avatar: Option<Arc<RenderImage>>,
        default_avatar: Option<Arc<gpui::Image>>,
        can_request: bool,
        cx: &mut Context<Self>,
    ) {
        let mut changed = swap_avatar(&mut self.avatar, &mut self.retired, avatar);
        if !same_arc(&self.default_avatar, &default_avatar) {
            self.default_avatar = default_avatar;
            changed = true;
        }
        if changed || self.can_request_avatar != can_request {
            self.can_request_avatar = can_request;
            cx.notify();
        }
    }

    /// The nickname as the server reported it, which the default icon is
    /// made from.
    pub fn nickname(&self) -> &str {
        &self.info.nickname
    }

    fn request_avatar(&mut self, cx: &mut Context<Self>) {
        let nickname = self.info.nickname.clone();
        let network = self.network;
        let result = self
            .owner
            .update(cx, |chat, _, cx| {
                chat.request_peer_avatar(network, &nickname, cx)
            })
            .unwrap_or_else(|_| Err(self.i18n.text("not_connected")));
        self.status = Some(match result {
            Ok(()) => self.i18n.text("whois_avatar_asked"),
            Err(error) => error,
        });
        cx.notify();
    }

    pub fn set_localizer(&mut self, i18n: Localizer, cx: &mut Context<Self>) {
        self.i18n = i18n;
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let nickname = self.info.nickname.clone();
        let network = self.network;
        let result = self
            .owner
            .update(cx, |chat, _, cx| chat.request_whois(network, &nickname, cx))
            .unwrap_or_else(|_| Err(self.i18n.text("not_connected")));
        self.status = Some(match result {
            Ok(()) => self.i18n.text("whois_updating"),
            Err(error) => error,
        });
        cx.notify();
    }

    fn private_message(&mut self, cx: &mut Context<Self>) {
        let nickname = self.info.nickname.clone();
        let network = self.network;
        let _ = self.owner.update(cx, |chat, window, cx| {
            chat.show_private_message_prompt(network, nickname, window, cx);
            window.activate_window();
        });
    }

    fn join(&mut self, channel: String, cx: &mut Context<Self>) {
        let network = self.network;
        let result = self
            .owner
            .update(cx, |chat, _, cx| chat.join_channel(network, &channel, cx))
            .unwrap_or_else(|_| Err(self.i18n.text("not_connected")));
        self.status = result.err();
        cx.notify();
    }

    fn row(&self, key: &str, value: impl IntoElement) -> Div {
        div()
            .flex()
            .gap_3()
            .child(
                div()
                    .w(px(110.))
                    .flex_shrink_0()
                    .text_color(self.theme.text_secondary)
                    .child(self.i18n.text(key)),
            )
            .child(div().flex_1().min_w_0().child(value))
    }

    /// Values come from the server and the user themselves (real name, away
    /// text), so bidirectional controls are neutralized before drawing.
    fn text_row(&self, key: &str, value: Option<String>) -> Option<Div> {
        value
            .filter(|value| !value.is_empty())
            .map(|value| self.row(key, display::neutralize_bidi_owned(value)))
    }

    fn channel_selector(&self, cx: &mut Context<Self>) -> Div {
        let theme = crate::theme::current(cx);
        let index = self.selected_channel.min(self.info.channels.len() - 1);
        let entry = self.info.channels[index].clone();
        let channel = channel_name(&entry).to_owned();
        let action = if self.joined.contains(&channel) {
            div()
                .flex_shrink_0()
                .text_color(theme.text_secondary)
                .child(self.i18n.text("whois_joined"))
                .into_any_element()
        } else {
            button("whois-join", self.i18n.text("whois_join"), false, theme)
                .on_click(cx.listener(move |this, _, _, cx| this.join(channel.clone(), cx)))
                .into_any_element()
        };
        let count = self.info.channels.len();
        let mut selector = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("whois-channel-select")
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .bg(theme.surface)
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(display::neutralize_bidi_owned(entry)),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.text_secondary)
                                .child(format!("{}/{count}  ▾", index + 1)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.channel_menu_open = !this.channel_menu_open;
                            cx.notify();
                        })),
                )
                .child(action),
        );
        if self.channel_menu_open {
            let mut menu = div()
                .id("whois-channel-menu")
                .track_scroll(&self.menu_scroll)
                .max_h(px(180.))
                .overflow_y_scroll()
                .bg(theme.surface)
                .border_1()
                .border_t_0()
                .border_color(theme.border);
            for (option, entry) in self.info.channels.iter().enumerate() {
                let joined = self.joined.contains(channel_name(entry));
                menu = menu.child(
                    div()
                        .id(("whois-channel-option", option))
                        .flex()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .when(option == index, |d| d.bg(theme.selected))
                        .hover(|d| d.bg(theme.hover_strong))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(display::neutralize_bidi(entry).into_owned()),
                        )
                        .when(joined, |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme.text_secondary)
                                    .child(self.i18n.text("whois_joined")),
                            )
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected_channel = option;
                            this.channel_menu_open = false;
                            cx.notify();
                        })),
                );
            }
            selector = selector.child(div().relative().child(menu).child(
                crate::scrollbar::scrollbar(
                    "scrollbar-whois-menu",
                    &self.menu_scroll,
                    theme.text_muted,
                ),
            ));
        }
        selector
    }
}

/// The window title, at open and on every refresh: the nickname as drawn.
fn window_title(info: &WhoisInfo) -> String {
    display::neutralize_bidi(&info.nickname).into_owned()
}

/// Strips the membership prefix a 319 reply puts before each channel.
fn channel_name(entry: &str) -> &str {
    let mut name = entry;
    loop {
        let mut chars = name.chars();
        match (chars.next(), chars.next()) {
            (Some('~' | '@' | '%' | '+'), Some(_)) => name = &name[1..],
            (Some('&' | '!'), Some('#' | '&' | '!')) => name = &name[1..],
            _ => return name,
        }
    }
}

fn idle_text(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let clock = format!("{}:{:02}:{:02}", rest / 3600, rest % 3600 / 60, rest % 60);
    if days > 0 {
        format!("{days}d {clock}")
    } else {
        clock
    }
}

fn signon_text(timestamp: i64) -> Option<String> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
}

fn button(
    id: impl Into<ElementId>,
    label: String,
    primary: bool,
    theme: crate::theme::Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .flex_shrink_0()
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .when(primary, |d| d.bg(theme.selected))
        .when(!primary, |d| d.bg(theme.surface))
        .hover(|d| d.bg(theme.hover_strong))
        .child(label)
}

impl Render for WhoisWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
        let title = window_title(&self.info);
        let content = self.render_content(cx);
        crate::decorations::window_frame(window, cx, title, content)
    }
}

impl WhoisWindow {
    fn render_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = crate::theme::current(cx);
        self.theme = theme;
        let info = self.info.clone();
        let user_host = match (&info.username, &info.host) {
            (Some(user), Some(host)) => Some(format!("{user}@{host}")),
            _ => None,
        };
        let mut details = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border);
        let askable = info.avatar_mark && self.can_request_avatar;
        let avatar_row = self.row(
            "whois_avatar",
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(px(AVATAR_SLOT))
                        .h(px(AVATAR_SLOT))
                        .flex_shrink_0()
                        .overflow_hidden()
                        .border_1()
                        .border_color(theme.border)
                        .children(match (&self.avatar, &self.default_avatar) {
                            (Some(image), _) => Some(img(image.clone()).size_full()),
                            (None, Some(image)) => Some(img(image.clone()).size_full()),
                            (None, None) => None,
                        }),
                )
                .child(if askable {
                    button(
                        "whois-avatar-fetch",
                        self.i18n.text("whois_avatar_fetch"),
                        false,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.request_avatar(cx)))
                    .into_any_element()
                } else {
                    // Disabled: no mark in the real name, or no exchange.
                    div()
                        .px_3()
                        .py_1()
                        .border_1()
                        .border_color(theme.border)
                        .text_color(theme.text_muted)
                        .child(self.i18n.text("whois_avatar_fetch"))
                        .into_any_element()
                }),
        );
        details = details
            .child(avatar_row)
            .children(self.text_row("whois_nickname", Some(info.nickname.clone())))
            .children(self.text_row("whois_user_host", user_host))
            .children(self.text_row("whois_real_name", info.realname.clone()))
            .children(self.text_row("whois_account", info.account.clone()));
        if !info.channels.is_empty() {
            details = details.child(self.row("whois_channels", self.channel_selector(cx)));
        }
        details = details
            .children(self.text_row("whois_server", info.server.clone()))
            .children(self.text_row("whois_server_info", info.server_info.clone()))
            .children(self.text_row("whois_operator", info.operator.clone()))
            .children(self.text_row("whois_away", info.away.clone()))
            .children(self.text_row("whois_idle", info.idle_seconds.map(idle_text)))
            .children(self.text_row("whois_signon", info.signon.and_then(signon_text)));
        if !info.extra.is_empty() {
            details = details.child(
                self.row(
                    "whois_other",
                    div().flex().flex_col().children(
                        info.extra
                            .iter()
                            .map(|line| div().child(display::neutralize_bidi(line).into_owned())),
                    ),
                ),
            );
        }
        div()
            .id("whois-screen")
            .key_context("WhoisWindow")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(theme.window)
            .text_size(px(13.))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                if this.channel_menu_open {
                    this.channel_menu_open = false;
                    cx.notify();
                } else {
                    window.remove_window();
                }
            }))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("whois-details")
                            .track_scroll(&self.details_scroll)
                            .size_full()
                            .overflow_y_scroll()
                            .child(details),
                    )
                    .child(crate::scrollbar::scrollbar(
                        "scrollbar-whois-details",
                        &self.details_scroll,
                        theme.text_muted,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(theme.warning)
                            .children(self.status.clone()),
                    )
                    .child(
                        button(
                            "whois-private-message",
                            self.i18n.text("member_private_message"),
                            false,
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.private_message(cx))),
                    )
                    .child(
                        button("whois-update", self.i18n.text("whois_update"), false, theme)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        button("whois-close", self.i18n.text("whois_close"), true, theme)
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
            .into_any_element()
    }
}

/// Shows `new` in place of `current`, queueing a replaced image for release;
/// returns whether the shown image changed.
fn swap_avatar(
    current: &mut Option<Arc<RenderImage>>,
    retired: &mut Vec<Arc<RenderImage>>,
    new: Option<Arc<RenderImage>>,
) -> bool {
    if same_arc(&*current, &new) {
        return false;
    }
    retired.extend(std::mem::replace(current, new));
    true
}

/// Whether both hold the same allocation (or neither holds one).
fn same_arc<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// What the icon slot gets for a lookup result: the image, or the default
/// icon when there is none. Loading keeps the slot empty. The last value
/// says a load is pending.
pub fn slot_contents<D>(
    shown: crate::avatars::Shown,
    default: impl FnOnce() -> D,
) -> (Option<Arc<RenderImage>>, Option<D>, bool) {
    use crate::avatars::Shown;
    match shown {
        Shown::Image(image) => (Some(image), None, false),
        Shown::Pending => (None, None, true),
        Shown::None => (None, Some(default()), false),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WhoisInfo, channel_name, idle_text, same_arc, slot_contents, swap_avatar, window_title,
    };
    use crate::avatars::Shown;
    use gpui::RenderImage;
    use std::sync::Arc;

    #[test]
    fn slot_shows_image_default_or_nothing_while_loading() {
        let a = image();
        let (img, default, pending) = slot_contents(Shown::Image(a.clone()), || "d");
        assert!(Arc::ptr_eq(&img.unwrap(), &a));
        assert!(default.is_none() && !pending);
        let (img, default, pending) = slot_contents(Shown::Pending, || "d");
        assert!(img.is_none() && default.is_none() && pending);
        // No avatar, a failed fetch or avatars switched off.
        let (img, default, pending) = slot_contents(Shown::None, || "d");
        assert!(img.is_none() && default == Some("d") && !pending);
    }

    #[test]
    fn avatar_pixels_cover_the_box_at_every_scale() {
        use super::avatar_pixels;
        assert_eq!(avatar_pixels(1.), 48);
        assert_eq!(avatar_pixels(1.5), 72);
        assert_eq!(avatar_pixels(2.), 96);
        // Rounded up, never short of the box.
        assert_eq!(avatar_pixels(1.25), 60);
        assert_eq!(avatar_pixels(1.01), 49);
    }

    #[test]
    fn default_is_only_built_when_needed() {
        slot_contents(Shown::Pending, || -> () { panic!("built") });
        slot_contents(Shown::Image(image()), || -> () { panic!("built") });
    }

    #[test]
    fn same_arc_compares_identity() {
        let (a, b) = (Arc::new(1), Arc::new(1));
        assert!(same_arc(&Some(a.clone()), &Some(a.clone())));
        assert!(!same_arc(&Some(a.clone()), &Some(b)));
        assert!(!same_arc(&Some(a), &None));
        assert!(same_arc::<i32>(&None, &None));
    }

    fn image() -> Arc<RenderImage> {
        let buffer = image::RgbaImage::from_raw(1, 1, vec![0; 4]).unwrap();
        Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
    }

    #[test]
    fn replaced_and_cleared_avatars_are_queued_for_release() {
        let (a, b) = (image(), image());
        let (mut current, mut retired) = (None, Vec::new());
        assert!(swap_avatar(&mut current, &mut retired, Some(a.clone())));
        assert!(retired.is_empty());
        assert!(!swap_avatar(&mut current, &mut retired, Some(a.clone())));
        assert!(swap_avatar(&mut current, &mut retired, Some(b.clone())));
        assert!(Arc::ptr_eq(&retired[0], &a));
        assert!(swap_avatar(&mut current, &mut retired, None));
        assert!(Arc::ptr_eq(&retired[1], &b));
        assert!(current.is_none());
    }

    #[test]
    fn strips_membership_prefixes_but_keeps_channel_prefixes() {
        assert_eq!(
            channel_name("@#えふわん721@irc:*.jp"),
            "#えふわん721@irc:*.jp"
        );
        assert_eq!(channel_name("@+#test"), "#test");
        assert_eq!(channel_name("&#test"), "#test");
        assert_eq!(channel_name("&local"), "&local");
        assert_eq!(channel_name("@&local"), "&local");
        assert_eq!(channel_name("#plain"), "#plain");
        assert_eq!(channel_name("!ABCDEtest"), "!ABCDEtest");
        assert_eq!(channel_name("@!ABCDEtest"), "!ABCDEtest");
        assert_eq!(channel_name("@+!ABCDE日本語"), "!ABCDE日本語");
        assert_eq!(channel_name("&!ABCDEtest"), "!ABCDEtest");
        assert_eq!(channel_name("!#test"), "#test");
    }

    #[test]
    fn window_title_neutralizes_bidi_but_leaves_the_nickname_alone() {
        let info = WhoisInfo {
            nickname: "mal\u{202E}lory".to_owned(),
            ..WhoisInfo::default()
        };
        assert_eq!(window_title(&info), "mal\u{200B}lory");
        assert_eq!(info.nickname, "mal\u{202E}lory");
    }

    #[test]
    fn formats_idle_time_as_clock_with_days() {
        assert_eq!(idle_text(665), "0:11:05");
        assert_eq!(idle_text(90_061), "1d 1:01:01");
    }
}
