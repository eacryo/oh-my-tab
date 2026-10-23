//! Rendering the cached menu model as an NSMenu.
//!
//! The extension is a renderer: every title, symbol and action comes from the model the app sent.
//! Each item carries its ClickTemplate in `representedObject`, so a click reports exactly what the
//! user saw even if the app has since published a newer model.

use finder_protocol::{ClickTemplate, MenuNode, Trigger};
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};

use crate::ffi::{log, new_menu, nsstring, nsstring_to_rust};

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
    let represented: *mut AnyObject = msg_send![sender, representedObject];
    if represented.is_null() {
        return None;
    }
    let json = nsstring_to_rust(represented);
    let template: Option<ClickTemplate> = finder_protocol::decode(&json);
    if template.is_none() {
        log("menu item had no readable click template");
    }
    template
}

#[cfg(test)]
mod tests {
    // Rendering needs AppKit, which the extension process only provides inside its host; the
    // model-shaping logic that *is* testable lives in finder-protocol.
}
