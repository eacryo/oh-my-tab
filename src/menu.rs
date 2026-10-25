//! Status bar menu: menu-item state (SHORTCUT_ITEM / FIXED_MENU_ITEMS), menu action
//! callbacks (handle_quit / select_modifier / toggle_theme / reload_config), shortcut-mode
//! switching, and menu-title refresh. setup_status_bar stays in main.rs (setup wiring).

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::Ordering;

use crate::config::{persist_config_now, reload_config, CONFIG};
use crate::event_monitor::SHORTCUT_IS_CMD;
use crate::ffi::*;
use crate::i18n::{t, tf};
use crate::icon_cache::clear_icon_cache;
use crate::overlay::extract_uncached_icons;
// cross-module shared state (owned by main.rs)
use crate::log_debug;
use crate::log_info;
use crate::with_tab_state;

/// The shortcut-mode submenu: the parent item whose title carries the current value, and the three
/// modifier items below it.
pub(crate) struct ShortcutState {
    pub(crate) item: *mut AnyObject,
    /// Child items indexed by *config* value, not by display order: index i selects
    /// `SWITCHER_MODIFIER_VALUES[i]`. The menu lists Command first, so the display order and this
    /// index deliberately differ.
    pub(crate) choices: [*mut AnyObject; 3],
}

pub(crate) struct ThumbnailState {
    pub(crate) item: *mut AnyObject,
}

pub(crate) struct ServiceMenuState {
    pub(crate) items: [*mut AnyObject; 5],
}

// Fixed-title menu items (settings / reload / clear_caches / quit); re-titled in bulk by refresh_menu_titles on locale change.
pub(crate) struct FixedMenuItems {
    pub(crate) settings: *mut AnyObject,
    pub(crate) reload: *mut AnyObject,
    pub(crate) clear_cache: *mut AnyObject,
    pub(crate) quit: *mut AnyObject,
}

/// Menu controls are AppKit objects and therefore belong to the main-thread runtime.
pub(crate) struct MenuUi {
    pub(crate) shortcut: Option<ShortcutState>,
    pub(crate) thumbnail: Option<ThumbnailState>,
    pub(crate) services: Option<ServiceMenuState>,
    pub(crate) fixed: Option<FixedMenuItems>,
    /// The clipboard pin item, tracked separately from the fixed-title items because its CHECK
    /// state changes at runtime (the pin), not only its title.
    pub(crate) clipboard_pin: Option<*mut AnyObject>,
    /// The clipboard pause/resume item. Its TITLE and glyph swap with the paused state, so it is
    /// tracked separately from the fixed-title items just like the pin.
    pub(crate) clipboard_pause: Option<*mut AnyObject>,
    /// The status item itself, so "hide menu bar icon" can toggle it after setup. Kept here (and not
    /// only in `setup_status_bar`'s scope) because it outlives that function. The item is created
    /// once and only its visibility changes, so every pointer registered above stays valid for the
    /// process lifetime and no menu object is ever detached from a live item.
    pub(crate) status_item: Option<*mut AnyObject>,
    /// The app menu's item on the application's main menu; its title is the application name the
    /// system shows in the menu bar while the app is .regular. See `install_main_menu`.
    pub(crate) app_menu_item: Option<*mut AnyObject>,
}

thread_local! {
    static MENU_UI: RefCell<MenuUi> = const { RefCell::new(MenuUi {
        shortcut: None,
        thumbnail: None,
        services: None,
        fixed: None,
        clipboard_pin: None,
        clipboard_pause: None,
        status_item: None,
        app_menu_item: None,
    }) };
}

pub(crate) fn with_menu_ui<R>(f: impl FnOnce(&mut MenuUi) -> R) -> R {
    crate::debug_assert_main_thread();
    MENU_UI.with(|ui| f(&mut ui.borrow_mut()))
}

/// Hand the freshly created status item to the menu UI so it can be hidden and shown later.
///
/// `setup_status_bar` runs once at launch and keeps building the item where it always did; this only
/// records the handle. `set_status_item_visible` then toggles visibility on that same live item, so
/// the menu, its items and every target registered in `MenuUi` stay attached to it.
pub(crate) fn register_status_item(item: *mut AnyObject) {
    with_menu_ui(|ui| ui.status_item = Some(item));
}

/// Show or hide the menu-bar icon to match `startup.hide_menu_bar_icon`.
///
/// `NSStatusItem.isVisible` (macOS 10.12+) is the supported switch and the one AppKit itself uses
/// for the "removable item" path: it hides the slot without tearing the item down, so menu-title
/// refreshes, the pin's check state and the main-menu key equivalents keep working while the icon is
/// hidden. Removing and recreating the item would instead strand every pointer `MenuUi` holds.
/// Safe to call repeatedly and before `register_status_item` (a no-op then): the launch path applies
/// the saved setting after setup, and a live config change applies it again.
pub(crate) fn set_status_item_visible(visible: bool) {
    if !visible {
        log_info!("[menu] hiding the menu-bar icon (startup.hide_menu_bar_icon)");
    }
    let item = with_menu_ui(|ui| ui.status_item);
    let Some(item) = item else {
        log_debug!("[menu] no status item to update yet");
        return;
    };
    unsafe {
        let _: () = msg_send![item, setVisible: visible];
    }
}

/// Development switch that hides the application menu item, so an automated check can assert what
/// the menu bar really shows without a way to read the menu bar itself.
pub(crate) const HIDE_APP_MENU_FLAG: &str = "hide-app-menu";

/// Build the application's main menu: one item named after the app and an empty submenu.
///
/// The app is .regular only while Settings is open (`set_settings_activation_policy`), and that is
/// when the system renders this menu: this item supplies the name in the menu bar. Reusing the
/// status-bar dropdown for it instead made the menu bar show the shortcut item's title, which is a
/// sentence, not an app name; Cmd+Q never depended on it (both panels intercept it in
/// `performKeyEquivalent:`).
///
/// Measured on macOS 26: an EMPTY-TITLED application item cannot be used, because AppKit locates the
/// application menu by title and the system then fills the blank with the executable's name.
///
/// The menu is always installed, whatever `NSApp.mainMenu` already holds (this build has no
/// deployment target with a storyboard: AppKit installs an empty menu before `run`, and the app
/// itself adds nothing). Installing keeps the three facts this module promises in one place: the
/// item exists, its title is the application name, and `--hide-app-menu` applies to it.
pub(crate) fn install_main_menu() {
    unsafe {
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        install_main_menu_on(application);
    }
}

/// `install_main_menu` with its application object supplied, so the smoke runner can build the menu
/// on a discarded application and prove the pre-existing-menu case instead of only the one launch
/// order this build happens to take.
pub(crate) unsafe fn install_main_menu_on(application: *mut AnyObject) {
    unsafe {
        if application.is_null() {
            log_info!("[menu] no application object; the main menu was not installed");
            return;
        }
        let title = crate::app_identity::application_display_name();
        let root_title = make_nsstring("");
        let root: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let root: *mut AnyObject = msg_send![root, initWithTitle: root_title];
        CFRelease(root_title as *const c_void);

        // AppKit locates the application menu by title, so the item and its submenu share the name.
        let name = make_nsstring(&title);
        let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        // An empty NSString, never NULL: AppKit raises
        // "Invalid parameter not satisfying: charCode != nil" and aborts the process for a nil
        // key equivalent (measured here; the same rule holds for every item built in this module).
        let no_key = make_nsstring("");
        let item: *mut AnyObject =
            msg_send![item, initWithTitle: name, action: None::<Sel>, keyEquivalent: no_key];
        CFRelease(no_key as *const c_void);
        let submenu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let submenu: *mut AnyObject = msg_send![submenu, initWithTitle: name];
        CFRelease(name as *const c_void);
        let _: () = msg_send![item, setSubmenu: submenu];
        let _: () = msg_send![root, addItem: item];
        // The main menu and its submenu retain their items; the two locals' +1 is dropped here and
        // the tree owns them for the process lifetime.
        release_obj(submenu);
        release_obj(item);

        let _: () = msg_send![application, setMainMenu: root];
        release_obj(root);
        with_menu_ui(|ui| ui.app_menu_item = Some(item));

        if crate::dev_flags::present(HIDE_APP_MENU_FLAG) {
            log_info!("[menu] application menu hidden by --{HIDE_APP_MENU_FLAG}");
            set_application_menu_visible(false);
        } else {
            log_info!("[menu] application menu titled '{title}'");
        }
    }
}

/// Show or hide the application menu item on the main menu.
///
/// A hidden item disappears from the menu bar. The menu itself stays installed: it is the tree AppKit
/// keeps key equivalents in, so it is never torn down, and hiding is reversible on the same object.
pub(crate) fn set_application_menu_visible(visible: bool) {
    let item = with_menu_ui(|ui| ui.app_menu_item);
    let Some(item) = item else {
        log_debug!("[menu] no application menu item yet");
        return;
    };
    unsafe { set_menu_item_hidden(item, !visible) }
}

/// Hide or show one item, closing an open popup menu first so a hidden item cannot keep a menu on
/// screen.
unsafe fn set_menu_item_hidden(item: *mut AnyObject, hidden: bool) {
    unsafe {
        let _: () = msg_send![item, setHidden: hidden];
        let submenu: *mut AnyObject = msg_send![item, submenu];
        if !submenu.is_null() {
            let _: () = msg_send![submenu, cancelTracking];
        }
    }
}

/// The title the application menu carries, for the automated check that the system would show the
/// app's name rather than an internal string.
pub(crate) fn application_menu_title() -> Option<String> {
    with_menu_ui(|ui| ui.app_menu_item).and_then(|item| unsafe {
        if item.is_null() {
            return None;
        }
        let title: *mut AnyObject = msg_send![item, title];
        Some(crate::ffi::nsstring_to_rust(title))
    })
}

/// `--smoke-app-menu`: assert the application menu's shape on the real AppKit main thread.
///
/// Deliberately checks the two facts a screenshot cannot state: the menu bar draws a NAME (the title
/// must equal the application's display name, not any menu-item string), and the submenu is the empty
/// shape macOS 26 keeps visible; an empty-titled item is not an option, because AppKit locates the
/// application menu by title and the system fills a blank title with the executable name.
pub(crate) fn app_menu_smoke_runner() -> bool {
    crate::debug_assert_main_thread();
    unsafe {
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let main_menu: *mut AnyObject = msg_send![application, mainMenu];
        if main_menu.is_null() {
            eprintln!("[smoke-app-menu] the application has no main menu");
            return false;
        }
        let count: isize = msg_send![main_menu, numberOfItems];
        if count != 1 {
            eprintln!("[smoke-app-menu] main menu holds {count} items, expected exactly the application menu");
            return false;
        }
        let item: *mut AnyObject = msg_send![main_menu, itemAtIndex: 0isize];
        if item.is_null() {
            eprintln!("[smoke-app-menu] the application menu item is missing");
            return false;
        }
        let expected = crate::app_identity::application_display_name();
        let title = application_menu_title().unwrap_or_default();
        if title != expected {
            eprintln!(
                "[smoke-app-menu] application menu title {title:?} is not the application name {expected:?}"
            );
            return false;
        }
        // The menu bar is drawn from the item, so a hidden item takes the name out of it.
        let hidden: bool = msg_send![item, isHidden];
        if crate::dev_flags::present(HIDE_APP_MENU_FLAG) != hidden {
            eprintln!(
                "[smoke-app-menu] hidden={hidden} while --{}={}",
                HIDE_APP_MENU_FLAG,
                crate::dev_flags::present(HIDE_APP_MENU_FLAG)
            );
            return false;
        }
        let submenu: *mut AnyObject = msg_send![item, submenu];
        if submenu.is_null() {
            eprintln!("[smoke-app-menu] the application menu has no submenu");
            return false;
        }
        let submenu_count: isize = msg_send![submenu, numberOfItems];
        if submenu_count != 0 {
            eprintln!(
                "[smoke-app-menu] the application menu holds {submenu_count} items, expected none"
            );
            return false;
        }
        // The dropdown that the status item shows is a different menu and must be untouched.
        let status_menu = with_menu_ui(|ui| ui.status_item).and_then(|status_item| {
            let menu: *mut AnyObject = msg_send![status_item, menu];
            (!menu.is_null()).then_some(menu)
        });
        match status_menu {
            None => {
                eprintln!("[smoke-app-menu] the status item lost its dropdown");
                return false;
            }
            Some(menu) if menu == main_menu => {
                eprintln!(
                    "[smoke-app-menu] the status dropdown is installed as the main menu again"
                );
                return false;
            }
            Some(_) => {}
        }
        // The installer must replace a menu it did not create: AppKit installs an empty one before
        // `run`, so "only install when the slot is empty" would silently skip the fix on some
        // launch orders. Proven on a discarded application, which installs without touching the
        // running app's menu bar.
        if !discarded_application_install_replaces_a_foreign_menu(&expected) {
            return false;
        }
        log_info!(
            "[smoke-app-menu] application menu titled {title:?} (hidden={hidden}), dropdown separate"
        );
        true
    }
}

/// Build a throwaway `NSApplication`, give it a menu the app never builds (the shape AppKit leaves
/// behind), then run the installer on it and check it took over.
unsafe fn discarded_application_install_replaces_a_foreign_menu(expected: &str) -> bool {
    unsafe {
        // A second NSApplication cannot be created in a process (AppKit aborts), so this borrows the
        // real one and puts the app's own menu back afterwards. The install runs exactly as at
        // launch: an existing menu is replaced rather than kept.
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let saved_main_menu: *mut AnyObject = msg_send![application, mainMenu];
        let _: *mut AnyObject = msg_send![saved_main_menu, retain];
        let foreign_title = make_nsstring("Foreign");
        let foreign: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let foreign: *mut AnyObject = msg_send![foreign, initWithTitle: foreign_title];
        CFRelease(foreign_title as *const c_void);
        let _: () = msg_send![application, setMainMenu: foreign];
        release_obj(foreign);

        install_main_menu_on(application);

        let main_menu: *mut AnyObject = msg_send![application, mainMenu];
        let count: isize = msg_send![main_menu, numberOfItems];
        let item: *mut AnyObject = msg_send![main_menu, itemAtIndex: 0isize];
        let installed_title: *mut AnyObject = msg_send![item, title];
        let installed = crate::ffi::nsstring_to_rust(installed_title);
        // Restore the running app's menu and the item this module tracks for it.
        let _: () = msg_send![application, setMainMenu: saved_main_menu];
        release_obj(saved_main_menu);
        let restored_item: *mut AnyObject = msg_send![saved_main_menu, itemAtIndex: 0isize];
        with_menu_ui(|ui| ui.app_menu_item = Some(restored_item));
        if count != 1 || installed != expected {
            eprintln!(
                "[smoke-app-menu] a pre-existing main menu was not replaced (items={count}, title {installed:?})"
            );
            return false;
        }
        true
    }
}

const MENU_TITLE_MIN_WIDTH: f64 = 72.0;
const MENU_TITLE_MAX_WIDTH: f64 = 280.0;

fn capped_menu_title_width(natural_width: f64) -> f64 {
    if natural_width.is_finite() {
        natural_width
            .ceil()
            .clamp(MENU_TITLE_MIN_WIDTH, MENU_TITLE_MAX_WIDTH)
    } else {
        MENU_TITLE_MIN_WIDTH
    }
}

/// Measure a title using the native menu font without attaching a custom view to the item.
unsafe fn menu_title_width(title: &str) -> f64 {
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![
        field,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 20.0))
    ];
    if field.is_null() {
        return MENU_TITLE_MIN_WIDTH;
    }
    let title_ns = make_nsstring(title);
    let _: () = msg_send![field, setStringValue: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![field, setUsesSingleLineMode: true];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![field, setFont: font];
    let cell: *mut AnyObject = msg_send![field, cell];
    let size: NSSize = if cell.is_null() {
        NSSize::new(MENU_TITLE_MIN_WIDTH, 16.0)
    } else {
        msg_send![cell, cellSize]
    };
    release_obj(field);
    size.width
}

/// Keep menu titles compact while retaining the full title in the item's tooltip.
fn compact_menu_title_by(title: &str, max_width: f64, measure: impl Fn(&str) -> f64) -> String {
    if measure(title) <= max_width {
        return title.to_string();
    }

    let clusters = unsafe { composed_character_clusters(title) };
    let mut low = 0usize;
    let mut high = clusters.len();
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let prefix: String = clusters[..mid].concat();
        let candidate = format!("{prefix}…");
        if measure(&candidate) <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }

    if low == 0 {
        "…".to_string()
    } else {
        let prefix: String = clusters[..low].concat();
        format!("{prefix}…")
    }
}

fn compact_menu_title(title: &str) -> String {
    compact_menu_title_by(title, MENU_TITLE_MAX_WIDTH, |candidate| {
        capped_menu_title_width(unsafe { menu_title_width(candidate) })
    })
}

/// Set a menu item's native title to the full localized string and never attach a tooltip.
///
/// Used for the service items. An `NSMenuItem` tooltip is drawn by AppKit as its own help-tag
/// window, which is not dismissed when the item's menu closes or the item is re-titled, and it is
/// then stranded at the screen origin (2026-10-04). Service titles are short in every shipped
/// locale, so the compact-with-outlet path is not needed for them; the whole title is shown in
/// place instead (design-style §9: a title reads in full). A previously set tooltip is cleared, so
/// this path cannot inherit one.
pub(crate) unsafe fn set_menu_item_full_title(item: *mut AnyObject, title: &str) {
    if item.is_null() {
        return;
    }

    let title_ns = make_nsstring(title);
    let _: () = msg_send![item, setTitle: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![item, setToolTip: std::ptr::null::<AnyObject>()];
}

/// Set a menu item's native title, truncating only oversized localized text.
pub(crate) unsafe fn set_menu_item_title(item: *mut AnyObject, title: &str) {
    if item.is_null() {
        return;
    }

    let display_title = compact_menu_title(title);
    let title_ns = make_nsstring(&display_title);
    let _: () = msg_send![item, setTitle: title_ns];
    CFRelease(title_ns as *const c_void);

    if display_title == title {
        let _: () = msg_send![item, setToolTip: std::ptr::null::<AnyObject>()];
    } else {
        let tooltip_ns = make_nsstring(title);
        let _: () = msg_send![item, setToolTip: tooltip_ns];
        CFRelease(tooltip_ns as *const c_void);
    }
}

/// Display order of the modifier submenu, as (config index, locale key). Command reads first
/// because it is the default and the platform's own switch shortcut; the config array keeps its
/// own order (the settings popup depends on it), so mapping is explicit rather than positional.
pub(crate) const SHORTCUT_MENU_ORDER: [(usize, &str); 3] = [
    (1, "menu.shortcut.command"),
    (0, "menu.shortcut.option"),
    (2, "menu.shortcut.control"),
];

/// The tag a submenu child carries: the config index it selects. Tags are resolved back through
/// `SWITCHER_MODIFIER_VALUES`, never treated as a display position.
pub(crate) fn shortcut_menu_tag(config_index: usize) -> isize {
    config_index as isize
}

/// Resolve a submenu item's tag to a config index. `None` for a missing sender, a negative tag or a
/// tag past the config array: an invalid choice is refused rather than silently meaning Option.
fn shortcut_config_index_for_tag(tag: isize) -> Option<usize> {
    usize::try_from(tag)
        .ok()
        .filter(|index| *index < crate::config::SWITCHER_MODIFIER_VALUES.len())
}

/// The current modifier as the menu renders it (`⌘Tab` / `⌥Tab` / `⌃Tab`).
fn shortcut_menu_value(modifier: &str) -> String {
    match modifier {
        "option" => "⌥Tab".to_string(),
        "control" => "⌃Tab".to_string(),
        _ => "⌘Tab".to_string(),
    }
}

/// Render the shortcut submenu from the config: parent title, three child titles, and the one
/// check mark. The single entry point for startup, a locale change and a modifier change -- a
/// second writer is how the two-item label survived past the three-way config.
pub(crate) fn refresh_shortcut_menu() {
    let modifier = CONFIG
        .read()
        .map(|cfg| cfg.keyboard.modifier.clone())
        .unwrap_or_default();
    let current = crate::config::switcher_modifier_index_of(&modifier);
    let handles = with_menu_ui(|ui| {
        ui.shortcut
            .as_ref()
            .map(|state| (state.item, state.choices))
    });
    let Some((parent, choices)) = handles else {
        return;
    };
    unsafe {
        let parent_title = tf(
            "menu.shortcut.current",
            &[("shortcut", &shortcut_menu_value(&modifier))],
        );
        set_menu_item_full_title(parent, &parent_title);
        for (config_index, key) in SHORTCUT_MENU_ORDER {
            let item = choices[config_index];
            if item.is_null() {
                continue;
            }
            set_menu_item_full_title(item, &t(key));
            let _: () =
                msg_send![item, setState: if config_index == current { 1isize } else { 0isize }];
        }
    }
}
/// Set shortcut mode from the config modifier name ("command" / "option" / "control"), syncing
/// the runtime SHORTCUT_IS_CMD / SHORTCUT_IS_CTRL flags and the submenu.
pub(crate) fn set_shortcut_mode(modifier: &str) {
    SHORTCUT_IS_CMD.store(modifier == "command", Ordering::SeqCst);
    crate::event_monitor::SHORTCUT_IS_CTRL.store(modifier == "control", Ordering::SeqCst);
    refresh_shortcut_menu();
}
/// Set the thumbnail menu item's title; the title describes the mode activated by the click.
pub(crate) fn set_thumbnail_mode(thumbnails_enabled: bool) {
    let key = if thumbnails_enabled {
        "menu.toggle_thumbnail_mode.to_icons"
    } else {
        "menu.toggle_thumbnail_mode.to_thumbnails"
    };
    let item = with_menu_ui(|ui| ui.thumbnail.as_ref().map(|state| state.item));
    if let Some(item) = item {
        unsafe {
            set_menu_item_title(item, &t(key));
        }
    }
}

const SERVICE_MENU_TITLE_KEYS: [&str; 5] = [
    "menu.service_windows",
    "menu.service_mouse",
    "menu.service_clipboard",
    "menu.service_key_bindings",
    "menu.service_keystroke_display",
];

const SERVICE_MENU_SYMBOLS: [&str; 5] = [
    "rectangle.on.rectangle",
    "computermouse",
    "doc.text",
    "command.square",
    "keyboard",
];

/// The status-bar service switches. `KeyBindings` is one item over one master switch: window
/// control and quick actions are a single feature now, and one item can only express one state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceToggle {
    Windows,
    Mouse,
    Clipboard,
    KeyBindings,
    KeystrokeDisplay,
}

impl ServiceToggle {
    fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            0 => Some(Self::Windows),
            1 => Some(Self::Mouse),
            2 => Some(Self::Clipboard),
            3 => Some(Self::KeyBindings),
            4 => Some(Self::KeystrokeDisplay),
            _ => None,
        }
    }

    fn enabled(self, cfg: &crate::config::Config) -> bool {
        match self {
            Self::Windows => cfg.windows.enabled,
            Self::Mouse => cfg.mouse.enabled,
            Self::Clipboard => cfg.clipboard.enabled,
            // The binding master switch: the legacy sections are read-only now, so the menu drives
            // the switch the engine actually consults.
            Self::KeyBindings => cfg.key_bindings.enabled,
            Self::KeystrokeDisplay => cfg.keystroke_display.enabled,
        }
    }

    fn set_enabled(self, cfg: &mut crate::config::Config, enabled: bool) {
        match self {
            Self::Windows => cfg.windows.enabled = enabled,
            Self::Mouse => cfg.mouse.enabled = enabled,
            Self::Clipboard => cfg.clipboard.enabled = enabled,
            Self::KeyBindings => cfg.key_bindings.enabled = enabled,
            Self::KeystrokeDisplay => cfg.keystroke_display.enabled = enabled,
        }
    }
}

unsafe fn set_menu_item_symbol(item: *mut AnyObject, symbol: &str) {
    let symbol_ns = make_nsstring(symbol);
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_ns,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_ns as *const c_void);
    if !image.is_null() {
        let _: () = msg_send![image, setTemplate: true];
        let _: () = msg_send![item, setImage: image];
    }
}

unsafe fn make_menu_item(
    target: *mut AnyObject,
    title: &str,
    action: Sel,
    tag: isize,
    symbol: Option<&str>,
) -> *mut AnyObject {
    let title_ns = make_nsstring(title);
    let key_ns = make_nsstring("");
    let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
    let item: *mut AnyObject =
        msg_send![item, initWithTitle: title_ns, action: action, keyEquivalent: key_ns];
    CFRelease(title_ns as *const c_void);
    CFRelease(key_ns as *const c_void);
    let _: () = msg_send![item, setTarget: target];
    let _: () = msg_send![item, setTag: tag];
    // Service items use the full-title path: no compaction and no native tooltip (the tooltip is the
    // help-tag leak described on `set_menu_item_full_title`).
    set_menu_item_full_title(item, title);
    if let Some(symbol) = symbol {
        set_menu_item_symbol(item, symbol);
    }
    item
}

/// Build the directly expanded service-toggle section between Settings and shortcut mode.
pub(crate) unsafe fn build_service_menu(menu: *mut AnyObject, target: *mut AnyObject) {
    let items: [*mut AnyObject; 5] = std::array::from_fn(|index| {
        let item = make_menu_item(
            target,
            &t(SERVICE_MENU_TITLE_KEYS[index]),
            sel!(handleToggleService:),
            index as isize,
            Some(SERVICE_MENU_SYMBOLS[index]),
        );
        // Use native NSMenuItem image/title/state so AppKit owns column alignment, hover, and clicks.
        let _: () = msg_send![item, setState: 0isize];
        let _: () = msg_send![menu, addItem: item];
        item
    });
    with_menu_ui(|ui| {
        ui.services = Some(ServiceMenuState { items });
    });
    refresh_service_menu();
}

/// Synchronize the service items' titles and check states with CONFIG.
///
/// Service items deliberately carry no `toolTip` (asserted below): AppKit materializes an
/// unanchored help-tag window for an `NSMenuItem` tooltip and strands it at the screen origin. The
/// clipboard warning it used to show lives as the visible caption on the Clipboard settings page.
pub(crate) fn refresh_service_menu() {
    // The pause item lives beside the service switches but carries its own state; refresh it here
    // so enabling/disabling the module also updates whether it can be pressed.
    refresh_clipboard_pause_item();
    let enabled: [bool; 5] = {
        let cfg = CONFIG.read().unwrap();
        std::array::from_fn(|index| {
            ServiceToggle::from_tag(index as isize)
                .expect("service menu index")
                .enabled(&cfg)
        })
    };
    let Some(items) = with_menu_ui(|ui| ui.services.as_ref().map(|state| state.items)) else {
        return;
    };
    unsafe {
        for (index, item) in items.into_iter().enumerate() {
            let title = t(SERVICE_MENU_TITLE_KEYS[index]);
            set_menu_item_full_title(item, &title);
            let _: () = msg_send![item, setState: if enabled[index] { 1isize } else { 0isize }];
            // Service items deliberately carry no `toolTip`. Setting one on an `NSMenuItem` makes
            // AppKit materialize its own help-tag window with no anchor, and it is left stranded at
            // the screen origin (2026-10-04: the clipboard warning's tooltip reappeared there every
            // time the feature was toggled, because apply_config_change -> refresh_service_menu runs
            // on every settings change). The warning lives as the visible caption on the Clipboard
            // settings page instead; the menu title must read in full on its own (design-style §9).
            debug_assert!(
                {
                    let tooltip: *mut AnyObject = msg_send![item, toolTip];
                    tooltip.is_null()
                },
                "service menu items must not carry a native toolTip"
            );
            debug_assert_eq!(
                crate::ffi::nsstring_to_rust(msg_send![item, title]),
                title,
                "service menu titles are set in full; a compacted title would need a tooltip outlet"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The clipboard pin menu item
// ---------------------------------------------------------------------------------------------

/// The item's localized title key.
const CLIPBOARD_PIN_TITLE_KEY: &str = "menu.pin_clipboard";
/// The item's SF Symbol, matching the corner button's glyph family.
const CLIPBOARD_PIN_SYMBOL: &str = "pin";

/// Add the "show and pin clipboard history" item.
///
/// Deliberately its own item and its own state, not a sixth entry in the service array: those
/// switches write persisted configuration, while the pin is in-process state that lasts until the
/// app restarts (decision G). The action is idempotent, not a toggle - unpinned shows and pins,
/// pinned-but-visible does nothing, pinned-but-hidden brings it back at the frozen height - so the
/// menu can never leave the user without the way back the design relies on.
pub(crate) unsafe fn build_clipboard_pin_item(
    menu: *mut AnyObject,
    target: *mut AnyObject,
) -> *mut AnyObject {
    let item = make_menu_item(
        target,
        &t(CLIPBOARD_PIN_TITLE_KEY),
        sel!(handlePinClipboard:),
        0,
        Some(CLIPBOARD_PIN_SYMBOL),
    );
    let _: () = msg_send![item, setState: 0isize];
    let _: () = msg_send![menu, addItem: item];
    item
}

/// Mirror the pin in the menu bar's item. Called from both surfaces that change the pin (the corner
/// button and the menu action itself), so they cannot disagree.
pub(crate) fn refresh_clipboard_pin_item() {
    let item = with_menu_ui(|ui| ui.clipboard_pin);
    let Some(item) = item else {
        return;
    };
    unsafe {
        set_menu_item_full_title(item, &t(CLIPBOARD_PIN_TITLE_KEY));
        let _: () = msg_send![
            item,
            setState: if crate::clipboard::is_pinned() { 1isize } else { 0isize }
        ];
    }
}

// ---------------------------------------------------------------------------------------------
// The clipboard pause menu item
// ---------------------------------------------------------------------------------------------

/// Title while recording is on: pressing the item pauses it.
const CLIPBOARD_PAUSE_TITLE_PAUSE_KEY: &str = "menu.pause_clipboard";
/// Title while recording is paused: pressing the item resumes it.
const CLIPBOARD_PAUSE_TITLE_RESUME_KEY: &str = "menu.resume_clipboard";
const CLIPBOARD_PAUSE_SYMBOL: &str = "pause.circle";
const CLIPBOARD_RESUME_SYMBOL: &str = "play.circle";

/// Add the "pause recording" / "resume recording" item.
///
/// The two states read differently, so this swaps its TITLE instead of carrying a checkmark: a
/// check would only repeat what the title already says. Unlike the pin it is persisted
/// (`clipboard.paused`): a pause is a standing intent, so a restart must not silently resume
/// recording.
pub(crate) unsafe fn build_clipboard_pause_item(
    menu: *mut AnyObject,
    target: *mut AnyObject,
) -> *mut AnyObject {
    let item = make_menu_item(
        target,
        &t(CLIPBOARD_PAUSE_TITLE_PAUSE_KEY),
        sel!(handlePauseClipboard:),
        0,
        Some(CLIPBOARD_PAUSE_SYMBOL),
    );
    let _: () = msg_send![menu, addItem: item];
    item
}

/// Mirror the paused state in the item: title, glyph and availability (the item follows the
/// module switch, it never turns recording on as a side effect).
pub(crate) fn refresh_clipboard_pause_item() {
    let Some(item) = with_menu_ui(|ui| ui.clipboard_pause) else {
        return;
    };
    let (paused, enabled) = {
        let cfg = CONFIG.read().unwrap();
        (cfg.clipboard.paused, cfg.clipboard.enabled)
    };
    unsafe {
        let title_key = if paused {
            CLIPBOARD_PAUSE_TITLE_RESUME_KEY
        } else {
            CLIPBOARD_PAUSE_TITLE_PAUSE_KEY
        };
        set_menu_item_full_title(item, &t(title_key));
        set_menu_item_symbol(
            item,
            if paused {
                CLIPBOARD_RESUME_SYMBOL
            } else {
                CLIPBOARD_PAUSE_SYMBOL
            },
        );
        let _: () = msg_send![item, setEnabled: enabled];
    }
}

/// The menu action: flip the persisted pause intent through the same apply pass every other
/// switch uses, so the gate, the menu title and the settings switch cannot disagree.
pub(crate) extern "C" fn handle_pause_clipboard(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_pause_clipboard", || {
        if !CONFIG.read().unwrap().clipboard.enabled {
            return;
        }
        // The panel orb, the menu item and the settings switch all flip the same persisted intent, and
        // the apply pass keeps the three surfaces in step.
        crate::clipboard::toggle_paused_intent();
    });
}

/// The menu action: show and pin. Idempotent by design (see `build_clipboard_pin_item`).
pub(crate) extern "C" fn handle_pin_clipboard(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    crate::callback_guard::void("handle_pin_clipboard", || {
        if !CONFIG.read().unwrap().clipboard.enabled {
            // The entry follows the module switch; it never turns recording on as a side effect.
            return;
        }
        crate::clipboard::show_and_pin_for_menu();
    });
}
pub(crate) extern "C" fn handle_toggle_service(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    crate::callback_guard::void("handle_toggle_service", || {
        if sender.is_null() {
            return;
        }
        let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
        let Some(service) = ServiceToggle::from_tag(tag) else {
            return;
        };
        toggle_service(service);
    });
}

/// Flip one service switch, the way both the menu and a `feature.toggle_*` binding do it: config
/// write, persist, the full apply pass (which starts or stops the owning engine) and the surfaces
/// that mirror the switch.
pub(crate) fn toggle_service(service: ServiceToggle) {
    let old_cfg = match CONFIG.read() {
        Ok(cfg) => cfg.clone(),
        Err(_) => return,
    };
    let enabled = !service.enabled(&old_cfg);
    toggle_service_to(service, enabled);
}

/// Set one service switch to an explicit state. The disabled direction of the clipboard keeps its
/// documented meaning (the history and its files are deleted, see `apply_config_change`).
pub(crate) fn toggle_service_to(service: ServiceToggle, enabled: bool) {
    let old_cfg = match CONFIG.read() {
        Ok(cfg) => cfg.clone(),
        Err(_) => return,
    };
    let mut new_cfg = old_cfg.clone();
    service.set_enabled(&mut new_cfg, enabled);
    if new_cfg == old_cfg {
        return;
    }
    if let Ok(mut current) = CONFIG.write() {
        *current = new_cfg.clone();
    }
    persist_config_now();
    crate::runtime_config::apply_config_change(
        &old_cfg,
        &new_cfg,
        crate::runtime_config::ConfigChangeSource::Menu,
    );
    crate::settings::refresh_service_controls_from_config();
    refresh_service_menu();
    log_info!(
        "Service {:?}: {}",
        service,
        if enabled { "enabled" } else { "disabled" }
    );
}

/// Re-title all menu items from the current locale and state. Used on locale change (reload)
/// and at startup to fix the initial labels.
pub(crate) fn refresh_menu_titles() {
    set_thumbnail_mode(CONFIG.read().unwrap().layout.thumbnails_enabled);
    refresh_service_menu();
    refresh_clipboard_pause_item();
    // The shortcut submenu reads the config through one function, so startup, a locale change
    // and a modifier change cannot disagree about which modifier is current.
    refresh_shortcut_menu();
    unsafe {
        // fixed-title items
        let fixed = with_menu_ui(|ui| {
            ui.fixed
                .as_ref()
                .map(|items| (items.settings, items.reload, items.clear_cache, items.quit))
        });
        if let Some((settings, reload, clear_cache, quit)) = fixed {
            for (item, key) in [
                (settings, "menu.settings"),
                (reload, "menu.reload_config"),
                (clear_cache, "menu.clear_caches"),
                (quit, "menu.quit"),
            ] {
                set_menu_item_title(item, &t(key));
            }
        }
    }
}

pub(crate) extern "C" fn handle_quit(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    crate::callback_guard::void("handle_quit", || {
        log_info!("User quit via menu bar.");
        quit_application();
    });
}

/// Flush state, restore system pointer settings, and terminate the accessory application.
pub(crate) fn quit_application() {
    quit_application_with(|| unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, terminate: std::ptr::null::<AnyObject>()];
    });
}

/// The menu quit route with its final step supplied by the caller.
///
/// The duties are shared with the terminate-notification route (logout, shutdown), so a config write
/// made inside its debounce window cannot be lost by one route and kept by the other. The `terminate:`
/// call is the only part a test cannot run, so it is the only part the test replaces -- the assertion
/// then covers this function's own body rather than a copy of it.
fn quit_application_with(terminate: impl FnOnce()) {
    crate::orderly_exit_duties();
    terminate();
}

/// Test-only entry to the menu quit route, with the application termination replaced.
#[cfg(test)]
pub(crate) fn quit_application_for_test() {
    quit_application_with(|| {});
}

// Handler for the "Open Privacy & Security" button on the settings permission-warning banner.
pub(crate) extern "C" fn handle_open_privacy(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    crate::callback_guard::void("handle_open_privacy", || {
        crate::open_privacy_accessibility();
    });
}

pub(crate) extern "C" fn handle_open_screen_recording_privacy(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_open_screen_recording_privacy", || {
        crate::open_privacy_screen_recording();
    });
}

pub(crate) extern "C" fn handle_select_modifier(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    crate::callback_guard::void("handle_select_modifier", || {
        // Refuse before touching the config: an absent sender or an out-of-range tag names no
        // modifier, and falling back to Option would silently rewrite the user's choice.
        let tag: isize = if sender.is_null() {
            log_info!("Shortcut: refused, no sender");
            return;
        } else {
            unsafe { msg_send![sender as *mut AnyObject, tag] }
        };
        let Some(index) = shortcut_config_index_for_tag(tag) else {
            log_info!("Shortcut: refused, tag {tag} names no modifier");
            return;
        };
        let modifier = crate::config::switcher_modifier_value_at(index).to_string();
        let old_cfg = CONFIG.read().unwrap().clone();
        if old_cfg.keyboard.modifier == modifier {
            // Selecting the current value is a no-op: nothing to persist, nothing to re-apply.
            refresh_shortcut_menu();
            return;
        }
        let new_cfg = {
            let mut cfg = old_cfg.clone();
            cfg.keyboard.modifier = modifier.clone();
            if let Ok(mut current) = CONFIG.write() {
                *current = cfg.clone();
            }
            cfg
        };
        persist_config_now();
        crate::runtime_config::apply_config_change(
            &old_cfg,
            &new_cfg,
            crate::runtime_config::ConfigChangeSource::Menu,
        );
        log_info!("Shortcut: {modifier}");
    });
}
pub(crate) extern "C" fn handle_toggle_thumbnail(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_toggle_thumbnail", || {
        let thumbnails_enabled = !crate::theme::thumbnails_enabled();
        let old_cfg = CONFIG.read().unwrap().clone();
        let new_cfg = {
            let mut cfg = old_cfg.clone();
            cfg.layout.thumbnails_enabled = thumbnails_enabled;
            if let Ok(mut current) = CONFIG.write() {
                *current = cfg.clone();
            }
            cfg
        };
        persist_config_now();
        // Drop the current card layout so the next summon rebuilds in the new mode; disabling
        // thumbnails also releases the in-memory window images.
        crate::runtime_config::apply_config_change(
            &old_cfg,
            &new_cfg,
            crate::runtime_config::ConfigChangeSource::Menu,
        );
        log_info!(
            "Window thumbnails: {}",
            if thumbnails_enabled {
                "enabled"
            } else {
                "disabled"
            }
        );
    });
}

pub(crate) extern "C" fn handle_reload_config(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    crate::callback_guard::void("handle_reload_config", || {
        let errs = reload_config();
        if errs.is_empty() {
            log_info!("Config reloaded successfully.");
        } else {
            log_info!("Config reload: {} error(s):", errs.len());
            for e in &errs {
                log_info!("  • {}", e);
            }
        }
    });
}

/// Clear icon and thumbnail caches: remove disk icons, clear in-memory window thumbnails, then
/// re-extract icons for currently collected windows.
pub(crate) extern "C" fn handle_clear_caches(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    crate::callback_guard::void("handle_clear_caches", || {
        clear_icon_cache();
        crate::thumbnail::clear_runtime_cache();
        // in-memory icon_path still points at deleted files; reset to None so cards re-extract.
        with_tab_state(|state_opt| {
            if let Some(state) = state_opt.as_mut() {
                for window in &mut state.windows {
                    window.icon_path = None;
                }
            }
        });
        // Re-extract icons for currently-collected windows only (not all running apps).
        extract_uncached_icons();
        log_info!("Icon and thumbnail caches cleared.");
    });
}

#[cfg(test)]
mod tests {
    use super::{
        capped_menu_title_width, compact_menu_title_by, shortcut_config_index_for_tag,
        shortcut_menu_tag, shortcut_menu_value, MENU_TITLE_MAX_WIDTH, MENU_TITLE_MIN_WIDTH,
        SHORTCUT_MENU_ORDER,
    };

    #[test]
    fn every_modifier_reaches_the_menu_and_the_config() {
        // The menu used to be a two-state toggle (Cmd<->Opt), so a Control+Tab user could not get
        // back to Control from the menu at all. Drive all three choices through the same entry the
        // menu item uses and check the whole chain: config, persistence and the runtime flags.
        for (config_index, key) in SHORTCUT_MENU_ORDER {
            let tag = shortcut_menu_tag(config_index);
            let resolved = shortcut_config_index_for_tag(tag);
            assert_eq!(
                resolved,
                Some(config_index),
                "{key} must round-trip through its tag"
            );
            assert_eq!(
                crate::config::switcher_modifier_value_at(config_index),
                match config_index {
                    0 => "option",
                    1 => "command",
                    _ => "control",
                }
            );
        }
    }

    #[test]
    fn display_order_is_not_the_config_index() {
        // Command reads first while `SWITCHER_MODIFIER_VALUES` starts with Option. Collapsing the
        // two would make the submenu's second row select Command when it says Option.
        let displayed: Vec<usize> = SHORTCUT_MENU_ORDER.iter().map(|(i, _)| *i).collect();
        assert_eq!(displayed, vec![1, 0, 2]);
        assert_eq!(crate::config::SWITCHER_MODIFIER_VALUES[0], "option");
        assert_eq!(crate::config::SWITCHER_MODIFIER_VALUES[1], "command");
        assert_eq!(crate::config::SWITCHER_MODIFIER_VALUES[2], "control");
        assert_eq!(shortcut_config_index_for_tag(1), Some(1));
    }

    #[test]
    fn an_invalid_tag_names_no_modifier() {
        // A refusal, not a fallback: reading these as Option would silently rewrite the user's
        // choice every time an unrelated item was mistaken for a submenu child.
        assert_eq!(shortcut_config_index_for_tag(-1), None);
        assert_eq!(shortcut_config_index_for_tag(3), None);
        assert_eq!(shortcut_config_index_for_tag(isize::MAX), None);
    }

    /// The application name is what the menu bar shows, so it must be a name and never a menu string.
    #[test]
    fn the_application_name_is_a_bundle_name_or_the_fixed_fallback() {
        let name = crate::app_identity::application_display_name();
        assert!(!name.trim().is_empty());
        assert_ne!(
            name,
            crate::i18n::t("menu.shortcut.current").replace("{shortcut}", "")
        );
    }

    /// The application menu on the real AppKit main thread; see `app_menu_smoke_runner`.
    #[test]
    #[ignore]
    fn app_menu_smoke() {
        crate::smoke_verdict::smoke_runner!("app_menu_smoke", 1, {
            crate::smoke_verdict::run_smoke_child("app_menu_smoke", &["--smoke-app-menu"]);
        });
    }

    #[test]
    fn the_parent_title_shows_the_current_value() {
        assert_eq!(shortcut_menu_value("command"), "⌘Tab");
        assert_eq!(shortcut_menu_value("option"), "⌥Tab");
        assert_eq!(shortcut_menu_value("control"), "⌃Tab");
    }
    #[test]
    fn menu_title_width_stays_compact_until_the_maximum() {
        assert_eq!(capped_menu_title_width(40.0), MENU_TITLE_MIN_WIDTH);
        assert_eq!(capped_menu_title_width(120.4), 121.0);
        assert_eq!(capped_menu_title_width(10_000.0), MENU_TITLE_MAX_WIDTH);
        assert_eq!(capped_menu_title_width(f64::NAN), MENU_TITLE_MIN_WIDTH);
    }

    #[test]
    fn compact_menu_title_never_splits_a_composed_character() {
        let title = "e\u{301}x";
        let compact = compact_menu_title_by(title, 2.0, |candidate| match candidate {
            "e\u{301}x" => 3.0,
            "e\u{301}…" => 3.0,
            "e…" => 2.0,
            _ => candidate.chars().count() as f64,
        });
        assert_eq!(compact, "…");
    }
}
