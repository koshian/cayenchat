// SPDX-License-Identifier: GPL-3.0-only
//! A horizontal slider for a whole number from 0 to a maximum (#950): the
//! value is set by a click or a drag on the track, and by the arrow, Home,
//! End and Page keys while it has the keyboard focus.

use std::{cell::Cell, rc::Rc};

use gpui::{
    AppContext, Bounds, Context, CursorStyle, Div, DragMoveEvent, ElementId, FocusHandle,
    InteractiveElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Rgba, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas, div, px, relative,
};

use crate::splitter::NoGhost;

/// Diameter of the knob; the track keeps half of it free at both ends so the
/// knob never leaves the element.
const KNOB: f32 = 14.;
/// Pages move by this many steps.
const PAGE: u32 = 10;

/// What a drag carries: which slider it belongs to.
struct Drag {
    id: ElementId,
}

/// The value under the pointer at `x`, for a track from `left` of `width`
/// pixels with values 0 to `max`.
pub(crate) fn value_at(x: f32, left: f32, width: f32, max: u32) -> u32 {
    if width <= 0. {
        return 0;
    }
    let fraction = ((x - left) / width).clamp(0., 1.);
    (fraction * max as f32).round() as u32
}

/// The value after `key` from `value`, or `None` when the key is not one the
/// slider handles.
pub(crate) fn after_key(key: &str, value: u32, max: u32) -> Option<u32> {
    Some(match key {
        "left" | "down" => value.saturating_sub(1),
        "right" | "up" => (value + 1).min(max),
        "pagedown" => value.saturating_sub(PAGE),
        "pageup" => (value + PAGE).min(max),
        "home" => 0,
        "end" => max,
        _ => return None,
    })
}

/// The colors a slider is drawn with.
#[derive(Clone, Copy)]
pub(crate) struct SliderColors {
    pub(crate) track: Rgba,
    pub(crate) fill: Rgba,
    pub(crate) knob: Rgba,
    /// The knob's border while the slider has the focus.
    pub(crate) ring: Rgba,
}

/// The track's measured bounds, filled in at paint time and read by the
/// pointer handlers. Keep one per slider.
pub(crate) type TrackBounds = Rc<Cell<Bounds<Pixels>>>;

/// A slider showing `value` of 0 to `max`. `on_change` gets each new value,
/// from the pointer and from the keys, only when it differs from `value`.
#[allow(clippy::too_many_arguments)] // Each is one independent slider property.
pub(crate) fn slider<V: 'static>(
    id: impl Into<ElementId>,
    value: u32,
    max: u32,
    track: &TrackBounds,
    focus: &FocusHandle,
    colors: SliderColors,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, u32, &mut Window, &mut Context<V>) + 'static,
) -> Stateful<Div> {
    let id: ElementId = id.into();
    let fraction = if max == 0 {
        0.
    } else {
        (value.min(max) as f32) / max as f32
    };
    let on_change = Rc::new(on_change);
    let set = {
        let on_change = on_change.clone();
        move |view: &mut V, new: u32, window: &mut Window, cx: &mut Context<V>| {
            if new != value {
                on_change(view, new, window, cx);
                cx.notify();
            }
        }
    };
    let selector = id.to_string();
    let at = {
        let track = track.clone();
        move |x: Pixels| {
            let bounds = track.get();
            value_at(
                f32::from(x),
                f32::from(bounds.origin.x),
                f32::from(bounds.size.width),
                max,
            )
        }
    };
    let measure = track.clone();
    let line = div()
        .relative()
        .w_full()
        .h(px(KNOB))
        .flex()
        .items_center()
        .child(
            canvas(move |bounds, _, _| measure.set(bounds), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(
            div()
                .w_full()
                .h(px(4.))
                .rounded(px(2.))
                .bg(colors.track)
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction))
                        .rounded(px(2.))
                        .bg(colors.fill),
                ),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .left(relative(fraction))
                .ml(px(-KNOB / 2.))
                .size(px(KNOB))
                .rounded_full()
                .border_2()
                .border_color(colors.fill)
                .bg(colors.knob),
        );
    let set_down = set.clone();
    let at_down = at.clone();
    let set_key = set.clone();
    let set_drag = set;
    let drag_id = id.clone();
    div()
        .id(id.clone())
        .debug_selector(move || selector)
        .track_focus(focus)
        .focus(move |style| style.border_color(colors.ring))
        .w_full()
        .h(px(KNOB + 8.))
        .px(px(KNOB / 2. + 2.))
        .flex()
        .items_center()
        .border_1()
        .border_color(gpui::transparent_black())
        .rounded(px(4.))
        .cursor(CursorStyle::PointingHand)
        .child(line)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                set_down(view, at_down(event.position.x), window, cx);
            }),
        )
        .on_drag(Drag { id: id.clone() }, |_, _, _, cx| cx.new(|_| NoGhost))
        .on_drag_move(
            cx.listener(move |view, event: &DragMoveEvent<Drag>, window, cx| {
                if event.drag(cx).id != drag_id {
                    return;
                }
                // GPUI ends a drag only on MouseUp, which can be lost; the
                // splitter's workaround applies here as well.
                if event.event.pressed_button != Some(MouseButton::Left) {
                    cx.stop_active_drag(window);
                    return;
                }
                set_drag(view, at(event.event.position.x), window, cx);
            }),
        )
        .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.modifiers.modified() {
                return;
            }
            if let Some(new) = after_key(keystroke.key.as_str(), value, max) {
                cx.stop_propagation();
                set_key(view, new, window, cx);
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_maps_to_the_nearest_value_and_stays_in_range() {
        assert_eq!(value_at(100., 100., 200., 100), 0);
        assert_eq!(value_at(200., 100., 200., 100), 50);
        assert_eq!(value_at(300., 100., 200., 100), 100);
        assert_eq!(value_at(-50., 100., 200., 100), 0);
        assert_eq!(value_at(900., 100., 200., 20), 20);
        // 5 of 20 steps over 200 px: 10 px per step, rounded.
        assert_eq!(value_at(154., 100., 200., 20), 5);
        assert_eq!(value_at(10., 0., 0., 20), 0);
    }

    #[test]
    fn keys_step_and_stop_at_the_ends() {
        assert_eq!(after_key("left", 5, 20), Some(4));
        assert_eq!(after_key("left", 0, 20), Some(0));
        assert_eq!(after_key("right", 20, 20), Some(20));
        assert_eq!(after_key("pageup", 15, 20), Some(20));
        assert_eq!(after_key("pagedown", 4, 20), Some(0));
        assert_eq!(after_key("home", 7, 20), Some(0));
        assert_eq!(after_key("end", 7, 20), Some(20));
        assert_eq!(after_key("tab", 7, 20), None);
    }
}
