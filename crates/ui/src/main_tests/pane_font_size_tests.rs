// SPDX-License-Identifier: GPL-3.0-only
use super::{LIST_PANE_TEXT_SIZE, pane_line_height, pane_text_size};
use gpui::px;

#[test]
fn unset_size_is_the_standard_one() {
    assert_eq!(pane_text_size(0, None), None);
    assert_eq!(
        pane_text_size(0, Some(LIST_PANE_TEXT_SIZE)),
        Some(px(LIST_PANE_TEXT_SIZE))
    );
}

#[test]
fn saved_size_wins_and_is_clamped() {
    assert_eq!(pane_text_size(18, Some(LIST_PANE_TEXT_SIZE)), Some(px(18.)));
    assert_eq!(pane_text_size(1, None), Some(px(8.)));
    assert_eq!(pane_text_size(999, None), Some(px(40.)));
}

#[test]
fn unset_line_height_is_the_default_one() {
    assert_eq!(pane_line_height(0), None);
}

#[test]
fn saved_line_height_is_a_ratio_and_is_clamped() {
    assert_eq!(pane_line_height(150), Some(1.5));
    assert_eq!(pane_line_height(50), Some(1.0));
    assert_eq!(pane_line_height(999), Some(2.0));
}
