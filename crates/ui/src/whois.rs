use crate::{ChatWindow, localization::Localizer};
use cayenchat_irc_core::WhoisInfo;
use gpui::{prelude::*, *};

const BORDER: u32 = 0xb7bdc4;
const MUTED: u32 = 0x6b737c;

pub struct WhoisWindow {
    owner: WindowHandle<ChatWindow>,
    info: WhoisInfo,
    status: Option<String>,
    i18n: Localizer,
    focus: FocusHandle,
}

impl WhoisWindow {
    pub fn open(
        owner: WindowHandle<ChatWindow>,
        info: WhoisInfo,
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
                        status: None,
                        i18n,
                        focus,
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
                    .text_color(rgb(MUTED))
                    .child(self.i18n.text(key)),
            )
            .child(div().flex_1().min_w_0().child(value))
    }

    fn text_row(&self, key: &str, value: Option<String>) -> Option<Div> {
        value
            .filter(|value| !value.is_empty())
            .map(|value| self.row(key, value))
    }

    fn channel_list(&self, cx: &mut Context<Self>) -> Div {
        let joined: Vec<bool> = {
            let chat = self.owner.read(cx).ok();
            self.info
                .channels
                .iter()
                .map(|entry| chat.is_some_and(|chat| chat.is_joined(channel_name(entry))))
                .collect()
        };
        let mut list = div().flex().flex_col().gap_1();
        for (index, entry) in self.info.channels.iter().enumerate() {
            let channel = channel_name(entry).to_owned();
            let action = if joined[index] {
                div()
                    .text_color(rgb(MUTED))
                    .child(self.i18n.text("whois_joined"))
                    .into_any_element()
            } else {
                button(("whois-join", index), self.i18n.text("whois_join"), false)
                    .on_click(cx.listener(move |this, _, _, cx| this.join(channel.clone(), cx)))
                    .into_any_element()
            };
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().min_w_0().truncate().child(entry.clone()))
                    .child(action),
            );
        }
        list
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

fn button(id: impl Into<ElementId>, label: String, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .flex_shrink_0()
        .border_1()
        .border_color(rgb(BORDER))
        .cursor_pointer()
        .when(primary, |d| d.bg(rgb(0xcbdbea)))
        .when(!primary, |d| d.bg(rgb(0xffffff)))
        .hover(|d| d.bg(rgb(0xdce5ee)))
        .child(label)
}

impl Render for WhoisWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .bg(rgb(0xffffff))
            .border_1()
            .border_color(rgb(BORDER));
        details = details
            .children(self.text_row("whois_nickname", Some(info.nickname.clone())))
            .children(self.text_row("whois_user_host", user_host))
            .children(self.text_row("whois_real_name", info.realname.clone()))
            .children(self.text_row("whois_account", info.account.clone()));
        if !info.channels.is_empty() {
            details = details.child(self.row("whois_channels", self.channel_list(cx)));
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
            .bg(rgb(0xf5f6f8))
            .text_size(px(13.))
            .text_color(rgb(0x20262d))
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, _| {
                if event.keystroke.key == "escape" {
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
                            .text_color(rgb(0x9a4b28))
                            .children(self.status.clone()),
                    )
                    .child(
                        button(
                            "whois-private-message",
                            self.i18n.text("member_private_message"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.private_message(cx))),
                    )
                    .child(
                        button("whois-update", self.i18n.text("whois_update"), false)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        button("whois-close", self.i18n.text("whois_close"), true)
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
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
