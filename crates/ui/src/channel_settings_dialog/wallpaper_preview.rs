// SPDX-License-Identifier: GPL-3.0-only
//! The miniature of the channel log in the channel settings (#989), the same
//! look and feel as the four-area miniature of the Appearance tab: the
//! wallpaper the channel shows (its own, the standard one, or none) under the
//! log's background color, a drop target for an image and a "Choose image…"
//! button. The picture and the colors come from the chat window's own
//! resolution (`channel_look::resolve`) and its loading
//! (`channel_look::Wallpaper`); an image ends in the same path as the file
//! field and a drop on the window (`offer_wallpaper`).

use gpui::{prelude::*, *};

use super::ChannelSettingsView;
use crate::{channel_look, settings_theme};

impl ChannelSettingsView {
    pub(super) fn render_wallpaper_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let palette = settings_theme::palette(cx);
        let mut theme = crate::theme::current(cx);
        // Opaque: the miniature shows the mix of picture and color, which the
        // pane's opacity (the desktop showing through) does not change.
        theme.panes.channel_log.a = 1.;
        let standard = self
            .owner
            .read(cx)
            .map(|chat| chat.appearance.wallpapers(theme.dark).channel_log.clone())
            .unwrap_or_default();
        let look = self.look(cx).ok();
        let resolved = channel_look::resolve(look.as_ref(), &standard, theme);
        let picture = self.miniature.element(
            resolved.wallpaper.as_ref(),
            resolved.background,
            false,
            window,
            cx,
            |this: &mut Self| &mut this.miniature,
        );
        let ring = settings_theme::current(cx).map_or(palette.text, |native| {
            settings_theme::color(native.defaults.focus_ring_color)
        });
        let drop_ring = theme.text;
        let hover = palette.selected;
        let text = resolved.text.unwrap_or(resolved.theme.text);
        div()
            .id("channel-look-preview")
            .debug_selector(|| "channel-look-preview".into())
            .relative()
            .overflow_hidden()
            .w_full()
            .h(px(110.))
            .p_1()
            .border_2()
            .border_color(palette.border)
            .when(picture.is_none(), |d| {
                d.bg(resolved.theme.panes.channel_log)
            })
            .text_color(text)
            .text_size(px(12.))
            // Only this area is marked while an image is dragged over it.
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                style.border_color(drop_ring).bg(hover)
            })
            .on_drop(
                cx.listener(|this, paths: &ExternalPaths, _, cx| this.on_wallpaper_drop(paths, cx)),
            )
            .children(picture)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.i18n.text("channel_look_preview_sample"))
                    .when(resolved.wallpaper.is_none(), |d| {
                        d.child(
                            div()
                                .text_color(theme.text_muted)
                                .child(self.i18n.text("wallpaper_none")),
                        )
                    })
                    .child(
                        settings_theme::button("channel-look-preview-choose", false, cx)
                            .track_focus(&self.controls.choose)
                            .focus(move |style| style.border_color(ring))
                            .text_size(px(11.))
                            .child(self.i18n.text("wallpaper_choose"))
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.browse_wallpaper(window, cx)
                                }),
                            ),
                    ),
            )
    }
}
