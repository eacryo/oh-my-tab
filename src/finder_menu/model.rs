//! Building the menu model the extension renders.
//!
//! Pure given the config, the current locale and the installed terminal set, so the arrangement
//! rules can be asserted without a Finder or a notification.

use crate::config::{Config, FINDER_MENU_FILE_TYPES};
use crate::finder_menu::terminals::{self, TerminalSpec};
use crate::i18n::{t, tf};
use finder_protocol::{Context, MenuAction, MenuModel, MenuNode};

/// Build the model for the current config. An empty list means "show nothing".
pub(crate) fn build(
    config: &Config,
    host_bundle_id: &str,
    host_instance: &str,
    model_version: u64,
) -> MenuModel {
    let section = &config.finder_menu;
    if !section.enabled {
        return MenuModel::empty(host_bundle_id, host_instance, model_version);
    }
    let terminals: Vec<&'static TerminalSpec> = section
        .terminals
        .iter()
        .filter_map(|bundle_id| terminals::spec(bundle_id))
        .filter(|spec| terminals::is_installed(spec.bundle_id))
        .collect();

    let new_file_items: Vec<MenuNode> = section
        .new_file_types
        .iter()
        .filter_map(|extension| {
            if !FINDER_MENU_FILE_TYPES.contains(&extension.as_str()) {
                return None;
            }
            Some(MenuNode::item(
                new_file_label(extension),
                Some("doc.text"),
                MenuAction::NewFile {
                    extension: extension.clone(),
                },
                &[Context::Items, Context::Container, Context::Sidebar],
            ))
        })
        .collect();
    let terminal_items: Vec<MenuNode> = terminals
        .iter()
        .map(|spec| {
            MenuNode::item(
                tf("finder_menu.open_in_terminal", &[("app", spec.name)]),
                Some("terminal"),
                MenuAction::OpenTerminal {
                    bundle_id: spec.bundle_id.to_string(),
                },
                &[Context::Items, Context::Container, Context::Sidebar],
            )
        })
        .collect();

    let mut items = Vec::new();
    match section.layout.as_str() {
        "flat" => {
            items.extend(new_file_items.iter().cloned());
            items.extend(terminal_items.iter().cloned());
            if section.copy_path {
                items.push(copy_path_item());
            }
        }
        "submenu" => {
            let mut children = Vec::new();
            if !new_file_items.is_empty() {
                children.push(MenuNode::submenu(
                    t("finder_menu.new_file"),
                    Some("doc.badge.plus"),
                    Some(&[Context::Items, Context::Container, Context::Sidebar][..]),
                    new_file_items.clone(),
                ));
            }
            if !terminal_items.is_empty() {
                children.push(MenuNode::submenu(
                    t("finder_menu.open_in_terminal_menu"),
                    Some("terminal"),
                    Some(&[Context::Items, Context::Container, Context::Sidebar][..]),
                    terminal_items.clone(),
                ));
            }
            if section.copy_path {
                children.push(copy_path_item());
            }
            if !children.is_empty() {
                items.push(MenuNode::submenu(
                    "Oh-My-Tab",
                    Some("menubar.rectangle"),
                    None,
                    children,
                ));
            }
        }
        // "grouped" and any value the config layer could not normalize.
        _ => {
            if !new_file_items.is_empty() {
                items.push(MenuNode::submenu(
                    t("finder_menu.new_file"),
                    Some("doc.badge.plus"),
                    None,
                    new_file_items,
                ));
            }
            if !terminal_items.is_empty() {
                items.push(MenuNode::submenu(
                    t("finder_menu.open_in_terminal_menu"),
                    Some("terminal"),
                    None,
                    terminal_items,
                ));
            }
            if section.copy_path {
                items.push(copy_path_item());
            }
        }
    }

    MenuModel {
        protocol_version: finder_protocol::PROTOCOL_VERSION,
        model_version,
        host_bundle_id: host_bundle_id.to_string(),
        host_instance: host_instance.to_string(),
        items,
    }
}

/// The menu label for one file type. Written as literal keys so the translation inventory scan in
/// i18n.rs sees them.
fn new_file_label(extension: &str) -> String {
    match extension {
        "md" => t("finder_menu.new_file_item.md"),
        "txt" => t("finder_menu.new_file_item.txt"),
        "json" => t("finder_menu.new_file_item.json"),
        other => format!(".{other}"),
    }
}

fn copy_path_item() -> MenuNode {
    MenuNode::item(
        t("finder_menu.copy_path"),
        Some("doc.on.doc"),
        MenuAction::CopyPath,
        &[Context::Items, Context::Container, Context::Sidebar],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FinderMenuSection, FINDER_MENU_LAYOUTS};

    fn config(layout: &str) -> Config {
        Config {
            finder_menu: FinderMenuSection {
                enabled: true,
                layout: layout.to_string(),
                copy_path: true,
                new_file_types: vec!["md".into(), "txt".into()],
                terminals: vec!["com.apple.Terminal".into()],
            },
            ..Config::default()
        }
    }

    #[test]
    fn disabled_builds_no_items() {
        let mut config = config("grouped");
        config.finder_menu.enabled = false;
        let model = build(&config, "com.example.app", "instance", 1);
        assert!(model.items.is_empty());
    }

    #[test]
    fn grouped_puts_new_files_and_terminals_in_submenus() {
        let model = build(&config("grouped"), "com.example.app", "instance", 1);
        assert_eq!(model.items.len(), 3);
        assert!(matches!(model.items[0], MenuNode::Submenu { .. }));
        assert!(matches!(model.items[1], MenuNode::Submenu { .. }));
        assert!(matches!(model.items[2], MenuNode::Item { .. }));
    }

    #[test]
    fn flat_has_no_submenus() {
        let model = build(&config("flat"), "com.example.app", "instance", 1);
        assert!(model
            .items
            .iter()
            .all(|item| matches!(item, MenuNode::Item { .. })));
        assert_eq!(model.items.len(), 2 + 1 + 1);
    }

    #[test]
    fn submenu_wraps_everything_in_one_node() {
        let model = build(&config("submenu"), "com.example.app", "instance", 1);
        assert_eq!(model.items.len(), 1);
        match &model.items[0] {
            MenuNode::Submenu {
                title, children, ..
            } => {
                assert_eq!(title, "Oh-My-Tab");
                assert_eq!(children.len(), 3);
            }
            other => panic!("expected outer submenu, got {other:?}"),
        }
    }

    #[test]
    fn unknown_layout_falls_back_to_grouped() {
        let model = build(&config("garbage"), "com.example.app", "instance", 1);
        assert_eq!(model.items.len(), 3);
    }

    #[test]
    fn unknown_file_types_are_skipped() {
        let mut config = config("grouped");
        config.finder_menu.new_file_types = vec!["exe".into(), "md".into()];
        let model = build(&config, "com.example.app", "instance", 1);
        let new_files = match &model.items[0] {
            MenuNode::Submenu { children, .. } => children.clone(),
            _ => panic!("expected new-file submenu"),
        };
        assert_eq!(new_files.len(), 1);
    }

    #[test]
    fn layout_values_are_the_advertised_ones() {
        assert!(FINDER_MENU_LAYOUTS.contains(&"grouped"));
    }
}
