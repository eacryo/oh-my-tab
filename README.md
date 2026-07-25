# oh-my-tab

> [简体中文](README-ZH.md) | English

A macOS window switcher — an alternative to the system Cmd+Tab. It runs as a **menu-bar accessory** app (no Dock icon), intercepts a global shortcut (**Option+Tab** by default, toggleable to Cmd+Tab), shows a floating **Liquid Glass** overlay of cards for currently-open windows, and raises the selected window on release using the Accessibility (AX) API.

It is pure Rust calling AppKit / CoreGraphics / ApplicationServices directly through `objc2` FFI — there is no Swift bridge and no Rust UI framework.

## Features

- Floating Liquid Glass overlay (`NSGlassEffectView` on macOS 26+, `NSVisualEffectView` fallback on older macOS).
- **Window-level** MRU ordering — each window is tracked independently by `(pid, CGWindowID)`, so switching to one window of an app doesn't drag the app's other windows forward.
- Raises **exactly one window** (located by `CGWindowID`) via the SkyLight private API `_SLPSSetFrontProcessWithOptions` + `AXRaise`, instead of `activate(AllWindows)` which would yank every window of the app to the front.
- Popups / panels / dropdowns are filtered out — a CG window must correspond to an AX `AXStandardWindow` to be kept (AX is authoritative).
- Minimized windows can optionally be shown (greyed out).
- TOML configuration, validated and **hot-reloadable** from the menu.
- Handcrafted, zero-dependency i18n (English, Simplified Chinese, Traditional Chinese) with automatic locale detection and live system-language follow.
- Per-launch log files with automatic 30-day retention (see [Logging](#logging)).

## Requirements

- macOS (developed on macOS 26; older versions supported via the `NSVisualEffectView` fallback).
- **Accessibility** permission granted to the app.

## Build & run

```sh
cargo build        # or cargo check for a fast type-check
cargo run          # build + run (takes over the global shortcut)
cargo clippy       # available, not wired into CI
```

There are **no tests** in the project.

## Permissions & runtime caveats

- The app requires **Accessibility** permission (`AXIsProcessTrusted`) for both the global key event tap and the AX window queries. Grant it under *System Settings → Privacy & Security → Accessibility*. A freshly built binary at a new path must be re-granted.
- If the event tap fails to create, the app prints an error and the shortcut silently does nothing — almost always a missing Accessibility grant.
- Runtime config: `~/.config/oh-my-tab/config.toml` (auto-created with defaults on first run).
- Icon cache: `~/Library/Caches/oh-my-tab-icons/{pid}.png`.

## Architecture

The codebase is organised into focused modules. The tricky parts span several files, so the breakdown below is load-bearing.

### Event flow & threading (spans all files)

1. `event_monitor` spawns a **dedicated thread** running a `CGEventTap` + `CFRunLoop`. It detects shortcut press (`CmdTabPressed`) and modifier-release (`CmdReleased`) and sends `GlobalEvent`s over a `flume` channel. The active shortcut (Cmd vs Opt) is the `SHORTCUT_IS_CMD` atomic, toggleable from the menu.
2. A **bridge thread** receives `GlobalEvent`s from flume and marshals each to the main thread via `performSelectorOnMainThread:` on the controller object (`OhMyTabController`).
3. The **main thread** runs `NSApplication.run` and owns all UI: ObjC callbacks build/refresh/show the overlay.

Shared state lives in global `static`s guarded by `Mutex` / `RwLock`: `TAB_STATE` (windows + selection + MRU), the ObjC object pointers, `CARD_INDEX_MAP`, `CONFIG`, and `LAST_ACTIVATED`. Raw ObjC pointers are wrapped in `ObjPtr` / `ObjClassPtr` with manual `Send` / `Sync`.

### Modules

| Module | Responsibility |
| --- | --- |
| `main.rs` | ObjC class registration, `NSApplication` setup, status-bar menu, overlay window creation, `NSWorkspace` / `NSLocale` notification observers, orchestration. |
| `event_monitor.rs` | `CGEventTap` on a dedicated thread; shortcut press/release detection; `GlobalEvent` channel. |
| `overlay.rs` | Overlay window, card views, keyboard/mouse navigation, rendering, theme application, window activation. |
| `window_collector.rs` | `CGWindowList` + AX enumeration, icon extraction/caching, MRU, window raising via the SkyLight private API. |
| `config.rs` | TOML config, validated, per-field resilient, hot-reloadable. |
| `i18n.rs` | Handcrafted TOML i18n, embedded at compile time, auto locale detection. |
| `settings.rs` | Settings window (controls, validation alerts, hot config application). |
| `menu.rs` | Status-bar menu and action callbacks. |
| `logger.rs` | Async logging (bounded channel, background writer). |
| `ffi.rs` / `theme.rs` | FFI primitives (CF/CG/NSString helpers, `Send`/`Sync` wrappers) and theme/layout accessors. |

### Key design points

- **AX is authoritative** for window filtering: a CG window must match an AX `AXStandardWindow` (paired by `CGWindowID` via the private `_AXUIElementGetWindow`), so popups/panels/dropdowns are dropped.
- **Titleless windows**: apps with a custom title bar (e.g. Microsoft To Do) expose an empty `AXTitle`; their windows are kept via a `titleless_pids` set, and only the *display* layer substitutes a placeholder (`"-"`). The stored title stays empty so `raise_ax_window` can still match the AX window.
- **macOS version split**: macOS 26+ renders with `NSGlassEffectView` (Liquid Glass); older macOS falls back to `NSVisualEffectView` (withinWindow + Dark).
- **Some calls use raw `objc_msgSend` FFI** because `objc2`'s `msg_send!` can't encode CF/CG types or void returns.

## Configuration

`~/.config/oh-my-tab/config.toml` — auto-created with defaults on first run. Loading is **per-field resilient**, not all-or-nothing: invalid fields fall back to defaults (logged, never fatal). Reloadable at runtime via the menu (*Reload Config*), which also re-applies theme and refreshes the overlay.

```toml
[appearance]
theme = "light"          # "dark" | "light" | "auto"
glass_style = "clear"    # "regular" | "clear"
glass_tint = "eeeeee66"  # RRGGBBAA
corner_radius = 64.0

[layout]
cards_per_row = 6
card_width = 140.0
card_height = 180.0
card_gap = 0.0
icon_size = 110.0

[colors]
# Each palette has: status_bar_text, app_name, win_title, icon_inner_bg,
# icon_text, card_bg_sel, card_border_sel — all RRGGBBAA.
[colors.dark]
status_bar_text = "999999ff"
app_name        = "ddddddff"
win_title       = "888888ff"
icon_inner_bg   = "22224444"
icon_text       = "9999bbff"
card_bg_sel     = "22224444"
card_border_sel = "5577ccff"
[colors.light]
status_bar_text = "333333ff"
app_name        = "1a1a1aff"
win_title       = "333333ff"
icon_inner_bg   = "d0d0e066"
icon_text       = "666688ff"
card_bg_sel     = "ffffff66"
card_border_sel = "5577ccff"

[fonts]
status_bar_size   = 13.0
status_bar_weight = 0.23
title_size        = 11.0
title_weight      = 0.23
app_name_size     = 13.0
app_name_weight   = 0.5

[keyboard]
modifier = "option"      # "option" (Option+Tab) | "command" (Cmd+Tab)

[i18n]
locale = "auto"          # "auto" | "en" | "zh-Hans" | "zh-Hant"

[windows]
show_minimized = false   # show minimized windows in the overlay

[logging]
level = "info"           # "info" | "warn" | "error"
file_path = ""           # empty = default timestamped path; see Logging below
```

## Logging

Logging is asynchronous and designed to **never block the UI / event loop**.

- **Async pipeline**: the `log_info!` / `log_warn!` / `log_error!` macros format a line and send it over a **bounded** `flume` channel (capacity 512) to a background writer thread. The caller never blocks — if the channel is full (e.g. disk writes stall), the **newest** entries are dropped (drop-newest) rather than stalling the caller. Normal load never triggers drops.
- **Destination**: `cargo run` (dev) → stdout only; a packaged `.app` (prod) → file.
- **Default file path**: `~/Library/Logs/oh-my-tab/oh-my-tab-<startup-timestamp>.log` — one file **per app launch**, where the timestamp is the process start time in a filename-safe form (e.g. `oh-my-tab-2026-07-25_17-08-30.log`).
- **Automatic 30-day cleanup**: at startup, the default log directory is scanned and any `oh-my-tab-*.log` whose modification time is older than 30 days is deleted. The current run's file is never pruned (its mtime keeps updating as it is written). Cleanup matches only the `oh-my-tab-*.log` pattern, so unrelated files in the same directory are left alone.
- **Within a single long run** the file still grows — there is no size-based rotation. The 30-day cleanup is cross-run, by mtime.

### Custom log path — and why the app never touches it

`[logging] file_path` lets you override the log destination by editing `config.toml` directly. It is **not exposed in the Settings UI** — to change it, edit `~/.config/oh-my-tab/config.toml` yourself.

When `file_path` is set (non-empty), the logger uses that path **verbatim**, in append mode. Crucially:

- **No timestamp is added** to a user-supplied path.
- **No cleanup is performed** on it.

The app deliberately **never writes extra files into, and never deletes any files from, a user-specified location.** If you point `file_path` at your own file or directory, you own its rotation and retention — the 30-day auto-cleanup applies *only* to the default `~/Library/Logs/oh-my-tab/` directory. This keeps the logger from surprising you by creating or deleting files in a location you explicitly chose to control.

## Internationalization

- Handcrafted TOML, zero dependencies, embedded at compile time via `include_str!` from `locales/{en,zh-Hans,zh-Hant}.toml` (no runtime file IO, no missing-file risk).
- Driven by `config.i18n.locale`: `"auto"` (default) | `"en"` | `"zh-Hans"` | `"zh-Hant"`. `"auto"` resolves from the system `NSLocale` preferred languages (scanned in order).
- **Hot-reload**: menu and settings re-title on config reload, and on system language change when `locale` is `auto`.
- **Adding a language**: create `locales/xx.toml` (same keys), register it in `i18n::locale_raw()`, add it to the `validate()` allow-list in `config.rs`, and extend `map_tag_to_supported()` if `auto` should map a system tag to it.

## Icon cache

`~/Library/Caches/oh-my-tab-icons/{pid}.png` — keyed by **PID** and "file exists = valid" (no TTL): an app update always relaunches with a new PID, which forces a re-extract. Icons are pre-cached at startup and on `NSWorkspaceDidLaunchApplicationNotification`. The cache can be cleared from the menu (*Clear Icon Cache*).

## Known limitations

- If the active shortcut is also intercepted by another `CGEventTap`-based app that is ahead of ours in the tap chain, pressing the shortcut may silently do nothing — there is no detection or fallback for this. (Apps using Carbon global hot keys are overridden by our filter tap; the risk is other filter-tap apps.) Secure Input also bypasses event taps entirely, with the same silent symptom.
- The active shortcut is a single toggle — only Option+Tab *or* Cmd+Tab is intercepted at any given moment.
- No test suite.

## Repository

https://github.com/eacryo/oh-my-tab
