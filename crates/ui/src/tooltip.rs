// SPDX-License-Identifier: GPL-3.0-only
//! Shared tooltip views and their pointer offset.

use gpui::{prelude::*, *};

use crate::settings_theme;

/// Offset from the pointer to the tooltip, including the gap the pointer can
/// cross before reaching the visible bubble.
pub(crate) const TOOLTIP_OFFSET: (Pixels, Pixels) = (px(10.), px(18.));

/// A plain text tooltip in the platform's tooltip style.
pub(crate) struct TextTooltip(pub(crate) SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .pl(TOOLTIP_OFFSET.0)
            .pt(TOOLTIP_OFFSET.1)
            .child(settings_theme::tooltip(self.0.clone(), cx))
    }
}

/// The look shared by the hover popups in the logs (the full URL and the
/// character reference original): the tooltip layout with the appearance
/// colors. Only the background is translucent, so text and buttons stay
/// readable.
fn hover_popup(text: SharedString, cx: &App) -> Div {
    let theme = crate::theme::current(cx);
    settings_theme::tooltip(text, cx)
        .bg(theme.panes.url_tooltip)
        .text_color(theme.panes.url_tooltip_text)
        .border_color(hsla(0., 0., 1., 0.2))
}

/// The popup showing a shortened URL in full. It is drawn in the link
/// color, and a click on it opens the URL.
pub(crate) struct UrlTooltip(pub(crate) SharedString);

impl Render for UrlTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::current(cx);
        let url = self.0.clone();
        // Only the visible bubble is clickable; the offset lies over the log
        // text and must leave it usable.
        div().pl(TOOLTIP_OFFSET.0).pt(TOOLTIP_OFFSET.1).child(
            hover_popup(self.0.clone(), cx)
                .id("url-tooltip")
                .text_color(theme.panes.url_tooltip_link)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| cx.open_url(&url)),
        )
    }
}

/// The bubble over decoded character references: where they came from, the
/// text as received, and the only copy button (the original; the characters
/// shown are copied by selecting them).
pub(crate) struct CharRefTooltip {
    original: SharedString,
    title: SharedString,
    copy_label: SharedString,
}

impl CharRefTooltip {
    pub(crate) fn new(original: SharedString, title: String, copy_label: String) -> Self {
        Self {
            original,
            title: title.into(),
            copy_label: copy_label.into(),
        }
    }
}

impl Render for CharRefTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::current(cx);
        let original = self.original.clone();
        div().pl(TOOLTIP_OFFSET.0).pt(TOOLTIP_OFFSET.1).child(
            hover_popup(SharedString::default(), cx)
                .id("char-ref-tooltip")
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(self.title.clone())
                        .child(
                            div()
                                .debug_selector(|| "char-ref-original".into())
                                .child(self.original.clone()),
                        )
                        .child(
                            div()
                                .id("char-ref-copy")
                                .debug_selector(|| "char-ref-copy".into())
                                .px(px(6.))
                                .py(px(1.))
                                .rounded(px(3.))
                                .border_1()
                                .border_color(hsla(0., 0., 1., 0.3))
                                .text_color(theme.panes.url_tooltip_link)
                                .cursor_pointer()
                                .hover(|d| d.bg(hsla(0., 0., 1., 0.12)))
                                .child(self.copy_label.clone())
                                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        original.to_string(),
                                    ));
                                }),
                        ),
                ),
        )
    }
}
