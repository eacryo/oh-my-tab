//! Rendering the cached menu model as an NSMenu.
//!
//! The extension is a renderer: every title, symbol and action comes from the model the app sent.
//! Each item carries its ClickTemplate in `representedObject`, so a click reports exactly what the
//! user saw even if the app has since published a newer model.

use finder_protocol::{ClickTemplate, MenuNode, Trigger};
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};

use crate::ffi::{log, new_menu, nsstring, nsstring_to_rust};

/// Click templates by menu-item tag.
///
/// Finder builds its own NSMenu from what the extension returns, across an XPC boundary, so the
/// object an item carries (representedObject) is not guaranteed to survive. RClick, the reference
/// Finder Sync menu app, therefore identifies items by an INTEGER tag, which does. Both are kept:
/// representedObject is the exact snapshot and the tag is the fallback.
///
/// A tag is assigned once per (template) content and reused, so the table stays bounded by the
/// number of distinct items the user can click, and a menu built later does not renumber the tags
/// of a menu Finder is still holding.
static TEMPLATES: ::std::sync::OnceLock<::std::sync::Mutex<TagTable>> =
    ::std::sync::OnceLock::new();

/// How many click snapshots the tag table keeps; older ones are evicted first.
///
/// The key includes the model version and the app instance, so it changes whenever the app
/// republishes its model; without a cap a long-running extension would accumulate one entry per
/// menu rebuild.
const TAG_TABLE_LIMIT: usize = 512;

/// Click snapshots by tag, oldest first.
///
/// Tags are strictly increasing and never reused, so evicting an old entry makes its tag
/// unresolvable -- an item from a menu built before the eviction is refused, and can never be
/// matched to a snapshot that was registered after it.
#[derive(Default)]
pub(crate) struct TagTable {
    entries: Vec<(isize, ClickTemplate)>,
    next_tag: isize,
}

fn tag_table() -> &'static ::std::sync::Mutex<TagTable> {
    TEMPLATES.get_or_init(|| ::std::sync::Mutex::new(TagTable::default()))
}

fn remember_in(table: &mut TagTable, template: ClickTemplate) -> isize {
    if let Some((tag, _)) = table.entries.iter().find(|(_, known)| *known == template) {
        return *tag;
    }
    table.next_tag += 1;
    let tag = table.next_tag;
    table.entries.push((tag, template));
    while table.entries.len() > TAG_TABLE_LIMIT {
        table.entries.remove(0);
    }
    tag
}

fn remember_template(template: ClickTemplate) -> isize {
    let mut table = tag_table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remember_in(&mut table, template)
}

/// Which click a menu item reports.
///
/// The represented object is the exact snapshot of the item the user clicked; the tag is the
/// fallback for a menu whose objects Finder did not carry over its XPC boundary. Pure so the
/// fallback, the precedence, a stale tag and the eviction rule are asserted without a Finder.
pub(crate) fn resolve_click(
    table: &TagTable,
    represented: Option<&str>,
    tag: isize,
) -> Option<ClickTemplate> {
    if let Some(json) = represented {
        if let Some(template) = finder_protocol::decode::<ClickTemplate>(json) {
            return Some(template);
        }
    }
    table
        .entries
        .iter()
        .find(|(known, _)| *known == tag)
        .map(|(_, template)| template.clone())
}
/// Build the context menu for `trigger`. None means "no items apply"; Finder then shows nothing.
pub(crate) fn build(trigger: Trigger, target: *mut AnyObject) -> Option<*mut AnyObject> {
    // No model (feature off, app gone, or nothing applies): offer no items at all.
    let model = crate::state::model(trigger.context())?;
    unsafe {
        let menu = new_menu("Oh-My-Tab");
        if menu.is_null() {
            return None;
        }
        populate(
            menu,
            &model.items,
            trigger,
            target,
            &model.host_instance,
            model.model_version,
        );
        let count: isize = msg_send![menu, numberOfItems];
        crate::ffi::log_static("[finder-ext] menu built");
        (count > 0).then_some(menu)
    }
}

unsafe fn populate(
    menu: *mut AnyObject,
    nodes: &[MenuNode],
    trigger: Trigger,
    target: *mut AnyObject,
    host_instance: &str,
    model_version: u64,
) {
    for node in nodes {
        match node {
            MenuNode::Separator => {
                let item: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
                let _: () = msg_send![menu, addItem: item];
            }
            MenuNode::Item {
                title,
                symbol,
                action,
                ..
            } => {
                // The identity comes from the model being rendered, never from whatever the
                // cache holds now: an item must report the process that built the menu it is in.
                let template = ClickTemplate {
                    action: action.clone(),
                    trigger,
                    host_instance: host_instance.to_string(),
                    model_version,
                };
                let Some(json) = finder_protocol::encode(&template) else {
                    continue;
                };
                let item = new_item(title, symbol.as_deref(), sel!(handleMenuClick:));
                if item.is_null() {
                    continue;
                }
                let _: () = msg_send![item, setTarget: target];
                // The tag is what survives Finder's own copy of the menu; see TEMPLATES.
                let tag = remember_template(template.clone());
                let _: () = msg_send![item, setTag: tag];
                let _: () = msg_send![item, setRepresentedObject: nsstring(&json)];
                let _: () = msg_send![menu, addItem: item];
            }
            MenuNode::Submenu {
                title,
                symbol,
                children,
                ..
            } => {
                // A submenu item carries no action of its own; its children do.
                let item = new_item(title, symbol.as_deref(), sel!(doesNotExist:));
                if item.is_null() {
                    continue;
                }
                let submenu = new_menu(title);
                if submenu.is_null() {
                    continue;
                }
                populate(
                    submenu,
                    children,
                    trigger,
                    target,
                    host_instance,
                    model_version,
                );
                let count: isize = msg_send![submenu, numberOfItems];
                if count == 0 {
                    continue;
                }
                let _: () = msg_send![item, setSubmenu: submenu];
                let _: () = msg_send![menu, addItem: item];
            }
        }
    }
}

/// One top-level menu item with an optional SF Symbol.
unsafe fn new_item(title: &str, symbol: Option<&str>, action: Sel) -> *mut AnyObject {
    let empty = nsstring("");
    let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
    if item.is_null() {
        return item;
    }
    let item: *mut AnyObject = msg_send![
        item,
        initWithTitle: nsstring(title),
        action: action,
        keyEquivalent: empty
    ];
    if item.is_null() {
        return item;
    }
    let _: *mut AnyObject = msg_send![item, autorelease];
    if let Some(symbol) = symbol {
        let image: *mut AnyObject = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: nsstring(symbol),
            accessibilityDescription: std::ptr::null::<AnyObject>()
        ];
        if !image.is_null() {
            let _: () = msg_send![item, setImage: image];
        }
    }
    item
}

/// Read the ClickTemplate a menu item carried.
pub(crate) unsafe fn template_for(sender: *mut AnyObject) -> Option<ClickTemplate> {
    if sender.is_null() {
        return None;
    }
    let tag: isize = msg_send![sender, tag];
    let represented: *mut AnyObject = msg_send![sender, representedObject];
    log(&format!(
        "menu click: tag={tag} represented={}",
        !represented.is_null()
    ));
    let json = (!represented.is_null()).then(|| nsstring_to_rust(represented));
    let table = tag_table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let resolved = resolve_click(&table, json.as_deref(), tag);
    drop(table);
    if resolved.is_none() {
        log("menu click: the item carries neither a template nor a known tag");
    }
    resolved
}

#[cfg(test)]
mod tests {
    // Rendering itself needs AppKit, which the extension process only provides inside its host;
    // the model shaping lives in finder-protocol and the click resolution below is pure.
    use super::*;
    use finder_protocol::MenuAction;

    fn template(action: MenuAction, instance: &str, version: u64) -> ClickTemplate {
        ClickTemplate {
            action,
            trigger: Trigger::Container,
            host_instance: instance.to_string(),
            model_version: version,
        }
    }

    /// The click fix itself: a menu item whose represented object was lost across Finder's XPC
    /// boundary is still resolved by its tag, to the snapshot it was built from.
    #[test]
    fn a_tag_resolves_the_snapshot_when_the_object_is_gone() {
        let mut table = TagTable::default();
        let md = template(
            MenuAction::NewFile {
                extension: "md".into(),
            },
            "instance-a",
            4,
        );
        let tag = remember_in(&mut table, md.clone());
        assert_eq!(resolve_click(&table, None, tag), Some(md));
    }

    #[test]
    fn the_represented_object_wins_over_the_tag() {
        let mut table = TagTable::default();
        let old = template(MenuAction::CopyPath, "instance-old", 1);
        let new = template(MenuAction::CopyPath, "instance-new", 9);
        let tag = remember_in(&mut table, old);
        let json = finder_protocol::encode(&new).unwrap();
        assert_eq!(resolve_click(&table, Some(&json), tag), Some(new));
    }

    #[test]
    fn an_unknown_or_invalid_tag_is_refused() {
        let mut table = TagTable::default();
        remember_in(&mut table, template(MenuAction::CopyPath, "instance", 2));
        assert_eq!(resolve_click(&table, None, 0), None);
        assert_eq!(resolve_click(&table, None, -3), None);
        assert_eq!(resolve_click(&table, None, 7), None);
        assert_eq!(resolve_click(&table, Some("not json"), 7), None);
    }

    /// The counterexample from review: a tag handed out before the table evicted anything must not
    /// resolve once a later, different snapshot has taken its place. Tags are never reused, so the
    /// stale one simply stops resolving and the click is refused.
    #[test]
    fn an_evicted_tag_never_resolves_to_a_newer_snapshot() {
        let mut table = TagTable::default();
        let first = remember_in(&mut table, template(MenuAction::CopyPath, "instance", 1));
        assert_eq!(first, 1);
        for version in 0..(TAG_TABLE_LIMIT as u64 + 1) {
            remember_in(
                &mut table,
                template(MenuAction::CopyPath, "instance", version + 2),
            );
        }
        let later = remember_in(
            &mut table,
            template(
                MenuAction::NewFile {
                    extension: "md".into(),
                },
                "instance",
                TAG_TABLE_LIMIT as u64 + 99,
            ),
        );
        assert_ne!(later, first, "the new snapshot must not reuse the old tag");
        assert_eq!(resolve_click(&table, None, first), None);
    }

    /// The table is capped: after enough distinct snapshots the oldest are evicted, so memory does
    /// not grow with every model republish.
    #[test]
    fn the_table_evicts_the_oldest_instead_of_growing() {
        let mut table = TagTable::default();
        for version in 0..(TAG_TABLE_LIMIT as u64 + 5) {
            remember_in(
                &mut table,
                template(MenuAction::CopyPath, "instance", version),
            );
        }
        assert_eq!(table.entries.len(), TAG_TABLE_LIMIT);
        let newest = template(MenuAction::CopyPath, "instance", TAG_TABLE_LIMIT as u64 + 4);
        assert_eq!(
            table.entries.last().map(|(_, template)| template),
            Some(&newest)
        );
    }
    #[test]
    fn the_same_snapshot_reuses_its_tag() {
        let mut table = TagTable::default();
        let first = remember_in(&mut table, template(MenuAction::CopyPath, "instance", 1));
        let again = remember_in(&mut table, template(MenuAction::CopyPath, "instance", 1));
        assert_eq!(first, again);
        assert_eq!(table.entries.len(), 1);
    }
}
