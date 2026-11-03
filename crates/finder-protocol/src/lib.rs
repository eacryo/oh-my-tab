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
pub const PROTOCOL_VERSION: u32 = 2;

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

/// Which selection a menu item needs before it is shown.
///
/// Visibility only: the app re-validates every click against the live selection and the file type
/// before it reads anything, because a distributed notification can be posted by any process and
/// the extension's view of the selection is a moment old.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuTarget {
    /// Applies to whatever the other rules already allow.
    #[default]
    Any,
    /// Shown only when exactly one regular file is selected. A folder, a special file (a FIFO,
    /// a device), more than one item, or a selection the extension could not inspect all hide it.
    SingleRegularFile,
}

/// What the extension could see of the selection when it built the menu.
///
/// `regular_file` is only meaningful for a single item: the extension stats at most one, because
/// every `SingleRegularFile` item is hidden for any other count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Exactly one item is selected.
    pub single: bool,
    /// That item is a regular file. False when it is not, or when it could not be inspected.
    pub regular_file: bool,
}

impl Selection {
    pub const NONE: Self = Self {
        single: false,
        regular_file: false,
    };
}

/// What clicking a leaf does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MenuAction {
    NewFile { extension: String },
    OpenTerminal { bundle_id: String },
    CopyPath,
    CopySha256,
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
        /// A selection requirement on top of `contexts`. Defaulted so a model built before this
        /// field existed still decodes as `Any`.
        #[serde(default)]
        targets: MenuTarget,
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
        Self::item_targeting(title, symbol, action, contexts, MenuTarget::Any)
    }

    /// An item that additionally requires a particular selection shape.
    pub fn item_targeting(
        title: impl Into<String>,
        symbol: Option<&str>,
        action: MenuAction,
        contexts: &[Context],
        targets: MenuTarget,
    ) -> Self {
        Self::Item {
            title: title.into(),
            symbol: symbol.map(str::to_string),
            action,
            contexts: contexts.to_vec(),
            targets,
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

    /// Whether the item's selection requirement holds.
    pub fn target_applies(&self, selection: Selection) -> bool {
        match self {
            Self::Item { targets, .. } => match targets {
                MenuTarget::Any => true,
                MenuTarget::SingleRegularFile => selection.single && selection.regular_file,
            },
            _ => true,
        }
    }
}

/// Keep only the nodes that apply in `context`, dropping submenus that end up empty and
/// separators (the builder emits none; a hand-made model must not leave a dangling line).
///
/// Pure, so the extension's rendering rules are asserted without a Finder: the app's model, the
/// context and the result are the whole input.
pub fn filter_model(model: &MenuModel, context: Context, selection: Selection) -> MenuModel {
    let mut filtered = model.clone();
    filtered.items = filter_nodes(&model.items, context, selection);
    filtered
}

fn filter_nodes(nodes: &[MenuNode], context: Context, selection: Selection) -> Vec<MenuNode> {
    let mut filtered = Vec::new();
    for node in nodes {
        match node {
            MenuNode::Separator => {}
            MenuNode::Item { .. } => {
                if node.applies(context) && node.target_applies(selection) {
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
                let children = filter_nodes(children, context, selection);
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

    /// The "Copy SHA-256" item is offered only for one regular file. Every other selection shape
    /// must hide it, because the app reads whatever the menu names and a folder, a FIFO or a
    /// multi-item selection has no single digest to copy.
    #[test]
    fn a_single_regular_file_item_needs_exactly_that_selection() {
        let item = MenuNode::item_targeting(
            "Copy SHA-256",
            None,
            MenuAction::CopySha256,
            &[Context::Items],
            MenuTarget::SingleRegularFile,
        );
        let cases = [
            (
                Selection {
                    single: true,
                    regular_file: true,
                },
                true,
                "one regular file",
            ),
            (
                Selection {
                    single: true,
                    regular_file: false,
                },
                false,
                "one folder or special file",
            ),
            (
                Selection {
                    single: false,
                    regular_file: false,
                },
                false,
                "several items",
            ),
            (Selection::NONE, false, "no selection"),
        ];
        for (selection, visible, what) in cases {
            assert_eq!(
                item.target_applies(selection),
                visible,
                "{what} must {} show the item",
                if visible { "" } else { "not" }
            );
        }
    }

    /// A plain item is unaffected by the selection: the new field must not change anything that
    /// was already in the menu.
    #[test]
    fn an_any_target_item_ignores_the_selection() {
        let item = MenuNode::item("Copy Path", None, MenuAction::CopyPath, &[Context::Items]);
        for selection in [
            Selection::NONE,
            Selection {
                single: true,
                regular_file: true,
            },
            Selection {
                single: false,
                regular_file: false,
            },
        ] {
            assert!(item.target_applies(selection));
        }
    }

    /// Filtering drops the item AND the submenu that held only it, so no dangling container is
    /// left in the menu.
    #[test]
    fn filtering_removes_an_emptied_submenu() {
        let model = MenuModel {
            protocol_version: PROTOCOL_VERSION,
            model_version: 1,
            host_bundle_id: "test".into(),
            host_instance: "1".into(),
            items: vec![MenuNode::submenu(
                "Oh-My-Tab",
                None,
                None,
                vec![MenuNode::item_targeting(
                    "Copy SHA-256",
                    None,
                    MenuAction::CopySha256,
                    &[Context::Items],
                    MenuTarget::SingleRegularFile,
                )],
            )],
        };
        let folder = Selection {
            single: true,
            regular_file: false,
        };
        assert!(filter_model(&model, Context::Items, folder)
            .items
            .is_empty());
        let file = Selection {
            single: true,
            regular_file: true,
        };
        assert_eq!(filter_model(&model, Context::Items, file).items.len(), 1);
    }

    /// A model written before `targets` existed decodes as `Any`, so an older build's model is
    /// still renderable rather than rejected.
    #[test]
    fn a_model_without_targets_decodes_as_any() {
        let json = r#"{"kind":"item","title":"Copy Path","symbol":null,"action":{"type":"copy_path"},"contexts":["items"]}"#;
        let node: MenuNode = serde_json::from_str(json).expect("legacy model decodes");
        assert!(node.target_applies(Selection::NONE));
    }
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
        let container = filter_model(&model, Context::Container, Selection::NONE);
        assert_eq!(
            container.items.len(),
            1,
            "only the new-file submenu applies"
        );
        let items = filter_model(&model, Context::Items, Selection::NONE);
        assert_eq!(items.items.len(), 1, "the submenu loses its only child");
        assert!(matches!(items.items[0], MenuNode::Item { .. }));
        let sidebar = filter_model(&model, Context::Sidebar, Selection::NONE);
        assert!(
            sidebar.items.is_empty(),
            "nothing applies to a sidebar click"
        );
    }

    #[test]
    fn filtering_an_empty_model_stays_empty() {
        let model = MenuModel::empty("com.example.app", "instance", 4);
        for context in [Context::Items, Context::Container, Context::Sidebar] {
            assert!(filter_model(&model, context, Selection::NONE)
                .items
                .is_empty());
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
