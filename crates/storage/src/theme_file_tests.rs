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
    appearance.split_wallpapers = Some(true);
    appearance.dark.wallpapers.channel_tree = RegionWallpaper {
        path: Some("/home/me/secret-name/tree.png".into()),
        opacity: Some(55),
        blur: Some(12),
        brightness: Some(-25),
        saturation: Some(130),
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
    assert_eq!((tree.brightness, tree.saturation), (Some(-25), Some(130)));
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
    appearance.wallpapers_mut(true).channel_tree.path = None;
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
        brightness: None,
        saturation: None,
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
    appearance.wallpapers_mut(false).channel_log.path = Some("x".into());
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
        brightness: Some(127),
        saturation: Some(250),
        fit: None,
    };
    appearance.origin.name = Some("\u{202E}  ".into());
    appearance.origin.fingerprint = Some("abc".into());
    appearance.clean_themes();
    assert_eq!(appearance.wallpapers.member_list.path, None);
    assert_eq!(appearance.wallpapers.member_list.opacity, Some(100));
    assert_eq!(appearance.wallpapers.member_list.brightness, Some(100));
    assert_eq!(appearance.wallpapers.member_list.saturation, Some(200));
    assert_eq!(appearance.origin, ThemeOrigin::default());
    let mut look = ChannelLook {
        wallpaper_brightness: Some(-128),
        wallpaper_saturation: Some(250),
        ..ChannelLook::default()
    };
    look.clean();
    assert_eq!(look.wallpaper_brightness, Some(-100));
    assert_eq!(look.wallpaper_saturation, Some(200));
}

#[test]
fn brightness_and_saturation_default_by_theme_and_stay_out_of_the_fingerprint_until_changed() {
    let wallpaper = RegionWallpaper::default();
    assert_eq!(wallpaper.brightness_value(true), -40);
    assert_eq!(wallpaper.brightness_value(false), 40);
    assert_eq!(wallpaper.saturation_percent(), 80);
    // An explicit default is the same appearance as unset.
    let mut appearance = Appearance::default();
    let plain = (appearance.fingerprint(true), appearance.fingerprint(false));
    appearance.dark.wallpapers.channel_log.brightness = Some(-40);
    appearance.wallpapers.channel_log.brightness = Some(40);
    appearance.dark.wallpapers.channel_log.saturation = Some(80);
    assert_eq!(
        (appearance.fingerprint(true), appearance.fingerprint(false)),
        plain
    );
    appearance.dark.wallpapers.channel_log.brightness = Some(-41);
    assert_ne!(appearance.fingerprint(true), plain.0);
    assert_eq!(appearance.fingerprint(false), plain.1);
}

#[test]
fn a_theme_file_with_a_tone_out_of_range_is_refused() {
    let image = STANDARD.encode(b"IMG-ok");
    let parse = |extra: &str| {
        let json = format!(
            r#"{{"version":1,"name":"T","mode":"dark","wallpapers":{{"member_list":{{"data":"{image}"{extra}}}}}}}"#
        );
        ThemeFile::parse(json.as_bytes(), &check)
    };
    assert!(parse(r#","brightness":-100,"saturation":200"#).is_ok());
    assert!(parse(r#","brightness":101"#).is_err());
    assert!(parse(r#","brightness":-128"#).is_err());
    assert!(parse(r#","saturation":201"#).is_err());
    // A file without them reads as unset.
    let theme = parse("").unwrap();
    let slot = theme.wallpapers.member_list.unwrap();
    assert_eq!((slot.brightness, slot.saturation), (None, None));
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

fn picture(name: &str, opacity: u8) -> RegionWallpapers {
    RegionWallpapers {
        channel_log: RegionWallpaper {
            path: Some(name.into()),
            opacity: Some(opacity),
            ..RegionWallpaper::default()
        },
        ..RegionWallpapers::default()
    }
}

#[test]
fn a_new_appearance_shares_one_set_of_wallpapers_between_the_themes() {
    let mut appearance = Appearance::default();
    assert!(!appearance.split_wallpapers());
    *appearance.wallpapers_mut(false) = picture("a", 30);
    assert_eq!(appearance.wallpapers(true), appearance.wallpapers(false));
    assert_eq!(appearance.wallpapers(true).channel_log.opacity, Some(30));
}

#[test]
fn switching_modes_back_and_forth_loses_and_mixes_nothing() {
    let mut appearance = Appearance::default();
    *appearance.wallpapers_mut(false) = picture("shared", 10);
    // Separate: both sides start as what they showed.
    appearance.set_split_wallpapers(true, false);
    assert_eq!(*appearance.wallpapers(false), picture("shared", 10));
    assert_eq!(*appearance.wallpapers(true), picture("shared", 10));
    *appearance.wallpapers_mut(false) = picture("light", 20);
    *appearance.wallpapers_mut(true) = picture("dark", 40);
    // Shared again: the shared set, untouched by the edits above.
    appearance.set_split_wallpapers(false, false);
    assert_eq!(*appearance.wallpapers(true), picture("shared", 10));
    *appearance.wallpapers_mut(true) = picture("shared2", 15);
    // Separate again: the earlier separate sets come back.
    appearance.set_split_wallpapers(true, true);
    assert_eq!(*appearance.wallpapers(false), picture("light", 20));
    assert_eq!(*appearance.wallpapers(true), picture("dark", 40));
    appearance.set_split_wallpapers(false, true);
    assert_eq!(*appearance.wallpapers(false), picture("shared2", 15));
    // Every set is still referred to, so none of the files is pruned.
    let paths = appearance.wallpaper_paths();
    for name in ["light", "dark", "shared2"] {
        assert!(paths.contains(name), "{name}");
    }
}

#[test]
fn turning_the_shared_set_on_for_older_separate_settings_starts_it_from_the_shown_theme() {
    let json = r#"{"wallpapers":{"channel_log":{"path":"light","opacity":20}},
        "dark":{"wallpapers":{"channel_log":{"path":"dark","opacity":40}}}}"#;
    let mut appearance: Appearance = serde_json::from_str(json).unwrap();
    appearance.clean_themes();
    assert!(appearance.split_wallpapers());
    appearance.set_split_wallpapers(false, true);
    assert_eq!(
        appearance.wallpapers(false).channel_log.path.as_deref(),
        Some("dark")
    );
    appearance.set_split_wallpapers(true, true);
    assert_eq!(
        appearance.wallpapers(false).channel_log.path.as_deref(),
        Some("light")
    );
}

#[test]
fn an_emptied_set_stays_empty_through_switches_saves_and_loads() {
    let reload = |a: &Appearance| {
        let mut back: Appearance =
            serde_json::from_str(&serde_json::to_string(a).unwrap()).unwrap();
        back.clean_themes();
        back
    };
    let mut appearance = Appearance::default();
    *appearance.wallpapers_mut(false) = picture("shared", 10);
    appearance.set_split_wallpapers(true, false);
    // Dark is deliberately emptied.
    *appearance.wallpapers_mut(true) = RegionWallpapers::default();
    appearance.set_split_wallpapers(false, false);
    appearance.set_split_wallpapers(true, false);
    assert_eq!(*appearance.wallpapers(true), RegionWallpapers::default());
    assert_eq!(*appearance.wallpapers(false), picture("shared", 10));
    let mut appearance = reload(&appearance);
    appearance.set_split_wallpapers(false, false);
    appearance.set_split_wallpapers(true, true);
    assert_eq!(*appearance.wallpapers(true), RegionWallpapers::default());
    // The shared set emptied on purpose stays empty too.
    *appearance.wallpapers_mut(false) = picture("light", 20);
    appearance.set_split_wallpapers(false, false);
    *appearance.wallpapers_mut(false) = RegionWallpapers::default();
    let mut appearance = reload(&appearance);
    appearance.set_split_wallpapers(true, false);
    appearance.set_split_wallpapers(false, true);
    assert_eq!(*appearance.wallpapers(false), RegionWallpapers::default());
}

#[test]
fn a_side_never_set_while_separate_follows_what_was_set_while_shared() {
    let mut appearance = Appearance::default();
    appearance.set_split_wallpapers(true, false);
    appearance.set_split_wallpapers(false, false);
    *appearance.wallpapers_mut(false) = picture("shared", 10);
    // Only light is set while separate; dark still follows the shared set.
    appearance.set_split_wallpapers(true, false);
    assert_eq!(*appearance.wallpapers(true), picture("shared", 10));
    *appearance.wallpapers_mut(false) = picture("light", 20);
    appearance.set_split_wallpapers(false, false);
    *appearance.wallpapers_mut(false) = picture("shared2", 30);
    appearance.set_split_wallpapers(true, false);
    assert_eq!(*appearance.wallpapers(false), picture("light", 20));
    assert_eq!(*appearance.wallpapers(true), picture("shared2", 30));
}

#[test]
fn settings_from_before_read_as_separate_when_the_sides_differ() {
    let load = |json: &str| {
        let mut appearance: Appearance = serde_json::from_str(json).unwrap();
        appearance.clean_themes();
        appearance
    };
    let light = r#""wallpapers":{"channel_log":{"path":"l","opacity":20}}"#;
    let dark = r#""dark":{"wallpapers":{"channel_log":{"path":"d","opacity":40}}}"#;
    // Different pictures: separate, each side as before.
    let both = load(&format!("{{{light},{dark}}}"));
    assert!(both.split_wallpapers());
    assert_eq!(
        both.wallpapers(false).channel_log.path.as_deref(),
        Some("l")
    );
    assert_eq!(both.wallpapers(true).channel_log.path.as_deref(), Some("d"));
    // One side only: the other shows none, so they stay separate.
    let one = load(&format!("{{{light}}}"));
    assert!(one.split_wallpapers());
    assert_eq!(one.wallpapers(true), &RegionWallpapers::default());
    assert_eq!(one.wallpapers(false).channel_log.opacity, Some(20));
    // The same picture: shared, shown unchanged on both sides.
    let same_dark = r#""dark":{"wallpapers":{"channel_log":{"path":"l","opacity":20}}}"#;
    let same = load(&format!("{{{light},{same_dark}}}"));
    assert!(!same.split_wallpapers());
    assert_eq!(same.wallpapers(true).channel_log.path.as_deref(), Some("l"));
    assert_eq!(same.wallpapers(false).channel_log.opacity, Some(20));
    // Neither: shared, and nothing is written for a mode nobody chose.
    let none = load("{}");
    assert_eq!(none, Appearance::default());
    assert!(
        !serde_json::to_string(&none)
            .unwrap()
            .contains("split_wallpapers")
    );
    // The choice is kept through a save and load.
    let mut chosen = Appearance::default();
    chosen.set_split_wallpapers(true, false);
    let back = load(&serde_json::to_string(&chosen).unwrap());
    assert!(back.split_wallpapers());
}

#[test]
fn applying_a_theme_never_changes_the_other_side_while_shared() {
    let mut appearance = Appearance::default();
    *appearance.wallpapers_mut(false) = picture("shared", 10);
    // The same wallpapers as shown: nothing to separate.
    appearance.set_theme_wallpapers(true, picture("shared", 10));
    assert!(!appearance.split_wallpapers());
    // Different ones for dark: dark changes, light keeps what it showed.
    appearance.set_theme_wallpapers(true, picture("themed", 70));
    assert!(appearance.split_wallpapers());
    assert_eq!(*appearance.wallpapers(true), picture("themed", 70));
    assert_eq!(*appearance.wallpapers(false), picture("shared", 10));
    // An older separate set is not restored for the other side.
    let mut again = Appearance::default();
    again.set_split_wallpapers(true, false);
    *again.wallpapers_mut(false) = picture("old light", 5);
    again.set_split_wallpapers(false, false);
    *again.wallpapers_mut(false) = picture("shared", 10);
    again.set_theme_wallpapers(true, picture("themed", 70));
    assert_eq!(*again.wallpapers(false), picture("shared", 10));
}
