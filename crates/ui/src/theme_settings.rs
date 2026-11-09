// SPDX-License-Identifier: GPL-3.0-only
//! Themes and standard wallpapers on the Appearance tab (#962).
//!
//! A theme is a preset: applying one copies its values into the appearance
//! form, and what is edited afterwards never reaches the theme. The tab shows
//! which theme each of the light and dark appearance came from and whether it
//! was edited since; imported themes are listed to apply or delete, and the
//! current light or dark appearance can be exported.

use std::{
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use cayenchat_media::wallpaper;
use cayenchat_model::attachment::AttachmentSource;
use cayenchat_storage::theme_file::{
    self, LibraryEntry, MAX_THEME_FILE_BYTES, MAX_THEME_WALLPAPER_BYTES, THEME_EXTENSION,
};
use cayenchat_storage::{Appearance, ThemeFile, ThemeKind, ThemeMode, WallpaperRegion};
use gpui::{prelude::*, *};

use crate::{
    SettingsWindow, channel_look,
    image_upload::{acceptable_attachment, image_file_filter, read_in_background},
    input::TextInput,
    settings_theme,
};

/// The four areas in the order of the chat window's panes: the label key of
/// each.
const AREAS: [(WallpaperRegion, &str); 4] = [
    (WallpaperRegion::ChannelLog, "channel_log"),
    (WallpaperRegion::CombinedLog, "combined_log"),
    (WallpaperRegion::MemberList, "member_list"),
    (WallpaperRegion::ChannelTree, "channel_tree"),
];

/// What the wallpaper section edits and how the last image went.
pub(crate) struct WallpaperUi {
    /// The side being edited: light or dark.
    pub(crate) dark: bool,
    /// The area whose controls are shown.
    pub(crate) region: WallpaperRegion,
    /// "Opening the image…", or why it could not be used (failed).
    pub(crate) status: Option<(String, bool)>,
    /// Reading and checking the latest image; replacing it drops an older
    /// one.
    task: Option<Task<()>>,
    /// The miniature's pictures, loaded like the chat window's
    /// (`channel_look::Wallpaper`: size limit, decode gate, released when an
    /// area has none).
    preview: channel_look::Wallpapers,
    /// The sliders of the selected area's strength and blur.
    strength_track: crate::slider::TrackBounds,
    strength_focus: FocusHandle,
    blur_track: crate::slider::TrackBounds,
    blur_focus: FocusHandle,
}

impl WallpaperUi {
    fn select(&mut self, dark: bool, region: WallpaperRegion) {
        self.dark = dark;
        self.region = region;
        self.status = None;
    }
}

/// Keeps `bytes` (already checked) as a file named by its content in
/// `directory` and returns its path, so the wallpaper no longer depends on
/// the original file or a temporary drop. The prefix keeps theme pruning off
/// it, and equal images share one file.
fn keep_image(directory: &Path, bytes: &[u8], extension: &str) -> Result<String, String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let mut hasher = std::hash::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let path = directory.join(format!("wallpaper-{:016x}.{extension}", hasher.finish()));
    if !path.is_file() {
        std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    }
    Ok(path.to_string_lossy().into_owned())
}

/// What the theme section holds besides the form's values.
pub(crate) struct ThemeUi {
    pub(crate) wallpaper: WallpaperUi,
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
    /// `dark`: the theme in use, which the wallpaper section edits first, so
    /// what is set there is what the chat window shows.
    pub(crate) fn new(dark: bool, cx: &mut Context<SettingsWindow>) -> Self {
        Self {
            wallpaper: WallpaperUi {
                dark,
                region: WallpaperRegion::ChannelLog,
                status: None,
                task: None,
                preview: channel_look::Wallpapers::default(),
                strength_track: Default::default(),
                strength_focus: cx.focus_handle().tab_stop(true),
                blur_track: Default::default(),
                blur_focus: cx.focus_handle().tab_stop(true),
            },
            library: crate::settings_file::library_directory()
                .map(|directory| theme_file::library_list(&directory))
                .unwrap_or_default(),
            busy: false,
            message: None,
            export: None,
        }
    }
}

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

    /// Asks for an image for the area of the light or dark theme. The file
    /// dialog and a drop on the area end in the same `wallpaper_offer`.
    pub(crate) fn wallpaper_choose(
        &mut self,
        dark: bool,
        region: WallpaperRegion,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.i18n.text("wallpaper_choose").into()),
            filter: Some(image_file_filter(&self.i18n)),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.wallpaper_offer(dark, region, paths, None, AttachmentSource::Chooser, cx)
            });
        })
        .detach();
    }

    /// Reads the image named by `paths` (one file; `keep` holds a macOS file
    /// promise until it is read), checks it, keeps a copy of it in the
    /// application's own `wallpapers` directory and makes that copy the
    /// standard wallpaper of `region` in the light or dark theme. The target
    /// is fixed here, so an answer that comes late never lands on whatever
    /// is selected by then. A failure shows its reason and leaves the earlier
    /// wallpaper as it was.
    pub(crate) fn wallpaper_offer(
        &mut self,
        dark: bool,
        region: WallpaperRegion,
        paths: Vec<PathBuf>,
        keep: Option<Arc<PromisedFiles>>,
        source: AttachmentSource,
        cx: &mut Context<Self>,
    ) {
        self.themes.wallpaper.select(dark, region);
        self.themes.wallpaper.status = Some((self.i18n.text("wallpaper_loading"), false));
        let read = read_in_background(paths, keep, source, cx);
        let i18n = self.i18n.clone();
        let directory = crate::settings_file::wallpaper_directory();
        self.themes.wallpaper.task = Some(cx.spawn(async move |this, cx| {
            let outcome: Result<String, String> = match read.await {
                Err(error) => Err(error.text(&i18n)),
                Ok(attachment) => match acceptable_attachment(attachment, None, &i18n) {
                    Err(error) => Err(error),
                    Ok(attachment) => {
                        cx.background_executor()
                            .spawn(async move {
                                let extension = check_image(&attachment.bytes)?;
                                keep_image(&directory?, &attachment.bytes, extension)
                            })
                            .await
                    }
                },
            };
            let _ = this.update(cx, |this, cx| {
                this.themes.wallpaper.task = None;
                this.themes.wallpaper.status = match outcome {
                    Ok(path) if path.len() <= cayenchat_storage::MAX_WALLPAPER_PATH_BYTES => {
                        this.settings
                            .values
                            .appearance
                            .wallpapers_mut(dark)
                            .get_mut(region)
                            .path = Some(path);
                        None
                    }
                    Ok(_) => Some((this.i18n.text("channel_look_wallpaper_long"), true)),
                    Err(error) => Some((
                        this.i18n.format("wallpaper_failed", &[("error", &error)]),
                        true,
                    )),
                };
                cx.notify();
            });
        }));
        cx.notify();
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
        wallpaper.blur = None;
        cx.notify();
    }

    /// Sets the strength of the picture, in percent. The stored opacity is the
    /// background's over the picture; the strength shown is the rest.
    pub(crate) fn wallpaper_set_strength(
        &mut self,
        dark: bool,
        region: WallpaperRegion,
        strength: u8,
        cx: &mut Context<Self>,
    ) {
        self.settings
            .values
            .appearance
            .wallpapers_mut(dark)
            .get_mut(region)
            .opacity = Some(100 - strength.min(100));
        cx.notify();
    }

    /// Sets the blur of the picture, in tenths of a pixel.
    pub(crate) fn wallpaper_set_blur(
        &mut self,
        dark: bool,
        region: WallpaperRegion,
        tenths: u8,
        cx: &mut Context<Self>,
    ) {
        self.settings
            .values
            .appearance
            .wallpapers_mut(dark)
            .get_mut(region)
            .blur = Some(tenths.min(cayenchat_storage::MAX_WALLPAPER_BLUR));
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

    /// The standard wallpapers: a miniature of the four areas laid out as in
    /// the chat window, each a drop target for one image, for the light or
    /// the dark theme (one at a time), and the controls of the selected area.
    pub(crate) fn render_wallpaper_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let palette = settings_theme::palette(cx);
        let WallpaperUi {
            dark,
            region: selected,
            ..
        } = self.themes.wallpaper;
        let appearance = self.shown_appearance(cx);
        // The chat window's own resolution (`channel_look::region_wallpaper`)
        // over the pane colors of the theme being edited.
        let theme = crate::theme::Theme::new(
            if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            WindowAppearance::Light,
            &appearance,
        );
        // Opaque: the miniature shows the mix of picture and color, which the
        // pane's opacity (the desktop showing through) does not change, on
        // the settings window instead of the desktop.
        let pane_of = |region: WallpaperRegion| {
            let pane = match region {
                WallpaperRegion::ChannelLog => theme.panes.channel_log,
                WallpaperRegion::CombinedLog => theme.panes.combined_log,
                WallpaperRegion::MemberList => theme.panes.member_list,
                WallpaperRegion::ChannelTree => theme.panes.channel_tree,
            };
            Rgba { a: 1., ..pane }
        };
        // The pictures: the chat window's loading and its layers over them.
        let layers = AREAS.map(|(region, _)| {
            let wallpaper = appearance.wallpapers(dark).get(region);
            let (request, over) = channel_look::region_wallpaper(wallpaper, pane_of(region));
            self.themes.wallpaper.preview.get_mut(region).element(
                request.as_ref(),
                over,
                false,
                window,
                cx,
                move |this: &mut Self| this.themes.wallpaper.preview.get_mut(region),
            )
        });
        let area =
            |region: WallpaperRegion, key: &str, layer: Option<Div>, cx: &mut Context<Self>| {
                let pane = pane_of(region);
                let has_layer = layer.is_some();
                let wallpaper = appearance.wallpapers(dark).get(region);
                let id = AREAS.iter().position(|(r, _)| *r == region).unwrap_or(0);
                let chosen = region == selected;
                let ring = palette.link;
                let hover = palette.selected;
                let drop_ring = theme.text;
                div()
                    .id(("wallpaper-area", id))
                    .relative()
                    .overflow_hidden()
                    .flex_1()
                    .min_h(px(70.))
                    .p_1()
                    .border_2()
                    .border_color(if chosen { ring } else { palette.border })
                    .when(!has_layer, |d| d.bg(pane))
                    .text_color(theme.text)
                    .text_size(px(12.))
                    .cursor_pointer()
                    // Only the area under the pointer is marked and receives the
                    // drop; the others are separate targets.
                    // The mark differs from the selected area's ring, so a
                    // selected area elsewhere is not taken for the target.
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.border_color(drop_ring).bg(hover)
                    })
                    .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                        let dark = this.themes.wallpaper.dark;
                        this.wallpaper_offer(
                            dark,
                            region,
                            paths.paths().to_vec(),
                            paths.keep_alive(),
                            AttachmentSource::Drop,
                            cx,
                        )
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let dark = this.themes.wallpaper.dark;
                        this.themes.wallpaper.select(dark, region);
                        cx.notify();
                    }))
                    .children(layer)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(self.i18n.text(key))
                            .when(wallpaper.path.is_none(), |d| {
                                d.child(
                                    div()
                                        .text_color(theme.text_muted)
                                        .child(self.i18n.text("wallpaper_none")),
                                )
                            })
                            .child(
                                settings_theme::button(("wallpaper-choose", id), false, cx)
                                    .text_size(px(11.))
                                    .child(self.i18n.text("wallpaper_choose"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let dark = this.themes.wallpaper.dark;
                                        this.themes.wallpaper.select(dark, region);
                                        this.wallpaper_choose(dark, region, cx)
                                    })),
                            ),
                    )
            };
        let mut layers = layers.into_iter();
        let [log, combined, members, tree] =
            AREAS.map(|(region, key)| area(region, key, layers.next().flatten(), cx));
        let preview = div()
            .ml(settings_theme::LABEL_INDENT)
            .flex()
            .gap_1()
            .w(px(520.))
            .h(px(280.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_1()
                    .child(log)
                    .child(combined),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(170.))
                    .child(members)
                    .child(tree),
            );

        let wallpaper = appearance.wallpapers(dark).get(selected).clone();
        let area_name = AREAS
            .iter()
            .find(|(region, _)| *region == selected)
            .map_or("", |(_, key)| *key);
        let shown = wallpaper.path.as_deref().map_or_else(
            || self.i18n.text("wallpaper_none"),
            |path| {
                Path::new(path)
                    .file_name()
                    .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
            },
        );
        let mode_button = |mode: bool, cx: &mut Context<Self>| {
            settings_theme::button(("wallpaper-mode", usize::from(mode)), mode == dark, cx)
                .child(self.theme_kind_label(mode))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.themes.wallpaper.dark = mode;
                    this.themes.wallpaper.status = None;
                    cx.notify();
                }))
        };
        let controls = div()
            .ml(settings_theme::LABEL_INDENT)
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(format!(
                "{} ({}): {}",
                self.i18n.text(area_name),
                self.theme_kind_label(dark),
                shown
            ))
            .child(
                settings_theme::button("wallpaper-change", false, cx)
                    .child(self.i18n.text("wallpaper_change"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let dark = this.themes.wallpaper.dark;
                        let region = this.themes.wallpaper.region;
                        this.wallpaper_choose(dark, region, cx)
                    })),
            )
            .when(wallpaper.path.is_some(), |d| {
                d.child(
                    settings_theme::button("wallpaper-clear", false, cx)
                        .child(self.i18n.text("wallpaper_clear"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let WallpaperUi { dark, region, .. } = this.themes.wallpaper;
                            this.wallpaper_clear(dark, region, cx)
                        })),
                )
            });
        let primary = settings_theme::primary_color(cx);
        let colors = crate::slider::SliderColors {
            track: palette.border,
            fill: primary.into(),
            knob: palette.surface,
            ring: primary.into(),
        };
        let strength = 100 - wallpaper.opacity_percent();
        let blur = wallpaper.blur_tenths();
        let sliders = wallpaper.path.is_some().then(|| {
            let row = |label: String, slider: Stateful<Div>| {
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(230.)).flex_shrink_0().child(label))
                    .child(div().w(px(220.)).flex_shrink_0().child(slider))
            };
            let ui = &self.themes.wallpaper;
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(row(
                    self.i18n
                        .format("wallpaper_strength", &[("value", &strength.to_string())]),
                    crate::slider::slider(
                        "wallpaper-strength",
                        u32::from(strength),
                        100,
                        &ui.strength_track,
                        &ui.strength_focus,
                        colors,
                        cx,
                        move |this, value, _, cx| {
                            let WallpaperUi { dark, region, .. } = this.themes.wallpaper;
                            this.wallpaper_set_strength(dark, region, value as u8, cx)
                        },
                    ),
                ))
                .child(row(
                    self.i18n.format(
                        "channel_look_blur",
                        &[("value", &format!("{}.{}", blur / 10, blur % 10))],
                    ),
                    crate::slider::slider(
                        "wallpaper-blur",
                        u32::from(blur),
                        u32::from(cayenchat_storage::MAX_WALLPAPER_BLUR),
                        &ui.blur_track,
                        &ui.blur_focus,
                        colors,
                        cx,
                        move |this, value, _, cx| {
                            let WallpaperUi { dark, region, .. } = this.themes.wallpaper;
                            this.wallpaper_set_blur(dark, region, value as u8, cx)
                        },
                    ),
                ))
        });
        div()
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
            )
            .child(
                div()
                    .ml(settings_theme::LABEL_INDENT)
                    .flex()
                    .gap_2()
                    .child(mode_button(false, cx))
                    .child(mode_button(true, cx)),
            )
            .child(preview)
            .child(controls)
            .children(sliders)
            .when_some(self.themes.wallpaper.status.clone(), |d, (text, failed)| {
                d.child(
                    div()
                        .ml(settings_theme::LABEL_INDENT)
                        .text_color(if failed {
                            palette.warning
                        } else {
                            palette.text_secondary
                        })
                        .child(text),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use cayenchat_storage::{Appearance, Settings, ThemeMode};
    use gpui::TestAppContext;

    // Not `super::*`: GPUI's glob would shadow `#[test]`.
    use super::{
        AttachmentSource, PathBuf, SettingsWindow, ThemeFile, WallpaperRegion, channel_look,
        check_image, export_file_name, theme_file,
    };
    use crate::{secrets, settings_file::TestFile, settings_with_channels};
    use gpui::{Entity, VisualTestContext, WindowAppearance};

    fn open<'a>(
        settings: &Settings,
        cx: &'a mut TestAppContext,
    ) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
        open_in(settings, ThemeMode::Light, cx)
    }

    fn open_in<'a>(
        settings: &Settings,
        mode: ThemeMode,
        cx: &'a mut TestAppContext,
    ) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
        let mut settings = settings.clone();
        settings.theme = mode;
        let settings = &settings;
        cx.update(|cx| {
            if !cx.has_global::<secrets::Credentials>() {
                secrets::install_memory(cx);
            }
            cx.set_global(crate::theme::Theme::new(
                mode,
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
    fn a_dropped_image_becomes_the_wallpaper_of_its_area_only_and_is_saved(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let settings = settings_with_channels("#a");
        let _file = TestFile::with(&settings);
        crate::settings_file::use_theme_root(root.path());
        let (form, cx) = open(&settings, cx);
        let source = root.path().join("original.png");
        std::fs::write(&source, png()).unwrap();

        form.update(cx, |form, cx| {
            form.wallpaper_offer(
                true,
                WallpaperRegion::ChannelTree,
                vec![source.clone()],
                None,
                AttachmentSource::Drop,
                cx,
            )
        });
        cx.run_until_parked();
        // The original may vanish (a temporary drop): the kept copy remains.
        std::fs::remove_file(&source).unwrap();

        let appearance = form.read_with(cx, |form, _| form.settings.values.appearance.clone());
        let kept = PathBuf::from(
            appearance
                .dark
                .wallpapers
                .channel_tree
                .path
                .clone()
                .unwrap(),
        );
        assert!(kept.starts_with(root.path().join("wallpapers")) && kept.is_file());
        // No other area or side changed.
        let mut expected = appearance.clone();
        expected.dark.wallpapers.channel_tree = Default::default();
        assert_eq!(expected.wallpapers, Default::default());
        assert_eq!(expected.dark.wallpapers, Default::default());
        form.read_with(cx, |form, _| {
            assert!(form.themes.wallpaper.status.is_none());
            assert!(form.themes.wallpaper.dark);
            assert_eq!(form.themes.wallpaper.region, WallpaperRegion::ChannelTree);
        });

        // Saved, and handed to the chat window.
        form.update(cx, |form, cx| form.autosave_now(None, cx));
        cx.run_until_parked();
        form.read_with(cx, |form, _| {
            assert_eq!(
                form.saved.appearance.dark.wallpapers.channel_tree.path,
                appearance.dark.wallpapers.channel_tree.path
            );
        });
        let on_disk = crate::settings_file::load().unwrap().unwrap();
        assert_eq!(
            on_disk.appearance.dark.wallpapers.channel_tree.path,
            appearance.dark.wallpapers.channel_tree.path
        );

        // The chat window asks to draw it in that area and side only.
        let wanted = |form: &Entity<SettingsWindow>, cx: &mut VisualTestContext, dark: bool| {
            form.update(cx, |form, cx| {
                form.owner
                    .update(cx, |chat, _, _| {
                        let theme = crate::theme::Theme::new(
                            if dark {
                                ThemeMode::Dark
                            } else {
                                ThemeMode::Light
                            },
                            WindowAppearance::Light,
                            &chat.appearance,
                        );
                        let standard = chat.appearance.wallpapers(dark);
                        let path = |wallpaper, pane| {
                            channel_look::region_wallpaper(wallpaper, pane)
                                .0
                                .map(|request| request.path)
                        };
                        [
                            path(&standard.channel_log, theme.panes.channel_log),
                            path(&standard.combined_log, theme.panes.combined_log),
                            path(&standard.member_list, theme.panes.member_list),
                            path(&standard.channel_tree, theme.panes.channel_tree),
                        ]
                    })
                    .unwrap()
            })
        };
        assert_eq!(
            wanted(&form, cx, true),
            [
                None,
                None,
                None,
                appearance.dark.wallpapers.channel_tree.path.clone()
            ]
        );
        assert_eq!(wanted(&form, cx, false), [None, None, None, None]);
        // The miniature loaded the same picture through the chat window's
        // loader (limits, decode gate).
        form.update(cx, |form, cx| {
            form.tab = crate::settings_window::SettingsTab::Appearance;
            cx.notify();
        });
        cx.run_until_parked();
        form.update(cx, |form, _| {
            let preview = &mut form.themes.wallpaper.preview;
            assert!(
                preview
                    .get_mut(WallpaperRegion::ChannelTree)
                    .slice(channel_look::Layers::plain(gpui::rgba(0xffffffff)))
                    .is_some()
            );
            assert!(
                preview
                    .get_mut(WallpaperRegion::ChannelLog)
                    .slice(channel_look::Layers::plain(gpui::rgba(0xffffffff)))
                    .is_none()
            );
        });

        // A bad file is refused with a reason and the earlier one stays.
        let bad = root.path().join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();
        form.update(cx, |form, cx| {
            form.wallpaper_offer(
                true,
                WallpaperRegion::ChannelTree,
                vec![bad],
                None,
                AttachmentSource::Chooser,
                cx,
            )
        });
        cx.run_until_parked();
        form.read_with(cx, |form, _| {
            assert!(form.themes.wallpaper.status.as_ref().unwrap().1);
            assert_eq!(
                form.settings
                    .values
                    .appearance
                    .dark
                    .wallpapers
                    .channel_tree
                    .path,
                appearance.dark.wallpapers.channel_tree.path
            );
        });
    }

    #[gpui::test]
    fn the_wallpaper_section_starts_on_the_theme_in_use(cx: &mut TestAppContext) {
        let settings = settings_with_channels("#a");
        let _file = TestFile::with(&settings);
        let (form, cx) = open_in(&settings, ThemeMode::Dark, cx);
        form.read_with(cx, |form, _| assert!(form.themes.wallpaper.dark));
        let (form, cx) = open_in(&settings, ThemeMode::Light, cx);
        form.read_with(cx, |form, _| assert!(!form.themes.wallpaper.dark));
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
        // One percent at a time, and the blur in tenths up to its limit.
        form.update(cx, |form, cx| {
            form.wallpaper_set_strength(false, WallpaperRegion::MemberList, 21, cx);
            form.wallpaper_set_blur(false, WallpaperRegion::MemberList, 12, cx)
        });
        assert_eq!(100 - strength(&form, cx), 21);
        form.update(cx, |form, cx| {
            form.wallpaper_set_strength(false, WallpaperRegion::MemberList, 250, cx);
            form.wallpaper_set_blur(false, WallpaperRegion::MemberList, 250, cx)
        });
        assert_eq!(100 - strength(&form, cx), 100);
        form.read_with(cx, |form, _| {
            let wallpaper = &form.settings.values.appearance.wallpapers.member_list;
            assert_eq!(wallpaper.blur, Some(cayenchat_storage::MAX_WALLPAPER_BLUR));
        });
        form.update(cx, |form, cx| {
            form.wallpaper_clear(false, WallpaperRegion::MemberList, cx)
        });
        form.read_with(cx, |form, _| {
            let wallpaper = &form.settings.values.appearance.wallpapers.member_list;
            assert!(
                wallpaper.path.is_none() && wallpaper.opacity.is_none() && wallpaper.blur.is_none()
            );
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
