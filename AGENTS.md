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
- Docs, comments, localization, and `AGENTS.md` changes need no Rust tests. Rust changes require `cargo fmt` (plus `cargo check` when interfaces or compilation are affected) and targeted tests when behavior changes.
- Cross-module, unsafe/FFI, concurrency, configuration, build, or release changes require the full gate above. Before handing off a completed feature, always run the full gate and keep all checks clean.
- Start the app with `scripts/dev-restart.sh`, never directly with `cargo run`. For runtime changes, run it after the full gate. If it reports `restart FAILED`, inspect the newest log under `~/Library/Logs/oh-my-tab/`, diagnose, and retry.
- When the onboarding guide does not need verification, launch with `--no-onboarding` (for example, `scripts/dev-restart.sh --no-onboarding`) so it does not appear; only run without it when testing onboarding flows.
- `scripts/dev-restart.sh` defaults to a **debug** build (`cargo build`): fast iteration with every debug assertion on. Use it for functional iteration and handoff.
- For feel/perf validation (scrolling, animation, latency), run `scripts/dev-restart.sh --opt`. It uses the `dev-opt` cargo profile (`target/dev-opt/`): optimized like release while keeping `debug-assertions`, so runtime speed is representative and development still fails fast.
- `release_doc_dev/<Cargo.toml version>.md` must exist and be non-empty: `scripts/dev-restart.sh` aborts before building otherwise, and a dev build embeds that file as its release notes.
- Report the timestamp-based `build-version` printed by the script.
- Pass development switches as argv through `scripts/dev-restart.sh` instead of editing code: any `--flag[=value]` it does not own is forwarded to the app (`-- <args>` forwards argv verbatim), and a switch applies to that launch only.
- Reach otherwise hard-to-reproduce states (first-run onboarding, permission branches, update notices) with such a switch; a feature that only appears in one of them should expose a `--`-style flag parsed through `crate::dev_flags`.
- **The running app must not read or receive environment variables as configuration.** `crate::dev_flags` and the script's argv passthrough are the only runtime configuration channels. Build and release scripts may read explicitly named packaging inputs, but must not forward the caller's environment to the launched app, dump it, or print sensitive values — the caller's shell may hold cloud credentials.

## Testing tiers

Three layers, split by **who decides pass/fail** — not by which transport drives them.

| Tier | Decides | How | Repeatable | Gate |
| --- | --- | --- | --- | --- |
| A1 | script | Pure-headless tests plus `--smoke-*` runners over the real AppKit view tree; GUI-dependent runners require a macOS graphical session | yes | yes, every change (run the applicable tier; an unavailable GUI tier is unrun, not passed) |
| A2 | script | `scripts/e2e/*.sh`, run through `scripts/e2e/run-all.sh` (the entry point; scenarios that steal focus are skipped unless `--include-focus`), driven by the `cua-driver` CLI and asserting app-written JSON state (`--e2e-state=<path>`) plus real WindowServer state | yes | before handoff/release |
| B | agent or person | MCP tools plus screenshots: taste, first-pass UI review, failure triage, bug investigation | no | never |

- Anything assertable belongs in A: severity is not the split, assertability is.
- A1 has two execution environments: pure-headless tests can run without a graphical session; AppKit view-tree smoke runners must actually run in a macOS graphical session. A headless run does not pass or waive an unrun GUI smoke.
- A2 covers what only real input reaches: the global hotkey → summon → raise path, permission branches, cross-app behavior. `--e2e-state` exists because AX cannot express CALayer content or internal state, so the app states those facts itself instead of the test guessing from pixels. A quick hotkey press-release legitimately skips the display path; display and layout assertions belong to A1 smoke runners.
- **Promotion rule: every bug found in tier B must land an assertion in tier A** — or, when it cannot be asserted yet, a state field that makes it assertable. Otherwise the same regression returns unnoticed.
- A2 hotkey scenarios press through the system event stream and therefore steal focus: run them only with the user's consent, never in a background loop.

## Mandatory development workflow

For every non-trivial implementation task:

1. Understand the change and inspect the relevant existing code before modifying anything.
2. If the user asks for a design consultation first (先和 Codex 商量方案, "design first", 先议后做), put the
   approach to the reviewer before writing any code: `./scripts/codex-review.sh --design` with a brief that
   states the goal and constraints, what you inspected, the approach, the alternatives you rejected and why,
   what you are unsure about, and how you intend to verify it. That round judges the approach, not code — the
   reviewer must not write it, and its answer ends with `PLAN-STATUS: AGREED` or `PLAN-STATUS: CONCERNS`.
   Report it like any other round, then decide what the user hears before building: report and carry on when
   the approach is the only reasonable one, and put the choice to the user when there are genuinely different
   approaches with different costs, or when the reviewer's concerns change the direction.
3. Implement the change.
4. Run the applicable gate (see *Build, run, and verification* and *Testing tiers*).
5. Run an independent review — only once steps 3 and 4 are done: implemented, building, and the relevant
   tests passing. A half-finished change is never sent to the reviewer, and the reviewer is never used to find
   compile errors or to explore an unfinished design.
6. Read and evaluate every finding rather than accepting it blindly.
7. Fix all valid BLOCKER, HIGH, and MEDIUM findings; fix LOW findings when they are worth it.
8. Ask the **same** reviewer session to review the fixes, then repeat until no significant findings remain.
9. Only then report the task as complete.

A design round does not satisfy the review requirement: it is answered before anything is built, and step 5
still happens in the same session, so the reviewer can judge whether the implementation followed the approach
it agreed to. "完成后送审" / "review it when you are done" is step 5 restated; 先商量方案 / "design first" is
step 2, and both can be asked together.

A task is not complete because the code compiles, the tests pass, the feature works, or the implementation
looks reasonable. Independent review is mandatory.

## Independent reviewer

The independent reviewer is the Codex CLI, driven by `scripts/codex-review.sh`. The implementation agent must
not act as its own final reviewer.

```sh
scripts/codex-review.sh                     # first review, or a re-review in the same session
scripts/codex-review.sh --design            # put an approach to the reviewer before writing code
scripts/codex-review.sh --note "<text>"     # the agent's own words: verdicts, disagreements, gate results
scripts/codex-review.sh --note-file <path>  # the same, read from a file
scripts/codex-review.sh --retry             # resume a round that produced no verdict
scripts/codex-review.sh --fresh             # start a new session when the old one is gone
scripts/codex-review.sh --timeout <sec>     # bound a run that hangs (default 1800, 0 disables)
scripts/codex-review.sh --finish            # clear the reviewer session for the next task
scripts/codex-review-selftest.sh            # check the helper itself, against a stub CLI, offline
```

- One session per task, and a re-review resumes **that** session so the reviewer remembers its previous
  findings, the agent's answers, and what was already tried. `--retry` resumes a round that produced no
  verdict; a round that reached one is re-reviewed by running the reviewer again. `--finish` clears the session
  at the end of the task.
- Only a round that actually concluded counts. A transport failure, a turn that never completed, a message
  that is not a verdict or a timeout is reported as a failed round with the reason and the archived evidence
  under `.agent-review/rounds/`, and is never counted toward completion. A tree that matches HEAD is refused
  before anything runs — a round that reviewed no change is not a review — with two exceptions: `--design`,
  which is asked before there is anything to review, and `--retry`, which resumes a round that produced no
  verdict on a tree that legitimately has not changed since.
- The reviewer inspects the diff and surrounding code, verifies correctness, and looks for regressions, edge
  cases, concurrency and lifetime problems, error handling, API misuse, performance, architecture, and
  missing tests. It must not modify files, implement fixes, refactor, or resolve findings itself.
- **The reviewer only sees the prompt, the diff and the repository.** Anything it needs and cannot read off
  the code goes in `--note`: the gates that were run and their results, an unrelated change left in the tree,
  the scope of the task, a deliberate trade-off, and — on a re-review — the answer to each finding.
- **Report every round as soon as it is read**, then continue the loop without waiting for confirmation: each
  finding with its severity and location, the agent's own verdict on it (accepted and what changed, rejected
  and why, or partially accepted and where the line was drawn), anything deferred and why, and whether the
  round passed. A finding is never fixed silently. If the same finding is still disputed after two rounds,
  stop and put both positions to the user instead of changing correct code to make a review pass.
- The reviewer is not always right. Evaluate each finding on its merits; a rejected finding keeps its reasoning
  in `--note` so the next round reads it.

The reviewer prompt carries a repository checklist (the patch-side view of the next four rules) and the prompt
and model live in the script; `OMT_REVIEW_MODEL` selects another model.

### What the reviewer is asked to weigh here

- **A quantitative claim needs a command that reproduces it.** Any number in a document or a code comment —
  contrast, a ratio, a coordinate, a size, a duration — must be reproducible by one named command. When the
  instrument behind it is later corrected, the number is void immediately and every place that quotes it must
  change with it. Writing an unverified number as a confirmed one has been a real defect in this repository
  more than once.
- **An instrument without counter-examples is not a gate.** Every measurement harness carries regression cases
  for the failures that defeated an earlier version of it (for example `png_stats.py --selftest`), and a check
  that covers only the happy path does not count as a gate.
- **A development switch must not leak.** An e2e scenario must leave a usable app behind: handing the user an
  instance whose panel text is transparent, or with a solid window pinned behind it, has happened. Scenario
  helpers restore in a trap, and the suite checks the running process for the switches that break it.
- **A document must not be quietly weakened or overstated.** If a change narrows a promise, says "enforced"
  where the measurement only suggests, or replaces a verified table with an unverified one, that is a finding —
  and the design document wins over the code unless the change updates it in the same commit with a reason.

## Architecture and invariants

- AppKit UI and dynamically registered Objective-C callbacks run on the main thread. Global input and device observers run on dedicated threads and marshal events back to main.
- Preserve existing mutex/RwLock ownership and raw Objective-C/Core Foundation lifetimes.
- The overlay supports icon-only and thumbnail cards. Thumbnail capture needs Screen Recording permission and must degrade gracefully to icons/fallbacks without it. Keep layout helpers pure where possible for unit testing.
- Dynamic card classes must not depend on Rust-side properties exposed through `msg_send!`; use the existing card-index/key maps.
- The Accessibility window list is authoritative for switchable windows. Preserve stored AX titles for activation; presentation-only fallbacks belong in the UI layer.
- Configuration is loaded per field: invalid values fall back individually without discarding valid settings. Runtime reload must preserve this and refresh affected UI.
- Mouse profiles match VID/PID; the mouse event tap is separate from the switcher tap, and pointer settings must be reapplied after reconnects.
- Clipboard history is optional and off by default. Gate recording and Option+V when disabled, and never record sensitive pasteboard markers. The history is written to disk in plaintext while the feature is on, so its disk footprint is the user's decision: turning the switch off clears the saved records, and `clear_on_quit` clears the history file and cached image data on an orderly exit (Command+Q, menu Quit, logout, shutdown); a crash or force quit cannot run that cleanup.
- Settings UI should reuse `SettingsSection`, `SettingsCard`, `SettingsRow`, `SettingsControl`, and `SettingsButton`; extend shared components when behavior is shared. A `SettingsButton`'s `tag` **selects its hover/normal palette**, so never carry an action id in `setTag:` — keep the component's tag and map the sender pointer to an action id instead, or the colour gets stuck after the pointer leaves.

For detailed subsystem behavior, inspect the relevant module and `docs/developer-notes-en.md` rather than adding implementation history here.

## UI design system

- `docs/design-style-en.md` is the normative style for **every** surface the app draws; the Chinese version is `docs/design-style.md`. Any user-visible change must follow it. When code and that document disagree, the document wins and the code is the bug — fix the code, or change the document in the same change, with a reason.
- It fixes the token set: semantic colors with measured contrast floors, the `12 / 14 / 20 / 26` type scale, a 4px spacing grid with named layout metrics, the `8 / 12 / 16` radius scale plus the concentric-derivation rule, four elevation levels, and two animation durations with one easing curve. Never introduce a literal color, font size, radius, spacing or duration at a call site: add the token in `src/theme.rs` (or the owning module), document it, then use it.
- Cards and rows are defined by border and surface, not by shadow; hover and selection never change layout; controls stay single-line and truncate long values with a tooltip; every animated surface honors Reduce Motion when the animation runs.
- `docs/ui-refresh-plan-en.md` (Chinese: `docs/ui-refresh-plan.md`) tracks the migration from the current values to those tokens, phase by phase. Follow its order, and keep its rule: every fix lands with a tier-A assertion, or with a state field that makes the fix assertable.

## Runtime requirements

- Accessibility permission is required for the global event tap and AX operations. Screen Recording is additionally required for thumbnails; both failures must degrade or report clearly rather than break switching.
- Runtime configuration is `~/.config/oh-my-tab/config.toml`.
- Logs default to `~/Library/Logs/oh-my-tab/oh-my-tab.log`; rotation and retention belong to the logger (`src/logger.rs`). A non-empty `logging.file_path` is appended verbatim without rotation or cleanup.

## Editing and localization

- Preserve unrelated user changes and avoid destructive Git commands unless explicitly requested.
- Write comments in English only, and only where they explain something the code cannot: FFI/Objective-C subtleties, thread/lock/ordering invariants, measured platform facts, non-obvious trade-offs. Do not restate the code and do not keep its history; both belong in commit messages, release notes, or tests.
- Keep user-visible strings in `t()`/`tf()`/`t_count()` and add keys to every supported locale; use `t_count()` for singular/plural counts. Developer logs stay in English; dynamic titles are data, not UI chrome. Never show a raw translation key, config key, enum name, hex value, VID/PID or internal preset id — see `docs/design-style-en.md` §11.
- Prefer native file editors. Use scripts only for genuinely programmatic changes; back up targets, assert all anchors, fail loudly, and inspect `git diff` afterward.

## Git and commits

- Do not commit, push, stage, or rewrite history unless explicitly requested. `style` is for code-style changes, not UI styling.
- If the input is exactly `/cmsg` or `cmsg`, inspect `git diff --cached`, `git diff`, and `git diff HEAD`; base the result on the combined diff and return exactly one English Conventional Commits line: `type: description`. Check `AD` status against the combined diff before deciding scope.
- If the user replies exactly `allow commit` after a message was provided, recheck status and all three diffs, then commit only the intended changes with that exact message; ask if scope is ambiguous.
- If the user replies exactly `allow push`, recheck the worktree, commit only intended changes with that exact message, and push the current branch to its configured upstream; ask if scope or upstream is ambiguous.
