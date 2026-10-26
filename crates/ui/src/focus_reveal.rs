// SPDX-License-Identifier: GPL-3.0-only
//! Scrolls a settings pane so that the field reached with Tab stays visible.
//!
//! A focused field reports its painted bounds here (one slot, rewritten only
//! when they change). After the frame that followed a Tab, the settings
//! window reads the slot and moves its scroll handle by the least amount that
//! shows the field; a field that is already visible leaves the pane alone.

use gpui::{App, Bounds, FocusHandle, Global, Pixels, Window, px};

/// Room kept between the field and the pane edge when it has to scroll.
const MARGIN: Pixels = px(8.);

#[derive(Default)]
struct FocusedBounds(Option<(FocusHandle, Bounds<Pixels>)>);

impl Global for FocusedBounds {}

/// Records `bounds` as the painted place of `handle` while it has the focus.
pub(crate) fn note(handle: &FocusHandle, bounds: Bounds<Pixels>, window: &Window, cx: &mut App) {
    if !handle.is_focused(window) {
        return;
    }
    let entry = Some((handle.clone(), bounds));
    if cx.default_global::<FocusedBounds>().0 != entry {
        cx.set_global(FocusedBounds(entry));
    }
}

/// The painted bounds of the focused field, when it is the one with the focus.
pub(crate) fn focused_bounds(window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
    let (handle, bounds) = cx.try_global::<FocusedBounds>()?.0.as_ref()?;
    (window.focused(cx)? == *handle).then_some(*bounds)
}

/// The scroll offset (zero or negative, `max` the most it can scroll) that
/// shows `target` inside `viewport` with the least movement. A target taller
/// than the viewport is aligned to its top.
pub(crate) fn offset_to_reveal(
    viewport: Bounds<Pixels>,
    target: Bounds<Pixels>,
    offset: Pixels,
    max: Pixels,
) -> Pixels {
    let top = viewport.top() + MARGIN;
    let bottom = viewport.bottom() - MARGIN;
    let delta = if target.top() < top {
        top - target.top()
    } else if target.bottom() > bottom {
        (bottom - target.bottom()).max(top - target.top())
    } else {
        return offset;
    };
    (offset + delta).clamp(-max.max(px(0.)), px(0.))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn bounds(top: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(top)), size(px(100.), px(height)))
    }

    #[test]
    fn visible_target_does_not_scroll() {
        let offset = offset_to_reveal(bounds(0., 400.), bounds(100., 30.), px(-50.), px(500.));
        assert_eq!(offset, px(-50.));
    }

    #[test]
    fn target_below_scrolls_just_enough() {
        let offset = offset_to_reveal(bounds(0., 400.), bounds(390., 30.), px(0.), px(500.));
        assert_eq!(offset, px(-28.));
    }

    #[test]
    fn target_above_scrolls_just_enough() {
        let offset = offset_to_reveal(bounds(0., 400.), bounds(-20., 30.), px(-100.), px(500.));
        assert_eq!(offset, px(-72.));
    }

    #[test]
    fn offset_stays_within_the_scrollable_range() {
        let offset = offset_to_reveal(bounds(0., 400.), bounds(-20., 30.), px(-4.), px(500.));
        assert_eq!(offset, px(0.));
        let offset = offset_to_reveal(bounds(0., 400.), bounds(390., 30.), px(-490.), px(500.));
        assert_eq!(offset, px(-500.));
    }

    #[test]
    fn tall_target_aligns_to_its_top() {
        let offset = offset_to_reveal(bounds(0., 100.), bounds(50., 300.), px(0.), px(500.));
        assert_eq!(offset, px(-42.));
    }
}
