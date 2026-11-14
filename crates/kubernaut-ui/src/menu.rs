//! The application menu in the macOS menu bar.
//!
//! On macOS the first menu is the application's own, titled with its name. The
//! other platforms have no native menu bar to put it in, and Settings and the
//! update check stay reachable from inside the window.

use gpui_kit::*;

use crate::app::{
    About, CheckForUpdates, HideApp, HideOtherApps, QuitApp, ShowAllApps, ShowSettings,
};

/// The menus, in the order they appear.
pub fn menus() -> Vec<Menu> {
    vec![Menu {
        name: "Kubernaut".into(),
        disabled: false,
        items: vec![
            MenuItem::action("About Kubernaut", About),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", ShowSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Kubernaut", HideApp),
            MenuItem::action("Hide Others", HideOtherApps),
            MenuItem::action("Show All", ShowAllApps),
            MenuItem::separator(),
            MenuItem::action("Quit Kubernaut", QuitApp),
        ],
    }]
}

#[cfg(test)]
mod tests {
    use gpui_kit::MenuItem;

    use super::menus;

    fn labels() -> Vec<String> {
        menus()
            .into_iter()
            .flat_map(|menu| menu.items)
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_menu_offers_about_updates_and_settings_before_quit() {
        let labels = labels();
        let at = |label: &str| labels.iter().position(|l| l == label);
        let (about, updates, settings, quit) = (
            at("About Kubernaut"),
            at("Check for Updates…"),
            at("Settings…"),
            at("Quit Kubernaut"),
        );
        assert!(about.is_some() && updates.is_some() && settings.is_some() && quit.is_some());
        assert!(about < updates && updates < settings && settings < quit);
    }

    #[test]
    fn the_first_menu_is_the_application_menu() {
        let menus = menus();
        assert_eq!(menus.first().map(|menu| &*menu.name), Some("Kubernaut"));
    }
}
