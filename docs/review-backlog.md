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
