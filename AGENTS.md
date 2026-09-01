# AGENTS.md

This file provides guidance that all agents must follow when working with code in this repository.

## Project overview

`oh-my-tab` is a macOS app/window switcher. It runs as an accessory app, intercepts the configured global shortcut (Command+Tab or Option+Tab), shows a floating overlay of application/window cards, and raises the selected window through the Accessibility API. It also includes optional mouse-enhancement and clipboard-history modules.

The application is written in Rust and calls AppKit, CoreGraphics, and ApplicationServices directly through `objc2` FFI. There is no Swift bridge or Rust UI framework.

## Build & run

```sh
cargo fmt
cargo check
cargo clippy
cargo test
scripts/dev-restart.sh
```

The normal test suite is headless-safe. GUI/permission-dependent smoke tests are marked `#[ignore]` and are not part of the normal `cargo test` run.

Choose verification according to the scope of the change:

- Documentation, comments, README/localization text, and `AGENTS.md` changes do not require Rust tests.
- Rust code changes must run `cargo fmt`.
- Behavioral changes must run the relevant targeted tests; add `cargo check` when the change affects compilation or module interfaces.
- Cross-module, unsafe/FFI, concurrency, configuration, build, or release-related changes must run the full gate: `cargo fmt` → `cargo check` → `cargo clippy` → `cargo test`.
- Before handing off a completed feature, always run the full gate. All required checks must pass cleanly.

The app is started through `scripts/dev-restart.sh`, not directly with `cargo run`. After the full gate passes for runtime changes, run the script. It builds the binary, assembles and signs `dist/Oh-My-Tab-Dev.app`, starts it through the user-level launchd wrapper, and verifies that it remains alive. If it reports `restart FAILED`, inspect the newest log under `~/Library/Logs/oh-my-tab/`, diagnose the failure, and repeat the verification and restart.

## Architecture and invariants

- AppKit UI and dynamically registered Objective-C callbacks belong on the main thread. Global input and device observers run on dedicated threads and marshal events back to the main thread.
- Shared application state is protected by the existing mutexes/RwLocks. Preserve the ownership and lifetime rules around raw Objective-C pointers and Core Foundation objects.
- The overlay has two card modes: icon-only and thumbnail. Layout helpers in `theme.rs` are intentionally pure where possible so geometry and navigation can be unit-tested. Thumbnail frames are memory-only and require Screen Recording permission; the overlay must still work without that permission.
- Dynamic card classes cannot reliably expose Rust-side properties through `msg_send!`; use the existing card-index/key maps when changing card reconciliation or selection logic.
- Window collection uses the Accessibility window list as the authority for which on-screen windows are switchable. Keep stored AX titles intact for activation; display-only fallbacks belong in the presentation layer.
- Configuration is loaded per field: invalid values fall back to defaults without discarding valid settings. Runtime reload must preserve this behavior and refresh affected UI.
- Mouse profiles match devices by VID/PID. The mouse event tap is separate from the switcher event tap, and pointer settings must be reapplied after device reconnects.
- Clipboard history is optional and disabled by default. When disabled, recording and the Option+V picker are gated off. Sensitive pasteboard markers must never be recorded, and persistence is opt-in because history can contain private plaintext or file references.

For detailed subsystem behavior, inspect the relevant module and `docs/developer-notes.md` rather than expanding this file with implementation history.

## Runtime requirements

- Accessibility permission is required for the global event tap and AX window operations. If the shortcut does nothing, check this permission first.
- Screen Recording permission is required for thumbnail capture; missing permission should degrade to the icon/fallback presentation rather than break switching.
- Runtime configuration is stored at `~/.config/oh-my-tab/config.toml`.
- Application logs are written under `~/Library/Logs/oh-my-tab/` unless `logging.file_path` overrides the destination.

## Editing conventions

- Preserve unrelated user changes in a dirty worktree.
- Use `apply_patch` for local file edits and avoid destructive Git commands unless explicitly requested.
- Add bilingual comments (Chinese first, English second) only for non-obvious logic, important design decisions, FFI/Objective-C subtleties, and workarounds.
- Keep user-visible strings in `t()`/`tf()` and add translation keys to all supported locale files. Developer logs remain in English; dynamic window/application titles are data, not UI chrome.

## Git and commits

Never commit, push, stage, or rewrite history on the user's behalf. When a commit message is requested, provide only one Conventional Commits line:

`type: description`

Use `fix` for corrections to existing visual/UI behavior and `feat` for new visual/UI behavior; reserve `style` for code-formatting or naming-only changes.
