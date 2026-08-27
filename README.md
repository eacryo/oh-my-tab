<p align="center">
  <img src="assets/Icon-512x512.png" width="120" height="120" alt="oh-my-tab">
</p>

<br />

<div align="center"><b>——&nbsp;&nbsp;&nbsp;macOS app switcher, clipboard history &amp; mouse control, in pure Rust&nbsp;&nbsp;&nbsp;——</b></div>

<br />

<p align="center">
  <a href="https://github.com/eacryo/oh-my-tab/releases"><img src="https://img.shields.io/github/v/release/eacryo/oh-my-tab?style=for-the-badge" alt="GitHub release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue?style=for-the-badge" alt="MIT License"></a>
  <a href="https://github.com/eacryo/oh-my-tab"><img src="https://img.shields.io/badge/platform-macOS-black?style=for-the-badge" alt="macOS"></a>
</p>

<br />

> [简体中文](README-ZH.md) | English

<br />

A macOS window switcher — an alternative to the system Cmd+Tab. It runs as a **menu-bar accessory** app (no Dock icon), intercepts a global shortcut (**Command+Tab** by default, toggleable to Option+Tab), shows a floating **Liquid Glass** overlay of cards for currently-open windows, and raises the selected window on release (via a private SkyLight API plus AX).

It is pure Rust calling AppKit / CoreGraphics / ApplicationServices directly through `objc2` FFI — there is no Swift bridge and no Rust UI framework.

- <img height="14" src="docs/icons/stack.svg"> **Native switcher**: app names, window titles, one card per window.
- <img height="14" src="docs/icons/key.svg"> **Keyboard navigation**: Tab, Shift+Tab, arrow keys, or mouse after Command/Option.
- <img height="14" src="docs/icons/zap.svg"> **Featherweight**: pure Rust — 1.5 MB binary, ~35 MB baseline memory, with the in-memory thumbnail cache bounded at ~64 MB. No Electron/Tauri.
- <img height="14" src="docs/icons/star.svg"> **Liquid Glass**: floating overlay (macOS 26; older macOS falls back to `NSVisualEffectView`).
- <img height="14" src="docs/icons/image.svg"> **Window thumbnails**: caption row above a 16:10 live preview, captured via a private WindowServer API and cached in memory — cached frames render instantly, background refresh keeps them current; balanced centered rows when they fit, leading MRU rows fill first and scroll continuously when overflowing. Requires **Screen Recording** permission — without it the switcher falls back to icon-only cards.
- <img height="14" src="docs/icons/history.svg"> **Window-level MRU**: switching one window never drags the app's others forward.
- <img height="14" src="docs/icons/eye.svg"> **Full window visibility**: every real window, including off-screen and minimized (toggleable).
- <img height="14" src="docs/icons/gear.svg"> **Hot-reloadable TOML**: validated config from the menu.
- <img height="14" src="docs/icons/globe.svg"> **Zero-dependency i18n**: English / Simplified / Traditional Chinese, live system-language follow.
- <img height="14" src="docs/icons/note.svg"> **Per-launch logs**: 30-day retention ([Logging](#logging)).
- <img height="14" src="docs/icons/sliders.svg"> **Mouse control** (optional): scroll modes, reversal, per-device acceleration, and **side-button → shortcut mapping** ([Configuration](#configuration)).
- <img height="14" src="docs/icons/copy.svg"> **Clipboard history** (optional): text, images, file copies — search, pin, delete, expiry, persistence ([Clipboard history](#clipboard-history)).

<br />

## <img height="16" src="docs/icons/download.svg">&nbsp;&nbsp;Install via Homebrew

If you just want to use the app (no need to build from source), install the prebuilt release via Homebrew Cask:

> ```sh
> brew install --cask eacryo/tap/oh-my-tab
> ```

This taps the [homebrew-tap](https://github.com/eacryo/homebrew-tap) repo and installs `Oh-My-Tab.app` into `/Applications`. Requires macOS 13+ on Apple Silicon.

- Update: `brew upgrade --cask oh-my-tab`
- Uninstall: `brew uninstall --cask oh-my-tab`

## <img height="16" src="docs/icons/image.svg">&nbsp;&nbsp;Screenshots

<p style="text-align: center;"><img src="docs/pictures/main_window.png" width="640" alt="Main window"></p>

<table style="border-collapse: collapse;">
  <tr>
    <td style="border: none; padding: 4px;"><img src="docs/pictures/settings.png" style="width: 100%;" alt="Settings"></td>
    <td style="border: none; padding: 4px;"><img src="docs/pictures/mouse.png" style="width: 100%;" alt="Mouse control"></td>
    <td style="border: none; padding: 4px;"><img src="docs/pictures/clipboard.png" style="width: 100%;" alt="Clipboard history"></td>
  </tr>
</table>

## <img height="16" src="docs/icons/copy.svg">&nbsp;&nbsp;Clipboard history

Optional (off by default). Summon with **Option+V**, navigate with the arrow keys / Enter / Esc / Backspace, or click; clicking outside closes it. Extra keys: **← pins/unpins** the selected entry; **→ opens a detail panel** beside the picker showing the full untruncated text or a large image preview (it follows ↑/↓ browsing live; Esc, ←, →, or a click on it closes it). The history records **three kinds of entries**:

| Kind | What is stored | Paste behavior |
|---|---|---|
| **Text** | The copied text, held in memory | The text is written back and Cmd+V is synthesized |
| **Image data** | An image copied inside an app (e.g. right-click → "Copy Image"): the original-format bytes are hashed and kept in a disk cache; only a downsampled thumbnail stays in RAM | The original bytes are written back under their original UTI — a JPG pastes back as JPG, an animated GIF as a GIF, never a PNG re-encode |
| **Image file** | An image FILE copied in Finder (Cmd+C): read once at copy time for a content hash and a thumbnail, then the bytes are discarded — only the path is kept | `public.file-url` is restored (file semantics, like Windows Win+V / Maccy): Finder duplicates the file, chat apps attach it. If the source file has been deleted, the paste is skipped |

> **Known v1 tradeoffs** — each entry records exactly one kind of content: a copy carrying **both text and an image** (e.g. copying an image from a web page) records only the text; **multiple-file copies and single non-image file copies are not recorded at all**. The same picture copied both as an image and as a file stays as two separate entries (they answer different paste semantics). Dedup is per-kind: text by exact content, images by content hash.

**Using an entry reorders the history by default** (like Maccy): selecting an entry and pressing Enter writes it back to the pasteboard, which the recorder sees as a re-copy and moves to the top. The **"Move used entries to top"** switch in Settings turns this off (like Windows Win+V). The picker's "Clear all" keeps pinned entries. An optional **"Save clipboard history to disk"** switch persists the history across restarts — see the privacy note under [Configuration](#configuration).

## <img height="16" src="docs/icons/alert.svg">&nbsp;&nbsp;Known Issues

**Some background-app thumbnails may temporarily appear white**: WindowServer can only capture the surface an app currently provides. A long-suspended WebView app (for example, Clash Verge Rev) may return its title bar with a white content area, especially just after oh-my-tab starts with an empty in-memory thumbnail cache. Activating the app and allowing its content to redraw lets a later capture recover the preview.

**Closing a window brings the same app's other window forward (macOS-native)**: closing the frontmost window of the active app makes macOS activate the app's remaining window — e.g. closing a Chrome incognito window brings the regular Chrome window to the front. oh-my-tab receives no event at that moment and does nothing; the next summon merely reflects the real frontmost window. This is identical to the behavior of the system Cmd+Tab.

If windows are already open when the app starts, their ordering differs from the native Cmd+Tab order. This is because there is no initial window-ordering data; oh-my-tab builds it by continuously observing window changes after launch.

**Device recognition caveats**: some exotic mice (e.g. ATK A9 SE, a Nearlink device) report an unexpected primary usage and appear as keyboards in System Settings; the device picker matches against the device's full HID usage pairs, so they are still recognized and configurable. Bluetooth keyboards whose HID descriptor fakes pointer usages are excluded via their Bluetooth GAP appearance. The picker refreshes live on plug/unplug and reconnect (plug events are debounced but never dropped).

Dev-only issues (icon staleness under `cargo run`, mouse control under a debugger) are collected in [docs/developer-notes.md](docs/developer-notes.md).

## <img height="16" src="docs/icons/tools.svg">&nbsp;&nbsp;Requirements

- macOS (developed on macOS 26; older versions supported via the `NSVisualEffectView` fallback, but availability is not guaranteed).
- **Accessibility** permission granted to the app.

## <img height="16" src="docs/icons/terminal.svg">&nbsp;&nbsp;Build & run

**Prerequisites:** Rust stable toolchain, Xcode Command Line Tools (`xcode-select --install`), macOS 13+. Accessibility permission is required at runtime (see Permissions below).

### Development

> ```sh
> cargo check       # fast type-check
> cargo run         # build + run (takes over the global shortcut)
> cargo clippy      # available, not wired into CI
> cargo test        # unit tests; add -- --ignored for the CG/AX smoke tests
> ```

`cargo run` launches the raw binary in **dev mode**: logs go to both stdout and the log file, and launch-at-login is inactive (SMAppService needs a `.app` bundle). The unit-test suite runs headless by default; the **smoke tests** are marked `#[ignore]` — they exercise the real CG/AX stack and need a GUI session plus an Accessibility grant (run with `cargo test -- --ignored`).

### Release `.app` + `.dmg`

> ```sh
> sh scripts/bundle.sh        # cargo build --release -> dist/Oh-My-Tab.app -> sign -> dist/Oh-My-Tab.dmg
> open dist/Oh-My-Tab.dmg     # install: drag Oh-My-Tab into Applications
> ```

`bundle.sh` assembles `dist/Oh-My-Tab.app` (binary + `Info.plist`), signs it, then packages it into `dist/Oh-My-Tab.dmg` (with an `Applications` symlink for drag-to-install). Both outputs live in `dist/` (gitignored), outside `target/` so the logger treats it as production (file logging, not stdout). Running the `.app` is required for launch-at-login (SMAppService) and for file logging; the `.dmg` is for distribution. Re-run the script after code changes — it copies the release binary at build time, and self-locates the repo root so it can be run from anywhere.

### Code signing

`bundle.sh` signs with the self-signed identity **`oh-my-tab-sign`** when present, falling back to ad-hoc (`codesign -s -`) if not. Creating this cert once is **strongly recommended** — it keeps the Accessibility grant stable across rebuilds (an ad-hoc signature changes on every rebuild, invalidating the grant each time):

1. *Keychain Access -> Certificate Assistant -> Create a Certificate...*
2. Name: `oh-my-tab-sign`, Identity Type: **Self Signed Root**, Certificate Type: **Code Signing**.
3. Create, then rebuild and reinstall. (The first `bundle.sh` run may prompt for keychain access — click *Always Allow*.)

If grants ever go stale (e.g. leftovers from old ad-hoc installs), clear them: `tccutil reset Accessibility com.eacryo.oh-my-tab`. A self-signed cert only stabilises TCC identity — it does **not** satisfy Gatekeeper for other users; that requires a paid Apple Developer ID certificate (set `SIGN_IDENTITY` in `scripts/bundle.sh`).

For the full release pipeline (Homebrew cask generation, the signing rationale, icon regeneration), see [docs/releasing.md](docs/releasing.md). For architecture notes, see [AGENTS.md](AGENTS.md).

## <img height="16" src="docs/icons/shield-lock.svg">&nbsp;&nbsp;Permissions & runtime caveats

- The app requires **Accessibility** permission (`AXIsProcessTrusted`) for both the global key event tap and the AX window queries. Grant it under *System Settings → Privacy & Security → Accessibility*. A freshly built binary must be re-granted -- unless you sign with a stable identity (see [Code signing](#code-signing)), in which case the grant persists across rebuilds.
- **Window thumbnails** additionally require the **Screen Recording** permission (a private WindowServer capture API is used, same as DockDoor/AltTab). Without it the switcher silently keeps icon-only cards; macOS 14+ may periodically re-ask you to re-confirm this permission. Frames are kept **in memory only** — nothing is ever written to disk.
- If the event tap fails to create, the app prints an error and the shortcut silently does nothing — almost always a missing Accessibility grant.
- Runtime config: `~/.config/oh-my-tab/config.toml` (auto-created with defaults on first run).
- Icon cache: `~/Library/Caches/oh-my-tab-icons/{bundle-id}.png` (keyed by bundle id, with a `.meta` mtime sidecar; clearable from the menu).

## <img height="16" src="docs/icons/gear.svg">&nbsp;&nbsp;Configuration

`~/.config/oh-my-tab/config.toml` — auto-created with defaults on first run. Loading is **per-field resilient**, not all-or-nothing: invalid fields fall back to defaults (logged, never fatal). Reloadable at runtime via the menu (*Reload Config*), which also re-applies theme and refreshes the overlay. The commonly edited keys:

```toml
[appearance]
theme = "light"          # "dark" | "light" | "auto"
glass_style = "regular"  # "regular" | "clear"
glass_tint = "eeeeee66"  # RRGGBBAA — default Liquid Glass overlay tint
corner_radius = 32.0

[layout]
thumbnails_enabled = true  # window thumbnails on cards; off = icon-only cards

[keyboard]
modifier = "command"     # "option" (Option+Tab) | "command" (Cmd+Tab)

[i18n]
locale = "auto"          # "auto" | "en" | "zh-Hans" | "zh-Hant"

[windows]
enabled = true            # app-switcher master switch (off = Cmd+Tab passes through to the system)
show_minimized = false    # show minimized windows in the overlay
overlay_position = "active_window"  # "active_window" (follow the active window's screen) | "main" (always the main screen)

[logging]
level = "info"           # "debug" | "info"
file_path = ""           # empty = default timestamped path; see Logging below

[startup]
launch_at_login = false  # launch at login (requires running as a .app bundle; macOS 13+)

[clipboard]
enabled = false          # clipboard history master switch (off by default)
max_entries = 50         # max history entries (1..=100)
persist = false          # save history to disk so it survives restarts (see the privacy note below)
auto_expire_days = 3     # unpinned entries expire after N days (memory AND disk); 0 = off
pin_follow_selection = true # after pin/unpin, move the selection to the toggled entry (false = keep the current position)
move_used_to_top = true  # pasting moves the used entry to the top (false = pasting never reorders, like Win+V)
picker_position = "main" # picker position: "mouse" (follow the cursor) | "main" (centered on the main screen)
show_source_app = false  # show the source app name in rows (the source is always recorded either way)

[mouse]
enabled = false          # master switch for the mouse-control event tap

# The first profile without device_* fields is the default ("all mice") layer.
# Additional profiles match a specific device by VID/PID and override the default
# per-field. Effective config = default layer merged with the matching device layer.
[[mouse.profiles]]
reverse_scroll = false   # flip scroll direction relative to the system
scroll_mode = "default"  # "default" | "line" (fixed lines per tick)
line_count = 3           # lines per tick in "line" mode (1..=10)
button_mappings_enabled = true  # per-profile mappings master switch (default true)

[mouse.profiles.pointer]
disable_acceleration = false  # disable system pointer acceleration (linear tracking)

# Button mappings: bind middle/side buttons (button number >= 2) to actions.
# Left (0) / right (1) can't be bound (you'd lock yourself out of clicking).
# Button numbers: 2 = middle, 3 = back, 4 = forward, 5+ = other side/macro
# buttons (they vary per mouse -- configure per device). Values can be a shortcut
# ("cmd+shift+v"), a system action ("missioncontrol"/"launchpad"/"showdesktop"/
# "appexpose", fired via Dock's private notification -- immune to system-shortcut
# occupancy), or "none" (swallow the button; it becomes inert). Binding Cmd+Tab /
# Option+V opens OUR overlay / clipboard (dispatched internally, no synthesized events).
[mouse.profiles.button_mappings]
"3" = "cmd+shift+v"
"4" = "alt+tab"

# Example per-device override layer (Logitech MCHOSE G3 V2):
[[mouse.profiles]]
device_vendor_id = 10007
device_product_id = 12976
reverse_scroll = true
scroll_mode = "line"
line_count = 3
```

The advanced `[colors]` and `[fonts]` sections (card text colors and sizes per theme) are written to the auto-created config file with their defaults — edit them there.

> **Clipboard-history persistence & privacy** — enabling `persist` (or the "Save clipboard
> history" switch in Settings) writes your clipboard history — copied text, filenames, and
> image bytes — to disk so it survives app restarts:
>
> - `~/.config/oh-my-tab/clipboard-history.toml` (text, filenames, sources, metadata; mode 600)
> - `~/Library/Caches/oh-my-tab-clip-images/` (image bytes and previews, keyed by content hash)
>
> These files are stored in **plain text / unencrypted**. The history file is readable by
> **any application running as your user** (mode 600 only blocks other user accounts), so do
> **not** enable persistence if you copy passwords, tokens, or other secrets. As a safeguard,
> content marked with the standard `nspasteboard.org` "Securing Copy" markers
> (`org.nspasteboard.ConcealedType` / `TransientType` / `AutoGeneratedType`, plus the
> 1Password marker `com.agilebits.onepassword`) is **never recorded** — password managers
> stamp these markers on password copies, so such content never reaches the history (memory
> or disk) in the first place. Persistence is off by default.

Mouse settings are also exposed in the Settings window (a **device picker** lists each connected mouse; pick one to edit its layer). The button-mappings section lists bound rows (button name + action description + keycaps); clicking **Edit** opens an edit panel (same as LinearMouse): record the trigger side button, pick the action type (Default / None / Key Press / Mission Control / Launchpad / Show Desktop / App Expose), and record the combo for Key Press, then confirm. Toggling `mouse.enabled` takes effect immediately — no app restart needed.

## <img height="16" src="docs/icons/note.svg">&nbsp;&nbsp;Logging

- **Destination**: `cargo run` (dev) → both stdout **and** the log file; a packaged `.app` (prod) → log file only.
- **Default file path**: `~/Library/Logs/oh-my-tab/oh-my-tab-<startup-timestamp>.log` — one file **per app launch**. Files in the default directory older than 30 days are deleted at startup (no size-based rotation within a single run).
- **Custom path**: `[logging] file_path` (edit `config.toml` directly; not exposed in Settings). A user-supplied path is used **verbatim**, in append mode — no timestamp is added and no cleanup is performed on it; rotation and retention are yours.
- **Privacy**: debug logs never record actual keystrokes. The switcher's key tap logs only `Tab` / `Command` / `Option` (and the summon combo name); every other key is logged as plain `Other` — no keycodes, no modifier details.

## <img height="16" src="docs/icons/heart.svg">&nbsp;&nbsp;Credits

The **mouse control** feature (scroll reversal, scroll modes, per-device configuration, and pointer-acceleration control) is inspired by and references [LinearMouse](https://github.com/linearmouse/linearmouse). We re-implemented its core features from scratch in pure Rust (via `objc2` FFI, no Swift bridge) and integrated them into oh-my-tab's configuration model. Many thanks to the original author and the LinearMouse project for the excellent work.

The **window switcher** (overlay design, card-based selection, Liquid Glass styling) draws inspiration from [BetterCmdTab](https://github.com/rokartur/BetterCmdTab). We re-implemented the ideas from scratch in pure Rust (via `objc2` FFI, no Swift bridge). Many thanks to the author for the excellent work.
