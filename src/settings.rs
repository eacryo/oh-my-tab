//! Settings window: SettingsUi state, control builders (text/popup/header/row), window
//! build/show, the live-apply control dispatcher (every change writes the in-memory CONFIG
//! immediately + persists to disk with a debounce), and restore-defaults (per page / whole
//! app). invalidate_settings_window drops the cached window so it rebuilds after a locale or
//! theme change.

use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRange, NSRect, NSSize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::{persist_config_now, schedule_config_persist, Config, CONFIG};
use crate::event_tap::{
    CGEventGetFlags, CGEventGetIntegerValueField, CGEventRef, CGEventTapProxy, CGEventType,
};
use crate::ffi::*;
use crate::i18n::{t, tf};
use crate::mouse::scrolling::ScrollMode;
use crate::mouse::shortcut::{button_name, describe_shortcut, display_shortcut};
use crate::runtime_config::{apply_config_change, ConfigChangeSource};
use crate::theme::{resolved_is_dark, ui_palette, UiPalette};
use crate::{log_debug, log_info};
// cross-module shared state (owned by main.rs)
use crate::MENU_TARGET;

// Locale popup items are displayed in each language's own script (the usual language-picker
// convention); debug builds add a long-English fixture, while production builds omit it.
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
const LOCALE_LABELS: [&str; 5] = [
    "Auto",
    "English",
    "简体中文",
    "繁體中文",
    "[TEST] English x3",
];
#[cfg(not(any(debug_assertions, feature = "dev-long-text")))]
const LOCALE_LABELS: [&str; 4] = ["Auto", "English", "简体中文", "繁體中文"];
const SCROLL_MODE_LABEL_KEYS: [&str; 3] = [
    "settings.scroll_mode_default",
    "settings.scroll_mode_line",
    "settings.scroll_mode_smooth",
];
const SCROLL_MODE_VALUES: [&str; 3] = ["default", "line", "smooth"];
const KEYSTROKE_DISPLAY_POSITION_VALUES: [&str; 2] = ["main", "caret"];
/// Order matters: it must match the popup's rows and the labels built in `page_builder`.
/// Index 1 ("bottom") is the documented default.
const KEYSTROKE_DISPLAY_INITIAL_POSITION_VALUES: [&str; 4] = ["top", "bottom", "left", "right"];
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
const LOCALE_VALUES: [&str; 5] = [
    "auto",
    "en",
    "zh-Hans",
    "zh-Hant",
    crate::i18n::TEST_LONG_LOCALE,
];
#[cfg(not(any(debug_assertions, feature = "dev-long-text")))]
const LOCALE_VALUES: [&str; 4] = ["auto", "en", "zh-Hans", "zh-Hant"];
const TEXT_SIZE_MIN: i64 = 13;
/// The footer text size's floor is 0, not 13: the far-left stop means "draw no footer at all", which
/// is the switch this slider replaced. A 0 value hides the band (`theme::show_footer_title`).
///
/// The slider's domain is the stop index 0..=STATUS_BAR_SIZE_STOPS-1, not the point size: stop 0
/// is the hidden band and stops 1..=8 map to 13..=20pt, so dragging left from 13 jumps straight to
/// "off" instead of offering 1..12pt.
const STATUS_BAR_SIZE_MIN: i64 = 0;
const STATUS_BAR_SIZE_STOPS: i64 = 9;
/// The stop the 15pt default sits on (13->1, 14->2, 15->3).
const STATUS_BAR_SIZE_DEFAULT_INDEX: i64 = 3;
const TEXT_SIZE_MAX: i64 = 20;
const TEXT_SIZE_DEFAULT: i64 = 15;
const CLIPBOARD_AUTO_EXPIRE_MIN: i64 = 0;
const CLIPBOARD_AUTO_EXPIRE_MAX: i64 = 7;
const CLIPBOARD_AUTO_EXPIRE_DEFAULT: i64 = 3;
pub(super) const SETTINGS_PAGE_COUNT: usize = 8;
/// A page's identity, independent of its position. `SETTINGS_PAGES` holds the sidebar order and
/// pairs each identity with its name; every control array is keyed by this identity, so a reorder
/// cannot leave a hand-written index order pointing at the wrong control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SettingsPageId {
    General,
    Switcher,
    Mouse,
    Clipboard,
    KeyBindings,
    KeystrokeDisplay,
    FinderMenu,
    About,
}

impl SettingsPageId {
    /// The sidebar title key. Lives with the identity so the sidebar build and the selection
    /// refresh read one list, not two.
    pub(super) fn title_key(self) -> &'static str {
        match self {
            SettingsPageId::General => "settings.sidebar_general",
            SettingsPageId::Switcher => "settings.sidebar_switcher",
            SettingsPageId::Mouse => "settings.sidebar_mouse",
            SettingsPageId::Clipboard => "settings.sidebar_clipboard",
            SettingsPageId::KeyBindings => "settings.sidebar_key_bindings",
            SettingsPageId::KeystrokeDisplay => "settings.sidebar_keystroke_display",
            SettingsPageId::FinderMenu => "settings.sidebar_finder_menu",
            SettingsPageId::About => "settings.sidebar_about",
        }
    }
}
/// A settings page in sidebar order: its stable identity plus its script-facing name. The position
/// in `SETTINGS_PAGES` *is* the sidebar index, and the identity is what every control array (sidebar
/// buttons, page scroll views, page documents, icons, titles) is keyed by. Putting the identity in
/// the same list as the order is the point: a reorder moves the page, its name, and every control
/// that belongs to it together, so there is no second hand-written order to fall out of step.
///
/// The names are the stable, user- and script-facing spelling (kebab-case). Numeric forms are still
/// accepted as raw indices, but they are positional and will mean something else once a page moves.
pub(crate) struct SettingsPageSpec {
    /// What the page *is*. Control lookup switches on this, never on the number.
    pub(crate) id: SettingsPageId,
    /// Name used by --open-settings=<name>, the view-root names and the debug validation labels.
    pub(crate) name: &'static str,
}

pub(crate) const SETTINGS_PAGES: [SettingsPageSpec; SETTINGS_PAGE_COUNT] = [
    SettingsPageSpec {
        id: SettingsPageId::General,
        name: "general",
    },
    SettingsPageSpec {
        id: SettingsPageId::Switcher,
        name: "switcher",
    },
    SettingsPageSpec {
        id: SettingsPageId::Mouse,
        name: "mouse",
    },
    SettingsPageSpec {
        id: SettingsPageId::Clipboard,
        name: "clipboard",
    },
    SettingsPageSpec {
        id: SettingsPageId::KeyBindings,
        name: "key-bindings",
    },
    SettingsPageSpec {
        id: SettingsPageId::KeystrokeDisplay,
        name: "keystroke-display",
    },
    SettingsPageSpec {
        id: SettingsPageId::FinderMenu,
        name: "finder-menu",
    },
    SettingsPageSpec {
        id: SettingsPageId::About,
        name: "about",
    },
];

/// The page's name in the sidebar's spelling, for the --open-settings parser and scripts.
pub(crate) fn settings_page_name(index: usize) -> &'static str {
    SETTINGS_PAGES[index].name
}

/// The page identity at a sidebar index. The table position is the index, so this is a lookup, not
/// a second ordering.
pub(super) fn settings_page_identity(index: usize) -> SettingsPageId {
    SETTINGS_PAGES[index].id
}

/// Whether two identities are the same page: the const-time counterpart of `PartialEq`, which cannot
/// be used in the const evaluation that derives the page-index constants.
const fn same_page(a: SettingsPageId, b: SettingsPageId) -> bool {
    matches!(
        (a, b),
        (SettingsPageId::General, SettingsPageId::General)
            | (SettingsPageId::Switcher, SettingsPageId::Switcher)
            | (SettingsPageId::Mouse, SettingsPageId::Mouse)
            | (SettingsPageId::Clipboard, SettingsPageId::Clipboard)
            | (SettingsPageId::KeyBindings, SettingsPageId::KeyBindings)
            | (
                SettingsPageId::KeystrokeDisplay,
                SettingsPageId::KeystrokeDisplay
            )
            | (SettingsPageId::FinderMenu, SettingsPageId::FinderMenu)
            | (SettingsPageId::About, SettingsPageId::About)
    )
}

/// The sidebar index of a page, evaluated at const time so the index constants below are DERIVED
/// from `SETTINGS_PAGES` rather than repeated as literals. A table reorder moves them with the page.
const fn page_index_const(id: SettingsPageId) -> usize {
    let mut index = 0;
    while index < SETTINGS_PAGE_COUNT {
        if same_page(SETTINGS_PAGES[index].id, id) {
            return index;
        }
        index += 1;
    }
    panic!("every page identity is in SETTINGS_PAGES")
}
/// The sidebar index a page identity occupies.
pub(super) fn settings_page_index(page: SettingsPageId) -> usize {
    SETTINGS_PAGES
        .iter()
        .position(|spec| spec.id == page)
        .expect("every page identity is in SETTINGS_PAGES")
}

/// Identities in sidebar order.
pub(super) fn settings_page_ids() -> [SettingsPageId; SETTINGS_PAGE_COUNT] {
    std::array::from_fn(settings_page_identity)
}

/// The A2 view-root names, in sidebar order: `sidebar_<i>_<stem>` and `page_<i>_<stem>`. Pure so the
/// anchor spellings `scripts/e2e/*.sh` look up are assertable without a live window. The stem is the
/// switch name with dashes spelled as underscores (`key_bindings`), which is what those scripts were
/// written against; deriving it from `SETTINGS_PAGES` keeps a page move from leaving a stale index
/// behind in a script.
pub(super) fn view_root_names() -> ([String; SETTINGS_PAGE_COUNT], [String; SETTINGS_PAGE_COUNT]) {
    let stem = |index: usize| settings_page_name(index).replace('-', "_");
    let sidebar = std::array::from_fn(|index| format!("sidebar_{index}_{}", stem(index)));
    let page = std::array::from_fn(|index| format!("page_{index}_{}", stem(index)));
    (sidebar, page)
}

/// Which page --open-settings=<spec> selects, or None when the value names no page.
///
/// A bare switch and the boolean truthy spelling (plus the pre-existing 1/true) select About, which is
/// where an update notice lands. A page is named by its table name or alias, or by a raw index; an
/// out-of-range index is refused rather than clamped, because silently opening a different page than
/// the one asked for is a verification trap.
pub(crate) fn parse_settings_page_spec(spec: &str) -> Option<usize> {
    let spec = spec.trim().to_ascii_lowercase();
    match spec.as_str() {
        "" | "1" | "true" | "yes" => return Some(SETTINGS_ABOUT_PAGE_INDEX),
        // Alias kept from the switch's first form, before the table named the pages.
        "bindings" => return Some(SETTINGS_KEY_BINDINGS_PAGE_INDEX),
        _ => {}
    }
    // A name resolves to the position it occupies: the table position is the sidebar index.
    if let Some(index) = SETTINGS_PAGES.iter().position(|page| page.name == spec) {
        return Some(index);
    }
    spec.parse::<usize>()
        .ok()
        .filter(|&page| page < SETTINGS_PAGE_COUNT)
}

// Each constant is DERIVED from `SETTINGS_PAGES` through its identity, so a table reorder moves the
// page and every caller that navigates by name together. The literals this replaces were a second
// copy of the sidebar order that the first review showed could drift from the table unnoticed.
pub(super) const SETTINGS_KEYSTROKE_DISPLAY_PAGE_INDEX: usize =
    page_index_const(SettingsPageId::KeystrokeDisplay);
/// The App Switcher page. The switcher overlay's own footer control opens HERE; the index stays
/// private to this module so the overlay asks for "the switcher settings", not a sidebar position.
pub(super) const SETTINGS_GENERAL_PAGE_INDEX: usize = page_index_const(SettingsPageId::General);
pub(super) const SETTINGS_SWITCHER_PAGE_INDEX: usize = page_index_const(SettingsPageId::Switcher);
/// The clipboard history page. `pub(crate)` because the clipboard module's smoke runner asserts that
/// its header button navigates HERE -- the page is named once, and the assertion cannot drift from it.
pub(crate) const SETTINGS_CLIPBOARD_PAGE_INDEX: usize = page_index_const(SettingsPageId::Clipboard);
/// The Mouse page. Its controls are the only ones that cost real time to fill (they resolve the selected
/// device and its mappings), so they are filled on demand rather than on every settings open.
pub(super) const SETTINGS_MOUSE_PAGE_INDEX: usize = page_index_const(SettingsPageId::Mouse);
/// The merged Key Bindings page (window control + quick actions in one list).
pub(super) const SETTINGS_KEY_BINDINGS_PAGE_INDEX: usize =
    page_index_const(SettingsPageId::KeyBindings);
pub(super) const SETTINGS_ABOUT_PAGE_INDEX: usize = page_index_const(SettingsPageId::About);

/// The Finder Menu page's read-only status line for the extension's System Settings state.
pub(super) fn finder_menu_status_text(enabled: bool) -> String {
    t(if enabled {
        "settings.finder_menu.status_enabled"
    } else {
        "settings.finder_menu.status_disabled"
    })
}

/// The same line, told apart from a plain "disabled" when the automatic restore after an update
/// failed: that is the one case where the user has to act because of something the app did.
pub(super) fn finder_menu_status_text_now(enabled: bool) -> String {
    if !enabled && crate::finder_menu::recovery::restore_failed() {
        return t("settings.finder_menu.status_restore_failed");
    }
    finder_menu_status_text(enabled)
}

/// Recording stage.
#[derive(PartialEq, Clone, Copy, Debug)]
enum RecStage {
    Idle,
    WaitingButton,
    WaitingCombo,
    /// The chord was captured; the recording stays suspended until the keys are released, so the
    /// binding engine does not act on a chord the user is still holding.
    WaitingRelease,
    Completing,
}

/// Recording stage (read/written on the main thread; the recording thread advances it via
/// performSelectorOnMainThread).
static REC_STAGE: Mutex<RecStage> = Mutex::new(RecStage::Idle);
/// The button number captured while recording (mouseEventButtonNumber, >= 2).
static REC_BUTTON: Mutex<u32> = Mutex::new(0);
/// The shortcut description captured while recording (written by the recording thread on
/// completion, read by the main-thread callback).
static REC_DESC: Mutex<String> = Mutex::new(String::new());
/// Completed combo payload handed off to the main-thread selector callback.
static REC_COMPLETED_DESC: Mutex<String> = Mutex::new(String::new());
/// Whether the chord just captured is a key with no config spelling (the top row in its media mode
/// reports an Apple private keycode). Written by the recording thread at capture, read by the
/// completion callback, and reset with the rest of the session state: such a key produces no trigger
/// text at all, and every surface reports it as unbindable instead of validating an invented token.
static REC_UNNAMED: AtomicBool = AtomicBool::new(false);
/// The selected device's in-edit mappings (in-memory until OK; rebuilt from config when the
/// device changes).
static MAPPING_EDITS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Mapping-row control address -> the physical button number it acts on, for the same reason the key
/// bindings page keeps `ROW_CONTROLS`: a `SettingsButton`'s `tag` selects its hover/normal palette
/// (AGENTS.md), so the button number must never travel in `setTag:`. Writing it there both froze the
/// row buttons' hover colour and left the handlers reading a value the component owns; the registry
/// is cleared in the same pass that tears the rows down, so a click can never reach a freed address.
static MAPPING_ROW_CONTROLS: LazyLock<Mutex<HashMap<usize, u32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Map a mapping row's control to the button number it edits/deletes.
fn register_mapping_control(view: *mut AnyObject, button: u32) {
    if view.is_null() {
        return;
    }
    if let Ok(mut controls) = MAPPING_ROW_CONTROLS.lock() {
        controls.insert(view as usize, button);
    }
}

/// The button number a mapping-row control belongs to, or `None` when the sender is not one of ours.
pub(super) fn mapping_control_button(sender: *mut AnyObject) -> Option<u32> {
    MAPPING_ROW_CONTROLS
        .lock()
        .ok()
        .and_then(|controls| controls.get(&(sender as usize)).copied())
}
/// The recording tap thread handle (short-lived; replaced on each recording session).
static RECORD_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
/// The recording thread's RunLoop (stopped on completion/cancel).
/// Send wrapper: raw pointers in statics need Send+Sync (same pattern as RunLoopMutex in
/// mouse/event_tap.rs).
struct RecRunLoopMutex(Mutex<Option<crate::event_tap::CFRunLoopRef>>);
unsafe impl Send for RecRunLoopMutex {}
unsafe impl Sync for RecRunLoopMutex {}
static REC_RUNLOOP: LazyLock<RecRunLoopMutex> = LazyLock::new(|| RecRunLoopMutex(Mutex::new(None)));

/// Modifiers accumulated live while recording (updated on flagsChanged during
/// waiting-for-combo).
static REC_MODS: Mutex<u32> = Mutex::new(0);
/// The main key of the chord just captured, and whether it has been released since.
static REC_CAPTURED_KEY: Mutex<Option<u16>> = Mutex::new(None);
static REC_CAPTURED_KEY_UP: AtomicBool = AtomicBool::new(false);
/// Recording mode: the panel records the side button (trigger) or the combo (Key Press).
#[derive(PartialEq, Clone, Copy, Debug)]
enum RecMode {
    /// No recording in flight. Also what a finished recording returns to, so a later permission
    /// revocation cannot mistake a completed session for an active one.
    Idle,
    PanelTrigger,
    PanelCombo,
    ClipboardShortcut,
    /// Recording a chord for a new key binding on the Key Bindings page.
    KeyBinding,
}

/// The current recording mode (main-thread; handle_recording_finished finishes per mode).
static REC_MODE: Mutex<RecMode> = Mutex::new(RecMode::Idle);

/// The action a `RecMode::KeyBinding` recording is binding a chord to, held from the moment the
/// add button is pressed until the recording finishes or is cancelled.
static REC_PENDING_ACTION: Mutex<Option<crate::key_bindings::Action>> = Mutex::new(None);

/// The button number being edited (while the panel is open; None for a new mapping until
/// the side button is recorded).
static EDIT_BUTTON: Mutex<Option<u32>> = Mutex::new(None);
/// The panel's action-type popup selection.
static EDIT_ACTION_IDX: Mutex<isize> = Mutex::new(0);
/// The combo recorded in the panel (Key Press action; empty = not recorded).
static EDIT_COMBO: Mutex<String> = Mutex::new(String::new());
/// The panel window and its controls.
static EDIT_PANEL: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static EDIT_PANEL_BTN_LABEL: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static EDIT_PANEL_ACTION: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static EDIT_PANEL_COMBO_BTN: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static EDIT_PANEL_COMBO_LABEL: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };
static EDIT_PANEL_OK: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The window dim layer while the panel is open (a translucent gray overlay that dims the
/// settings window, modal-style).
static EDIT_DIM: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };

/// Recording-cancel flag: set on cancel. It bails the tap-creation retry loop early (the
/// recording thread may still be sleeping through permission-retries, where CFRunLoopStop
/// is a no-op -- without this flag the tap would linger and keep swallowing keys) and lets
/// the callback defensively pass everything through once idle.
static REC_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The recording tap's CFMachPort: disabled immediately on cancel/finish via
/// CGEventTapEnable(false) -- CFRunLoopStop is asynchronous and the tap keeps dispatching
/// until the loop actually exits, which could swallow the first keystroke after cancel.
struct RecTapMutex(Mutex<Option<crate::event_tap::CFMachPortRef>>);
unsafe impl Send for RecTapMutex {}
unsafe impl Sync for RecTapMutex {}
static REC_TAP: LazyLock<RecTapMutex> = LazyLock::new(|| RecTapMutex(Mutex::new(None)));

// Holds pointers to the settings window's controls (non-modal, reused, hidden not destroyed).
pub(super) struct SettingsUi {
    window: *mut AnyObject,
    /// The sidebar's search field, the detail pane's results view, and the query behind them (see
    /// the search section of `settings::sidebar`).
    pub(in crate::settings) settings_search: sidebar::SettingsSearch,
    sidebar_highlight: *mut AnyObject, // NSView: selected-row highlight background (layer-backed)
    /// The sidebar buttons, indexed by sidebar order. Keyed by POSITION and filled and read
    /// through the page identity (`settings_page_index`), so adding or moving a page changes no field
    /// list in two places -- the mapping exists once.
    sidebar_buttons: [*mut AnyObject; SETTINGS_PAGE_COUNT],
    /// The page scroll views, indexed the same way as `sidebar_buttons`.
    page_views: [*mut AnyObject; SETTINGS_PAGE_COUNT],
    about_subtitle: *mut AnyObject, // About-page version label
    theme: *mut AnyObject,          // NSPopUpButton: auto / light / dark
    panel_material: *mut AnyObject, // NSPopUpButton: liquid-glass / frost / opaque
    glass_style: *mut AnyObject,    // NSPopUpButton: regular / clear (liquid glass only)
    corner_radius: *mut AnyObject,  // NSTextField
    modifier: *mut AnyObject,       // NSPopUpButton: option / command
    locale: *mut AnyObject,         // NSPopUpButton: auto / en / zh-Hans / zh-Hant
    show_minimized: *mut AnyObject, // show minimized windows
    show_hidden_app_windows: *mut AnyObject, // show hidden-app windows
    show_other_desktops: *mut AnyObject, // show windows on other macOS desktops (Spaces)
    thumbnails_enabled: *mut AnyObject, // window display mode
    focused_thumbnail_prewarm: *mut AnyObject, // focused thumbnail prewarm
    show_app_name_in_cards: *mut AnyObject, // app name in card titles
    thumbnail_size: *mut AnyObject, // NSPopUpButton: auto / 120% / ... / 75%
    card_text_size: *mut AnyObject, // card text size
    card_text_size_value_label: *mut AnyObject, // card text-size value
    status_bar_text_size: *mut AnyObject, // footer text size
    status_bar_text_size_value_label: *mut AnyObject, // footer text-size value
    show_footer_title: *mut AnyObject, // window title in the overlay's footer
    windows_enabled: *mut AnyObject, // app-switcher master switch
    overlay_position: *mut AnyObject, // overlay position (follow active window / main screen)
    activation_mode: *mut AnyObject, // activation mode (hover / click)
    // In-overlay action keys (Cmd+W / M / H / Q on the highlighted card); all default off.
    panel_close_enabled: *mut AnyObject,
    panel_minimize_enabled: *mut AnyObject,
    panel_hide_enabled: *mut AnyObject,
    panel_quit_enabled: *mut AnyObject,
    keystroke_display_enabled: *mut AnyObject,
    keystroke_display_mode: *mut AnyObject,
    keystroke_display_tap_level: *mut AnyObject,
    keystroke_display_position: *mut AnyObject,
    keystroke_display_initial_position: *mut AnyObject,
    keystroke_display_max_extent: *mut AnyObject, // NSSlider: stream cap, percent
    keystroke_display_max_extent_value: *mut AnyObject, // the slider's read-only readout
    log_level: *mut AnyObject, // NSPopUpButton: trace / debug / info / warn / error
    launch_at_login: *mut AnyObject, // launch at login
    hide_menu_bar_icon: *mut AnyObject, // hide the menu-bar status item
    reverse_scroll: *mut AnyObject, // reverse scrolling
    enable_mouse: *mut AnyObject, // enable mouse control
    scroll_mode: *mut AnyObject, // NSPopUpButton: default/line
    line_count: *mut AnyObject, // NSSlider: line count slider
    line_count_label: *mut AnyObject, // the row's label
    line_count_value_label: *mut AnyObject, // slider's current value (read-only)
    disable_pointer_accel: *mut AnyObject, // disable pointer acceleration
    pointer_accel_slider: *mut AnyObject, // NSSlider: pointer acceleration, 0..=40
    pointer_accel_label: *mut AnyObject, // the row's label
    pointer_accel_value_label: *mut AnyObject, // slider's current value (read-only)
    smooth_scrolling_preset: *mut AnyObject,
    smooth_scrolling_response: *mut AnyObject,
    smooth_scrolling_response_value: *mut AnyObject,
    smooth_scrolling_speed: *mut AnyObject,
    smooth_scrolling_speed_value: *mut AnyObject,
    smooth_scrolling_acceleration: *mut AnyObject,
    smooth_scrolling_acceleration_value: *mut AnyObject,
    smooth_scrolling_inertia: *mut AnyObject,
    smooth_scrolling_inertia_value: *mut AnyObject,
    /// The Mouse page row the binding table occupies (it grows with the number of bindings).
    mapping_layout_row: usize,
    /// The About page row the inline update host occupies (its height follows the flow).
    update_host_row: usize,
    /// The About page row the check-updates action band occupies. The inline update flow collapses it
    /// to zero height so the panel can take over the same position.
    update_band_row: usize,
    mapping_scroll: *mut AnyObject,     // the bindings scroll view
    mapping_doc: *mut AnyObject,        // document view
    mapping_card: *mut AnyObject,       // the mappings outer card
    mapping_panel: *mut AnyObject,      // the nested rounded table panel
    mapping_rows: Vec<MappingRow>,      // live binding rows
    clipboard_enabled: *mut AnyObject,  // enable clipboard history
    clipboard_shortcut: *mut AnyObject, // action button that records the global history chord
    clipboard_shortcut_error: *mut AnyObject, // inline validation error under the capture button
    clipboard_shortcut_row_height: f64,
    clipboard_pin_follow_row_height: f64,
    clipboard_row_gap: f64,
    clipboard_clear_on_quit: *mut AnyObject, // clear clipboard history when the app quits
    clipboard_separate_clear_all: *mut AnyObject, // show the opt-in "clear all" header button
    clipboard_move_used_to_top: *mut AnyObject, // move used entries to top
    clipboard_delete_after_paste: *mut AnyObject, // delete entry after paste
    /// The top warning banner's label and its "Open Privacy & Security" button. Which warning the
    /// strip carries is recomputed from live state (a missing permission, or a clipboard storage
    /// failure) rather than fixed at build time, and the button belongs to the permission copy only.
    permission_warning_label: *mut AnyObject,
    permission_warning_button: *mut AnyObject,
    /// The page scroll views' unreserved height. The banner is a sibling of those views, so the page
    /// it covers is shortened by the banner's height and every other page keeps this one.
    page_viewport_h: f64,
    clipboard_clear_system_pasteboard_after_paste: *mut AnyObject, // clear system pasteboard after paste
    clipboard_max_entries: *mut AnyObject,                         // max history entries
    clipboard_auto_expire_days: *mut AnyObject,                    // auto-expire days (0 = never)
    clipboard_auto_expire_days_value_label: *mut AnyObject,        // auto-expire value
    clipboard_image_rows: *mut AnyObject,                          // image row height (1..3 rows)
    clipboard_image_rows_value_label: *mut AnyObject,              // image-rows value
    clipboard_show_source_app: *mut AnyObject,                     // show the source app
    clipboard_pin_follow: *mut AnyObject,                          // selection after pin
    // (follow the pinned entry / keep current position)
    clipboard_picker_placement: *mut AnyObject, // clipboard on the cursor screen
    // (follow the cursor / screen center)
    clipboard_picker_placement_row_height: f64,
    clipboard_paused: *mut AnyObject, // pause recording (persisted, unlike the panel pin)
    /// Per-row "remove" buttons of the excluded-applications list. The SENDER POINTER identifies
    /// which entry to drop: a button's tag selects its hover/normal palette, so it must never carry
    /// an action id.
    clipboard_excluded_remove: Vec<*mut AnyObject>,
    /// The "add a running app" popup and the candidates it currently lists, so a selection index
    /// maps to an identifier without re-enumerating running apps.
    clipboard_excluded_add: *mut AnyObject,
    clipboard_excluded_candidates: Vec<(String, String)>,
    clipboard_excluded_restore: *mut AnyObject,
    // Key-bindings page. The rows themselves are dynamic (added, deleted, toggled), so only the
    // fixed furniture lives here: the master switch, the flipped container the rows are rendered
    // into, and its empty-state hint. Adding happens in the footer's panel, not in page controls.
    key_bindings_enabled: *mut AnyObject,
    key_bindings_doc: *mut AnyObject,
    key_bindings_empty: *mut AnyObject,
    /// The footer band: one container view holding the full-width "Add binding" button, so a rebuild
    /// removes and redraws the whole band instead of accumulating views.
    key_bindings_footer: *mut AnyObject,
    /// The footer's single "Add binding" button, so the master switch can freeze it.
    key_bindings_footer_button: *mut AnyObject,
    /// The canvas row whose height the binding list owns (it grows and shrinks as rows come and go).
    key_bindings_row: usize,
    key_binding_rows: Vec<bindings::BindingRow>,
    /// The `(action, trigger)` pairs the rendered rows were built from: a config change with the
    /// same shape only needs its switch states synced, not a rebuild that would destroy the very
    /// control whose action is running.
    key_binding_items_rendered: Vec<(String, String)>,
    add_mapping_button: *mut AnyObject, // add-mapping button
    mapping_enabled: *mut AnyObject,    // mappings master switch (per-device)
    mapping_empty: *mut AnyObject,      // empty-state hint (in-card)
    device_indicator: *mut AnyObject,   // device indicator (opens picker)
    device_info_caption: *mut AnyObject,
    restore_defaults: RestoreDefaultsControl, // restore-defaults control
    // One "Restore Page Defaults" control per page (embedded at the end of each page's
    // scrolling document).
    page_restores: [RestoreDefaultsControl; SETTINGS_PAGE_COUNT],
    /// One layout owner per page (index order matches `page_restores`): rows, cards, the page
    /// title, the restore control and the document height all follow from its row list.
    page_canvases: [PageCanvas; SETTINGS_PAGE_COUNT],
    permission_warning_view: *mut AnyObject, // permission-warning banner container
    update_auto_check: *mut AnyObject,       // Sparkle auto-check switch
    update_auto_download: *mut AnyObject,    // Sparkle auto-download switch
    update_check_button: *mut AnyObject,     // check-updates button
    update_host: *mut AnyObject,             // In-about update flow host container
    update_host_window: *mut AnyObject,      // host's settings window
    update_card: *mut AnyObject,             // Updates card (grows when expanded)
    update_divider: *mut AnyObject,          // divider between the update rows and the action band
    update_card_expanded: bool,              // whether expanded for a flow
    accessibility_permission_status: *mut AnyObject,
    accessibility_permission_button: *mut AnyObject,
    screen_recording_permission_status: *mut AnyObject,
    /// The About page's keychain-access row: its status label and its "grant access" button.
    keychain_access_status: *mut AnyObject,
    keychain_access_button: *mut AnyObject,
    // Finder Menu page. The status line is read-only; the two switch lists mirror the config's
    // vec-backed fields (file types and terminal apps) in their identity-table order.
    finder_menu_status: *mut AnyObject,
    finder_menu_enabled: *mut AnyObject,
    finder_menu_layout: *mut AnyObject,
    finder_menu_new_file_switches: Vec<*mut AnyObject>,
    finder_menu_terminal_switches: Vec<*mut AnyObject>,
    finder_menu_copy_path: *mut AnyObject,
}

impl SettingsUi {
    /// The sidebar button for a page, or null when the window is not built yet.
    pub(super) fn sidebar_button(&self, page: SettingsPageId) -> *mut AnyObject {
        self.sidebar_buttons[settings_page_index(page)]
    }

    /// The page scroll view for a page, or null when the page has not been built yet.
    pub(super) fn page_view(&self, page: SettingsPageId) -> *mut AnyObject {
        self.page_views[settings_page_index(page)]
    }
}

/// One button-mapping row (read-only display):
/// - label: the button name
/// - desc_label: the action description (system-action name / None text; keycaps for Key
///   Press)
/// - action_icon: the SF Symbol for non-keyboard actions
/// - edit: the edit button (tag = button number; opens the edit panel)
/// - delete: the delete button (tag = button number)
/// - caps: keycap pills (shown for Key Press)
struct MappingRow {
    label: *mut AnyObject,
    desc_label: *mut AnyObject,
    action_icon: *mut AnyObject,
    edit: *mut AnyObject,
    delete: *mut AnyObject,
    separator: *mut AnyObject,
    caps: Vec<*mut AnyObject>,
}
unsafe impl Send for MappingRow {}

impl MappingRow {
    /// Every view of this row that takes part in the availability / disabled-hint registry
    /// (action_icon is null on Key Press rows). Registration and unregistration share this list
    /// so the two sides cannot drift apart and leave a dangling registry entry behind
    /// (see SettingsRow::forget).
    fn interactive_views(&self) -> impl Iterator<Item = *mut AnyObject> + '_ {
        [
            self.label,
            self.desc_label,
            self.action_icon,
            self.edit,
            self.delete,
        ]
        .into_iter()
        .chain(self.caps.iter().copied())
    }
}

/// Mapping-row height (independent of the global row_h; shared by the card height in build
/// and by render).
const MAPPING_HEADER_H: f64 = 32.0;
const MAPPING_ROW_H: f64 = 40.0;

// Layout constants for the nested mapping table (HTML `.mapping-table`).
const MAPPING_PANEL_X: f64 = 8.0; // sub-table horizontal inset
const MAPPING_PANEL_TOP: f64 = 8.0; // sub-table top padding
const MAPPING_CELL_X: f64 = 12.0; // row content left padding
const MAPPING_ACTION_TOP: f64 = 12.0; // gap above the add-mapping button
const MAPPING_ACTION_H: f64 = 32.0; // add-mapping button height
const MAPPING_CARD_PAD_BOT: f64 = 8.0; // card bottom padding

/// Gap from a section header to the top edge of its grouped card (the mock's
/// `.section-title` margin-bottom; measured 7.99pt).
const SETTINGS_SECTION_CARD_GAP: f64 = 8.0;
/// Horizontal padding of the settings detail content inside the pane (`.settings-content
/// { padding: 0 var(--sp-7) }`). The page layout and the search results' page-style chunks both
/// use it, so their cards share the same columns.
pub(super) const SETTINGS_DETAIL_INSET: f64 = 28.0;

/// Gap between the previous card's bottom edge and the next section header (the mock's
/// `.settings-section + .settings-section` margin-top; measured 19.99pt).
const SETTINGS_SECTION_HEADER_GAP: f64 = 20.0;

/// Optical trailing inset for row controls, matching the text's visible leading inset.
const SETTINGS_CONTROL_TRAILING_INSET: f64 = 16.0;
/// Clear space between a localized row label and its control column.
pub(crate) const SETTINGS_CONTROL_LABEL_GAP: f64 = 16.0;
/// Vertical offset that centers a 32pt control inside a described (two-line) row. Every described
/// row in the settings pages positions its control this way; the value is a token so a call site
/// does not carry its own copy of the layout number.
pub(crate) const SETTINGS_DESCRIBED_CONTROL_OFFSET: f64 = 10.0;

/// The action-type popup items; index maps 1:1 to semantics (shared by render and the
/// change handler).
const MAPPING_ACTION_KEYS: [&str; 8] = [
    "settings.mapping_action_default",
    "settings.mapping_action_none",
    "settings.mapping_action_key",
    "settings.mapping_action_missioncontrol",
    "settings.mapping_action_launchpad",
    "settings.mapping_action_showdesktop",
    "settings.mapping_action_appexpose",
    "settings.mapping_action_switcher",
];

/// SF Symbols shared by the action popup and mapping rows; indices match `MAPPING_ACTION_KEYS`.
const MAPPING_ACTION_SYMBOLS: [&str; 8] = [
    "dot.circle",
    "slash.circle",
    "keyboard",
    "square.grid.2x2",
    "square.grid.3x3",
    "macwindow",
    "rectangle.on.rectangle",
    "arrow.left.arrow.right",
];
thread_local! {
    static SETTINGS_UI: RefCell<Option<SettingsUi>> = const { RefCell::new(None) };
}

pub(super) fn with_settings_ui<R>(f: impl FnOnce(&mut Option<SettingsUi>) -> R) -> R {
    #[cfg(not(test))]
    crate::debug_assert_main_thread();
    SETTINGS_UI.with(|ui| f(&mut ui.borrow_mut()))
}

/// Refresh just the clipboard shortcut recorder after a config reload without rebuilding Settings.
pub(crate) fn refresh_clipboard_shortcut_control_from_config() {
    dispatch::refresh_clipboard_shortcut_record_control();
}

pub(crate) fn refresh_clipboard_separate_clear_all_switch_from_config() {
    dispatch::refresh_clipboard_separate_clear_all_switch_from_config();
}

/// Re-read the pause switch after a change that did not come from the switch itself (the menu item,
/// a config reload, or a resume that failed and forced the persisted intent back to paused).
pub(crate) fn refresh_clipboard_pause_switch_from_config() {
    dispatch::refresh_clipboard_pause_switch_from_config();
}

/// Rebuild the settings content on the next run-loop turn.
///
/// The excluded-applications card is laid out from the current list, so adding or removing an entry
/// has to rebuild the page. It must NOT be rebuilt inside the button's own action: the rebuild
/// releases the button AppKit is currently dispatching to.
pub(crate) fn rebuild_settings_content_deferred() {
    // A plain request keeps any pending scroll record: whichever rebuild ends up running restores
    // the position the user was looking at.
    queue_settings_content_rebuild();
}

/// Take this run-loop turn's single rebuild slot. False means a rebuild is already queued and the
/// new request must fold into it.
///
/// This is the arbitration the coalescing depends on, kept separate from the AppKit call so it can be
/// asserted: without it, two requests queue two callbacks, the first consumes the scroll record and
/// the second runs a plain rebuild that jumps the page back to the top.
fn claim_rebuild_slot() -> bool {
    !REBUILD_QUEUED.swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// Open the next run-loop turn for a new request.
fn release_rebuild_slot() {
    REBUILD_QUEUED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Queue one rebuild for the next run-loop turn, at most once per turn.
fn queue_settings_content_rebuild() {
    if !claim_rebuild_slot() {
        return;
    }
    unsafe {
        let target = (*crate::MENU_TARGET.lock().unwrap()).map(|t| t.0);
        let Some(target) = target else {
            release_rebuild_slot();
            return;
        };
        let _: () = msg_send![
            target,
            performSelectorOnMainThread: sel!(handleRebuildSettingsContent:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// Where the page was scrolled when a rebuild was requested. Captured BEFORE the page is torn down
/// (the new page starts at the top) and consumed by the deferred handler.
///
/// The request is COALESCED with `REBUILD_PENDING`: several edits in one run-loop turn queue ONE
/// rebuild. Without that, two queued callbacks would let the first consume the record and restore
/// the position, and the second fall through to a plain rebuild that jumps back to the top.
static PENDING_PAGE_SCROLL: std::sync::Mutex<Option<(SettingsPageId, f64)>> =
    std::sync::Mutex::new(None);

/// Whether a rebuild is already queued for this run-loop turn.
static REBUILD_QUEUED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Rebuild the content while keeping the clipboard page where the user left it: removing or
/// restoring a row must not throw a long page back to its top.
pub(crate) fn rebuild_settings_content_deferred_keeping_scroll(page: SettingsPageId) {
    // Captured synchronously, before the queued rebuild tears the page down.
    let distance = unsafe { window::page_scroll_distance_from_top(page) };
    if let Ok(mut pending) = PENDING_PAGE_SCROLL.lock() {
        *pending = distance.map(|d| (page, d));
    }
    queue_settings_content_rebuild();
}

/// Open the next run-loop turn for a new rebuild request and hand this one's scroll record to the
/// handler. Clearing the queued flag FIRST is what makes a rebuild requested during this handler
/// queue a fresh one instead of being swallowed.
pub(crate) fn take_rebuild_request() -> Option<(SettingsPageId, f64)> {
    release_rebuild_slot();
    PENDING_PAGE_SCROLL
        .lock()
        .ok()
        .and_then(|mut pending| pending.take())
}

/// Re-read the General appearance controls after a non-rebuilding appearance change (a panel-material
/// change): the material and glass-style popups plus the dependent row's visibility.
pub(crate) fn refresh_appearance_controls_from_config() {
    dispatch::refresh_appearance_controls_from_config();
}

pub(crate) fn set_clipboard_shortcut_recording_ui(recording: bool, error: Option<&str>) {
    dispatch::set_clipboard_shortcut_recording_ui(recording, error);
}

pub(crate) fn apply_recorded_clipboard_shortcut(shortcut: &str) {
    dispatch::apply_recorded_clipboard_shortcut(shortcut);
}

/// Whether the settings UI is already borrowed on this thread. An Objective-C notification callback
/// can be delivered *synchronously from inside* `with_settings_ui` (making the settings window key
/// or activating the app posts the window/app notifications right there), and any callback that
/// reads the settings UI would then re-enter `borrow_mut` -- which panics, and a panic inside an
/// `extern "C"` callback aborts the app (that is how the guide's "Open App Settings" crashed on
/// 2026-09-28). Such callbacks ask this first and defer to the next runloop turn instead.
pub(super) fn settings_ui_borrowed() -> bool {
    SETTINGS_UI.with(|ui| ui.try_borrow_mut().is_err())
}
/// Whether a background update check found a version the user has not opened yet.
static UPDATE_AVAILABLE: AtomicBool = AtomicBool::new(false);
/// Update the About-tab marker and retain the state until the settings window is created.
pub(crate) fn set_update_available(available: bool) {
    UPDATE_AVAILABLE.store(available, Ordering::SeqCst);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                set_about_sidebar_indicator(ui);
            }
        });
    }
}

/// Refresh the About-tab marker and permission row after a relaunch becomes necessary.
pub(crate) fn refresh_permission_restart_state() {
    crate::debug_assert_main_thread();
    unsafe {
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                set_about_sidebar_indicator(ui);
            }
        });
    }
    refresh_permission_ui_if_visible();
}

unsafe fn set_about_sidebar_indicator(ui: &SettingsUi) {
    widgets::set_sidebar_update_indicator(
        ui.sidebar_button(SettingsPageId::About),
        UPDATE_AVAILABLE.load(Ordering::SeqCst) || crate::restart::restart_required(),
    );
}

/// A visible settings window cannot be rebuilt during an appearance notification without
/// discarding unsaved edits; rebuild it immediately after the user closes it instead.
static SYSTEM_APPEARANCE_REBUILD_PENDING: AtomicBool = AtomicBool::new(false);

/// Hidden About-header easter-egg click state; consecutive clicks must happen within a short window.
static ABOUT_HEADER_CLICKS: Mutex<(u8, Option<Instant>)> = Mutex::new((0, None));
const ABOUT_HEADER_CLICK_WINDOW: Duration = Duration::from_secs(1);

/// The currently-selected device scope on the Mouse page. None = "All Mice";
/// Some((vid,pid)) = a specific mouse.
static SELECTED_DEVICE: Mutex<Option<Option<crate::mouse::device::DeviceKey>>> = Mutex::new(None);

/// "Auto switch to active device" toggle (in-memory, not persisted to config).
#[allow(dead_code)]
static AUTO_SWITCH_DEVICE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Run the Finder Menu page smoke (`--smoke-finder-menu`).
pub(crate) fn finder_menu_page_smoke_runner() -> bool {
    finder_menu_smoke::run()
}

pub(crate) mod bindings;
pub(crate) mod components;
mod dispatch;
mod finder_menu_smoke;
pub(crate) mod mapping;
mod page_builder;
pub(crate) mod page_canvas;
pub(crate) mod portability;
pub(crate) mod restore;
pub(crate) mod select;
mod sidebar;
pub(crate) mod tooltip;
pub(crate) mod widgets;
mod window;

use components::{
    row_action_button, RestoreDefaultsControl, SettingsButton, SettingsButtonRole, SettingsCard,
    SettingsControl, SettingsLayout, SettingsMappingActionIcon, SettingsPage, SettingsPageHeader,
    SettingsRow, SettingsSection, SettingsSelect, SettingsSidebar, ROW_ACTION_BTN_H,
    ROW_ACTION_BTN_W,
};
use dispatch::*;
use mapping::*;
pub(crate) use mapping::{
    cancel_recording_from_main, handle_add_mapping, handle_clipboard_shortcut_record,
    handle_delete_mapping, handle_mapping_cancel, handle_mapping_confirm, handle_mapping_edit,
    handle_mapping_enabled_changed, handle_panel_action_changed, handle_panel_record_combo,
    handle_panel_record_trigger, handle_recording_cancelled, handle_recording_finished,
};
use page_canvas::*;
pub(super) use restore::{
    collapse_restore_confirmations, collapse_restore_confirmations_on_external_click,
};
pub(crate) use restore::{
    confirm_alert, handle_page_restore_defaults, handle_page_restore_defaults_cancel,
    handle_page_restore_defaults_confirm, handle_restore_defaults, handle_restore_defaults_cancel,
    handle_restore_defaults_confirm,
};
use widgets::*;
use window::*;
// Entry points exposed to the rest of the crate (implemented in child modules; main.rs
// consumes them through use settings::*).
pub(crate) use dispatch::{
    end_max_extent_preview, handle_clipboard_enabled_toggle, handle_clipboard_excluded_add,
    handle_clipboard_excluded_remove, handle_clipboard_excluded_restore, handle_device_changed,
    handle_enable_mouse_toggle, handle_export_logs, handle_export_settings,
    handle_export_settings_ready, handle_import_settings, handle_import_settings_ready,
    handle_key_bindings_toggle, handle_pause_recording_toggle, handle_rebuild_settings_content,
    handle_settings_search_began_editing, handle_settings_search_changed,
    handle_settings_search_clear, handle_settings_search_ended_editing,
    handle_windows_enabled_toggle, left_mouse_button_is_down, on_control_changed,
    on_control_text_did_change, on_control_text_did_end_editing, on_sidebar_select,
    refresh_clipboard_unavailable_notice, refresh_device_popup_if_open,
    refresh_service_controls_from_config,
    refresh_switcher_keystroke_and_mouse_controls_from_config,
};
pub(crate) use window::{
    close_settings_from_switcher, e2e_presented_page, invalidate_settings_window,
    refresh_finder_menu_status_if_visible, refresh_permission_ui_if_visible,
    refresh_system_appearance, reopen_for_app_activation, settings_collapsible_row_smoke_runner,
    settings_layout_smoke_runner, settings_mouse_profile_callback_smoke_runner,
    settings_state_sync_smoke_runner, settings_vertical_zoom_smoke_runner,
    settings_window_is_visible,
};

fn parse_f64(s: &str) -> Result<f64, ()> {
    s.trim().parse::<f64>().map_err(|_| ())
}
fn parse_usize(s: &str) -> Result<usize, ()> {
    s.trim().parse::<usize>().map_err(|_| ())
}

fn text_size_slider_value(value: f64) -> i64 {
    if value.is_finite() {
        (value.round() as i64).clamp(TEXT_SIZE_MIN, TEXT_SIZE_MAX)
    } else {
        TEXT_SIZE_DEFAULT
    }
}

/// The footer-title slider's own value: its floor is 0 ("no footer at all"), not `TEXT_SIZE_MIN`.
/// Sharing `text_size_slider_value` here would clamp a saved 0 back to 13 on the next open, so the
/// panel would draw no footer while the settings page claimed 13.
///
/// The slider domain is stop indices, so this maps a stored point size onto its stop: 0 stays 0,
/// a saved 1..12 (from before the discrete stops existed, or hand-edited) snaps to the nearest
/// legal stop -- 0 below the midpoint, 13 above it.
fn status_bar_size_slider_value(value: f64) -> i64 {
    // Share the renderer's normalization (`theme::normalize_status_bar_size`) so a hand-edited or
    // fractional config shows the same stop the overlay actually draws.
    if !value.is_finite() {
        return STATUS_BAR_SIZE_DEFAULT_INDEX;
    }
    let size = crate::theme::normalize_status_bar_size(value);
    if size <= 0.0 {
        0
    } else {
        (size as i64 - TEXT_SIZE_MIN + 1).clamp(1, STATUS_BAR_SIZE_STOPS - 1)
    }
}

/// The point size a footer-title slider stop means, for the readout and the config value.
/// Stop 0 is the hidden band; stops 1..=8 are 13..=20pt.
fn status_bar_size_for_index(index: i64) -> i64 {
    if index <= 0 {
        STATUS_BAR_SIZE_MIN
    } else {
        (TEXT_SIZE_MIN + index - 1).clamp(TEXT_SIZE_MIN, TEXT_SIZE_MAX)
    }
}

use crate::config::{DeviceMatcher, MouseProfile};

/// The currently-selected device scope on the Mouse page (reads SELECTED_DEVICE; defaults to
/// None = "All Mice" when uninitialized).
fn current_selected_device() -> Option<crate::mouse::device::DeviceKey> {
    // SELECTED_DEVICE: outer Option = "initialized?"; inner None = "All Mice".
    SELECTED_DEVICE.lock().unwrap().unwrap_or(None) // uninitialized -> None (All Mice)
}

/// Find the index of the profile matching (VID,PID) in CONFIG. None = find "All Mice".
fn find_profile_index(
    cfg: &Config,
    device: Option<crate::mouse::device::DeviceKey>,
) -> Option<usize> {
    // The matching rule is shared with runtime resolution (mouse::resolve::matches): the
    // virtual-pointer profile matches by "injected", ordinary ones by VID/PID, the wildcard by
    // neither.
    cfg.mouse
        .profiles
        .iter()
        .position(|p| crate::mouse::resolve::matches(p, device))
}

/// The selected device's own profile index, creating one for the current device when absent
/// (shared by the mouse-page field writes, mapping commits and restore-defaults). Those three
/// call sites each carried a copy of the "create by (VID,PID) when missing" block; with the
/// virtual-pointer profile that would have meant the same edit three times and an easy miss.
/// Matching still goes through resolve::matches (the same rule runtime resolution uses), and
/// profile creation lives here only.
fn selected_device_profile_index(cfg: &mut Config) -> usize {
    let device = current_selected_device();
    if let Some(idx) = find_profile_index(cfg, device) {
        return idx;
    }
    let matcher = match device {
        // Virtual pointer: no VID/PID; the device_injected flag is what identifies it.
        Some(key) if key == crate::mouse::device::VIRTUAL_DEVICE_KEY => DeviceMatcher {
            injected: Some(true),
            ..Default::default()
        },
        Some((vid, pid)) => DeviceMatcher {
            vendor_id: Some(vid),
            product_id: Some(pid),
            ..Default::default()
        },
        None => DeviceMatcher::default(),
    };
    cfg.mouse.profiles.push(MouseProfile {
        device: matcher,
        ..Default::default()
    });
    cfg.mouse.profiles.len() - 1
}

/// Read the effective value for the currently-selected device (merging the "All Mice" profile +
/// the device profile), resolved from a given Config. Used to show the effective config in the
/// UI, and for the restore-defaults preview (passing Config::default()).
fn resolve_selected_from(cfg: &Config) -> crate::mouse::resolve::ResolvedMouse {
    let dev = current_selected_device();
    crate::mouse::resolve::resolve_from_config(cfg, dev)
}

pub(crate) extern "C" fn on_settings_open(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    crate::callback_guard::void("on_settings_open", || {
        show_settings();
    });
}

/// Build the settings switch component for reuse by other native panels and bind the caller's
/// target/action.
pub(crate) unsafe fn make_shared_switch(
    right_x: f64,
    y: f64,
    h: f64,
    checked: bool,
    target: *mut AnyObject,
    action: Sel,
) -> *mut AnyObject {
    let switch = SettingsControl::switch(right_x, y, h, checked);
    if !switch.is_null() {
        let _: () = msg_send![switch, setTarget: target];
        let _: () = msg_send![switch, setAction: action];
    }
    switch
}

/// Manual update check: hand the request to Sparkle's standard update UI.
pub(crate) extern "C" fn handle_check_for_updates(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_check_for_updates", || {
        start_inline_update_check();
    });
}

/// Shared entry of the inline check: used by both the About-page button and the update
/// notification click.
fn start_inline_update_check() {
    // Enter the inline checking phase: switch the button and disable it, arming a timeout guard so
    // the button recovers even if Sparkle never calls back.
    crate::updater::begin_inline_check();
    if !crate::updater::check_for_updates() {
        // When Sparkle is unavailable, offer a retry and alert instead of leaving the button
        // stuck on "Checking…".
        crate::updater::set_check_button_status(&t("settings.btn_retry_update_check"), true);
        show_alert(
            &t("settings.update_unavailable_title"),
            &t("settings.update_unavailable_message"),
        );
    }
}

/// The settings window pointer, or null when it has not been created (or was invalidated).
///
/// Callers must not cache it: `invalidate_settings_window` releases the window, so a stale pointer
/// would dangle. Used by the scroller-notification filter to ignore every *other* window of ours
/// (the switcher panel and friends) that also becomes key.
pub(crate) fn settings_window_ptr() -> *mut AnyObject {
    with_settings_ui(|ui| {
        ui.as_ref()
            .map(|ui| ui.window)
            .unwrap_or(std::ptr::null_mut())
    })
}

/// Landing point of the update-notification click: open the settings window, jump to the
/// About page, and start an inline check. Once the settings window exists, its host view
/// is registered, so the "update found" UI renders INLINE in the About page (see
/// updater::render_target) instead of a standalone window.
/// After a scroller-style change: measure this page's scroller footprint and rebuild the content
/// when it differs from the value the layout was built with, so the layout follows the visible
/// width (see the A/B notes in src/scroller.rs).
/// This settings page's measured scroller footprint (0 = overlay); -1 when there is no window.
pub(crate) fn page_reserved_now() -> f64 {
    let scroll = with_settings_ui(|ui| {
        ui.as_ref()
            .map(|ui| ui.page_view(SettingsPageId::About))
            .unwrap_or(std::ptr::null_mut())
    });
    if scroll.is_null() {
        return -1.0;
    }
    unsafe { crate::scroller::reserved_width(scroll) }
}

pub(crate) fn resync_page_layout_for_scroller() {
    let scroll = with_settings_ui(|ui| {
        ui.as_ref()
            .map(|ui| ui.page_view(SettingsPageId::About))
            .unwrap_or(std::ptr::null_mut())
    });
    if scroll.is_null() {
        return;
    }
    let measured = unsafe { crate::scroller::reserved_width(scroll) };
    if (measured - crate::scroller::reserved()).abs() < 0.5 {
        return;
    }
    crate::scroller::note_reserved(measured);
    unsafe { window::rebuild_settings_content_now() };
}

/// A2 E2E: exposes the view roots whose geometry must be asserted (name → view); `e2e_state`
/// walks them into JSON. Only semantic roots (highlight, the sidebar buttons, the page containers)
/// are listed -- this is not a generic traversal entry point.
pub(crate) fn e2e_view_roots() -> Vec<(String, *mut AnyObject)> {
    let (sidebar_names, page_names) = view_root_names();
    with_settings_ui(|ui| match ui {
        Some(ui) => {
            let mut roots: Vec<(String, *mut AnyObject)> =
                vec![("sidebar_highlight".to_string(), ui.sidebar_highlight)];
            let buttons = window::sidebar_buttons(ui);
            let pages = window::page_scroll_views(ui);
            for (index, view) in buttons.into_iter().enumerate() {
                roots.push((sidebar_names[index].clone(), view));
            }
            for (index, view) in pages.into_iter().enumerate() {
                roots.push((page_names[index].clone(), view));
            }
            roots.retain(|(_, view)| !view.is_null());
            roots
        }
        None => Vec::new(),
    })
}

/// A2 E2E: the selected sidebar page's stable NAME, so a scenario asserts the page it means
/// rather than a sidebar position. The position is derived from SETTINGS_PAGES and moves when the
/// table is reordered; the name does not.
pub(crate) fn e2e_selected_sidebar_name() -> &'static str {
    settings_page_name(e2e_selected_sidebar())
}

/// A2 E2E: the page the settings window was last ordered front on, by name (None if it never was
/// presented). Same reasoning as e2e_selected_sidebar_name: the historic-first-frame semantics are
/// unchanged, only the spelling is stable.
pub(crate) fn e2e_presented_page_name() -> Option<&'static str> {
    e2e_presented_page().map(settings_page_name)
}

/// A2 E2E: the view-root names keyed by page NAME -- (name, sidebar_root, page_root). A scenario
/// addresses a page's root by name and never writes the sidebar position into a script. The
/// strings still carry the index because the app builds them that way; this map is what keeps a
/// reorder from leaving a stale index behind in a scenario.
pub(crate) fn e2e_root_names_by_page() -> Vec<(&'static str, String, String)> {
    let (sidebar, page) = view_root_names();
    (0..SETTINGS_PAGE_COUNT)
        .map(|index| {
            (
                settings_page_name(index),
                sidebar[index].clone(),
                page[index].clone(),
            )
        })
        .collect()
}

/// A2 E2E: the currently selected sidebar page index.
pub(crate) fn e2e_selected_sidebar() -> usize {
    SIDEBAR_SELECTED.load(Ordering::SeqCst)
}

/// Whether the About page is the one actually on screen: the window is visible AND that page is selected.
/// The updater renders its prompt into the About page's host, so "the settings window exists" (or even "is
/// visible" while the user sits on another page) is not the same question.
pub(crate) fn about_page_is_visible() -> bool {
    settings_window_is_visible()
        && SIDEBAR_SELECTED.load(Ordering::SeqCst) == SETTINGS_ABOUT_PAGE_INDEX
}

/// Development-switch helper: open the settings window on a specific page (`0=General` ..
/// `SETTINGS_ABOUT_PAGE_INDEX=About`; `SETTINGS_PAGES` is the list).
/// Unlike `open_about_updates` it triggers no update check; it only parks the window on a page.
pub(crate) fn show_settings_page(page: usize) {
    // Present directly on the requested page: `show_settings_inner` selects it (twice, for the pill
    // commit) while the window is still hidden, so the page is right on the first frame. Selecting after
    // the presentation showed General first and then jumped to the target.
    window::show_settings_on_page(page.min(SETTINGS_PAGE_COUNT - 1));
}

/// Build the settings window ahead of its first open (see `window::prewarm_settings_window`).
pub(crate) fn prewarm_settings_window() {
    window::prewarm_settings_window();
}

/// Whether the settings window has been built in this process (shown or not): the state the startup warm-up
/// creates, and the precondition the latency scenario waits for before it clicks.
pub(crate) fn settings_window_is_built() -> bool {
    with_settings_ui(|ui| ui.is_some())
}

/// Wall time of the last settings open (see `window::e2e_last_open_ms`).
pub(crate) fn e2e_last_open_ms() -> u64 {
    window::e2e_last_open_ms()
}

/// Open the settings window on the clipboard history page. The page index stays private to this
/// module; callers that mean "the clipboard settings" name that, not a number.
pub(crate) fn show_clipboard_page() {
    show_settings_page(SETTINGS_CLIPBOARD_PAGE_INDEX);
}

/// Open the settings window on the App Switcher page -- the switcher overlay's own settings entry.
pub(crate) fn show_switcher_page() {
    show_settings_page(SETTINGS_SWITCHER_PAGE_INDEX);
}

/// Open the settings window on the Keystroke Display page -- the floating key panel's settings entry.
pub(crate) fn show_keystroke_display_page() {
    show_settings_page(SETTINGS_KEYSTROKE_DISPLAY_PAGE_INDEX);
}

pub(crate) fn open_about_updates() {
    // Same presenting call as every other entry: the page is selected before the window appears.
    show_settings_page(SETTINGS_ABOUT_PAGE_INDEX);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                scroll_page_to_top(u.page_view(SettingsPageId::About));
            }
        });
    }
    start_inline_update_check();
}

/// Bottom gap kept below the expanded inline update card, matching the padding the compact About
/// page was fitted with.
const UPDATE_CARD_BOTTOM_PADDING: f64 = 32.0;

/// Expand the About page Updates card at the start of a flow so inline update
/// status/progress/buttons fit within the following space.
///
/// The page's layout owner places the host row, the card and the document: the flow only hands over
/// the host's height (its row consumption), the collapse of the action band it replaces, and the
/// tighter bottom padding the expanded card keeps. The card follows the host's bottom edge on its
/// own, exactly like every other card follows its last row, so no card geometry is computed here.
pub(crate) fn expand_update_section(window_h: f64) {
    with_settings_ui(|ui_guard| {
        let Some(ui) = ui_guard.as_mut() else {
            return;
        };
        if ui.update_card.is_null()
            || ui.update_host.is_null()
            || !ui.page_canvases[SETTINGS_ABOUT_PAGE_INDEX].is_bound()
        {
            return;
        }
        unsafe {
            // The flow owns these views' visibility: the panel replaces the check button in the same
            // position, so the band collapses out of the layout while the panel is shown. The divider
            // the panel hangs from belongs to the page and stays visible in every state.
            let _: () = msg_send![ui.update_host, setHidden: false];
            let _: () = msg_send![ui.update_check_button, setHidden: true];
            ui.update_card_expanded = true;
            set_about_restore_control_visible_for_ui(ui, false);

            let canvas = &ui.page_canvases[SETTINGS_ABOUT_PAGE_INDEX];
            canvas.set_bottom_padding(UPDATE_CARD_BOTTOM_PADDING);
            // The band's zero height is what glues the panel's top edge to the divider: without it the
            // panel would start a whole row lower and leave the collapsed button's slot empty.
            canvas.set_row_consume(ui.update_band_row, 0.0);
            // Each Sparkle phase may need a different height: the host grows downward from the
            // divider, and the page re-flows around it.
            canvas.set_row_consume(ui.update_host_row, window_h);
            let host_frame: NSRect = msg_send![ui.update_host, frame];
            let _: () = msg_send![
                ui.update_host,
                setFrameSize: NSSize::new(host_frame.size.width, window_h)
            ];
            let _: () = msg_send![ui.window, layoutIfNeeded];
            debug_validate_settings_page(ui.page_view(SettingsPageId::About), "about-expanded");
        }
    });
}

/// Collapse the About page Updates card and document when a flow ends, restoring the compact look.
/// Hide/show the About page's restore control while the inline update card is expanded, so
/// the grown card and the control don't overlap at the page bottom.
/// Update the About restore control while the caller already owns the settings UI guard.
unsafe fn set_about_restore_control_visible_for_ui(ui: &mut SettingsUi, visible: bool) {
    let control = &mut ui.page_restores[SETTINGS_ABOUT_PAGE_INDEX];
    let _: () = msg_send![control.container, setHidden: !visible];
    let _: () = msg_send![control.surface, setHidden: !visible];
}

pub(crate) fn collapse_update_section() {
    with_settings_ui(|ui_guard| {
        let Some(ui) = ui_guard.as_mut() else {
            return;
        };
        if ui.update_card.is_null() || !ui.update_card_expanded {
            return;
        }
        unsafe {
            // Restore the compact UI first (the restore control becomes visible again), so the
            // layout owner's re-flow accounts for it and lands on the compact document height.
            let host_frame: NSRect = msg_send![ui.update_host, frame];
            let _: () = msg_send![
                ui.update_host,
                setFrameSize: NSSize::new(host_frame.size.width, 0.0)
            ];
            let _: () = msg_send![ui.update_host, setHidden: true];
            let _: () = msg_send![ui.update_check_button, setHidden: false];
            ui.update_card_expanded = false;
            set_about_restore_control_visible_for_ui(ui, true);
            if ui.page_canvases[SETTINGS_ABOUT_PAGE_INDEX].is_bound() {
                let canvas = &ui.page_canvases[SETTINGS_ABOUT_PAGE_INDEX];
                // Undo the flow's geometry: the host row shrinks back to nothing, the action band
                // returns to the shared row height, the card returns to its derived height and the
                // page keeps its usual bottom padding.
                canvas.set_row_consume(ui.update_host_row, 0.0);
                canvas.set_row_consume(ui.update_band_row, SettingsLayout::SINGLE_LINE_ROW_H);
                canvas.set_bottom_padding(SettingsPageHeader::BOTTOM_PADDING);
            }
            let _: () = msg_send![ui.window, layoutIfNeeded];
        }
    });
}

/// Open an external link in the default browser.
unsafe fn open_external_url(url: &str) {
    let url_string = make_nsstring(url);
    let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_string];
    CFRelease(url_string as *const c_void);
    if !url.is_null() {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let _: bool = msg_send![workspace, openURL: url];
    }
}

/// Open the project's official website in the default browser.
pub(crate) extern "C" fn handle_open_official_website(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_open_official_website", || unsafe {
        open_external_url("https://oh-my-tab.app")
    });
}

/// Open the project's GitHub repository in the default browser.
pub(crate) extern "C" fn handle_open_github(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    crate::callback_guard::void("handle_open_github", || unsafe {
        open_external_url("https://github.com/eacryo/oh-my-tab")
    });
}

/// Open the System Settings pane where the Finder extension is enabled (the Finder Menu page's
/// status row). Finder Sync is a system-managed extension, so the app can open the pane but cannot
/// turn it on itself.
pub(crate) extern "C" fn handle_open_finder_extension_management(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_open_finder_extension_management", || {
        crate::finder_menu::open_extension_management();
    });
}

/// Log field-level changes submitted from Settings without recording clipboard contents.
fn log_config_changes(old: &Config, new: &Config) {
    macro_rules! changed {
        ($name:literal, $old:expr, $new:expr) => {{
            let old_value = &$old;
            let new_value = &$new;
            if old_value != new_value {
                log_debug!(
                    "[settings] config changed: {}: {:?} -> {:?}",
                    $name,
                    old_value,
                    new_value
                );
            }
        }};
    }

    changed!(
        "appearance.theme",
        old.appearance.theme,
        new.appearance.theme
    );
    changed!(
        "appearance.panel_material",
        old.appearance.panel_material,
        new.appearance.panel_material
    );
    changed!(
        "appearance.glass_style",
        old.appearance.glass_style,
        new.appearance.glass_style
    );
    changed!(
        "appearance.corner_radius",
        old.appearance.corner_radius,
        new.appearance.corner_radius
    );

    changed!(
        "colors.dark.status_bar_text",
        old.colors.dark.status_bar_text,
        new.colors.dark.status_bar_text
    );
    changed!(
        "colors.dark.app_name",
        old.colors.dark.app_name,
        new.colors.dark.app_name
    );
    changed!(
        "colors.dark.win_title",
        old.colors.dark.win_title,
        new.colors.dark.win_title
    );
    changed!(
        "colors.dark.icon_inner_bg",
        old.colors.dark.icon_inner_bg,
        new.colors.dark.icon_inner_bg
    );
    changed!(
        "colors.dark.icon_text",
        old.colors.dark.icon_text,
        new.colors.dark.icon_text
    );
    changed!(
        "colors.dark.card_bg_sel",
        old.colors.dark.card_bg_sel,
        new.colors.dark.card_bg_sel
    );
    changed!(
        "colors.dark.card_border_sel",
        old.colors.dark.card_border_sel,
        new.colors.dark.card_border_sel
    );
    changed!(
        "colors.light.status_bar_text",
        old.colors.light.status_bar_text,
        new.colors.light.status_bar_text
    );
    changed!(
        "colors.light.app_name",
        old.colors.light.app_name,
        new.colors.light.app_name
    );
    changed!(
        "colors.light.win_title",
        old.colors.light.win_title,
        new.colors.light.win_title
    );
    changed!(
        "colors.light.icon_inner_bg",
        old.colors.light.icon_inner_bg,
        new.colors.light.icon_inner_bg
    );
    changed!(
        "colors.light.icon_text",
        old.colors.light.icon_text,
        new.colors.light.icon_text
    );
    changed!(
        "colors.light.card_bg_sel",
        old.colors.light.card_bg_sel,
        new.colors.light.card_bg_sel
    );
    changed!(
        "colors.light.card_border_sel",
        old.colors.light.card_border_sel,
        new.colors.light.card_border_sel
    );

    changed!(
        "fonts.status_bar_size",
        old.fonts.status_bar_size,
        new.fonts.status_bar_size
    );
    changed!(
        "fonts.status_bar_weight",
        old.fonts.status_bar_weight,
        new.fonts.status_bar_weight
    );
    changed!(
        "fonts.title_size",
        old.fonts.title_size,
        new.fonts.title_size
    );
    changed!(
        "fonts.title_weight",
        old.fonts.title_weight,
        new.fonts.title_weight
    );
    changed!(
        "fonts.app_name_size",
        old.fonts.app_name_size,
        new.fonts.app_name_size
    );
    changed!(
        "fonts.app_name_weight",
        old.fonts.app_name_weight,
        new.fonts.app_name_weight
    );

    changed!(
        "keyboard.modifier",
        old.keyboard.modifier,
        new.keyboard.modifier
    );
    changed!(
        "keystroke_display.enabled",
        old.keystroke_display.enabled,
        new.keystroke_display.enabled
    );
    changed!(
        "keystroke_display.mode",
        old.keystroke_display.mode,
        new.keystroke_display.mode
    );
    changed!(
        "keystroke_display.tap_level",
        old.keystroke_display.tap_level,
        new.keystroke_display.tap_level
    );
    changed!(
        "keystroke_display.display_position",
        old.keystroke_display.display_position,
        new.keystroke_display.display_position
    );
    changed!("i18n.locale", old.i18n.locale, new.i18n.locale);
    changed!("windows.enabled", old.windows.enabled, new.windows.enabled);
    changed!(
        "windows.show_minimized",
        old.windows.show_minimized,
        new.windows.show_minimized
    );
    changed!(
        "windows.show_other_desktops",
        old.windows.show_other_desktops,
        new.windows.show_other_desktops
    );
    changed!(
        "layout.thumbnails_enabled",
        old.layout.thumbnails_enabled,
        new.layout.thumbnails_enabled
    );
    changed!(
        "layout.focused_thumbnail_prewarm",
        old.layout.focused_thumbnail_prewarm,
        new.layout.focused_thumbnail_prewarm
    );
    changed!(
        "layout.show_app_name_in_cards",
        old.layout.show_app_name_in_cards,
        new.layout.show_app_name_in_cards
    );
    changed!(
        "layout.show_footer_title",
        old.layout.show_footer_title,
        new.layout.show_footer_title
    );
    changed!(
        "layout.card_text_size",
        old.layout.card_text_size,
        new.layout.card_text_size
    );
    changed!(
        "layout.thumbnail_size",
        old.layout.thumbnail_size,
        new.layout.thumbnail_size
    );
    changed!(
        "windows.overlay_position",
        old.windows.overlay_position,
        new.windows.overlay_position
    );
    changed!(
        "windows.activation_mode",
        old.windows.activation_mode,
        new.windows.activation_mode
    );
    changed!("logging.level", old.logging.level, new.logging.level);
    changed!(
        "logging.file_path",
        old.logging.file_path,
        new.logging.file_path
    );
    changed!(
        "startup.launch_at_login",
        old.startup.launch_at_login,
        new.startup.launch_at_login
    );
    changed!(
        "startup.hide_menu_bar_icon",
        old.startup.hide_menu_bar_icon,
        new.startup.hide_menu_bar_icon
    );
    changed!(
        "updates.automatically_check",
        old.updates.automatically_check,
        new.updates.automatically_check
    );

    changed!(
        "clipboard.enabled",
        old.clipboard.enabled,
        new.clipboard.enabled
    );
    changed!(
        "clipboard.paused",
        old.clipboard.paused,
        new.clipboard.paused
    );
    // The list has no single scalar to print; the entry count is the change that matters for a log
    // that must never carry clipboard content or identifiers.
    if old.clipboard.excluded_apps != new.clipboard.excluded_apps {
        crate::log_info!(
            "[settings] clipboard.excluded_apps: {} -> {} entries",
            old.clipboard.excluded_apps.len(),
            new.clipboard.excluded_apps.len()
        );
    }
    changed!(
        "clipboard.max_entries",
        old.clipboard.max_entries,
        new.clipboard.max_entries
    );
    changed!(
        "clipboard.show_source_app",
        old.clipboard.show_source_app,
        new.clipboard.show_source_app
    );
    changed!(
        "clipboard.clear_on_quit",
        old.clipboard.clear_on_quit,
        new.clipboard.clear_on_quit
    );
    changed!(
        "clipboard.separate_clear_all",
        old.clipboard.separate_clear_all,
        new.clipboard.separate_clear_all
    );
    changed!(
        "clipboard.move_used_to_top",
        old.clipboard.move_used_to_top,
        new.clipboard.move_used_to_top
    );
    changed!(
        "clipboard.delete_after_paste",
        old.clipboard.delete_after_paste,
        new.clipboard.delete_after_paste
    );
    changed!(
        "clipboard.clear_system_pasteboard_after_paste",
        old.clipboard.clear_system_pasteboard_after_paste,
        new.clipboard.clear_system_pasteboard_after_paste
    );
    changed!(
        "clipboard.auto_expire_days",
        old.clipboard.auto_expire_days,
        new.clipboard.auto_expire_days
    );
    changed!(
        "clipboard.picker_placement",
        old.clipboard.picker_placement,
        new.clipboard.picker_placement
    );
    changed!(
        "clipboard.pin_follow_selection",
        old.clipboard.pin_follow_selection,
        new.clipboard.pin_follow_selection
    );

    changed!(
        "finder_menu.enabled",
        old.finder_menu.enabled,
        new.finder_menu.enabled
    );
    changed!(
        "finder_menu.layout",
        old.finder_menu.layout,
        new.finder_menu.layout
    );
    changed!(
        "finder_menu.copy_path",
        old.finder_menu.copy_path,
        new.finder_menu.copy_path
    );
    changed!(
        "finder_menu.new_file_types",
        old.finder_menu.new_file_types,
        new.finder_menu.new_file_types
    );
    changed!(
        "finder_menu.terminals",
        old.finder_menu.terminals,
        new.finder_menu.terminals
    );

    changed!("mouse.enabled", old.mouse.enabled, new.mouse.enabled);
    changed!(
        "window_control.enabled",
        old.window_control.enabled,
        new.window_control.enabled
    );
    changed!(
        "window_control.up",
        old.window_control.up,
        new.window_control.up
    );
    changed!(
        "window_control.down",
        old.window_control.down,
        new.window_control.down
    );
    changed!(
        "window_control.left",
        old.window_control.left,
        new.window_control.left
    );
    changed!(
        "window_control.right",
        old.window_control.right,
        new.window_control.right
    );
    changed!(
        "window_control.display_up",
        old.window_control.display_up,
        new.window_control.display_up
    );
    changed!(
        "window_control.display_down",
        old.window_control.display_down,
        new.window_control.display_down
    );
    changed!(
        "window_control.display_left",
        old.window_control.display_left,
        new.window_control.display_left
    );
    changed!(
        "window_control.display_right",
        old.window_control.display_right,
        new.window_control.display_right
    );

    // Mouse profiles contain nested mappings, so compare and log complete Debug snapshots.
    let old_profiles = format!("{:?}", old.mouse.profiles);
    let new_profiles = format!("{:?}", new.mouse.profiles);
    if old_profiles != new_profiles {
        log_debug!(
            "[settings] config changed: mouse.profiles: {:?} -> {:?}",
            old_profiles,
            new_profiles
        );
    }
}

/// Populate settings controls from current CONFIG (refreshed on each open). Rebuilds the device
/// popup to reflect hot-plug changes.
/// Whether the mouse page's controls must be (re)filled right now. Pure, so both orderings the caller can
/// present are testable: `requested` is the page this open is FOR (`None` for an in-place refresh) and
/// `selected` is the page currently on screen, which only matters for a refresh.
///
/// The page is expensive (it resolves the selected device and renders its mappings, and asking which
/// devices are connected used to cost ~170ms), and the settings window opens on General or Clipboard far
/// more often than on Mouse, so filling it for every open was pure waste.
fn mouse_page_needs_fill(
    requested: Option<usize>,
    selected: usize,
    materialized: bool,
    dirty: bool,
) -> bool {
    let target = requested.unwrap_or(selected);
    target == SETTINGS_MOUSE_PAGE_INDEX && (!materialized || dirty)
}

/// Whether the mouse page's controls currently hold data (false after a content rebuild).
static MOUSE_PAGE_MATERIALIZED: AtomicBool = AtomicBool::new(false);
/// Whether the mouse page's data may have changed since it was filled (a config or permission change, or
/// a fresh rebuild).
static MOUSE_PAGE_DIRTY: AtomicBool = AtomicBool::new(true);

/// How many times the settings content has been rebuilt since the process started. Exposed so
/// `--smoke-settings-layout` can tell an in-place update from a rebuild; `note_settings_content_rebuilt`
/// itself is only a notification.
static SETTINGS_CONTENT_REBUILDS: AtomicUsize = AtomicUsize::new(0);

/// How many times the settings content has been rebuilt since the process started.
pub(crate) fn settings_content_rebuild_count() -> usize {
    SETTINGS_CONTENT_REBUILDS.load(Ordering::SeqCst)
}

/// The window's content was rebuilt: every control is new, so the mouse page has to be filled again.
pub(super) fn note_settings_content_rebuilt() {
    SETTINGS_CONTENT_REBUILDS.fetch_add(1, Ordering::SeqCst);
    MOUSE_PAGE_MATERIALIZED.store(false, Ordering::SeqCst);
    MOUSE_PAGE_DIRTY.store(true, Ordering::SeqCst);
    // The numeric fields are new too: an edit recorded against the old controls must not commit through a
    // fresh field the user has never touched.
    dispatch::note_text_fields_rebuilt();
}

/// The mouse page was just filled from the live config.
fn note_mouse_page_materialized() {
    MOUSE_PAGE_MATERIALIZED.store(true, Ordering::SeqCst);
    MOUSE_PAGE_DIRTY.store(false, Ordering::SeqCst);
}

/// A dispatch path filled every mouse-page control itself (the user picked another device): the page holds
/// the reloaded device's values, so the lazy loader has nothing left to do.
pub(super) fn note_mouse_page_materialized_after_direct_fill() {
    note_mouse_page_materialized();
}

/// The mouse page's inputs changed (a device was plugged or unplugged): whatever it holds is stale, so the
/// next time it is the page in use it must be filled again. Used by the hot-plug path, which only rebuilds
/// the device popup and only while the window is visible.
pub(super) fn note_mouse_page_dirty() {
    MOUSE_PAGE_DIRTY.store(true, Ordering::SeqCst);
}

/// E2E: whether the mouse page's controls hold data (the lazy fill is observable state, not an
/// implementation detail a test can infer).
pub(crate) fn e2e_mouse_page_materialized() -> bool {
    MOUSE_PAGE_MATERIALIZED.load(Ordering::SeqCst)
}

/// E2E: the mouse page's master switch state (0/1), which a scenario can read back to see that a reload
/// really refilled this page's controls. 0 when there is no window.
pub(crate) fn e2e_mouse_enabled_state() -> isize {
    unsafe {
        with_settings_ui(|ui_guard| {
            let Some(ui) = ui_guard.as_ref() else {
                return 0;
            };
            if ui.enable_mouse.is_null() {
                return 0;
            }
            msg_send![ui.enable_mouse, state]
        })
    }
}

/// E2E: the text the mouse page shows for the pointer's line count, which is what "the page is filled"
/// means in user terms (empty before the page is materialized).
pub(crate) fn e2e_mouse_line_count_text() -> String {
    unsafe {
        with_settings_ui(|ui_guard| {
            let Some(ui) = ui_guard.as_ref() else {
                return String::new();
            };
            if ui.line_count_value_label.is_null() {
                return String::new();
            }
            let text: *mut AnyObject = msg_send![ui.line_count_value_label, stringValue];
            if text.is_null() {
                String::new()
            } else {
                crate::ffi::nsstring_to_rust(text)
            }
        })
    }
}

/// Load the settings controls from the live config for a page that is being opened (`Some(page)`) or for an
/// in-place refresh (`None`).
fn load_settings_values_for(requested_page: Option<usize>) {
    if requested_page.is_none() {
        // An in-place refresh means the values may have changed: whatever the mouse page shows is stale.
        MOUSE_PAGE_DIRTY.store(true, Ordering::SeqCst);
    }
    let cfg = CONFIG.read().unwrap().clone();
    load_settings_from(&cfg, requested_page);
}

fn load_settings_values() {
    load_settings_values_for(None);
}

/// Populate settings controls from a given config (shared by normal open and post-restore
/// refill).
fn load_settings_from(cfg: &Config, requested_page: Option<usize>) {
    unsafe {
        with_settings_ui(|ui_guard| {
            let ui = match ui_guard.as_mut() {
                Some(u) => u,
                None => return,
            };
            let theme_idx: isize = match cfg.appearance.theme.as_str() {
                "dark" => 0,
                "light" => 1,
                _ => 2,
            };
            let _: () = msg_send![ui.theme, selectItemAtIndex: theme_idx];
            let pm_idx: isize =
                crate::config::panel_material_index_of(&cfg.appearance.panel_material).unwrap_or(0)
                    as isize;
            let _: () = msg_send![ui.panel_material, selectItemAtIndex: pm_idx];
            // The glass sub-option rows exist only while Liquid Glass is the selected
            // material; with another material their controls are null and there is
            // nothing to synchronize.
            if !ui.glass_style.is_null() {
                let gs_idx: isize =
                    crate::glass::glass_style_index(&cfg.appearance.glass_style) as isize;
                let _: () = msg_send![ui.glass_style, selectItemAtIndex: gs_idx];
            }
            set_field(ui.corner_radius, cfg.appearance.corner_radius);
            let card_text_size = text_size_slider_value(cfg.layout.card_text_size);
            // The slider's domain is stop indices; the readout shows the point size the stop
            // means, so the label reads "0" (hidden) or "13".."20", never a raw index.
            let status_bar_text_size = status_bar_size_slider_value(cfg.fonts.status_bar_size);
            let _: () = msg_send![ui.card_text_size, setIntegerValue: card_text_size as isize];
            set_field(ui.card_text_size_value_label, card_text_size);
            let _: () = msg_send![
                ui.status_bar_text_size,
                setIntegerValue: status_bar_text_size as isize
            ];
            set_field(
                ui.status_bar_text_size_value_label,
                status_bar_size_for_index(status_bar_text_size),
            );
            let mod_idx =
                crate::config::switcher_modifier_index_of(&cfg.keyboard.modifier) as isize;
            let _: () = msg_send![ui.modifier, selectItemAtIndex: mod_idx];
            let _: () = msg_send![
                ui.keystroke_display_enabled,
                setState: if cfg.keystroke_display.enabled { 1isize } else { 0isize }
            ];
            let mode_idx: isize = match cfg.keystroke_display.mode.as_str() {
                "shortcuts" => 1,
                "commands" => 2,
                _ => 0,
            };
            let _: () = msg_send![ui.keystroke_display_mode, selectItemAtIndex: mode_idx];
            let tap_idx: isize = if cfg.keystroke_display.tap_level == "hid" {
                1
            } else {
                0
            };
            let _: () = msg_send![ui.keystroke_display_tap_level, selectItemAtIndex: tap_idx];
            let display_position_idx = KEYSTROKE_DISPLAY_POSITION_VALUES
                .iter()
                .position(|mode| *mode == cfg.keystroke_display.display_position)
                .unwrap_or(1) as isize;
            let _: () = msg_send![
                ui.keystroke_display_position,
                selectItemAtIndex: display_position_idx
            ];
            let initial_position_idx = KEYSTROKE_DISPLAY_INITIAL_POSITION_VALUES
                .iter()
                .position(|edge| *edge == cfg.keystroke_display.initial_position)
                .unwrap_or(1) as isize;
            let _: () = msg_send![
                ui.keystroke_display_initial_position,
                selectItemAtIndex: initial_position_idx
            ];
            let max_extent = crate::config::keystroke_display_max_extent_percent_snap(
                cfg.keystroke_display.max_extent_percent,
            );
            let _: () = msg_send![
                ui.keystroke_display_max_extent,
                setIntegerValue: max_extent as isize
            ];
            page_builder::set_max_extent_readout(ui.keystroke_display_max_extent_value, max_extent);
            // locale: select the item matching CONFIG.i18n.locale; fall back to index 0 (auto).
            let loc_idx: isize = LOCALE_VALUES
                .iter()
                .position(|v| *v == cfg.i18n.locale.as_str())
                .map(|i| i as isize)
                .unwrap_or(0);
            let _: () = msg_send![ui.locale, selectItemAtIndex: loc_idx];
            // windows_enabled / show_minimized: switch state (1=on / 0=off).
            let we_state = if cfg.windows.enabled { 1isize } else { 0isize };
            let _: () = msg_send![ui.windows_enabled, setState: we_state];
            let sm_state = if cfg.windows.show_minimized {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![ui.show_minimized, setState: sm_state];
            let hidden_app_windows_state = if cfg.windows.show_hidden_app_windows {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![
                ui.show_hidden_app_windows,
                setState: hidden_app_windows_state
            ];
            let other_desktops_state = if cfg.windows.show_other_desktops {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![ui.show_other_desktops, setState: other_desktops_state];
            // Window display mode index 0 = icons only, 1 = icons and thumbnails.
            let th_idx: isize = if cfg.layout.thumbnails_enabled { 1 } else { 0 };
            let _: () = msg_send![ui.thumbnails_enabled, selectItemAtIndex: th_idx];
            let ts_idx = crate::config::thumbnail_size_index_of(&cfg.layout.thumbnail_size)
                .unwrap_or(0) as isize;
            let _: () = msg_send![ui.thumbnail_size, selectItemAtIndex: ts_idx];
            let prewarm_state = if cfg.layout.focused_thumbnail_prewarm {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![ui.focused_thumbnail_prewarm, setState: prewarm_state];
            let app_name_state = if cfg.layout.show_app_name_in_cards {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![ui.show_app_name_in_cards, setState: app_name_state];
            // overlay_position: popup index 0 = follow active window (active_window), 1 = main (main).
            let op_idx = match cfg.windows.overlay_position.as_str() {
                "main" => 1,
                _ => 0, // "active_window" (default)
            };
            let _: () = msg_send![ui.overlay_position, selectItemAtIndex: op_idx as isize];
            // activation_mode: popup index 0 = activate on hover (hover), 1 = activate on click (click).
            let activation_idx = match cfg.windows.activation_mode.as_str() {
                "click" => 1,
                _ => 0,
            };
            let _: () = msg_send![ui.activation_mode, selectItemAtIndex: activation_idx as isize];
            // In-overlay action keys: four independent switches, off by default. They must be filled
            // here (the ordinary load path), not only by the external config-change refresh, or a
            // freshly opened or rebuilt page shows them off while the config says otherwise.
            for (control, on) in [
                (ui.panel_close_enabled, cfg.windows.panel_close_enabled),
                (
                    ui.panel_minimize_enabled,
                    cfg.windows.panel_minimize_enabled,
                ),
                (ui.panel_hide_enabled, cfg.windows.panel_hide_enabled),
                (ui.panel_quit_enabled, cfg.windows.panel_quit_enabled),
            ] {
                let _: () = msg_send![control, setState: if on { 1isize } else { 0isize }];
            }
            // log_level: popup index 0..1 = debug, info; default index 1 (info).
            let ll_idx = match cfg.logging.level.as_str() {
                "debug" => 0,
                _ => 1, // "info" (default)
            };
            let _: () = msg_send![ui.log_level, selectItemAtIndex: ll_idx as isize];
            // launch_at_login: set the switch state from CONFIG.startup.launch_at_login.
            let _: () = msg_send![ui.launch_at_login, setState: if cfg.startup.launch_at_login { 1isize } else { 0isize }];
            // hide_menu_bar_icon: the switch reflects the saved value; the status item itself is
            // toggled by runtime_config, so the launch path and a live edit agree.
            let _: () = msg_send![ui.hide_menu_bar_icon, setState: if cfg.startup.hide_menu_bar_icon { 1isize } else { 0isize }];
            let _: () = msg_send![
                ui.update_auto_check,
                setState: if cfg.updates.automatically_check { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.update_auto_download,
                setState: if cfg.updates.automatically_download { 1isize } else { 0isize }
            ];
            update_windows_controls_enabled(ui);

            // Mouse page: filled only when it is the page being opened, or when it is the page on screen
            // and its data is stale. Everything else in this function is a cheap control assignment; this
            // block resolves a device and renders its mappings.
            if mouse_page_needs_fill(
                requested_page,
                SIDEBAR_SELECTED.load(Ordering::SeqCst),
                MOUSE_PAGE_MATERIALIZED.load(Ordering::SeqCst),
                MOUSE_PAGE_DIRTY.load(Ordering::SeqCst),
            ) {
                materialize_mouse_page(ui, cfg);
            }
            // Not part of the mouse fill: this one helper also recomputes the display-mode pair (General)
            // and the clipboard-delete rows (Clipboard), so every open needs it whatever page it is for.
            update_conditional_rows(ui);

            // Clipboard page: populate the recorded chord from the global config.
            let recording = *REC_STAGE.lock().unwrap() != RecStage::Idle
                && *REC_MODE.lock().unwrap() == RecMode::ClipboardShortcut;
            dispatch::set_clipboard_shortcut_capture_state(
                ui,
                &cfg.clipboard.shortcut,
                recording,
                None,
            );
            let _: () = msg_send![
                ui.clipboard_enabled,
                setState: if cfg.clipboard.enabled { 1isize } else { 0isize }
            ];
            // Key-bindings page: the master switch is static, the rows are rendered from the config
            // by the bindings module (which also owns their enable state).
            let _: () = msg_send![
                ui.key_bindings_enabled,
                setState: if cfg.key_bindings.enabled { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.clipboard_clear_on_quit,
                setState: if cfg.clipboard.clear_on_quit { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.clipboard_separate_clear_all,
                setState: if cfg.clipboard.separate_clear_all {
                    1isize
                } else {
                    0isize
                }
            ];
            let _: () = msg_send![
                ui.clipboard_show_source_app,
                setState: if cfg.clipboard.show_source_app { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.clipboard_move_used_to_top,
                setState: if cfg.clipboard.move_used_to_top { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.clipboard_delete_after_paste,
                setState: if cfg.clipboard.delete_after_paste { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.clipboard_clear_system_pasteboard_after_paste,
                setState: if cfg.clipboard.clear_system_pasteboard_after_paste {
                    1isize
                } else {
                    0isize
                }
            ];
            // The clipboard controls are populated after the initial conditional-row pass, so
            // apply visibility once more from the saved config here.
            update_clipboard_delete_dependent_visibility_from_config(ui, cfg);
            let _: () = msg_send![
                ui.clipboard_paused,
                setState: if cfg.clipboard.paused { 1isize } else { 0isize }
            ];
            set_field(
                ui.clipboard_max_entries,
                cfg.clipboard.max_entries.to_string(),
            );
            let auto_expire_days = (cfg.clipboard.auto_expire_days as i64)
                .clamp(CLIPBOARD_AUTO_EXPIRE_MIN, CLIPBOARD_AUTO_EXPIRE_MAX);
            let _: () = msg_send![
                ui.clipboard_auto_expire_days,
                setIntegerValue: auto_expire_days as isize
            ];
            set_field(ui.clipboard_auto_expire_days_value_label, auto_expire_days);
            let image_rows = (cfg.clipboard.image_rows as i64).clamp(1, 3);
            let _: () = msg_send![
                ui.clipboard_image_rows,
                setIntegerValue: image_rows as isize
            ];
            set_field(ui.clipboard_image_rows_value_label, image_rows);
            // pin_follow_selection: popup index 0 = follow, 1 = keep.
            let pin_idx: isize = if cfg.clipboard.pin_follow_selection {
                0
            } else {
                1
            };
            let _: () = msg_send![ui.clipboard_pin_follow, selectItemAtIndex: pin_idx];
            // picker_placement: popup index 0 = follow the cursor, 1 = screen center. The same mapping
            // `apply_control_field` writes back, so an untouched page cannot show one placement while
            // the picker uses the other.
            let placement_idx = dispatch::picker_placement_index(&cfg.clipboard.picker_placement);
            let _: () = msg_send![ui.clipboard_picker_placement, selectItemAtIndex: placement_idx];
            update_clipboard_controls_enabled(ui);
            // Finder Menu page: the status line is read-only state from System Settings, and every
            // switch mirrors the config's vec-backed fields (membership, not order).
            fill_finder_menu_controls(ui, cfg);
            bindings::render_binding_rows(ui, cfg);
            bindings::update_binding_controls_enabled(ui);
        });
    }
}

/// Fill every mouse-page control from `cfg`: the master switch, the per-device controls, the device popup,
/// the mappings in edit, and the conditional rows that depend on them.
///
/// Extracted from `load_settings_from` so the page can be materialized on demand (see
/// `mouse_page_needs_fill`) and from outside the value load (the sidebar selection).
unsafe fn materialize_mouse_page(ui: &mut SettingsUi, cfg: &Config) {
    // Calibrate SELECTED_DEVICE first (against the current device list; falls back to the first device
    // when uninitialized/unplugged), then resolve, so the UI shows the actually effective device's config
    // (fixes the wrong-profile display on first open).
    ensure_selected_device();
    // Resolve the selected device's effective config from the given cfg (Config::default() during the
    // restore-defaults preview).
    let resolved = resolve_selected_from(cfg);
    // enable_mouse (master switch) always reads the global flag. The fill lands on a page the
    // sidebar has already shown (see materialize_mouse_page_if_visible), so it must not animate.
    widgets::set_switch_state_immediate(ui.enable_mouse, cfg.mouse.enabled);
    // Fill the mouse page's per-device controls (reverse/accel/mode/line count/preset).
    fill_mouse_device_controls(ui, &resolved);

    // The mappings in-edit = the selected device's OWN profile mappings (not the merged values:
    // edits/deletes only touch this device's dedicated profile; the wildcard "All Mice" profile has no UI
    // entry, so it isn't edited here).
    let dev = current_selected_device();
    let prof_idx = find_profile_index(cfg, dev);
    *MAPPING_EDITS.lock().unwrap() = prof_idx
        .map(|i| cfg.mouse.profiles[i].button_mappings.clone())
        .unwrap_or_default();

    render_mapping_rows_locked(ui);

    // Rebuild the device popup (refreshed on each settings open to reflect hot-plug).
    rebuild_device_popup(ui);

    // Freeze/unfreeze the controls below based on the enable_mouse state, and recompute the two conditional
    // row groups THIS page owns (the line-count and tracking-speed blocks). The combined
    // `update_conditional_rows` also covers the display-mode pair and the clipboard-delete rows, which
    // belong to other pages: it stays in the value load so those pages keep being refreshed on every open.
    update_mouse_controls_enabled(ui);
    update_mode_dependent_visibility(ui);
    update_pointer_accel_visibility(ui);

    note_mouse_page_materialized();
}

/// Fill the mouse page if the page on screen is the Mouse page and its data is stale. Called after a
/// sidebar selection (never from inside `with_settings_ui`: the value load re-borrows the settings slot).
pub(super) fn materialize_mouse_page_if_visible() {
    if !mouse_page_needs_fill(
        None,
        SIDEBAR_SELECTED.load(Ordering::SeqCst),
        MOUSE_PAGE_MATERIALIZED.load(Ordering::SeqCst),
        MOUSE_PAGE_DIRTY.load(Ordering::SeqCst),
    ) {
        return;
    }
    let cfg = CONFIG.read().unwrap().clone();
    unsafe {
        with_settings_ui(|ui_guard| {
            if let Some(ui) = ui_guard.as_mut() {
                materialize_mouse_page(ui, &cfg);
            }
        });
    }
}

pub(super) fn materialize_mouse_page_for_search() {
    if MOUSE_PAGE_MATERIALIZED.load(Ordering::SeqCst) && !MOUSE_PAGE_DIRTY.load(Ordering::SeqCst) {
        return;
    }
    let cfg = CONFIG.read().unwrap().clone();
    unsafe {
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_mut() {
                materialize_mouse_page(ui, &cfg);
            }
        });
    }
}

/// Fill the mouse page's per-device controls (reverse/disable-accel/tracking-speed/mode/line-count).
/// Shared by load_settings_from and handle_device_changed.
unsafe fn fill_mouse_device_controls(
    ui: &SettingsUi,
    resolved: &crate::mouse::resolve::ResolvedMouse,
) {
    // reverse_scroll / disable_pointer_accel: effective values, applied without the spring (the
    // fill lands on an already visible page).
    widgets::set_switch_state_immediate(ui.reverse_scroll, resolved.reverse_scroll);
    widgets::set_switch_state_immediate(ui.disable_pointer_accel, resolved.disable_acceleration);
    // scroll_mode: effective value.
    let sm_idx: isize = SCROLL_MODE_VALUES
        .iter()
        .position(|v| *v == resolved.scroll_mode.as_str())
        .map(|i| i as isize)
        .unwrap_or(0);
    let _: () = msg_send![ui.scroll_mode, selectItemAtIndex: sm_idx];
    // line_count: effective value (Line mode's lines-per-notch slider).
    let _: () = msg_send![ui.line_count, setIntegerValue: resolved.line_count as isize];
    // Sync the slider's value label.
    set_field(ui.line_count_value_label, resolved.line_count);
    // The mappings master switch: the effective value (merged across profiles), filled silently.
    widgets::set_switch_state_immediate(ui.mapping_enabled, resolved.button_mappings_enabled);
    // Pointer acceleration / tracking speed: the configured value wins; when unset, show the
    // device's live value (same as LinearMouse -- read the device instead of inventing a
    // default); the fallback applies only when that read fails.
    // With "All Mice" selected there is no single device to read, so fall back to the first
    // connected one (LinearMouse does the same via firstMatchedDevice).
    let source_device = current_selected_device().or_else(|| {
        crate::mouse::device::connected_devices()
            .first()
            .map(|d| (d.vendor_id, d.product_id))
    });
    let acceleration = resolved
        .acceleration
        .or_else(|| source_device.and_then(crate::mouse::pointer::read_acceleration))
        .unwrap_or(crate::mouse::pointer::FALLBACK_ACCELERATION);
    // The slider is linear (0..=10); a value outside the range (the device value may come from
    // another tool) is clamped so the readout and the handle stay consistent.
    let acceleration = acceleration.clamp(
        crate::config::MOUSE_ACCELERATION_MIN,
        crate::config::MOUSE_ACCELERATION_MAX,
    );
    let _: () = msg_send![ui.pointer_accel_slider, setDoubleValue: acceleration];
    set_field(
        ui.pointer_accel_value_label,
        pointer_accel_display(acceleration),
    );

    let smooth = resolved.smooth_scrolling;
    let preset_index = crate::mouse::smooth::presets::SmoothPreset::ALL
        .iter()
        .position(|preset| *preset == smooth.preset)
        .unwrap_or(0) as isize;
    let _: () = msg_send![ui.smooth_scrolling_preset, selectItemAtIndex: preset_index];
    for (slider, value, label) in [
        (
            ui.smooth_scrolling_response,
            smooth.response.clamp(0.0, 2.0),
            ui.smooth_scrolling_response_value,
        ),
        (
            ui.smooth_scrolling_speed,
            smooth.speed.clamp(0.0, 8.0),
            ui.smooth_scrolling_speed_value,
        ),
        (
            ui.smooth_scrolling_acceleration,
            smooth.acceleration.clamp(0.0, 8.0),
            ui.smooth_scrolling_acceleration_value,
        ),
        (
            ui.smooth_scrolling_inertia,
            smooth.inertia.clamp(0.0, 8.0),
            ui.smooth_scrolling_inertia_value,
        ),
    ] {
        let _: () = msg_send![slider, setDoubleValue: value];
        set_field(label, format!("{value:.1}"));
    }
}

/// Build the settings window once, store it in SETTINGS_UI, then reuse (hide, not destroy).
// Custom settings window subclass overriding performClose:/close so both the red close button and
// direct close paths route through hide_settings (which flips activation policy back to
// .accessory), instead of the default orderOut (which would not trigger the policy switch or
// clean up the independent shared color panel). create_settings_window can be called again after
// invalidate_settings_window, so registration is guarded with OnceLock.
extern "C" fn settings_window_perform_close(
    _self: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) {
    crate::callback_guard::void("settings_window_perform_close", || {
        hide_settings();
    });
}

extern "C" fn settings_window_close(_self: *mut AnyObject, _cmd: Sel) {
    crate::callback_guard::void("settings_window_close", || {
        hide_settings();
    });
}

// Constants for Cmd+Q handling: NSEventModifierFlagCommand = 1 << 20, ANSI Q keyCode = 12.
const NSEVENT_MODIFIER_FLAG_COMMAND: u64 = 1 << 20;
const KEYCODE_Q: u16 = 12;
/// ANSI F keyCode: the search field's focus shortcut (the `⌘F` hint in the sidebar's shell).
const KEYCODE_F: u16 = 3;
/// NSEventModifierFlagShift / NSEventModifierFlagOption: Cmd+F is the focus shortcut only in its
/// bare form, so the editing chords the field relies on still reach it untouched.
const NSEVENT_MODIFIER_FLAG_SHIFT: u64 = 1 << 17;
const NSEVENT_MODIFIER_FLAG_OPTION: u64 = 1 << 19;
const NSEVENT_TYPE_LEFT_MOUSE_DOWN: usize = 1;
/// NSEventTypeLeftMouseUp. The panel size preview ends here: a slider drag can also finish outside
/// the window, so the mouse-up is observed at the window's event-dispatch point rather than on the
/// control. Any button's up settles the drag, because the preview must never outlive one.
const NSEVENT_TYPE_LEFT_MOUSE_UP: usize = 2;
const NSEVENT_TYPE_RIGHT_MOUSE_UP: usize = 4;

/// End inline text editing when the user clicks elsewhere in the settings window.
///
/// The settings window uses a borderless collection of plain NSViews as its page background.
/// Those views do not become first responder themselves, so AppKit otherwise leaves an
/// NSTextField editor active after a click on empty page space. Resigning before dispatching the
/// mouse event lets the clicked control become first responder again when appropriate.
/// Panic boundary for the C callback: a panic cannot unwind through an `extern "C"` frame (it
/// aborts the process), so it is contained here.
extern "C" fn settings_window_send_event(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("settings_window_send_event", || unsafe {
        settings_window_send_event_inner(_self, _cmd, event)
    });
}

unsafe fn settings_window_send_event_inner(
    _self: *mut AnyObject,
    _cmd: Sel,
    event: *mut AnyObject,
) {
    unsafe {
        if !event.is_null() {
            let event_type: usize = msg_send![event, type];
            // A slider drag can also end outside the window (or on another control), so the
            // preview is settled here, at the one place every mouse-up passes through, instead of
            // on the slider itself -- and it settles before the event is dispatched, so the panel
            // is back under the real stream by the time the next tick runs.
            if event_type == NSEVENT_TYPE_LEFT_MOUSE_UP || event_type == NSEVENT_TYPE_RIGHT_MOUSE_UP
            {
                dispatch::end_max_extent_preview();
            }
            if event_type == NSEVENT_TYPE_LEFT_MOUSE_DOWN {
                let window = _self;
                collapse_restore_confirmations_on_external_click(window, event);
                // Closing a dropdown on an outside click no longer lives here: it is handled by the
                // local event monitor in widgets, because the dropdown has its own popup window and
                // hooking only the settings window misses the recording panel and others.
                tooltip::SettingsTooltip::handle_mouse_down(window, event);
                let first_responder: *mut AnyObject = msg_send![window, firstResponder];
                if !first_responder.is_null() {
                    let is_text_field: bool =
                        msg_send![first_responder, isKindOfClass: class!(NSTextField)];
                    // While an NSTextField is being edited, AppKit installs its shared
                    // NSTextView field editor as the window's first responder.
                    let is_field_editor: bool =
                        msg_send![first_responder, isKindOfClass: class!(NSTextView)];
                    if is_text_field || is_field_editor {
                        let _: bool =
                            msg_send![window, makeFirstResponder: std::ptr::null::<AnyObject>()];
                    }
                }
            }
        }
        // Super dispatch names the class that DEFINED this override (OhMyTabSettingsWindow),
        // never the superclass: ObjcSuper derives the superclass from it, and naming NSWindow
        // here would start the search one level too high.
        type SendEvent = unsafe extern "C" fn(*mut crate::ffi::ObjcSuper, Sel, *mut AnyObject);
        let mut sup = crate::ffi::ObjcSuper::for_defined_class(
            _self as *mut c_void,
            window::settings_window_class_handle(),
        );
        let send: SendEvent = std::mem::transmute(crate::ffi::objc_msgSendSuper as *const ());
        send(&mut sup, sel!(sendEvent:), event);
    }
}

/// Override of performKeyEquivalent: on the settings window: intercept Cmd+Q while this
/// window is key. Command-combo dispatch goes through performKeyEquivalent: (key-window
/// responder chain before mainMenu). With the settings window open the app is active and the
/// window is key, so Cmd+Q is guaranteed to land here -- no reliance on mainMenu dispatch
/// (which doesn't work for status-bar menus on accessory apps, the earlier pitfall).
/// Other command combos fall through to super so text editing etc. keeps working.
extern "C" fn settings_window_perform_key_equivalent(
    _self: *mut AnyObject,
    _cmd: Sel,
    event: *mut AnyObject,
) -> objc2::runtime::Bool {
    crate::callback_guard::value(
        "settings_window_perform_key_equivalent",
        objc2::runtime::Bool::NO,
        || {
            unsafe {
                let keycode: u16 = msg_send![event, keyCode];
                let flags: u64 = msg_send![event, modifierFlags];
                if keycode == KEYCODE_Q && (flags & NSEVENT_MODIFIER_FLAG_COMMAND) != 0 {
                    // Same path as the menu Quit item: restore pointer acceleration + terminate.
                    crate::quit_from_settings();
                    return objc2::runtime::Bool::YES;
                }
                // Cmd+F focuses the sidebar search. The sidebar shell draws a `⌘F` hint, so the shortcut
                // has to be real: a hint for a binding the window does not answer would be a promise the
                // app breaks. Shift and option variants are left to super, as they belong to text editing.
                if keycode == KEYCODE_F
                    && (flags & NSEVENT_MODIFIER_FLAG_COMMAND) != 0
                    && (flags & (NSEVENT_MODIFIER_FLAG_SHIFT | NSEVENT_MODIFIER_FLAG_OPTION)) == 0
                    && sidebar::focus_settings_search()
                {
                    return objc2::runtime::Bool::YES;
                }
                // Super dispatch names the class that DEFINED this override (OhMyTabSettingsWindow),
                // never the superclass; see settings_window_send_event_inner.
                // The selector's encoding is "B" (Objective-C BOOL), so the callback and the super
                // dispatch both use objc2's Bool rather than a Rust bool.
                type PerformKeyEquivalent = unsafe extern "C" fn(
                    *mut crate::ffi::ObjcSuper,
                    Sel,
                    *mut AnyObject,
                )
                    -> objc2::runtime::Bool;
                let mut sup = crate::ffi::ObjcSuper::for_defined_class(
                    _self as *mut c_void,
                    window::settings_window_class_handle(),
                );
                let perform: PerformKeyEquivalent =
                    std::mem::transmute(crate::ffi::objc_msgSendSuper as *const ());
                perform(&mut sup, sel!(performKeyEquivalent:), event)
            }
        },
    )
}

#[cfg(test)]
mod mouse_page_lazy_fill_tests {
    use super::*;

    /// The two orderings the caller can present, plus the staleness rules. Filling the mouse page costs a
    /// device resolution and a mapping render, and it used to happen on EVERY settings open -- including
    /// the ones that open on General or Clipboard.
    #[test]
    fn the_mouse_page_is_filled_only_when_it_is_the_one_in_use() {
        let mouse = SETTINGS_MOUSE_PAGE_INDEX;
        let clipboard = SETTINGS_CLIPBOARD_PAGE_INDEX;
        // Opening FOR the mouse page: fill when the controls are new or stale...
        assert!(mouse_page_needs_fill(Some(mouse), clipboard, false, true));
        assert!(mouse_page_needs_fill(Some(mouse), clipboard, true, true));
        // ...and nothing when they already hold current data: that is the saving, and every input that
        // can change them (config reload, device hot-plug, content rebuild) marks the page dirty.
        assert!(!mouse_page_needs_fill(Some(mouse), clipboard, true, false));
        // Opening for another page: never fill, even when the mouse page is what is on screen right now
        // (it is about to be hidden -- this is the ordering that made reopening from the Mouse page pay).
        assert!(!mouse_page_needs_fill(Some(clipboard), mouse, false, true));
        // An in-place refresh: the page on screen decides, and only when its data is stale.
        assert!(mouse_page_needs_fill(None, mouse, true, true));
        assert!(!mouse_page_needs_fill(None, mouse, true, false));
        assert!(!mouse_page_needs_fill(None, clipboard, true, true));
        // A rebuilt window has fresh controls: not materialized even though the flag may look loaded.
        assert!(mouse_page_needs_fill(None, mouse, false, false));
    }
}

#[cfg(test)]
mod tests {
    use crate::smoke_verdict::smoke_runner;
    /// The coalescing contract: several rebuild requests in one run-loop turn queue ONE callback,
    /// and the scroll record survives that folding. If the slot arbitration is removed (every request
    /// queues its own callback), the second claim below returns true and this test fails -- which is
    /// exactly the defect that let a second, scroll-less rebuild jump the page back to the top.
    #[test]
    fn rebuild_requests_coalesce_into_one_turn_and_keep_the_scroll_record() {
        // Isolate the process-wide slot/record from any other test in this binary.
        super::release_rebuild_slot();
        *super::PENDING_PAGE_SCROLL.lock().unwrap() = None;

        assert!(
            super::claim_rebuild_slot(),
            "the first request takes the slot"
        );
        assert!(
            !super::claim_rebuild_slot(),
            "a request in the same turn must fold into the queued rebuild"
        );
        *super::PENDING_PAGE_SCROLL.lock().unwrap() = Some((SettingsPageId::Clipboard, 120.0));

        // The handler turns the record over exactly once...
        assert_eq!(
            super::take_rebuild_request(),
            Some((SettingsPageId::Clipboard, 120.0))
        );
        assert_eq!(super::take_rebuild_request(), None, "consumed once");
        // ...and opens the next turn, so a later edit still gets its own rebuild.
        assert!(
            super::claim_rebuild_slot(),
            "the slot is free again for the next run-loop turn"
        );
        super::release_rebuild_slot();
    }

    use super::{settings_ui_borrowed, with_settings_ui};

    /// The sidebar label is `t(title_key())`, and `t()` returns the key itself when no locale
    /// entry resolves it -- so a botched key is shown verbatim in the sidebar (regression: commit
    /// 89650ca shipped "settings.sidebar_button(SettingsPageId::General)" and the sidebar rendered
    /// it). Assert both the mapping and that every key resolves, so a raw key cannot come back
    /// unnoticed. The old GUI assertion compared the button title to `t(title_key())`, which is
    /// self-consistent and stayed green on the broken key.
    #[test]
    fn sidebar_title_keys_resolve_in_the_locales() {
        use super::SettingsPageId;
        let expected = [
            (SettingsPageId::General, "settings.sidebar_general"),
            (SettingsPageId::Switcher, "settings.sidebar_switcher"),
            (SettingsPageId::Mouse, "settings.sidebar_mouse"),
            (SettingsPageId::Clipboard, "settings.sidebar_clipboard"),
            (SettingsPageId::KeyBindings, "settings.sidebar_key_bindings"),
            (
                SettingsPageId::KeystrokeDisplay,
                "settings.sidebar_keystroke_display",
            ),
            (SettingsPageId::FinderMenu, "settings.sidebar_finder_menu"),
            (SettingsPageId::About, "settings.sidebar_about"),
        ];
        assert_eq!(
            expected.len(),
            super::SETTINGS_PAGE_COUNT,
            "every settings page must be covered here"
        );
        for (page, key) in expected {
            assert_eq!(page.title_key(), key, "the sidebar key for {page:?} moved");
            assert_ne!(
                crate::i18n::t(key),
                key,
                "{key} resolves to itself, so the sidebar would show a raw key"
            );
        }
    }

    #[test]
    fn borrow_probe_sees_an_active_settings_ui_borrow() {
        // The notification handlers defer on this probe instead of re-entering `with_settings_ui`
        // (a nested borrow panics inside an `extern "C"` callback, which aborts the app).
        assert!(!settings_ui_borrowed());
        with_settings_ui(|_| assert!(settings_ui_borrowed()));
        assert!(!settings_ui_borrowed());
    }

    #[test]
    fn pointer_accel_slider_clamps_and_rounds() {
        assert_eq!(pointer_accel_from_slider(0.0), 0.0);
        assert_eq!(pointer_accel_from_slider(0.6875), 0.69);
        assert_eq!(pointer_accel_from_slider(1.0), 1.0);
        assert_eq!(pointer_accel_from_slider(1.234), 1.23);
        // Bottom 0 and top 10 (the linear slider's ends); a negative value (unreachable in theory)
        // clamps to 0.
        assert_eq!(pointer_accel_from_slider(-0.42), 0.0);
        assert_eq!(pointer_accel_from_slider(99.0), MOUSE_ACCELERATION_MAX);
        assert_eq!(
            pointer_accel_from_slider(f64::NAN),
            crate::mouse::pointer::FALLBACK_ACCELERATION
        );
    }

    #[test]
    fn pointer_accel_readout_shows_two_decimals() {
        assert_eq!(pointer_accel_display(0.0), "0.00");
        // The 1.00 default is macOS's factory default for the mouse key (what the pointer felt like
        // before this setting existed).
        assert_eq!(pointer_accel_display(1.0), "1.00");
        assert_eq!(pointer_accel_display(0.6875), "0.69");
    }

    use super::{
        parse_settings_page_spec, settings_page_identity, settings_page_index, settings_page_name,
        SettingsPageId, SETTINGS_ABOUT_PAGE_INDEX, SETTINGS_CLIPBOARD_PAGE_INDEX,
        SETTINGS_KEYSTROKE_DISPLAY_PAGE_INDEX, SETTINGS_KEY_BINDINGS_PAGE_INDEX,
        SETTINGS_MOUSE_PAGE_INDEX, SETTINGS_PAGES, SETTINGS_PAGE_COUNT,
        SETTINGS_SWITCHER_PAGE_INDEX,
    };

    /// The table is the single source of truth for the sidebar order: each entry claims the index it
    /// actually occupies, the names are unique, and the page-index constants agree with their names.
    /// A reorder that forgets one of those silently opens the wrong page for a script.
    #[test]
    fn the_page_table_matches_the_sidebar_order_it_indexes() {
        let mut names = std::collections::HashSet::new();
        for (position, page) in SETTINGS_PAGES.iter().enumerate() {
            assert_eq!(settings_page_identity(position), page.id);
            assert!(names.insert(page.name), "duplicate page name {}", page.name);
            assert_eq!(settings_page_name(position), page.name);
        }
        assert_eq!(names.len(), SETTINGS_PAGE_COUNT);
        // The named constants are what the rest of the app navigates by; each must land on the page
        // its name promises, or the switch and the overlay agree on a wrong number.
        assert_eq!(
            settings_page_name(SETTINGS_KEY_BINDINGS_PAGE_INDEX),
            "key-bindings"
        );
        assert_eq!(
            settings_page_name(SETTINGS_KEYSTROKE_DISPLAY_PAGE_INDEX),
            "keystroke-display"
        );
        assert_eq!(settings_page_name(SETTINGS_MOUSE_PAGE_INDEX), "mouse");
        assert_eq!(
            settings_page_name(SETTINGS_CLIPBOARD_PAGE_INDEX),
            "clipboard"
        );
        assert_eq!(settings_page_name(SETTINGS_SWITCHER_PAGE_INDEX), "switcher");
        assert_eq!(settings_page_name(SETTINGS_ABOUT_PAGE_INDEX), "about");
    }

    /// The table and the identity list are two hand-written orders that must agree, and the identity
    /// is what every control array is keyed by. This is the counter-example the first review asked
    /// for: a table whose order no longer matches the identities would still pass the name/index
    /// tests, but this one fails because the identity at each index is wrong.
    #[test]
    fn the_page_identities_match_the_table_at_every_index() {
        let mut seen = std::collections::HashSet::new();
        for (index, page) in SETTINGS_PAGES.iter().enumerate() {
            // The identity at an index is the one the table put there, and it round-trips. A
            // control array keyed by identity therefore cannot be indexed by a different page.
            assert_eq!(settings_page_identity(index), page.id);
            assert_eq!(settings_page_index(page.id), index);
            assert!(seen.insert(page.id), "{:?} appears twice", page.id);
        }
        // Every identity appears exactly once, so the table names every page.
        assert_eq!(seen.len(), SETTINGS_PAGE_COUNT);
        // The identities are distinct pages, not interchangeable numbers.
        assert_ne!(
            settings_page_index(SettingsPageId::Switcher),
            settings_page_index(SettingsPageId::Clipboard)
        );
        assert_ne!(SettingsPageId::General, SettingsPageId::About);
    }

    /// Every page must be reachable by its name, and nothing that names no page may resolve. The
    /// rejected values are the counter-examples: a typo and an out-of-range index used to be silent
    /// no-ops or a clamp onto a different page, which is exactly what a script cannot detect.
    #[test]
    fn the_page_switch_names_every_page_and_refuses_the_rest() {
        for (index, page) in SETTINGS_PAGES.iter().enumerate() {
            assert_eq!(parse_settings_page_spec(page.name), Some(index));
            // Case and surrounding whitespace are normalized, because the switch arrives as typed.
            assert_eq!(
                parse_settings_page_spec(&format!("  {}  ", page.name.to_uppercase())),
                Some(index)
            );
        }
        // A bare switch and the boolean truthy spellings keep their pre-table meaning: About.
        for spec in ["", "1", "true", "yes", "TRUE", " about "] {
            assert_eq!(
                parse_settings_page_spec(spec),
                Some(SETTINGS_ABOUT_PAGE_INDEX)
            );
        }
        // The historical alias, kept from the switch's first form.
        assert_eq!(
            parse_settings_page_spec("bindings"),
            Some(SETTINGS_KEY_BINDINGS_PAGE_INDEX)
        );
        // A raw index inside the range is a page; one past either end names nothing.
        assert_eq!(parse_settings_page_spec("0"), Some(0));
        assert_eq!(
            parse_settings_page_spec(&(SETTINGS_PAGE_COUNT - 1).to_string()),
            Some(SETTINGS_PAGE_COUNT - 1)
        );
        assert_eq!(
            parse_settings_page_spec(&SETTINGS_PAGE_COUNT.to_string()),
            None
        );
        assert_eq!(parse_settings_page_spec("99"), None);
        // A near miss must not resolve to a different page.
        for spec in ["key-binding", "keybindings", "keystroke", "generall"] {
            assert_eq!(
                parse_settings_page_spec(spec),
                None,
                "{spec} must not resolve"
            );
        }
    }

    /// The A2 view-root names are what `scripts/e2e/*.sh` look up by, so they are derived from the
    /// same table the switch uses. This pins the spelling without needing a live window: the root
    /// name is `sidebar_<i>_<name>` / `page_<i>_<name>`.
    #[test]
    fn the_view_root_names_follow_the_page_table() {
        let (sidebar, page) = super::view_root_names();
        let roots: Vec<&str> = sidebar
            .iter()
            .chain(page.iter())
            .map(String::as_str)
            .collect();
        // Asserted through the name-keyed map the scripts actually read, not a hand-written list: a
        // page reorder moves the strings AND the map together, so this cannot pass while a script's
        // lookup would break. Every published root is one of the derived strings.
        for (index, (name, sidebar_root, page_root)) in
            super::e2e_root_names_by_page().into_iter().enumerate()
        {
            assert_eq!(name, super::settings_page_name(index));
            assert_eq!(sidebar_root, sidebar[index]);
            assert_eq!(page_root, page[index]);
            let stem = name.replace('-', "_");
            assert_eq!(sidebar_root, format!("sidebar_{index}_{stem}"));
            assert_eq!(page_root, format!("page_{index}_{stem}"));
            assert!(roots.contains(&sidebar_root.as_str()));
            assert!(roots.contains(&page_root.as_str()));
        }
        // The pages the A2 scripts anchor on must be published by NAME, so a reorder cannot drop one
        // silently.
        for expected in [
            "general",
            "mouse",
            "key-bindings",
            "keystroke-display",
            "about",
        ] {
            assert!(
                super::e2e_root_names_by_page()
                    .iter()
                    .any(|(name, _, _)| *name == expected),
                "no published root for page {expected}"
            );
        }
    }

    use super::settings_effective_corner_radius;
    use super::{pointer_accel_display, pointer_accel_from_slider};
    use crate::config::MOUSE_ACCELERATION_MAX;

    #[test]
    fn mapping_row_interactive_views_match_the_registry_contract() {
        // Registration (mapping::update_mapping_controls_enabled) and unregistration
        // (mapping::render_mapping_rows_locked) share MappingRow::interactive_views. It must
        // cover every view that takes part in availability registration and exclude the
        // never-registered separator: a missing view leaves a dangling registry address that
        // crashes the next settings click.
        let p = |n: usize| n as *mut objc2::runtime::AnyObject;
        let row = super::MappingRow {
            label: p(1),
            desc_label: p(2),
            action_icon: p(3),
            edit: p(4),
            delete: p(5),
            separator: p(6),
            caps: vec![p(7), p(8)],
        };
        assert_eq!(
            row.interactive_views()
                .map(|view| view as usize)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 7, 8]
        );
    }

    #[test]
    fn settings_corner_radius_uses_the_largest_effective_corner() {
        assert_eq!(
            settings_effective_corner_radius(Some([18.0, 26.0, 20.0, 24.0]), 12.0),
            26.0
        );
    }

    #[test]
    fn settings_corner_radius_falls_back_for_missing_or_invalid_values() {
        assert_eq!(settings_effective_corner_radius(None, 26.0), 26.0);
        assert_eq!(
            settings_effective_corner_radius(Some([18.0, f64::NAN, 20.0, 24.0]), 26.0),
            26.0
        );
    }

    /// Exercise the real settings window on the AppKit main thread. This cannot run in the
    /// normal headless suite because NSWindow construction is GUI/session dependent.
    /// The same layout smoke with the Reduce Motion ask on: a switch state change must take the
    /// no-animation branch, which used to send `[CALayer layer]` and trap the process.
    #[test]
    #[ignore]
    fn settings_layout_smoke_under_reduce_motion() {
        smoke_runner!("settings_layout_smoke_under_reduce_motion", 1, {
            crate::smoke_verdict::run_smoke_child(
                "settings_layout_smoke_under_reduce_motion",
                &["--smoke-settings-layout", "--reduce-motion"],
            );
        });
    }

    #[test]
    #[ignore]
    fn settings_vertical_zoom_smoke() {
        smoke_runner!("settings_vertical_zoom_smoke", 1, {
            // The child is the process that ran the assertions; it records the verdict
            // through the shared channel and this parent reports what it recorded.
            crate::smoke_verdict::run_smoke_child(
                "settings_vertical_zoom_smoke",
                &["--smoke-settings-vertical-zoom"],
            );
        });
    }

    /// The Finder Menu page's own A1 check. Separate from the all-pages layout smoke because that
    /// runner stops on an earlier, pre-existing clipboard assertion (docs/review-backlog.md entry
    /// 41) and would never reach any page's own assertions.
    #[test]
    #[ignore]
    fn finder_menu_page_smoke() {
        smoke_runner!("finder_menu_page_smoke", 1, {
            // The child is the process that ran the assertions; it records the verdict
            // through the shared channel and this parent reports what it recorded.
            crate::smoke_verdict::run_smoke_child(
                "finder_menu_page_smoke",
                &["--smoke-finder-menu"],
            );
        });
    }

    #[test]
    #[ignore]
    fn settings_layout_smoke() {
        smoke_runner!("settings_layout_smoke", 1, {
            // The child is the process that ran the assertions; it records the verdict
            // through the shared channel and this parent reports what it recorded.
            crate::smoke_verdict::run_smoke_child(
                "settings_layout_smoke",
                &["--smoke-settings-layout"],
            );
        });
    }

    /// Call the registered NSApplication reopen delegate and verify custom handling opens Settings.
    #[test]
    #[ignore]
    fn application_reopen_delegate_opens_settings_smoke() {
        smoke_runner!("application_reopen_delegate_opens_settings_smoke", 1, {
            // The child is the process that ran the assertions; it records the verdict
            // through the shared channel and this parent reports what it recorded.
            crate::smoke_verdict::run_smoke_child(
                "application_reopen_delegate_opens_settings_smoke",
                &["--smoke-app-reopen"],
            );
        });
    }

    /// Rebuilding settings content must not reset controls to their construction defaults.
    #[test]
    #[ignore]
    fn settings_state_sync_after_content_rebuild_smoke() {
        smoke_runner!("settings_state_sync_after_content_rebuild_smoke", 1, {
            // The child is the process that ran the assertions; it records the verdict
            // through the shared channel and this parent reports what it recorded.
            crate::smoke_verdict::run_smoke_child(
                "settings_state_sync_after_content_rebuild_smoke",
                &["--smoke-settings-state-sync"],
            );
        });
    }

    /// Hiding a conditional clipboard row must preserve adjacent rows and close the layout gap.
    #[test]
    #[ignore]
    fn settings_collapsible_row_geometry_smoke() {
        smoke_runner!("settings_collapsible_row_geometry_smoke", 1, {
            // The child ran the geometry assertions; take the verdict it recorded.
            crate::smoke_verdict::run_smoke_child(
                "settings_collapsible_row_geometry_smoke",
                &["--smoke-settings-collapsible-row"],
            );
        });
    }

    /// Mouse profile edits must leave the RefCell borrow before runtime refresh re-enters settings.
    #[test]
    #[ignore]
    fn settings_mouse_profile_callback_smoke() {
        smoke_runner!("settings_mouse_profile_callback_smoke", 1, {
            // The child is the process that ran the assertions; it records the verdict
            // through the shared channel and this parent reports what it recorded.
            crate::smoke_verdict::run_smoke_child(
                "settings_mouse_profile_callback_smoke",
                &["--smoke-settings-mouse-profile-callback"],
            );
        });
    }

    /// Create a pixel-based continuous CGEvent and round-trip every field used by smooth scroll.
    #[test]
    #[ignore]
    fn smooth_scroll_synthetic_event_fields_smoke() {
        smoke_runner!("smooth_scroll_synthetic_event_fields_smoke", 1, {
            // The child is the process that ran the assertions; it records the verdict
            // through the shared channel and this parent reports what it recorded.
            crate::smoke_verdict::run_smoke_child(
                "smooth_scroll_synthetic_event_fields_smoke",
                &["--smoke-smooth-scroll-event"],
            );
        });
    }
}
