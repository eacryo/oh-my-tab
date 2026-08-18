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

A macOS window switcher — an alternative to the system Cmd+Tab. It runs as a **menu-bar accessory** app (no Dock icon), intercepts a global shortcut (**Command+Tab** by default, toggleable to Option+Tab), shows a floating **Liquid Glass** overlay of cards for currently-open windows, and raises the selected window on release using the Accessibility (AX) API.

It is pure Rust calling AppKit / CoreGraphics / ApplicationServices directly through `objc2` FFI — there is no Swift bridge and no Rust UI framework.

- <img height="14" src="docs/icons/stack.svg"> **Native switcher**: app names, window titles, one card per window.
- <img height="14" src="docs/icons/key.svg"> **Keyboard navigation**: Tab, arrow keys, or mouse after Command/Option.
- <img height="14" src="docs/icons/zap.svg"> **Featherweight**: pure Rust — 1.5 MB binary, ~35 MB memory. No Electron/Tauri.
- <img height="14" src="docs/icons/star.svg"> **Liquid Glass**: floating overlay (macOS 26).
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
    <td style="border: none; padding: 4px;"><img src="docs/pictures/experimental.png" style="width: 100%;" alt="Experimental settings"></td>
  </tr>
</table>

## <img height="16" src="docs/icons/copy.svg">&nbsp;&nbsp;Clipboard history

Optional (off by default). Summon with **Option+V**, navigate with the arrow keys / Enter / Esc / Backspace, or click; clicking outside closes it. Extra keys: **← pins/unpins** the selected entry; **→ opens a detail panel** beside the picker showing the full untruncated text or a large image preview (it follows ↑/↓ browsing live; Esc, ←, →, or a click on it closes it). The history records **three kinds of entries**:

| Kind | What is stored | Paste behavior |
|---|---|---|
| **Text** | The copied text, held in memory | The text is written back and Cmd+V is synthesized |
| **Image data** | An image copied inside an app (e.g. right-click → "Copy Image"): the original-format bytes are hashed and kept in a disk cache (`~/Library/Caches/oh-my-tab-clip-images/{hash}`) plus a downsampled thumbnail; only the thumbnail stays in RAM | The original bytes are written back under their original UTI — a JPG pastes back as JPG, an animated GIF as a GIF, never a PNG re-encode |
| **Image file** | An image FILE copied in Finder (Cmd+C): the file is read once at copy time to compute a content hash and a thumbnail, then the bytes are discarded — only the path, hash and thumbnail are kept | `public.file-url` is restored (file semantics, like Windows Win+V / Maccy): Finder duplicates the file, chat apps attach it, GIF animation stays intact. If the source file has been deleted, the paste is skipped |

> **One kind per entry (known limitation)** — each entry records exactly one kind of content: either the **text** or the **image** (image data / image file). When a copy carries **both text and an image** (e.g. copying an image from a web page, which usually also puts text on the pasteboard), only the **text** is recorded and the image is dropped. **Multiple files are not recorded at all** — copying several files at once (Cmd+C on a multi-selection in Finder, whether or not they include images) produces no entry, and neither does copying a single NON-image file: only single image-file copies are recognized (a file copy is detected by its `public.file-url`; its filename text is never recorded as a plain-text entry). These are deliberate v1 tradeoffs — unlike Windows Win+V, which keeps every pasteboard format in one entry and lets the destination app pick what it supports on paste.

**Dedup is per-kind** (never across kinds):
- Text dedups by exact text.
- Image-data entries dedup by content hash; image-file entries dedup by content hash too (a file and its Finder duplicate — identical bytes, different paths — collapse into one entry, keeping the latest path).
- The same picture copied both as an image and as a file stays as **two separate entries** (they answer different paste semantics). They share the content hash, so they share the disk cache; deleting one entry only removes the cache files when no other entry still references them.

**Using an entry reorders the history by default**: selecting an entry and pressing Enter writes it back to the pasteboard, which the recorder sees as a re-copy and moves to the top (like Maccy). The **"Move used entries to top"** switch in Settings turns this off — pasting then never reorders the history (like Windows Win+V).

The picker's "Clear all" keeps pinned entries; per-entry delete (Backspace), auto-expiry, and the entry-cap trim follow the same cache rules. An optional **"Save clipboard history to disk"** switch persists the history across restarts — see the privacy note under [Configuration](#configuration).

**Cache locations**: image entries keep their original-format bytes and previews under `~/Library/Caches/oh-my-tab-clip-images/` — the original bytes as `{hash:016x}` (no extension), the downsampled thumbnail as `{hash:016x}.preview`, and the lazily generated detail-panel large preview as `{hash:016x}.detail` (all keyed by the content hash). File-copy entries store **no bytes** (reference-only, a pasted file is read from its original path). The cache directory is wiped at startup unless persistence is on; cache files are deleted in sync with per-entry delete / clear-all / auto-expiry / the entry-cap trim. With **"Save clipboard history"** on, the history itself lives at `~/.config/oh-my-tab/clipboard-history.toml` (mode 600, plaintext — see the privacy note under [Configuration](#configuration)).

## <img height="16" src="docs/icons/alert.svg">&nbsp;&nbsp;Known Issues

**Closing a window brings the same app's other window forward (macOS-native)**: closing the frontmost window of the active app makes macOS activate the app's remaining window — e.g. closing a Chrome incognito window brings the regular Chrome window to the front. oh-my-tab receives no event at that moment and does nothing; the next summon merely reflects the real frontmost window. This is identical to the behavior of the system Cmd+Tab.

If windows are already open when the app starts, their ordering differs from the native Cmd+Tab order. This is because there is no initial window-ordering data; oh-my-tab builds it by continuously observing window changes after launch.

**Dev-mode icon staleness**: when running the raw binary via `cargo run`, the overlay may occasionally show oh-my-tab's own card with the letter placeholder instead of the app icon, and it can persist until the icon cache is cleared. The icon cache is keyed by bundle id with the executable's **mtime** as a staleness fingerprint; in dev mode the binary is relinked on every build, changing its mtime mid-session, which invalidates the cached entry for the running instance. Packaged `.app` builds are unaffected (the binary mtime is stable after install). If you hit this in dev, use *Clear Icon Cache* from the menu or delete `~/Library/Caches/oh-my-tab-icons/`.

**Device recognition caveats**: a device is treated as a mouse/trackpad when it **conforms to** the Pointer (1,1), Mouse (1,2), or Trackpad (1,5) usage on the Generic Desktop page — checked via the public `IOHIDServiceClientConformsTo` API, which inspects the device's full `DeviceUsagePairs` rather than a single `PrimaryUsage` value. This is needed because some real mice report an unexpected primary usage: for example, **ATK A9 SE** (a Nearlink/星闪 device) exposes `PrimaryUsage = 6 (Keyboard)`, so macOS shows it as a keyboard in System Settings — but it also declares Mouse (1,2) in its `DeviceUsagePairs`, and `ConformsTo` catches it. Using `PrimaryUsage` alone would silently drop such devices from the picker and force their events onto the "last active" profile. Known caveats (same behavior as LinearMouse):

- **Bluetooth keyboards are excluded from the device picker** even when their HID descriptor fakes pointer usages (e.g. Kzzi-i75 declares full Mouse collections). The picker cross-checks the Bluetooth **GAP Appearance** (0x03C1 = keyboard) from bluetoothd's NVRAM cache, matched by the HID service's Bluetooth address — the same source macOS's Bluetooth pane uses for its icons. Devices absent from the NVRAM cache (freshly paired) or non-Bluetooth devices fall back to HID-only classification.
- The device picker refreshes **live**: unplugging a device removes it from the list immediately, and a reconnect reappears automatically (plug events are debounced but never dropped; a delayed recheck catches the fast BLE sleep-wake pattern).

**Mouse control may fail when launched under a debugger**: when the app is started in Debug mode via RustRover (or another debugger), frequent mouse usage (scrolling / clicking) right after launch can cause mouse control features (reverse scrolling, per-device settings) to stop working — the app stops receiving mouse events, scrolling reverts to the system direction, and pointer acceleration settings stop applying, until the app is restarted. Launching the packaged `.app` or running the binary directly from a terminal is unaffected; this only affects unsigned dev builds started under a debugger (a macOS 26 restriction on HID-level event listening for debugger-launched processes).

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
> ```

`cargo run` launches the raw binary in **dev mode**: logs go to both stdout and the log file, and launch-at-login is inactive (SMAppService needs a `.app` bundle). There are **no tests** in the project.

### Release `.app` + `.dmg`

> ```sh
> sh scripts/bundle.sh        # cargo build --release -> dist/Oh-My-Tab.app -> sign -> dist/Oh-My-Tab.dmg
> open dist/Oh-My-Tab.dmg     # install: drag Oh-My-Tab into Applications
> ```

`bundle.sh` assembles `dist/Oh-My-Tab.app` (binary + `Info.plist`), signs it, then packages it into `dist/Oh-My-Tab.dmg` (with an `Applications` symlink for drag-to-install). Both outputs live in `dist/` (gitignored), outside `target/` so the logger treats it as production (file logging, not stdout). Running the `.app` is required for launch-at-login (SMAppService) and for file logging; the `.dmg` is for distribution.

Re-run `sh scripts/bundle.sh` after code changes (the bundle copies the release binary at build time). The script self-locates the repo root, so it can be run from anywhere; it references `assets/Info.plist` and writes to `dist/`.

### Code signing

`bundle.sh` signs with the self-signed identity **`oh-my-tab-sign`** when present, falling back to ad-hoc (`codesign -s -`) if not. Creating this cert once is **strongly recommended** -- it keeps the Accessibility grant stable across rebuilds.

**Why:** an ad-hoc-signed app's designated requirement is just the bare CDHash, which changes on every rebuild. macOS TCC keys the Accessibility grant to that CDHash, so each rebuild invalidates the grant (TCC logs `Failed to match existing code requirement` / `errSecCSReqFailed`), and stale entries from previous installs make it worse. A self-signed cert makes the designated requirement cert-based (`certificate leaf = H"..."`), stable across rebuilds, so the grant persists.

**Create the cert once** (Keychain Access):

1. *Keychain Access -> Certificate Assistant -> Create a Certificate...*
2. Name: `oh-my-tab-sign`, Identity Type: **Self Signed Root**, Certificate Type: **Code Signing**.
3. Create. (The first `bundle.sh` run may prompt for keychain access -- click *Always Allow*.)

Then rebuild, reinstall, and grant Accessibility once. From then on, rebuilds keep the same cert identity, so **no re-granting is needed**. If grants ever go stale (e.g. left over from old ad-hoc installs), clear them:

```sh
tccutil reset Accessibility com.eacryo.oh-my-tab
```

**Caveat:** a self-signed cert only stabilises TCC identity -- it does **not** satisfy Gatekeeper for other users (they still see "unidentified developer" and must right-click -> Open). For friction-free distribution you need a paid Apple **Developer ID Application** certificate; if you have one, set `SIGN_IDENTITY` in `scripts/bundle.sh` to its name.

### Homebrew cask release

`scripts/release.sh` is the full release pipeline: it runs `bundle.sh` first, then generates `dist/oh-my-tab.rb` -- a Homebrew cask file with the dmg's `sha256` and the `version` read from `Cargo.toml`, plus a `zap trash:` block so `brew uninstall --cask` also removes the icon cache, logs, and config.

```sh
sh scripts/release.sh        # bundle.sh -> dist/Oh-My-Tab.dmg + dist/oh-my-tab.rb
```

The cask pins `depends_on macos: :ventura` + `depends_on arch: :arm64`, so it installs only on macOS 13+ Apple Silicon -- the same restriction noted under [Install via Homebrew](#install-via-homebrew). Its `url` points at `https://github.com/eacryo/oh-my-tab/releases/download/v#{version}/Oh-My-Tab.dmg`, so the dmg must be published to a GitHub release tagged `v<version>` (matching the `Cargo.toml` version).

To publish a new version:

1. Bump `version` in `Cargo.toml`.
2. Run `sh scripts/release.sh` -> produces `dist/Oh-My-Tab.dmg` and `dist/oh-my-tab.rb`.
3. Create a GitHub release tagged `v<version>` and upload `dist/Oh-My-Tab.dmg` to it.
4. Copy `dist/oh-my-tab.rb` into the [homebrew-tap](https://github.com/eacryo/homebrew-tap) repo's `Casks/` directory and push.

After step 4, `brew install --cask eacryo/tap/oh-my-tab` (or `brew upgrade --cask`) picks up the new version.

`brew install --cask` actually reads the committed copy in the tap repo: [`Casks/oh-my-tab.rb`](https://github.com/eacryo/homebrew-tap/blob/main/Casks/oh-my-tab.rb) in `eacryo/homebrew-tap`. `release.sh` only regenerates it locally so you can copy the new version over.

## <img height="16" src="docs/icons/package.svg">&nbsp;&nbsp;App icon

The app icon (`AppIcon.icns`) is generated from `assets/Icon-Default-1024x1024@1x.png` and bundled into `Contents/Resources/`. The committed `assets/AppIcon.icns` is used directly by `bundle.sh`, so contributors need no extra tooling to build the `.app`.

To regenerate it after replacing the source PNG:

```sh
./scripts/build-icon-from-png.sh   # 1024x1024 PNG -> 10 .iconset sizes (sips) -> assets/AppIcon.icns
```

Requires `iconutil` (Xcode CLT); `sips` ships with macOS. Then commit the regenerated `assets/AppIcon.icns` alongside the `assets/Icon-Default-1024x1024@1x.png` change.

If `assets/AppIcon.icon` (a directory) is present, `bundle.sh` also bundles it into `Contents/Resources/` for the macOS 26+ Liquid Glass icon format, which macOS prefers over `.icns`.

## <img height="16" src="docs/icons/shield-lock.svg">&nbsp;&nbsp;Permissions & runtime caveats

- The app requires **Accessibility** permission (`AXIsProcessTrusted`) for both the global key event tap and the AX window queries. Grant it under *System Settings → Privacy & Security → Accessibility*. A freshly built binary must be re-granted -- unless you sign with a stable identity (see [Code signing](#code-signing)), in which case the grant persists across rebuilds.
- If the event tap fails to create, the app prints an error and the shortcut silently does nothing — almost always a missing Accessibility grant.
- Runtime config: `~/.config/oh-my-tab/config.toml` (auto-created with defaults on first run).
- Icon cache: `~/Library/Caches/oh-my-tab-icons/{bundle-id}.png` (keyed by bundle id, with a `.meta` mtime sidecar).

## <img height="16" src="docs/icons/graph.svg">&nbsp;&nbsp;Architecture

The codebase is organised into focused modules. The tricky parts span several files, so the breakdown below is load-bearing.

### Event flow & threading (spans all files)

1. `event_monitor` spawns a **dedicated thread** running a `CGEventTap` + `CFRunLoop`. It detects shortcut press (`CmdTabPressed`) and modifier-release (`CmdReleased`) and sends `GlobalEvent`s over a `flume` channel. The active shortcut (Cmd vs Opt) is the `SHORTCUT_IS_CMD` atomic, toggleable from the menu. `Option+V` is also detected here (`ClipboardToggled`); it is passed through untouched when the clipboard feature is disabled, so other apps can use the combo.
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
| `mouse/` | Mouse control: a second HID-level `CGEventTap` for scroll/button events, scroll modes (default/line), pointer acceleration control, and per-device matching (`device.rs` / `resolve.rs`). |
| `clipboard.rs` | Clipboard history: `NSPasteboard` polling + change notifications, history/pin/dedup/delete logic, the Option+V picker and auto-paste. |
| `menu.rs` | Status-bar menu and action callbacks. |
| `logger.rs` | Async logging (bounded channel, background writer). |
| `ffi.rs` / `theme.rs` | FFI primitives (CF/CG/NSString helpers, `Send`/`Sync` wrappers) and theme/layout accessors. |

### Key design points

- **AX is authoritative** for window filtering: a CG window must match an AX `AXStandardWindow` (paired by `CGWindowID` via the private `_AXUIElementGetWindow`), so popups/panels/dropdowns are dropped.
- **Titleless windows**: apps with a custom title bar (e.g. Microsoft To Do) expose an empty `AXTitle`; their windows are kept via a `titleless_pids` set, and only the *display* layer substitutes a placeholder (`"-"`). The stored title stays empty so `raise_ax_window` can still match the AX window.
- **macOS version split**: macOS 26+ renders with `NSGlassEffectView` (Liquid Glass); older macOS falls back to `NSVisualEffectView` (withinWindow + Dark).
- **Some calls use raw `objc_msgSend` FFI** because `objc2`'s `msg_send!` can't encode CF/CG types or void returns.

## <img height="16" src="docs/icons/gear.svg">&nbsp;&nbsp;Configuration

`~/.config/oh-my-tab/config.toml` — auto-created with defaults on first run. Loading is **per-field resilient**, not all-or-nothing: invalid fields fall back to defaults (logged, never fatal). Reloadable at runtime via the menu (*Reload Config*), which also re-applies theme and refreshes the overlay.

```toml
[appearance]
theme = "light"          # "dark" | "light" | "auto"
glass_style = "clear"    # "regular" | "clear"
glass_tint = "eeeeee66"  # RRGGBBAA
corner_radius = 32.0

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
card_bg_sel     = "3460AF11"
card_border_sel = "3460AF24"

[fonts]
status_bar_size   = 13.0
status_bar_weight = 0.23
title_size        = 11.0
title_weight      = 0.23
app_name_size     = 13.0
app_name_weight   = 0.5

[keyboard]
modifier = "command"     # "option" (Option+Tab) | "command" (Cmd+Tab)

[i18n]
locale = "auto"          # "auto" | "en" | "zh-Hans" | "zh-Hant"

[windows]
enabled = true            # app-switcher master switch (off = Cmd+Tab passes through to the system)
show_minimized = false   # show minimized windows in the overlay

[logging]
level = "info"           # "debug" | "info"
file_path = ""           # empty = default timestamped path; see Logging below

[startup]
launch_at_login = false  # launch at login (requires running as a .app bundle; macOS 13+)

[clipboard]
enabled = false          # clipboard history master switch (off by default)
max_entries = 50         # max history entries (1..=100)
persist = false          # save history to disk so it survives restarts (see the privacy note below)
auto_expire_days = 30    # unpinned entries expire after N days (memory AND disk); 0 = off
pin_follow_selection = true # after pin/unpin, move the selection to the toggled entry (false = keep the current position)
```

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

```toml
[mouse]
enabled = true           # master switch for the mouse-control event tap

# The first profile without device_* fields is the default ("all mice") layer.
# Additional profiles match a specific device by VID/PID and override the default
# per-field. Effective config = default layer merged with the matching device layer.
[[mouse.profiles]]
reverse_scroll = false   # flip scroll direction relative to the system
scroll_mode = "default"  # "default" | "line" (fixed lines per tick)
line_count = 3           # lines per tick in "line" mode (1..=10)

[mouse.profiles.pointer]
disable_acceleration = false  # disable system pointer acceleration (linear tracking)

# Button mappings: bind middle/side buttons (button number >= 2) to actions (keyboard
# shortcut / system action / disabled). Left (0) / right (1) can't be bound (you'd lock
# yourself out of clicking). Button numbers: 2 = middle, 3 = back, 4 = forward, 5+ = other
# side/macro buttons (they vary per mouse -- configure per device). Values can be a shortcut
# ("cmd+shift+v"), a system action ("missioncontrol"/"launchpad"/"showdesktop"/"appexpose",
# fired via Dock's private notification -- immune to system-shortcut occupancy), or "none"
# (swallow the button; it becomes inert). Binding Cmd+Tab / Option+V opens OUR overlay /
# clipboard (dispatched internally, no synthesized events).
# 按键映射:把中键/侧键(按钮号 >= 2)绑定成快捷键(按下 = keyDown,松开 = keyUp)。
# 左键(0)/右键(1)不允许绑定,防止把自己锁死。按钮号是鼠标按键编号:2 = 中键,3 = 后退,
# 4 = 前进,5+ = 其他侧键/宏键(不同鼠标可能不同,按设备分别配置)。
# 绑定 Cmd+Tab / Option+V 会打开**我们自己的**浮窗/剪贴板(合成事件回环到本应用的 tap)。
[ mouse.profiles.button_mappings ]
"3" = "cmd+shift+v"
"4" = "alt+tab"

# Per-profile mappings master switch (default true; false skips this mouse's mappings and
# events pass through). Independent per device -- different mice can differ; the "All Mice"
# default profile can carry its own value too.
button_mappings_enabled = true

# Example per-device override layer (Logitech MCHOSE G3 V2):
[[mouse.profiles]]
device_vendor_id = 10007
device_product_id = 12976
reverse_scroll = true
scroll_mode = "line"
line_count = 3

```

Mouse settings are also exposed in the Settings window (a **device picker** lists each connected mouse; pick one to edit its layer). The button-mappings section lists bound rows (button name + action description + keycaps); clicking **Edit** opens an edit panel (same as LinearMouse): record the trigger side button, pick the action type (Default / None / Key Press / Mission Control / Launchpad / Show Desktop / App Expose), and record the combo for Key Press, then confirm. Toggling `mouse.enabled` takes effect immediately — the mouse event tap is hot-switched on OK, no app restart needed.

## <img height="16" src="docs/icons/note.svg">&nbsp;&nbsp;Logging

Logging is asynchronous and designed to **never block the UI / event loop**.

- **Async pipeline**: the `log_debug!` / `log_info!` macros format a line and send it over a **bounded** `flume` channel (capacity 512) to a background writer thread. The caller never blocks — if the channel is full (e.g. disk writes stall), the **newest** entries are dropped (drop-newest) rather than stalling the caller. Normal load never triggers drops.
- **Destination**: `cargo run` (dev) → both stdout **and** the log file; a packaged `.app` (prod) → log file only.
- **Default file path**: `~/Library/Logs/oh-my-tab/oh-my-tab-<startup-timestamp>.log` — one file **per app launch**, where the timestamp is the process start time in a filename-safe form (e.g. `oh-my-tab-2026-07-25_17-08-30.log`).
- **Automatic 30-day cleanup**: at startup, the default log directory is scanned and any `oh-my-tab-*.log` whose modification time is older than 30 days is deleted. The current run's file is never pruned (its mtime keeps updating as it is written). Cleanup matches only the `oh-my-tab-*.log` pattern, so unrelated files in the same directory are left alone.
- **Within a single long run** the file still grows — there is no size-based rotation. The 30-day cleanup is cross-run, by mtime.
- **stderr capture**: at startup, stderr (fd 2) is redirected into the log pipeline — NSLog / AppKit internal messages (e.g. `[Menu_Tracking]` warnings) and Rust panics appear in the log at **Info** level with a `[stderr]` prefix instead of only in the terminal.
- **Privacy**: debug logs never record the actual keystrokes. The switcher's key tap logs only `Tab` / `Command` / `Option` (and the summon combo name); every other key is logged as plain `Other` — no keycodes, no modifier details, so passwords and typed text never reach the log.

### Custom log path — and why the app never touches it

`[logging] file_path` lets you override the log destination by editing `config.toml` directly. It is **not exposed in the Settings UI** — to change it, edit `~/.config/oh-my-tab/config.toml` yourself.

When `file_path` is set (non-empty), the logger uses that path **verbatim**, in append mode. Crucially:

- **No timestamp is added** to a user-supplied path.
- **No cleanup is performed** on it.

The app deliberately **never writes extra files into, and never deletes any files from, a user-specified location.** If you point `file_path` at your own file or directory, you own its rotation and retention — the 30-day auto-cleanup applies *only* to the default `~/Library/Logs/oh-my-tab/` directory. This keeps the logger from surprising you by creating or deleting files in a location you explicitly chose to control.

## <img height="16" src="docs/icons/globe.svg">&nbsp;&nbsp;Internationalization

- Handcrafted TOML, zero dependencies, embedded at compile time via `include_str!` from `locales/{en,zh-Hans,zh-Hant}.toml` (no runtime file IO, no missing-file risk).
- Driven by `config.i18n.locale`: `"auto"` (default) | `"en"` | `"zh-Hans"` | `"zh-Hant"`. `"auto"` resolves from the system `NSLocale` preferred languages (scanned in order).
- **Hot-reload**: menu and settings re-title on config reload, and on system language change when `locale` is `auto`.
- **Adding a language**: create `locales/xx.toml` (same keys), register it in `i18n::locale_raw()`, add it to the `validate()` allow-list in `config.rs`, and extend `map_tag_to_supported()` if `auto` should map a system tag to it.

## <img height="16" src="docs/icons/database.svg">&nbsp;&nbsp;Icon cache

`~/Library/Caches/oh-my-tab-icons/` — keyed by the app's **bundle identifier** (e.g. `com.microsoft.edgemac`), not by PID. Each app is stored as a pair of files:

- `{bundle-id}.png`: the rendered icon image, read directly by the overlay cards.
- `{bundle-id}.meta`: a tiny text file holding the app executable's modification time (mtime, seconds since the UNIX epoch).

The `.meta` sidecar is the **update signal**. A bundle id stays the same across app updates, so the mtime is what tells us the app was updated or reinstalled: on a cache hit the stored mtime is compared against the executable's current mtime, and a mismatch invalidates the `.png` and forces a re-extract. Without it, an app that changed its icon in an update would keep showing the old one until the cache was cleared manually.

Keying by bundle id means **PID recycling can never serve another app's stale icon** — the old `{pid}.png` design did exactly that, where a recycled PID would hit a leftover file from a different app. It also means the cache survives oh-my-tab restarts. Non-bundle apps (no bundle id) fall back to a hash of the executable path as the key, and legacy `{pid}.png` files from older versions are migrated away once at startup.

Icons are pre-cached at startup and on `NSWorkspaceDidLaunchApplicationNotification`. The cache can be cleared from the menu (*Clear Icon Cache*).

## <img height="16" src="docs/icons/heart.svg">&nbsp;&nbsp;Credits

The **mouse control** feature (scroll reversal, scroll modes, per-device configuration, and pointer-acceleration control) is inspired by and references [LinearMouse](https://github.com/linearmouse/linearmouse). We re-implemented its core features from scratch in pure Rust (via `objc2` FFI, no Swift bridge) and integrated them into oh-my-tab's configuration model. Many thanks to the original author and the LinearMouse project for the excellent work.

The **window switcher** (overlay design, card-based selection, Liquid Glass styling) draws inspiration from [BetterCmdTab](https://github.com/rokartur/BetterCmdTab). We re-implemented the ideas from scratch in pure Rust (via `objc2` FFI, no Swift bridge). Many thanks to the author for the excellent work.
