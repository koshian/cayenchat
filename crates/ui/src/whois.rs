use crate::{ChatWindow, localization::Localizer};
use cayenchat_irc_core::WhoisInfo;
use gpui::{prelude::*, *};
use std::collections::HashSet;

pub struct WhoisWindow {
    owner: WindowHandle<ChatWindow>,
    info: WhoisInfo,
    // Lowercase joined channel names pushed by the owner; reading the owner
    // while rendering would re-enter it during its own update.
    joined: HashSet<String>,
    selected_channel: usize,
    channel_menu_open: bool,
    status: Option<String>,
    i18n: Localizer,
    focus: FocusHandle,
    // Refreshed at the start of each render for row helpers.
    theme: crate::theme::Theme,
}

impl WhoisWindow {
    pub fn open(
        owner: WindowHandle<ChatWindow>,
        info: WhoisInfo,
        joined: HashSet<String>,
        i18n: Localizer,
        cx: &mut App,
    ) -> Result<WindowHandle<Self>, String> {
        let bounds = Bounds::centered(None, size(px(460.), px(480.)), cx);
        let title = info.nickname.clone();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(360.), px(300.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    window.focus(&focus);
                    Self {
                        owner,
                        info,
                        joined,
                        selected_channel: 0,
                        channel_menu_open: false,
                        status: None,
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
            window.set_window_title(&info.nickname);
            let selected = self.info.channels.get(self.selected_channel).cloned();
            self.selected_channel = selected
                .and_then(|entry| info.channels.iter().position(|other| *other == entry))
                .unwrap_or(0);
            self.channel_menu_open = false;
            self.info = info;
            self.status = None;
        } else {
            self.status = Some(
                self.i18n
                    .format("whois_not_found", &[("nickname", &info.nickname)]),
            );
        }
        cx.notify();
    }

    pub fn set_joined(&mut self, joined: HashSet<String>, cx: &mut Context<Self>) {
        if self.joined != joined {
            self.joined = joined;
            cx.notify();
        }
    }

    pub fn set_localizer(&mut self, i18n: Localizer, cx: &mut Context<Self>) {
        self.i18n = i18n;
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let nickname = self.info.nickname.clone();
        let result = self
            .owner
            .update(cx, |chat, _, cx| chat.request_whois(&nickname, cx))
            .unwrap_or_else(|_| Err(self.i18n.text("not_connected")));
        self.status = Some(match result {
            Ok(()) => self.i18n.text("whois_updating"),
            Err(error) => error,
        });
        cx.notify();
    }

    fn private_message(&mut self, cx: &mut Context<Self>) {
        let nickname = self.info.nickname.clone();
        let _ = self.owner.update(cx, |chat, window, cx| {
            chat.show_private_message_prompt(nickname, window, cx);
            window.activate_window();
        });
    }

    fn join(&mut self, channel: String, cx: &mut Context<Self>) {
        let result = self
            .owner
            .update(cx, |chat, _, cx| chat.join_channel(&channel, cx))
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

    fn text_row(&self, key: &str, value: Option<String>) -> Option<Div> {
        value
            .filter(|value| !value.is_empty())
            .map(|value| self.row(key, value))
    }

    fn channel_selector(&self, cx: &mut Context<Self>) -> Div {
        let theme = crate::theme::current(cx);
        let index = self.selected_channel.min(self.info.channels.len() - 1);
        let entry = self.info.channels[index].clone();
        let channel = channel_name(&entry).to_owned();
        let action = if self.joined.contains(&channel.to_lowercase()) {
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
                        .child(div().flex_1().min_w_0().truncate().child(entry))
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
                .max_h(px(180.))
                .overflow_y_scroll()
                .bg(theme.surface)
                .border_1()
                .border_t_0()
                .border_color(theme.border);
            for (option, entry) in self.info.channels.iter().enumerate() {
                let joined = self.joined.contains(&channel_name(entry).to_lowercase());
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
                        .child(div().flex_1().min_w_0().truncate().child(entry.clone()))
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
            selector = selector.child(menu);
        }
        selector
    }
}

/// Strips the membership prefix a 319 reply puts before each channel.
fn channel_name(entry: &str) -> &str {
    let mut name = entry;
    loop {
        let mut chars = name.chars();
        match (chars.next(), chars.next()) {
            (Some('~' | '@' | '%' | '+' | '!'), Some(_)) => name = &name[1..],
            (Some('&'), Some('#' | '&')) => name = &name[1..],
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
        let title = self.info.nickname.clone();
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
        details = details
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
                    div()
                        .flex()
                        .flex_col()
                        .children(info.extra.iter().map(|line| div().child(line.clone()))),
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
                    .id("whois-details")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(details),
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

#[cfg(test)]
mod tests {
    use super::{channel_name, idle_text};

    #[test]
    fn strips_membership_prefixes_but_keeps_ampersand_channels() {
        assert_eq!(
            channel_name("@#えふわん721@irc:*.jp"),
            "#えふわん721@irc:*.jp"
        );
        assert_eq!(channel_name("@+#test"), "#test");
        assert_eq!(channel_name("&#test"), "#test");
        assert_eq!(channel_name("&local"), "&local");
        assert_eq!(channel_name("@&local"), "&local");
        assert_eq!(channel_name("#plain"), "#plain");
    }

    #[test]
    fn formats_idle_time_as_clock_with_days() {
        assert_eq!(idle_text(665), "0:11:05");
        assert_eq!(idle_text(90_061), "1d 1:01:01");
    }
}
