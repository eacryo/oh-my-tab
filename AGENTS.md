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

After every successful restart, `scripts/dev-restart.sh` prints the current timestamp-based `build-version` (the `CFBundleVersion` written into the app bundle). Always report that `build-version` back to the user after a restart so it is clear exactly which build is running.

## Architecture and invariants

- AppKit UI and dynamically registered Objective-C callbacks belong on the main thread. Global input and device observers run on dedicated threads and marshal events back to the main thread.
- Shared application state is protected by the existing mutexes/RwLocks. Preserve the ownership and lifetime rules around raw Objective-C pointers and Core Foundation objects.
- The overlay has two card modes: icon-only and thumbnail. Layout helpers in `theme.rs` are intentionally pure where possible so geometry and navigation can be unit-tested. Thumbnail frames are memory-only and require Screen Recording permission; the overlay must still work without that permission.
- Dynamic card classes cannot reliably expose Rust-side properties through `msg_send!`; use the existing card-index/key maps when changing card reconciliation or selection logic.
- Window collection uses the Accessibility window list as the authority for which on-screen windows are switchable. Keep stored AX titles intact for activation; display-only fallbacks belong in the presentation layer.
- Configuration is loaded per field: invalid values fall back to defaults without discarding valid settings. Runtime reload must preserve this behavior and refresh affected UI.
- Mouse profiles match devices by VID/PID. The mouse event tap is separate from the switcher event tap, and pointer settings must be reapplied after device reconnects.
- Clipboard history is optional and disabled by default. When disabled, recording and the Option+V picker are gated off. Sensitive pasteboard markers must never be recorded, and persistence is opt-in because history can contain private plaintext or file references.
- When modifying the Settings UI, prefer reusing the existing semantic components (`SettingsSection`, `SettingsCard`, `SettingsRow`, `SettingsControl`, and `SettingsButton`) before adding page-specific layout or control code. Extend an existing component when the behavior is shared; only bypass the component layer when the control has genuinely different interaction or rendering semantics.

For detailed subsystem behavior, inspect the relevant module and `docs/developer-notes.md` rather than expanding this file with implementation history.

## Runtime requirements

- Accessibility permission is required for the global event tap and AX window operations. If the shortcut does nothing, check this permission first.
- Screen Recording permission is required for thumbnail capture; missing permission should degrade to the icon/fallback presentation rather than break switching.
- Runtime configuration is stored at `~/.config/oh-my-tab/config.toml`.
- Application logs default to `~/Library/Logs/oh-my-tab/oh-my-tab.log`; the active file rolls at 10 MB into `.1` through `.5`, and each launch writes a session marker. Legacy logs and stale backups older than 30 days are pruned from the default directory. A non-empty `logging.file_path` overrides this behavior and is appended to verbatim without automatic rotation or cleanup.

## Editing conventions

- Preserve unrelated user changes in a dirty worktree.
- Avoid destructive Git commands (e.g. `reset`/`checkout`/`clean` that discard modifications) unless explicitly requested.
- Add bilingual comments (Chinese first, English second) only for non-obvious logic, important design decisions, FFI/Objective-C subtleties, and workarounds.
- Keep user-visible strings in `t()`/`tf()` and add translation keys to all supported locale files. Developer logs remain in English; dynamic window/application titles are data, not UI chrome.

## File editing

Prefer the harness's native file-editing tools (e.g. Edit, Write, or apply_patch) when modifying source code.

- Use Edit/apply_patch for targeted changes to existing files.
- Use Write when creating new files or intentionally replacing an entire file.
- Do not use Python, shell scripts, sed, perl, or similar tools for ordinary source-code edits or simple text replacement.
- Scripts are acceptable for genuinely programmatic or bulk transformations, code generation, migrations, or changes spanning many files where scripted editing is clearly safer and more efficient.
- Before running a script that modifies files, leave an escape hatch: back up the target files (e.g. `cp file file.bak`) or confirm the current state is recoverable via Git.
- Scripts must fail loudly: assert that every anchor/marker was found before editing, and abort (re-reading the file) when any is missing. Never continue with default or empty bounds from a failed search.
- When using a script to modify source files, inspect the resulting diff (`git diff`) before considering the task complete.

## Git and commits
If user only input "cmsg", then give user a commit message follow below rule:
Unless the user explicitly requests it, do not commit, push, stage, or rewrite history on the user's behalf. 
When a commit message is requested, provide only one Conventional Commits line:

`type: description`

Before generating a commit message, inspect all three Git views: `git diff --cached` for staged
changes, `git diff` for unstaged changes, and `git diff HEAD` for the final combined worktree
diff. Base the message on the combined `git diff HEAD` result. Do not treat an `AD` status as a
net addition without checking the combined diff: it may be an index addition that is deleted in
the worktree.

Note that the Style word only used for code style change, not UI style change.
UI style change usually use feat or fix.

If the user replies exactly "allow commit" after a commit message was provided in this conversation, inspect git status and all three Git diff views first. Commit only the intended changes with that exact message; do not stage unrelated changes. Ask if the scope is ambiguous.

If the user replies exactly "allow push" after a commit message was provided in this conversation, inspect the worktree first, commit only the intended changes with that exact message, then push the current branch to its configured upstream. Ask if the scope or upstream is ambiguous.