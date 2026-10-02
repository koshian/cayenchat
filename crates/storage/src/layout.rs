//! Where the main window and its panes were last left.
//!
//! This is application UI state, kept in its own file beside the settings so
//! that saving it every time the window moves never races the settings
//! window's autosave, and so that damage to it can never stop the application
//! from starting: a missing, unreadable or unknown file reads as "nothing
//! saved". Sizes are logical pixels, as GPUI reports them.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const LAYOUT_VERSION: u32 = 1;
/// Largest value accepted for a coordinate or size; anything beyond is damage.
const LIMIT: f32 = 100_000.;

/// A window rectangle in desktop coordinates (the origin may be negative on a
/// display left of or above the primary one).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl WindowRect {
    fn is_sane(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= LIMIT)
            && self.width > 0.
            && self.height > 0.
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub version: u32,
    /// The window's normal rectangle: where it returns to when not
    /// maximized.
    pub window: Option<WindowRect>,
    pub maximized: bool,
    /// Width of the right column (members and channel tree).
    pub right_width: Option<f32>,
    /// Height of the member list in that column.
    pub members_height: Option<f32>,
    /// Share of the left column's height that the main log takes.
    pub log_split: Option<f32>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            version: LAYOUT_VERSION,
            window: None,
            maximized: false,
            right_width: None,
            members_height: None,
            log_split: None,
        }
    }
}

impl Layout {
    /// Drops values that cannot be real (NaN, infinite, absurd or not
    /// positive), so a hand-edited or damaged file restores what it can.
    pub fn sanitized(mut self) -> Self {
        if self.window.is_some_and(|rect| !rect.is_sane()) {
            self.window = None;
            self.maximized = false;
        }
        let sane = |value: Option<f32>| value.filter(|v| v.is_finite() && *v > 0. && *v <= LIMIT);
        self.right_width = sane(self.right_width);
        self.members_height = sane(self.members_height);
        self.log_split = sane(self.log_split).filter(|share| *share < 1.);
        self
    }
}

/// `window.json` beside the settings file (in a test build's own directory).
pub fn layout_path() -> Result<PathBuf, String> {
    Ok(crate::settings_path()?.with_file_name("window.json"))
}

/// The saved layout, or the empty one when there is none or it cannot be
/// used. Never an error: UI state must not get in the way of starting.
pub fn load_layout() -> Layout {
    layout_path()
        .map(|path| load_layout_from(&path))
        .unwrap_or_default()
}

pub fn load_layout_from(path: &Path) -> Layout {
    let Ok(bytes) = fs::read(path) else {
        return Layout::default();
    };
    match serde_json::from_slice::<Layout>(&bytes) {
        // A file from a newer version may mean something else by its fields.
        Ok(layout) if layout.version == LAYOUT_VERSION => layout.sanitized(),
        _ => Layout::default(),
    }
}

pub fn save_layout(layout: &Layout) -> Result<(), String> {
    save_layout_to(&layout_path()?, layout)
}

/// Writes the file whole or not at all (a temporary file, then a rename), so
/// closing the window in the middle of a save cannot leave half a file.
pub fn save_layout_to(path: &Path, layout: &Layout) -> Result<(), String> {
    let parent = path.parent().ok_or("The layout path has no directory.")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create the layout directory: {error}"))?;
    let bytes = serde_json::to_vec_pretty(&Layout {
        version: LAYOUT_VERSION,
        ..layout.clone()
    })
    .map_err(|error| format!("Could not serialize the layout: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|error| format!("Could not write the layout: {error}"))?;
    fs::rename(&temporary, path).map_err(|error| format!("Could not save the layout: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            window: Some(WindowRect {
                x: -1200.,
                y: 40.,
                width: 960.,
                height: 600.,
            }),
            maximized: true,
            right_width: Some(300.),
            members_height: Some(220.),
            log_split: Some(0.4),
            ..Layout::default()
        }
    }

    #[test]
    fn a_saved_layout_reads_back_including_negative_coordinates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("window.json");
        assert_eq!(load_layout_from(&path), Layout::default(), "no file yet");
        save_layout_to(&path, &layout()).unwrap();
        assert_eq!(load_layout_from(&path), layout());
        assert!(!path.with_extension("json.tmp").exists(), "no leftover");
    }

    #[test]
    fn damaged_or_unknown_files_read_as_nothing_saved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("window.json");
        for text in [
            "",
            "not json",
            "[1, 2]",
            r#"{"version": 99, "window": null}"#,
        ] {
            fs::write(&path, text).unwrap();
            assert_eq!(load_layout_from(&path), Layout::default(), "{text:?}");
        }
        // Unknown fields and a missing version are tolerated where the
        // version is the current one.
        fs::write(
            &path,
            r#"{"version": 1, "extra": true, "right_width": 280}"#,
        )
        .unwrap();
        assert_eq!(load_layout_from(&path).right_width, Some(280.));
    }

    #[test]
    fn impossible_values_are_dropped_and_the_rest_kept() {
        let mut damaged = layout();
        damaged.window = Some(WindowRect {
            x: f32::NAN,
            ..damaged.window.unwrap()
        });
        damaged.right_width = Some(-5.);
        damaged.members_height = Some(f32::INFINITY);
        damaged.log_split = Some(1.5);
        let cleaned = damaged.sanitized();
        assert_eq!(cleaned.window, None);
        assert!(
            !cleaned.maximized,
            "maximized without a rectangle means nothing"
        );
        assert_eq!(cleaned.right_width, None);
        assert_eq!(cleaned.members_height, None);
        assert_eq!(cleaned.log_split, None);

        let mut zero = layout();
        zero.window.as_mut().unwrap().width = 0.;
        assert_eq!(zero.sanitized().window, None);
        assert_eq!(layout().sanitized(), layout(), "a good layout is unchanged");
    }

    #[test]
    fn the_file_sits_beside_the_settings() {
        let layout = layout_path().unwrap();
        assert_eq!(layout.file_name().unwrap(), "window.json");
        assert_eq!(layout.parent(), crate::settings_path().unwrap().parent());
    }
}
