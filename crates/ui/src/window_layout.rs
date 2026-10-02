//! Reopening the main window where it was left.
//!
//! Only what GPUI cannot know lives here: whether a saved rectangle still
//! suits the displays that are connected now. Displays come and go (a laptop
//! undocked, a monitor unplugged, a resolution change), so a saved position is
//! used only where it still overlaps a display, and is moved and shrunk to fit
//! that display; otherwise the caller opens the window in its usual place.

use cayenchat_storage::layout::Layout;
use gpui::{Bounds, Pixels, Size, WindowBounds, point, px, size};

/// The bounds to open the window with, or `None` when nothing is saved or no
/// connected display shows any of the saved rectangle.
pub fn restored_bounds(
    layout: &Layout,
    displays: &[Bounds<Pixels>],
    min: Size<Pixels>,
) -> Option<WindowBounds> {
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
    let display = displays
        .iter()
        .map(|display| (overlap(display), display))
        .filter(|(area, _)| *area > 0.)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, display)| *display)?;

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
    Some(if layout.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_storage::layout::WindowRect;

    const MIN: fn() -> Size<Pixels> = || size(px(720.), px(420.));

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

    fn windowed(bounds: Option<WindowBounds>) -> (f32, f32, f32, f32) {
        match bounds {
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
        assert!(matches!(got, Some(WindowBounds::Windowed(_))));
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
        assert_eq!(windowed(got), (100., 100., 720., 420.));
        // A display smaller than the minimum keeps the window's top left.
        let tiny = [display(0., 0., 640., 400.)];
        let got = restored_bounds(&saved(10., 10., 960., 600., false), &tiny, MIN());
        assert_eq!(windowed(got), (0., 0., 720., 420.));
    }

    #[test]
    fn with_several_displays_the_one_showing_most_of_it_decides() {
        // A second monitor to the left (negative x) and a primary to the right.
        let screens = [
            display(0., 0., 1920., 1080.),
            display(-1280., 0., 1280., 1024.),
        ];
        // Mostly on the left monitor.
        let got = restored_bounds(&saved(-1100., 100., 960., 600., false), &screens, MIN());
        assert_eq!(windowed(got), (-1100., 100., 960., 600.));
        // Straddling, mostly on the primary: it stays within the primary.
        let got = restored_bounds(&saved(-200., 100., 960., 600., false), &screens, MIN());
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
    fn maximized_keeps_the_restore_rectangle() {
        let screens = [display(0., 0., 1920., 1080.)];
        let got = restored_bounds(&saved(200., 100., 960., 600., true), &screens, MIN());
        assert!(matches!(got, Some(WindowBounds::Maximized(_))));
        assert_eq!(windowed(got), (200., 100., 960., 600.));
    }
}
