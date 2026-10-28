# AGENTS.md

Guidance for agents working in this repository.

## Project

`oh-my-tab` is a macOS menu-bar window switcher. It intercepts Command+Tab, Option+Tab or Control+Tab, displays a floating application/window overlay, and raises the selected window through Accessibility APIs. Optional modules provide mouse enhancement and clipboard history.

The app is Rust-only and calls AppKit, CoreGraphics, and ApplicationServices through `objc2` FFI; there is no Swift bridge or Rust UI framework.

## Build, run, and verification

```sh
scripts/gate.sh                 # the whole gate, one command, one outcome per tier
scripts/gate.sh --offline       # the tiers that need no graphical session
scripts/dev-restart.sh          # then launch the app (runtime changes only)
```

The gate is `scripts/gate.sh`, and **it is the gate**: `selftests`, `fmt`, `check`, `clippy`,
`release-check`, `cargo-test`, `a1` (the GUI smoke runners) and `a2` (the e2e scenarios), in that order —
cheapest and most likely to fail first. Each tier prints exactly one `gate: <tier> PASSED|FAILED|NOT RUN`
line and the run ends with a table of them, so a tier that could not decide is never read as one that passed.
Its exit status follows the same rule: 0 = every selected tier passed, 1 = at least one failed,
**75 = incomplete** (nothing failed, but a tier did not run, for example `--offline` or no graphical
session). An aggregator child (`smoke-runners.sh`, `run-all.sh`) that exits 75 is recorded as NOT RUN,
not FAILED (`run_step` in `scripts/gate.sh`), because it could not decide. A green `--offline` run is not
a green gate.

**Match the gate to the change. A full run costs about 6.5 minutes, so it must not be the loop.**

Measured on 2026-10-24 with a warm `target/`. `fmt`, `check`, `clippy`, `release-check` and `cargo-test`
together take about 5s. `a1` takes 44s. `selftests` takes 99s, and most of that is `agent-review-selftest.sh`
at 84s. `a2` takes 226s, more than half the total. `a2` also needs a focus baseline, another desktop and a
bound Space shortcut, so it is the tier most exposed to the machine. Paying for it on every edit buys noise,
not signal. A cold build adds its own compile time on top and belongs to none of these.

| When | Run | Cost |
| --- | --- | --- |
| Iterating, nothing handed off | `cargo build` plus the tests for what changed | seconds |
| Before a review round or a commit | `fmt`, `check`, `clippy`, `cargo-test`, `a1` | ~1.5 min |
| Before handoff or release | the whole gate, `a2` included | ~6.5 min |

The whole gate is also required for any change to FFI, concurrency, configuration, build or release code.

`scripts/gate.sh --tier=<name>` selects tiers and may be repeated. The middle row is one command:
`scripts/gate.sh --tier=fmt --tier=check --tier=clippy --tier=cargo-test --tier=a1`.

Two rules bound that shortcut. Report every tier that did not run as NOT RUN in the handoff. Never report
an unrun tier as green. The middle row suits a change whose blast radius is the code that was edited.

- `scripts/cargo-test.sh` runs the suite and **fails when the run executed no test**. A plain `cargo test`
  exits 0 for a filter that matched nothing (measured: `running 0 tests ... ok`), so a typo in a filter reads
  as a pass; the wrapper makes that case a failure. Pass a filter as an argument and it keeps that guard;
  add `--require-match` when the filter must match the `oh-my-tab` lib tests specifically (other workspace
  targets legitimately run zero tests under a filter, so that is an opt-in rather than the default).
- `scripts/smoke-runners.sh --list|--run` is the A1 tier: it derives the runner list from the compiled
  `#[ignore]` tests instead of a transcribed list, refuses a `target/debug/oh-my-tab` older than `src/`,
  and classifies each runner by the `smoke-result:` line it prints. Silence is NOT RUN, never a pass.
- `scripts/selftests.sh` runs every `scripts/*-selftest.sh` offline, so the gate helpers are checked by
  something: the dev-instance lock and ownership probe, the Finder extension cleanup, the e2e disturbance
  gates, the reviewer script, the cargo-test wrapper, the launch codecs, the documentation statistics
  (`scripts/doc-stats.py`: rule parity between the guide's two languages) and the gate itself; `ls
  scripts/*-selftest.sh` lists them, and the run prints the count it ran. The version agreement between the
  root `Cargo.toml` and every workspace member is asserted twice: `src/version_consistency.rs` in
  `cargo-test`, and `scripts/version-consistency-selftest.sh` against `cargo metadata`. The packaging
  steps take the version from the root manifest only, so a release that bumps it alone would ship an
  `.app` whose embedded crates claim another version, with nothing else noticing. The packaging path
  asserts the same agreement itself: `scripts/bundle.sh`'s `workspace_version_mismatches` refuses to
  package a disagreeing workspace, so a release run that skips the gate is still covered.
- The normal `cargo test` suite is headless-safe; GUI/permission-dependent smoke tests are marked `#[ignore]`.
- Docs, comments, localization, and `AGENTS.md` changes need no Rust tests. Rust changes require `cargo fmt` (plus `cargo check` when interfaces or compilation are affected) and targeted tests when behavior changes.
- Pick the tier row from the table above rather than defaulting to the whole gate. A cross-module change, or one in unsafe/FFI, concurrency, configuration, build or release code, always takes the whole gate. So does any handoff.
- Start the app with `scripts/dev-restart.sh`, never directly with `cargo run`.
- `scripts/dev-restart.sh --check` verifies a launch and then **stops it**, leaving neither the process nor
  its launchd job behind; that is the shape an automated check wants. Use it when an A2 or GUI check needs a
  real instance and the run is done afterwards. It promises only what it can prove: `instance: stopped`, not
  "nothing changed" — the config file, clipboard history and caches are the app's and the run does not
  restore them. Interactive iteration keeps the default, which leaves the window open.
- **A launch is verified from the running process, never from the plist on disk.** The script waits for the
  app's own `[launch] pid=… executable=… version=… bundle=…` record — written by the process itself, so it can
  only describe the process that wrote it — and fails when the pid, the executable path or the version
  disagree, or when the process exited right after writing it. When the log window is unusable (rotated,
  truncated, unreadable) the run reports `build timestamp: unverified` instead of passing; that is a report,
  not a pass. Report the `build-version` the script prints, which is the one the process reported.
- **One build/test/launch transaction at a time, across worktrees.** Every worktree shares one bundle id, one
  launchd label and one app log — that is what keeps the TCC grants and the keychain ACL valid across
  rebuilds — so two concurrent runs fight over one instance and the loser's bootout stops an instance the
  other run is verifying. `scripts/dev-restart.sh` takes a `lockf` claim in the git common directory before it
  stops anything, and waits up to `--lock-wait=<sec>` (default 900) before failing with exit 75. The claim
  covers the transaction only: **the app the script leaves running is not protected**, and the next run may
  replace it. `scripts/dev-lock-selftest.sh` checks the claim offline, including the cross-worktree case.
- **A registered Finder extension is not a working one.** Finder starts the extension through launchd, and a
  per-request launchd instance (`<appex bundle id>.<uuid>`) pinned to an appex path that no longer exists makes
  every request fail while `pluginkit` still reports the extension as elected (`+`) — the menu is then silently
  empty and nothing else complains. So `scripts/dev-restart.sh` removes exactly those dead-path instances before
  rebuilding, stops the extension process of *its own* appex after registering the new bundle, restarts Finder
  only when that appex has no extension running, and reports whether the extension process is actually up
  instead of letting `+` stand for "works". Never boot the host app's own launchd instance
  (`application.<host bundle id>.<pid>.<pid>`), and never stop another appex's extension: doing the former on
  2026-10-23 killed the app's log writers and invalidated `--whose-instance`.
  `scripts/finder-extension-selftest.sh` asserts both rules offline and against throwaway launchd services it
  creates itself: it reads the real launchd list but passes an allowlist of exactly its own labels, so it never
  boots out an instance it did not create.
- **The claim protects the transaction, not the window — so prove the window is yours before drawing any
  conclusion from it.** Because the claim is released when the transaction ends, another worktree may
  legitimately replace the instance between this worktree's `restart ok` and this worktree's UI check; the
  window then shows *that* worktree's build while both runs believe they verified their own. Before a UI, cua,
  screenshot or latency conclusion, run `scripts/dev-restart.sh --whose-instance`: exit 0 means the running
  instance is this worktree's build (and nothing competes for the shortcut), and **any other exit code means a
  conclusion drawn now would describe something else** — 1 a dev build from another worktree, an
  unrecognised location, or an earlier build of this same tree (`VERDICT: STALE_BUILD`, which shares exit 1
  with NOT_MINE because the running build is not this one), 3 nothing running, 4 the identity could not be
  established, 5 contested by another instance, 6 this worktree has no built bundle. The version comparison
  runs on the first query, so a replaced instance is reported then, not only by a later re-check. It builds,
  launches and stops nothing and takes no claim (by design: a query that queued on the claim would wait past
  the very replacement it exists to detect). Bind what you observe to the `pid` and `started` it prints, and
  run `scripts/dev-restart.sh --identity-expects=<pid>[,<started>]` afterwards: if that no longer reports
  `MINE`, the instance was replaced and the observation is void. It is a snapshot, not protection — it cannot
  prove the window belongs to the process, nor that the instance stayed put in between; see
  `docs/dev-instance-ownership.md`.
- **`--whose-instance` and `--identity-expects` are not substitutes for the claim.** They cannot be combined with
  `--check` or app arguments (exit 2), and a run that needs a protected window for a while still has no hold-lock
  option — use one transaction and re-check, rather than assuming the window stays.
- **Stop instances by identity, never by name.** Enumerate, verify each pid's executable path, terminate that
  pid individually (TERM, then KILL only if it refuses), and name the pids in the output. A broad
  `pkill -x oh-my-tab` also reaches copies this run cannot account for. The packaged install under
  /Applications registers the same global shortcut, so it is stopped too — with its pids printed, because it
  is the user's own app — and nothing may quietly restore the two-instance situation that once made new
  features look dead.
- For runtime changes, run `scripts/dev-restart.sh` after the gate for that change. If it reports `restart FAILED`,
  inspect the newest log under `~/Library/Logs/oh-my-tab/`, diagnose, and retry.
- When the onboarding guide does not need verification, launch with `--no-onboarding` (for example, `scripts/dev-restart.sh --no-onboarding`) so it does not appear; only run without it when testing onboarding flows.
- `scripts/dev-restart.sh` defaults to a **debug** build (`cargo build`): fast iteration with every debug assertion on. Use it for functional iteration and handoff. It re-assembles and re-signs the dev bundle only when the built binary, a bundled resource or the signing identity changed (an input fingerprint decides), and it signs with the Apple-issued identity when one is installed (`CODESIGN_IDENTITY` overrides the discovery) -- signing without a secure timestamp, so a local build never depends on Apple's timestamp service. The accepted development identities are the Apple-issued one and the self-signed `oh-my-tab-sign`; **ad-hoc is refused**, and a failure to sign with the Apple identity **aborts the build** unless `--allow-signing-fallback` asks for the self-signed identity on purpose (`--require-stable-signing` demands the Apple identity, both for a fresh build and when reusing an existing bundle). That stable designated requirement is what keeps the login-keychain ACL of the clipboard key and the TCC grants valid across rebuilds: with a self-signed or ad-hoc identity, macOS judges the ACL by the binary, so every rebuild re-prompts.
- For feel/perf validation (scrolling, animation, latency), run `scripts/dev-restart.sh --opt`. It uses the `dev-opt` cargo profile (`target/dev-opt/`): optimized like release while keeping `debug-assertions`, so runtime speed is representative and development still fails fast.
- `release_doc_dev/<Cargo.toml version>.md` must exist and be non-empty: `scripts/dev-restart.sh` aborts before building otherwise, and a dev build embeds that file as its release notes.
- Report the timestamp-based `build-version` printed by the script.
- Pass development switches as argv through `scripts/dev-restart.sh` instead of editing code: any `--flag[=value]` it does not own is forwarded to the app (`-- <args>` forwards argv verbatim), and a switch applies to that launch only.
- Reach otherwise hard-to-reproduce states (first-run onboarding, permission branches, update notices) with such a switch; a feature that only appears in one of them should expose a `--`-style flag parsed through `crate::dev_flags`.
- Park the settings window on a page at launch with `--open-settings=<page>` (`general`, `switcher`, `mouse`, `clipboard`, `key-bindings`, `keystroke-display`, `finder-menu`, `about`; the names come from `SETTINGS_PAGES` in `src/settings.rs`). A bare/`1`/`true` value selects About, and an out-of-range index is refused rather than clamped. Use it instead of clicking through the sidebar when a script, cua, or a screenshot needs a specific page: `scripts/dev-restart.sh --no-onboarding --open-settings=key-bindings`. It opens the window, so it takes focus; pair it with `--no-onboarding` unless the guide is what is under test.
  - `--show-clipboard[=after:N]` opens the clipboard picker and `--show-overlay` leaves the switcher standing; both exist because the real gesture steals focus, so a script cannot press it.
- **The running app must not read or receive environment variables as configuration.** `crate::dev_flags` and the script's argv passthrough are the only runtime configuration channels. Build and release scripts may read explicitly named packaging inputs, but must not forward the caller's environment to the launched app, dump it, or print sensitive values — the caller's shell may hold cloud credentials.

## Testing tiers

Three layers, split by **who decides pass/fail** — not by which transport drives them.

| Tier | Decides | How | Repeatable | Gate |
| --- | --- | --- | --- | --- |
| A1 | script | Pure-headless tests plus `--smoke-*` runners over the real AppKit view tree; GUI-dependent runners require a macOS graphical session | yes | yes, every change (run the applicable tier; an unavailable GUI tier is unrun, not passed) |
| A2 | script | `scripts/e2e/*.sh`, run through `scripts/e2e/run-all.sh` (the entry point; a scenario declares each disturbance in its header -- `E2E_STEALS_FOCUS`, `E2E_COVERS_SCREEN`, `E2E_CHANGES_PREFS` -- and each needs its own switch: `--include-focus`, `--include-screen`, `--include-prefs`), driven by the `cua-driver` CLI and asserting app-written JSON state (`--e2e-state=<path>`) plus real WindowServer state | yes | before handoff/release |
| B | agent or person | MCP tools plus screenshots: taste, first-pass UI review, failure triage, bug investigation | no | never |

- Anything assertable belongs in A: severity is not the split, assertability is.
- A1 has two execution environments: pure-headless tests can run without a graphical session; AppKit view-tree smoke runners must actually run in a macOS graphical session. A headless run does not pass or waive an unrun GUI smoke.
- A2 covers what only real input reaches: the global hotkey → summon → raise path, permission branches, cross-app behavior. `--e2e-state` exists because AX cannot express CALayer content or internal state, so the app states those facts itself instead of the test guessing from pixels. A quick hotkey press-release legitimately skips the display path; display and layout assertions belong to A1 smoke runners.
- **Promotion rule: every bug found in tier B must land an assertion in tier A** — or, when it cannot be asserted yet, a state field that makes it assertable. Otherwise the same regression returns unnoticed.
- A2 hotkey scenarios press through the system event stream and therefore steal focus: run them only with the user's consent, never in a background loop.

## Mandatory development workflow

For every non-trivial implementation task:

1. Understand the change and inspect the relevant existing code before modifying anything. When the change adds
   or alters a settings field, start from `docs/settings-field-checklist.md`: it names the full chain (config
   struct, locale keys, page builder, dispatch table, runtime change flags, renderer) and the assertions each
   layer needs. That chain is not derivable from the type system — Objective-C dispatch and the control-pointer
   tables are invisible to symbol navigation — so "where is this field read" is answered with `grep`, and the
   checklist exists to bound that search instead of re-deriving it from the code each time.
2. If the user asks for a design consultation first (先和 Codex 商量方案, "design first", 先议后做), put the
   approach to the reviewer before writing any code: `./scripts/agent-review.sh --design` with a brief that
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
7. Triage every valid finding by **impact, not by label** (see *Review budget and triage*): fix the in-scope
   ones now, and record the rest in `docs/review-backlog.md` with a reason and a revisit trigger.
8. Ask the **same** reviewer session to review the fixes, then repeat within the review budget. State the
   deferred findings in `--note` so the reviewer judges the deferral, not just the fixes.
9. Only then report the task as complete.

A design round does not satisfy the review requirement: it is answered before anything is built, and step 5
still happens in the same session, so the reviewer can judge whether the implementation followed the approach
it agreed to. "完成后送审" / "review it when you are done" is step 5 restated; 先商量方案 / "design first" is
step 2, and both can be asked together.

A task is not complete because the code compiles, the tests pass, the feature works, or the implementation
looks reasonable. Independent review is mandatory.

Before reporting completion, all of the following are true:

- the requested implementation is complete;
- the applicable gate ran and is clean (an unavailable GUI tier is unrun, not passed);
- independent review has run in this task's session;
- every valid finding was either fixed or **deferred under *Review budget and triage*** — recorded in
  `docs/review-backlog.md` with its reason and revisit trigger, and stated in `--note`. There is no
  "no valid MEDIUM may remain" requirement: the budget and the impact classes decide what is fixed now,
  and a remaining finding that is out of scope is reported, not hidden;
- the fixes were re-reviewed in the same session, within the budget;
- no unresolved BLOCKER or HIGH finding remains, and no in-scope finding was deferred to end a round.

Do **not** run `--finish` here: the session stays open until the user says the task is done. Report completion
and, if the session is still open, say so and offer to close it.

### Parallelising a wide change

A change that touches many independent files (a mechanical migration, the same edit repeated across
modules, a set of separate documents) should be split across subagents rather than walked through
serially: the work is wide, not deep, and one agent doing it file by file spends the whole budget on
transcription. Split by **file**, give each subagent an exclusive set, and state the shared rules once
in a brief they all receive.

- **Partition so no two agents write the same file.** Overlap is the one failure mode that costs more
  than doing it serially: two agents editing one file produce a diff neither can reason about, and the
  compile errors then get attributed to the wrong change.
- **Put the contract in the brief, not in each file.** The shared helper's signature, the rule that
  changed (for example "object parameters are `*mut AnyObject`, BOOL returns are
  `objc2::runtime::Bool`"), and the skeleton to copy. A teammate that has to discover the pattern from
  the diff will invent a second one.
- **Say what not to touch.** Another task's in-flight files, the shared helper module, `Cargo.toml`,
  and `git` itself (no commits, no `cargo fmt` across the tree) belong in the brief as prohibitions.
- **Reserve the shared-file and cross-cutting work for the lead.** The helper module, the test file
  every partition feeds, the gate wiring and the final `cargo fmt` are one owner's job, because they
  are the points all the partitions meet.
- **Broadcast a discovered rule to everyone still running.** When one agent finds that the dependency
  does something unexpected (a type that has no `Encode` implementation, a method that must be called
  before another), the others are about to hit the same wall; send it once rather than let each
  rediscover it.
- **Collect and verify centrally.** A subagent's "cargo check is green" describes its own files at the
  moment it looked; the lead runs the full gate on the merged tree and reads the diff for collisions.
  Review and the gate are never delegated — the lead still owns step 5.

This does not change the workflow above: the partition still needs the same gate, the same independent
review, and the same triage. It changes who types the mechanical part.

## Investigation budget and stopping

Step 1 asks for the relevant code to be understood before it is changed. It does not ask for that to continue
indefinitely, and treating "certain" as the goal is the failure mode this section exists to stop: certainty has
no stopping point, so an agent aiming for it spends the budget on its own instrument and reaches the user with
nothing built. Bind the depth of an **exploratory** investigation to what it costs to be wrong, not to how large
the uncertainty feels.

- **Name the fork before the dig.** Before a read that is not obviously necessary, say what the competing
  readings are and which action each one implies. If every branch leads to the same next action, the answer
  cannot change what you do — stop investigating and do it.
- **Take the cheapest discriminating observation first.** One observation that separates two hypotheses beats
  ten that corroborate one.
- **Two misses end the branch.** Two consecutive exploratory steps that did not change the plan mean this
  branch has saturated: the next message either acts or asks a question. It stops *this* line of exploration
  and names the next step; it is not a claim that the information is objectively complete.
- **Prefer acting when acting is reversible.** The cheaper a change is to undo, the less it has to be proven
  first; `git` can revert an uncommitted edit in a moment, a persisted-state migration or a keychain change
  cannot.
- **Ask instead of dissolving a real ambiguity.** When two readings genuinely differ in cost, or in what the
  reader ends up seeing, put the choice to the user.
- **Recognise the anti-patterns by name:** commit archaeology as anxiety management; precision theatre —
  measuring something the decision does not depend on; collecting confirmations after the answer is settled;
  and treating "I could check one more thing" as a reason to.

A progress prompt, not a gate: **more than roughly ten read-only tool calls, or several minutes, with no
write** means the next message should name the decision that is still open and why it is worth another look.
The count is a nudge. The test that decides is the one above it — *will this information change what I do
next?*

**This section never overrides a requirement to check.** Security, concurrency, data-migration and FFI/lifetime
work needs the verification it needs; a user-specified read-only scope is a scope, not a budget to spend; and
"reversible" is judged by what the user loses, not by how few files the change touches.

## Review budget and triage

A review loop that fixes everything the reviewer can find does not converge: each fix adds surface, the next
round examines that surface, and the tail is spent on the agent's own test instrument instead of the product.
One observed task took 21 rounds; the last BLOCKER/HIGH came in round 11, and rounds 12–29 found 45 issues
without a single one — most of them races inside diagnostic plumbing that only existed to satisfy an earlier
round. Budget the loop and triage by impact instead.

- **Budget: at most 3 code-review rounds per task** (the first review plus at most two re-reviews; a design
  round does not count). A fourth round happens only when the user asks for one. Reaching the budget is not a
  failure: report the remaining findings with the deferral and stop.
- **In scope, always — whatever the label:** a crash, panic, leak or unbounded growth; data loss; a
  user-visible behaviour that is wrong, missing or inconsistent with what the UI promises; a breach of an
  invariant in *Architecture and invariants*; a regression of documented behaviour; a security or privacy
  problem; a broken gate, or a gate that reports PASS where it cannot decide. A MEDIUM in one of these classes
  is fixed now even when the reviewer ranked it low.
- **Deferred to `docs/review-backlog.md`:** rigour of the test instrument beyond the minimum below; races or
  edge cases that only a diagnostic path can reach; extra assertions and "you could also check X"; comment,
  copy and doc polish that changes no promise; unrun coverage; re-deriving a number that is not user-facing;
  speculative hardening. Each entry records the round, severity, why it is deferred, and the trigger that
  brings it back (usually "next change in this module").
- **The promotion rule is satisfied by the cheapest sufficient assertion.** One assertion that fails on the
  original defect — ideally on a pure function, or on a state field the scenario already reads — is enough.
  Generalising it into a protocol (ownership, epochs, reclamation, cross-thread staging) is backlog work, and
  building it during the fix is what produced most of the tail above.
- **State every deferral in `--note`.** The reviewer only sees the prompt, the diff and the repository, so a
  deferred finding must be visible there with its reason; a round whose only remaining findings are deferred
  ones counts as PASS. Deferring is a decision, not a silent skip.
- **Never defer a finding to end a round.** If it is in scope, fix it; if it is not, say so and record it.

## Independent reviewer

The independent reviewer is a separate model session, driven by `scripts/agent-review.sh`: this repository
reviews with the Codex CLI by default, and the `reviewer` key can select the local pi agent instead (see the
reviewer-config paragraph below). The implementation agent must not act as its own final reviewer.

```sh
scripts/agent-review.sh                     # first review, or a re-review in the same session
scripts/agent-review.sh --design            # put an approach to the reviewer before writing code
scripts/agent-review.sh --note "<text>"     # the agent's own words: verdicts, disagreements, gate results
scripts/agent-review.sh --note-file <path>  # the same, read from a file
scripts/agent-review.sh --retry             # resume a round that produced no verdict
scripts/agent-review.sh --fresh             # start a new session when the old one is gone
scripts/agent-review.sh --task <slug>       # this task's session, archive and lock (see below)
scripts/agent-review.sh --tasks             # list the sessions this repository holds
scripts/agent-review.sh --timeout <sec>     # bound a run that hangs (default 1800, 0 disables)
scripts/agent-review.sh --finish            # clear this task's reviewer session for the next task
scripts/agent-review-selftest.sh            # check the helper itself, against a stub CLI, offline
```

- One session per task, and a re-review resumes **that** session so the reviewer remembers its previous
  findings, the agent's answers, and what was already tried. `--retry` resumes a round that produced no
  verdict; a round that reached one is re-reviewed by running the reviewer again.
- **`--finish` is the user's call, not the agent's.** The session is kept until the user says the task is
  done — not until the agent judges the work finished. An agent that clears the session on its own judgement
  silently discards the reviewer's memory of still-open findings, and a later round re-reports what was
  already settled or assumes settled what was not. Two consequences:
  - Run `--finish` **only** when the user states the feature is complete or the review is over (for example
    「这个功能完成了」「审查到此为止」). Do not run it to tidy up, to save tokens, or because the work looks
    like it has landed.
  - On `cmsg` (see *Git and commits*), if the task's session is still open, say so and offer to close it —
    do not close it silently. `scripts/agent-review.sh --tasks` shows the state: `open` means the task is
    still on its session, `closed` means it was ended on purpose.
- A session that has run past the budget grows more expensive every round, and a round that hits `--timeout`
  loses its work. The helper warns on the round that starts over budget; closing the session is still the
  user's decision, and the way to close it early without losing continuity is `--finish` followed by a fresh
  round that hands the still-open findings over in `--note`.
- When more than one task is in flight at once — two agents working two tasks, or a design round for one task
  while another task's implementation is under review — each task names its own key with `--task <slug>` (or
  says it once through `OMT_REVIEW_TASK`), so that no round can resume another task's session, read its
  findings, clear it, or wait on its lock: session, lock, round kind, counter and archive all move under
  `.agent-review/tasks/<key>/`, and `--tasks` lists whose session is whose. Every round of a task passes the
  same key, `--finish` included, and a design round and the review of the implementation it agreed to are
  deliberately the same task under one key — that is what lets the reviewer judge the implementation against
  what it agreed. A round that names no key belongs to the keyless single-agent flow. A key is lowercased,
  takes letters, digits, `.`, `_` and `-`, and may not be `default`, `tasks` or `rounds`. **The key separates
  what the reviewer remembers, not the diff**: two agents in one working tree still put both tasks' changes in
  front of the reviewer, so a tree per task is what makes two reviews independent, and what is in scope for a
  task is what `--note` says.
- **A round without a verdict is not a review.** Only a round that actually concluded counts. A transport
  failure, a turn that never completed, a message that is not a verdict or a timeout is reported as a failed
  round with the reason and the archived evidence under `.agent-review/rounds/`, and is never counted toward
  completion. Retry it with `--retry` **at most twice, then stop and tell the user**; a retry resumes the same
  kind of round it was (a design round stays a design round), and says that nothing in the tree changed rather
  than claiming an update.
- **An unrecoverable round and a lost session are different things.** Several failed attempts mean something is
  wrong with the transport or the configuration as often as they mean the session is gone, so report what the
  helper printed rather than asserting the session no longer exists. Once a resume is confirmed lost, say so —
  the reviewer's memory of its earlier findings is gone — and continue with `--fresh`, handing the still-open
  findings and every deferral over in `--note`. A session is never discarded silently, and `--fresh` says when
  it replaces one.
- **A tree that matches HEAD is refused** (exit 65) before anything runs — a round that reviewed no change is
  not a review — with two exceptions: `--design`, which is asked before there is anything to review, and
  `--retry`, which resumes a round that produced no verdict on a tree that legitimately has not changed since.
- **A re-review revisits every previous finding and classifies it** — RESOLVED, PARTIALLY RESOLVED,
  UNRESOLVED, or REJECTED WITH VALID JUSTIFICATION — then looks for regressions and newly introduced problems.
  A finding is never treated as fixed merely because the code changed.
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
lives in the script. The reviewer is chosen by the top-level `reviewer` key in `agent-review.toml` at the
repository root: `"codex"`, `"pi"` or `"none"`. There is no built-in default — this repository ships
`agent-review.toml` with `reviewer = "codex"` — so a missing file or a file without the key refuses the
round rather than guessing; `"none"` is an explicit no-op that exits 0. codex settings live in
`[codex]` (`model` / `fast` / `reasoning_effort`) and pi settings in `[pi]` (`provider` / `model` /
`thinking`), all three required when `reviewer = "pi"`. `OMT_REVIEW_REVIEWER`, `OMT_REVIEW_MODEL`,
`OMT_REVIEW_FAST`, `OMT_REVIEW_REASONING_EFFORT`, `OMT_REVIEW_PI_PROVIDER`, `OMT_REVIEW_PI_MODEL` and
`OMT_REVIEW_PI_THINKING` override one setting each. `fast = true` asks Codex for Fast mode (`service_tier =
"priority"`), which is billed at a higher rate; `false` sends `service_tier = "default"` explicitly so a Fast
mode configured in `~/.codex/config.toml` cannot leak into a review. `reasoning_effort` is one of codex's own
levels (`codex debug models` lists them); omitting it sends no flag, so the model default or a globally
configured level stays in force. pi runs non-interactively in JSON mode with `edit` and `write` disabled and
only `read/bash/grep/find/ls` enabled, so it has no dedicated file-writing tool; `bash` can still write files
and runs with the account's permissions, so the prompt's no-modification rule is the remaining guard. pi
resumes one session per task through `--session-id`, and the `--session-id` flag is its only handle on the
reviewer session. A configuration that cannot be read refuses the round rather than being ignored, and each
round's outcome records the reviewer and the settings it ran under, including whether Codex declined the
requested reasoning effort.

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

- **`ARCHITECTURE.md` (repository root) is the map of the source tree and the ownership rules.** Read it before
  adding a source file, moving a responsibility between files, or wiring a feature into an existing surface: it
  lists every module file by file with what each owns, states the layering and threading rules, and says where a
  change of each kind belongs. When a file is added, renamed or removed, update its tree in the same edit — a
  tree that disagrees with `src/` is worse than no tree. `docs/developer-notes-en.md` holds the deeper
  per-subsystem behaviour this file and `ARCHITECTURE.md` link to rather than repeat.
- AppKit UI and dynamically registered Objective-C callbacks run on the main thread. Global input and device observers run on dedicated threads and marshal events back to main.
- Preserve existing mutex/RwLock ownership and raw Objective-C/Core Foundation lifetimes.
- The overlay supports icon-only and thumbnail cards. Thumbnail capture needs Screen Recording permission and must degrade gracefully to icons/fallbacks without it. Keep layout helpers pure where possible for unit testing.
- Dynamic card classes must not depend on Rust-side properties exposed through `msg_send!`; use the existing card-index/key maps.
- The Accessibility window list is authoritative for switchable windows. Preserve stored AX titles for activation; presentation-only fallbacks belong in the UI layer.
- Configuration is loaded per field: invalid values fall back individually without discarding valid settings. Runtime reload must preserve this and refresh affected UI.
- Mouse profiles match VID/PID; the mouse event tap is separate from the switcher tap, and pointer settings must be reapplied after reconnects.
- Clipboard history is optional and off by default. Gate recording and Option+V when disabled, and never record sensitive pasteboard markers.
  Recording can also be **paused** without turning the feature off: the pause is persisted, the poll keeps advancing the changeCount baseline so a resume can never record what was copied during the pause, and `paused` outranks the master switch (a paused session that is disabled and re-enabled still records nothing). Applications on the **exclusion list** are matched by bundle identifier -- the frontmost app, plus the copier declared by `org.nspasteboard.source` as an ADDITIONAL signal that can never override the frontmost verdict -- and their copies are dropped before the payload is read, hashed, cached or icon-extracted. While the feature is on, the history is written to disk **encrypted** (AES-256-GCM under a random key in the login keychain; the history TOML, the original image bytes and both preview files), and the key is never deleted — turning the switch off deletes the records instead, and `clear_on_quit` clears the history file and cached image data on an orderly exit (Command+Q, menu Quit, logout, shutdown); a crash or force quit cannot run that cleanup. The user's storage decision is the disk footprint, and the app must never fall back to plaintext: when the key cannot be read the feature is **unavailable** -- nothing is recorded (no in-memory fallback either), the session's own traffic is dropped, and the settings banner, the opened panel and a system notification say so (the two in-app surfaces each carry a "grant keychain access" action that re-reads the key once; a system notification has no in-app action). An app cannot adopt a keychain item it did not create (`errSecInvalidOwnerEdit`), so a legacy item needs a one-time user deletion. Deletion is a verified transaction — an unreadable record set, a failed delete or an unrevoked purge marker blocks loading and writing rather than silently overwriting anything. See `docs/clipboard-encryption-plan.md`.
- Settings UI should reuse `SettingsSection`, `SettingsCard`, `SettingsRow`, `SettingsControl`, and `SettingsButton`; extend shared components when behavior is shared. A `SettingsButton`'s `tag` **selects its hover/normal palette**, so never carry an action id in `setTag:` — keep the component's tag and map the sender pointer to an action id instead, or the colour gets stuck after the pointer leaves.

For detailed subsystem behavior, inspect the relevant module and `docs/developer-notes-en.md` rather than adding implementation history here.

## UI design system

- `docs/design-style-en.md` is the normative style for **every** surface the app draws; the Chinese version is `docs/design-style.md`. Any user-visible change must follow it. When code and that document disagree, the document wins and the code is the bug — fix the code, or change the document in the same change, with a reason.
- It fixes the token set: semantic colors with measured contrast floors, the `12 / 14 / 20 / 26` type scale, a 4px spacing grid with named layout metrics, the `8 / 12 / 16` radius scale plus the concentric-derivation rule, the `med` and `high` elevation tokens (a surface with no shadow needs no token), and two animation durations with one easing curve. Never introduce a literal color, font size, radius, spacing or duration at a call site: add the token in `src/theme.rs` (or the owning module), document it, then use it.
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
- **A release note that has already shipped is history, not a file to improve.** Both channels publish. `release_doc/` goes to the stable feed. `release_doc_dev/` goes to the dev feed. A version reaching one feed says nothing about the other. Before editing any note, establish three facts: the channel, the version, and whether that channel published it. An unpublished draft is editable. The rule lives in `docs/doc-style-en.md` §2.7–§2.8 and is not repeated here.
- If the input is exactly `/cmsg` or `cmsg`, inspect `git diff --cached`, `git diff`, and `git diff HEAD`; base the result on the combined diff and return exactly one English Conventional Commits line: `type: description`. Check `AD` status against the combined diff before deciding scope. A commit is not the end of a review task, so **do not run `--finish` here**: if the task's reviewer session is still open, say so alongside the message and offer to close it (see *Independent reviewer*).
- If the user replies exactly `allow commit` after a message was provided, recheck status and all three diffs, then commit only the intended changes with that exact message; ask if scope is ambiguous.
- If the user replies exactly `allow push`, recheck the worktree, commit only intended changes with that exact message, and push the current branch to its configured upstream; ask if scope or upstream is ambiguous.
