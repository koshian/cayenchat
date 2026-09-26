//! Client-side window frame for compositors without server decorations.
//!
//! GNOME's Wayland compositor does not implement `xdg-decoration`, so every
//! application draws its own title bar. This frame follows GNOME's Adwaita
//! header bar: the desktop's button layout, double-click action and interface
//! font come from the XDG desktop portal (see `desktop`), and its colors follow
//! the active light or dark theme. Server-decorated windows (macOS, Windows,
//! X11 and Wayland compositors with `xdg-decoration`) get the content as is.

use gpui::{
    AnyElement, App, Bounds, CursorStyle, Decorations, Hitbox, HitboxBehavior, Hsla, MouseButton,
    PathBuilder, Pixels, Point, ResizeEdge, SharedString, Size, Tiling, Window, WindowControls,
    canvas, div, hsla, point, prelude::*, px, transparent_black,
};

use crate::desktop::{self, DoubleClick, WindowButton};

const TITLEBAR_HEIGHT: Pixels = px(38.);
const BUTTON_SIZE: Pixels = px(24.);
const BUTTON_GAP: Pixels = px(6.);
/// Transparent margin around a floating window that holds its shadow and the
/// resize handles, like GTK's invisible borders.
const SHADOW: Pixels = px(10.);
const CORNER_RADIUS: Pixels = px(12.);

/// Adwaita header bar colors.
#[derive(Clone, Copy)]
struct HeaderColors {
    background: Hsla,
    backdrop: Hsla,
    foreground: Hsla,
    shade: Hsla,
    outline: Hsla,
    button: f32,
}

impl HeaderColors {
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                background: gray(0x30),
                backdrop: gray(0x24),
                foreground: hsla(0., 0., 1., 1.),
                shade: hsla(0., 0., 0., 0.36),
                outline: hsla(0., 0., 1., 0.07),
                button: 1.,
            }
        } else {
            Self {
                background: gray(0xeb),
                backdrop: gray(0xfa),
                foreground: hsla(0., 0., 0., 0.8),
                shade: hsla(0., 0., 0., 0.07),
                outline: hsla(0., 0., 0., 0.14),
                button: 0.,
            }
        }
    }

    /// Button fill: the foreground's lightness at a small opacity.
    fn button(self, alpha: f32) -> Hsla {
        hsla(0., 0., self.button, alpha)
    }
}

fn gray(value: u8) -> Hsla {
    hsla(0., 0., f32::from(value) / 255., 1.)
}

pub fn window_frame(
    window: &mut Window,
    cx: &App,
    title: impl Into<SharedString>,
    content: impl IntoElement,
) -> AnyElement {
    let Decorations::Client { tiling } = window.window_decorations() else {
        return content.into_any_element();
    };
    let inset = if tiling.is_tiled() { px(0.) } else { SHADOW };
    window.set_client_inset(inset);
    let colors = HeaderColors::new(crate::theme::current(cx).dark);
    let floating = !tiling.is_tiled();
    div()
        .id("window-backdrop")
        .size_full()
        .bg(transparent_black())
        .when(!tiling.top, |d| d.pt(inset))
        .when(!tiling.bottom, |d| d.pb(inset))
        .when(!tiling.left, |d| d.pl(inset))
        .when(!tiling.right, |d| d.pr(inset))
        .when(floating, |d| {
            d.child(resize_cursor_layer(inset))
                // Only the margin reaches this handler; the window stops
                // mouse moves and handles its own presses.
                .on_mouse_move(|_, window, _| window.refresh())
                .on_mouse_down(MouseButton::Left, move |event, window, _| {
                    let size = window.window_bounds().get_bounds().size;
                    if let Some(edge) = resize_edge(event.position, inset, size) {
                        window.start_window_resize(edge);
                    }
                })
        })
        .child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .cursor(CursorStyle::Arrow)
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .border_color(colors.outline)
                .when(!tiling.top, |d| d.border_t_1())
                .when(!tiling.bottom, |d| d.border_b_1())
                .when(!tiling.left, |d| d.border_l_1())
                .when(!tiling.right, |d| d.border_r_1())
                .when(!(tiling.top || tiling.left), |d| {
                    d.rounded_tl(CORNER_RADIUS)
                })
                .when(!(tiling.top || tiling.right), |d| {
                    d.rounded_tr(CORNER_RADIUS)
                })
                .when(floating, |d| {
                    d.shadow(vec![gpui::BoxShadow {
                        color: hsla(0., 0., 0., 0.3),
                        offset: point(px(0.), px(2.)),
                        blur_radius: SHADOW / 2.,
                        spread_radius: px(0.),
                    }])
                })
                .child(titlebar(window, cx, title.into(), tiling, colors))
                .child(div().flex_1().min_h_0().child(content)),
        )
        .into_any_element()
}

/// Window position of the content's top-left corner, for placing absolutely
/// positioned popups at pointer coordinates below a client-side title bar.
pub fn content_origin(window: &Window) -> Point<Pixels> {
    match window.window_decorations() {
        Decorations::Server => point(px(0.), px(0.)),
        Decorations::Client { tiling } => {
            let inset = if tiling.is_tiled() { px(0.) } else { SHADOW };
            let edge = |tiled: bool| if tiled { px(0.) } else { inset + px(1.) };
            point(edge(tiling.left), edge(tiling.top) + TITLEBAR_HEIGHT)
        }
    }
}

fn titlebar(
    window: &Window,
    cx: &App,
    title: SharedString,
    tiling: Tiling,
    colors: HeaderColors,
) -> impl IntoElement {
    let settings = desktop::current(cx);
    let controls = window.window_controls();
    let maximized = window.is_maximized();
    let active = window.is_window_active();
    let foreground = if active {
        colors.foreground
    } else {
        colors.foreground.opacity(0.5)
    };
    let available = |buttons: &[WindowButton]| -> Vec<WindowButton> {
        buttons
            .iter()
            .copied()
            .filter(|button| supported(*button, controls))
            .collect()
    };
    let left = available(&settings.buttons.left);
    let right = available(&settings.buttons.right);
    // Keep the centered title clear of the wider button group.
    let reserved = (BUTTON_SIZE + BUTTON_GAP) * left.len().max(right.len()) as f32 + px(12.);
    let double_click = settings.double_click;
    div()
        .id("client-titlebar")
        .relative()
        .flex()
        .flex_shrink_0()
        .items_center()
        .h(TITLEBAR_HEIGHT)
        .px(px(7.))
        .bg(if active {
            colors.background
        } else {
            colors.backdrop
        })
        .text_color(foreground)
        .border_b_1()
        .border_color(colors.shade)
        .when(!(tiling.top || tiling.left), |d| {
            d.rounded_tl(CORNER_RADIUS)
        })
        .when(!(tiling.top || tiling.right), |d| {
            d.rounded_tr(CORNER_RADIUS)
        })
        .on_mouse_down(MouseButton::Left, move |event, window, _| {
            if event.click_count != 2 {
                window.start_window_move();
                return;
            }
            match double_click {
                DoubleClick::ToggleMaximize => window.zoom_window(),
                DoubleClick::Minimize => window.minimize_window(),
                DoubleClick::Menu => window.show_window_menu(event.position),
                DoubleClick::None => {}
            }
        })
        .on_mouse_down(MouseButton::Right, |event, window, _| {
            window.show_window_menu(event.position);
        })
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(reserved)
                .right(reserved)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_size(px(14.))
                        .when_some(settings.title_font, |d, font| d.font_family(font))
                        .child(title),
                ),
        )
        .child(button_group(&left, maximized, colors, foreground))
        .child(div().flex_1())
        .child(button_group(&right, maximized, colors, foreground))
}

fn supported(button: WindowButton, controls: WindowControls) -> bool {
    match button {
        WindowButton::Minimize => controls.minimize,
        WindowButton::Maximize => controls.maximize,
        WindowButton::Close => true,
    }
}

fn button_group(
    buttons: &[WindowButton],
    maximized: bool,
    colors: HeaderColors,
    foreground: Hsla,
) -> impl IntoElement {
    div().flex().gap(BUTTON_GAP).children(
        buttons
            .iter()
            .map(|button| window_button(*button, maximized, colors, foreground)),
    )
}

fn window_button(
    button: WindowButton,
    maximized: bool,
    colors: HeaderColors,
    foreground: Hsla,
) -> impl IntoElement {
    let id = match button {
        WindowButton::Minimize => "titlebar-minimize",
        WindowButton::Maximize => "titlebar-maximize",
        WindowButton::Close => "titlebar-close",
    };
    div()
        .id(id)
        .size(BUTTON_SIZE)
        .flex_shrink_0()
        .rounded_full()
        .bg(colors.button(0.1))
        .hover(|d| d.bg(colors.button(0.15)))
        .active(|d| d.bg(colors.button(0.3)))
        // Keep the title bar from starting a window move under the button.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, _| match button {
            WindowButton::Minimize => window.minimize_window(),
            WindowButton::Maximize => window.zoom_window(),
            WindowButton::Close => window.remove_window(),
        })
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    paint_icon(button, maximized, bounds, foreground, window)
                },
            )
            .size_full(),
        )
}

/// Draws Adwaita-like symbolic window icons, 8px wide in a 24px button.
fn paint_icon(
    button: WindowButton,
    maximized: bool,
    bounds: Bounds<Pixels>,
    color: Hsla,
    window: &mut Window,
) {
    let center = bounds.center();
    let at = |x: f32, y: f32| point(center.x + px(x), center.y + px(y));
    let mut path = PathBuilder::stroke(px(1.5));
    match button {
        WindowButton::Minimize => {
            path.move_to(at(-4., 3.5));
            path.line_to(at(4., 3.5));
        }
        WindowButton::Maximize if maximized => {
            // Restore: two overlapping squares.
            path.move_to(at(-4., -1.5));
            path.line_to(at(1.5, -1.5));
            path.line_to(at(1.5, 4.));
            path.line_to(at(-4., 4.));
            path.close();
            path.move_to(at(-1.5, -1.5));
            path.line_to(at(-1.5, -4.));
            path.line_to(at(4., -4.));
            path.line_to(at(4., 1.5));
            path.line_to(at(1.5, 1.5));
        }
        WindowButton::Maximize => {
            path.move_to(at(-4., -4.));
            path.line_to(at(4., -4.));
            path.line_to(at(4., 4.));
            path.line_to(at(-4., 4.));
            path.close();
        }
        WindowButton::Close => {
            path.move_to(at(-4., -4.));
            path.line_to(at(4., 4.));
            path.move_to(at(4., -4.));
            path.line_to(at(-4., 4.));
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// Shows resize cursors over the transparent margin.
fn resize_cursor_layer(inset: Pixels) -> impl IntoElement {
    canvas(
        |_, window, _| {
            window.insert_hitbox(
                Bounds::new(
                    point(px(0.), px(0.)),
                    window.window_bounds().get_bounds().size,
                ),
                HitboxBehavior::Normal,
            )
        },
        move |_, hitbox: Hitbox, window, _| {
            let size = window.window_bounds().get_bounds().size;
            if let Some(edge) = resize_edge(window.mouse_position(), inset, size) {
                window.set_cursor_style(resize_cursor(edge), &hitbox);
            }
        },
    )
    .size_full()
    .absolute()
}

fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

fn resize_edge(position: Point<Pixels>, inset: Pixels, size: Size<Pixels>) -> Option<ResizeEdge> {
    // Corners get a larger grab area than the thin margin.
    let corner = inset * 2.;
    let left = position.x < inset;
    let right = position.x > size.width - inset;
    let top = position.y < inset;
    let bottom = position.y > size.height - inset;
    let near_left = position.x < corner;
    let near_right = position.x > size.width - corner;
    let near_top = position.y < corner;
    let near_bottom = position.y > size.height - corner;
    Some(match () {
        _ if (top && near_left) || (left && near_top) => ResizeEdge::TopLeft,
        _ if (top && near_right) || (right && near_top) => ResizeEdge::TopRight,
        _ if (bottom && near_left) || (left && near_bottom) => ResizeEdge::BottomLeft,
        _ if (bottom && near_right) || (right && near_bottom) => ResizeEdge::BottomRight,
        _ if top => ResizeEdge::Top,
        _ if bottom => ResizeEdge::Bottom,
        _ if left => ResizeEdge::Left,
        _ if right => ResizeEdge::Right,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn margin_positions_map_to_resize_edges() {
        let window = size(px(400.), px(300.));
        let edge = |x: f32, y: f32| resize_edge(point(px(x), px(y)), SHADOW, window);
        assert_eq!(edge(200., 150.), None);
        assert_eq!(edge(200., 3.), Some(ResizeEdge::Top));
        assert_eq!(edge(397., 150.), Some(ResizeEdge::Right));
        assert_eq!(edge(3., 15.), Some(ResizeEdge::TopLeft));
        assert_eq!(edge(390., 297.), Some(ResizeEdge::BottomRight));
    }
}
