// SPDX-License-Identifier: MPL-2.0
//! Appearance themes (#962): the light or dark half of the appearance as a
//! preset that can be saved, shared and applied.
//!
//! A theme is a source, not the current setting: applying one copies its
//! values into [`Appearance`], and nothing edited afterwards reaches the
//! theme. [`ThemeOrigin`] only remembers which theme the values came from and
//! a fingerprint of them, so the settings can say "modified" without keeping
//! a second copy.
//!
//! The `.cayentheme` file is one JSON document, gzip-compressed on export.
//! Import takes the compressed and the plain form alike (told apart by the
//! gzip magic bytes), and checks the sizes before it trusts anything in it.

use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{Appearance, DEFAULT_WALLPAPER_OPACITY, clamp_font_size, color_value, private_file};

/// The file name extension of a theme file, without the dot.
pub const THEME_EXTENSION: &str = "cayentheme";
/// The format version written; a file with another one is refused.
pub const THEME_VERSION: u32 = 1;
/// The largest file read, compressed or not.
pub const MAX_THEME_FILE_BYTES: u64 = 48 * 1024 * 1024;
/// The largest JSON document a (compressed) file may expand to. Four
/// wallpapers of [`MAX_THEME_WALLPAPER_BYTES`] in Base64 fit with room.
pub const MAX_THEME_JSON_BYTES: u64 = 64 * 1024 * 1024;
/// The largest wallpaper image a theme carries, after Base64 decoding.
pub const MAX_THEME_WALLPAPER_BYTES: usize = 8 * 1024 * 1024;
/// The longest theme name kept, in characters.
pub const MAX_THEME_NAME_CHARS: usize = 80;
/// Themes the library holds.
pub const MAX_LIBRARY_THEMES: usize = 64;
/// The name of the built-in light and dark themes.
pub const DEFAULT_LIGHT_NAME: &str = "CayenChat Default Light";
pub const DEFAULT_DARK_NAME: &str = "CayenChat Default Dark";

/// The areas that take a wallpaper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallpaperRegion {
    ChannelLog,
    CombinedLog,
    MemberList,
    ChannelTree,
}

impl WallpaperRegion {
    pub const ALL: [Self; 4] = [
        Self::ChannelLog,
        Self::CombinedLog,
        Self::MemberList,
        Self::ChannelTree,
    ];

    /// The key in files.
    pub fn key(self) -> &'static str {
        match self {
            Self::ChannelLog => "channel_log",
            Self::CombinedLog => "combined_log",
            Self::MemberList => "member_list",
            Self::ChannelTree => "channel_tree",
        }
    }
}

/// The standard wallpaper of one area for one of the themes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RegionWallpaper {
    /// Path of an image file. For an applied theme it is a file the app
    /// keeps itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// How much of it shows over the background, in percent; unset is
    /// [`DEFAULT_WALLPAPER_OPACITY`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<u8>,
}

impl RegionWallpaper {
    pub fn opacity_percent(&self) -> u8 {
        self.opacity.unwrap_or(DEFAULT_WALLPAPER_OPACITY).min(100)
    }
}

/// The standard wallpapers of the four areas.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RegionWallpapers {
    pub channel_log: RegionWallpaper,
    pub combined_log: RegionWallpaper,
    pub member_list: RegionWallpaper,
    pub channel_tree: RegionWallpaper,
}

impl RegionWallpapers {
    pub fn get(&self, region: WallpaperRegion) -> &RegionWallpaper {
        match region {
            WallpaperRegion::ChannelLog => &self.channel_log,
            WallpaperRegion::CombinedLog => &self.combined_log,
            WallpaperRegion::MemberList => &self.member_list,
            WallpaperRegion::ChannelTree => &self.channel_tree,
        }
    }

    pub fn get_mut(&mut self, region: WallpaperRegion) -> &mut RegionWallpaper {
        match region {
            WallpaperRegion::ChannelLog => &mut self.channel_log,
            WallpaperRegion::CombinedLog => &mut self.combined_log,
            WallpaperRegion::MemberList => &mut self.member_list,
            WallpaperRegion::ChannelTree => &mut self.channel_tree,
        }
    }

    pub(crate) fn clean(&mut self) {
        for region in WallpaperRegion::ALL {
            let wallpaper = self.get_mut(region);
            if wallpaper
                .path
                .as_deref()
                .is_some_and(|p| p.trim().is_empty() || p.len() > crate::MAX_WALLPAPER_PATH_BYTES)
            {
                wallpaper.path = None;
            }
            wallpaper.opacity = wallpaper.opacity.map(|p| p.min(100));
        }
    }
}

/// Where the values of one theme (light or dark) came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeOrigin {
    /// The theme applied last; unset is the built-in default theme.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// [`Appearance::fingerprint`] of the values right after applying it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// The color settings of one theme, light or dark.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeColors {
    pub member_list_background: String,
    pub channel_tree_background: String,
    pub channel_log_background: String,
    pub channel_log_alternate: String,
    pub channel_event_color: String,
    pub notice_color: String,
    pub highlight_color: String,
    pub combined_log_background: String,
    pub combined_log_alternate: String,
    pub url_tooltip_color: String,
    pub url_tooltip_link_color: String,
    pub url_tooltip_text_color: String,
    pub link_color: String,
    /// Empty is automatic.
    pub link_background: String,
}

impl ThemeColors {
    fn defaults(dark: bool) -> Self {
        Appearance::default().theme_colors(dark)
    }
}

/// A font family and size (0 is the standard size) of one area.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeFont {
    pub family: String,
    pub size: u16,
}

/// The fonts, which the light and dark appearance share.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeFonts {
    pub channel_log: ThemeFont,
    pub combined_log: ThemeFont,
    pub member_list: ThemeFont,
    pub channel_tree: ThemeFont,
    pub input: ThemeFont,
    pub time: ThemeFont,
}

impl ThemeFonts {
    fn clean(&mut self) {
        for font in [
            &mut self.channel_log,
            &mut self.combined_log,
            &mut self.member_list,
            &mut self.channel_tree,
            &mut self.input,
            &mut self.time,
        ] {
            font.size = clamp_font_size(font.size);
            // A family is a name, not a document.
            font.family = font
                .family
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect();
        }
    }
}

/// A wallpaper inside a theme file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeWallpaper {
    /// The image file (PNG, JPEG, GIF or WebP), Base64.
    pub data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeWallpapers {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_log: Option<ThemeWallpaper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined_log: Option<ThemeWallpaper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_list: Option<ThemeWallpaper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_tree: Option<ThemeWallpaper>,
}

impl ThemeWallpapers {
    pub fn get(&self, region: WallpaperRegion) -> Option<&ThemeWallpaper> {
        match region {
            WallpaperRegion::ChannelLog => self.channel_log.as_ref(),
            WallpaperRegion::CombinedLog => self.combined_log.as_ref(),
            WallpaperRegion::MemberList => self.member_list.as_ref(),
            WallpaperRegion::ChannelTree => self.channel_tree.as_ref(),
        }
    }

    fn slot(&mut self, region: WallpaperRegion) -> &mut Option<ThemeWallpaper> {
        match region {
            WallpaperRegion::ChannelLog => &mut self.channel_log,
            WallpaperRegion::CombinedLog => &mut self.combined_log,
            WallpaperRegion::MemberList => &mut self.member_list,
            WallpaperRegion::ChannelTree => &mut self.channel_tree,
        }
    }
}

/// Which half of the appearance a theme is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    Light,
    Dark,
}

impl ThemeKind {
    pub fn is_dark(self) -> bool {
        self == Self::Dark
    }

    pub fn from_dark(dark: bool) -> Self {
        if dark { Self::Dark } else { Self::Light }
    }
}

/// A `.cayentheme` document. It holds no server, channel or path of the
/// person who made it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeFile {
    pub version: u32,
    pub name: String,
    pub mode: ThemeKind,
    /// Color settings by key. A key left out takes the built-in default; a
    /// key this version does not know is ignored.
    #[serde(default)]
    pub colors: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_tooltip_opacity: Option<u8>,
    /// Left out, the fonts stay as they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fonts: Option<ThemeFonts>,
    #[serde(default)]
    pub wallpapers: ThemeWallpapers,
}

/// What can go wrong with a theme file. The text is for the person.
pub type ThemeError = String;

/// Checks that a wallpaper image can be drawn and returns the extension of
/// its format. The caller supplies it because decoding belongs to the media
/// crate.
pub type ImageCheck<'a> = &'a dyn Fn(&[u8]) -> Result<&'static str, String>;

/// A theme name made safe to show: control and bidirectional control
/// characters are dropped, the ends trimmed, the length bounded.
pub fn clean_theme_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    *c,
                    '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
                )
        })
        .collect();
    kept.trim().chars().take(MAX_THEME_NAME_CHARS).collect()
}

impl ThemeFile {
    /// The built-in theme for light or dark: the standard colors, standard
    /// fonts and no wallpaper. It is never stored.
    pub fn builtin(dark: bool) -> Self {
        let defaults = Appearance::default();
        Self {
            version: THEME_VERSION,
            name: if dark {
                DEFAULT_DARK_NAME
            } else {
                DEFAULT_LIGHT_NAME
            }
            .into(),
            mode: ThemeKind::from_dark(dark),
            colors: color_map(&defaults.theme_colors(dark)),
            url_tooltip_opacity: Some(defaults.url_tooltip_opacity),
            fonts: Some(defaults.theme_fonts()),
            wallpapers: ThemeWallpapers::default(),
        }
    }

    /// The light or dark half of `appearance` as a theme called `name`.
    /// `read` returns the bytes of a wallpaper file; one that cannot be read
    /// or is too large fails the export, so a theme never silently loses a
    /// picture.
    pub fn capture(
        appearance: &Appearance,
        dark: bool,
        name: &str,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, ThemeError> {
        let name = clean_theme_name(name);
        if name.is_empty() {
            return Err("The theme needs a name.".into());
        }
        let mut wallpapers = ThemeWallpapers::default();
        for region in WallpaperRegion::ALL {
            let wallpaper = appearance.wallpapers(dark).get(region);
            let Some(path) = &wallpaper.path else {
                continue;
            };
            let bytes = read(path)?;
            if bytes.len() > MAX_THEME_WALLPAPER_BYTES {
                return Err(format!(
                    "A wallpaper is larger than {} MiB.",
                    MAX_THEME_WALLPAPER_BYTES / (1024 * 1024)
                ));
            }
            *wallpapers.slot(region) = Some(ThemeWallpaper {
                data: STANDARD.encode(bytes),
                opacity: wallpaper.opacity,
            });
        }
        Ok(Self {
            version: THEME_VERSION,
            name,
            mode: ThemeKind::from_dark(dark),
            colors: color_map(&appearance.theme_colors(dark)),
            url_tooltip_opacity: Some(appearance.url_tooltip_opacity),
            fonts: Some(appearance.theme_fonts()),
            wallpapers,
        })
    }

    /// The gzip-compressed JSON that is written to a `.cayentheme` file.
    pub fn to_gzip(&self) -> Result<Vec<u8>, ThemeError> {
        let json = serde_json::to_vec(self).map_err(|e| format!("Could not write theme: {e}"))?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&json)
            .and_then(|()| encoder.finish())
            .map_err(|e| format!("Could not compress theme: {e}"))
    }

    /// Reads a theme from a file's bytes: gzip-compressed or plain JSON,
    /// told apart by content. Everything is checked: the sizes before and
    /// after expanding, the version, the colors and numbers, and each image.
    /// The result is cleaned, so applying it needs no further checks.
    pub fn parse(bytes: &[u8], check_image: ImageCheck<'_>) -> Result<Self, ThemeError> {
        if bytes.len() as u64 > MAX_THEME_FILE_BYTES {
            return Err("The theme file is too large.".into());
        }
        let json = if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut json = Vec::new();
            GzDecoder::new(bytes)
                .take(MAX_THEME_JSON_BYTES + 1)
                .read_to_end(&mut json)
                .map_err(|_| "The theme file is damaged.".to_owned())?;
            if json.len() as u64 > MAX_THEME_JSON_BYTES {
                return Err("The theme file is too large once expanded.".into());
            }
            std::borrow::Cow::Owned(json)
        } else {
            std::borrow::Cow::Borrowed(bytes)
        };
        let mut file: ThemeFile = serde_json::from_slice(&json)
            .map_err(|_| "This is not a CayenChat theme file.".to_owned())?;
        file.check(check_image)?;
        Ok(file)
    }

    fn check(&mut self, check_image: ImageCheck<'_>) -> Result<(), ThemeError> {
        if self.version != THEME_VERSION {
            return Err(format!(
                "Theme file version {} is not supported.",
                self.version
            ));
        }
        self.name = clean_theme_name(&self.name);
        if self.name.is_empty() {
            return Err("The theme has no name.".into());
        }
        // Resolve the colors now, so a bad value fails the import and not
        // the later apply.
        self.colors = color_map(&self.resolve_colors()?);
        if let Some(opacity) = self.url_tooltip_opacity
            && !(crate::MIN_URL_TOOLTIP_OPACITY..=100).contains(&opacity)
        {
            return Err("The popup opacity is out of range.".into());
        }
        if let Some(fonts) = &mut self.fonts {
            fonts.clean();
        }
        for region in WallpaperRegion::ALL {
            let slot = self.wallpapers.slot(region);
            if let Some(wallpaper) = slot {
                if wallpaper.opacity.is_some_and(|o| o > 100) {
                    return Err("A wallpaper opacity is out of range.".into());
                }
                wallpaper_bytes(wallpaper, check_image)?;
            }
        }
        Ok(())
    }

    /// The built-in colors for the mode with the file's keys laid over.
    fn resolve_colors(&self) -> Result<ThemeColors, ThemeError> {
        let mut base = color_map(&ThemeColors::defaults(self.mode.is_dark()));
        for (key, value) in &self.colors {
            // Keys of a newer version are ignored.
            let Some(slot) = base.get_mut(key) else {
                continue;
            };
            let Some(text) = value.as_str() else {
                return Err(format!("The color {key} is not text."));
            };
            let text = text.trim();
            let allowed_empty = key == "link_background" && text.is_empty();
            if !allowed_empty && color_value(text).is_none() {
                return Err(format!("The color {key} must be #RRGGBB."));
            }
            *slot = Value::String(text.to_ascii_uppercase());
        }
        serde_json::from_value(Value::Object(base)).map_err(|e| format!("Bad colors: {e}"))
    }

    /// Copies the theme into the matching half of `appearance` and notes it
    /// as the origin. Wallpaper images are written under `wallpaper_dir`
    /// (named by content, so applying again reuses the file), which makes the
    /// applied appearance independent of the theme file and of the library.
    /// `appearance` is untouched when this fails.
    pub fn apply(
        &self,
        appearance: &mut Appearance,
        wallpaper_dir: &Path,
        check_image: ImageCheck<'_>,
    ) -> Result<(), ThemeError> {
        let dark = self.mode.is_dark();
        let colors = self.resolve_colors()?;
        let mut wallpapers = RegionWallpapers::default();
        for region in WallpaperRegion::ALL {
            let Some(theme_wallpaper) = self.wallpapers.get(region) else {
                continue;
            };
            let (bytes, extension) = wallpaper_bytes(theme_wallpaper, check_image)?;
            let digest = Sha256::digest(&bytes);
            let stem: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
            let path = wallpaper_dir.join(format!("{stem}.{extension}"));
            if fs::metadata(&path).ok().map(|m| m.len()) != Some(bytes.len() as u64) {
                private_file::create_dir(wallpaper_dir)
                    .and_then(|()| private_file::write(&path, &bytes, private_file::Flush::Skip))
                    .map_err(|e| format!("Could not save a wallpaper: {e}"))?;
            }
            *wallpapers.get_mut(region) = RegionWallpaper {
                path: Some(path.to_string_lossy().into_owned()),
                opacity: theme_wallpaper.opacity,
            };
        }
        appearance.set_theme_colors(dark, colors);
        if let Some(opacity) = self.url_tooltip_opacity {
            appearance.url_tooltip_opacity = opacity;
        }
        if let Some(fonts) = &self.fonts {
            appearance.set_theme_fonts(fonts);
        }
        *appearance.wallpapers_mut(dark) = wallpapers;
        let builtin = self.name == builtin_name(dark) && self.is_builtin_values(dark);
        *appearance.origin_mut(dark) = ThemeOrigin {
            name: (!builtin).then(|| self.name.clone()),
            fingerprint: (!builtin).then(|| appearance.fingerprint(dark)),
        };
        Ok(())
    }

    /// Whether this is the built-in theme (as [`Self::builtin`] writes it),
    /// so applying it returns to the unnamed default.
    fn is_builtin_values(&self, dark: bool) -> bool {
        let builtin = Self::builtin(dark);
        self.mode == builtin.mode
            && self.colors == builtin.colors
            && self.url_tooltip_opacity == builtin.url_tooltip_opacity
            && self.fonts == builtin.fonts
            && self.wallpapers == builtin.wallpapers
    }
}

pub fn builtin_name(dark: bool) -> &'static str {
    if dark {
        DEFAULT_DARK_NAME
    } else {
        DEFAULT_LIGHT_NAME
    }
}

/// The decoded, checked image of a theme wallpaper and its extension.
fn wallpaper_bytes(
    wallpaper: &ThemeWallpaper,
    check_image: ImageCheck<'_>,
) -> Result<(Vec<u8>, &'static str), ThemeError> {
    // Four characters carry three bytes; refuse before allocating.
    if wallpaper.data.len() / 4 * 3 > MAX_THEME_WALLPAPER_BYTES + 3 {
        return Err("A wallpaper is too large.".into());
    }
    let bytes = STANDARD
        .decode(wallpaper.data.as_bytes())
        .map_err(|_| "A wallpaper is not valid Base64.".to_owned())?;
    if bytes.len() > MAX_THEME_WALLPAPER_BYTES {
        return Err("A wallpaper is too large.".into());
    }
    let extension = check_image(&bytes)?;
    Ok((bytes, extension))
}

fn color_map(colors: &ThemeColors) -> Map<String, Value> {
    match serde_json::to_value(colors) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

impl Appearance {
    pub fn theme_colors(&self, dark: bool) -> ThemeColors {
        if dark {
            let d = &self.dark;
            ThemeColors {
                member_list_background: d.member_list_background.clone(),
                channel_tree_background: d.channel_tree_background.clone(),
                channel_log_background: d.channel_log_background.clone(),
                channel_log_alternate: d.channel_log_alternate.clone(),
                channel_event_color: d.channel_event_color.clone(),
                notice_color: d.notice_color.clone(),
                highlight_color: d.highlight_color.clone(),
                combined_log_background: d.combined_log_background.clone(),
                combined_log_alternate: d.combined_log_alternate.clone(),
                url_tooltip_color: d.url_tooltip_color.clone(),
                url_tooltip_link_color: d.url_tooltip_link_color.clone(),
                url_tooltip_text_color: d.url_tooltip_text_color.clone(),
                link_color: d.link_color.clone(),
                link_background: d.link_background.clone(),
            }
        } else {
            ThemeColors {
                member_list_background: self.member_list_background.clone(),
                channel_tree_background: self.channel_tree_background.clone(),
                channel_log_background: self.channel_log_background.clone(),
                channel_log_alternate: self.channel_log_alternate.clone(),
                channel_event_color: self.channel_event_color.clone(),
                notice_color: self.notice_color.clone(),
                highlight_color: self.highlight_color.clone(),
                combined_log_background: self.combined_log_background.clone(),
                combined_log_alternate: self.combined_log_alternate.clone(),
                url_tooltip_color: self.url_tooltip_color.clone(),
                url_tooltip_link_color: self.url_tooltip_link_color.clone(),
                url_tooltip_text_color: self.url_tooltip_text_color.clone(),
                link_color: self.link_color.clone(),
                link_background: self.link_background.clone(),
            }
        }
    }

    pub fn set_theme_colors(&mut self, dark: bool, c: ThemeColors) {
        if dark {
            let d = &mut self.dark;
            d.member_list_background = c.member_list_background;
            d.channel_tree_background = c.channel_tree_background;
            d.channel_log_background = c.channel_log_background;
            d.channel_log_alternate = c.channel_log_alternate;
            d.channel_event_color = c.channel_event_color;
            d.notice_color = c.notice_color;
            d.highlight_color = c.highlight_color;
            d.combined_log_background = c.combined_log_background;
            d.combined_log_alternate = c.combined_log_alternate;
            d.url_tooltip_color = c.url_tooltip_color;
            d.url_tooltip_link_color = c.url_tooltip_link_color;
            d.url_tooltip_text_color = c.url_tooltip_text_color;
            d.link_color = c.link_color;
            d.link_background = c.link_background;
        } else {
            self.member_list_background = c.member_list_background;
            self.channel_tree_background = c.channel_tree_background;
            self.channel_log_background = c.channel_log_background;
            self.channel_log_alternate = c.channel_log_alternate;
            self.channel_event_color = c.channel_event_color;
            self.notice_color = c.notice_color;
            self.highlight_color = c.highlight_color;
            self.combined_log_background = c.combined_log_background;
            self.combined_log_alternate = c.combined_log_alternate;
            self.url_tooltip_color = c.url_tooltip_color;
            self.url_tooltip_link_color = c.url_tooltip_link_color;
            self.url_tooltip_text_color = c.url_tooltip_text_color;
            self.link_color = c.link_color;
            self.link_background = c.link_background;
        }
    }

    pub fn theme_fonts(&self) -> ThemeFonts {
        let font = |family: &String, size: u16| ThemeFont {
            family: family.clone(),
            size,
        };
        ThemeFonts {
            channel_log: font(&self.channel_log_font, self.channel_log_font_size),
            combined_log: font(&self.combined_log_font, self.combined_log_font_size),
            member_list: font(&self.member_list_font, self.member_list_font_size),
            channel_tree: font(&self.channel_tree_font, self.channel_tree_font_size),
            input: font(&self.input_font, self.input_font_size),
            time: font(&self.time_font, self.time_font_size),
        }
    }

    pub fn set_theme_fonts(&mut self, fonts: &ThemeFonts) {
        self.channel_log_font = fonts.channel_log.family.clone();
        self.channel_log_font_size = fonts.channel_log.size;
        self.combined_log_font = fonts.combined_log.family.clone();
        self.combined_log_font_size = fonts.combined_log.size;
        self.member_list_font = fonts.member_list.family.clone();
        self.member_list_font_size = fonts.member_list.size;
        self.channel_tree_font = fonts.channel_tree.family.clone();
        self.channel_tree_font_size = fonts.channel_tree.size;
        self.input_font = fonts.input.family.clone();
        self.input_font_size = fonts.input.size;
        self.time_font = fonts.time.family.clone();
        self.time_font_size = fonts.time.size;
    }

    pub fn wallpapers(&self, dark: bool) -> &RegionWallpapers {
        if dark {
            &self.dark.wallpapers
        } else {
            &self.wallpapers
        }
    }

    pub fn wallpapers_mut(&mut self, dark: bool) -> &mut RegionWallpapers {
        if dark {
            &mut self.dark.wallpapers
        } else {
            &mut self.wallpapers
        }
    }

    pub fn origin(&self, dark: bool) -> &ThemeOrigin {
        if dark {
            &self.dark.origin
        } else {
            &self.origin
        }
    }

    pub fn origin_mut(&mut self, dark: bool) -> &mut ThemeOrigin {
        if dark {
            &mut self.dark.origin
        } else {
            &mut self.origin
        }
    }

    /// A digest of everything a theme sets for this half of the appearance:
    /// its colors, the popup opacity, the fonts and the wallpapers. Two
    /// appearances with the same fingerprint look the same.
    pub fn fingerprint(&self, dark: bool) -> String {
        let mut colors = self.theme_colors(dark);
        // Case does not matter in a color.
        for value in [
            &mut colors.member_list_background,
            &mut colors.channel_tree_background,
            &mut colors.channel_log_background,
            &mut colors.channel_log_alternate,
            &mut colors.channel_event_color,
            &mut colors.notice_color,
            &mut colors.highlight_color,
            &mut colors.combined_log_background,
            &mut colors.combined_log_alternate,
            &mut colors.url_tooltip_color,
            &mut colors.url_tooltip_link_color,
            &mut colors.url_tooltip_text_color,
            &mut colors.link_color,
            &mut colors.link_background,
        ] {
            value.make_ascii_uppercase();
        }
        let wallpapers: Vec<(&str, Option<&str>, u8)> = WallpaperRegion::ALL
            .iter()
            .map(|region| {
                let wallpaper = self.wallpapers(dark).get(*region);
                (
                    region.key(),
                    wallpaper.path.as_deref(),
                    wallpaper.opacity_percent(),
                )
            })
            .collect();
        let document = serde_json::json!({
            "colors": colors,
            "url_tooltip_opacity": self.url_tooltip_opacity,
            "fonts": self.theme_fonts(),
            "wallpapers": wallpapers,
        });
        let digest = Sha256::digest(document.to_string().as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The name of the theme the light or dark values come from, and
    /// whether they have been edited since. No origin is the built-in
    /// default, which the values match when they are the defaults.
    pub fn theme_status(&self, dark: bool) -> (String, bool) {
        let origin = self.origin(dark);
        let current = self.fingerprint(dark);
        match (&origin.name, &origin.fingerprint) {
            (Some(name), Some(applied)) => (name.clone(), *applied != current),
            (Some(name), None) => (name.clone(), true),
            (None, _) => (
                builtin_name(dark).to_owned(),
                current != Appearance::default().fingerprint(dark),
            ),
        }
    }

    /// Makes the light or dark wallpapers and origin usable after reading.
    pub(crate) fn clean_themes(&mut self) {
        for dark in [false, true] {
            self.wallpapers_mut(dark).clean();
            let origin = self.origin_mut(dark);
            origin.name = origin
                .name
                .as_deref()
                .map(clean_theme_name)
                .filter(|n| !n.is_empty());
            if origin.name.is_none() {
                origin.fingerprint = None;
            }
        }
    }

    /// Every wallpaper path the settings refer to, for deciding which files
    /// the app still needs.
    pub fn wallpaper_paths(&self) -> BTreeSet<&str> {
        let mut paths = BTreeSet::new();
        for dark in [false, true] {
            for region in WallpaperRegion::ALL {
                paths.extend(self.wallpapers(dark).get(region).path.as_deref());
            }
        }
        paths.extend(
            self.channel_looks
                .iter()
                .filter_map(|l| l.wallpaper.as_deref()),
        );
        paths
    }
}

/// The directory of the wallpapers that applied themes left.
pub fn wallpaper_directory() -> Result<PathBuf, String> {
    Ok(crate::settings_path()?
        .parent()
        .ok_or("Could not find the settings directory.")?
        .join("wallpapers"))
}

/// The directory of the imported themes.
pub fn library_directory() -> Result<PathBuf, String> {
    Ok(crate::settings_path()?
        .parent()
        .ok_or("Could not find the settings directory.")?
        .join("themes"))
}

/// Deletes files in `wallpaper_dir` that none of `appearances` refers to
/// (the saved settings and the one about to be saved, say). Only files this
/// module wrote (a 32-digit hexadecimal name) are touched.
pub fn prune_wallpapers(wallpaper_dir: &Path, appearances: &[&Appearance]) {
    let Ok(entries) = fs::read_dir(wallpaper_dir) else {
        return;
    };
    let used: BTreeSet<&str> = appearances
        .iter()
        .flat_map(|appearance| appearance.wallpaper_paths())
        .collect();
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()));
        if ours && !path.to_str().is_some_and(|p| used.contains(p)) {
            let _ = fs::remove_file(path);
        }
    }
}

/// One imported theme in the library.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub id: String,
    pub name: String,
    pub mode: ThemeKind,
}

const LIBRARY_INDEX: &str = "index.json";

/// The imported themes, newest last. Entries whose file is gone are left out.
pub fn library_list(directory: &Path) -> Vec<LibraryEntry> {
    let Ok(bytes) = fs::read(directory.join(LIBRARY_INDEX)) else {
        return Vec::new();
    };
    let mut entries: Vec<LibraryEntry> = serde_json::from_slice(&bytes).unwrap_or_default();
    entries.retain(|e| valid_id(&e.id) && directory.join(file_name(&e.id)).is_file());
    entries.truncate(MAX_LIBRARY_THEMES);
    entries
}

/// Adds `theme` to the library, replacing none: the same name twice makes two
/// entries. Fails when the library is full.
pub fn library_add(directory: &Path, theme: &ThemeFile) -> Result<LibraryEntry, ThemeError> {
    let mut entries = library_list(directory);
    if entries.len() >= MAX_LIBRARY_THEMES {
        return Err(format!(
            "At most {MAX_LIBRARY_THEMES} themes can be kept. Delete one first."
        ));
    }
    let entry = LibraryEntry {
        id: uuid::Uuid::new_v4().simple().to_string(),
        name: theme.name.clone(),
        mode: theme.mode,
    };
    let bytes = theme.to_gzip()?;
    private_file::create_dir(directory)
        .and_then(|()| {
            private_file::write(
                &directory.join(file_name(&entry.id)),
                &bytes,
                private_file::Flush::Skip,
            )
        })
        .map_err(|e| format!("Could not save the theme: {e}"))?;
    entries.push(entry.clone());
    write_index(directory, &entries)?;
    Ok(entry)
}

/// Reads a library theme back, checking it again as on import.
pub fn library_load(
    directory: &Path,
    id: &str,
    check_image: ImageCheck<'_>,
) -> Result<ThemeFile, ThemeError> {
    if !valid_id(id) {
        return Err("No such theme.".into());
    }
    let bytes = fs::read(directory.join(file_name(id))).map_err(|_| "No such theme.".to_owned())?;
    ThemeFile::parse(&bytes, check_image)
}

/// Removes a theme from the library. Appearance already applied from it is
/// not affected: applying copied everything.
pub fn library_remove(directory: &Path, id: &str) -> Result<(), ThemeError> {
    if !valid_id(id) {
        return Ok(());
    }
    let mut entries = library_list(directory);
    entries.retain(|e| e.id != id);
    write_index(directory, &entries)?;
    let _ = fs::remove_file(directory.join(file_name(id)));
    Ok(())
}

fn write_index(directory: &Path, entries: &[LibraryEntry]) -> Result<(), ThemeError> {
    let bytes = serde_json::to_vec_pretty(entries).map_err(|e| e.to_string())?;
    private_file::write(
        &directory.join(LIBRARY_INDEX),
        &bytes,
        private_file::Flush::Skip,
    )
    .map_err(|e| format!("Could not save the theme list: {e}"))
}

fn file_name(id: &str) -> String {
    format!("{id}.{THEME_EXTENSION}")
}

/// Ids are made here; one read from disk must not name another path.
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "theme_file_tests.rs"]
mod tests;
