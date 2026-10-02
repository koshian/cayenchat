//! A draggable boundary between two panes.
//!
//! The handle is a thin element that reports the size its pane would have
//! when dragged; the owner stores the size and lays the panes out with it.
//! Nothing is drawn or measured here beyond the handle itself, and sizes stay
//! within the limits the owner gives, so a pane can never become unusable.

use std::{cell::Cell, ops::RangeInclusive};

use gpui::{
    AppContext, Context, CursorStyle, DragMoveEvent, ElementId, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Render, Rgba, StatefulInteractiveElement, Styled, Window, div, px,
};

/// Which way the handle moves: `Horizontal` sits between side-by-side panes
/// and changes widths, `Vertical` sits between stacked panes and changes
/// heights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Which side of the handle the pane being sized is on: dragging away from
/// the pane makes it larger when it is `Before` (left or top) and smaller
/// when it is `After` (right or bottom).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Before,
    After,
}

/// Width of the grabbable strip, in pixels. The line drawn inside is 1 px.
pub const HANDLE_THICKNESS: f32 = 5.;

/// The pane size after the pointer moved from `start` to `now` (both along
/// the handle's axis, in pixels) while the pane was `start_size`, kept within
/// `limits`.
pub fn resized(
    start_size: f32,
    start: f32,
    now: f32,
    anchor: Anchor,
    limits: &RangeInclusive<f32>,
) -> f32 {
    let delta = now - start;
    let size = match anchor {
        Anchor::Before => start_size + delta,
        Anchor::After => start_size - delta,
    };
    size.clamp(*limits.start(), *limits.end())
}

/// What the drag carries. GPUI keeps the value from the moment the drag
/// starts, so the starting size and pointer stay fixed while the pane moves.
struct Drag {
    axis: Axis,
    anchor: Anchor,
    id: ElementId,
    start_size: f32,
    /// Pointer coordinate where the button went down, read when the drag
    /// starts.
    start: Cell<Option<f32>>,
}

thread_local! {
    /// Where the button last went down on a handle. GPUI starts a drag only
    /// after the pointer has moved a little, and rebuilds the elements every
    /// frame, so the press cannot live in the handle; measuring from it keeps
    /// the handle under the pointer. There is one pointer, hence one value.
    static PRESS: Cell<Option<f32>> = const { Cell::new(None) };
}

/// The view GPUI draws at the pointer while dragging: nothing.
pub(crate) struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn along(axis: Axis, point: gpui::Point<gpui::Pixels>) -> f32 {
    match axis {
        Axis::Horizontal => f32::from(point.x),
        Axis::Vertical => f32::from(point.y),
    }
}

/// A handle for a pane currently `size` pixels long, within `limits`. While
/// it is dragged, `on_resize` gets the new size; it is called again for
/// every pointer move and the window redraws afterwards.
pub fn splitter<V: 'static>(
    id: impl Into<ElementId>,
    axis: Axis,
    anchor: Anchor,
    size: f32,
    limits: RangeInclusive<f32>,
    line: Rgba,
    cx: &mut Context<V>,
    on_resize: impl Fn(&mut V, f32, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let id: ElementId = id.into();
    let drag = Drag {
        axis,
        anchor,
        id: id.clone(),
        start_size: size,
        start: Cell::new(None),
    };
    let handle = div()
        .id(id.clone())
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center();
    let handle = match axis {
        Axis::Horizontal => handle
            .w(px(HANDLE_THICKNESS))
            .h_full()
            .cursor(CursorStyle::ResizeLeftRight)
            .child(div().w(px(1.)).h_full().bg(line)),
        Axis::Vertical => handle
            .h(px(HANDLE_THICKNESS))
            .w_full()
            .flex_col()
            .cursor(CursorStyle::ResizeUpDown)
            .child(div().h(px(1.)).w_full().bg(line)),
    };
    handle
        .on_mouse_down(MouseButton::Left, move |event, _, _| {
            PRESS.with(|press| press.set(Some(along(axis, event.position))));
        })
        .on_drag(drag, |drag, _, window, cx| {
            let pressed = PRESS.with(Cell::take);
            drag.start.set(Some(
                pressed.unwrap_or_else(|| along(drag.axis, window.mouse_position())),
            ));
            cx.new(|_| NoGhost)
        })
        .on_drag_move(
            cx.listener(move |view, event: &DragMoveEvent<Drag>, window, cx| {
                let drag = event.drag(cx);
                if drag.id != id {
                    return;
                }
                let Some(start) = drag.start.get() else {
                    return;
                };
                let new = resized(
                    drag.start_size,
                    start,
                    along(drag.axis, event.event.position),
                    drag.anchor,
                    &limits,
                );
                on_resize(view, new, window, cx);
                cx.notify();
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_before_the_handle_grows_with_the_pointer() {
        let limits = 100.0..=400.0;
        assert_eq!(resized(200., 500., 530., Anchor::Before, &limits), 230.);
        assert_eq!(resized(200., 500., 470., Anchor::Before, &limits), 170.);
    }

    #[test]
    fn a_pane_after_the_handle_grows_against_the_pointer() {
        let limits = 100.0..=400.0;
        assert_eq!(resized(200., 500., 470., Anchor::After, &limits), 230.);
        assert_eq!(resized(200., 500., 530., Anchor::After, &limits), 170.);
    }

    #[test]
    fn sizes_stay_within_the_limits() {
        let limits = 100.0..=400.0;
        assert_eq!(resized(200., 0., -5_000., Anchor::Before, &limits), 100.);
        assert_eq!(resized(200., 0., 5_000., Anchor::Before, &limits), 400.);
        assert_eq!(resized(200., 0., 5_000., Anchor::After, &limits), 100.);
        assert_eq!(resized(200., 0., -5_000., Anchor::After, &limits), 400.);
    }

    struct Panes {
        width: f32,
        resizes: usize,
    }

    impl Render for Panes {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .child(div().w(px(self.width)).h_full())
                .child(splitter(
                    "panes",
                    Axis::Horizontal,
                    Anchor::Before,
                    self.width,
                    120.0..=300.0,
                    gpui::rgb(0x888888),
                    cx,
                    |view: &mut Panes, width, _, _| {
                        view.width = width;
                        view.resizes += 1;
                    },
                ))
                .child(div().flex_1().h_full())
        }
    }

    #[gpui::test]
    fn dragging_the_handle_resizes_within_the_limits(cx: &mut gpui::TestAppContext) {
        let (panes, cx) = cx.add_window_view(|_, _| Panes {
            width: 200.,
            resizes: 0,
        });
        cx.run_until_parked();
        let at = |x: f32| gpui::point(px(x), px(50.));
        // The handle starts right after the 200 px pane; the pointer reaches
        // it before pressing, as with a real mouse.
        cx.simulate_mouse_move(at(202.), None, gpui::Modifiers::none());
        cx.simulate_mouse_down(at(202.), gpui::MouseButton::Left, gpui::Modifiers::none());
        for x in [210., 230., 252.] {
            cx.simulate_mouse_move(at(x), gpui::MouseButton::Left, gpui::Modifiers::none());
            cx.run_until_parked();
        }
        let width = panes.read_with(cx, |panes, _| panes.width);
        assert!((248.0..=252.0).contains(&width), "dragged to {width}");
        cx.simulate_mouse_move(at(900.), gpui::MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(panes.read_with(cx, |panes, _| panes.width), 300.);
        cx.simulate_mouse_move(at(0.), gpui::MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(panes.read_with(cx, |panes, _| panes.width), 120.);
        cx.simulate_mouse_up(at(0.), gpui::MouseButton::Left, gpui::Modifiers::none());
        // Moving without a button changes nothing.
        let resizes = panes.read_with(cx, |panes, _| panes.resizes);
        cx.simulate_mouse_move(at(250.), None, gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(panes.read_with(cx, |panes, _| panes.resizes), resizes);
    }

    #[test]
    fn no_movement_keeps_the_size() {
        let limits = 100.0..=400.0;
        assert_eq!(resized(250., 42., 42., Anchor::Before, &limits), 250.);
        assert_eq!(resized(250., 42., 42., Anchor::After, &limits), 250.);
    }
}
