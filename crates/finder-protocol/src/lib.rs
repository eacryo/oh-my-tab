//! Wire contract between the main app and the Finder Sync app extension.
//!
//! The extension is sandboxed and short-lived; all it needs is the menu tree plus enough
//! context to decide which items apply. Every message carries the host bundle id and the main
//! app's launch instance, so messages from another channel (dev vs release) or from an earlier
//! app process are discarded instead of acting on a stale menu.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Bumped whenever a wire shape changes incompatibly. Both sides refuse a message from another
/// protocol version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Cap on a single notification payload. Menu models are small; anything larger is a bug or a
/// spoof attempt and is rejected before parsing.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Where a menu item is meaningful, as reported by Finder's menu kind and the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Context {
    /// An item (or several) is selected.
    Items,
    /// The folder background is clicked.
    Container,
    /// A sidebar entry is clicked.
    Sidebar,
}

/// What clicking a leaf does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MenuAction {
    NewFile { extension: String },
    OpenTerminal { bundle_id: String },
    CopyPath,
}

/// One rendered menu element. The app builds the tree; the extension only walks it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MenuNode {
    Item {
        title: String,
        symbol: Option<String>,
        action: MenuAction,
        contexts: Vec<Context>,
    },
    Submenu {
        title: String,
        symbol: Option<String>,
        /// `None` means "always visible": used by the single outer submenu of the submenu layout.
        contexts: Option<Vec<Context>>,
        children: Vec<MenuNode>,
    },
    Separator,
}

impl MenuNode {
    pub fn item(
        title: impl Into<String>,
        symbol: Option<&str>,
        action: MenuAction,
        contexts: &[Context],
    ) -> Self {
        Self::Item {
            title: title.into(),
            symbol: symbol.map(str::to_string),
            action,
            contexts: contexts.to_vec(),
        }
    }

    pub fn submenu(
        title: impl Into<String>,
        symbol: Option<&str>,
        contexts: Option<&[Context]>,
        children: Vec<MenuNode>,
    ) -> Self {
        Self::Submenu {
            title: title.into(),
            symbol: symbol.map(str::to_string),
            contexts: contexts.map(|contexts| contexts.to_vec()),
            children,
        }
    }

    /// Whether this node applies in a context.
    pub fn applies(&self, context: Context) -> bool {
        match self {
            Self::Item { contexts, .. } => contexts.contains(&context),
            Self::Submenu { contexts, .. } => contexts
                .as_ref()
                .is_none_or(|contexts| contexts.contains(&context)),
            Self::Separator => true,
        }
    }
}

/// Keep only the nodes that apply in `context`, dropping submenus that end up empty and
/// separators (the builder emits none; a hand-made model must not leave a dangling line).
///
/// Pure, so the extension's rendering rules are asserted without a Finder: the app's model, the
/// context and the result are the whole input.
pub fn filter_model(model: &MenuModel, context: Context) -> MenuModel {
    let mut filtered = model.clone();
    filtered.items = filter_nodes(&model.items, context);
    filtered
}

fn filter_nodes(nodes: &[MenuNode], context: Context) -> Vec<MenuNode> {
    let mut filtered = Vec::new();
    for node in nodes {
        match node {
            MenuNode::Separator => {}
            MenuNode::Item { .. } => {
                if node.applies(context) {
                    filtered.push(node.clone());
                }
            }
            MenuNode::Submenu {
                title,
                symbol,
                contexts,
                children,
            } => {
                if contexts
                    .as_ref()
                    .is_some_and(|contexts| !contexts.contains(&context))
                {
                    continue;
                }
                let children = filter_nodes(children, context);
                if !children.is_empty() {
                    filtered.push(MenuNode::Submenu {
                        title: title.clone(),
                        symbol: symbol.clone(),
                        contexts: contexts.clone(),
                        children,
                    });
                }
            }
        }
    }
    filtered
}

/// The whole menu the extension renders, plus the identity of who produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuModel {
    pub protocol_version: u32,
    pub model_version: u64,
    pub host_bundle_id: String,
    pub host_instance: String,
    pub items: Vec<MenuNode>,
}

impl MenuModel {
    pub fn empty(host_bundle_id: &str, host_instance: &str, model_version: u64) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            model_version,
            host_bundle_id: host_bundle_id.to_string(),
            host_instance: host_instance.to_string(),
            items: Vec::new(),
        }
    }
}

/// What a menu item carries: the action to run and the surface that asked, so the click path can
/// resolve the right selection without a second lookup at menu-build time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClickTemplate {
    pub action: MenuAction,
    pub trigger: Trigger,
    /// The app instance whose model produced the item the user clicked. A click from a menu built
    /// by an earlier app process must not act against the current one, so the value travels with
    /// the item instead of being read at click time.
    pub host_instance: String,
    pub model_version: u64,
}

/// Which Finder surface opened the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Items,
    Container,
    Sidebar,
    Toolbar,
}

impl Trigger {
    pub fn context(self) -> Context {
        match self {
            Self::Container => Context::Container,
            Self::Sidebar => Context::Sidebar,
            Self::Items | Self::Toolbar => Context::Items,
        }
    }
}

/// Extension -> app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtToApp {
    /// Liveness + "which model do you have"; the app answers with `State`.
    Sync {
        protocol_version: u32,
        host_bundle_id: String,
        host_instance: String,
        model_version: u64,
    },
    Click {
        protocol_version: u32,
        host_bundle_id: String,
        host_instance: String,
        model_version: u64,
        request_id: String,
        action: MenuAction,
        /// The selected items, or the folder for a background click. Paths are absolute.
        paths: Vec<String>,
        trigger: Trigger,
    },
}

/// App -> extension.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppToExt {
    State {
        protocol_version: u32,
        host_bundle_id: String,
        host_instance: String,
        /// Present only when the app's model is newer than the extension's.
        model: Option<MenuModel>,
    },
    Ack {
        request_id: String,
    },
    Quit,
}

/// Serialize a message, refusing anything above the size cap.
pub fn encode<T: Serialize>(message: &T) -> Option<String> {
    let encoded = serde_json::to_string(message).ok()?;
    (encoded.len() <= MAX_MESSAGE_BYTES).then_some(encoded)
}

/// Parse a message from one of our own channels.
pub fn decode<T: DeserializeOwned>(json: &str) -> Option<T> {
    if json.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    serde_json::from_str(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> MenuModel {
        MenuModel {
            protocol_version: PROTOCOL_VERSION,
            model_version: 7,
            host_bundle_id: "com.example.app".into(),
            host_instance: "instance".into(),
            items: vec![
                MenuNode::submenu(
                    "New File",
                    Some("doc.badge.plus"),
                    None,
                    vec![MenuNode::item(
                        "Markdown",
                        Some("doc.text"),
                        MenuAction::NewFile {
                            extension: "md".into(),
                        },
                        &[Context::Items, Context::Container],
                    )],
                ),
                MenuNode::item(
                    "Open in Ghostty",
                    Some("terminal"),
                    MenuAction::OpenTerminal {
                        bundle_id: "com.mitchellh.ghostty".into(),
                    },
                    &[Context::Container],
                ),
                MenuNode::Separator,
            ],
        }
    }

    #[test]
    fn click_template_round_trips() {
        let template = ClickTemplate {
            action: MenuAction::NewFile {
                extension: "md".into(),
            },
            trigger: Trigger::Container,
            host_instance: "instance".into(),
            model_version: 3,
        };
        assert_eq!(
            decode::<ClickTemplate>(&encode(&template).unwrap()).unwrap(),
            template
        );
    }

    #[test]
    fn menu_model_round_trips() {
        let encoded = encode(&model()).expect("encode");
        let decoded: MenuModel = decode(&encoded).expect("decode");
        assert_eq!(decoded, model());
    }

    #[test]
    fn click_round_trips() {
        let click = ExtToApp::Click {
            protocol_version: PROTOCOL_VERSION,
            host_bundle_id: "com.example.app".into(),
            host_instance: "instance".into(),
            model_version: 3,
            request_id: "request".into(),
            action: MenuAction::CopyPath,
            paths: vec!["/tmp/example".into()],
            trigger: Trigger::Items,
        };
        let decoded: ExtToApp = decode(&encode(&click).expect("encode")).expect("decode");
        assert_eq!(decoded, click);
    }

    #[test]
    fn oversized_messages_are_refused() {
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(encode(&big).is_none());
    }

    #[test]
    fn garbage_is_refused() {
        assert!(decode::<AppToExt>("{").is_none());
        assert!(decode::<AppToExt>("{\"type\":\"unknown\"}").is_none());
    }

    #[test]
    fn filtering_drops_items_for_other_contexts_and_empty_submenus() {
        let model = MenuModel {
            protocol_version: PROTOCOL_VERSION,
            model_version: 1,
            host_bundle_id: "com.example.app".into(),
            host_instance: "instance".into(),
            items: vec![
                MenuNode::submenu(
                    "New File",
                    Some("doc.badge.plus"),
                    None,
                    vec![MenuNode::item(
                        "Markdown",
                        None,
                        MenuAction::NewFile {
                            extension: "md".into(),
                        },
                        &[Context::Container],
                    )],
                ),
                MenuNode::item("Copy Path", None, MenuAction::CopyPath, &[Context::Items]),
                MenuNode::Separator,
            ],
        };
        let container = filter_model(&model, Context::Container);
        assert_eq!(
            container.items.len(),
            1,
            "only the new-file submenu applies"
        );
        let items = filter_model(&model, Context::Items);
        assert_eq!(items.items.len(), 1, "the submenu loses its only child");
        assert!(matches!(items.items[0], MenuNode::Item { .. }));
        let sidebar = filter_model(&model, Context::Sidebar);
        assert!(
            sidebar.items.is_empty(),
            "nothing applies to a sidebar click"
        );
    }

    #[test]
    fn filtering_an_empty_model_stays_empty() {
        let model = MenuModel::empty("com.example.app", "instance", 4);
        for context in [Context::Items, Context::Container, Context::Sidebar] {
            assert!(filter_model(&model, context).items.is_empty());
        }
    }

    #[test]
    fn contexts_filter_items() {
        let node = MenuNode::item("x", None, MenuAction::CopyPath, &[Context::Items]);
        assert!(node.applies(Context::Items));
        assert!(!node.applies(Context::Container));
        assert!(matches!(
            MenuNode::submenu("s", None, None, vec![]),
            MenuNode::Submenu { .. }
        ));
    }
}
