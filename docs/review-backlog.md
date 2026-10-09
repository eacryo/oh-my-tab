# Review backlog

Findings that were valid but out of scope under *Review budget and triage* (AGENTS.md), plus known gaps that
no round closed. Nothing here is a claim that the behaviour is correct — each entry is a decision to schedule
the work instead of doing it inside the change that surfaced it.

Add an entry when a round is triaged, not later: round, severity, why it is deferred, and the trigger that
brings it back. Remove an entry when its work lands, and say so in the commit or release note.

## Open

| # | Item | From | Severity | Why deferred | Revisit trigger |
| --- | --- | --- | --- | --- | --- |
| 1 | Cross-desktop minimized window: no A2 scenario asserts the restore path end to end | feature review, R9–R12 | MEDIUM | the path exists and is covered by A1/A2 for the current desktop; the cross-desktop variant needs a second desktop with a minimized window, which the scenario cannot set up deterministically | next change to `raise_other_desktop_window` or the minimized restore path |
| 2 | Same app with windows on two desktops: no A2 scenario | feature review, R9–R12 | MEDIUM | requires the app to own a window on each desktop; the frame-level `ax_published_pids`/`ax_recovered_pids` evidence only stops the existing scenario from mis-failing on it | next change to cross-desktop admission |
| 3 | A real second window whose desktop has not been visited since this process started stays hidden | WeChat duplicate fix | LOW (accepted trade-off) | hiding it is what kills the dead duplicate card; the window appears after its desktop is visited once | if a user reports a missing cross-desktop window |
| 4 | An app whose `kAXWindows` **and** key/main slots are both empty is refused outright, so a real window it owns on another desktop stays hidden | WeChat duplicate fix, narrowed again by the Stats fix | LOW (accepted trade-off) | in that state nothing tells a real window from a closed menu-bar panel, and both wrong answers are worse than hiding it: admitting the CG entries put Stats' closed "Combined modules" panel on screen as a dead card, while a failed *query* (a different state) keeps the fallback for windows that are visible | if a user reports a missing cross-desktop window for an app that answers AX with nothing at all |
| 6 | Synthetic Dock swipe for an instant, menu-bar-correct Space switch (BetterCmdTab) | improvement candidate | — (optimisation) | ported and measured 2026-10-07: the three-phase gesture (`CGEvent` fields 55/110/123/124/129/130/132) is created and posted successfully (`delivered=true`) both from a plain process and from inside the app process, and the Space does **not** move on macOS 26 in either case. The reference also installs a swipe-suppressor event tap and gates the feature behind its `instantSpaceSwitch` preference, so the event sequence alone is not the whole mechanism. The probe stays behind `--space-swipe=left|right` (`src/space_swipe.rs`) for the next attempt | if the animated cross-Space transition is reported as too slow, or when the suppressor-tap interaction is understood |
| 10 | Thumbnail card size still moves a ladder step when the window **count** crosses a threshold, and a set much wider than the base ratio can need a scroll row at the chosen size | user reports, 2026-10-07 | LOW (recorded trade-off) | the user chose to keep the current behaviour for now and have it documented: `docs/design-style{,-en}.md` §9 states the policy and `docs/developer-notes{,-en}.md` the mechanism, including the reproducible log line | when the size jump is reported again, or on the next overlay-layout change: (a) prefer the largest step whose overflow is at most one teaser row before stepping down, and (b) smooth the count thresholds (more steps, or a row-count anchor). **(c) is done**: the user-pinned Thumbnail size dropdown (2026-10-07) removed the order dependence, and `auto` now estimates rows from the count and the widest card only |
| 18 | A fullscreen window **on another desktop** has no end-to-end card assertion (its `fullscreen` flag and `window_server` source) | state-decode review, R1–R3 (this session) | LOW (coverage gap) | the cross-desktop state assertions now land in `scripts/e2e/space-desktops.sh` (minimized/fullscreen/sources/`ordered_in`/`ax_pairing`/raw row fields, PASS 2026-10-08) and the `0x20` Space mask was verified on this machine by toggling `AXFullScreen` on an off-desktop window; what is still missing is a scenario that puts a window in a fullscreen Space *and* asserts its card's fullscreen flag (this app cannot fullscreen its own probe window, so the scenario would have to drive another app) | next change to `space-desktops.sh` or `window_state.rs` |
| 19 | The A2 scenarios disagree about the switcher chord: `space-desktops.sh` now derives it from `keyboard.modifier`, but `tab-repeat.sh` hard-codes Command+Shift+Tab and `switch-basic.sh` defaults to `cmd+tab` instead of reading the config | cross-desktop state review, 2026-10-08 | MEDIUM (instrument) | both scenarios pass on a Command-configured machine (this one), and the shared `scripts/e2e/lib/hotkey.py` regression cases already cover the mapping, so this is a consistency fix rather than a broken gate | next change to either scenario, or the first time the suite runs on a machine configured with `option` |
| 11 | No A-tier coverage for a settings control writing the config (any dropdown, including Thumbnail size) | this session | LOW (coverage gap) | the app's custom select needs a real mouse-down to open its menu, and no scenario drives one; the read direction (config -> popup) and the mapping are covered, the write direction is code-reviewed only | when a smoke can drive a control with persistence suppressed, or when one of these writes breaks |
| 16 | The keystroke display HUD keeps its native window shadow instead of the shared elevation carrier | panel outline/elevation change, 2026-10-07 | MEDIUM (recorded decision) | it is placed `PANEL_EDGE_MARGIN` (18pt) from the visible edge, and a `high`-level shadow needs 64-76pt of window padding; measured, AppKit then constrains the padded window back into the visible area and the panel moves by 26pt, which also changes what a saved `keystroke_display.position` means. Giving the HUD a shadow is a placement decision of its own (move it inward, or pad per-edge against the screen edge it sits on), not a second elevation level | next change to the keystroke display's placement or saved position, or when the HUD's native shadow is reported as the dark ring the other panels turned off |
| 17 | `liquid-glass` panel outline is drawn by the shared decoration view, so it coexists with the glass's own edge highlight | panel outline/elevation change, 2026-10-07 | LOW (to watch) | measured on the picker: the stroke is 1pt at the `card_border` tone on the panel's own edge and nothing else changed; a double edge on `.regular` glass has not been reported or measured as a defect. The document's §3.4 states the outline is one visual contract across materials, so a material-specific exception would have to be a documented one | if the glass edge reads as two rims, or on the next change to the glass style/tint |
| 12 | ~~A fullscreen window whose origin is the current desktop is missing from that desktop's cards~~ **CLOSED 2026-10-07** by the location-based contract: every fullscreen Space's window is admitted whatever the switch says, so neither a missing origin nor the CG-only gate's old `OtherDesktop` requirement can hide it (`space-desktops.sh` pins it: "with the switch off a fullscreen window is still a card") |
| 13 | `scripts/e2e/space-groups.sh` cannot run on this machine: it needs a bound "move left/right a space" shortcut for its `ctrl+left` step, and keys 79/80 are enabled with no keys assigned | this session | LOW (environment) | the scenario is a focus scenario (skipped by `run-all.sh` unless `--include-focus`); the origin logic it covers is asserted by A1 unit tests, and its fullscreen phase was exercised once (it learned the origin and listed the fullscreen window) | when a Space-switch shortcut exists, or when the scenario is reworked to switch Spaces by another means |
| 14 | `scripts/e2e/space-desktops.sh` picks its cross-desktop target by heuristic, so it can land on a window the app cannot admit yet (the documented "desktop not visited in this process" narrowing) and report an unrun phase | this session | LOW (instrument) | the scenario now filters to an ordinary other desktop whose app has no window here, and reports NOT RUN with the admitted cards instead of a false FAIL; picking the target from the admitted other-desktop cards themselves needs a scenario restructure | when the scenario is next touched, or when a target choice hides a real admission regression |
| 15 | ~~The fullscreen origin is inferred from the current Space adjacency, so a reordered Space list groups that Space with the wrong desktop~~ **CLOSED 2026-10-07**: the origin no longer takes part in admission (the contract reads the window's location), so a wrong association can no longer hide or misplace a card; the inference stays as an e2e diagnostic, with `a_reordered_space_list_groups_by_adjacency` still pinning what it reports |
| 9 | `space-desktops.sh`'s capture-eligibility check reports NOT RUN when the summon frame arrives before the thumbnail workset is published | this session | LOW (instrument) | the check is honest (NOT RUN, never a false PASS) and a re-run passes; making it wait for a published workset is instrument polish | next change to that scenario |
| 8 | Config-change in-flight invalidation has no multi-thread interleaving assertion | feature review, R9 | LOW | the behaviour is asserted end to end (A2 switch off/on); the interleaving needs a deterministic hook | next change to `runtime_config` invalidation |

## Closed in the same session (do not reopen)

The rounds listed below were all fixed in-session before the budget policy existed; they are recorded here only
so a later reader does not re-derive them. The fixes are in the tree, with their A1 counter-examples.

- R12–R18: ownership of the staged eligibility evidence (per-pass staging, `FORGET_EPOCH`, prewarm paths).
- R21–R22: the card render decision (`thumbnail_rendered`), the exact-focus check ordering, and the normal-path
  branch assertion in `scripts/e2e/space-desktops.sh`.
- R24–R28: AX-identity history for cross-desktop admission (per-thread evidence staging, counted pass epochs,
  lifecycle veto, mark reclamation).
- Origin-Space front process (was item 5 here): **not a defect on this machine.** Measured 2026-10-07: a
  cross-Space commit followed by a return through `SLSManagedDisplaySetCurrentSpace` left the origin Space
  showing the app that was frontmost before the switch, and the user then ran the real scenario -- switch
  away with the switcher, return with a four-finger horizontal swipe -- and the front app was correct. The
  reference implementation repairs this with `SLSSpaceSetFrontPSN` because it fronts apps through
  `_SLPSSetFrontProcessWithOptions`; our activation path does not clobber it, and the SLPS *rescue* path
  (which shares that call) was not exercised in either test because activation landed the switch. If a
  report of "returning to a desktop shows the wrong front app" ever arrives, the fix is prepared: snapshot
  `NSWorkspace.frontmostApplication` and the origin Space id before the front-switch, then call
  `SLSSpaceSetFrontPSN` afterwards, only when the target's cross-Space membership is confirmed and the
  origin's front was not us (the guard that avoids undoing the raise, AltTab #5586).
- `scripts/e2e/settings-layout.sh` (was item 7 here): the chronic 33/44 failure was one stale constant
  (`TOP_PADDING = 50` vs the normative 48) plus three more expectations left behind by the same token
  migration (in-row button height 28 -> 32, row-to-row 62 -> 60 = 52 + 8, card-to-header 77 -> 72.2 =
  24 + 4 + 20 + 16.2 + 8). The 58.7 step that looked off-grid is the same row pitch measured through a
  row's value label, which sits 1.3pt above its row label. Repaired to 44/44 with no tolerance widened.

- The terminal classification cannot read the system frontmost app yet. `NSWorkspace.frontmostApplication`
  belongs to the main thread and `terminal_evidence` runs on `ax-raiser`, so the read is gone and
  `frontmost_pid_matches` is `None`: while the raise experiment carries `--activation-api` the terminal is
  therefore `unknown` rather than `landed`, and the product path records no terminal at all (`terminal=-`,
  measured 2026-10-08, `scripts/e2e/space-desktops.sh --include-focus` still PASS). Deferred because the
  honest fix is the main-thread verification channel the phase-2 repair step has to build anyway, and
  keeping a background AppKit call alive for a diagnostic is exactly the breach the invariant forbids.
  Trigger: the change that adds that channel (then sample the frontmost pid on the main thread and carry it
  back as plain data), or any move of the terminal into product behaviour.

- Recovery-coverage evidence for the cross-desktop raise (raised in review round 2 as MEDIUM: deleting the
  pre-action wait also shortened the recovery coverage). Three states still lack a counter-example: a first
  AX raise that matches no element, a target that only arrives late, and a cross-desktop *minimized* restore.
  Deferred because each needs a purpose-built scenario -- an element absent on the first attempt, a delayed
  arrival, a minimized window parked on another desktop -- and nothing can force those states from outside
  today. What *is* covered: `scripts/e2e/space-desktops.sh --include-focus` drives the rescue branch end to
  end and now reconciles the system frontmost pid **and** the exact focused window (phase 4), and
  `--other-desktop-no-activation` makes the suppressed-activation branch deterministic
  (measured 2026-10-08: `activation=false`, `first_rescue_ms=0`, `elapsed_ms=216`, target app frontmost,
  focused window == the selected one).
  Trigger: the next change to the raise or recovery path, or the first report of a delayed or minimized
  cross-desktop switch that does not land.

- The notification **click** path has no assertion. `did_receive_notification_response` (the
  `didReceiveNotificationResponse:withCompletionHandler:` delegate method) is exercised only when a user
  clicks an update or clipboard notice, and it cannot be driven from a smoke runner: the callback reads
  `response.notification.request.identifier`, and `UNNotificationResponse` has no public initializer to
  build a fake one from. Its arity is correct -- three explicit arguments, unlike the `willPresent`
  callback that crashed on 2026-10-08 because it was one argument short and therefore read the
  notification object as the completion handler -- and its zero-argument completion block uses the same
  hand-rolled invoke the module's authorization block already runs successfully. Deferred as unrun
  coverage. Trigger: any change to the click path, or a report that clicking a notice does nothing.

- The keychain prompt is **solved and the mechanism measured** (2026-10-08): the ACL is judged against the
  app that *created* the item, so an item created by an ad-hoc build makes every later build a stranger
  (`SecItemDelete` on it is refused with `errSecInvalidOwnerEdit`), while an item created by the Developer
  ID-signed build survives rebuilds (probe and real app: 67 ms, no prompt). The dev bundle now signs with
  Developer ID and the one-time reset -- user deletes the item, the app creates a fresh one and keeps the
  old storage aside as `*.failed-<ts>` -- was performed on this machine. No backlog work is pending here;
  what remains unverified is a *release* install that predates Developer ID signing, which would need the
  same one-time reset (the release channel does sign with Developer ID today).
