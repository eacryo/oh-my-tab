//! Building the menu model the extension renders.
//!
//! Pure given the config, the current locale and the installed terminal set, so the arrangement
//! rules can be asserted without a Finder or a notification.
//!
//! The copy items exist twice: one item per selection shape, and the extension's `MenuTarget`
//! filter picks the set that matches what is selected when Finder asks for the menu. Both sets are
//! in the model at all times, so a right-click never rebuilds anything.

use crate::config::{Config, FINDER_MENU_FILE_TYPES};
use crate::finder_menu::terminals::{self, TerminalSpec};
use crate::i18n::{t, tf};
use finder_protocol::{Context, MenuAction, MenuModel, MenuNode, MenuTarget, PathLayout};

/// Build the model for the current config. An empty list means "show nothing".
///
/// `peer_protocol` is the extension's protocol version, taken from its last `Sync`. A v2 extension
/// (the process an update leaves behind) receives exactly the two single-selection items it knows;
/// sending it a v3 model would fail to decode and leave the menu empty until Finder replaced it.
pub(crate) fn build(
    config: &Config,
    host_bundle_id: &str,
    host_instance: &str,
    model_version: u64,
    peer_protocol: u32,
) -> MenuModel {
    let section = &config.finder_menu;
    if !section.enabled {
        // Even an empty model states the protocol of the peer it is for: the extension drops a
        // State whose version is not its own, and a v2 process must recognise this one.
        let mut model = MenuModel::empty(host_bundle_id, host_instance, model_version);
        model.protocol_version = peer_protocol;
        return model;
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
    let copy_items = copy_items(section, peer_protocol);

    let mut items = Vec::new();
    match section.layout.as_str() {
        "flat" => {
            items.extend(new_file_items.iter().cloned());
            items.extend(terminal_items.iter().cloned());
            items.extend(copy_items);
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
            children.extend(copy_items);
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
            items.extend(copy_items);
        }
    }

    MenuModel {
        protocol_version: peer_protocol,
        model_version,
        host_bundle_id: host_bundle_id.to_string(),
        host_instance: host_instance.to_string(),
        items,
    }
}

/// The copy items, in the order the menu shows them.
///
/// The two single-selection items come first and the four multiple-selection items follow, so a
/// filtered menu reads "Copy Path / Copy SHA-256" for one item and the four variants for several,
/// each group in the order the user asked for.
fn copy_items(section: &crate::config::FinderMenuSection, peer_protocol: u32) -> Vec<MenuNode> {
    let mut items = Vec::new();
    if peer_protocol < 3 {
        // A v2 extension knows only these two shapes.
        if section.copy_path {
            items.push(legacy_copy_path_item());
        }
        if section.copy_sha256 {
            items.push(legacy_copy_sha256_item());
        }
        return items;
    }
    if section.copy_path {
        items.push(single_copy_path_item());
        items.push(copy_paths_item(PathLayout::Newline));
        items.push(copy_paths_item(PathLayout::SingleLine));
    }
    if section.copy_sha256 {
        items.push(single_copy_sha256_item());
        items.push(copy_sha256_many_item(false));
        items.push(copy_sha256_many_item(true));
    }
    items
}

/// The menu label for one file type. Written as literal keys so the translation inventory scan in
/// i18n.rs sees them.
///
/// The extension is part of every label: the flat layout puts these items straight into the context
/// menu with no "New File" parent, so "Text File" did not say which file it would create.
fn new_file_label(extension: &str) -> String {
    match extension {
        "md" => t("finder_menu.new_file_item.md"),
        "txt" => t("finder_menu.new_file_item.txt"),
        "json" => t("finder_menu.new_file_item.json"),
        "docx" => t("finder_menu.new_file_item.docx"),
        "xlsx" => t("finder_menu.new_file_item.xlsx"),
        "pptx" => t("finder_menu.new_file_item.pptx"),
        // A type that is allowed but has no dedicated label yet: keep the same shape instead of
        // degrading to a bare extension.
        other => {
            let extension = format!(".{other}");
            tf(
                "finder_menu.new_file_item.other",
                &[("extension", extension.as_str())],
            )
        }
    }
}

/// One selected item: its path is what the user wants, folder or file alike.
fn single_copy_path_item() -> MenuNode {
    MenuNode::item_targeting(
        t("finder_menu.copy_path"),
        Some("doc.on.doc"),
        MenuAction::CopyPath,
        &[Context::Items, Context::Container, Context::Sidebar],
        MenuTarget::Single,
    )
}

/// Several selected items: every path, in the selected order.
fn copy_paths_item(layout: PathLayout) -> MenuNode {
    let (title, symbol) = match layout {
        PathLayout::Newline => (t("finder_menu.copy_paths_newline"), "doc.on.doc"),
        PathLayout::SingleLine => (t("finder_menu.copy_paths_single"), "text.alignleft"),
    };
    MenuNode::item_targeting(
        title,
        Some(symbol),
        MenuAction::CopyPaths { layout },
        &[Context::Items],
        MenuTarget::Multiple,
    )
}

/// One regular file: its digest, with no file name, exactly as before.
fn single_copy_sha256_item() -> MenuNode {
    MenuNode::item_targeting(
        t("finder_menu.copy_sha256"),
        Some("number"),
        MenuAction::CopySha256,
        &[Context::Items],
        MenuTarget::SingleRegularFile,
    )
}

/// Several selected items. Whether any of them can be hashed is decided at click time: statting
/// every selected item here would run on Finder's menu thread, and one stalled volume would hold
/// the whole menu. The item is offered for any multiple selection; the app reports what it skipped.
fn copy_sha256_many_item(with_name: bool) -> MenuNode {
    let (title, symbol) = if with_name {
        (t("finder_menu.copy_sha256_named"), "doc.plaintext")
    } else {
        (t("finder_menu.copy_sha256_digests"), "number")
    };
    MenuNode::item_targeting(
        title,
        Some(symbol),
        MenuAction::CopySha256Many { with_name },
        &[Context::Items],
        MenuTarget::Multiple,
    )
}

/// The v2 shapes, for the one extension process an update leaves running. `Any` is what v2 used
/// for the path item: a v2 decoder does not know `Single`.
fn legacy_copy_path_item() -> MenuNode {
    MenuNode::item(
        t("finder_menu.copy_path"),
        Some("doc.on.doc"),
        MenuAction::CopyPath,
        &[Context::Items, Context::Container, Context::Sidebar],
    )
}

fn legacy_copy_sha256_item() -> MenuNode {
    MenuNode::item_targeting(
        t("finder_menu.copy_sha256"),
        Some("number"),
        MenuAction::CopySha256,
        &[Context::Items],
        MenuTarget::SingleRegularFile,
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
                copy_sha256: true,
                new_file_types: vec!["md".into(), "txt".into()],
                terminals: vec!["com.apple.Terminal".into()],
            },
            ..Config::default()
        }
    }

    /// The model a current extension gets.
    fn build(config: &Config) -> MenuModel {
        super::build(config, "com.example.app", "instance", 1, 3)
    }

    #[test]
    fn disabled_builds_no_items() {
        let mut config = config("grouped");
        config.finder_menu.enabled = false;
        let model = super::build(&config, "com.example.app", "instance", 1, 3);
        assert!(model.items.is_empty());
        // A v2 peer must still recognise the empty model as its own protocol.
        let model = super::build(&config, "com.example.app", "instance", 1, 2);
        assert!(model.items.is_empty());
        assert_eq!(model.protocol_version, 2);
    }

    /// Six copy items are in the model at all times: the two single-selection ones and the four
    /// multiple-selection ones. The extension shows one set or the other.
    #[test]
    fn grouped_puts_new_files_and_terminals_in_submenus() {
        let model = build(&config("grouped"));
        // Two submenus, then the six copy items.
        assert_eq!(model.items.len(), 2 + 6);
        assert!(matches!(model.items[0], MenuNode::Submenu { .. }));
        assert!(matches!(model.items[1], MenuNode::Submenu { .. }));
        assert!(model.items[2..]
            .iter()
            .all(|item| matches!(item, MenuNode::Item { .. })));
    }

    #[test]
    fn flat_has_no_submenus() {
        let model = build(&config("flat"));
        assert!(model
            .items
            .iter()
            .all(|item| matches!(item, MenuNode::Item { .. })));
        // Two file types, one terminal, six copy items.
        assert_eq!(model.items.len(), 2 + 1 + 6);
    }

    #[test]
    fn submenu_wraps_everything_in_one_node() {
        let model = build(&config("submenu"));
        assert_eq!(model.items.len(), 1);
        match &model.items[0] {
            MenuNode::Submenu {
                title, children, ..
            } => {
                assert_eq!(title, "Oh-My-Tab");
                assert_eq!(children.len(), 2 + 6);
            }
            other => panic!("expected outer submenu, got {other:?}"),
        }
    }

    #[test]
    fn unknown_layout_falls_back_to_grouped() {
        let model = build(&config("garbage"));
        assert_eq!(model.items.len(), 2 + 6);
    }

    /// The filtered menu is what the user reads: two copy items for one selected item, four for
    /// several. The new-file and terminal items apply to both, so only the copy actions are
    /// compared.
    #[test]
    fn a_single_selection_offers_two_copy_items_and_a_multiple_offers_four() {
        let model = build(&config("flat"));
        let copy_actions = |selection: finder_protocol::Selection| {
            let filtered = finder_protocol::filter_model(&model, Context::Items, selection);
            let mut actions = Vec::new();
            collect_copy_actions(&filtered.items, &mut actions);
            actions
        };
        let one = copy_actions(finder_protocol::Selection {
            count: 1,
            regular_file: true,
        });
        assert_eq!(
            one,
            vec!["CopyPath".to_string(), "CopySha256".to_string()],
            "one selected item keeps exactly the two items it had"
        );
        let many = copy_actions(finder_protocol::Selection {
            count: 2,
            regular_file: false,
        });
        assert_eq!(
            many,
            vec![
                "CopyPaths(newline)".to_string(),
                "CopyPaths(single_line)".to_string(),
                "CopySha256Many(false)".to_string(),
                "CopySha256Many(true)".to_string(),
            ],
            "several selected items get the four variants, in the menu's order"
        );
        // A folder selection of one item keeps the single path item and hides the digest one.
        let folder = copy_actions(finder_protocol::Selection {
            count: 1,
            regular_file: false,
        });
        assert_eq!(folder, vec!["CopyPath".to_string()]);
    }

    /// The one appex process an update leaves behind announced v2. It must receive only the shapes
    /// it can decode: the two single items, with v2's `Any` target on the path item.
    #[test]
    fn a_v2_peer_gets_only_the_two_single_items() {
        let model = super::build(&config("flat"), "com.example.app", "instance", 1, 2);
        assert_eq!(model.items.len(), 2 + 1 + 2);
        assert_eq!(model.protocol_version, 2);
        let mut actions = Vec::new();
        collect_actions(&model.items, &mut actions);
        assert!(actions
            .iter()
            .any(|action| matches!(action, MenuAction::CopyPath)));
        assert!(actions
            .iter()
            .any(|action| matches!(action, MenuAction::CopySha256)));
        assert!(
            !actions.iter().any(|action| matches!(
                action,
                MenuAction::CopyPaths { .. } | MenuAction::CopySha256Many { .. }
            )),
            "a v2 peer cannot decode a v3-only action"
        );
        // A v2 decoder knows `any` and `single_regular_file`, nothing else.
        for node in &model.items {
            if let MenuNode::Item { targets, .. } = node {
                assert!(
                    matches!(targets, MenuTarget::Any | MenuTarget::SingleRegularFile),
                    "{targets:?} is not a v2 target"
                );
            }
        }
    }

    fn collect_actions(nodes: &[MenuNode], out: &mut Vec<MenuAction>) {
        for node in nodes {
            match node {
                MenuNode::Item { action, .. } => out.push(action.clone()),
                MenuNode::Submenu { children, .. } => collect_actions(children, out),
                MenuNode::Separator => {}
            }
        }
    }

    fn collect_copy_actions(nodes: &[MenuNode], out: &mut Vec<String>) {
        let mut actions = Vec::new();
        collect_actions(nodes, &mut actions);
        for action in actions {
            match action {
                MenuAction::CopyPath => out.push("CopyPath".into()),
                MenuAction::CopyPaths { layout } => out.push(match layout {
                    PathLayout::Newline => "CopyPaths(newline)".into(),
                    PathLayout::SingleLine => "CopyPaths(single_line)".into(),
                }),
                MenuAction::CopySha256 => out.push("CopySha256".into()),
                MenuAction::CopySha256Many { with_name } => {
                    out.push(format!("CopySha256Many({with_name})"))
                }
                MenuAction::NewFile { .. } | MenuAction::OpenTerminal { .. } => {}
            }
        }
    }

    /// Every label names the extension it creates, including a type without its own wording: the
    /// menu must not fall back to a bare ".exe" beside labels that say "New .md File".
    #[test]
    fn every_new_file_label_names_its_extension() {
        for extension in ["md", "txt", "json", "docx", "xlsx", "pptx", "exe"] {
            let label = super::new_file_label(extension);
            assert!(
                label.contains(&format!(".{extension}")),
                "{extension} is not named in {label:?}"
            );
        }
    }

    /// The flat layout has no "New File" parent, so the label itself has to say what it creates.
    /// This pins the rendered titles, not just the locale lookup.
    #[test]
    fn a_flat_menu_names_each_new_file_extension() {
        let mut config = config("flat");
        config.finder_menu.new_file_types = vec!["md".into(), "txt".into(), "json".into()];
        let model = build(&config);
        let titles: Vec<&str> = model
            .items
            .iter()
            .filter_map(|node| match node {
                MenuNode::Item { title, .. } => Some(title.as_str()),
                MenuNode::Submenu { .. } | MenuNode::Separator => None,
            })
            .collect();
        for (extension, key) in [
            ("md", "finder_menu.new_file_item.md"),
            ("txt", "finder_menu.new_file_item.txt"),
            ("json", "finder_menu.new_file_item.json"),
        ] {
            let title = t(key);
            assert!(
                titles.contains(&title.as_str()),
                "{extension} is missing from {titles:?} (expected {title:?})"
            );
            assert!(
                title.contains(&format!(".{extension}")),
                "the title {title:?} does not name its extension"
            );
        }
    }

    #[test]
    fn unknown_file_types_are_skipped() {
        let mut config = config("grouped");
        config.finder_menu.new_file_types = vec!["exe".into(), "md".into()];
        let model = build(&config);
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
