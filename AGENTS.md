# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) and OpenCode when working with code in this repository.

## What this is

`oh-my-tab` is a macOS app/window switcher (an alternative to the system Cmd+Tab). It runs as an **accessory** app (no Dock icon; lives in the menu bar), intercepts a global shortcut (default **Command+Tab**, toggleable to Option+Tab), shows a floating "Liquid Glass" overlay of cards for currently-open windows, and raises the selected window on release using the Accessibility (AX) API. It also ships a **mouse enhancement** module: scroll-mode control (default passthrough/reverse vs. fixed-line), per-device pointer-acceleration control, and per-device profiles matching by VID/PID.

It is Rust calling AppKit/CoreGraphics/ApplicationServices directly via `objc2` FFI — there is no Swift bridge and no Rust UI framework.

## Build & run

```sh
cargo build        # or cargo check for fast type-checking
cargo run          # build + run (takes over the global shortcut)
cargo clippy       # not configured in CI, but available
cargo fmt          # auto-format; run after every code change
cargo test         # unit tests (fast, headless-safe; smoke tests are #[ignore])
```

**After every code modification, run (in this order, commands lowercase):**
`cargo fmt` → `cargo check` → `cargo clippy` → `cargo test`. All must pass cleanly before the change is considered done.

**Testing strategy** (see also the `.github/workflows/ci.yml`):
- The test suite is **text-assertion based** — no screenshots/visual regression. Layout/color/branch logic is extracted into pure functions (`compute_window_height`, `resolve_locale_from`, `sort_windows_by_mru`, `parse_bluetooth_info`, etc.) and verified with `assert_eq!`.
- Unit tests cover: config parse/validate/merge/migration + save/load roundtrips (via `tempfile`), i18n locale resolution + cross-locale key-set consistency, mouse profile merging, scroll delta math, Bluetooth NVRAM TLV parsing, MRU sort/prune, theme layout math, logger cleanup, clipboard history logic (dedup-to-front, pin/unpin, delete).
- **Smoke tests** (`window_collector::tests::*_smoke`, `settings::tests::*`, `clipboard::tests::*`, `#[ignore]`) exercise the real CG/AX stack and need a GUI session + Accessibility grant; run with `cargo test -- --ignored`. CI skips them. Note the icon-cache smoke clears only the test cache dir (`oh-my-tab-icons-test`) — never the user's real cache.
- `tempfile` is a `[dev-dependencies]`-only crate — never linked into the release binary.

**Permissions & runtime caveats:**
- The app requires **Accessibility** permission (`AXIsProcessTrusted`) for both the global key event tap and AX window queries. Grant it under System Settings → Privacy & Security → Accessibility. A freshly built binary at a new path must be re-granted.
- If the event tap fails to create, the app prints an error and the shortcut silently does nothing — almost always a missing Accessibility grant.
- Runtime config: `~/.config/oh-my-tab/config.toml` (auto-created with defaults on first run).
- Icon cache: `~/Library/Caches/oh-my-tab-icons/{bundle-id}.png` (keyed by the app's bundle identifier, plus a `{bundle-id}.meta` sidecar storing the executable mtime so an app update forces a re-extract).

## Architecture

Top-level modules: `main` (UI, ObjC classes, orchestration), `event_monitor` + `event_tap` (global-shortcut tap; `event_tap` is the shared CGEventTap/CFRunLoop leaf used by both the switcher and the mouse module), `window_collector`, `overlay`, `settings`, `menu`, `config`, `i18n`, `theme`, `autostart`, `clipboard`, `logger`, `ffi` (CF/ObjC primitives), and `mouse` — a submodule hub (`src/mouse.rs`) with `device` / `event_tap` / `ffi` / `pointer` / `resolve` / `scrolling`. The tricky parts span several files, so the breakdown below is load-bearing.

### Event flow & threading (spans all files)
1. `event_monitor::start` spawns a **dedicated thread** running a `CGEventTap` + `CFRunLoop`. It detects Tab+modifier-down (`CmdTabPressed`) and modifier-release (`CmdReleased`), and sends `GlobalEvent`s over a `flume` channel. The active shortcut (Cmd vs Opt) is the `SHORTCUT_IS_CMD` atomic, toggleable from the menu. `Option+V` is also detected here (`ClipboardToggled`); when the clipboard feature is disabled the combo is passed through untouched so other apps can use it.
2. A **bridge thread** receives `GlobalEvent`s from flume and marshals each to the main thread via `performSelectorOnMainThread:` on the controller object (`OhMyTabController`).
3. The **main thread** runs `NSApplication.run` and owns all UI: ObjC callbacks (`on_cmd_tab_pressed`, `on_cmd_released`, card/container view methods) build/refresh/show the overlay.

Shared state is global `static`s guarded by `Mutex`/`RwLock`: `TAB_STATE` (the windows + selection + MRU), the ObjC object pointers (`CONTROLLER`/`OVERLAY_WINDOW`/`CONTAINER`/`STATUS_LABEL`), `CARD_INDEX_MAP`, `CONFIG`, and `LAST_ACTIVATED` (in `window_collector`). Raw ObjC pointers are wrapped in `ObjPtr`/`ObjClassPtr` with manual `Send`/`Sync`.

### `main.rs` — UI, ObjC classes, orchestration
- Custom ObjC classes (`OhMyTabCardView`, `OhMyTabContainerView`, `OhMyTabController`, menu target) are **registered at runtime** via `objc_allocateClassPair` + `class_addMethod`, not declared in a bridge. Method impls are `extern "C" fn`s.
- **Some calls must use raw `objc_msgSend` FFI** because `objc2`'s `msg_send!` can't encode CF/CG types or (historically) void returns. See `hex_to_cg_color`, `layer_set_background`, `layer_set_border`, and `activate_pid`. When adding AppKit calls that touch `CGColorRef` or return void, follow those patterns.
- **Card ↔ index mapping**: dynamically-registered classes can't expose properties through `msg_send!` reliably, so each card view's index is stored in `CARD_INDEX_MAP` keyed by view pointer (not as an ObjC property). `get_card_index`/`set_card_index`/`remove_card_index` manage it.
- **macOS version split**: macOS 26+ renders the overlay with `NSGlassEffectView` (Liquid Glass); older macOS falls back to `NSVisualEffectView` (withinWindow + Dark). Detected via `AnyClass::get(c"NSGlassEffectView")`.
- **Hover gating**: `MOUSE_MOVED` prevents the card under the cursor from being selected when the overlay first appears; the user must move the mouse first (matches native Cmd+Tab).
- Card views are rebuilt by `show_overlay` (full rebuild on summon) and **surgically by `rebuild_cards`** (in-place swap of individual cards, used after icons get extracted mid-session so icons appear without re-summoning).

### `window_collector.rs` — window enumeration, icons, raising
- `collect_windows` is the core. It takes `CGWindowListCopyWindowInfo` (on-screen, `kCGWindowLayer == 0`) and cross-references each with the per-PID **AX window list** (`AXUIElement`). **AX is authoritative**: a CG window must correspond to an AX `AXStandardWindow` to be kept (this is how popups/panels/dropdowns are filtered out).
- **Titleless windows**: some apps (e.g. Microsoft To Do) have a custom title bar and expose an **empty `AXTitle`** plus an empty `kCGWindowName`. Such windows are kept via `titleless_pids` (apps whose AX windows are *all* untitled). Their internal `window_title` stays empty (so `raise_ax_window` can still match the AX window by its empty title) and only the *display* layer substitutes a placeholder via `display_title()` (`"-"`). Do not fill the stored title with the placeholder — it would break window targeting.
- **Icon cache** is keyed by the app's **bundle identifier** (falling back to the executable path for non-bundle apps), NOT by PID -- this prevents PID recycling from serving another app's stale icon (the old `{pid}.png` design did exactly that). Each entry has a `{bundle-id}.meta` sidecar storing the executable's mtime; a mismatch (app updated/reinstalled) forces a re-extract, so no TTL is needed. Non-bundle apps without an executable URL fall back to a `pid_{pid}` key with no verification. Identity resolution happens in `resolve_app_identity` (one `NSRunningApplication` lookup + stat per pid, batched in `collect_windows`'s first pass). Icons are pre-cached at startup (`cache_running_app_icons`) and on app launch (`NSWorkspaceDidLaunchApplicationNotification`, extracted on a background thread with an autorelease pool). `extract_uncached_icons` (runs on 2nd+ Tab while visible) fills any still-missing icons and calls `rebuild_cards`. Legacy `{pid}.png` files are migrated away once at startup (`migrate_legacy_cache`).
- **MRU ordering**: `mru` (window_id → Instant) is updated on each switch; `LAST_ACTIVATED` (pid → Instant) is fed by `NSWorkspaceDidActivateApplicationNotification` so switches made via Dock/system Cmd+Tab are reflected.
- `raise_ax_window(pid, title)` focuses a specific AX window by matching `AXTitle`, preceded by `activate_pid` (`NSRunningApplication activateWithOptions:`).

### `config.rs` — TOML config, validated, reloadable
- Global singleton `CONFIG: LazyLock<RwLock<Config>>` at `~/.config/oh-my-tab/config.toml`.
- Loading is **per-field resilient**, not all-or-nothing: `validate()` collects errors, then `merge_valid()` keeps loaded values for valid fields and resets invalid ones to defaults. Config errors are logged, never fatal.
- Reloadable at runtime via the menu ("Reload Config" → `reload_config`), which also re-applies theme and refreshes the overlay.
- Color fields are 8-hex-digit `RRGGBBAA` strings; `parse_hex8` converts them. Many layout/font values are read live from `CONFIG` inside the UI helpers (e.g. `card_w()`, `current_colors()`), so config changes take effect on the next render.
- **Mouse section**: `mouse.enabled` (master switch) and `mouse.profiles` — per-device profiles keyed by VID/PID (`device.vendor_id`/`product_id`), each with `scroll_mode` / `line_count` / `reverse` / pointer-acceleration fields. Validated with the same per-field resilient pattern; legacy flat mouse fields are migrated via `MouseSection::migrate_legacy()`. Config matches devices by hardware identity (VID+PID), never by PID or name.
- **Clipboard section**: `clipboard.enabled` (master switch, default false) and `clipboard.max_entries` (1..=100, default 50). `clipboard::start()`/`stop()` hot-switch the polling timer from Settings; the disabled switch also gates summoning and recording (see `clipboard.rs`).

### `mouse/` submodule — device registry, attribution, scroll & acceleration
- **Module hub** (`mouse.rs`): `start()`/`stop()` idempotently manage the dedicated **mouse thread** (its own CGEventTap + CFRunLoop; taps are per-thread, so it cannot share the switcher's tap). `mouse::event_tap` is the tap/runloop implementation; `mouse::ffi` centralizes the IOKit private SPI.
- **Device registry & attribution chain** (`device.rs`): `device_from_cgevent` resolves which device produced an event: `CGEventCopyIOHIDEvent` -> `IOHIDEventGetSenderID` -> `IOHIDEventSystemClientCopyServiceForRegistryID` -> `CFEqual` against the enumerated list. The `IOHIDEventSystemClient` must be **scheduled on the mouse thread's runloop** or the registry-ID map is incomplete and attribution silently falls back to "All Mice" (this is why the runloop is recorded at startup). On a miss, the client is force-rebuilt and re-enumerated once (a stale client's cache dies on Bluetooth reconnect); still missing -> `LAST_ACTIVE_KEY` (VID/PID, stable across reconnects) -> "All Mice" profile.
- **Enumeration filter**: pointer/mouse/trackpad is decided by `IOHIDServiceClientConformsTo` over the full `DeviceUsagePairs`, NOT the `PrimaryUsage` scalar — real mice (e.g. ATK A9 SE Nearlink) can report PrimaryUsage = Keyboard(6), and a `{1,2,5}` whitelist would drop them.
- **Bluetooth keyboard exclusion (load-bearing)**: some keyboard firmware *fakes* pointer usages in its HID descriptor (e.g. KZI I75 declares full Mouse collections), so the ConformsTo filter alone admits keyboards. The fix reads the device's **GAP Appearance** from bluetoothd's `BluetoothInfo` cache in NVRAM (`IOService:/options`, private TLV: tag `0x0e` = BT address, tag `0x11` = appearance **little-endian**, 0x03C1 = keyboard) via `bluetooth_appearance_map()`, matched by the HID service's `DeviceAddress` property. Appearance = keyboard -> device excluded from the registry. Devices absent from the NVRAM cache (freshly paired) or non-Bluetooth fall back to HID-only classification — never false-positively dropped.
- **Plug/unplug monitor**: `start_plug_monitor` uses an `IOHIDManager` with **separate matching/removal callbacks** (direction matters). The 500ms debounce (`LAST_PLUG_HANDLE`) breaks the startup callback burst, but skipped events are **not dropped**: they schedule a one-shot delayed recheck (~700ms, `schedule_recheck`/`run_recheck`) that cheap-diffs the device set with the existing client and only rebuilds + re-applies + notifies the UI when it changed. A matching event right after a removal (the BLE sleep-wake pattern) sets the force flag: the stale client makes the cheap diff unreliable, so a full rebuild is forced. This is what makes a reconnected Bluetooth mouse reappear in the settings picker without reopening.
- **UI notification**: `notify_devices_changed()` (shared by the plug callback, the delayed recheck, and the attribution self-heal) hops to the main thread via `performSelectorOnMainThread: handleDevicesChanged:`; settings rebuilds the device popup live (`refresh_device_popup_if_open`, `rebuild_device_popup`). The device list is external live state and is NOT OK/Cancel-gated; the popup selection is restored from in-memory `SELECTED_DEVICE` (`ensure_selected_device` recalibrates it against the connected list on each settings open).
- **Pointer acceleration** (`pointer.rs`): acceleration properties live on the `IOHIDServiceClient` instance, so a Bluetooth disconnect wipes them; `apply()` must re-run on every plug event / delayed recheck / attribution self-heal. Idempotent, checks `mouse.enabled` internally.
- **Scrolling** (`scrolling.rs`): two modes — Default (passthrough, optionally reversed per device) and Line (fixed line count). **Resolve** (`resolve.rs`): effective per-device config = "All Mice" base profile merged with the matched device profile (by VID/PID).

### `clipboard.rs` — history clipboard (text + images, Option+V)
- **Recording**: a main-thread `NSTimer` polls `NSPasteboard.changeCount` every 0.5s, plus a process-wide `NSPasteboardDidChangeNotification` observer for instant capture (polling alone skips rapid consecutive copies between samples). Both honor `clipboard.enabled` — the master switch gates recording AND summoning (`Option+V` is passed through untouched when disabled, so other apps can use the combo).
- **History model**: `CLIP_HISTORY: Mutex<Vec<ClipEntry>>` (`ClipEntry { text, pinned, source, image }`), newest first, pinned entries always on top (100 entries max). **Global dedup-to-front**: re-copying existing content moves the old entry to the front (keeping its pinned state — pinned entries go to the top of the pinned block), so the list never holds duplicates. Pure helpers (`record_text`, `find_by_text`, `move_entry_to_front`, `pin_entry`/`unpin_entry`, `delete_entry`) are unit-tested.
- **Image memory model (load-bearing)**: image-DATA entries **never hold the original bytes in memory** — recording hashes the bytes (FNV-1a) and writes them to a disk cache (`~/Library/Caches/oh-my-tab-clip-images/{hash:016x}`, plus a `{hash}.preview` PNG for the thumbnail); the entry keeps only `uti`, `hash`, the cache `data_path`, and a **downsampled (~480px)** PNG preview (the only image bytes resident in RAM). Pasting reads the bytes back from disk on demand. Lifecycle: when `clipboard.persist` is OFF (default) the cache dir is wiped at `start()` (the history is not persisted, leftovers are orphans — swept BEFORE the initial poll); when ON it is kept and the history is loaded instead. Cache files are deleted in sync with `delete_entry`, "clear all" (kept: pinned), and the `truncate(max)` trim in both `record_text` and `record_image`. Test builds use a per-thread test cache dir (`oh-my-tab-clip-images-test-{thread}`) so parallel tests can't race each other's wipe/delete.
- **Persistence (load-bearing, `clipboard.persist`, default OFF)**: when enabled, the history is saved to `~/.config/oh-my-tab/clipboard-history.toml` (serde+toml, atomic temp+rename, **mode 600, plaintext — privacy risk documented in the README**) after every mutation (poll, pin/unpin, delete, clear-all) and loaded at `start()` (merge into memory via dedup rules; pinned entries join the pinned block, the rest append at the tail; trimmed to `max_entries`). The TOML skips `preview_png`/`data_path` (the former is restored from `{hash}.preview`, the latter rebuilt from the hash; a data entry whose cache file is gone is dropped; a future `version` is rejected). `settings.rs` hot-switches it via `apply_persist_toggle` (ON → load+merge, OFF → delete the file).
- **Sensitive-marker interception (load-bearing)**: content stamped with the nspasteboard.org "Securing Copy" markers (`org.nspasteboard.TransientType` / `ConcealedType` / `AutoGeneratedType`, `com.agilebits.onepassword`) is NEVER recorded — checked via one `availableTypeFromArray:` probe in `poll_clipboard` before anything is read (matches Maccy; password managers stamp these on password copies).
- **File-copy entries (load-bearing)**: a Finder file copy is read ONCE at record time (transiently) for a content hash + a downsampled thumbnail preview; the bytes are then DISCARDED (no data-cache write, no shadow copy — data_path stays empty; same reference semantics as Windows Win+V / Maccy). `record_image` dedups file entries by content hash among file entries (a file and its Finder duplicate — different paths, identical bytes — collapse into one entry, with `source_path` updated to the latest copy) and data entries by hash among data entries (never across classes); a degenerate entry whose decode failed (hash=0) dedups by path. The entry's `text` holds the filename (shown in the row, searchable). Pasting restores `public.file-url` + filename; if the source file is gone, the paste is **skipped** (no bytes to fall back to).
- **Paste semantics (load-bearing)**: file-copy entries restore `public.file-url` + filename on paste (file semantics, mirrors Windows Win+V — Finder duplicates the original file, chat apps attach it, GIF animation intact). Image-data entries paste the original UTI verbatim (JPG → JPG, GIF → GIF, never a PNG re-encode). File-copy entries show a thumbnail in the row (decoded at record time; the empty-preview fallback shows the filename text). A failed write-back (e.g. cache miss) skips the synthesized Cmd+V so stale pasteboard content is never pasted.
- **The picker**: a `NSPanel` (nonactivating) following the cursor (bottom-right offset, flips/clamps to the screen), rows with multi-line text (max 3 lines) or a thumbnail, a pin button and a delete button (Backspace SF Symbol) per row, arrow/Enter/Esc/Backspace navigation, wheel scrolling with a custom indicator, "Clear all" (keeps pinned entries), and an empty-state hint. Clicking outside (resign-key) hides it.
- **Paste (load-bearing)**: `paste_at` must **close the picker BEFORE synthesizing Cmd+V** (`CGEventCreateKeyboardEvent` + `CGEventPost` at session level) — while the panel is the key window, a synthesized key event routes to our own app and never reaches the user's input field. The pasteboard write-back is then deduped away by the history logic.
- **Smoke tests**: `clipboard::tests::picker_rebuild_smoke` runs the real binary via `--smoke-clipboard` (injects entries, drives keyboard navigation with real NSEvents, scrolls) to cover rebuild/cleanup paths that crashed historically (UAF, deadlocks, msg_send encoding panics).

### `logger.rs` — two-tier logging + stderr capture
- Only two levels: Debug (diagnostic detail) / Info (normal runtime info); errors/warnings all go through `log_info!` (content preserved, no separate tiers). Macros (`log_debug!`/`log_info!`) funnel into a bounded flume channel; the writer thread prints to stdout in dev mode and appends to `~/Library/Logs/oh-my-tab/oh-my-tab-*.log` (30-day cleanup; user-supplied `log.file_path` is used verbatim, never pruned).
- **stderr capture (load-bearing)**: at init, stderr (fd 2) is redirected to a pipe and a reader thread hands each line to the log pipeline at **Info** level with a `[stderr]` prefix. This is what makes NSLog/AppKit internal warnings (e.g. `[Menu_Tracking]` messages) and Rust panics visible in the app's own log instead of only in the terminal. Do not localize log lines (`println!`/`eprintln!` stay English).

### `i18n.rs` - internationalization (handcrafted TOML, zero deps)
- A handcrafted TOML-based i18n system, deliberately DIY (not `rust-i18n`/`fluent`) to keep dependencies minimal and stay isomorphic with `config.rs`. Translation files are embedded at compile time via `include_str!` from `locales/{en,zh-Hans,zh-Hant}.toml` (no runtime file IO, no missing-file risk).
- **Locale flow**: driven by `config.i18n.locale` (`"auto"` | `"en"` | `"zh-Hans"` | `"zh-Hant"`, default `"auto"`). Resolution priority: config value (non-`auto` & supported) > first matching entry in the system `NSLocale preferredLanguages` list (scanned **in order**, so a supported language lower in the user's preference beats the default - e.g. `[ja, zh-Hans, en]` -> `zh-Hans`) > `"en"`. Chinese tags split by script/region: `Hant` / `TW` / `HK` / `MO` -> `zh-Hant`; everything else (incl. bare `zh`, `CN`, `SG`) -> `zh-Hans`.
- **API**: `t(key) -> String` (current locale -> en fallback -> key itself) and `tf(key, &[("name","value")])` for templates with `{name}` placeholders. Locale TOML uses `[section]` groups, flattened at load to dot keys (e.g. `[menu]` `settings` -> `menu.settings`). Keys: `[menu]` / `[settings]` / `[alert]` / `[errors]`.
- **Hot-reload**: `config.rs::reload_config()` and the `CONFIG` LazyLock init both call `i18n::apply_config_locale()`. `main.rs::refresh_menu_titles()` re-titles all 6 menu items (theme + shortcut labels are state-dependent; settings/reload/clear_cache/quit are static, stored in `FIXED_MENU_ITEMS`); `invalidate_settings_window()` releases the cached settings window so it rebuilds with the new locale on next open. Both run from `handle_reload_config`, `apply_config_refresh`, and `on_locale_changed`.
- **Live system-language follow**: `on_locale_changed` observes `NSLocaleCurrentLocaleDidChangeNotification` on the default `NSNotificationCenter`; when the system language changes and `config.i18n.locale` is `auto`, it re-resolves and refreshes the UI (explicit locales short-circuit). It self-marshals to the main thread via `performSelectorOnMainThread` since the notification's delivery thread isn't guaranteed.
- **No-cycle constraint (load-bearing)**: `i18n.rs` NEVER reads `CONFIG` (only `NSLocale`). This is required because `CONFIG`'s LazyLock init calls `validate()` -> `tf()` -> `I18N` init; if `I18N` read `CONFIG` it would deadlock. Do not break this.
- **Add a language**: create `locales/xx.toml` (same keys), register it in `i18n::locale_raw()`, add it to the `validate()` allow-list in `config.rs`, and extend `map_tag_to_supported()` if `auto` should map a system tag to it.
- Validation errors from `config.rs::validate()` are themselves localized via `tf()` (they surface in `show_alert`).

## Commit Messages

**Never commit or push.** Do not run `git commit`, `git push`, or any command that creates commits, rewrites history, or pushes to a remote - the user handles all git write operations themselves. When asked, only *generate the commit message text*; never stage (`git add`) or execute it on the user's behalf.

When asked for a commit message, always follow the [Conventional Commits](https://www.conventionalcommits.org/) specification and keep it to a single line. No body, no bullet points, no footers.

Format: `type: description`

Types: feat, fix, docs, style, refactor, perf, test, chore, ci, build

Example: `feat: implement compact floating window selector with auto-resize`

## Code Comments

Add comments at key locations: non-obvious logic, important design decisions, FFI/ObjC bridging subtleties, and any workaround whose reason isn't obvious from the code. Comments must be bilingual - Chinese first, then English on the following line(s) - mirroring the existing style in the codebase. Example:

```rust
// 只保留标准窗口，过滤弹出面板/下拉菜单等非标准窗口
// Only keep AXStandardWindow, filtering out popups/panels/dropdowns
```

## Internationalization

- **All user-visible strings go through `t()` / `tf()`** in `i18n.rs` - never hardcode Chinese or English literals in UI code (menu titles, settings window labels, alert titles/buttons, validation errors). Add the key to **both** `locales/en.toml` and `locales/zh-Hans.toml` and reference it via `t("section.key")` (or `tf("section.key", &[("name","value")])` when interpolation is needed).
- **Do NOT localize**: `println!`/`eprintln!` log lines (developer-facing, stay English) and `status_text` (the dynamic app/window title shown under the overlay - that's data, not UI chrome).
- UI strings are single-language per locale. Settings labels are pure Chinese in `zh-Hans`/`zh-Hant` (no embedded English config-key hints) and plain friendly names in `en`; format hints like `(RRGGBBAA)` on hex input fields are kept across all locales. This is a per-locale wording choice captured in the TOML, not a code concern.
- Comments stay bilingual Chinese-first then English (see Code Comments above); that convention is about code comments, separate from UI-string localization.
