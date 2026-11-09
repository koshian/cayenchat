// SPDX-License-Identifier: MPL-2.0
use std::io::Write;

use flate2::{Compression, write::GzEncoder};

use super::*;
use crate::{ChannelLook, FontSet};

/// Stands in for the media crate: "images" start with `IMG`.
fn check(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.starts_with(b"IMG") {
        Ok("png")
    } else {
        Err("This is not an image.".into())
    }
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn sample() -> Appearance {
    let mut appearance = Appearance::default();
    appearance.dark.channel_log_background = "#102030".into();
    appearance.dark.member_list_background = "#0A0B0C".into();
    appearance.fonts.channel_log_font = "Noto Serif".into();
    appearance.fonts.channel_log_font_size = 18;
    appearance.dark.wallpapers.channel_tree = RegionWallpaper {
        path: Some("/home/me/secret-name/tree.png".into()),
        opacity: Some(55),
        blur: Some(12),
        fit: Some(crate::WallpaperFit::Contain),
    };
    appearance.channel_looks.push(ChannelLook {
        server: "libera".into(),
        channel: "#private".into(),
        wallpaper: Some("/home/me/chan.png".into()),
        ..ChannelLook::default()
    });
    appearance
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    assert!(path.ends_with("tree.png"));
    Ok(b"IMG-tree-bytes".to_vec())
}

#[test]
fn export_import_apply_round_trips_the_dark_side() {
    let dir = tempfile::tempdir().unwrap();
    let source = sample();
    let theme = ThemeFile::capture(&source, true, "  My\u{202E} Dark ", &check, read).unwrap();
    assert_eq!(theme.name, "My Dark");
    let bytes = theme.to_gzip().unwrap();
    assert!(bytes.starts_with(&[0x1f, 0x8b]));

    let imported = ThemeFile::parse(&bytes, &check).unwrap();
    assert_eq!(imported, theme);

    let mut target = Appearance::default();
    imported.apply(&mut target, dir.path(), &check).unwrap();
    assert_eq!(target.dark.channel_log_background, "#102030");
    assert_eq!(target.dark.member_list_background, "#0A0B0C");
    // The theme's fonts went to the dark side only: the shared fonts moved
    // to separate ones, and the light side keeps the old fonts.
    assert!(target.split_fonts);
    assert_eq!(target.fonts(true).channel_log_font, "Noto Serif");
    assert_eq!(target.fonts(true).channel_log_font_size, 18);
    assert_eq!(*target.fonts(false), FontSet::default());
    // The light side is untouched.
    assert_eq!(
        target.theme_colors(false),
        Appearance::default().theme_colors(false)
    );
    let tree = &target.dark.wallpapers.channel_tree;
    assert_eq!(tree.opacity, Some(55));
    assert_eq!(tree.blur, Some(12));
    let path = tree.path.clone().unwrap();
    assert!(path.starts_with(dir.path().to_str().unwrap()));
    assert_eq!(std::fs::read(&path).unwrap(), b"IMG-tree-bytes");
    assert_eq!(target.theme_status(true), ("My Dark".into(), false));
    // The light half keeps its theme and is not modified by the dark one's
    // fonts.
    assert_eq!(
        target.theme_status(false),
        (DEFAULT_LIGHT_NAME.into(), false)
    );
}

#[test]
fn a_shared_theme_carries_no_servers_channels_or_paths() {
    let theme = ThemeFile::capture(&sample(), true, "Mine", &check, read).unwrap();
    let json = String::from_utf8(serde_json::to_vec(&theme).unwrap()).unwrap();
    for private in ["libera", "#private", "/home/me", "secret-name", "chan.png"] {
        assert!(!json.contains(private), "{private} leaked");
    }
}

#[test]
fn plain_hand_edited_json_is_read_and_missing_colors_take_the_default() {
    let json = br##"{"version":1,"name":"Hand","mode":"light",
        "colors":{"channel_log_background":"#abcdef","unknown_key":"x"}}"##;
    let theme = ThemeFile::parse(json, &check).unwrap();
    let mut appearance = Appearance {
        channel_tree_background: "#000000".into(),
        ..Appearance::default()
    };
    theme
        .apply(&mut appearance, Path::new("/nonexistent"), &check)
        .unwrap();
    assert_eq!(appearance.channel_log_background, "#ABCDEF");
    assert_eq!(
        appearance.channel_tree_background,
        Appearance::default().channel_tree_background
    );
    assert_eq!(appearance.theme_status(false), ("Hand".into(), false));
}

#[test]
fn editing_after_applying_marks_the_theme_modified_and_undoing_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let theme = ThemeFile::capture(&sample(), true, "Mine", &check, read).unwrap();
    let mut appearance = Appearance::default();
    theme.apply(&mut appearance, dir.path(), &check).unwrap();
    let applied = appearance.dark.channel_log_background.clone();

    appearance.dark.channel_log_background = "#111111".into();
    assert_eq!(appearance.theme_status(true), ("Mine".into(), true));
    // Case alone is not an edit.
    appearance.dark.channel_log_background = applied.to_lowercase();
    assert_eq!(appearance.theme_status(true), ("Mine".into(), false));
    // Fonts and wallpapers count.
    appearance.fonts_mut(true).input_font = "Mono".into();
    assert!(appearance.theme_status(true).1);
    appearance.fonts_mut(true).input_font.clear();
    appearance.dark.wallpapers.channel_tree.path = None;
    assert!(appearance.theme_status(true).1);
}

#[test]
fn the_default_is_unmodified_until_edited_and_applying_it_again_returns() {
    let dir = tempfile::tempdir().unwrap();
    let mut appearance = Appearance::default();
    assert_eq!(
        appearance.theme_status(false),
        (DEFAULT_LIGHT_NAME.into(), false)
    );
    assert_eq!(
        appearance.theme_status(true),
        (DEFAULT_DARK_NAME.into(), false)
    );
    appearance.channel_log_background = "#123456".into();
    assert_eq!(
        appearance.theme_status(false),
        (DEFAULT_LIGHT_NAME.into(), true)
    );
    ThemeFile::builtin(false)
        .apply(&mut appearance, dir.path(), &check)
        .unwrap();
    assert_eq!(appearance, Appearance::default());
    assert_eq!(appearance.origin(false), &ThemeOrigin::default());

    // A named theme whose values equal the defaults is still that theme.
    let mut named = ThemeFile::builtin(true);
    named.name = "Same as default".into();
    named.apply(&mut appearance, dir.path(), &check).unwrap();
    assert_eq!(
        appearance.theme_status(true),
        ("Same as default".into(), false)
    );
}

#[test]
fn applying_a_theme_leaves_everything_but_the_look_alone() {
    let dir = tempfile::tempdir().unwrap();
    let theme = ThemeFile::capture(&sample(), true, "Mine", &check, read).unwrap();
    let mut appearance = Appearance {
        alternate_rows: true,
        compact_urls: true,
        ..Appearance::default()
    };
    appearance.channel_looks.push(ChannelLook {
        server: "s".into(),
        channel: "#c".into(),
        font_size: 12,
        ..ChannelLook::default()
    });
    let before = appearance.channel_looks.clone();
    theme.apply(&mut appearance, dir.path(), &check).unwrap();
    assert!(appearance.alternate_rows && appearance.compact_urls);
    assert_eq!(appearance.channel_looks, before);
}

#[test]
fn bad_files_are_refused_without_touching_the_appearance() {
    let dir = tempfile::tempdir().unwrap();
    let ok =
        |extra: &str| format!(r##"{{"version":1,"name":"T","mode":"dark"{extra}}}"##).into_bytes();
    let parse = |bytes: &[u8]| ThemeFile::parse(bytes, &check);

    assert!(parse(&ok("")).is_ok());
    assert!(parse(b"not json").is_err());
    assert!(parse(&gzip(b"not json")).is_err());
    assert!(parse(&[0x1f, 0x8b, 0, 1, 2]).is_err());
    assert!(parse(br#"{"version":2,"name":"T","mode":"dark"}"#).is_err());
    assert!(parse(br#"{"version":1,"name":"T","mode":"sepia"}"#).is_err());
    assert!(parse(br#"{"version":1,"name":" \u0007 ","mode":"dark"}"#).is_err());
    assert!(parse(&ok(r##","colors":{"notice_color":"red"}"##)).is_err());
    assert!(parse(&ok(r#","colors":{"notice_color":5}"#)).is_err());
    assert!(parse(&ok(r#","url_tooltip_opacity":5"#)).is_err());
    assert!(parse(&ok(r#","wallpapers":{"channel_log":{"data":"!!!"}}"#)).is_err());
    // Valid Base64 of something that is not an image.
    let text = STANDARD.encode(b"hello");
    assert!(
        parse(&ok(&format!(
            r#","wallpapers":{{"member_list":{{"data":"{text}"}}}}"#
        )))
        .is_err()
    );
    let image = STANDARD.encode(b"IMG-ok");
    assert!(
        parse(&ok(&format!(
            r#","wallpapers":{{"member_list":{{"data":"{image}","opacity":101}}}}"#
        )))
        .is_err()
    );
    assert!(
        parse(&ok(&format!(
            r#","wallpapers":{{"member_list":{{"data":"{image}","opacity":60}}}}"#
        )))
        .is_ok()
    );

    // A failing apply changes nothing.
    let mut theme = ThemeFile::builtin(false);
    theme.wallpapers.channel_log = Some(ThemeWallpaper {
        data: text,
        opacity: None,
        blur: None,
        fit: None,
    });
    let mut appearance = Appearance::default();
    assert!(theme.apply(&mut appearance, dir.path(), &check).is_err());
    assert_eq!(appearance, Appearance::default());
}

#[test]
fn sizes_are_limited_before_and_after_expanding_and_per_image() {
    // The input itself.
    let big = vec![b' '; MAX_THEME_FILE_BYTES as usize + 1];
    assert!(
        ThemeFile::parse(&big, &check)
            .unwrap_err()
            .contains("large")
    );
    // A small file that expands past the limit (a compression bomb).
    let bomb = gzip(&vec![b' '; MAX_THEME_JSON_BYTES as usize + 1]);
    assert!(bomb.len() < 1024 * 1024);
    assert!(
        ThemeFile::parse(&bomb, &check)
            .unwrap_err()
            .contains("expanded")
    );
    // One image over the per-image limit, small enough to pass the others.
    let data = STANDARD.encode(vec![b'I'; MAX_THEME_WALLPAPER_BYTES + 1]);
    let json = format!(
        r#"{{"version":1,"name":"T","mode":"dark","wallpapers":{{"channel_log":{{"data":"{data}"}}}}}}"#
    );
    assert!(
        ThemeFile::parse(json.as_bytes(), &check)
            .unwrap_err()
            .contains("large")
    );
    // Exporting a picture over the limit fails instead of dropping it.
    let mut appearance = Appearance::default();
    appearance.wallpapers.channel_log.path = Some("x".into());
    let error = ThemeFile::capture(&appearance, false, "T", &check, |_| {
        Ok(vec![0; MAX_THEME_WALLPAPER_BYTES + 1])
    })
    .unwrap_err();
    assert!(error.contains("larger"));
    // A file that is not an image fails the export; its bytes are not kept.
    let error = ThemeFile::capture(&appearance, false, "T", &check, |_| {
        Ok(b"-----BEGIN OPENSSH PRIVATE KEY-----".to_vec())
    })
    .unwrap_err();
    assert!(error.contains("not an image"), "{error}");
    // A name is bounded.
    let long = "x".repeat(500);
    assert_eq!(
        clean_theme_name(&long).chars().count(),
        MAX_THEME_NAME_CHARS
    );
}

#[test]
fn the_library_keeps_themes_apart_from_the_applied_appearance() {
    let library = tempfile::tempdir().unwrap();
    let wallpapers = tempfile::tempdir().unwrap();
    let theme = ThemeFile::capture(&sample(), true, "Shared", &check, read).unwrap();
    let entry = library_add(library.path(), &theme).unwrap();
    assert_eq!(library_list(library.path()), vec![entry.clone()]);
    assert_eq!(entry.mode, ThemeKind::Dark);

    let loaded = library_load(library.path(), &entry.id, &check).unwrap();
    let mut appearance = Appearance::default();
    loaded
        .apply(&mut appearance, wallpapers.path(), &check)
        .unwrap();
    let before = appearance.clone();

    library_remove(library.path(), &entry.id).unwrap();
    assert!(library_list(library.path()).is_empty());
    assert!(library_load(library.path(), &entry.id, &check).is_err());
    // The applied appearance and its wallpaper are as they were.
    assert_eq!(appearance, before);
    let path = appearance
        .dark
        .wallpapers
        .channel_tree
        .path
        .clone()
        .unwrap();
    assert!(Path::new(&path).is_file());
}

#[test]
fn the_library_is_bounded_and_ids_cannot_name_other_files() {
    let library = tempfile::tempdir().unwrap();
    let theme = ThemeFile::builtin(false);
    for _ in 0..MAX_LIBRARY_THEMES {
        library_add(library.path(), &theme).unwrap();
    }
    assert!(library_add(library.path(), &theme).is_err());
    assert!(library_load(library.path(), "../index", &check).is_err());
    assert!(library_remove(library.path(), "../../x").is_ok());
    assert_eq!(library_list(library.path()).len(), MAX_LIBRARY_THEMES);
    // An entry whose file is gone is not listed.
    let first = library_list(library.path())[0].id.clone();
    std::fs::remove_file(library.path().join(file_name(&first))).unwrap();
    assert_eq!(library_list(library.path()).len(), MAX_LIBRARY_THEMES - 1);
}

#[test]
fn pruning_removes_only_unused_files_of_our_own_naming() {
    let dir = tempfile::tempdir().unwrap();
    let mut appearance = Appearance::default();
    let theme = ThemeFile::capture(&sample(), true, "Mine", &check, read).unwrap();
    theme.apply(&mut appearance, dir.path(), &check).unwrap();
    let kept = appearance
        .dark
        .wallpapers
        .channel_tree
        .path
        .clone()
        .unwrap();
    let stale = dir.path().join(format!("{}.png", "ab".repeat(16)));
    let foreign = dir.path().join("notes.txt");
    std::fs::write(&stale, b"IMG").unwrap();
    std::fs::write(&foreign, b"x").unwrap();
    prune_wallpapers(dir.path(), &[&appearance]);
    assert!(Path::new(&kept).is_file() && foreign.is_file());
    assert!(!stale.exists());
}

#[test]
fn old_settings_read_as_the_default_theme_and_keep_their_colors_and_wallpapers() {
    let old = r##"{"channel_log_background":"#FAFAFA",
        "dark":{"channel_log_background":"#101010"},
        "channel_looks":[{"server":"s","channel":"#c","wallpaper":"/pic.png"}]}"##;
    let appearance: Appearance = serde_json::from_str(old).unwrap();
    assert_eq!(appearance.channel_log_background, "#FAFAFA");
    assert_eq!(appearance.dark.channel_log_background, "#101010");
    assert_eq!(appearance.wallpapers, RegionWallpapers::default());
    assert_eq!(appearance.dark.origin, ThemeOrigin::default());
    // Someone's own colors under the built-in name read as edited.
    assert_eq!(
        appearance.theme_status(false),
        (DEFAULT_LIGHT_NAME.into(), true)
    );
    // The channel's picture is its own: neither inheriting nor switched off.
    let look = &appearance.channel_looks[0];
    assert_eq!(look.wallpaper.as_deref(), Some("/pic.png"));
    assert!(!look.no_wallpaper);
}

#[test]
fn a_channel_cannot_both_switch_the_wallpaper_off_and_set_one() {
    let mut look = ChannelLook {
        wallpaper: Some("/pic.png".into()),
        no_wallpaper: true,
        ..ChannelLook::default()
    };
    look.clean();
    assert!(look.wallpaper.is_none() && look.no_wallpaper && look.is_set());
}

#[test]
fn hostile_settings_values_are_cleaned_on_load() {
    let mut appearance = Appearance::default();
    appearance.wallpapers.member_list = RegionWallpaper {
        path: Some("x".repeat(5000)),
        opacity: Some(250),
        blur: Some(250),
        fit: None,
    };
    appearance.origin.name = Some("\u{202E}  ".into());
    appearance.origin.fingerprint = Some("abc".into());
    appearance.clean_themes();
    assert_eq!(appearance.wallpapers.member_list.path, None);
    assert_eq!(appearance.wallpapers.member_list.opacity, Some(100));
    assert_eq!(appearance.origin, ThemeOrigin::default());
}

#[test]
fn empty_or_mistyped_color_fields_are_left_out_and_the_file_reads_back() {
    let mut appearance = Appearance::default();
    appearance.dark.channel_log_background = String::new();
    appearance.dark.member_list_background = "#12".into();
    appearance.dark.link_background = String::new();
    let theme = ThemeFile::capture(&appearance, true, "Blank", &check, read).unwrap();
    assert!(!theme.colors.contains_key("channel_log_background"));
    assert!(!theme.colors.contains_key("member_list_background"));
    let back = ThemeFile::parse(&theme.to_gzip().unwrap(), &check).unwrap();
    let defaults = ThemeColors::defaults(true);
    let mut applied = Appearance::default();
    back.apply(&mut applied, tempfile::tempdir().unwrap().path(), &check)
        .unwrap();
    assert_eq!(
        applied.dark.channel_log_background,
        defaults.channel_log_background
    );
    assert_eq!(applied.dark.link_background, "");
}

#[test]
fn unknown_color_keys_are_skipped_not_stored() {
    let big = format!("[{}]", "0,".repeat(200_000) + "0");
    let json = format!(
        r##"{{"version":1,"name":"X","mode":"dark","colors":{{"future":{big},"other":{{"a":[1]}},"link_color":"#112233"}}}}"##
    );
    let theme = ThemeFile::parse(json.as_bytes(), &check).unwrap();
    assert!(!theme.colors.contains_key("future"));
    assert!(!theme.colors.contains_key("other"));
    assert_eq!(theme.colors["link_color"], "#112233");
    // A known key with a non-text value is still refused.
    let bad = r#"{"version":1,"name":"X","mode":"dark","colors":{"link_color":[1,2]}}"#;
    assert!(ThemeFile::parse(bad.as_bytes(), &check).is_err());
}

#[test]
fn library_names_are_cleaned_and_empty_ones_dropped() {
    let library = tempfile::tempdir().unwrap();
    let theme = ThemeFile::capture(&sample(), true, "Shared", &check, read).unwrap();
    let a = library_add(library.path(), &theme).unwrap();
    let b = library_add(library.path(), &theme).unwrap();
    let index = serde_json::to_vec(&[
        LibraryEntry {
            name: format!("\u{202E}evil{}", "x".repeat(1000)),
            ..a
        },
        LibraryEntry {
            name: "\u{202E}\n".into(),
            ..b
        },
    ])
    .unwrap();
    fs::write(library.path().join(LIBRARY_INDEX), index).unwrap();
    let list = library_list(library.path());
    assert_eq!(list.len(), 1);
    assert!(!list[0].name.contains('\u{202E}'));
    assert!(list[0].name.chars().count() <= MAX_THEME_NAME_CHARS);
}

#[test]
fn a_prepared_theme_lands_on_the_current_appearance_and_keeps_other_edits() {
    let dir = tempfile::tempdir().unwrap();
    let theme = ThemeFile::capture(&sample(), true, "Dark", &check, read).unwrap();
    let prepared = theme.prepare(dir.path(), &check).unwrap();
    // Meanwhile the light side and the shared fonts were edited.
    let mut live = Appearance {
        channel_log_background: "#ABCDEF".into(),
        ..Appearance::default()
    };
    live.fonts.input_font_size = 24;
    prepared.apply_to(&mut live);
    assert_eq!(live.channel_log_background, "#ABCDEF");
    assert_eq!(live.dark.channel_log_background, "#102030");
    assert_eq!(live.fonts(true).channel_log_font, "Noto Serif");
    assert_eq!(live.fonts(false).channel_log_font, "");
    assert_eq!(live.fonts(false).input_font_size, 24);
    assert_eq!(live.theme_status(true), ("Dark".into(), false));
    assert!(live.theme_status(false).1);
}

#[test]
fn a_theme_without_fonts_leaves_edited_fonts_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut theme = ThemeFile::capture(&sample(), true, "Dark", &check, read).unwrap();
    theme.fonts = None;
    let prepared = theme.prepare(dir.path(), &check).unwrap();
    let mut live = Appearance::default();
    live.fonts.input_font_size = 24;
    prepared.apply_to(&mut live);
    assert_eq!(live.fonts(false).input_font_size, 24);
    assert_eq!(live.fonts(true).input_font_size, 24);
    assert!(!live.split_fonts);
}
