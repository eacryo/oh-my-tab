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
- `scripts/dev-restart.sh` defaults to a **debug** build (`cargo build`): fast iteration with every debug assertion on (objc2 `msg_send!` signature verification, `debug_assert_main_thread`, overflow checks). Use it for functional iteration and handoff.
- For feel/perf validation (scrolling, animation, latency), run `scripts/dev-restart.sh --opt`. It uses the `dev-opt` cargo profile (`target/dev-opt/`): optimized like release while keeping `debug-assertions`, so runtime speed is representative but development still fails fast. Neither mode changes the dev bundle identity (`com.eacryo.oh-my-tab.dev`); both write `dist/Oh-My-Tab-Dev.app`, which `scripts/release-dev.sh` also uses for the dev channel.
- Report the timestamp-based `build-version` printed by the script.
- Pass development switches through the script instead of editing code: any `--flag[=value]` argument the script does not own is forwarded to the app (and `scripts/dev-restart.sh -- <args>` forwards argv verbatim), so a new switch needs no script edit. Switches apply to that launch only — the script pkills old instances first. Use this to reach states that are otherwise hard to reproduce (first-run onboarding, permission branches, update notices); a feature that only appears in such a state should expose a `--`-style switch for verification, and it is parsed through `crate::dev_flags`.
- **The running app must not read or receive environment variables as configuration.** Pass development switches as argv through `scripts/dev-restart.sh`; `crate::dev_flags` and the script's argv passthrough are the only runtime configuration channels. Build and release scripts may read explicitly named packaging inputs, but must not forward the caller's environment to the launched app, dump it, or print sensitive values. The caller's shell may contain cloud credentials and proxies; a missing forward filter leaked the whole environment once, on 2026-09-22.

## Testing tiers

Three layers, split by **who decides pass/fail** — not by which transport drives them (`cua-driver call` and the MCP tools are the same tool set on the same daemon; the CLI is the scriptable face, MCP the agent-facing one).

| Tier | Decides | How | Repeatable | Gate |
| --- | --- | --- | --- | --- |
| A1 | script | `cargo test` plus the `--smoke-*` runners (real AppKit view tree, headless-safe) | yes | yes, every change |
| A2 | script | `scripts/e2e/*.sh`, run through `scripts/e2e/run-all.sh` (the entry point; scenarios that steal focus are skipped unless `--include-focus`), driven by the `cua-driver` CLI and asserting app-written JSON state (`--e2e-state=<path>`) plus real WindowServer state | yes | before handoff/release |
| B | agent or person | MCP tools plus screenshots: taste, first-pass UI review, failure triage, bug investigation | no | never |

- Anything assertable belongs in A. Big problems usually are (state and OS facts), small ones often are not (visual taste) — severity is not the split, assertability is.
- A2 covers what only real input reaches: the global hotkey → summon → raise path, permission branches, cross-app behavior. `--e2e-state` exists because AX cannot express CALayer content (the sidebar highlight pill) or internal state (the selected card index), so the app states those facts itself instead of the test guessing from pixels. A quick hotkey press-release legitimately skips the display path; display and layout assertions belong to A1 smoke runners.
- **Promotion rule: every bug found in tier B must land an assertion in tier A** — or, when it cannot be asserted yet, a state field that makes it assertable. Otherwise the same regression returns unnoticed.
- A2 hotkey scenarios press through the system event stream and therefore steal focus: run them only with the user's consent, never in a background loop.

## Architecture and invariants

- AppKit UI and dynamically registered Objective-C callbacks run on the main thread. Global input and device observers run on dedicated threads and marshal events back to main.
- Preserve existing mutex/RwLock ownership and raw Objective-C/Core Foundation lifetimes.
- The overlay supports icon-only and thumbnail cards. Thumbnail capture needs Screen Recording permission and must degrade gracefully to icons/fallbacks without it. Keep layout helpers pure where possible for unit testing.
- Dynamic card classes must not depend on Rust-side properties exposed through `msg_send!`; use the existing card-index/key maps.
- The Accessibility window list is authoritative for switchable windows. Preserve stored AX titles for activation; presentation-only fallbacks belong in the UI layer.
- Configuration is loaded per field: invalid values fall back individually without discarding valid settings. Runtime reload must preserve this and refresh affected UI.
- Mouse profiles match VID/PID; the mouse event tap is separate from the switcher tap, and pointer settings must be reapplied after reconnects.
- Clipboard history is optional and off by default. Gate recording and Option+V when disabled, never record sensitive pasteboard markers, and persist only when explicitly enabled.
- Settings UI should reuse `SettingsSection`, `SettingsCard`, `SettingsRow`, `SettingsControl`, and `SettingsButton`; extend shared components when behavior is shared. A `SettingsButton`'s `tag` **selects its hover/normal palette** (the hover handlers read it: primary blue, action, compact grey), so never carry an action id in `setTag:` — keep the component's tag and map the sender pointer to an action id instead, or the button's colour gets stuck after the pointer leaves.

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
