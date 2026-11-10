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

use crate::{
    Appearance, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR, DEFAULT_WALLPAPER_SATURATION,
    MAX_WALLPAPER_BLUR, MAX_WALLPAPER_BRIGHTNESS, MAX_WALLPAPER_SATURATION, clamp_font_size,
    clamp_wallpaper_brightness, color_value, default_wallpaper_brightness, private_file,
};

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
    /// [`DEFAULT_BACKGROUND_OPACITY`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<u8>,
    /// Blur sigma of the picture in tenths of a pixel (0 to
    /// [`MAX_WALLPAPER_BLUR`]); unset is [`DEFAULT_WALLPAPER_BLUR`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blur: Option<u8>,
    /// Brightness of the picture (-100 to 100); unset is
    /// [`default_wallpaper_brightness`] of the theme.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<i8>,
    /// Saturation of the picture in percent (0 to 200); unset is
    /// [`DEFAULT_WALLPAPER_SATURATION`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saturation: Option<u8>,
    /// How the picture fits the area; unset is [`WallpaperFit::default`]. A
    /// name this version does not know reads as unset.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::lenient_fit"
    )]
    pub fit: Option<crate::WallpaperFit>,
}

impl RegionWallpaper {
    pub fn opacity_percent(&self) -> u8 {
        self.opacity.unwrap_or(DEFAULT_BACKGROUND_OPACITY).min(100)
    }

    pub fn blur_tenths(&self) -> u8 {
        self.blur
            .unwrap_or(DEFAULT_WALLPAPER_BLUR)
            .min(MAX_WALLPAPER_BLUR)
    }

    /// The brightness in the theme `dark`, capped to the range.
    pub fn brightness_value(&self, dark: bool) -> i8 {
        clamp_wallpaper_brightness(
            self.brightness
                .unwrap_or_else(|| default_wallpaper_brightness(dark)),
        )
    }

    pub fn saturation_percent(&self) -> u8 {
        self.saturation
            .unwrap_or(DEFAULT_WALLPAPER_SATURATION)
            .min(MAX_WALLPAPER_SATURATION)
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
            wallpaper.blur = wallpaper.blur.map(|b| b.min(MAX_WALLPAPER_BLUR));
            wallpaper.brightness = wallpaper.brightness.map(clamp_wallpaper_brightness);
            wallpaper.saturation = wallpaper
                .saturation
                .map(|s| s.min(MAX_WALLPAPER_SATURATION));
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

/// The fonts of one side, light or dark, whichever the theme is for.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<i8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::lenient_fit"
    )]
    pub fit: Option<crate::WallpaperFit>,
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
    #[serde(default, deserialize_with = "deserialize_colors")]
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
            fonts: Some(defaults.theme_fonts(dark)),
            wallpapers: ThemeWallpapers::default(),
        }
    }

    /// The light or dark half of `appearance` as a theme called `name`.
    /// `read` returns the bytes of a wallpaper file; one that cannot be read
    /// or is too large fails the export, so a theme never silently loses a
    /// picture. A file `check_image` refuses fails it too: the path comes
    /// from settings that may not be the user's own, and a file that is not
    /// an image must not end up inside a shared theme.
    pub fn capture(
        appearance: &Appearance,
        dark: bool,
        name: &str,
        check_image: ImageCheck,
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
            check_image(&bytes)?;
            *wallpapers.slot(region) = Some(ThemeWallpaper {
                data: STANDARD.encode(bytes),
                opacity: wallpaper.opacity,
                blur: wallpaper.blur,
                brightness: wallpaper.brightness,
                saturation: wallpaper.saturation,
                fit: wallpaper.fit,
            });
        }
        Ok(Self {
            version: THEME_VERSION,
            name,
            mode: ThemeKind::from_dark(dark),
            colors: valid_colors(color_map(&appearance.theme_colors(dark))),
            url_tooltip_opacity: Some(appearance.url_tooltip_opacity),
            fonts: Some(appearance.theme_fonts(dark)),
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
                if wallpaper.blur.is_some_and(|b| b > MAX_WALLPAPER_BLUR) {
                    return Err("A wallpaper blur is out of range.".into());
                }
                if wallpaper.brightness.is_some_and(|b| {
                    !(-MAX_WALLPAPER_BRIGHTNESS..=MAX_WALLPAPER_BRIGHTNESS).contains(&b)
                }) {
                    return Err("A wallpaper brightness is out of range.".into());
                }
                if wallpaper
                    .saturation
                    .is_some_and(|s| s > MAX_WALLPAPER_SATURATION)
                {
                    return Err("A wallpaper saturation is out of range.".into());
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
        self.prepare(wallpaper_dir, check_image)?
            .apply_to(appearance);
        Ok(())
    }

    /// The part of applying that reads and writes files: resolves the colors
    /// and writes the images. [`PreparedTheme::apply_to`] then sets the values
    /// on whatever appearance is current, so nothing is carried over from a
    /// copy taken earlier.
    pub fn prepare(
        &self,
        wallpaper_dir: &Path,
        check_image: ImageCheck<'_>,
    ) -> Result<PreparedTheme, ThemeError> {
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
                blur: theme_wallpaper.blur,
                brightness: theme_wallpaper.brightness,
                saturation: theme_wallpaper.saturation,
                fit: theme_wallpaper.fit,
            };
        }
        let dark = self.mode.is_dark();
        Ok(PreparedTheme {
            dark,
            name: self.name.clone(),
            builtin: self.name == builtin_name(dark) && self.is_builtin_values(dark),
            colors,
            url_tooltip_opacity: self.url_tooltip_opacity,
            fonts: self.fonts.clone(),
            wallpapers,
        })
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

/// A theme whose files are written and colors resolved, ready to be set on an
/// appearance. Holds no part of any appearance.
#[derive(Debug, Clone)]
pub struct PreparedTheme {
    dark: bool,
    name: String,
    builtin: bool,
    colors: ThemeColors,
    url_tooltip_opacity: Option<u8>,
    fonts: Option<ThemeFonts>,
    wallpapers: RegionWallpapers,
}

impl PreparedTheme {
    pub fn is_dark(&self) -> bool {
        self.dark
    }

    /// Sets the theme's values on its side of `appearance` and notes it as
    /// the origin. Nothing of the other side is touched, and fonts the theme
    /// does not carry stay as they are.
    pub fn apply_to(&self, appearance: &mut Appearance) {
        let dark = self.dark;
        appearance.set_theme_colors(dark, self.colors.clone());
        if let Some(opacity) = self.url_tooltip_opacity {
            appearance.url_tooltip_opacity = opacity;
        }
        if let Some(fonts) = &self.fonts {
            appearance.set_theme_fonts(dark, fonts);
        }
        *appearance.wallpapers_mut(dark) = self.wallpapers.clone();
        *appearance.origin_mut(dark) = ThemeOrigin {
            name: (!self.builtin).then(|| self.name.clone()),
            fingerprint: (!self.builtin).then(|| appearance.fingerprint(dark)),
        };
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

/// Drops the colors that import would refuse: a field left empty or mistyped
/// in the settings form. A key left out takes the built-in color, which is
/// what drawing falls back to, so the file reads back.
fn valid_colors(mut colors: Map<String, Value>) -> Map<String, Value> {
    colors.retain(|key, value| {
        let text = value.as_str().unwrap_or("").trim();
        color_value(text).is_some() || (key == "link_background" && text.is_empty())
    });
    colors
}

/// One color field as read: text, or anything else (refused later for a key
/// this version knows). Nested values are skipped, never built.
enum ColorField {
    Text(String),
    Other,
}

impl<'de> Deserialize<'de> for ColorField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ColorField;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a color")
            }
            fn visit_str<E>(self, v: &str) -> Result<ColorField, E> {
                Ok(ColorField::Text(v.to_owned()))
            }
            fn visit_bool<E>(self, _: bool) -> Result<ColorField, E> {
                Ok(ColorField::Other)
            }
            fn visit_i64<E>(self, _: i64) -> Result<ColorField, E> {
                Ok(ColorField::Other)
            }
            fn visit_u64<E>(self, _: u64) -> Result<ColorField, E> {
                Ok(ColorField::Other)
            }
            fn visit_f64<E>(self, _: f64) -> Result<ColorField, E> {
                Ok(ColorField::Other)
            }
            fn visit_unit<E>(self) -> Result<ColorField, E> {
                Ok(ColorField::Other)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<ColorField, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ColorField::Other)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<ColorField, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(ColorField::Other)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Reads the `colors` object keeping only the keys this version knows, each
/// as text (or null for a value that is not text). Unknown keys and their
/// values are skipped without being stored, so a small file cannot grow into
/// a large structure.
fn deserialize_colors<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Map<String, Value>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Map<String, Value>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object of colors")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let known = color_map(&ThemeColors::defaults(false));
            let mut out = Map::new();
            while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                if known.contains_key(key.as_ref()) {
                    let value = match map.next_value::<ColorField>()? {
                        ColorField::Text(text) => Value::String(text),
                        ColorField::Other => Value::Null,
                    };
                    out.insert(key.into_owned(), value);
                } else {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(Visitor)
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

    /// The fonts of the light or dark side, as a theme carries them.
    pub fn theme_fonts(&self, dark: bool) -> ThemeFonts {
        let set = self.fonts(dark);
        let font = |family: &String, size: u16| ThemeFont {
            family: family.clone(),
            size,
        };
        ThemeFonts {
            channel_log: font(&set.channel_log_font, set.channel_log_font_size),
            combined_log: font(&set.combined_log_font, set.combined_log_font_size),
            member_list: font(&set.member_list_font, set.member_list_font_size),
            channel_tree: font(&set.channel_tree_font, set.channel_tree_font_size),
            input: font(&set.input_font, set.input_font_size),
            time: font(&set.time_font, set.time_font_size),
        }
    }

    /// Sets the fonts of the light or dark side. While the fonts are shared
    /// and `fonts` differ from them, the appearance moves to separate fonts
    /// first, so the other side keeps its own.
    pub fn set_theme_fonts(&mut self, dark: bool, fonts: &ThemeFonts) {
        let mut set = self.fonts(dark).clone();
        set.channel_log_font = fonts.channel_log.family.clone();
        set.channel_log_font_size = fonts.channel_log.size;
        set.combined_log_font = fonts.combined_log.family.clone();
        set.combined_log_font_size = fonts.combined_log.size;
        set.member_list_font = fonts.member_list.family.clone();
        set.member_list_font_size = fonts.member_list.size;
        set.channel_tree_font = fonts.channel_tree.family.clone();
        set.channel_tree_font_size = fonts.channel_tree.size;
        set.input_font = fonts.input.family.clone();
        set.input_font_size = fonts.input.size;
        set.time_font = fonts.time.family.clone();
        set.time_font_size = fonts.time.size;
        set.clamp_sizes();
        if set == *self.fonts(dark) {
            return;
        }
        if !self.split_fonts {
            // Move to separate fonts without restoring older separate values:
            // the other side keeps what it shows now.
            let other = self.fonts(!dark).clone();
            if dark {
                self.light_fonts = Some(other);
            } else {
                self.dark_fonts = Some(other);
            }
            self.split_fonts = true;
        }
        *self.fonts_mut(dark) = set;
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
        // A blur is listed only when set, so the fingerprint of a theme
        // applied before blur existed still matches.
        let wallpapers: Vec<Value> = WallpaperRegion::ALL
            .iter()
            .map(|region| {
                let wallpaper = self.wallpapers(dark).get(*region);
                let mut entry = vec![
                    Value::from(region.key()),
                    Value::from(wallpaper.path.as_deref()),
                    Value::from(wallpaper.opacity_percent()),
                ];
                if wallpaper.blur_tenths() != DEFAULT_WALLPAPER_BLUR {
                    entry.push(Value::from(wallpaper.blur_tenths()));
                }
                // The values in effect, so unset and the default are alike.
                if wallpaper.brightness_value(dark) != default_wallpaper_brightness(dark) {
                    entry.push(Value::from(format!(
                        "brightness {}",
                        wallpaper.brightness_value(dark)
                    )));
                }
                if wallpaper.saturation_percent() != DEFAULT_WALLPAPER_SATURATION {
                    entry.push(Value::from(format!(
                        "saturation {}",
                        wallpaper.saturation_percent()
                    )));
                }
                if let Some(fit) = wallpaper
                    .fit
                    .filter(|fit| *fit != crate::WallpaperFit::default())
                {
                    entry.push(Value::from(format!("{fit:?}")));
                }
                Value::Array(entry)
            })
            .collect();
        let document = serde_json::json!({
            "colors": colors,
            "url_tooltip_opacity": self.url_tooltip_opacity,
            "fonts": self.theme_fonts(dark),
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
    // The index is read from disk: names are cleaned as on import, and an
    // entry left without one is dropped.
    for entry in &mut entries {
        entry.name = clean_theme_name(&entry.name);
    }
    entries.retain(|e| {
        !e.name.is_empty() && valid_id(&e.id) && directory.join(file_name(&e.id)).is_file()
    });
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
