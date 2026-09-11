<p align="center">
  <img src="assets/Icon-512x512.png" width="120" height="120" alt="oh-my-tab">
</p>

<br />

<div align="center"><b>——&nbsp;&nbsp;&nbsp;Bring the Windows way to your MacBook: thumbnail window switching, clipboard history &amp; reversed mouse scrolling&nbsp;&nbsp;&nbsp;——</b></div>

<br />

<p align="center">
  <a href="https://github.com/eacryo/oh-my-tab/releases"><img src="https://img.shields.io/github/v/release/eacryo/oh-my-tab?style=for-the-badge" alt="GitHub release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue?style=for-the-badge" alt="MIT License"></a>
  <a href="https://github.com/eacryo/oh-my-tab"><img src="https://img.shields.io/badge/platform-macOS-black?style=for-the-badge" alt="macOS"></a>
</p>

<br />

<p align="center">
  <a href="README-ZH.md">简体中文</a> | English
</p>

<p align="center">
  Official Website: <a href="https://oh-my-tab.app/">https://oh-my-tab.app/</a>
</p>

<br />

oh-my-tab is a macOS window switcher that complements the system Cmd+Tab: it runs as a **menu-bar accessory** app (no Dock icon), intercepts a global shortcut (**Command+Tab** by default, toggleable to Option+Tab), shows a floating **Liquid Glass** overlay of cards for currently-open windows, and raises the selected window on release (via a private SkyLight API plus AX).

It is written in pure Rust, calling AppKit / CoreGraphics / ApplicationServices directly through `objc2` FFI — there is no Swift bridge and no Rust UI framework.

- <img height="14" src="docs/icons/stack.svg"> **Native switcher**: app names, window titles, one card per window, across multiple displays.
- <img height="14" src="docs/icons/image.svg"> **Window thumbnails**: caption row above a 16:10 live preview, captured via a private WindowServer API and cached in memory — cached frames render instantly and a background refresh keeps them current; rows are balanced when they fit, and when they overflow the grid fills in MRU order and scrolls continuously. Requires **Screen Recording** permission — without it the switcher falls back to icon-only cards. Turning thumbnails off immediately releases cached window frames from memory.
- <img height="14" src="docs/icons/history.svg"> **Window-level MRU**: switching one window keeps the app's other windows in their existing order.
- <img height="14" src="docs/icons/eye.svg"> **Full window visibility**: every real window, including off-screen and minimized (toggleable).
- <img height="14" src="docs/icons/key.svg"> **Keyboard navigation**: Tab, Shift+Tab, arrow keys, or mouse after Command/Option; the shortcut can be switched to Option+Tab.
- <img height="14" src="docs/icons/tools.svg"> **Window control**: maximize, snap to halves or quarters, or minimize with Option+arrow keys, and move windows across displays with Option+Shift+arrow keys.
- <img height="14" src="docs/icons/zap.svg"> **Quick actions**: Option+I opens Settings, Option+E opens Finder, Option+D shows the desktop, Option+L locks the screen, and double-tapping Control locates the pointer.
- <img height="14" src="docs/icons/copy.svg"> **Clipboard history** (optional): text, images, file copies — search, pin, delete, expiry, persistence ([Clipboard history](#clipboard-history)).
- <img height="14" src="docs/icons/sliders.svg"> **Mouse control** (optional): scroll modes, reversal, per-device acceleration, and **side-button → shortcut mapping**.
- <img height="14" src="docs/icons/star.svg"> **Appearance**: light, dark, or system theme, plus Liquid Glass styling (`NSGlassEffectView`, with an `NSVisualEffectView` fallback), tint, corner radius, and font size.
- <img height="14" src="docs/icons/gear.svg"> **Settings**: configure appearance and features from the Settings window, with changes applied immediately.
- <img height="14" src="docs/icons/globe.svg"> **Zero-dependency i18n**: English / Simplified / Traditional Chinese, following the system language live.
- <img height="14" src="docs/icons/package.svg"> **Lightweight**: pure Rust with a bounded in-memory thumbnail cache — no Electron/Tauri runtime.
- <img height="14" src="docs/icons/note.svg"> **Per-launch logs**: 30-day retention ([Logging](#logging)).

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

<div align="center"><img src="docs/pictures/main_window.png" width="640" alt="Main window"></div>

<div align="center"><img src="docs/videos/settings_page.gif" width="560" alt="Settings page demo"></div>

## Quick start

- **Window switching**: hold Command (or Option, if configured), press Tab / Shift+Tab or the arrow keys to pick a window, then release the modifier to switch.
- **Clipboard history**: once enabled, press **Option+V** to summon it; it supports both keyboard and mouse, and closes when you click outside.
- **Permissions**: window switching needs Accessibility permission; thumbnails need Screen Recording. Without Screen Recording, switching still works and cards simply fall back to icons.

## <img height="16" src="docs/icons/copy.svg">&nbsp;&nbsp;Clipboard history

Optional (off by default). Summon with **Option+V**, navigate with the arrow keys / Enter / Esc / Backspace, or click; clicking outside closes it. Extra keys: **← pins/unpins** the selected entry; **→ opens a detail panel** beside the picker showing the full untruncated text or a large image preview (it follows ↑/↓ browsing live; Esc, ←, →, or a click on it closes it). The history records **three kinds of entries**:

| Kind | What is stored | Paste behavior |
|---|---|---|
| **Text** | The copied text, held in memory | The text is written back and Cmd+V is synthesized |
| **Image data** | An image copied inside an app (e.g. right-click → "Copy Image"): the original-format bytes are hashed and kept in a disk cache; only a downsampled thumbnail stays in RAM | The original bytes are written back under their original UTI, preserving the format: JPG remains JPG and animated GIF remains GIF |
| **Image file** | An image FILE copied in Finder (Cmd+C): read once at copy time for a content hash and a thumbnail, then the bytes are discarded — only the path is kept | `public.file-url` is restored (file semantics, like Windows Win+V / Maccy): Finder duplicates the file, chat apps attach it. If the source file has been deleted, the paste is skipped |

> **Known v1 tradeoffs** — each entry records exactly one kind of content: a copy carrying **both text and an image** (e.g. copying an image from a web page) records only the text; **multiple-file copies and single non-image file copies are omitted from history**. The same picture copied both as an image and as a file stays as two separate entries (they answer different paste semantics). Dedup is per-kind: text by exact content, images by content hash.

**Using an entry reorders the history by default** (like Maccy): selecting an entry and pressing Enter writes it back to the pasteboard, which the recorder sees as a re-copy and moves to the top. The **"Move used entries to the top"** switch in Settings turns this off (like Windows Win+V). With the optional **"Delete entry after paste"** switch on, holding **Option** while pressing Enter or clicking a row pastes the entry and removes it from the history right away (one-shot paste). Its dependent **"Also delete the corresponding system clipboard item"** switch additionally removes the corresponding clipboard content after a short delay, if no newer copy replaced it. The picker's "Clear history" keeps pinned entries. An optional **"Save clipboard history to disk"** switch persists the history across restarts — see the privacy note below.

> **Clipboard persistence is off by default.** Enabling it writes copied text, filenames, and image data to disk in plain text, so do not enable it if you copy passwords or tokens. See the [official website](https://oh-my-tab.app/) for details.

## <img height="16" src="docs/icons/alert.svg">&nbsp;&nbsp;Known Issues

~~**Some background-app thumbnails may temporarily appear white**: WindowServer can only capture the surface an app currently provides. A long-suspended WebView app (for example, Clash Verge Rev) may return its title bar with a white content area, especially just after oh-my-tab starts with an empty in-memory thumbnail cache. Activating the app and allowing its content to redraw lets a later capture recover the preview.~~ **Resolved**: blank captures are detected before caching and never overwrite a real thumbnail — the only exception is light/dark appearance changes, which re-capture every card (blank placeholder included) so no stale-theme frame lingers. A suspended WebView window keeps its last real frame; before its first activation the card shows a placeholder frame instead, and switching to the window (across apps or between windows of the same app) refreshes it automatically.

**Telegram's fullscreen image viewer has no separate thumbnail**: Telegram's media viewer is a special high-level floating window above its normal windows. To avoid treating it as a separate switchable window, oh-my-tab excludes it from the window list and thumbnail capture. While the viewer is open, the switcher displays Telegram's main-window thumbnail.

**Some application windows may be unavailable to thumbnail capture**: Certain applications mark editor or other protected windows as non-shareable, or render their content on a protected surface. Even with Screen Recording permission granted, other windows from the same application still capture normally. In this case the window remains switchable, but its thumbnail may stay on the placeholder or last valid frame. This is a limitation of how the application or WindowServer shares window content, not a window-identification bug.

If windows are already open when the app starts, their initial ordering is seeded from WindowServer's front-to-back order. This provides an initial approximation; live activation events refine the window-level MRU after launch.


Development-only issues and raw-binary debugging notes are collected in [docs/developer-notes-en.md](docs/developer-notes-en.md).

## <img height="16" src="docs/icons/tools.svg">&nbsp;&nbsp;Requirements

- macOS 13+ on Apple Silicon.
- **Accessibility** permission granted to the app.

## <img height="16" src="docs/icons/terminal.svg">&nbsp;&nbsp;Build & run

**Prerequisites:** Rust stable toolchain, Xcode Command Line Tools (`xcode-select --install`), macOS 13+. Accessibility permission is required at runtime (see Permissions below).

### Development

> ```sh
> cargo fmt
> cargo check       # fast type-check
> cargo clippy
> cargo test        # unit tests; add -- --ignored for the CG/AX smoke tests
> ./scripts/dev-restart.sh  # build, sign, and launch the development .app
> ```

`scripts/dev-restart.sh` builds and assembles a separately signed development `.app`, then launches it through the per-user `launchd` domain. This keeps Accessibility and Screen Recording permissions associated with the development bundle and ensures the running process contains the latest build. Layout QA fixtures, the debug-only layout assertions, and the GUI smoke test are documented in [docs/developer-notes-en.md](docs/developer-notes-en.md).

### Release

> ```sh
> sh scripts/bundle.sh        # cargo build --release -> .app -> sign -> .dmg + Sparkle .zip
> open dist/Oh-My-Tab.dmg     # install: drag Oh-My-Tab into Applications
> ```

`bundle.sh` assembles `dist/Oh-My-Tab.app` (release binary, `Info.plist`, and app icon resources), signs it, then packages `dist/Oh-My-Tab.dmg` (with an `Applications` symlink for drag-to-install) and the Sparkle archive `dist/Oh-My-Tab.zip`. The outputs live in `dist/` (gitignored), outside `target/`, so the logger treats them as production builds (file logging, not stdout); running the `.app` is what enables launch-at-login (SMAppService) and file logging. Re-run the script after code changes — it copies the release binary at build time and self-locates the repo root.

The full release pipeline (`release.sh` / `release-dev.sh`, including `--push`), the isolated development channel (bundle ID `com.eacryo.oh-my-tab.dev`), the R2 upload flow, Sparkle automatic updates, and code signing are documented in [docs/releasing-en.md](docs/releasing-en.md).

## <img height="16" src="docs/icons/shield-lock.svg">&nbsp;&nbsp;Permissions & runtime caveats

- The app requires **Accessibility** permission (`AXIsProcessTrusted`) for both the global key event tap and the AX window queries. Grant it under *System Settings → Privacy & Security → Accessibility*. A freshly built binary must be re-granted — unless you sign with a stable identity (see [Code signing](docs/releasing-en.md#code-signing-why-a-self-signed-certificate-stabilizes-permissions)), in which case the grant persists across rebuilds.
- **Window thumbnails** additionally require the **Screen Recording** permission (System Settings → Privacy & Security → Screen Recording). A private WindowServer capture API is used, same as DockDoor/AltTab. Without it the switcher silently keeps icon-only cards; granting it later resumes thumbnail capture without restarting. Frames are kept **in memory only** — nothing is ever written to disk.
- If the event tap fails to create, the app prints an error and the shortcut silently does nothing — almost always a missing Accessibility grant.
- Icon cache: `~/Library/Caches/oh-my-tab-icons/{bundle-id}.png` (keyed by bundle id, with a `.meta` mtime sidecar; clearable from the menu).

## <img height="16" src="docs/icons/gear.svg">&nbsp;&nbsp;Settings

All options are managed from the in-app Settings window and apply immediately. The window covers appearance, window switching, window control, quick actions, clipboard history, mouse control, startup, and updates.

## <img height="16" src="docs/icons/note.svg">&nbsp;&nbsp;Logging

- **Destination**: the development `.app` launched by `scripts/dev-restart.sh` and packaged `.app` builds write to the log file. A raw `cargo run` also writes to stdout and is reserved for low-level debugging.
- **Default file path**: `~/Library/Logs/oh-my-tab/oh-my-tab.log`. When the active file reaches 10 MB, it rolls through `oh-my-tab.log.1` to `oh-my-tab.log.5` and keeps the newest five backups. Each launch writes a session marker so runs remain distinguishable. Legacy per-launch logs and stale backups older than 30 days are deleted at startup.
- **Log level**: can be changed from the Settings window when troubleshooting is needed.
- **Memory diagnostics**: at the `info` log level, after roughly 60 seconds and then every 5 minutes, the app records one `[mem]` line containing the active feature profile (`mouse:on|off`, `thumbs:on|off`, `clipboard:off|memory|persistent`), process footprint/RSS, sampled footprint peak, thread count, and estimated thumbnail/clipboard/window ledgers. `footprint` is the macOS physical-footprint metric used for the Activity Monitor Memory column and is the primary number for memory pressure; `rss` is current resident memory and can fall as macOS compresses or reclaims pages. `footprint_peak_sampled` is sampled by the app, while `rss_peak_kernel` is the kernel's lifetime high-water mark. Clipboard memory is split into text, preview, and metadata estimates; original image bytes in the disk cache are not counted as resident memory. Persistence mainly changes history lifetime and startup restoration; the per-entry RAM model remains the same. Clipboard contents and window imagery are excluded from the log.
- **Privacy**: debug logs record only `Tab` / `Command` / `Option` (and the summon combo name) from the switcher's key tap; every other key is logged as plain `Other`, without keycodes or modifier details.

## <img height="16" src="docs/icons/heart.svg">&nbsp;&nbsp;Credits

The **mouse control** feature (scroll reversal, scroll modes, per-device configuration, and pointer-acceleration control) is inspired by and references [LinearMouse](https://github.com/linearmouse/linearmouse). We re-implemented its core features from scratch in pure Rust (via `objc2` FFI, no Swift bridge) and integrated them into oh-my-tab's configuration model. Many thanks to the original author and the LinearMouse project for their work.

The **window switcher** (overlay design, card-based selection, Liquid Glass styling) draws inspiration from [BetterCmdTab](https://github.com/rokartur/BetterCmdTab). We re-implemented the ideas from scratch in pure Rust (via `objc2` FFI, no Swift bridge). Many thanks to the author.
