// SPDX-License-Identifier: GPL-3.0-only
//! Themes and standard wallpapers on the Appearance tab (#962).
//!
//! A theme is a preset: applying one copies its values into the appearance
//! form, and what is edited afterwards never reaches the theme. The tab shows
//! which theme each of the light and dark appearance came from and whether it
//! was edited since; imported themes are listed to apply or delete, and the
//! current light or dark appearance can be exported.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use cayenchat_media::wallpaper;
use cayenchat_storage::theme_file::{
    self, LibraryEntry, MAX_THEME_FILE_BYTES, MAX_THEME_WALLPAPER_BYTES, THEME_EXTENSION,
};
use cayenchat_storage::{Appearance, ThemeFile, ThemeKind, WallpaperRegion};
use gpui::{prelude::*, *};

use crate::{SettingsWindow, input::TextInput, settings_theme};

/// What the theme section holds besides the form's values.
pub(crate) struct ThemeUi {
    pub(crate) library: Vec<LibraryEntry>,
    /// A file is being read or written; further file actions wait.
    busy: bool,
    /// How the last action went.
    message: Option<String>,
    export: Option<ExportPrompt>,
}

struct ExportPrompt {
    dark: bool,
    name: Entity<TextInput>,
}

impl ThemeUi {
    pub(crate) fn new() -> Self {
        Self {
            library: crate::settings_file::library_directory()
                .map(|directory| theme_file::library_list(&directory))
                .unwrap_or_default(),
            busy: false,
            message: None,
            export: None,
        }
    }
}

/// The step of the wallpaper strength buttons, in percent.
const OPACITY_STEP: i16 = 5;

/// The check that makes an image fit to be a wallpaper.
fn check_image(bytes: &[u8]) -> Result<&'static str, String> {
    wallpaper::validate(bytes).map_err(|error| {
        match error {
            cayenchat_media::LoadError::NotImage | cayenchat_media::LoadError::Unsupported => {
                "A wallpaper is not a PNG, JPEG, GIF or WebP image."
            }
            cayenchat_media::LoadError::TooLarge => "A wallpaper image is too large.",
            _ => "A wallpaper image is damaged.",
        }
        .to_owned()
    })
}

/// Reads at most `limit` bytes of `path`; a longer file is an error.
fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("The file is too large.".into());
    }
    Ok(bytes)
}

/// A file name for a theme: its name without characters files dislike.
fn export_file_name(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = stem.trim().trim_matches('.');
    format!(
        "{}.{THEME_EXTENSION}",
        if stem.is_empty() { "theme" } else { stem }
    )
}

impl SettingsWindow {
    fn theme_kind_label(&self, dark: bool) -> String {
        self.i18n.text(if dark {
            "theme_kind_dark"
        } else {
            "theme_kind_light"
        })
    }

    fn theme_message(&mut self, message: String, cx: &mut Context<Self>) {
        self.themes.message = Some(message);
        cx.notify();
    }

    fn theme_error(&mut self, error: &str, cx: &mut Context<Self>) {
        let message = self.i18n.format("theme_error", &[("error", error)]);
        self.theme_message(message, cx);
    }

    /// The appearance as the form shows it now.
    fn shown_appearance(&self, cx: &App) -> Appearance {
        self.settings
            .snapshot_with(cx, false)
            .map(|settings| settings.appearance)
            .unwrap_or_else(|_| self.settings.values.appearance.clone())
    }

    /// Makes `appearance` what the form shows and, through the autosave that
    /// every change starts, what is saved.
    fn adopt_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.show_appearance(&appearance, cx);
        self.settings.values.appearance = appearance;
        self.font_picker = None;
        self.color_picker = None;
        cx.notify();
    }

    /// Returns the light or dark appearance to the built-in default theme.
    pub(crate) fn theme_use_default(&mut self, dark: bool, cx: &mut Context<Self>) {
        let mut appearance = self.shown_appearance(cx);
        // The built-in theme carries no wallpaper, so no file is written.
        let outcome = ThemeFile::builtin(dark).apply(&mut appearance, Path::new(""), &check_image);
        match outcome {
            Ok(()) => {
                self.adopt_appearance(appearance, cx);
                let message = self.i18n.format(
                    "theme_applied",
                    &[
                        ("name", theme_file::builtin_name(dark)),
                        ("kind", &self.theme_kind_label(dark)),
                    ],
                );
                self.theme_message(message, cx);
            }
            Err(error) => self.theme_error(&error, cx),
        }
    }

    /// Asks for a theme file and adds it to the list of imported themes.
    pub(crate) fn theme_import(&mut self, cx: &mut Context<Self>) {
        if self.themes.busy {
            return;
        }
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
            filter: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.themes.busy = true;
                let directory = crate::settings_file::library_directory();
                let importing = cx.background_spawn(async move {
                    let bytes = read_limited(&path, MAX_THEME_FILE_BYTES)?;
                    let theme = ThemeFile::parse(&bytes, &check_image)?;
                    let directory = directory?;
                    let entry = theme_file::library_add(&directory, &theme)?;
                    Ok::<_, String>((entry, theme_file::library_list(&directory)))
                });
                cx.spawn(async move |this, cx| {
                    let result = importing.await;
                    let _ = this.update(cx, |this, cx| {
                        this.themes.busy = false;
                        match result {
                            Ok((entry, library)) => {
                                this.themes.library = library;
                                let message = this.i18n.format(
                                    "theme_imported",
                                    &[
                                        ("name", &entry.name),
                                        ("kind", &this.theme_kind_label(entry.mode.is_dark())),
                                    ],
                                );
                                this.theme_message(message, cx);
                            }
                            Err(error) => this.theme_error(&error, cx),
                        }
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    /// Applies an imported theme to the matching side of the appearance.
    pub(crate) fn theme_apply(&mut self, id: String, cx: &mut Context<Self>) {
        if self.themes.busy {
            return;
        }
        self.themes.busy = true;
        let directories = (
            crate::settings_file::library_directory(),
            crate::settings_file::wallpaper_directory(),
        );
        let applying = cx.background_spawn(async move {
            let (library, wallpapers) = (directories.0?, directories.1?);
            let theme = theme_file::library_load(&library, &id, &check_image)?;
            let prepared = theme.prepare(&wallpapers, &check_image)?;
            Ok::<_, String>((theme, prepared, wallpapers))
        });
        cx.spawn(async move |this, cx| {
            let result = applying.await;
            let _ = this.update(cx, |this, cx| {
                this.themes.busy = false;
                match result {
                    Ok((theme, prepared, wallpapers)) => {
                        // Only the files were prepared off the thread; the
                        // values go onto the form as it is now, which may
                        // have been edited meanwhile.
                        let mut appearance = this.shown_appearance(cx);
                        prepared.apply_to(&mut appearance);
                        // What neither the saved settings nor the shown ones
                        // use is gone.
                        let saved = this.saved.appearance.clone();
                        let kept = appearance.clone();
                        cx.background_spawn(async move {
                            theme_file::prune_wallpapers(&wallpapers, &[&kept, &saved]);
                        })
                        .detach();
                        this.adopt_appearance(appearance, cx);
                        let message = this.i18n.format(
                            "theme_applied",
                            &[
                                ("name", &theme.name),
                                ("kind", &this.theme_kind_label(theme.mode.is_dark())),
                            ],
                        );
                        this.theme_message(message, cx);
                    }
                    Err(error) => this.theme_error(&error, cx),
                }
            });
        })
        .detach();
    }

    /// Removes a theme from the list. What was applied from it stays.
    pub(crate) fn theme_delete(&mut self, id: &str, cx: &mut Context<Self>) {
        let Ok(directory) = crate::settings_file::library_directory() else {
            return;
        };
        match theme_file::library_remove(&directory, id) {
            Ok(()) => {
                self.themes.library = theme_file::library_list(&directory);
                let message = self.i18n.text("theme_deleted");
                self.theme_message(message, cx);
            }
            Err(error) => self.theme_error(&error, cx),
        }
    }

    /// Opens the name prompt of "export the current look as a theme" for the
    /// light or dark side. The theme name starts as the origin's.
    pub(crate) fn theme_begin_export(&mut self, dark: bool, cx: &mut Context<Self>) {
        let (name, _) = self.shown_appearance(cx).theme_status(dark);
        let placeholder = self.i18n.text("theme_export_name");
        let field = cx.new(|cx| TextInput::new_settings_field(&placeholder, &name, false, cx));
        self.themes.export = Some(ExportPrompt { dark, name: field });
        self.themes.message = None;
        cx.notify();
    }

    pub(crate) fn theme_cancel_export(&mut self, cx: &mut Context<Self>) {
        self.themes.export = None;
        cx.notify();
    }

    /// Asks where to save, then writes the exported theme. Nothing about the
    /// settings or the origin changes.
    pub(crate) fn theme_choose_export_path(&mut self, cx: &mut Context<Self>) {
        if self.themes.busy {
            return;
        }
        let Some(prompt) = &self.themes.export else {
            return;
        };
        let dark = prompt.dark;
        let name = theme_file::clean_theme_name(prompt.name.read(cx).text());
        if name.is_empty() {
            let message = self.i18n.text("theme_name_empty");
            self.theme_message(message, cx);
            return;
        }
        let appearance = self.shown_appearance(cx);
        let directory = ["HOME", "USERPROFILE"]
            .into_iter()
            .find_map(std::env::var_os)
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(std::env::temp_dir);
        let chosen = cx.prompt_for_new_path(&directory, Some(&export_file_name(&name)));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut path))) = chosen.await else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension(THEME_EXTENSION);
            }
            let _ = this.update(cx, |this, cx| {
                this.themes.busy = true;
                let writing = cx.background_spawn({
                    let path = path.clone();
                    async move {
                        let theme = ThemeFile::capture(
                            &appearance,
                            dark,
                            &name,
                            &check_image,
                            |wallpaper| {
                                read_limited(Path::new(wallpaper), MAX_THEME_WALLPAPER_BYTES as u64)
                            },
                        )?;
                        std::fs::write(&path, theme.to_gzip()?).map_err(|error| error.to_string())
                    }
                });
                cx.spawn(async move |this, cx| {
                    let result = writing.await;
                    let _ = this.update(cx, |this, cx| {
                        this.themes.busy = false;
                        match result {
                            Ok(()) => {
                                this.themes.export = None;
                                let message = this.i18n.format(
                                    "theme_exported",
                                    &[("path", &path.display().to_string())],
                                );
                                this.theme_message(message, cx);
                            }
                            Err(error) => this.theme_error(&error, cx),
                        }
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    /// Asks for an image, checks it, and makes it the standard wallpaper of
    /// the area.
    pub(crate) fn wallpaper_choose(
        &mut self,
        dark: bool,
        region: WallpaperRegion,
        cx: &mut Context<Self>,
    ) {
        if self.themes.busy {
            return;
        }
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.i18n.text("wallpaper_choose").into()),
            filter: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.themes.busy = true;
                let checking = cx.background_spawn({
                    let path = path.clone();
                    async move {
                        let bytes = read_limited(&path, wallpaper::MAX_FILE_BYTES)?;
                        check_image(&bytes).map(|_| ())
                    }
                });
                cx.spawn(async move |this, cx| {
                    let result = checking.await;
                    let _ = this.update(cx, |this, cx| {
                        this.themes.busy = false;
                        let text = path.to_string_lossy().into_owned();
                        match result {
                            Ok(()) if text.len() <= cayenchat_storage::MAX_WALLPAPER_PATH_BYTES => {
                                this.settings
                                    .values
                                    .appearance
                                    .wallpapers_mut(dark)
                                    .get_mut(region)
                                    .path = Some(text);
                                this.themes.message = None;
                                cx.notify();
                            }
                            Ok(()) => {
                                let message = this.i18n.text("channel_look_wallpaper_long");
                                this.theme_message(message, cx);
                            }
                            Err(error) => this.theme_error(&error, cx),
                        }
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    pub(crate) fn wallpaper_clear(
        &mut self,
        dark: bool,
        region: WallpaperRegion,
        cx: &mut Context<Self>,
    ) {
        let wallpaper = self
            .settings
            .values
            .appearance
            .wallpapers_mut(dark)
            .get_mut(region);
        wallpaper.path = None;
        wallpaper.opacity = None;
        cx.notify();
    }

    pub(crate) fn wallpaper_step_opacity(
        &mut self,
        dark: bool,
        region: WallpaperRegion,
        delta: i16,
        cx: &mut Context<Self>,
    ) {
        let wallpaper = self
            .settings
            .values
            .appearance
            .wallpapers_mut(dark)
            .get_mut(region);
        // The stored opacity is the background's over the picture; the picture's
        // strength shown to the user is the rest.
        let next = (i16::from(wallpaper.opacity_percent()) - delta).clamp(0, 100);
        wallpaper.opacity = Some(next as u8);
        cx.notify();
    }

    /// The theme section: where the light and dark appearance came from, the
    /// imported themes and the file actions.
    pub(crate) fn render_theme_settings(&self, cx: &mut Context<Self>) -> Div {
        let palette = settings_theme::palette(cx);
        let appearance = self.shown_appearance(cx);
        let hint = |key: &str| {
            div()
                .ml(settings_theme::LABEL_INDENT)
                .text_color(palette.text_secondary)
                .child(self.i18n.text(key))
        };
        let mut section = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("theme_heading")),
            )
            .child(hint("theme_hint"));
        for dark in [false, true] {
            let (name, modified) = appearance.theme_status(dark);
            let id = if dark { "dark" } else { "light" };
            section =
                section.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(settings_theme::label_column().child(self.theme_kind_label(dark)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .debug_selector(move || format!("theme-source-{id}"))
                                .child(self.i18n.format("theme_source", &[("name", &name)]))
                                .when(modified, |d| {
                                    d.child(
                                        div()
                                            .ml_2()
                                            .text_color(palette.warning)
                                            .child(self.i18n.text("theme_modified")),
                                    )
                                }),
                        )
                        .child(
                            settings_theme::button(("theme-default", usize::from(dark)), false, cx)
                                .child(self.i18n.text("theme_use_default"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.theme_use_default(dark, cx)
                                })),
                        )
                        .child(
                            settings_theme::button(("theme-export", usize::from(dark)), false, cx)
                                .child(self.i18n.text("theme_export"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.theme_begin_export(dark, cx)
                                })),
                        ),
                );
        }
        if let Some(prompt) = &self.themes.export {
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(hint("theme_export_hint"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(settings_theme::label_column().child(format!(
                                "{} ({})",
                                self.i18n.text("theme_export_name"),
                                self.theme_kind_label(prompt.dark)
                            )))
                            .child(div().flex_1().min_w_0().child(prompt.name.clone()))
                            .child(
                                settings_theme::button("theme-export-save", true, cx)
                                    .child(self.i18n.text("theme_export_save"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.theme_choose_export_path(cx)
                                    })),
                            )
                            .child(
                                settings_theme::button("theme-export-cancel", false, cx)
                                    .child(self.i18n.text("cancel"))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.theme_cancel_export(cx)),
                                    ),
                            ),
                    ),
            );
        }
        section = section.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    settings_theme::label_column().child(self.i18n.text("theme_library_heading")),
                )
                .child(
                    settings_theme::button("theme-import", false, cx)
                        .child(self.i18n.text("theme_import"))
                        .on_click(cx.listener(|this, _, _, cx| this.theme_import(cx))),
                ),
        );
        if self.themes.library.is_empty() {
            section = section.child(hint("theme_library_empty"));
        }
        for (index, entry) in self.themes.library.iter().enumerate() {
            let apply_id = entry.id.clone();
            let delete_id = entry.id.clone();
            section =
                section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .ml(settings_theme::LABEL_INDENT)
                        .child(div().flex_1().min_w_0().child(format!(
                            "{} ({})",
                            cayenchat_model::display::neutralize_bidi(&entry.name),
                            self.theme_kind_label(entry.mode == ThemeKind::Dark)
                        )))
                        .child(
                            settings_theme::button(("theme-apply", index), false, cx)
                                .child(self.i18n.text("theme_apply"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.theme_apply(apply_id.clone(), cx)
                                })),
                        )
                        .child(
                            settings_theme::button(("theme-delete", index), false, cx)
                                .child(self.i18n.text("theme_delete"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.theme_delete(&delete_id, cx)
                                })),
                        ),
                );
        }
        section.when_some(self.themes.message.clone(), |d, message| {
            d.child(div().ml(settings_theme::LABEL_INDENT).child(message))
        })
    }

    /// The standard wallpapers of the four areas for light and dark.
    pub(crate) fn render_wallpaper_settings(&self, cx: &mut Context<Self>) -> Div {
        let palette = settings_theme::palette(cx);
        let mut section = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("wallpaper_heading")),
            )
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .text_color(palette.text_secondary)
                    .child(self.i18n.text("wallpaper_hint")),
            );
        for (index, (region, key)) in [
            (WallpaperRegion::ChannelLog, "channel_log"),
            (WallpaperRegion::CombinedLog, "combined_log"),
            (WallpaperRegion::MemberList, "member_list"),
            (WallpaperRegion::ChannelTree, "channel_tree"),
        ]
        .into_iter()
        .enumerate()
        {
            for dark in [false, true] {
                let wallpaper = self
                    .settings
                    .values
                    .appearance
                    .wallpapers(dark)
                    .get(region)
                    .clone();
                let shown = wallpaper.path.as_deref().map_or_else(
                    || self.i18n.text("wallpaper_none"),
                    |path| {
                        Path::new(path)
                            .file_name()
                            .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
                    },
                );
                let id = index * 2 + usize::from(dark);
                section = section.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(settings_theme::label_column().child(format!(
                            "{} ({})",
                            self.i18n.text(key),
                            self.theme_kind_label(dark)
                        )))
                        .child(div().flex_1().min_w_0().child(shown))
                        .child(
                            settings_theme::button(("wallpaper-choose", id), false, cx)
                                .child(self.i18n.text("wallpaper_choose"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.wallpaper_choose(dark, region, cx)
                                })),
                        )
                        .when(wallpaper.path.is_some(), |d| {
                            d.child(
                                settings_theme::button(("wallpaper-clear", id), false, cx)
                                    .child(self.i18n.text("wallpaper_clear"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.wallpaper_clear(dark, region, cx)
                                    })),
                            )
                            .child(
                                settings_theme::button(("wallpaper-weaker", id), false, cx)
                                    .child("−")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.wallpaper_step_opacity(dark, region, -OPACITY_STEP, cx)
                                    })),
                            )
                            .child(format!("{}%", 100 - wallpaper.opacity_percent()))
                            .child(
                                settings_theme::button(("wallpaper-stronger", id), false, cx)
                                    .child("+")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.wallpaper_step_opacity(dark, region, OPACITY_STEP, cx)
                                    })),
                            )
                        }),
                );
            }
        }
        section
    }
}

#[cfg(test)]
mod tests {
    use cayenchat_storage::{Appearance, Settings, ThemeMode};
    use gpui::TestAppContext;

    // Not `super::*`: GPUI's glob would shadow `#[test]`.
    use super::{
        OPACITY_STEP, PathBuf, SettingsWindow, ThemeFile, WallpaperRegion, check_image,
        export_file_name, theme_file,
    };
    use crate::{secrets, settings_file::TestFile, settings_with_channels};
    use gpui::{Entity, VisualTestContext, WindowAppearance};

    fn open<'a>(
        settings: &Settings,
        cx: &'a mut TestAppContext,
    ) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            if !cx.has_global::<secrets::Credentials>() {
                secrets::install_memory(cx);
            }
            cx.set_global(crate::theme::Theme::new(
                ThemeMode::Light,
                WindowAppearance::Light,
                &Appearance::default(),
            ));
        });
        let owner = cx.add_window(|window, cx| {
            crate::ChatWindow::with_settings(settings.clone(), None, window, cx)
        });
        let (form, cx) = cx
            .add_window_view(|window, cx| SettingsWindow::new(owner, settings.clone(), window, cx));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (form, cx)
    }

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::new(8, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn text(form: &Entity<SettingsWindow>, cx: &mut VisualTestContext, dark: bool) -> String {
        form.read_with(cx, |form, cx| {
            let field = if dark {
                &form.settings.dark_channel_log_background
            } else {
                &form.settings.channel_log_background
            };
            field.read(cx).text().to_owned()
        })
    }

    fn status(
        form: &Entity<SettingsWindow>,
        cx: &mut VisualTestContext,
        dark: bool,
    ) -> (String, bool) {
        form.read_with(cx, |form, cx| form.shown_appearance(cx).theme_status(dark))
    }

    #[gpui::test]
    fn the_default_button_returns_one_side_and_clears_the_modified_mark(cx: &mut TestAppContext) {
        let mut settings = settings_with_channels("#a");
        settings.appearance.channel_log_background = "#123456".into();
        settings.appearance.dark.channel_log_background = "#654321".into();
        let _file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        assert_eq!(
            status(&form, cx, false),
            (theme_file::builtin_name(false).into(), true)
        );

        form.update(cx, |form, cx| form.theme_use_default(false, cx));
        assert_eq!(
            status(&form, cx, false),
            (theme_file::builtin_name(false).into(), false)
        );
        assert_eq!(text(&form, cx, false), "#FFFFFF");
        // The dark side is as it was.
        assert_eq!(text(&form, cx, true), "#654321");
        assert!(status(&form, cx, true).1);
    }

    #[gpui::test]
    fn an_imported_theme_is_applied_by_copy_and_survives_its_own_deletion(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let settings = settings_with_channels("#a");
        let _file = TestFile::with(&settings);
        crate::settings_file::use_theme_root(root.path());

        // A dark theme with a wallpaper for the channel tree, in the library.
        let mut source = Appearance::default();
        source.dark.channel_log_background = "#102030".into();
        source.dark.wallpapers.channel_tree.path = Some("tree.png".into());
        source.dark.wallpapers.channel_tree.opacity = Some(70);
        let theme =
            ThemeFile::capture(&source, true, "Night", &check_image, |_| Ok(png())).unwrap();
        let library = root.path().join("themes");
        let entry = theme_file::library_add(&library, &theme).unwrap();

        let (form, cx) = open(&settings, cx);
        form.read_with(cx, |form, _| {
            assert_eq!(form.themes.library, vec![entry.clone()])
        });
        form.update(cx, |form, cx| form.theme_apply(entry.id.clone(), cx));
        cx.run_until_parked();

        assert_eq!(text(&form, cx, true), "#102030");
        assert_eq!(status(&form, cx, true), ("Night".into(), false));
        // The light side keeps its theme.
        assert_eq!(text(&form, cx, false), "#FFFFFF");
        let wallpaper = form.read_with(cx, |form, _| {
            form.settings
                .values
                .appearance
                .dark
                .wallpapers
                .channel_tree
                .clone()
        });
        assert_eq!(wallpaper.opacity, Some(70));
        let path = PathBuf::from(wallpaper.path.unwrap());
        assert!(path.starts_with(root.path().join("wallpapers")) && path.is_file());

        // Editing marks it modified; the name stays.
        form.update(cx, |form, cx| {
            form.settings
                .dark_channel_log_background
                .update(cx, |field, cx| field.set_text("#000001", cx))
        });
        assert_eq!(status(&form, cx, true), ("Night".into(), true));

        // Deleting the theme from the list changes nothing applied.
        let before = form.read_with(cx, |form, cx| form.shown_appearance(cx));
        form.update(cx, |form, cx| form.theme_delete(&entry.id, cx));
        form.read_with(cx, |form, _| assert!(form.themes.library.is_empty()));
        assert_eq!(
            form.read_with(cx, |form, cx| form.shown_appearance(cx)),
            before
        );
        assert!(path.is_file());
    }

    #[gpui::test]
    fn the_wallpaper_strength_steps_and_clearing_forgets_the_picture(cx: &mut TestAppContext) {
        let mut settings = settings_with_channels("#a");
        settings.appearance.wallpapers.member_list.path = Some("/pic.png".into());
        let _file = TestFile::with(&settings);
        let (form, cx) = open(&settings, cx);
        let strength = |form: &Entity<SettingsWindow>, cx: &mut VisualTestContext| {
            form.read_with(cx, |form, _| {
                form.settings
                    .values
                    .appearance
                    .wallpapers
                    .member_list
                    .opacity_percent()
            })
        };
        // The picture's strength is the rest of the background's opacity.
        assert_eq!(100 - strength(&form, cx), 20);
        form.update(cx, |form, cx| {
            form.wallpaper_step_opacity(false, WallpaperRegion::MemberList, OPACITY_STEP, cx)
        });
        assert_eq!(100 - strength(&form, cx), 25);
        form.update(cx, |form, cx| {
            for _ in 0..30 {
                form.wallpaper_step_opacity(false, WallpaperRegion::MemberList, OPACITY_STEP, cx);
            }
        });
        assert_eq!(100 - strength(&form, cx), 100);
        form.update(cx, |form, cx| {
            form.wallpaper_clear(false, WallpaperRegion::MemberList, cx)
        });
        form.read_with(cx, |form, _| {
            let wallpaper = &form.settings.values.appearance.wallpapers.member_list;
            assert!(wallpaper.path.is_none() && wallpaper.opacity.is_none());
        });
    }

    #[test]
    fn export_file_names_are_safe_and_end_in_the_theme_extension() {
        assert_eq!(export_file_name("Night / Blue"), "Night _ Blue.cayentheme");
        assert_eq!(export_file_name("..\\x"), "_x.cayentheme");
        assert_eq!(export_file_name("..."), "theme.cayentheme");
        assert!(export_file_name("日本語").ends_with(".cayentheme"));
    }
}
