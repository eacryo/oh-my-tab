# AGENTS.md

Guidance for agents working in this repository.

## Project

`oh-my-tab` is a macOS menu-bar window switcher. It intercepts Command+Tab or Option+Tab, displays a floating application/window overlay, and raises the selected window through Accessibility APIs. Optional modules provide mouse enhancement and clipboard history.

The app is Rust-only and calls AppKit, CoreGraphics, and ApplicationServices through `objc2` FFI; there is no Swift bridge or Rust UI framework.

## Build, run, and verification

```sh
cargo fmt
cargo check
cargo clippy
cargo test
scripts/dev-restart.sh
```

- The normal `cargo test` suite is headless-safe; GUI/permission-dependent smoke tests are marked `#[ignore]`.
- Documentation, comments, localization, and `AGENTS.md` changes need no Rust tests. Rust changes require `cargo fmt`; behavioral changes require targeted tests, plus `cargo check` when interfaces or compilation are affected.
- Cross-module, unsafe/FFI, concurrency, configuration, build, or release changes require the full gate above. Before handing off a completed feature, always run the full gate and keep all checks clean.
- Start the app with `scripts/dev-restart.sh`, never directly with `cargo run`. For runtime changes, run it after the full gate. If it reports `restart FAILED`, inspect the newest log under `~/Library/Logs/oh-my-tab/`, diagnose, and retry.
- After a successful restart, report the timestamp-based `build-version` printed by the script.

## Architecture and invariants

- AppKit UI and dynamically registered Objective-C callbacks run on the main thread. Global input and device observers run on dedicated threads and marshal events back to main.
- Preserve existing mutex/RwLock ownership and raw Objective-C/Core Foundation lifetimes.
- The overlay supports icon-only and thumbnail cards. Thumbnail capture needs Screen Recording permission and must degrade gracefully to icons/fallbacks without it. Keep layout helpers pure where possible for unit testing.
- Dynamic card classes must not depend on Rust-side properties exposed through `msg_send!`; use the existing card-index/key maps.
- The Accessibility window list is authoritative for switchable windows. Preserve stored AX titles for activation; presentation-only fallbacks belong in the UI layer.
- Configuration is loaded per field: invalid values fall back individually without discarding valid settings. Runtime reload must preserve this and refresh affected UI.
- Mouse profiles match VID/PID; the mouse event tap is separate from the switcher tap, and pointer settings must be reapplied after reconnects.
- Clipboard history is optional and off by default. Gate recording and Option+V when disabled, never record sensitive pasteboard markers, and persist only when explicitly enabled.
- Settings UI should reuse `SettingsSection`, `SettingsCard`, `SettingsRow`, `SettingsControl`, and `SettingsButton`; extend shared components when behavior is shared.

For detailed subsystem behavior, inspect the relevant module and `docs/developer-notes-en.md` rather than adding implementation history here.

## Runtime requirements

- Accessibility permission is required for the global event tap and AX operations. Screen Recording is additionally required for thumbnails; both failures must degrade or report clearly rather than break switching.
- Runtime configuration is `~/.config/oh-my-tab/config.toml`.
- Logs default to `~/Library/Logs/oh-my-tab/oh-my-tab.log`; the active file rotates at 10 MB through `.1`–`.5`, writes a launch marker, and prunes legacy/stale backups older than 30 days. A non-empty `logging.file_path` is appended verbatim without rotation or cleanup.

## Editing and localization

- Preserve unrelated user changes and avoid destructive Git commands unless explicitly requested.
- Use bilingual comments (Chinese first, English second) only for non-obvious logic, design decisions, FFI/Objective-C subtleties, and workarounds.
- Keep user-visible strings in `t()`/`tf()`/`t_count()` and add keys to every supported locale; use `t_count()` for singular/plural counts. Developer logs stay in English; dynamic titles are data, not UI chrome.
- Prefer `apply_patch`/native file editors. Use scripts only for genuinely programmatic changes; back up targets, assert all anchors, fail loudly, and inspect `git diff` afterward.

## Git and commits

- Do not commit, push, stage, or rewrite history unless explicitly requested. `style` is for code-style changes, not UI styling.
- If the input is exactly `/cmsg` or `cmsg`, inspect `git diff --cached`, `git diff`, and `git diff HEAD`; base the result on the combined diff and return exactly one English Conventional Commits line: `type: description`. Check `AD` status against the combined diff before deciding scope.
- If the user replies exactly `allow commit` after a message was provided, recheck status and all three diffs, then commit only the intended changes with that exact message; ask if scope is ambiguous.
- If the user replies exactly `allow push`, recheck the worktree, commit only intended changes with that exact message, and push the current branch to its configured upstream; ask if scope or upstream is ambiguous.
