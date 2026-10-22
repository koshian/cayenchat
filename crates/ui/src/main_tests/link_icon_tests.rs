// SPDX-License-Identifier: GPL-3.0-only
use super::{
    compact_urls::{self, ICON},
    link_icon_origin,
};
use gpui::{
    Context, IntoElement, ParentElement, Render, Styled, StyledText, TextLayout, Window, div, px,
};

struct Text {
    text: String,
    layout: TextLayout,
}

impl Render for Text {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let styled = StyledText::new(self.text.clone());
        self.layout = styled.layout().clone();
        div().w(px(120.)).child(styled)
    }
}

/// The icon is painted at the row that holds the room it covers, also when the
/// wrap falls right before the room (the cursor index then reports the end of
/// the previous row) and when the room is in the middle of a row.
#[gpui::test]
fn icon_follows_its_room_across_a_wrap(cx: &mut gpui::TestAppContext) {
    check_rooms_across_wraps(cx, " ");
}

/// The same holds when the text before the URL has no space, so no ordinary
/// wrap opportunity exists and only the start of the room can take the break.
#[gpui::test]
fn icon_follows_its_room_after_an_unspaced_prefix(cx: &mut gpui::TestAppContext) {
    check_rooms_across_wraps(cx, "");
}

fn check_rooms_across_wraps(cx: &mut gpui::TestAppContext, separator: &str) {
    let url = "https://example.com/a";
    let mut text = String::new();
    let mut starts = Vec::new();
    // Rows of different fill make some URL start at a wrap boundary.
    for filler in 0..40 {
        text.push_str(&"w".repeat(filler));
        text.push_str(separator);
        starts.push(text.len());
        text.push_str(&format!("{ICON}{url} "));
    }
    let (view, cx) = cx.add_window_view(|_, _| Text {
        text: text.clone(),
        layout: TextLayout::default(),
    });
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        let layout = &view.layout;
        let mut wrapped = 0;
        for start in starts {
            let origin = link_icon_origin(layout, start).expect("painted");
            // The room is followed by the URL's first letter on the same row.
            let body = layout
                .position_for_index(start + ICON.len() + 1)
                .expect("painted");
            assert_eq!(origin.y, body.y, "icon row for room at {start}");
            assert!(origin.x < body.x, "icon is left of its URL at {start}");
            // The room is never split by a wrap, and the painted icon ends
            // before the URL's first letter.
            for part in [compact_urls::LEAD.len(), 5, 8, 11] {
                let at = layout.position_for_index(start + part).expect("painted");
                assert_eq!(at.y, body.y, "room split at {part} for room at {start}");
            }
            let first = layout
                .position_for_index(start + ICON.len())
                .expect("painted");
            let icon_end = origin.x + layout.line_height() * 0.65;
            assert!(icon_end <= first.x, "icon covers the URL at {start}");
            let plain = layout.position_for_index(start).expect("painted");
            wrapped += usize::from(plain.y != origin.y);
        }
        assert!(wrapped > 0, "the text must put a room on a wrap boundary");
    });
}
