//! A color picker: a saturation/brightness square, a hue bar and a `#RRGGBB`
//! field that stay in step. The owner listens for [`ColorChanged`] and may set
//! the color from outside; nothing here knows about settings.

use std::{cell::Cell, rc::Rc};

use gpui::{
    AppContext, Bounds, Context, Entity, EntityId, EventEmitter, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, StatefulInteractiveElement,
    Styled, Window, canvas, div, hsla, linear_color_stop, linear_gradient, prelude::FluentBuilder,
    px, rgb,
};

use crate::{input::TextInput, splitter::NoGhost};

/// Side of the saturation/brightness square, and length of the hue bar.
const SIZE: f32 = 160.;
const BAR_HEIGHT: f32 = 14.;
const MARKER: f32 = 10.;

/// Hue in degrees `0..360`, saturation and brightness in `0..=1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    pub fn from_rgb(rgb: u32) -> Self {
        let channel = |shift: u32| ((rgb >> shift) & 0xFF) as f32 / 255.;
        let (r, g, b) = (channel(16), channel(8), channel(0));
        let max = r.max(g).max(b);
        let delta = max - r.min(g).min(b);
        let h = if delta == 0. {
            0.
        } else if max == r {
            60. * ((g - b) / delta).rem_euclid(6.)
        } else if max == g {
            60. * ((b - r) / delta + 2.)
        } else {
            60. * ((r - g) / delta + 4.)
        };
        Self {
            h,
            s: if max == 0. { 0. } else { delta / max },
            v: max,
        }
    }

    pub fn to_rgb(self) -> u32 {
        let chroma = self.v * self.s;
        let sector = self.h.rem_euclid(360.) / 60.;
        let x = chroma * (1. - (sector % 2. - 1.).abs());
        let (r, g, b) = match sector as u32 {
            0 => (chroma, x, 0.),
            1 => (x, chroma, 0.),
            2 => (0., chroma, x),
            3 => (0., x, chroma),
            4 => (x, 0., chroma),
            _ => (chroma, 0., x),
        };
        let m = self.v - chroma;
        let byte = |channel: f32| ((channel + m) * 255.).round().clamp(0., 255.) as u32;
        byte(r) << 16 | byte(g) << 8 | byte(b)
    }
}

pub fn format_hex(rgb: u32) -> String {
    format!("#{rgb:06X}")
}

/// `#RRGGBB` in either case, with surrounding spaces ignored.
pub fn parse_hex(text: &str) -> Option<u32> {
    cayenchat_storage::color_value(text.trim())
}

/// The color the user picked or typed.
pub struct ColorChanged(pub u32);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Handle {
    Square,
    Bar,
}

/// What a drag carries: which control of which picker is being dragged. GPUI
/// hands a drag's moves to every element listening for this type, so a picker
/// has to check that the drag is its own.
struct Dragging(Handle, EntityId);

pub struct ColorPicker {
    hsv: Hsv,
    hex: Entity<TextInput>,
    /// Whether the `#RRGGBB` field is drawn under the controls.
    show_hex: bool,
    square: Rc<Cell<Bounds<Pixels>>>,
    bar: Rc<Cell<Bounds<Pixels>>>,
}

impl EventEmitter<ColorChanged> for ColorPicker {}

impl ColorPicker {
    pub fn new(initial: u32, cx: &mut Context<Self>) -> Self {
        let hex = cx.new(|cx| TextInput::new_field("#RRGGBB", &format_hex(initial), false, cx));
        // Typing a complete color moves the picker; other text is left alone.
        cx.observe(&hex, |this, hex, cx| {
            if let Some(color) = parse_hex(hex.read(cx).text())
                && color != this.color()
            {
                this.hsv = keep_hue(this.hsv, Hsv::from_rgb(color));
                cx.emit(ColorChanged(color));
                cx.notify();
            }
        })
        .detach();
        Self {
            hsv: Hsv::from_rgb(initial),
            hex,
            show_hex: true,
            square: Rc::default(),
            bar: Rc::default(),
        }
    }

    /// Hides the picker's own `#RRGGBB` field, for an owner that already
    /// shows the color as text (the field still follows the picker and still
    /// moves it; it is only not drawn).
    pub fn without_hex_field(mut self) -> Self {
        self.show_hex = false;
        self
    }

    pub fn color(&self) -> u32 {
        self.hsv.to_rgb()
    }

    pub fn shows_hex_field(&self) -> bool {
        self.show_hex
    }

    /// Shows `color` without announcing it, for when the owner changed it.
    pub fn set_color(&mut self, color: u32, cx: &mut Context<Self>) {
        self.hsv = keep_hue(self.hsv, Hsv::from_rgb(color));
        self.show_hex(cx);
        cx.notify();
    }

    fn show_hex(&self, cx: &mut Context<Self>) {
        let text = format_hex(self.color());
        self.hex.update(cx, |hex, cx| hex.set_text(&text, cx));
    }

    /// Moves the picker to where the pointer is on `handle`.
    fn pick(&mut self, handle: Handle, position: Point<Pixels>, cx: &mut Context<Self>) {
        let bounds = match handle {
            Handle::Square => self.square.get(),
            Handle::Bar => self.bar.get(),
        };
        // Not laid out yet.
        if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return;
        }
        let x = fraction(position.x, bounds.origin.x, bounds.size.width);
        let y = fraction(position.y, bounds.origin.y, bounds.size.height);
        match handle {
            Handle::Square => {
                self.hsv.s = x;
                self.hsv.v = 1. - y;
            }
            // 360 is red again; stay inside the last sector.
            Handle::Bar => self.hsv.h = (x * 360.).min(359.99),
        }
        self.show_hex(cx);
        cx.emit(ColorChanged(self.color()));
        cx.notify();
    }
}

/// The new hue is kept for greys and black, which have none of their own, so
/// moving the brightness to zero and back does not reset the hue bar.
fn keep_hue(old: Hsv, new: Hsv) -> Hsv {
    if new.s == 0. || new.v == 0. {
        Hsv { h: old.h, ..new }
    } else {
        new
    }
}

fn fraction(position: Pixels, origin: Pixels, length: Pixels) -> f32 {
    (f32::from(position - origin) / f32::from(length)).clamp(0., 1.)
}

fn marker(left: f32, top: f32) -> impl IntoElement {
    div()
        .absolute()
        .left(px(left - MARKER / 2.))
        .top(px(top - MARKER / 2.))
        .size(px(MARKER))
        .rounded_full()
        .border_1()
        .border_color(rgb(0xFFFFFF))
        .shadow_sm()
}

impl Render for ColorPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hue = Hsv {
            s: 1.,
            v: 1.,
            ..self.hsv
        };
        let white = hsla(0., 0., 1., 1.);
        let no_white = hsla(0., 0., 1., 0.);
        let no_black = hsla(0., 0., 0., 0.);
        let black = hsla(0., 0., 0., 1.);
        let (square, bar) = (self.square.clone(), self.bar.clone());
        let owner = cx.entity_id();
        let hue_stops = (0..6).map(|sector| {
            let from = Hsv {
                h: sector as f32 * 60.,
                s: 1.,
                v: 1.,
            }
            .to_rgb();
            let to = Hsv {
                h: (sector + 1) as f32 * 60.,
                s: 1.,
                v: 1.,
            }
            .to_rgb();
            div().flex_1().h_full().bg(linear_gradient(
                90.,
                linear_color_stop(rgb(from), 0.),
                linear_color_stop(rgb(to), 1.),
            ))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w(px(SIZE))
            .child(
                div()
                    .id("color-square")
                    .relative()
                    .size(px(SIZE))
                    .bg(rgb(hue.to_rgb()))
                    .cursor_crosshair()
                    .child(
                        canvas(move |bounds, _, _| square.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .child(div().absolute().size_full().bg(linear_gradient(
                        90.,
                        linear_color_stop(white, 0.),
                        linear_color_stop(no_white, 1.),
                    )))
                    .child(div().absolute().size_full().bg(linear_gradient(
                        180.,
                        linear_color_stop(no_black, 0.),
                        linear_color_stop(black, 1.),
                    )))
                    .child(marker(self.hsv.s * SIZE, (1. - self.hsv.v) * SIZE))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.pick(Handle::Square, event.position, cx)
                        }),
                    )
                    .on_drag(Dragging(Handle::Square, owner), |_, _, _, cx| {
                        cx.new(|_| NoGhost)
                    })
                    .on_drag_move(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<Dragging>, _, cx| {
                            let drag = event.drag(cx);
                            if drag.0 == Handle::Square && drag.1 == owner {
                                this.pick(Handle::Square, event.event.position, cx);
                            }
                        },
                    )),
            )
            .child(
                div()
                    .id("color-bar")
                    .relative()
                    .flex()
                    .w(px(SIZE))
                    .h(px(BAR_HEIGHT))
                    .cursor_pointer()
                    .child(
                        canvas(move |bounds, _, _| bar.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .children(hue_stops)
                    .child(marker(self.hsv.h / 360. * SIZE, BAR_HEIGHT / 2.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.pick(Handle::Bar, event.position, cx)
                        }),
                    )
                    .on_drag(Dragging(Handle::Bar, owner), |_, _, _, cx| {
                        cx.new(|_| NoGhost)
                    })
                    .on_drag_move(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<Dragging>, _, cx| {
                            let drag = event.drag(cx);
                            if drag.0 == Handle::Bar && drag.1 == owner {
                                this.pick(Handle::Bar, event.event.position, cx);
                            }
                        },
                    )),
            )
            .when(self.show_hex, |d| d.child(self.hex.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_and_hsv_round_trip_for_every_primary_and_grey() {
        for rgb in [
            0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0x00FFFF, 0xFF00FF,
            0x808080, 0x007D00, 0xD46A8E, 0x1F2124, 0xF2F5FF,
        ] {
            assert_eq!(Hsv::from_rgb(rgb).to_rgb(), rgb, "{rgb:06X}");
        }
        let red = Hsv::from_rgb(0xFF0000);
        assert_eq!((red.h, red.s, red.v), (0., 1., 1.));
        assert_eq!(Hsv::from_rgb(0x00FF00).h, 120.);
        assert_eq!(Hsv::from_rgb(0x0000FF).h, 240.);
    }

    #[test]
    fn hex_text_is_strict_like_the_settings_fields() {
        assert_eq!(parse_hex(" #ff8800 "), Some(0xFF8800));
        assert_eq!(parse_hex("#FF8800"), Some(0xFF8800));
        assert_eq!(parse_hex("FF8800"), None);
        assert_eq!(parse_hex("#FF88"), None);
        assert_eq!(parse_hex("#GG8800"), None);
        assert_eq!(format_hex(0x0A0B0C), "#0A0B0C");
    }

    #[test]
    fn greys_keep_the_hue_the_picker_had() {
        let green = Hsv::from_rgb(0x00FF00);
        let grey = keep_hue(green, Hsv::from_rgb(0x808080));
        assert_eq!(grey.h, 120.);
        assert_eq!(keep_hue(green, Hsv::from_rgb(0x0000FF)).h, 240.);
        assert_eq!(keep_hue(green, Hsv::from_rgb(0x000000)).h, 120.);
    }

    struct Host {
        picker: Entity<ColorPicker>,
        heard: Vec<u32>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.picker.clone())
        }
    }

    fn open(
        cx: &mut gpui::TestAppContext,
        initial: u32,
    ) -> (Entity<Host>, &mut gpui::VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            let picker = cx.new(|cx| ColorPicker::new(initial, cx));
            cx.subscribe(&picker, |host: &mut Host, _, event: &ColorChanged, _| {
                host.heard.push(event.0)
            })
            .detach();
            Host {
                picker,
                heard: Vec::new(),
            }
        });
        cx.run_until_parked();
        (host, cx)
    }

    struct Pair(Entity<ColorPicker>, Entity<ColorPicker>);

    impl Render for Pair {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().flex().child(self.0.clone()).child(self.1.clone())
        }
    }

    #[gpui::test]
    fn dragging_one_picker_leaves_the_other_alone(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let (pair, cx) = cx.add_window_view(|_, cx| {
            Pair(
                cx.new(|cx| ColorPicker::new(0xFF0000, cx)),
                cx.new(|cx| ColorPicker::new(0x0000FF, cx)),
            )
        });
        cx.run_until_parked();
        let (first, second) = pair.read_with(cx, |pair, _| (pair.0.clone(), pair.1.clone()));
        let at = |x: f32, y: f32| gpui::point(px(x), px(y));
        let none = gpui::Modifiers::none();
        cx.simulate_mouse_move(at(80., 40.), None, none);
        cx.simulate_mouse_down(at(80., 40.), MouseButton::Left, none);
        cx.simulate_mouse_move(at(90., 50.), MouseButton::Left, none);
        cx.simulate_mouse_move(at(130., 100.), MouseButton::Left, none);
        cx.run_until_parked();
        assert_ne!(first.read_with(cx, |p, _| p.color()), 0xFF0000, "dragged");
        assert_eq!(
            second.read_with(cx, |p, _| p.color()),
            0x0000FF,
            "untouched"
        );
    }

    #[gpui::test]
    fn clicking_and_dragging_pick_colors_and_update_the_hex_field(cx: &mut gpui::TestAppContext) {
        let (host, cx) = open(cx, 0xFF0000);
        let at = |x: f32, y: f32| gpui::point(px(x), px(y));
        let none = gpui::Modifiers::none();
        // Halfway across, a quarter down: saturation .5, brightness .75.
        cx.simulate_mouse_move(at(80., 40.), None, none);
        cx.simulate_mouse_down(at(80., 40.), MouseButton::Left, none);
        cx.run_until_parked();
        let expected = Hsv {
            h: 0.,
            s: 0.5,
            v: 0.75,
        }
        .to_rgb();
        let (picker, heard) =
            host.read_with(cx, |host, _| (host.picker.clone(), host.heard.clone()));
        assert_eq!(heard.last(), Some(&expected), "{heard:?}");
        assert_eq!(picker.read_with(cx, |p, _| p.color()), expected);
        let hex = picker.read_with(cx, |p, cx| p.hex.read(cx).text().to_owned());
        assert_eq!(hex, format_hex(expected));
        // Dragging past the corner clamps to full saturation and darkness.
        // (The move that starts a drag is not itself a drag move.)
        cx.simulate_mouse_move(at(90., 45.), MouseButton::Left, none);
        cx.simulate_mouse_move(at(400., 400.), MouseButton::Left, none);
        cx.run_until_parked();
        assert_eq!(picker.read_with(cx, |p, _| p.color()), 0x000000);
        cx.simulate_mouse_up(at(400., 400.), MouseButton::Left, none);
        // The hue bar sits below the square (160 px + 8 px gap).
        cx.simulate_mouse_move(at(80., 175.), None, none);
        cx.simulate_mouse_down(at(80., 175.), MouseButton::Left, none);
        cx.run_until_parked();
        let hue = picker.read_with(cx, |p, _| p.hsv.h);
        assert!((179.0..=181.0).contains(&hue), "{hue}");
    }

    #[gpui::test]
    fn typing_a_color_moves_the_picker_and_set_color_does_not_announce(
        cx: &mut gpui::TestAppContext,
    ) {
        let (host, cx) = open(cx, 0xFF0000);
        let picker = host.read_with(cx, |host, _| host.picker.clone());
        let hex = picker.read_with(cx, |p, _| p.hex.clone());
        hex.update(cx, |hex, cx| hex.set_text("#00FF00", cx));
        cx.run_until_parked();
        assert_eq!(picker.read_with(cx, |p, _| p.color()), 0x00FF00);
        assert_eq!(host.read_with(cx, |h, _| h.heard.clone()), [0x00FF00]);
        // Half-typed text changes nothing.
        hex.update(cx, |hex, cx| hex.set_text("#00FF", cx));
        cx.run_until_parked();
        assert_eq!(picker.read_with(cx, |p, _| p.color()), 0x00FF00);
        // The owner setting a color shows it in the field and says nothing.
        picker.update(cx, |p, cx| p.set_color(0x0000FF, cx));
        cx.run_until_parked();
        assert_eq!(
            picker.read_with(cx, |p, cx| p.hex.read(cx).text().to_owned()),
            "#0000FF"
        );
        assert_eq!(host.read_with(cx, |h, _| h.heard.len()), 1);
    }
}
