# 架构与测试整改方案（已评审通过）

This file is the plan the design review agreed on (, revision 7, task: arch-test-plan (state: .agent-review/tasks/arch-test-plan, from --task)
reviewer: codex (agent-review.toml)
reviewer: model=gpt-6.1-sol (agent-review.toml), fast=off (agent-review.toml)
reviewer: reasoning_effort=high (agent-review.toml)
Resuming codex reviewer session:
01a169b6-f06b-7632-9866-a27231d833ed


上一轮设计审查没有遗留发现。此前“固定容量无法保障查询竞态安全”的 MEDIUM 已在批准方案中 **RESOLVED（设计层面）**；实际实现尚无法验证。

**MEDIUM — 整改文档未完成，且没有实现可供审查。**  
位置：[docs/architecture-test-remediation-plan.md:1](/Users/ceres/RustroverProjects/oh-my-tab/docs/architecture-test-remediation-plan.md:1)。

当前唯一改动是这个未跟踪文件，内容只有“架构与测试整改方案（已评审通过）”标题和空行。没有源码或脚本改动，无法核对已批准的门禁、实例身份、PID 回收及 FFI 防护是否落地。

建议补齐正式文档中的已批准方案、验收条件和实施状态，完成实现后附实际门禁结果重新送审。设计通过不能作为实现通过的证据。

本轮未修改文件、运行构建、测试或应用。当前实现不能判定通过审查。

REVIEW-STATUS: FINDINGS). It is the source of truth for what this branch implements; the section numbers below are the ones the plan uses.

## Revision 7 (agreed)

Status: draft for design review round 7. Round 4 resolved the A1 authority rule, the catch-boundary
rule, the normal A2 observation phase and the PID generation rule, and left two defects: the A2
**restore** phase contradicted the saved-identity rule, and the PID reclaim's cost came from a scan
whose scope was not established. Section 0 records both fixes. No code has been changed.

## 0. What round 6 changed

| R6 finding | Change |
|---|---|
| A fixed retention set cannot bound the lifetime of an unfinished workspace query, so it cannot be what makes the reclaim safe; a stale "alive" answer could clear a newer terminate mark | **ACCEPTED — the retention set was carrying a concurrency job it cannot do.** §4.8 now makes **correctness come from the generation, not from capacity**: (a) the terminate mark and the generation counter share **one row per pid** (`PidRow { generation, terminated }`), so there is no second container to bound; (b) a workspace query is stamped with the ledger generation it was computed against, and the in-lock apply is refused when the row's generation has moved — a stale answer can neither clear a newer terminate mark nor re-admit the pid; (c) the in-lock check that already guards every request (`request_for_generation`, `capture.rs:325-328`) rejects any request whose expected generation is older than the row's. `RECENT_TERMINATIONS` no longer exists. |

The counter-example the reviewer specified is in §4.8: pause a query, produce more terminations than
any capacity, resume the query, and the old pid must still be refused.

Reviewer's two answers adopted: the terminate mark shares the generation row and the generation
re-check is what makes it safe (no capacity is used as race protection); the negative control uses an
**instance-local** `#[cfg(test)]` seam on the ledger object, so it cannot affect parallel tests, and
it asserts that *the detector* reports the violation while the suite itself still passes.

## 0b. What round 5 changed

| R5 finding | Change |
|---|---|
| The ledger bound includes the ledger itself, so the assertion holds even when nothing is reclaimed | **ACCEPTED — it was a tautology.** The bound is now `ledger_pids ⊆ live_pids ∪ referenced_pids` (one row per pid, see §4.8); the ledger's own size is not on the right-hand side, so a build that never reclaims must **fail** it. Round 6's fix supersedes the constant-sized retention set this row originally introduced. The bound is asserted immediately after each growth-triggered reclaim, and the plan does not claim lazy shrinking. |

Reviewer's other three answers adopted: either phase failing ⇒ exit 1, with the log naming which
phase, and no new top-level outcome; the growth-only trigger stays with no timer, and the plan
promises the bound only after a growth-triggered reclaim, never as a continuous guarantee; §4.9 is
not complete on step 1 plus an allowlist — the in-scope repairs, the full guard and the four-part
acceptance must all land, with anything deferred triaged by impact, recorded in
`docs/review-backlog.md` and stated in the review note, and no BLOCKER/HIGH left open. Added to the
verification list: a generation change during the out-of-lock query (the answer is stale and the
in-lock generation re-check must still reject), and a failed restore must not skip other cleanup.

## 0a. What round 4 changed

| R4 finding | Change |
|---|---|
| 1. The restore relaunches the app, so verifying the restored instance against the **old** phase's identity must fail — the plan contradicted itself | **ACCEPTED.** §3.4 now defines the **restore as a phase of its own**: (i) close the tested phase with `omt_verify` against its saved identity — **before** anything is restored; (ii) restore; (iii) `omt_phase_begin` on the restored instance to save **its** identity; (iv) `omt_verify` the restored instance against that. The two verdicts are kept separately, and a failed tested phase is never overwritten by a successful restore. Two scenarios: a normal restore relaunch passes end-to-end; a replaced tested instance whose restore succeeds still fails overall. |
| 2. The PID reclaim's scan cost has no source basis, and its lock scope is unspecified | **ACCEPTED.** §4.8 now (a) triggers reclaim **only when an insert actually grows** the ledger (a new pid key, or a terminate mark that is a new key), so repeated requests for the same pid never scan; (b) runs the workspace query **outside** `CAPTURE_STATE` — the current code already holds it across the Objective-C calls in `app_terminated` (`pregen.rs:199-206`), so the new path is stricter than today; (c) computes within the lock only pure Rust set operations over `desired`/in-flight keys, with the bound stated as *live app pids + pids still referenced by a queued or in-flight job + ledger size*, never "visible windows" (`desired` also carries background pregen and prefetch jobs). |

Reviewer's answers adopted: §4.9 ships in two steps inside one phase (repairs with tests, then the
full boundary, allowlist enumerated and gaps reported to the backlog and the review note); reclaim
runs at every real growth entry point, not only termination, still with no timer; in-process runners
may use their own stderr, provided the selftest proves libtest capture cannot hide it.

## 0b. What round 3 changed

| R3 finding | Change |
|---|---|
| 1. A1's "the app child is the only authority" excludes runners that assert **in the test process** | **ACCEPTED.** The rule is now "the process that executed the assertion writes the verdict", with two shapes that share one wire format: (a) **in-process** runners (`slider_double_click_smoke`, `stepped_slider_rests_on_whole_steps_smoke`, `objc_exception_guard_contains_a_raise`, `collect_windows_smoke`) write `smoke-result:` from the test process; (b) **child** runners write it from the app process into the `--e2e-state` file, and the parent re-prints it verbatim, never upgrading it. §3.3 also gains the third self-test the reviewer asked for: an in-process runner, a child runner and a permission-short-circuit all reporting the same way. |
| 2. A2 does not verify each observation phase against the **saved** identity | **ACCEPTED.** `omt_verify` is defined as `--identity-expects=<saved pid>,<saved started>` and **never** adopts a new identity; the only call that captures an identity is `omt_phase_begin`. Each phase: begin (capture) → observations → `omt_verify` **before a deliberate restart or the trap restore**; a deliberate restart begins a new phase. New counter-example: a replacement during phase 1 followed by a normal second launch must still fail, and the restore must still run. |
| 3. PID reclaim has no real trigger; the bound was overstated | **ACCEPTED.** §4.8 now names concrete entry points: `terminate_pid` (`capture.rs:608-612`) and `enqueue_job_inner`'s `request_for_generation` (`:1466-1490`) / the worker's existing housekeeping path, with a **bounded reclaim inside the insert path** (no new timer). The bound is stated as "live apps + entries still referenced by `desired`/in-flight jobs", not "live apps", which is the contradiction the reviewer found. Reclaim never resets the counter: `pid_generations` is **split** into a small `recent: HashMap<pid, u64>` (bounded, reclaimable) and a process-local monotonic `next_generation: u64` allocator, so a reclaimed pid gets a higher generation than any it has ever had, without keeping a history table. |
| 4. Narrowing the catch boundary does not make an unsafe mid-state safe | **ACCEPTED.** A handler whose mid-state is unsafe is **first** repaired so that a panic leaves the invariant true and its resources released; only then may it be wrapped. An entry that cannot be made safe is **deferred and reported**, never counted as guarded. The acceptance check is four-part (§4.9): the whole panic-touching body is inside one guard; the selector's fallback is the class-defined one; resource cleanup is on both paths; the post-partial state is inline with the class's stated invariant. A scan pass alone never completes the phase. |

Reviewer's answers to round 3's open questions are adopted: a state file is the right carrier (the
location of the assertion, not the carrier, decides authorship); the bounded reclaim at a growth
entry point is enough without a timer; a `#[cfg(test)]` mutation seam is acceptable when it is local
and cannot affect parallel tests.

## 1. Goal and constraints

The user asked for: what is unreasonable in this project's architecture and testing, a written plan,
review until agreed, then — only after agreement — a `dev/...` branch with the change implemented and
the full test suite run there.

`AGENTS.md` is authoritative (tier A decides, tier B never decides pass/fail, a gate that cannot
decide is not a pass, every tier-B defect is promoted to a tier-A assertion, 3 code-review rounds).
The tiers are split by **who decides pass/fail**, which is why §3.3's authority rule is phrased as
"the process that ran the assertion", not "the app process". Nothing is committed or pushed unless
the user asks. GUI tiers need a graphical session and A2 needs per-disturbance consent; the plan
keeps that visible.

## 2. The three problems underneath the findings

**P-A — rules that live only in prose have no mechanism that keeps them true.** `callback_guard` is
declared mandatory in `ARCHITECTURE.md`; measured, 375 `extern "C" fn` definitions carry 87 guards
and 288 do not. Per-field config fallback is declared and its totality unchecked. Each rule needs a
table or a failing test, not more care.

**P-B — the gate cannot decide what it reports.** No gate command; the zero-test guard is one sum
across binaries; A2 reports "0 failed, exit 0" with a third of its scenarios skipped; A1's verdict
is the libtest harness's opinion of a process it never inspected.

**P-C — a small set of genuinely broken invariants.** §4.2, §4.5, §4.6, §4.7, §4.8.

## 3. Tier 0 — the gate

### 3.1 Outcome vocabulary and exit conventions

| Process result | Outcome |
|---|---|
| exit 0 and its own report says the checks ran | **PASSED** |
| exit 0 and its report says NOT RUN, or no report | **NOT RUN** |
| non-zero (including a signal, 128+n), or could not start | **FAILED** |

A **leaf** (scenario, smoke runner, test binary) exits 0 or non-zero and never 75. An
**aggregator** (`run-all.sh`, `smoke-runners.sh`, `gate.sh`) exits 0 (all passed), 1 (any failed) or
75 (none failed, some not run and not exempt); precedence 1 > 75 > 0; a child's 75 is NOT RUN to its
parent. `gate.sh` prints `gate: <step> <outcome>` per step plus a closing table. `--offline` exempts
**only** the enumerated GUI items, which are still printed as NOT RUN.

### 3.2 `scripts/cargo-test.sh`

Whole-invocation default unchanged. Opt-in `--require-match` fails when the **`oh-my-tab` lib test
binary's own `test result:` line** ran zero tests; other workspace lines are ignored for that
purpose. Selftest includes the positive case (one target matches, others legitimately zero) and the
`--require-match` failure.

### 3.3 A1 smoke — the asserting process writes the verdict

Three existing shapes, one wire format `smoke-result: PASSED <n checks>` /
`smoke-result: NOT RUN <reason>`:

| Shape | Who writes the verdict | Where it goes | Example |
|---|---|---|---|
| **in-process** — the `#[ignore]` test itself runs the AppKit assertions | the test process | stderr, and the `<e2e-state>.smoke_result` field when the test was given a state path | `slider_double_click_smoke`, `stepped_slider_rests_on_whole_steps_smoke`, `objc_exception_guard_contains_a_raise`, `collect_windows_smoke` |
| **child** — the test spawns `target/debug/oh-my-tab --smoke-*` | the app process (the only process that ran those checks) | `--e2e-state` file | `overlay_runtime_smoke`, `settings_layout_smoke` |
| **short-circuit** — the runner cannot run here (no Accessibility, no Aqua) | the runner, before returning | same as its shape | `collect_windows_smoke` on missing Accessibility (`window_collector.rs:1824-1829`) |

`scripts/smoke-runners.sh`: enumerates via `cargo test --lib -- --list --ignored`; requires
`cargo build` and a **fresh app artifact** (newer than every file under `src/`) before the tier;
classifies from each runner's own line under §3.1; a child-shaped runner that re-prints its child's
verdict can never upgrade it. Selftests: an in-process runner, a child runner, and a
permission-short-circuit runner each report through the same format and classify identically; a
silent exit is NOT RUN; a signal is FAILED; a stale artifact is NOT RUN. The freshness helper is the
existing `keystroke_display` one (`mod.rs:589-618`) promoted to a shared `test_support` module and
used at **every** spawn site.

### 3.4 A2 — identity per phase, saved and never re-adopted

`scripts/e2e/lib/instance.sh`:

- `omt_phase_begin` — launch through `dev-restart.sh`, then `--whose-instance`, store
  `pid,started` in a phase variable. The **only** call that captures an identity.
- `omt_verify` — `dev-restart.sh --identity-expects=<saved pid>,<saved started>`; exit 0 required.
  It **never** updates the saved identity, so a replaced instance cannot be adopted silently.
  Called after the phase's observations and **before** any deliberate restart and **before** the
  trap's restore.
- **restoring is a phase of its own** (§0a): `omt_phase_close` verifies the tested phase against
  its saved identity **before** anything is restored; then the restore runs; then `omt_phase_begin`
  on the restored instance saves *its* identity; then `omt_verify` checks the restored instance
  against that. The two verdicts are recorded separately (`phase: tested|restore`), and a failed
  tested phase is never overwritten by a successful restore — the scenario's exit status is 1 if
  either phase failed.

A deliberate second launch of the *tested* app is a new tested phase with its own saved identity; a
replacement between two phases is still caught because the previous phase's verify runs before the
relaunch starts.
Under `--gate`, `run-all.sh` fails a scenario that invokes `dev-restart.sh` outside the helper
(source check). Selftests: normal two-phase scenario passes; a replacement during phase 1 followed
by a normal second launch **fails**; the restore still runs after that failure; each
`--identity-expects` exit code (1/3/4/5/6) is reported distinctly.

Consent: `finder-quick-actions.sh` gains `# E2E_CHANGES_PREFS=1`; `scrollbar-style-flip.sh`'s
comment corrected. Selftest: focus+prefs requires both switches. Incomplete runs exit 75 under
`--gate`.

### 3.5 Selftests run, and `gate.sh` has its own

`scripts/selftests.sh` runs every `scripts/*-selftest.sh`, including `cargo-test`, `smoke-runners`
and `gate`. Cases: a child aggregator's 75 propagates as incomplete; a silent child exit 0 is NOT
RUN; libtest output capture does not hide a child's verdict; `--offline` does not exempt a headless
check; zero steps ⇒ 1.

### 3.6 Documentation corrections (docs only)

- `ARCHITECTURE.md`'s layering diagram: `config` is in the leaf row while `config.rs` calls
  `runtime_config::apply_config_change` (`config.rs:3048`) and `key_bindings::validate`. Correct the
  diagram and state that `CONFIG` is a shared cell. No code move.
- `AGENTS.md`: name the three new scripts and the §3.1 vocabulary.
- The `callback_guard` sentence is not weakened; §4.9 brings the code up to it.

## 4. Tier 1 — the invariants

### 4.2 Startup config errors are computed and dropped  [HIGH]

`CONFIG` is built via `let (cfg, _errs) = Config::load_or_default();` (`config.rs:2749`) and `run()`
re-validates the repaired config (`lib.rs:3037`), so an unreadable or unparseable `config.toml`
produces an empty report and the app runs silently on defaults. Change: `load_for_global()` returns
the errors, `CONFIG` stores them, `run()` reports them plus `validate()`, `reload_config` returns
`(errors, repaired)`, `--e2e-state` gains `config_load_errors`. Assertions: a syntax-error config
yields a non-empty report (fails today); a per-field bad value keeps its siblings (§4.3).

### 4.3 Per-field fallback, tested behaviourally  [MEDIUM]

Siblings at distinct non-default legal values; one invalid field; assert in order (the invalid value
**did** produce a validation error; only that field fell back; every sibling kept its value). A local
`#[cfg(test)]` mutation seam replaces `merge_valid` with a section-resetting stub for one test, so
the test is proven able to fail — the seam is scoped to that test and does not touch the global
function (round 3's constraint). Fields are discovered by scanning `validate()`'s source for literal
prefixes; an unrecognized prefix fails the scan rather than being skipped.

### 4.4 Settings: the linkage is unasserted, one documented defect is real  [MEDIUM]

**(a)** Two scans: every control a freeze list or the apply table names is non-null after a build;
every `bind_control(...)` call site names a pointer `control_field_of` has a branch for. The
pointer→field table is scoped to **static field controls**; indexed Finder rows, the
separately-dispatched key-binding rows (`dispatch.rs:77-80`) and labels are outside its domain, and
the table's doc comment says so.

**(b)** `show_app_name_in_cards` changes rendering (`runtime_config.rs:308-312`) but is in neither
`needs_in_place_control_sync` (`:75-89`) nor the in-place refill — the checklist's own example
(`docs/settings-field-checklist.md:51-64`). Fix it; the state-sync smoke asserts it. Fails today.

### 4.5 The orderly-exit duties differ by route  [HIGH]

`quit_application` (`menu.rs:654`) flushes the config; `on_will_terminate` (`lib.rs:817`) is the
logout/shutdown route and does not. One idempotent `orderly_exit_duties()` called by both.
Assertions: the notification route flushes (counter seam); a persist test pauses inside
`write_sealed_file`, calls `discard_history_verified`, and asserts the delete is ordered after the
rename and the generation re-check runs **under** the lock (`persist.rs:1796` bumps before the lock,
`:360-362` re-checks inside); keeping the history still drains; clearing it leaves no file; both
routes together are a no-op the second time.

### 4.6 The clipboard source identity  [HIGH]

`bundle_id_for_pid` (`app_identity.rs:98`) returns `None` for both "no bundle id" and "stale pid";
`copy_is_excluded` (`monitor.rs:240-243`) reads that as "not excluded"; `record_source` (`:257`)
resolves the pid again.

| Case (non-empty list) | Verdict | Records? |
|---|---|---|
| frontmost resolved, in list | Excluded | no |
| frontmost resolved, not in list | NotExcluded | yes |
| frontmost resolved, genuinely **no** bundle id (`NoIdentifier`) | NotExcluded | yes |
| frontmost **lookup failed** (`LookupFailed`) | **Blocked**, whatever `declared` says | no |
| frontmost excluded, `declared` anything | Excluded | no |
| frontmost not excluded, `declared` in list | Excluded | no |
| list empty | NotExcluded | yes |

One resolution producing `FrontmostSource::{Resolved { bundle: Option<String> }, LookupFailed}`, used
by both the exclusion test and the record. The decision and the whole Blocked path run **before**
the payload is read, hashed, cached or icon-extracted (`monitor.rs:104-110`), asserted by a marker.

### 4.7 The pause gate  [MEDIUM]

`pause_gate_settled(old_enabled, new_enabled, paused) -> GateAction` with four cases; `--e2e-state`
`clipboard_gate: paused|baselining|active` asserted by the clipboard smoke. Labelled: the pure
decision is coverage; the state field is the promotion the rule allows.

### 4.8 Two PID containers grow without a reclaim rule  [MEDIUM]

`cancel_pid` inserts into `terminated_pids` (`capture.rs:603-608`), only `activate_pid` removes it
(`:614-619`), and `pid_generations` keeps an entry per PID ever seen; neither has a reclaim rule.

Change — one `PidLedger` with a stated bound and a **real** reclaim entry point:

- **GROWTH-ONLY trigger**: every insert path (`activate_pid` `capture.rs:614-619`,
  `cancel_pid` `:603-608`, and `request_for_generation` `:319-330`) first asks the ledger whether
  this pid key is **new** (absent from both maps). Reclaim runs only on that answer, so a repeated
  request for a known pid — the hot path — never scans and never enumerates. No timer is added.
- **lock discipline**: the workspace query (`NSRunningApplication` for each ledger pid, in one
  autorelease pool) runs **before** `CAPTURE_STATE` is taken and its answer is passed in as a pure
  set; today `app_terminated` holds the lock across Objective-C calls (`pregen.rs:199-206`), so the
  new path is stricter than the code it replaces. Inside the lock only pure Rust set work runs.
- **one row per pid**: `PidRow { generation: u64, terminated: bool }` in a single map, replacing
  `pid_generations` + `terminated_pids`. There is no second container to bound, and no capacity is
  used as race protection.
- **correctness is the generation, not the capacity**: a workspace answer is stamped with the ledger
  generation it was computed against; the in-lock apply is **refused** when any row's generation has
  moved since, so a stale answer can neither clear a newer terminate mark nor re-admit the pid. Every
  request already carries its expected generation and the existing check in `request_for_generation`
  (`capture.rs:325-328`) rejects one that is older than the row's.
- **bound**, counted as pids (one row each) and stated as a set inclusion:

      ledger_pids  ⊆  live_pids ∪ referenced_pids

  where `live_pids` is a **non-stale** workspace answer and `referenced_pids` is the pids of entries
  in `desired` and of in-flight jobs. The ledger's own size is not in the right-hand side, so the
  assertion can fail. `desired` also carries background pregen and prefetch jobs, so the bound is not
  the visible-window count and the plan does not claim it is.
- **reclaim**: a row is dropped when its pid is not live in a non-stale answer, absent from
  `desired` and absent from every in-flight job's key. Rows kept for (b)/(c) are what keep an old job
  rejected; a terminated row whose pid is not referenced is dropped like any other, because a stale
  answer cannot revive it (the generation guard above). The bound is promised **after each
  growth-triggered reclaim**, not continuously, and the plan does not claim lazy shrinking.
- **generation without a history table**: the counters live in the reclaimed table; a reclaimed pid
  is given `next_generation` from a single process-local `AtomicU64` that only increases, so its
  next generation is greater than any it ever had and the `0` default can never revive an old job.

Assertions: a fresh incarnation of a cancelled pid is capturable; an old job for the previous
incarnation is **still rejected after reclaim**; thousands of distinct launch/terminate cycles leave
`ledger_pids` inside the right-hand side immediately after each reclaim (no global cache clear); a
generation bump between the out-of-lock query and the in-lock check still rejects the old job; and the
**stale-answer** case the reviewer specified — pause a query, produce more terminations than any
capacity the old design had, resume it, and the old pid is still refused. The **negative control**
reuses the same operation sequence with an **instance-local** `#[cfg(test)]` seam that disables
reclaim on that ledger only (no global switch, so parallel tests are unaffected); it asserts that the
detector reports the violation while the suite itself still passes.

### 4.9 `callback_guard` is not the boundary it claims  [HIGH]

Measured: 375 definitions, 87 guarded, 288 unguarded in 30 files.

Staged, with round 3's rule that containment is not a repair:

1. **Classify by selector contract**: void UI actions; `Bool` predicates; object-returning selectors
   (`fieldEditorForObject:`, `outline_hit_test`, the `glass.rs` shapes); notification handlers
   (`v@:@`); timers; event taps. Per class: the fallback, the resources to release, and the
   **invariant that must hold when the handler panics mid-way**.
2. **Make the unsafe classes safe first.** A handler whose partial execution would leave a
   SettingsUi/`CONFIG`/cache state invalid is repaired so that the invariant holds on the panic path
   (order the mutation so the observable state changes last, or restore it in a guard that runs on
   unwind) *before* any boundary is added. A handler that cannot be made safe is **deferred and
   listed with its reason** and is not counted as guarded.
3. **Guard the whole panic-touching body**, not a fragment: an `extern "C" fn` whose body can
   panic must have its entire body inside one `callback_guard::…(|| …)`. A guard around an inner
   fragment leaves the rest outside the boundary and fails the scan.
4. **Acceptance is four-part**, not the scan alone: whole body guarded; selector-appropriate
   fallback; cleanup on both paths; post-partial invariant asserted by a test for that class.
5. **Batches**: per file, disjoint, lead owning the scan test, the helper changes and the final gate.

§7 no longer claims containment is a complete mitigation: it contains the panic and, for the
repaired classes, preserves the invariant; for deferred entries it does neither yet and says so.

## 5. Tier 2 — small fixes and coverage, labelled

| # | Item | Class | Assertion |
|---|---|---|---|
| 2.1 | `is_gui_smoke_process` hand-list; `--smoke-panel-keys` makes the report branch unreachable (`lib.rs:3548`) | gate shape | a test that every `--smoke-` literal is in the one table or a documented exception |
| 2.2 | `refresh_highlight` may run more than once per summon | perf, unmeasured | existing GUI smoke — **coverage** |
| 2.3 | `visible == true` branch skips `begin_switcher_activity` | **coverage** | state-sequence test; does not prove the old branch wrong |
| 2.4 | the generation guard's publish decision unasserted (`window_refresh.rs:707`) | coverage → regression | `decide_publish` returns false for a stale generation and a transition-active result |
| 2.5 | the excluded-app candidate list captured at build (`page_builder.rs:2529`) | coverage | smoke: hide, reopen, a new candidate appears |

Dropped earlier and recorded in the backlog with revisit triggers: the mouse cache snapshot and the
switcher cross-product fix (withdrawn in §9).

## 6. Verification

Per change: `cargo fmt`; `cargo check --all-targets`; `cargo clippy --all-targets` with no new
warnings; `cargo check --release`; targeted `scripts/cargo-test.sh <filter> --require-match`.
On the branch: `scripts/gate.sh` and, separately, `scripts/gate.sh --offline`, both reported line by
line; the A1 counts with each NOT RUN reason; the A2 per-scenario verdicts; `build-version` from
`scripts/dev-restart.sh` after runtime changes. GUI tiers that cannot run stay NOT RUN; the task is
not called complete on a green `--offline` alone. §5 labels regression tests versus coverage.

## 7. Out of scope

`lib.rs`/`config.rs` splits; allocation findings #34–#38; `MainThreadSlot` reentrancy as a defect
(no reachable re-entry); theme token literals (backlog #41); `mapping.rs` writing `CONFIG` outside
`apply_config_change` (unverified without a device).

## 8. Sequence

1. Phase 0 — gate (§3). 2. Phase 1 — callback guard (§4.9), classification and repairs first.
3. Phase 2 — invariants (§4.2, §4.5–§4.8). 4. Phase 3 — drift surfaces and small fixes (§4.3, §4.4,
§5). 5. Phase 4 — the gate on the branch, docs and backlog re-triaged.

Branch `dev/arch-test-remediation`; each phase a commit; nothing committed or pushed unless asked.

## 9. Retractions

Clipboard purge ordering; `clipboard_paused`/freeze-list; `refresh_service_controls_from_config`;
`load_settings_from` ordering; the architecture tree's `mod.rs` rows, clipboard directory and
`ui/corner_glyph.rs` placement; `merge_valid`'s `thumbnail_size` branch; the settings-prewarm guard;
the switcher/bindings double claim in both forms (`MODIFIER_MASK` includes Command, so an
exact-match engine cannot match a chord the listener claims via an extra modifier); and
`terminated_pids`'s "missed notification" framing.

## 11. Status

Design rounds run: 6. The plan is revised after each round rather than defended; every finding of the
first five rounds is either adopted or recorded as a retraction in §9 with the source evidence. When
this revision is agreed, implementation starts on `dev/arch-test-remediation`, and the code review
that follows is a separate requirement — this design round cannot stand in for it.
