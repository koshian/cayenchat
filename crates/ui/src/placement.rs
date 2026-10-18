// SPDX-License-Identifier: GPL-3.0-only
//! Putting a new window where it was laid out on Windows, whose scale can
//! differ between displays. The arithmetic stays in [`crate::window_layout`].

#[cfg(any(target_os = "windows", test))]
use gpui::{Bounds, Pixels};

/// A logical rectangle as the physical pixels of a display with `scale`.
#[cfg(any(target_os = "windows", test))]
pub fn physical_rect(bounds: Bounds<Pixels>, scale: f32) -> [i32; 4] {
    let physical = |value: Pixels| (f32::from(value) * scale).round() as i32;
    [
        physical(bounds.origin.x),
        physical(bounds.origin.y),
        physical(bounds.size.width),
        physical(bounds.size.height),
    ]
}

/// `rect` (`[x, y, width, height]`) moved, and shrunk when larger, to lie inside
/// `work` (`[left, top, right, bottom]`, a display's work area).
#[cfg(any(target_os = "windows", test))]
pub fn fit_to_work_area(rect: [i32; 4], work: [i32; 4]) -> [i32; 4] {
    let [x, y, width, height] = rect;
    let [left, top, right, bottom] = work;
    let width = width.min((right - left).max(0));
    let height = height.min((bottom - top).max(0));
    [
        x.min(right - width).max(left),
        y.min(bottom - height).max(top),
        width,
        height,
    ]
}

/// A new window is created on whichever display Windows picks by default, and
/// GPUI turns the requested logical bounds into pixels with the scale of that
/// display, not of the target one. With different scales the window would land
/// elsewhere, so it is put where wanted in physical pixels, as laid out in the
/// owner's `scale`, once it exists. Best effort: failures leave it as created.
#[cfg(target_os = "windows")]
pub fn place_native(window: &gpui::Window, bounds: Bounds<Pixels>, scale: f32) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClientRect, GetWindowRect, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos,
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(native) = handle.as_raw() else {
        return;
    };
    let hwnd = native.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let [x, y, width, height] = physical_rect(bounds, scale);
    // SAFETY: plain queries and a move of a live window with local structs.
    unsafe {
        let (mut outer, mut inner): (RECT, RECT) = std::mem::zeroed();
        let mut origin = POINT { x: 0, y: 0 };
        if GetWindowRect(hwnd, &mut outer) == 0
            || GetClientRect(hwnd, &mut inner) == 0
            || ClientToScreen(hwnd, &mut origin) == 0
        {
            return;
        }
        // The wanted rectangle is the content's; the window adds its frame.
        let (left, top) = (origin.x - outer.left, origin.y - outer.top);
        let extra_width = (outer.right - outer.left) - (inner.right - inner.left);
        let extra_height = (outer.bottom - outer.top) - (inner.bottom - inner.top);
        let mut rect = [
            x - left,
            y - top,
            width + extra_width,
            height + extra_height,
        ];
        // Keep the frame and title bar inside the work area (taskbar excluded) of
        // the display the window is on; the content-based layout cannot know
        // their size.
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if !monitor.is_null() && GetMonitorInfoW(monitor, &mut info) != 0 {
            let work = info.rcWork;
            rect = fit_to_work_area(rect, [work.left, work.top, work.right, work.bottom]);
        }
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            rect[0],
            rect[1],
            rect[2],
            rect[3],
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    #[test]
    fn physical_rect_uses_the_given_scale() {
        // Laid out at 200%: a logical 1230 is physical 2460, whatever the
        // scale of the display the window happens to be created on.
        let bounds = Bounds::new(point(px(1230.), px(20.)), size(px(900.), px(750.)));
        assert_eq!(physical_rect(bounds, 2.0), [2460, 40, 1800, 1500]);
        assert_eq!(physical_rect(bounds, 1.0), [1230, 20, 900, 750]);
    }

    const WORK: [i32; 4] = [0, 0, 1440, 900];

    #[test]
    fn fit_keeps_a_rect_that_already_fits() {
        assert_eq!(
            fit_to_work_area([100, 50, 900, 780], WORK),
            [100, 50, 900, 780]
        );
    }

    #[test]
    fn fit_pushes_the_title_bar_below_the_top_edge() {
        assert_eq!(fit_to_work_area([270, -31, 900, 780], WORK)[1], 0);
    }

    #[test]
    fn fit_pushes_inside_the_left_and_right_edges() {
        assert_eq!(fit_to_work_area([-8, 10, 900, 780], WORK)[0], 0);
        assert_eq!(fit_to_work_area([1000, 10, 900, 780], WORK)[0], 540);
    }

    #[test]
    fn fit_respects_a_taskbar_on_the_top_and_left() {
        let work = [60, 40, 1500, 1040];
        assert_eq!(fit_to_work_area([0, 0, 900, 780], work), [60, 40, 900, 780]);
        assert_eq!(fit_to_work_area([100, 600, 900, 780], work)[1], 260);
    }

    #[test]
    fn fit_shrinks_a_rect_larger_than_the_work_area() {
        assert_eq!(
            fit_to_work_area([-50, -50, 2000, 1200], WORK),
            [0, 0, 1440, 900]
        );
    }

    #[test]
    fn physical_rect_keeps_negative_origins() {
        let bounds = Bounds::new(point(px(-1000.), px(-10.)), size(px(500.), px(400.)));
        assert_eq!(physical_rect(bounds, 1.5), [-1500, -15, 750, 600]);
    }
}
