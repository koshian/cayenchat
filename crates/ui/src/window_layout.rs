// SPDX-License-Identifier: GPL-3.0-only
//! Reopening the main window where it was left.
//!
//! Only what GPUI cannot know lives here: whether a saved rectangle still
//! suits the displays that are connected now. Displays come and go (a laptop
//! undocked, a monitor unplugged, a resolution change), so a saved position is
//! used only where it still overlaps a display, and is moved and shrunk to fit
//! that display; otherwise the caller opens the window in its usual place.

use cayenchat_storage::layout::Layout;
use gpui::{Bounds, Pixels, Size, WindowBounds, point, px, size};

/// Where to open the window again.
#[derive(Debug)]
pub struct Restored {
    pub bounds: WindowBounds,
    /// Index into the displays given of the one the window goes on.
    pub display: usize,
}

/// The bounds to open the window with and their display, or `None` when
/// nothing is saved or no connected display shows any of the saved rectangle.
pub fn restored_bounds(
    layout: &Layout,
    displays: &[Bounds<Pixels>],
    min: Size<Pixels>,
) -> Option<Restored> {
    let saved = layout.window?;
    let (min_width, min_height) = (f32::from(min.width), f32::from(min.height));
    // The display showing most of the saved rectangle.
    let overlap = |display: &Bounds<Pixels>| {
        let (left, top) = (f32::from(display.left()), f32::from(display.top()));
        let (right, bottom) = (f32::from(display.right()), f32::from(display.bottom()));
        let width = (saved.x + saved.width).min(right) - saved.x.max(left);
        let height = (saved.y + saved.height).min(bottom) - saved.y.max(top);
        if width > 0. && height > 0. {
            width * height
        } else {
            0.
        }
    };
    let (index, display) = displays
        .iter()
        .enumerate()
        .map(|(index, display)| (overlap(display), index, display))
        .filter(|(area, _, _)| *area > 0.)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, index, display)| (index, *display))?;

    let (left, top) = (f32::from(display.left()), f32::from(display.top()));
    let (screen_width, screen_height) = (
        f32::from(display.size.width),
        f32::from(display.size.height),
    );
    // Not smaller than the window allows, not larger than the display.
    let width = saved.width.min(screen_width).max(min_width);
    let height = saved.height.min(screen_height).max(min_height);
    // Moved onto the display as far as it fits (a window larger than the
    // display, only possible at the minimum size, keeps its top left).
    let x = saved.x.min(left + screen_width - width).max(left);
    let y = saved.y.min(top + screen_height - height).max(top);
    let bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(height)));
    Some(Restored {
        bounds: if layout.maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        },
        display: index,
    })
}

/// Like [`restored_bounds`] for a layout whose rectangle is relative to the
/// display named by `layout.display` (macOS, where GPUI reports every display
/// at the origin and window coordinates are display-relative, #130).
/// `uuids[i]` is the UUID of `displays[i]`. The window goes on the named
/// display only; when it is gone (or the rectangle no longer overlaps it) the
/// result is `None`, the usual place, rather than coordinates that mean
/// something else on another display. A layout without a display falls back to
/// [`restored_bounds`].
pub fn restored_on_named_display(
    layout: &Layout,
    displays: &[Bounds<Pixels>],
    uuids: &[Option<String>],
    min: Size<Pixels>,
) -> Option<Restored> {
    let Some(name) = &layout.display else {
        return restored_bounds(layout, displays, min);
    };
    let index = uuids.iter().position(|uuid| {
        uuid.as_deref()
            .is_some_and(|uuid| uuid.eq_ignore_ascii_case(name))
    })?;
    let restored = restored_bounds(layout, &displays[index..=index], min)?;
    Some(Restored {
        display: index,
        ..restored
    })
}

/// How much of the owner's top (title bar and a little more) a window placed
/// below or above it overlaps.
const OWNER_OVERLAP: f32 = 48.;

/// Where a new secondary window of `wanted` size goes relative to `owner`.
///
/// Its horizontal center is the owner's. It sits a little below the owner's
/// top, or above the owner when that would leave the display, and is finally
/// moved onto the whole `display` (not its work area). A window larger than the
/// display is shrunk to it but never below `min`. `owner` and `display` must
/// be in the same coordinate space.
pub fn secondary_bounds(
    owner: Bounds<Pixels>,
    display: Bounds<Pixels>,
    wanted: Size<Pixels>,
    min: Size<Pixels>,
) -> Bounds<Pixels> {
    let (left, top) = (f32::from(display.left()), f32::from(display.top()));
    let (screen_width, screen_height) = (
        f32::from(display.size.width),
        f32::from(display.size.height),
    );
    let width = f32::from(wanted.width)
        .min(screen_width)
        .max(f32::from(min.width));
    let height = f32::from(wanted.height)
        .min(screen_height)
        .max(f32::from(min.height));
    let center = f32::from(owner.origin.x) + f32::from(owner.size.width) / 2.;
    let below = f32::from(owner.origin.y) + OWNER_OVERLAP;
    let y = if below + height <= top + screen_height {
        below
    } else {
        f32::from(owner.origin.y) + OWNER_OVERLAP - height
    };
    let x = (center - width / 2.)
        .min(left + screen_width - width)
        .max(left);
    let y = y.min(top + screen_height - height).max(top);
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_storage::layout::WindowRect;

    fn secondary(
        owner: (f32, f32, f32, f32),
        screen: (f32, f32, f32, f32),
    ) -> (f32, f32, f32, f32) {
        let rect = |(x, y, w, h): (f32, f32, f32, f32)| display(x, y, w, h);
        let b = secondary_bounds(
            rect(owner),
            rect(screen),
            size(px(900.), px(750.)),
            size(px(760.), px(540.)),
        );
        (
            f32::from(b.origin.x),
            f32::from(b.origin.y),
            f32::from(b.size.width),
            f32::from(b.size.height),
        )
    }

    #[test]
    fn a_secondary_window_is_centered_on_the_owner_a_little_below_its_top() {
        let got = secondary((500., 100., 960., 600.), (0., 0., 1920., 1080.));
        assert_eq!(got, (530., 148., 900., 750.));
    }

    #[test]
    fn a_secondary_window_goes_above_the_owner_when_below_leaves_the_display() {
        // 848 + 750 > 1100: above, its bottom 48 below the owner's top.
        let got = secondary((500., 800., 960., 300.), (0., 0., 1920., 1100.));
        assert_eq!(got, (530., 98., 900., 750.));
    }

    #[test]
    fn a_secondary_window_stays_on_the_display_at_edges_and_on_other_monitors() {
        let screens = (0., 0., 1920., 1080.);
        assert_eq!(
            secondary((0., 0., 400., 300.), screens),
            (0., 48., 900., 750.)
        );
        assert_eq!(
            secondary((1600., 700., 320., 380.), screens),
            (1020., 0., 900., 750.)
        );
        // A monitor to the left with negative coordinates.
        let got = secondary((-1200., 50., 800., 600.), (-1280., 0., 1280., 1024.));
        assert_eq!(got, (-1250., 98., 900., 750.));
        // Shrunk to a small display, but not below the minimum.
        let got = secondary((0., 0., 800., 600.), (0., 0., 800., 600.));
        assert_eq!(got, (0., 0., 800., 600.));
        let got = secondary((0., 0., 500., 400.), (0., 0., 700., 500.));
        assert_eq!(got, (0., 0., 760., 540.));
    }

    const MIN: fn() -> Size<Pixels> = || size(px(320.), px(240.));

    fn display(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    fn saved(x: f32, y: f32, width: f32, height: f32, maximized: bool) -> Layout {
        Layout {
            window: Some(WindowRect {
                x,
                y,
                width,
                height,
            }),
            maximized,
            ..Layout::default()
        }
    }

    fn windowed(restored: Option<Restored>) -> (f32, f32, f32, f32) {
        match restored.map(|restored| restored.bounds) {
            Some(WindowBounds::Windowed(b)) | Some(WindowBounds::Maximized(b)) => (
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height),
            ),
            other => panic!("expected bounds, got {other:?}"),
        }
    }

    #[test]
    fn a_rectangle_that_fits_comes_back_unchanged() {
        let screens = [display(0., 0., 1920., 1080.)];
        let got = restored_bounds(&saved(100., 80., 960., 600., false), &screens, MIN());
        assert!(matches!(
            got,
            Some(Restored {
                bounds: WindowBounds::Windowed(_),
                display: 0
            })
        ));
        assert_eq!(windowed(got), (100., 80., 960., 600.));
    }

    #[test]
    fn nothing_saved_or_no_display_overlapping_means_the_usual_place() {
        let screens = [display(0., 0., 1920., 1080.)];
        assert!(restored_bounds(&Layout::default(), &screens, MIN()).is_none());
        // The monitor it was on is gone: it was at x 2000.. beside this one.
        assert!(restored_bounds(&saved(2100., 100., 960., 600., false), &screens, MIN()).is_none());
        // Touching an edge shows nothing.
        assert!(restored_bounds(&saved(1920., 0., 960., 600., false), &screens, MIN()).is_none());
        assert!(restored_bounds(&saved(0., 0., 960., 600., false), &[], MIN()).is_none());
    }

    #[test]
    fn a_window_partly_off_screen_is_moved_back_onto_it() {
        let screens = [display(0., 0., 1920., 1080.)];
        let got = restored_bounds(&saved(1500., 800., 960., 600., false), &screens, MIN());
        assert_eq!(windowed(got), (960., 480., 960., 600.));
        let got = restored_bounds(&saved(-300., -50., 960., 600., false), &screens, MIN());
        assert_eq!(windowed(got), (0., 0., 960., 600.));
    }

    #[test]
    fn the_size_is_kept_between_the_minimum_and_the_display() {
        let screens = [display(0., 0., 1280., 800.)];
        let got = restored_bounds(&saved(0., 0., 3000., 2000., false), &screens, MIN());
        assert_eq!(windowed(got), (0., 0., 1280., 800.));
        let got = restored_bounds(&saved(100., 100., 200., 100., false), &screens, MIN());
        assert_eq!(windowed(got), (100., 100., 320., 240.));
        // A small size the user chose is not enlarged.
        let got = restored_bounds(&saved(100., 100., 400., 300., false), &screens, MIN());
        assert_eq!(windowed(got), (100., 100., 400., 300.));
        // A display smaller than the minimum keeps the window's top left.
        let tiny = [display(0., 0., 300., 200.)];
        let got = restored_bounds(&saved(10., 10., 960., 600., false), &tiny, MIN());
        assert_eq!(windowed(got), (0., 0., 320., 240.));
    }

    #[test]
    fn with_several_displays_the_one_showing_most_of_it_decides() {
        // A second monitor to the left (negative x) and a primary to the right.
        let screens = [
            display(0., 0., 1920., 1080.),
            display(-1280., 0., 1280., 1024.),
        ];
        // Mostly on the left monitor, which is named so that the window opens
        // there rather than on the primary.
        let got = restored_bounds(&saved(-1100., 100., 960., 600., false), &screens, MIN());
        assert_eq!(got.as_ref().map(|restored| restored.display), Some(1));
        assert_eq!(windowed(got), (-1100., 100., 960., 600.));
        // Straddling, mostly on the primary: it stays within the primary.
        let got = restored_bounds(&saved(-200., 100., 960., 600., false), &screens, MIN());
        assert_eq!(got.as_ref().map(|restored| restored.display), Some(0));
        assert_eq!(windowed(got), (0., 100., 960., 600.));
        // With the left monitor gone it is brought to the primary.
        let got = restored_bounds(
            &saved(-1100., 100., 960., 600., false),
            &screens[..1],
            MIN(),
        );
        assert!(got.is_none(), "no overlap with the remaining display");
        let got = restored_bounds(&saved(-600., 100., 960., 600., false), &screens[..1], MIN());
        assert_eq!(windowed(got), (0., 100., 960., 600.));
    }

    #[test]
    fn three_displays_with_one_offset_keep_the_window_where_it_was() {
        // Primary in the middle, one to the right raised by 300 and one to
        // the left (#125).
        let screens = [
            display(0., 0., 1920., 1080.),
            display(1920., -300., 2560., 1440.),
            display(-1920., 0., 1920., 1080.),
        ];
        let got = restored_bounds(&saved(2200., -250., 1200., 800., false), &screens, MIN());
        assert_eq!(got.as_ref().map(|restored| restored.display), Some(1));
        assert_eq!(windowed(got), (2200., -250., 1200., 800.));
        let got = restored_bounds(&saved(-1500., 300., 1000., 700., true), &screens, MIN());
        assert_eq!(got.as_ref().map(|restored| restored.display), Some(2));
        assert_eq!(windowed(got), (-1500., 300., 1000., 700.));
    }

    #[test]
    fn a_relative_rectangle_goes_to_the_display_it_names() {
        // macOS: every display at the origin, coordinates relative to it.
        let screens = [display(0., 0., 1920., 1080.), display(0., 0., 1440., 900.)];
        let uuids = [Some("AAAA".to_string()), Some("BBBB".to_string())];
        let mut layout = saved(100., 80., 960., 600., false);
        layout.display = Some("bbbb".into());
        let got = restored_on_named_display(&layout, &screens, &uuids, MIN());
        assert_eq!(got.as_ref().map(|restored| restored.display), Some(1));
        assert_eq!(windowed(got), (100., 80., 960., 600.));
        // Fitted to the named display, not the first.
        layout.window.as_mut().unwrap().x = 1200.;
        let got = restored_on_named_display(&layout, &screens, &uuids, MIN());
        assert_eq!(windowed(got), (480., 80., 960., 600.));
        // The display is gone: the usual place, not the first display.
        layout.display = Some("CCCC".into());
        assert!(restored_on_named_display(&layout, &screens, &uuids, MIN()).is_none());
        // No display saved: as before.
        layout.display = None;
        let got = restored_on_named_display(&layout, &screens, &uuids, MIN());
        assert_eq!(got.as_ref().map(|restored| restored.display), Some(0));
    }

    #[test]
    fn maximized_keeps_the_restore_rectangle() {
        let screens = [display(0., 0., 1920., 1080.)];
        let got = restored_bounds(&saved(200., 100., 960., 600., true), &screens, MIN());
        assert!(matches!(
            got,
            Some(Restored {
                bounds: WindowBounds::Maximized(_),
                ..
            })
        ));
        assert_eq!(windowed(got), (200., 100., 960., 600.));
    }
}
