// SPDX-License-Identifier: GPL-3.0-only
use super::{About, OpenSettings, Quit, app_menus, menu_access_keys};
use crate::localization::Localizer;
use cayenchat_storage::Language;
use gpui::{Action, MenuItem};

fn actions(items: &[MenuItem]) -> Vec<&'static str> {
    items
        .iter()
        .filter_map(|item| match item {
            MenuItem::Action { action, .. } => Some(action.name()),
            _ => None,
        })
        .collect()
}

#[test]
fn settings_is_offered_once_and_the_window_menu_is_macos_only() {
    for language in [Language::English, Language::Japanese] {
        let menus = app_menus(false, true, false, &Localizer::new(language));
        let mut all: Vec<&str> = Vec::new();
        for menu in &menus {
            all.extend(actions(&menu.items));
        }
        let count = |name: &str| all.iter().filter(|action| **action == name).count();
        assert_eq!(count(OpenSettings.name()), 1);
        assert_eq!(count(Quit.name()), 1);
        // The first menu holds Settings and Quit on every platform; macOS
        // puts About above Settings.
        let first = actions(&menus[0].items);
        let leading = if cfg!(target_os = "macos") {
            About.name()
        } else {
            OpenSettings.name()
        };
        assert_eq!(first.first(), Some(&leading));
        assert_eq!(first.last(), Some(&Quit.name()));
        assert_eq!(
            menus.iter().any(|menu| menu.menu_type.is_some()),
            cfg!(target_os = "macos")
        );
    }
}

#[test]
fn about_is_in_the_app_menu_on_macos_and_the_help_menu_elsewhere() {
    for language in [Language::English, Language::Japanese] {
        let i18n = Localizer::new(language);
        let menus = app_menus(false, true, false, &i18n);
        let holders: Vec<usize> = menus
            .iter()
            .enumerate()
            .filter(|(_, menu)| actions(&menu.items).contains(&About.name()))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(holders.len(), 1);
        let holder = &menus[holders[0]];
        let label = i18n.text("menu_about");
        assert!(holder.items.iter().any(|item| matches!(
            item, MenuItem::Action { name, .. } if *name == label
        )));
        if cfg!(target_os = "macos") {
            assert_eq!(holders[0], 0);
        } else {
            assert_eq!(holder.name.as_ref(), i18n.text("menu_help"));
            assert_eq!(holders[0], menus.len() - 1);
            // Every bar menu has an access key, in order.
            assert_eq!(menu_access_keys(&i18n).keys.len(), menus.len());
        }
        // Names stay unique, so Help does not duplicate File or the like.
        let mut names: Vec<_> = menus.iter().map(|menu| menu.name.to_string()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), menus.len());
    }
}
