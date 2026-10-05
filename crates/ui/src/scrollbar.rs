//! A thin overlay scrollbar for vertically scrolling panes.
//!
//! GPUI scrolls with the wheel only and draws no bar. This paints a thumb at
//! the right edge of the pane it is placed in (the pane's container must be
//! `relative`), shows how much is scrolled, and lets the pointer drag the
//! thumb or click the track to jump. It reads and writes the scroll position
//! through the same handle the pane scrolls with, so nothing else changes.
//! The bar is hidden while everything fits.

use std::{cell::RefCell, rc::Rc};

use gpui::{
    Bounds, Canvas, CursorStyle, DispatchPhase, ElementId, HitboxBehavior, ListState, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Point, Rgba, ScrollHandle, Styled,
    UniformListScrollHandle, canvas, fill, point, px,
};

/// Width of the grabbable strip, in pixels.
const TRACK_WIDTH: f32 = 10.;
/// Width of the drawn thumb, centered in the strip.
const THUMB_WIDTH: f32 = 6.;
/// The thumb never gets shorter than this, however long the content is.
const MIN_THUMB: f32 = 24.;

/// What the bar scrolls.
#[derive(Clone)]
pub enum Source {
    /// A virtualized `list`, such as the logs and the channel tree.
    List(ListState),
    /// A plain `overflow_y_scroll` element tracked with `track_scroll`.
    Handle(ScrollHandle),
}

impl From<&ListState> for Source {
    fn from(state: &ListState) -> Self {
        Self::List(state.clone())
    }
}

impl From<&ScrollHandle> for Source {
    fn from(handle: &ScrollHandle) -> Self {
        Self::Handle(handle.clone())
    }
}

impl From<&UniformListScrollHandle> for Source {
    fn from(handle: &UniformListScrollHandle) -> Self {
        Self::Handle(handle.0.borrow().base_handle.clone())
    }
}

/// Visible height, farthest scroll distance and current scroll distance.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    viewport: f32,
    max: f32,
    offset: f32,
}

impl Source {
    fn metrics(&self) -> Metrics {
        let (viewport, max, offset) = match self {
            Self::List(state) => (
                state.viewport_bounds().size.height,
                state.max_offset_for_scrollbar().height,
                -state.scroll_px_offset_for_scrollbar().y,
            ),
            Self::Handle(handle) => (
                handle.bounds().size.height,
                handle.max_offset().height,
                -handle.offset().y,
            ),
        };
        Metrics {
            viewport: f32::from(viewport),
            max: f32::from(max),
            offset: f32::from(offset),
        }
    }

    fn scroll_to(&self, offset: f32) {
        match self {
            Self::List(state) => state.set_offset_from_scrollbar(point(px(0.), px(-offset))),
            Self::Handle(handle) => {
                let current = handle.offset();
                handle.set_offset(point(current.x, px(-offset)));
            }
        }
    }

    fn drag_started(&self) {
        if let Self::List(state) = self {
            state.scrollbar_drag_started();
        }
    }

    fn drag_ended(&self) {
        if let Self::List(state) = self {
            state.scrollbar_drag_ended();
        }
    }
}

/// Top and height of the thumb inside a track `track` pixels tall.
fn thumb(track: f32, metrics: Metrics) -> (f32, f32) {
    let content = metrics.viewport + metrics.max;
    let height = (track * metrics.viewport / content)
        .max(MIN_THUMB)
        .min(track);
    let travel = track - height;
    let top = travel * (metrics.offset / metrics.max).clamp(0., 1.);
    (top, height)
}

/// The scroll distance for a thumb whose top is `top` pixels down the track.
fn offset_for_thumb(track: f32, thumb_height: f32, top: f32, max: f32) -> f32 {
    let travel = track - thumb_height;
    if travel <= 0. {
        return 0.;
    }
    max * (top / travel).clamp(0., 1.)
}

/// A thumb being dragged: which bar, and how far below the thumb's top the
/// pointer is held.
struct Drag {
    id: ElementId,
    grab: f32,
}

thread_local! {
    /// There is one pointer, hence at most one drag. The bar is rebuilt every
    /// frame, so the drag cannot live in it.
    static DRAG: RefCell<Option<Drag>> = const { RefCell::new(None) };
}

fn dragging(id: &ElementId) -> Option<f32> {
    DRAG.with(|drag| {
        drag.borrow()
            .as_ref()
            .filter(|drag| &drag.id == id)
            .map(|drag| drag.grab)
    })
}

/// A scrollbar for `source`, drawn in `color` (its alpha is raised while the
/// pointer is over it or dragging). Place it as the last child of a
/// `relative` container that also holds the scrolling pane.
pub fn scrollbar(id: impl Into<ElementId>, source: impl Into<Source>, color: Rgba) -> Canvas<()> {
    let id: ElementId = id.into();
    let source: Source = source.into();
    let hitbox = Rc::new(RefCell::new(None));
    let prepaint_hitbox = hitbox.clone();
    let prepaint_source = source.clone();
    canvas(
        move |bounds, window, _| {
            let metrics = prepaint_source.metrics();
            // Nothing to scroll: leave the pane's events alone.
            *prepaint_hitbox.borrow_mut() = (metrics.max > 0.5)
                .then(|| window.insert_hitbox(bounds, HitboxBehavior::BlockMouseExceptScroll));
        },
        move |bounds, (), window, _| {
            let Some(hitbox) = hitbox.borrow_mut().take() else {
                return;
            };
            let metrics = source.metrics();
            let track = f32::from(bounds.size.height);
            let (top, height) = thumb(track, metrics);
            let active = hitbox.is_hovered(window) || dragging(&id).is_some();
            if active {
                window.set_window_cursor_style(CursorStyle::Arrow);
            }
            let mut tint = color;
            tint.a = if active { 0.8 } else { 0.45 };
            let left = bounds.right() - px((TRACK_WIDTH + THUMB_WIDTH) / 2.);
            let mut quad = fill(
                Bounds::new(
                    Point::new(left, bounds.top() + px(top)),
                    gpui::size(px(THUMB_WIDTH), px(height)),
                ),
                tint,
            );
            quad.corner_radii = px(THUMB_WIDTH / 2.).into();
            window.paint_quad(quad);

            window.on_mouse_event({
                let (id, source, hitbox) = (id.clone(), source.clone(), hitbox.clone());
                move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.button != MouseButton::Left
                        || !hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let metrics = source.metrics();
                    let track = f32::from(bounds.size.height);
                    let (top, height) = thumb(track, metrics);
                    let pointer = f32::from(event.position.y - bounds.top());
                    // On the thumb keep the grip; on the track jump so the
                    // thumb is centered under the pointer.
                    let grab = if (top..=top + height).contains(&pointer) {
                        pointer - top
                    } else {
                        height / 2.
                    };
                    source.drag_started();
                    source.scroll_to(offset_for_thumb(track, height, pointer - grab, metrics.max));
                    DRAG.with(|drag| {
                        *drag.borrow_mut() = Some(Drag {
                            id: id.clone(),
                            grab,
                        })
                    });
                    window.refresh();
                    cx.stop_propagation();
                }
            });
            window.on_mouse_event({
                let (id, source) = (id.clone(), source.clone());
                move |event: &MouseMoveEvent, phase, window, _| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    let Some(grab) = dragging(&id) else {
                        return;
                    };
                    let metrics = source.metrics();
                    let track = f32::from(bounds.size.height);
                    let (_, height) = thumb(track, metrics);
                    let pointer = f32::from(event.position.y - bounds.top());
                    source.scroll_to(offset_for_thumb(track, height, pointer - grab, metrics.max));
                    window.refresh();
                }
            });
            window.on_mouse_event({
                let (id, source) = (id.clone(), source.clone());
                move |_: &MouseUpEvent, phase, window, _| {
                    if phase != DispatchPhase::Capture || dragging(&id).is_none() {
                        return;
                    }
                    DRAG.with(|drag| *drag.borrow_mut() = None);
                    source.drag_ended();
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .top_0()
    .right_0()
    .bottom_0()
    .w(px(TRACK_WIDTH))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(viewport: f32, max: f32, offset: f32) -> Metrics {
        Metrics {
            viewport,
            max,
            offset,
        }
    }

    #[test]
    fn thumb_length_is_the_visible_share_of_the_content() {
        // Half of the content is visible.
        assert_eq!(thumb(200., metrics(200., 200., 0.)), (0., 100.));
    }

    #[test]
    fn thumb_reaches_the_bottom_at_the_end() {
        let (top, height) = thumb(200., metrics(200., 200., 200.));
        assert_eq!(top + height, 200.);
    }

    #[test]
    fn thumb_has_a_minimum_length() {
        let (_, height) = thumb(200., metrics(200., 1_000_000., 0.));
        assert_eq!(height, MIN_THUMB);
    }

    #[test]
    fn dragging_maps_the_thumb_back_to_a_scroll_distance() {
        let (_, height) = thumb(200., metrics(200., 200., 0.));
        assert_eq!(offset_for_thumb(200., height, 0., 200.), 0.);
        assert_eq!(offset_for_thumb(200., height, 50., 200.), 100.);
        assert_eq!(offset_for_thumb(200., height, 5_000., 200.), 200.);
        assert_eq!(offset_for_thumb(200., height, -5_000., 200.), 0.);
    }
}
