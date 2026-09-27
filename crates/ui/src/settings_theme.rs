//! A narrow native-theme-gpui adapter for existing settings controls.
//!
//! Pin the last GPUI 0.2.2 connector (0.5.7). Use its mapped colors and raw
//! per-widget metrics without replacing the IME-aware text input.

use gpui::{prelude::*, *};
use native_theme_gpui::{ResolvedTheme, SystemTheme};

#[derive(Default)]
struct NativeSettingsTheme {
    variants: Option<[Variant; 2]>,
    loading: bool,
}
impl Global for NativeSettingsTheme {}

struct Variant {
    resolved: ResolvedTheme,
    mapped: gpui_component::Theme,
}

impl Variant {
    fn new(resolved: ResolvedTheme, name: &str, dark: bool, reduce_transparency: bool) -> Self {
        let mapped = native_theme_gpui::to_theme(&resolved, name, dark, reduce_transparency);
        Self { resolved, mapped }
    }
}

/// Reload on opening settings or a GPUI appearance change, never per frame.
/// macOS's AppKit reader stays on the main thread. D-Bus/registry work on
/// other platforms runs in the background; controls use the old palette
/// (or the app fallback) while it is pending. No polling or watcher thread.
pub fn refresh(cx: &mut App) {
    if !cx.has_global::<NativeSettingsTheme>() {
        cx.set_global(NativeSettingsTheme::default());
    }
    if cx.global::<NativeSettingsTheme>().loading {
        return;
    }
    cx.global_mut::<NativeSettingsTheme>().loading = true;
    #[cfg(target_os = "macos")]
    finish(SystemTheme::from_system(), cx);
    #[cfg(not(target_os = "macos"))]
    {
        let read = cx.background_spawn(async { SystemTheme::from_system() });
        cx.spawn(async move |cx| {
            let result = read.await;
            let _ = cx.update(|cx| finish(result, cx));
        })
        .detach();
    }
}

fn finish(result: native_theme::Result<SystemTheme>, cx: &mut App) {
    let cache = cx.global_mut::<NativeSettingsTheme>();
    cache.loading = false;
    cache.variants = match result {
        Ok(system) => Some([
            Variant::new(
                system.light,
                &system.name,
                false,
                system.accessibility.reduce_transparency,
            ),
            Variant::new(
                system.dark,
                &system.name,
                true,
                system.accessibility.reduce_transparency,
            ),
        ]),
        Err(error) => {
            log::warn!("Could not load native settings theme; using app colors: {error}");
            None
        }
    };
    cx.refresh_windows();
}

fn variant(cx: &App) -> Option<&Variant> {
    let index = usize::from(crate::theme::current(cx).dark);
    cx.try_global::<NativeSettingsTheme>()?
        .variants
        .as_ref()
        .map(|variants| &variants[index])
}

pub fn current(cx: &App) -> Option<&ResolvedTheme> {
    variant(cx).map(|v| &v.resolved)
}

pub fn color(value: native_theme_gpui::Rgba) -> Rgba {
    let [r, g, b, a] = value.to_f32_array();
    Rgba { r, g, b, a }
}

/// Settings-only palette: never changes user-configured chat/log colors.
pub fn palette(cx: &App) -> crate::theme::Theme {
    let mut palette = crate::theme::current(cx);
    if let Some(variant) = variant(cx) {
        let defaults = &variant.resolved.defaults;
        let mapped = &variant.mapped;
        palette.window = mapped.background.into();
        palette.surface = color(defaults.surface_color);
        palette.text = mapped.foreground.into();
        palette.text_secondary = mapped.muted_foreground.into();
        palette.text_muted = color(defaults.disabled_text_color);
        palette.border = mapped.border.into();
        palette.tab_inactive = palette.window;
        palette.hover = mapped.list_hover.into();
        // Existing selection rows inherit normal text; use the inactive
        // highlight instead of the saturated accent that needs white text.
        palette.selected = color(defaults.selection_inactive_background);
    }
    palette
}

pub fn input_palette(cx: &App) -> crate::theme::Theme {
    let mut palette = palette(cx);
    if let Some(native) = current(cx) {
        palette.surface = color(native.input.background_color);
        palette.text = color(native.input.font.color);
        palette.placeholder = color(native.input.placeholder_color).into();
        palette.text_selection = color(native.input.selection_background);
    }
    palette
}

/// Preserve each caller's click handler, labels and stable element IDs.
pub fn button(id: impl Into<ElementId>, primary: bool, cx: &App) -> Stateful<Div> {
    let fallback = crate::theme::current(cx);
    let mut button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .px_3()
        .py_1()
        .border_1()
        .border_color(fallback.border)
        .bg(if primary {
            fallback.selected
        } else {
            fallback.surface
        })
        .text_color(fallback.text)
        .cursor_pointer();
    if let Some(native) = current(cx) {
        let b = &native.button;
        let background = color(if primary {
            b.primary_background
        } else {
            b.background_color
        });
        let foreground = color(if primary {
            b.primary_text_color
        } else {
            b.font.color
        });
        // Primary states retain the matching foreground/accent pair. The
        // reader's hover/active pair describes a secondary button only.
        let hover = if primary {
            background
        } else {
            color(b.hover_background)
        };
        let active = if primary {
            background
        } else {
            color(b.active_background.unwrap_or(b.hover_background))
        };
        let hover_text = if primary {
            foreground
        } else {
            color(b.hover_text_color)
        };
        let active_text = if primary {
            foreground
        } else {
            color(b.active_text_color)
        };
        button = button
            .bg(background)
            .text_color(foreground)
            .font_family(b.font.family.clone())
            .text_size(px(b.font.size))
            .min_w(px(b.min_width))
            .min_h(px(b.min_height))
            .rounded(px(b.border.corner_radius))
            .border(px(b.border.line_width))
            .border_color(if primary {
                background
            } else {
                color(b.border.color)
            })
            .px(px(b.border.padding_horizontal))
            .py(px(b.border.padding_vertical))
            .hover(move |d| d.bg(hover).text_color(hover_text))
            .active(move |d| d.bg(active).text_color(active_text));
    }
    button.style().align_self = Some(AlignSelf::FlexStart);
    button
}

pub fn checkbox(checked: bool, enabled: bool, cx: &App) -> Div {
    let fallback = crate::theme::current(cx);
    let mut indicator = div()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .size(px(16.))
        .text_size(px(13.))
        .line_height(px(14.))
        .border_1()
        .border_color(fallback.border)
        .bg(if checked {
            fallback.selected
        } else {
            fallback.surface
        })
        .text_color(fallback.text);
    if let Some(native) = current(cx) {
        let c = &native.checkbox;
        indicator = indicator
            .size(px(c.indicator_width.max(14.)))
            .rounded(px(c.border.corner_radius))
            .border(px(c.border.line_width))
            .border_color(color(if checked {
                c.checked_background
            } else {
                c.unchecked_border_color.unwrap_or(c.border.color)
            }))
            .bg(color(if checked {
                c.checked_background
            } else {
                c.unchecked_background.unwrap_or(c.background_color)
            }))
            .text_color(color(c.indicator_color))
            .when(!enabled, |d| d.opacity(c.disabled_opacity));
    } else if !enabled {
        indicator = indicator.opacity(0.5);
    }
    indicator.when(checked, |d| d.child("✓"))
}

#[cfg(test)]
mod tests {
    use super::{
        NativeSettingsTheme, Variant, button, checkbox, color, current, finish, input_palette,
        palette,
    };
    use crate::input::TextInput;
    use cayenchat_storage::{Appearance, ThemeMode};
    use gpui::{
        Context, Entity, Focusable, Modifiers, Render, TestAppContext, Window, WindowAppearance,
        div, prelude::*,
    };

    fn app_theme(dark: bool) -> crate::theme::Theme {
        crate::theme::Theme::new(
            if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            WindowAppearance::Light,
            &Appearance::default(),
        )
    }

    fn preset(name: &str) -> NativeSettingsTheme {
        NativeSettingsTheme {
            variants: Some([false, true].map(|dark| {
                let (mapped, resolved) = native_theme_gpui::from_preset(name, dark).unwrap();
                Variant { resolved, mapped }
            })),
            loading: false,
        }
    }

    #[gpui::test]
    fn native_modes_preserve_chat_colors_and_errors_fall_back(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(app_theme(false));
            assert_eq!(palette(cx), crate::theme::current(cx));
            cx.set_global(preset("macos-sonoma"));
            for dark in [false, true] {
                cx.set_global(app_theme(dark));
                let original = crate::theme::current(cx);
                assert_eq!(palette(cx).panes, original.panes);
                assert_eq!(palette(cx).dark, dark);
                assert_eq!(crate::theme::current(cx), original);
                assert_eq!(
                    input_palette(cx).surface,
                    color(current(cx).unwrap().input.background_color)
                );
            }
            finish(
                Err(native_theme_gpui::Error::PlatformUnsupported { platform: "test" }),
                cx,
            );
            assert!(current(cx).is_none());
            assert_eq!(palette(cx), crate::theme::current(cx));
            assert!(!cx.global::<NativeSettingsTheme>().loading);
        });
    }

    /// Render every platform preset in both modes with actual text input and
    /// click handlers. This checks the GPUI type boundary and retained input,
    /// not whether a non-host OS reader works on its real desktop.
    #[gpui::test]
    fn controls_render_and_keep_input_across_platform_palettes(cx: &mut TestAppContext) {
        struct Controls {
            input: Entity<TextInput>,
            clicks: usize,
        }
        impl Render for Controls {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        button("primary", true, cx)
                            .debug_selector(|| "primary".into())
                            .child("Save 保存")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clicks += 1;
                                cx.notify();
                            })),
                    )
                    .child(button("secondary", false, cx).child("Cancel キャンセル"))
                    .child(checkbox(true, true, cx))
                    .child(checkbox(false, true, cx))
                    .child(checkbox(true, false, cx))
                    .child(self.input.clone())
            }
        }
        cx.update(|cx| {
            cx.set_global(app_theme(false));
            crate::input::bind_keys(false, cx);
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| TextInput::new_settings_field("Host", "", false, cx));
            window.focus(&input.focus_handle(cx));
            Controls { input, clicks: 0 }
        });
        cx.simulate_input("irc.example.org");
        for name in ["macos-sonoma", "windows-11", "adwaita", "kde-breeze"] {
            cx.update(|_, cx| cx.set_global(preset(name)));
            for dark in [false, true] {
                cx.update(|_, cx| {
                    cx.set_global(app_theme(dark));
                    cx.refresh_windows();
                });
                cx.run_until_parked();
                let bounds = cx.debug_bounds("primary").expect("button must render");
                cx.simulate_click(bounds.center(), Modifiers::default());
                assert_eq!(
                    view.read_with(cx, |view, cx| view.input.read(cx).text().to_owned()),
                    "irc.example.org"
                );
            }
        }
        assert_eq!(view.read_with(cx, |view, _| view.clicks), 8);
    }
}
