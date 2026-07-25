# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) and OpenCode when working with code in this repository.

## What this is

`oh-my-tab` is a macOS app/window switcher (an alternative to the system Cmd+Tab). It runs as an **accessory** app (no Dock icon; lives in the menu bar), intercepts a global shortcut (default **Option+Tab**, toggleable to Cmd+Tab), shows a floating "Liquid Glass" overlay of cards for currently-open windows, and raises the selected window on release using the Accessibility (AX) API.

It is Rust calling AppKit/CoreGraphics/ApplicationServices directly via `objc2` FFI — there is no Swift bridge and no Rust UI framework.

## Build & run

```sh
cargo build        # or cargo check for fast type-checking
cargo run          # build + run (takes over the global shortcut)
cargo clippy       # not configured in CI, but available
```

There are **no tests** in the project.

**Permissions & runtime caveats:**
- The app requires **Accessibility** permission (`AXIsProcessTrusted`) for both the global key event tap and AX window queries. Grant it under System Settings → Privacy & Security → Accessibility. A freshly built binary at a new path must be re-granted.
- If the event tap fails to create, the app prints an error and the shortcut silently does nothing — almost always a missing Accessibility grant.
- Runtime config: `~/.config/oh-my-tab/config.toml` (auto-created with defaults on first run).
- Icon cache: `~/Library/Caches/oh-my-tab-icons/{pid}.png`.

## Architecture

Five modules. The tricky parts span several files, so the breakdown below is load-bearing.

### Event flow & threading (spans all files)
1. `event_monitor::start` spawns a **dedicated thread** running a `CGEventTap` + `CFRunLoop`. It detects Tab+modifier-down (`CmdTabPressed`) and modifier-release (`CmdReleased`), and sends `GlobalEvent`s over a `flume` channel. The active shortcut (Cmd vs Opt) is the `SHORTCUT_IS_CMD` atomic, toggleable from the menu.
2. A **bridge thread** receives `GlobalEvent`s from flume and marshals each to the main thread via `performSelectorOnMainThread:` on the controller object (`OhMyTabController`).
3. The **main thread** runs `NSApplication.run` and owns all UI: ObjC callbacks (`on_cmd_tab_pressed`, `on_cmd_released`, card/container view methods) build/refresh/show the overlay.

Shared state is global `static`s guarded by `Mutex`/`RwLock`: `TAB_STATE` (the windows + selection + MRU), the ObjC object pointers (`CONTROLLER`/`OVERLAY_WINDOW`/`CONTAINER`/`STATUS_LABEL`), `CARD_INDEX_MAP`, `CONFIG`, and `LAST_ACTIVATED` (in `window_collector`). Raw ObjC pointers are wrapped in `ObjPtr`/`ObjClassPtr` with manual `Send`/`Sync`.

### `main.rs` — UI, ObjC classes, orchestration
- Custom ObjC classes (`OhMyTabCardView`, `OhMyTabContainerView`, `OhMyTabController`, menu target) are **registered at runtime** via `objc_allocateClassPair` + `class_addMethod`, not declared in a bridge. Method impls are `extern "C" fn`s.
- **Some calls must use raw `objc_msgSend` FFI** because `objc2`'s `msg_send!` can't encode CF/CG types or (historically) void returns. See `hex_to_cg_color`, `layer_set_background`, `layer_set_border`, and `activate_pid`. When adding AppKit calls that touch `CGColorRef` or return void, follow those patterns. Background: `notes/window-activation-approaches.md`.
- **Card ↔ index mapping**: dynamically-registered classes can't expose properties through `msg_send!` reliably, so each card view's index is stored in `CARD_INDEX_MAP` keyed by view pointer (not as an ObjC property). `get_card_index`/`set_card_index`/`remove_card_index` manage it.
- **macOS version split**: macOS 26+ renders the overlay with `NSGlassEffectView` (Liquid Glass); older macOS falls back to `NSVisualEffectView` (withinWindow + Dark). Detected via `AnyClass::get(c"NSGlassEffectView")`.
- **Hover gating**: `MOUSE_MOVED` prevents the card under the cursor from being selected when the overlay first appears; the user must move the mouse first (matches native Cmd+Tab).
- Card views are rebuilt by `show_overlay` (full rebuild on summon) and **surgically by `rebuild_cards`** (in-place swap of individual cards, used after icons get extracted mid-session so icons appear without re-summoning).

### `window_collector.rs` — window enumeration, icons, raising
- `collect_windows` is the core. It takes `CGWindowListCopyWindowInfo` (on-screen, `kCGWindowLayer == 0`) and cross-references each with the per-PID **AX window list** (`AXUIElement`). **AX is authoritative**: a CG window must correspond to an AX `AXStandardWindow` to be kept (this is how popups/panels/dropdowns are filtered out).
- **Titleless windows**: some apps (e.g. Microsoft To Do) have a custom title bar and expose an **empty `AXTitle`** plus an empty `kCGWindowName`. Such windows are kept via `titleless_pids` (apps whose AX windows are *all* untitled). Their internal `window_title` stays empty (so `raise_ax_window` can still match the AX window by its empty title) and only the *display* layer substitutes a placeholder via `display_title()` (`"-"`). Do not fill the stored title with the placeholder — it would break window targeting.
- **Icon cache** is keyed by **PID** and is "file exists = valid" (no TTL): app updates relaunch with a new PID, which forces a re-extract, so no expiry is needed. Icons are pre-cached at startup (`cache_running_app_icons`) and on app launch (`NSWorkspaceDidLaunchApplicationNotification`, extracted on a background thread with an autorelease pool). `extract_uncached_icons` (runs on 2nd+ Tab while visible) fills any still-missing icons and calls `rebuild_cards`.
- **MRU ordering**: `mru` (window_id → Instant) is updated on each switch; `LAST_ACTIVATED` (pid → Instant) is fed by `NSWorkspaceDidActivateApplicationNotification` so switches made via Dock/system Cmd+Tab are reflected.
- `raise_ax_window(pid, title)` focuses a specific AX window by matching `AXTitle`, preceded by `activate_pid` (`NSRunningApplication activateWithOptions:`).

### `config.rs` — TOML config, validated, reloadable
- Global singleton `CONFIG: LazyLock<RwLock<Config>>` at `~/.config/oh-my-tab/config.toml`.
- Loading is **per-field resilient**, not all-or-nothing: `validate()` collects errors, then `merge_valid()` keeps loaded values for valid fields and resets invalid ones to defaults. Config errors are logged, never fatal.
- Reloadable at runtime via the menu ("Reload Config" → `reload_config`), which also re-applies theme and refreshes the overlay.
- Color fields are 8-hex-digit `RRGGBBAA` strings; `parse_hex8` converts them. Many layout/font values are read live from `CONFIG` inside the UI helpers (e.g. `card_w()`, `current_colors()`), so config changes take effect on the next render.

### `i18n.rs` - internationalization (handcrafted TOML, zero deps)
- A handcrafted TOML-based i18n system, deliberately DIY (not `rust-i18n`/`fluent`) to keep dependencies minimal and stay isomorphic with `config.rs`. Translation files are embedded at compile time via `include_str!` from `locales/{en,zh-Hans,zh-Hant}.toml` (no runtime file IO, no missing-file risk).
- **Locale flow**: driven by `config.i18n.locale` (`"auto"` | `"en"` | `"zh-Hans"` | `"zh-Hant"`, default `"auto"`). Resolution priority: config value (non-`auto` & supported) > first matching entry in the system `NSLocale preferredLanguages` list (scanned **in order**, so a supported language lower in the user's preference beats the default - e.g. `[ja, zh-Hans, en]` -> `zh-Hans`) > `"en"`. Chinese tags split by script/region: `Hant` / `TW` / `HK` / `MO` -> `zh-Hant`; everything else (incl. bare `zh`, `CN`, `SG`) -> `zh-Hans`.
- **API**: `t(key) -> String` (current locale -> en fallback -> key itself) and `tf(key, &[("name","value")])` for templates with `{name}` placeholders. Locale TOML uses `[section]` groups, flattened at load to dot keys (e.g. `[menu]` `settings` -> `menu.settings`). Keys: `[menu]` / `[settings]` / `[alert]` / `[errors]`.
- **Hot-reload**: `config.rs::reload_config()` and the `CONFIG` LazyLock init both call `i18n::apply_config_locale()`. `main.rs::refresh_menu_titles()` re-titles all 5 menu items (theme + shortcut labels are state-dependent; settings/reload/quit are static, stored in `FIXED_MENU_ITEMS`); `invalidate_settings_window()` releases the cached settings window so it rebuilds with the new locale on next open. Both run from `handle_reload_config`, `apply_config_refresh`, and `on_locale_changed`.
- **Live system-language follow**: `on_locale_changed` observes `NSLocaleCurrentLocaleDidChangeNotification` on the default `NSNotificationCenter`; when the system language changes and `config.i18n.locale` is `auto`, it re-resolves and refreshes the UI (explicit locales short-circuit). It self-marshals to the main thread via `performSelectorOnMainThread` since the notification's delivery thread isn't guaranteed.
- **No-cycle constraint (load-bearing)**: `i18n.rs` NEVER reads `CONFIG` (only `NSLocale`). This is required because `CONFIG`'s LazyLock init calls `validate()` -> `tf()` -> `I18N` init; if `I18N` read `CONFIG` it would deadlock. Do not break this.
- **Add a language**: create `locales/xx.toml` (same keys), register it in `i18n::locale_raw()`, add it to the `validate()` allow-list in `config.rs`, and extend `map_tag_to_supported()` if `auto` should map a system tag to it.
- Validation errors from `config.rs::validate()` are themselves localized via `tf()` (they surface in `show_alert`).


## Commit Messages

When asked for a commit message, always follow the [Conventional Commits](https://www.conventionalcommits.org/) specification and keep it to a single line. No body, no bullet points, no footers.

Format: `type: description`

Types: feat, fix, docs, style, refactor, perf, test, chore, ci, build

Example: `feat(ui): implement compact floating window selector with auto-resize`

## Code Comments

Add comments at key locations: non-obvious logic, important design decisions, FFI/ObjC bridging subtleties, and any workaround whose reason isn't obvious from the code. Comments must be bilingual - Chinese first, then English on the following line(s) - mirroring the existing style in the codebase. Example:

```rust
// 只保留标准窗口，过滤弹出面板/下拉菜单等非标准窗口
// Only keep AXStandardWindow, filtering out popups/panels/dropdowns
```

## Internationalization

- **All user-visible strings go through `t()` / `tf()`** in `i18n.rs` - never hardcode Chinese or English literals in UI code (menu titles, settings window labels, alert titles/buttons, validation errors). Add the key to **both** `locales/en.toml` and `locales/zh-Hans.toml` and reference it via `t("section.key")` (or `tf("section.key", &[("name","value")])` when interpolation is needed).
- **Do NOT localize**: `println!`/`eprintln!` log lines (developer-facing, stay English) and `status_text` (the dynamic app/window title shown under the overlay - that's data, not UI chrome).
- UI strings are single-language per locale. The zh settings labels intentionally keep the English config-key as a hint (e.g. `"主题 theme"`) since it helps users edit `config.toml`; en uses plain friendly names. This is a per-locale wording choice captured in the TOML, not a code concern.
- Comments stay bilingual Chinese-first then English (see Code Comments above); that convention is about code comments, separate from UI-string localization.
