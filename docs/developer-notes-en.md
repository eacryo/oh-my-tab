# Development Notes

These notes cover issues that affect source builds and debugger-launched binaries, not users of Homebrew installations or packaged `.app` builds. The normal development path is `scripts/dev-restart.sh`; a bare `cargo run` is reserved for low-level diagnostics. The README links here for development-only issues.

## Floating panels: the panel/window rect contract, the outline and the elevation shadow

The three floating panels (the switcher overlay, the clipboard picker and the clipboard detail) draw a 1pt
`card_border` outline and, at the `high` level, an elevation shadow. Three things about that are easy to
break, so they are stated here rather than only in the code.

**Two rects, one meaning each.** A panel's *window* is larger than the panel: the window server clips
whatever exceeds a window's frame, and the shadow needs room outside the panel, so the window is the panel
rect padded by `theme::elevation_insets(level)`. The **panel rect is the semantic one**: layout,
hit-testing, the picker/detail group geometry, `PICKER_EDGE_MARGIN`, `clamp_into_visible`, the switcher's
size budget, the keystroke HUD's saved position and the `--e2e-state` geometry all mean the panel. Only
`setFrame:` gets the padded rect. `glass::set_panel_frame` / `animate_panel_frame` / `panel_frame_of` are
the only ways to convert, and every panel resize must go through them: a raw `setFrame:` with a panel rect
shrinks the window, which shrinks the material through its fixed autoresizing margins — measured, the
keystroke display's keycaps then overflowed their strip by 8pt, because they are laid out for the panel.
`panel_frame_of` returns the frame unchanged for a window with no remembered padding, so it is safe on
windows that never installed a backdrop.

**The padding is a measurement, not arithmetic.** `radius + |offset|` is where a layer shadow's core ends,
not where it reaches zero: padding by the radius alone left the tail 8 tone units dark at the window's
outermost ring. `theme::ELEVATION_SHADOW_TAIL_ALLOWANCE` is what the measurement asked for, and the
padding also comes out of the placement budgets and the visible-area clamp (a padded window that overflows
is constrained by AppKit, which *moves the panel* — measured, 26pt at the menu bar — instead of clipping
the shadow). `scripts/e2e/panel-edge.sh` is the command that reproduces the verdict.

**The shadow needs a carrier, and the carrier must not clip.** `masksToBounds` clips a layer's own shadow,
and every material sets it (glass's is load-bearing), so the shadow cannot live on the material layer. The
material is therefore a child of a carrier view, and the shadow is on a **raw `CALayer` sublayer** of the
carrier, never on a view's layer: AppKit reconfigures view-managed layers, and `addSubview:` was measured to
zero a view layer's `shadowOpacity` while leaving its radius — a shadow that silently renders nothing.
`CALayer.shadowPath` copies what it is assigned ("Upon assignment the path is copied", in Apple's own
header), and Core Animation holds CF-typed layer properties with CF ownership (Apple QA1565), so the path is
created, assigned and released in one place (`ffi::layer_set_rounded_shadow_path`) and the layer keeps its own
copy. The colour helpers on the same layer need no cache for the same reason. The
carrier itself must not clip (`masksToBounds = false`): the glass variants draw their edge outside the
glass's bounds, which is why the keystroke-display smoke now asserts *the host does not clip* directly
instead of the class proxy it used before (glass itself, or an `NSVisualEffectView`) — that proxy rejected
any non-masking host, the carrier included, while the property the original bug was about is the mask.

The carrier's `layout` recomputes the shadow path from its own bounds, and that hook is deliberate: the
panels resize *animatedly* (the picker and detail open and close, the switcher reflows as cards close), so
a path set at each resize call site would be right for the first and last frame only.

**The outline is a decoration view above the material.** One implementation for all three materials: the
material branches differ (frost carries a rounded mask image, glass clips itself) and `swap_backdrop`
migrates `content_parent`'s subviews, which is how a decoration would otherwise be carried into the new
hierarchy and leave a stale stroke behind. It overrides `hitTest:` to return nil, because it covers the
whole panel and would otherwise swallow every click meant for the panel's content.

**Development switches.** `--panel-outline=off|after:N` and `--panel-shadow=off|med|high|after:N` exist for
the A2 measurements: each produces the counter-example frame the assertion diffs against, and `after:N`
does it inside one launch (a translucent material does not re-render identically across launches, so the
pair has to come from one). `--panel-shadow=off` removes the carrier entirely, which also makes it the
pre-carrier baseline for the blur-retention gate. None of them may be left running: `scripts/e2e/run-all.sh`
checks the running process for all of them, alongside `--panel-backdrop` and `--clipboard-blank-text`.

## Icons may be incorrect in development mode

When running the bare binary with `cargo run` for diagnostics, the overlay may occasionally show oh-my-tab's own card as an initial-letter placeholder instead of its application icon, and the problem may persist until the icon cache is cleared manually. The icon cache is keyed by bundle ID and uses the executable **mtime** as its invalidation fingerprint. Each development build relinks the binary and changes its mtime, invalidating the running instance's cache entry. Packaged `.app` builds are unaffected because the installed binary's mtime remains stable. For normal development runs, use `scripts/dev-restart.sh`; if the diagnostic binary shows this issue, use the *Clear Icon and Thumbnail Cache* menu item or delete `~/Library/Caches/oh-my-tab-icons/`.

## Mouse control may fail when launched from a debugger

When the app is launched in Debug mode through RustRover or another debugger, frequent mouse activity during startup may cause scroll reversal and per-device mouse settings to stop working. In that state, the app stops receiving mouse events, scrolling returns to the system default, and pointer-acceleration changes no longer apply until the app restarts. This issue has been observed with unsigned development builds launched by a debugger and appears related to macOS 26 restrictions on HID-layer event monitoring. Packaged `.app` builds and binaries launched from a terminal have not shown the same behavior.

## Fullscreen Space grouping

**Since 2026-10-07 admission no longer reads this association** (see the next section for the contract): the switcher still records a fullscreen Space's source desktop only after observing a matching WindowServer leave/join pair and confirming the target Space type and display, but that record is **e2e diagnostics only** (`space_contexts` reports which desktop a fullscreen Space is attributed to). A fullscreen window already present at launch, an event gap, or ambiguous display/source evidence simply reports "no origin" in the diagnostics -- it changes nothing about which windows appear. Space membership notifications are consumed in order. A Space switch that grouping handles correctly leaves the candidate set unchanged, so `--e2e-state` publishes a separate `refresh_context` frame when the active Space context changes; scripts must assert only on applied frames (`refresh` / `refresh_context`), since other frames pair the new context with the previous card list. Only a known event loss (queue overflow) clears pending transition evidence; a membership event whose owner cannot be resolved merely downgrades the diagnostic continuity flag, because a fullscreen transition emits such events for auxiliary windows of the same app and clearing on them destroyed the transition being learned. The pair window is 3s rather than 1s: the 1325 join can land while SkyLight still types the new Space ordinary, and the pair must survive until a later query corrects the type. Legacy membership fallback admits only positively onscreen windows and never treats fullscreen bounds as a cross-Space exemption. These private SkyLight event/query paths are best-effort and may be unavailable on future macOS releases.

### Windows on other desktops

**The contract since 2026-10-07: admission reads *where* a window is, never which desktop a
fullscreen Space came from.** The current Space's windows and every fullscreen Space's windows are
always candidates (a fullscreen window has no AX element while it is on its own Space -- `kAXWindows`
is filtered by the current Space -- so its reachability cannot depend on an origin association macOS
never exposes); another *ordinary* desktop's windows need `windows.show_other_desktops` (labelled
"Always show windows from other desktops"), and are admitted too while a fullscreen Space is current,
when every desktop's windows are shown. The origin association (learned, or inferred from the native
Space order) is diagnostics only now, so a missing or wrong one can no longer hide a window; the price
is deliberate cross-Space bleed: with the switch off, other desktops' fullscreen windows still appear.
An admitted cross-Space window carries no AX element, so it keeps the CG window name as its title, has
no readable minimized state and is never re-captured. Admission stays under AX authority. When an app's `kAXWindows` is empty but its key/main slots
still name one of its windows, AX has answered about that app's window set: a CG window in neither
slot and never identified by AX before is a surface AX excludes, and is refused -- 微信 keeps a
280x380 off-screen window titled 微信 beside its real 1097x833 window, and from another desktop that
helper used to become a second card whose raise does nothing (`ax NO MATCH`). The rule runs before
the other-desktop exception because such an app lands in `ax_empty_pids`, which that exception would
otherwise admit. A small history of AX-identified windows (pid + process start + CGWindowID, cleared
on destroy with a veto against older collections and on process end) keeps a real second window
admissible once its desktop has been visited; a real window whose desktop has never been visited
since this process started stays hidden until it is. When the key/main slots are empty too, AX has named no
window of that app at all, and the app has no switchable window to show: its CG entries are panels and
are refused on every desktop. Stats is the observed case -- a 280x800, layer 0, parentless, off-screen
menu-bar panel titled "Combined modules" that answers nothing to AX (`kAXWindows` empty,
`AXFocusedWindow`/`AXMainWindow` unsupported); it became a dead card on its own desktop as well as
across Spaces, and selecting it did nothing. The three states are distinct and must not be conflated:
AX named a window (the other-desktop exception applies, still behind the membership and shape gates),
the AX query *failed* (`ax_failed_pids`, nothing is known, the CG fallback stays), or AX answered and
named no window (refused). A read that came back with no window element while one of the three
attribute reads failed counts as a failed query, not an empty answer: the window list is
Space-filtered, so the window may have been reachable only through the key/main slot that just failed.
A failed query admits only a window that is **on screen**: with no AX answer nothing distinguishes a
real window from a closed menu-bar panel, so the fallback covers only what the user can see (freezing
Stats so its AX read times out used to admit both its visible settings window and its closed
"Combined modules" panel -- the pair a user reported).

Admission requires a non-empty membership list that names a Space the
accepted topology manages: an empty or unmanaged list is the shape an orderOut'd or helper surface
presents, and stays rejected with the switch on. The switch never widens the legacy fallback, which
cannot tell another desktop from an off-screen window. The minimized option still applies first --
against the supplemented state below, so a window the WindowServer reports as minimized on another
desktop is filtered by it too.

**State on another desktop comes from the WindowServer, not from the app.** A window on another
desktop is usually absent from `kAXWindows` (AppKit builds that list from a Space-restricted
WindowServer query), so the accessibility answer cannot describe it. One batched
`SLSWindowQueryWindows` call per collection pass carries what the card needs for every window the
pass can name: `attributes` bit `0x2` (ordered in), `tags` bit 60 (minimized) and bit 39 (the app is
hidden), and `space_type_mask` bit `0x20` (the window's Space is a fullscreen Space). The decode
precedence, the per-field evidence sources and the counter-examples live in
`src/window_collector/window_state.rs`; the raw bits and the source of every presented flag are
published per card by `--e2e-state` (`minimized_source`, `fullscreen_source`, `ordered_in`,
`ax_pairing`, `ws_*`). The bit positions are undocumented WindowServer fields. `--space-state-record` walks the app's own
probe window through ordered-in, ordered-out, minimized, restored, app-hidden and app-unhidden and
prints the raw fields, and `--smoke-space-state-matrix` asserts the recorded matrix (it fails, rather
than passes, while a cell is unpinned). Measured on this machine (macOS 27.0.1 / 26A434,
2026-10-07): being ordered in sets `attributes` bit `0x2` and ordering out clears it; minimize sets
`tags` bit 60 (`0x200100482001` -> `0x1000200100480001`) and clears `0x2`; `deminiaturize:` brings
`0x2` back and clears bit 60 in the same 20ms-polled sample (no sample showed an ordered-in window
with the minimized tag still set); hiding the app sets `tags` bit 39 (`0x208100480001`). Two further facts
from the same run: an ordered-out window is NOT minimized (its tag loses only the on-screen bit 13,
which is the reference implementation's #5714 discrimination), and `kAXWindows` omits an ordered-out
window while a hidden app's windows stay listed. The fullscreen Space mask `0x20` was then verified on a real
cross-desktop window by toggling `AXFullScreen` on it: the row read `mask=0x20` while the window sat
in a newly created fullscreen Space, and `mask=0x1` again after leaving it. `--space-state-record`
itself cannot produce that cell, because this app is a menu-bar application and AppKit refuses
`toggleFullScreen:` for it.

Three further measured facts about a window that is on another desktop, each of which the naive
model gets wrong:
- its `attributes` bit `0x2` stays SET while it is unminimized (the bit describes the window's own
  Space, not "on the screen the user is looking at"); minimizing it there clears `0x2` and sets the
  minimized tag, and restoring it brings `0x2` back with the tag already clear in the same 20ms
  sample. The presented state is therefore unchanged by which desktop is current.
- `kAXWindows` lists nothing for an app whose windows are all on another desktop, while
  `kAXFocusedWindow`/`kAXMainWindow` still name such a window: that is the `RecoveredAx` route the
  switcher admits it through (and the reason a cross-desktop card can exist at all). An app that
  answers accessibility with no window at all stays refused, as documented above.
- the remote-token sweep reaches such a window's element (measured: 43 ids for an off-desktop
  Chrome window), which is what the action path would use if it ever needed more than key/main.

The end-to-end assertions for a cross-desktop card -- `minimized`/`fullscreen` and their evidence
sources, `ordered_in`, `ax_pairing`, the raw row fields -- live in `scripts/e2e/space-desktops.sh`
(run it with `--include-focus`); the scenario derives the injected chord from `keyboard.modifier`
rather than assuming one.

The minimized decode deliberately lets the ordered-in bit outrank a minimized claim. The reference
implementation measured a *Dock* restore where the WindowServer keeps its minimized tag set for a
while after the accessibility read already says "restored"; this repository's own recording used the
programmatic `deminiaturize:`, where the tag was already clear in the same sample that showed the
window ordered in again, so it did not reproduce that delay and says nothing about the Dock path.
The rule is the conservative one either way: while the WindowServer reports the window ordered in, it
is not presented as minimized. When the ordered-in field
could not be read, the accessibility answer decides alone and the WindowServer tag is consulted only
for a window accessibility does not publish at all.

Thumbnails: such a card never *requests* a capture -- the private capture call this app uses
(`SLSHWCaptureWindowList`) cannot capture a window off the active desktop, and no reference
implementation asks it to -- but it does show a frame the cache already holds from when the window was
on its own desktop -- the producers filter it out while the renderer keeps reading the cache, and the A2 snapshot
publishes `thumbnail_ready` so that behaviour is assertable. Refusing to render that frame as well was
wrong: a user who visits both desktops has one for most cards, and reference implementations keep the
last thumbnail the same way.

Activation: the exact-window front-switch (`_SLPSSetFrontProcessWithOptions` with the window id and
the userGenerated mode, plus the targeted click) and app activation (`NSRunningApplication
activateWithOptions:`) both move the Space; which one lands first depends on the state. AltTab relies
on the front-switch for a cross-Space target ("it also makes macOS switch to a Space showing it") and
BetterCmdTab keeps it as its menu-bar-correct cross-Space fallback -- after rejecting
`CGSManagedDisplaySetCurrentSpace`, which sets the Space directly but skips the Space-transition
machinery and left the destination without a menu bar when leaving a full-screen Space. This project now
sends the exact-window front-switch and the targeted click **immediately** (it no longer waits for the
window to join the active desktop), and submits the AX raise with them; a window that has not arrived
yet simply misses that raise, and one bounded late re-check re-applies it
(`OTHER_DESKTOP_LATE_SETTLE_BUDGET`). The order was chosen by measurement, with one caveat: these numbers are **phase
timings inside the raise worker** (zeroed when the worker starts, so they exclude the earlier
activation call and the queueing, and they are not the moment the five success criteria first held).
The waiting version took 3,227-3,288ms per cross-desktop commit (worst case 4,613ms, which never landed
at all), the immediate version 139-291ms on the same machine (2026-10-08, macOS 27.0.1/26A434; from the
`[raise] other-desktop terminal` records of `scripts/e2e/space-desktops.sh --include-focus`:
`first_rescue_ms=0`, `first_ax_attempt_ms<=30`, `elapsed_ms` within that range). The reference
implementations do not wait for this signal either: AltTab and DockDoor do not wait at all,
BetterCmdTab waits only for the Space-change notification on its own synthetic-gesture path, and
vorssaint-utils waits on Space visibility rather than on a window's `kCGWindowIsOnscreen`. The signal
this project keeps is still the CG flag (`kCGWindowIsOnscreen`, option 8): the membership query lags
during a transition -- measured, the membership read taken right after the CG flag already reported
`onscreen=true` still showed the target's Space and the active Space disjoint (the two reads run one
after the other, they are not two samples of one instant). `--raise-wait` restores the waiting path for comparison only and is
off by default. One more limit: the terminal's system-frontmost criterion is unavailable
until the main-thread channel exists, so under the experiment switch the terminal can only be
`unknown` -- it never claims `landed`. An earlier
version relied on app activation alone and skipped the front-switch; macOS refuses that activation in
some states (`activateWithOptions=false` with the target never becoming frontmost, which is what the
user's log showed), and such a card then did nothing at all. The earlier note here claiming the
front-switch "reports success while the active Space stays put" came from a probe binary without
Accessibility trust and was wrong: the call does switch, and both reference implementations rely on it
for a cross-Space target. Which attempt moved the Space is published per raise, and
`scripts/e2e/space-desktops.sh` runs one pass with activation suppressed so the rescue is verified on
its own rather than inferred.

## Thumbnail card sizing

The overlay's thumbnail grid gives every card **one** height. The user pins it with the App Switcher
page's **Thumbnail size** dropdown (`layout.thumbnail_size`: `auto`, or a percent of the base card),
or `auto` picks it as the first step of `THUMB_SCALE_STEPS` whose **count-based estimate** fits
(`thumb_scale_for_panel` -> `thumb_count_estimate_fits`). A pinned step skips the search entirely, so
the panel wraps and scrolls instead of shrinking the cards.

`auto`'s estimate is count-only by construction: it uses the count, the panel budget and a
**reference card at the base preview ratio** -- never the sequence, the windows' shapes or the
set-wide width cap. Every card in a base-ratio set is exactly that wide, so any row that fits holds at
least `floor((max_inner + gap) / (reference_w + gap))` cards, which makes `ceil(count / per_row)` an
upper bound on the rows the packings can produce -- and `overflowed` is exactly `rows > max_rows`. A
step that passes therefore never overflows a base-ratio set. Wider windows are not covered by that
bound: a set much wider than the base ratio can need one row more and scrolls at the chosen size,
which is the price of keeping the automatic step independent of window shapes. A pinned percent fixes
the size only -- `thumb_widths_with_max_card_w` still derives each card's width from its own window and
re-packs, so the wrapping follows the windows' count, shapes and order.

Three facts still make the automatic step a coarse function of the window set:

- The row budget `thumb_max_rows` is derived from the card height and the usable panel height, so it
  changes with the step: taller cards get fewer rows.
- The number of cards per row is a step function of the card width, which follows each window's
  aspect ratio. Crossing a "one more per row" threshold changes the row count for the whole set.
- The per-set width cap comes from `thumbnail_max_card_width`: the widest window that counts as
  maximized (>= 90% of the screen width and >= 80% of the usable height) sets it for every card.

The packing itself still depends on the **order** of the cards -- `pack_rows` preserves MRU order and
then minimises leftover width, so the same multiset of aspects can need three rows in one order and
four in another -- which is exactly why `auto` no longer lays the packing out to choose its step. With
a pinned percent the order only changes which rows the cards land in, never their size.

Observed 2026-10-07 (reproduce with `grep "layout mode=thumbnail" ~/Library/Logs/oh-my-tab/oh-my-tab.log`):
adding one window moved the chosen step from 1.10 to 0.85 and the card height from 248 to 201,
because 1.10 through 0.90 all need four rows while the height budget allows three, and only 0.85 is
narrow enough for five cards per row. The same day, with the same 11 windows, summoning from a
maximized window chose 1.10 (248pt) and summoning from a non-maximized one chose 1.00 (230pt): at
1.10 the same multiset packed into three rows in one MRU order and four in the other. The recorded trade-off is "fit without scrolling wins over a
stable size"; the alternative -- accept at most one overflowing teaser row before stepping down, and
stop the width cap from following a single maximized window -- is tracked in `docs/review-backlog.md`.

## Logging and memory diagnostics

The default log path is `~/Library/Logs/oh-my-tab/oh-my-tab.log`. Once the active file reaches 10 MB, it rolls through `oh-my-tab.log.1` to `oh-my-tab.log.5`. Each launch writes a session marker; legacy per-launch logs and backups older than 30 days are cleaned up at startup.

Log entries are stamped with the local wall clock in ISO-8601 form with milliseconds and the UTC offset (`2026-10-09T10:37:23.286+08:00`, and `...Z` in a UTC zone; reproduce with `tail -1 ~/Library/Logs/oh-my-tab/oh-my-tab.log`). Local time is what the user reading their own log expects; the offset is what puts two entries on one timeline when they come from different zones, or when a DST fall-back hour occurs twice inside a single file. The offset comes from `tm_gmtoff`, so a zone that is not a whole hour away from UTC keeps its minutes, and it is stamped per entry rather than once per session because the zone can change — or shift under DST — inside one run.

After roughly 60 seconds, the app writes its first `[mem]` sample, followed by one every 5 minutes. The log includes the active feature profile, process footprint/RSS, sampled footprint peak, thread count, and estimated thumbnail, clipboard, and window ledgers. `footprint` is macOS's physical-footprint metric and corresponds to the Activity Monitor Memory column; it is the primary number for assessing memory pressure. `rss` is current resident memory and may fall as macOS compresses or reclaims pages. `footprint_peak_sampled` is sampled by the app, while `rss_peak_kernel` is the kernel's process-lifetime high-water mark.

The clipboard ledger separates text, preview, and metadata estimates; original image bytes in the disk cache are not counted as resident memory. Logs contain no clipboard contents or window imagery. Debug logs record only `Tab`, `Command`, `Option`, and the summon-combination name from the switcher's key tap. All other keys are logged as `Other`, without keycodes or modifier details.

## Device identification details

To identify a mouse or trackpad, the app checks whether the device conforms to Generic Desktop Pointer (1,1), Mouse (1,2), or Trackpad (1,5) usages. It calls the public `IOHIDServiceClientConformsTo` API against the complete `DeviceUsagePairs` instead of relying on a single `PrimaryUsage` value.

This matters because some mice report an incorrect primary usage. For example, **ATK A9 SE** (a Nearlink mouse) reports `PrimaryUsage = 6 (Keyboard)` and appears as a keyboard in System Settings, while its `DeviceUsagePairs` also declares Mouse (1,2). Checking the complete usage pairs recognizes the device as a mouse; relying on `PrimaryUsage` alone would send its events to the “recently used” profile.

Bluetooth keyboards are excluded even when their HID descriptors advertise pointer usages; Kzzi-i75, for example, declares a complete Mouse collection. The device picker cross-checks the Bluetooth **GAP Appearance** value (`0x03C1` = keyboard) against the cache written by `bluetoothd` to NVRAM, matching entries by Bluetooth address. macOS uses the same source for the Bluetooth panel icon. Devices that are absent from the cache, such as newly paired devices, and non-Bluetooth devices fall back to the HID-only check.

The device picker refreshes when devices connect or disconnect. These events are debounced, and delayed rechecks cover short BLE sleep and wake cycles.

## Clipboard recording controls (pause, excluded applications)

Two controls sit in front of every write path:

- **Pause** (`clipboard.paused`, persisted) stops adding entries without turning the feature off. The
  effective state is the runtime gate in `src/clipboard.rs` (`CollectionGate`), not the config field, and the
  gate has three positions: `Active`, `Paused`, and `Baselining` (closed while a resume adopts its
  baseline). A paused poll still advances `LAST_CHANGE_COUNT`, so content copied during the pause can never be
  recorded afterwards; a resume re-reads the changeCount as a baseline WITHOUT reading the pasteboard, keeps
  the gate closed until that succeeds, and leaves the intent -- config, menu title, settings switch, panel
  glyph -- in the paused state when it fails. `paused` outranks `enabled`: pause -> disable -> enable still
  records nothing, and a single config change that both enables and unpauses goes through the same baseline
  path. An ordinary startup or enable keeps the documented record-current-pasteboard behaviour.
- **Excluded applications** (`clipboard.excluded_apps`, a list of bundle identifiers) are matched in the poll
  BEFORE the payload is read, so an excluded copy is never hashed, cached, drawn or icon-extracted. Two
  signals are OR-ed: the frontmost application's bundle identifier, and the copier named by
  `org.nspasteboard.source` when the pasteboard carries a well-formed identifier. The declared identifier is
  self-reported by the writer, so it is only ever an ADDITIONAL signal -- it cannot override the frontmost
  verdict. An empty identifier, a malformed one, or one over 255 bytes is ignored rather than allowed to
  disable the frontmost check. The built-in list is short on purpose (only identifiers with a documented
  source); a config written before the field existed receives it, an explicitly emptied list stays empty, and
  later versions do NOT append new defaults to a saved list.

`--e2e-state` reports `clipboard_privacy` (`paused`, `gate`, `excluded_count`, `excluded_apps`) so a scenario
can assert the controls without reading them off the UI; `gate` is the three-state `CollectionGate` and is the
direct evidence that a resume recorded nothing; the history itself is still the evidence for "nothing was
recorded".

## Clipboard history storage (encryption, states, migration)

While the clipboard feature is on, every byte it persists is sealed: the history TOML, the original image bytes, the thumbnail preview and the detail preview all go through the envelope in `src/clipboard/crypto.rs` (AES-256-GCM; `magic | kind | key_id | object_id | nonce | ct+tag`, with the AAD binding magic, kind, key_id and object_id). The content format inside the envelope is unchanged, so the TOML schema and `HISTORY_VERSION` still describe the history itself. The reader recomputes `object_id` from the logical file name it asked for and rejects a file whose header disagrees, so two complete same-kind files cannot be swapped.

The master key is one login-keychain generic-password item (`com.eacryo.oh-my-tab.clipboard-history` / `master-key`, value `key_id || key`), created with `SecItemAdd` semantics — never updated, so a second build channel (the `.dev` bundle) cannot overwrite the release key. Dev and release share the item on purpose, because they share the history file; a rebuild with an unchanged Info.plist and the same identity reads it silently in a probe, while a different bundle identifier prompts. The development build used to be re-signed on every launch (a fresh timestamp build number inside Info.plist) and asked for the keychain password every time; **located and fixed on 2026-10-08**: an item's ACL is judged against the signing identity of the app that CREATED it, and this item was created by an ad-hoc build. The dev bundle now signs with Developer ID and the item was reset once, after which a rebuilt dev bundle reads it silently (67 ms, measured). See "Keychain authorization" below for the full rule and its unverified edges. Every `--smoke-*` launch is already isolated from the keychain; use `--clip-no-keychain` for other development runs that do not need it.

`src/clipboard/storage.rs` owns the session state, and every write, sweep and delete asks it first:

- `Ready` — key available, history loaded; reads, writes and orphan sweeps are allowed.
- `Unavailable(reason)` — no key this session (refused, cancelled, locked, or `--clip-no-keychain`). The feature is **unavailable**: nothing is recorded, written, swept or deleted (user decision 2026-10-08, replacing the earlier half-working "memory only" state); entering it discards whatever this session recorded before the load decided, along with its pending bytes. The settings banner and the panel's empty state carry the same sentence, with a "Grant keychain access" action that re-reads the key (one system prompt).
- `Blocked(reason)` — data is present but unreadable (`damaged`, `foreign-key`, `version`, `migration`) or a purge is unfinished (`purge-pending`): nothing is written, swept or deleted, and the key is never replaced. **The action follows the state** (2026-10-12): only `Unavailable` offers "Grant keychain access"; `migration`/`damaged`/`foreign-key`/`version` offer "Clear and Restart" (the same purge transaction the switch-off path runs), because re-reading the key only re-derives the same `Blocked`; `purge-pending` offers nothing (it retries at launch and on every config change). The About page's row reports the state (a `Blocked` state shows "History unreadable" / "Cleanup unfinished" with no keychain button; `Uninitialized` shows "Key not read yet"). The `migration`/`damaged` sentence carries "restarting the app retries it"; `foreign-key`/`version` do not.

Invariants worth keeping: a key is only created when no encrypted data exists (or after a purge); sweeps only run after a successful load; failure paths isolate (rename aside) instead of deleting; a missing key with encrypted data moves the whole storage set aside (`*.failed-<ts>`) before starting fresh.

The plaintext → encrypted migration is resumable and commits atomically: every referenced cache file is converted first (a sealed file is only accepted when it authenticates under the current key with the expected kind and object id), then the encrypted index is published, and only then is the plaintext index removed. Any file that cannot be converted blocks the commit (`migration`), leaving the plaintext index and every original untouched for the next attempt.

Deletion is a transaction (`purge-pending`): the intent is written first (next to the history file, falling back to `~/Library/Application Support/oh-my-tab/` — never `~/Library/Caches`, which the system may clear), then in-flight work is invalidated, then the files are deleted and verified, then the intent is revoked. Only a verified deletion plus a verified revocation restores writing; a stale marker keeps writes off, so it cannot delete freshly written history. Pending purges are processed at launch and on every config change, before anything loads or writes. The delete scope covers the app's own kept-aside copies (`*.failed-<ts>[-n]`) and the isolation marker `clipboard-history.isolating` (copies are matched strictly against the format `rename_aside` writes, derived from the live paths, and symlinks are never followed). The transaction ends with a **completion boundary**: the generation is advanced and the final state committed in ONE `SESSION` critical section (`Done` ⇒ `Uninitialized`, otherwise `Blocked(PurgePending)`), which invalidates a load result captured during the transaction — without it, a load that captured the generation after the purge bumped it but read the index before the delete would re-apply records the user asked to delete. The transaction runs **synchronously on the calling thread** (switch-off, `clear_on_quit` and "Clear and Restart" alike), so a click that lands while a migration or an image write holds a lock waits for it; the user explicitly accepted that worst case on 2026-10-12 (measured with a temporary instrument, numbers in the task's review archive), and the constraints for ever moving it to a background executor are in `docs/clipboard-encryption-plan.md` §4.7.

Tests and smoke runs never touch the real keychain or the real pasteboard: `cfg(test)` and `--smoke-*` use a fixed test key with the temporary storage directories (every `--smoke-*` launch enables that isolated session at the top of `run`, before the config or the storage is read), and a smoke launch installs no pasteboard observer and no polling timer, so it records nothing the user has copied -- their history is encrypted with the test key published in the source, which would make such a record readable. `--clip-no-keychain` forces the `Unavailable` path so the unavailable branch stays reachable. `--e2e-state` reports `clipboard_storage` (`state` / `reason` / `key_provider` / `plaintext_leftover` / `keychain_acl`) and `clipboard_notice` (the action each surface ACTUALLY applied: `settings_banner` / `picker_empty_state` / `about_row` are `grant` / `clear` / `hidden` / `absent` / `unset`, plus `about_status`) — `state`/`reason` say what the state machine decided, but cannot tell "the button is hidden" from "the surface does not exist", nor the state's action from the one the UI applied; a scenario that asserts encryption must still read the file itself, because a self-report is not evidence.
Two operational notes.

**The off-main load and its cost.** The load runs off the main thread because a keychain read can block (measured in a development build: the read itself took ~11s, the rest of the decision 0.2s — the app stayed fully responsive, which is the point of the split); `clipboard_storage.state` therefore reads `uninitialized` until that load lands, and a scenario asserting it must wait and trigger a fresh snapshot.

**Keychain authorization** (measured, inferred and unverified must stay apart): **located and fixed on 2026-10-08**. The rule (three C probes plus the real app): **an item's ACL is judged against the signing identity of the app that CREATED it**. An item created by a self-signed or ad-hoc build stops being recognized as soon as that binary changes (probe: rebuild and re-sign the same path -> the next read prompts) -- the historical behaviour of this machine, no longer current: after the one-time reset on 2026-10-08 the item is Developer ID-created and a rebuild read it in 67 ms with no prompt. An item created by a Developer ID-signed build survives the same rebuild (probe: silent; real app: silent). **Do not read stability off the ACL's textual form**: macOS reports a path for every non-Apple-anchored app through the deprecated reading API, so `--e2e-state`'s `keychain_acl` (`requirement` / `foreign-requirement` / `path` / `none` / `unknown`) is a form report. **An app cannot adopt an item it did not create**: a Developer ID-signed app deleting one created by a different signature gets `errSecInvalidOwnerEdit` (-25244, measured), so a legacy item has to be deleted once by the user and re-created by the app (the old storage set is kept aside as `*.failed-<ts>`). **Unverified**: a release (Developer ID) install in a legacy environment that was never reset -- "prompts at most once" is an expectation, not a guarantee. Every `--smoke-*` launch is already isolated (test key, temporary directories), so an A1 smoke never reads the keychain and never prompts; `--clip-no-keychain` remains for other development or UI verification runs that do not need it.



## Global key bindings

Window control and the quick actions are one feature: one ordered binding list, one action registry,
one event engine, one master switch. `[key_bindings]` holds `enabled` and `[[key_bindings.items]]`
holds `action` / `trigger` / `enabled`.

- **One engine.** `key_bindings::engine` owns the only session-level tap for bindings and resolves a
  press against an immutable `Arc<BindingTable>` snapshot. The tap never reads the config: liveness
  is decided once, when the table is built (master switch AND row switch), so no second place can
  keep a binding alive or kill it. `Action::group` survives as a classification for the picker and
  for "does this act on the frontmost window", no longer as a gate. Adding an action means adding a variant in
  `key_bindings::action`, an `ALL` entry, a config id, a label key in every locale, and an
  `execute` arm; nothing else enumerates actions.
- **Press pairing.** A press the engine swallows is remembered by its physical keycode until the
  matching release, so releasing Option before the main key still pairs, and a repeat or release the
  engine never took is passed through instead of firing an action. Our own synthesized input (mouse
  key mappings, the lock-screen chord, Finder's Cmd+N) carries `SYNTHETIC_MARKER` and is filtered
  before the record is touched — without that, a binding on the lock-screen chord would lock again
  on every pass.
- **Reserved chords.** The app switcher (`keyboard.modifier` + Tab, extra modifiers allowed) and the
  clipboard shortcut own their chords; both are computed from the effective config, never from a
  hardcoded list. `Config::settle_reserved_shortcuts` is the one place that resolves a collision, and
  it runs both at load (after per-field fallback) and at runtime before anything is published: the
  clipboard yields first (it can be rebound, the switcher cannot), then the bindings yield to what
  the switcher and the clipboard finally own. A clipboard that already holds its default still
  collides, so the binding is the side that gives way. The settled config is what gets persisted,
  cached (`event_monitor`'s chord), painted and published, so no later `publish(new)` can disagree
  with it. Both settings UIs refuse to create such a pair, and chords with no Command/Option/Control
  are refused: a binding is swallowed everywhere.
- **Recording and the add flow.** The settings recorder keeps the engine suspended from the start of
  a recording until the captured chord has actually been released (`RecStage::WaitingRelease`, with
  the captured key's repeats and release swallowed, decided by the pure `release_wait_step`); the
  engine passes every event through while `RECORDING` is set. A recorded chord becomes a *draft* in
  the add row, which asks for confirmation before writing, and a cancelled recording, a closed
  window, a revoked permission or a page switch clears it.
- **Execution rules** live in `key_bindings::execute`, the only main-thread entry: the settings
  recorder wins (no binding runs while a chord is being recorded), an in-flight switcher session is
  cancelled first (its modifier release would otherwise commit the card selected before this
  action), and a window action never acts on this app's own windows (fixed-layout panels; it no-ops
  rather than quietly acting on a background window). Bindings always fire otherwise — including
  while oh-my-tab is frontmost.
- **Migration is one-way.** The pre-binding `[window_control]` and `[quick_actions]` sections are
  still read and seeded into the list (row switch from the matching flag, never ANDed at load), then
  never written back: the first save drops them, so a downgrade loses the binding list. The master
  switch is OR-ed from everything the older files could have stated — the sections' `enabled` flags
  and the retired `key_bindings.window_control_enabled` / `quick_actions_enabled` keys an interim
  build wrote — so a user who had either group on keeps it; a `key_bindings.enabled` the file states
  itself always wins. Because those two switches now share one master switch, a group that was **off**
  is migrated with its rows seeded **off**: rows enabled while their group was off would come alive
  with the master switch the other group holds, arming interceptions the user had turned off (the
  window-direction rows override Option+arrows in text fields). A group that is on keeps its per-row
  flags, so switching the master switch off and on again restores exactly the bindings the user had.
  The same fold applies to an interim file that already carries its own `items`: its disabled group's
  rows are switched off in place, preserving their order and triggers, because that file was written
  while the two switches were still separate. A `key_bindings.enabled` the file states itself is the
  user's own merged decision, so it folds nothing into either list. When both groups were off the rows
  keep their flags — the feature is simply not enabled yet. A missing list seeds the factory entries;
  `items = []` means the user deleted every binding and is respected; a mistyped entry is dropped on
  its own, keeping its siblings.
- **One page, one switch.** The settings page is a single card: the master switch, then every binding
  in config order (window control and quick actions are no longer shown as separate cards), and a
  footer band carrying one full-width "Add binding" button. Turning the master switch off freezes the
  rows, the footer button and the panel rather than hiding them, so the user can see what is installed
  before enabling it. The status-bar menu carries one Key Bindings item over the master switch: an item
  can only express one state, so two group items could not be merged into it.
- **Every row owns its actions.** A binding is edited and deleted from the row itself: "Edit" opens the
  panel prefilled with that binding, "Delete" removes it **without a confirmation step** (the user's
  explicit choice). There is no page-level selection any more -- the +/− segmented control and the
  `selection_bg` wash behind a chosen row are gone -- because each row's buttons carry the item index
  they act on in the address-keyed registry. That registry covers the *click*: a rebuild unregisters
  every old address and re-registers the new ones, so a button cannot fire against a binding the list
  no longer holds. It does **not** make the index safe to *write* against, because a click is not the
  only way the list changes: a config reload can remove an entry while the panel is open, and an index
  that still resolves then points at a different binding -- editing `B` in `[A, B, C]`, having `B`
  dropped externally and reloading to `[A, C]` would overwrite `C`. The write path therefore compares
  **identity**, not position: `EditTarget` captures the action and chord the panel was opened on, and
  `resolve_edit_target` refuses the write with `errors.key_binding_edit_stale` unless that same binding
  is still at that index. Delete re-reads the config length for the same reason. An index thus never
  has to be invalidated the way the old selection did, and `key_bindings_selected` is published as
  `null` -- kept only because the A2 schema requires the field to be present, with no promise about its
  type; an assertion on it must expect `null`.
- **Edit and add share one panel.** The panel is the same surface in both modes: `BINDING_EDITING` holds
  the item being edited or `None` to append, and it decides only what confirm writes and what the
  fields start on. Prefill goes through pure helpers (`picker_index_for`, `chord_text_for`) so the rules
  are testable without a window. Two details matter: the entry being edited is **excluded from its own
  duplicate check** (recording a binding's own chord is the commonest edit, and counting that row
  against itself would refuse it), and a write whose index no longer exists is refused with its own
  message rather than overwriting whichever binding moved into that slot.
- **Adding happens in a panel.** ＋ opens a floating `NSPanel` (the model the Mouse page's mapping
  panel established) holding the action popup, the chord row and Cancel/OK; the page itself stays a
  plain list. The panel owns the draft until OK, so nothing is written before the user agrees. Two
  rules keep the panel honest: changing the action drops any chord recorded for the previous action,
  and OK re-reads the popup before writing, so a stale draft can never be confirmed against a
  different action. Leaving the page closes the panel and cancels the recording; so does closing
  settings. The panel is created `alloc`/`init` and `setReleasedWhenClosed: false`, and
  `close_binding_panel` clears every stored view slot *before* releasing it — the panel deallocates
  its content view's subtree, so a slot left filled would dangle.
- **e2e state.** `--e2e-state` publishes `key_bindings_entries`, `key_bindings_live` and
  `key_bindings_enabled`, so a scenario can assert the migration and that nothing is live while the
  master switch is off. It also still publishes `key_bindings_selected`, always `null` now, because
  the A2 schema requires the field to be present and makes no promise about its type.

## Switcher panel action keys (Cmd+W / M / H / Q)

Each of the four in-panel action keys is gated by its own switch under the App Switcher page
(`windows.panel_close_enabled` / `_minimize_enabled` / `_hide_enabled` / `_quit_enabled`, all off by
default: quitting an app is destructive and hiding one changes what the user is looking at).

- **Routing.** Command-modified key equivalents never reach `keyDown:`; AppKit offers them to
  `performKeyEquivalent:` first, so the overlay overrides it on its window (`src/overlay/panel_keys.rs`),
  the same mechanism the settings window uses for its own Cmd+Q.
- **Ownership, not outcome, decides consumption.** Once `panel_actions` has recorded a press as
  claimed, the handler consumes it even when the action can no longer run (the panel went invisible
  between the tap's pass-through and the main-thread call). Returning false there would hand a
  panel-claimed Cmd+Q to the app's own Quit menu item and quit oh-my-tab -- the failure the design
  round named -- so the action is cancelled instead.
- **The summon becomes sticky.** Pressing an action key keeps the panel open and stops a later release
  of the trigger modifier from committing a card; the user commits with Return or a click and dismisses
  with Esc. Every other exit clears the sticky state, so nothing outlives its summon.
- **Hiding or quitting moves the frontmost app**, which makes the non-activating panel resign key; the
  ordinary resign path would treat that as a click-away and close the panel. A time-boxed,
  consume-once guard (`ACTION_FOCUS_WINDOW`, 750 ms) suppresses only the resign the action caused.
- **Keys follow the layout.** The physical key for each action is resolved from the live input source
  on the main thread (`keycode_for_character`), with that action's ANSI position as a per-action
  fallback, so one unresolvable key never disables the other three.

## Settings backup (export and import)

"Export settings" writes the live config as ordinary TOML under a small comment header
(`# oh-my-tab settings export`, `# format = N`); the body is therefore also a valid `config.toml`.
"Import settings" parses it with the app's own loader rather than deserializing it directly, which is
what makes both directions survive a version difference: a field this build does not know is ignored,
one it knows but rejects falls back on its own, and an older file's legacy `[quick_actions]` /
`[window_control]` sections still seed the binding list. The header's format version guards the *file
protocol* only -- it rises when the meaning of the file changes, not when the app gains settings.

Two rules keep an import from doing damage. A file without the marker is refused, so a file that merely
happens to be valid TOML cannot import as "everything at its default" and wipe the user's settings; and
`restore_local_fields` keeps this machine's own values for the login item, the absolute log path, the
keystroke panel's saved position, and the clipboard storage decisions (`enabled`, `max_entries`,
`auto_expire_days`, `clear_on_quit`) -- importing `clipboard.enabled = false` over a machine that has it
on would otherwise run the feature's disable path and delete the stored history. A re-export is
lossless for what this build understands, not for the bytes: unknown fields have nowhere to live in the
live `Config`.

## Menu-bar icon and reopening

"Hide menu bar icon" (`startup.hide_menu_bar_icon`) calls `NSStatusItem.isVisible = NO` on the live
item instead of destroying it, so menu-title refreshes, the clipboard pin's check state and the main
menu's key equivalents keep working while the icon is hidden. The switch travels with export/import
(it is a display preference, not a per-machine registration, unlike launch-at-login).

With the icon hidden the app must not become unreachable: the switcher's footer gear and the clipboard
panel's gear open the relevant settings page, and activating the app again (Finder, Spotlight,
`open -a`) is handled by `applicationShouldHandleReopen:hasVisibleWindows:`, which opens Settings
with that callback returning NO so AppKit does not also perform its generic reopen. Launching the
binary path directly is not that path: a second process is refused by the single-instance lock and
exits, so the running instance never sees it.

## Finder context menu (Finder Sync extension)

The right-click menu is the only feature with a **second process in its own bundle**. Finder Sync
extensions must run sandboxed and Finder only draws them from a plug-in it discovered, so the
extension can neither write a file nor launch an application. The split is therefore not a style
choice: the app builds the menu, the extension draws it, and every click comes back to the app.

**Message path.** The app pushes a `MenuModel` (titles already localized, icons as SF Symbol names,
each leaf carrying its action) over `<host bundle id>.finder.app2ext`. The extension caches the last
model, re-renders it per right-click, and reports a click over `<host bundle id>.finder.ext2app`.
Both directions carry the host bundle id and the app's launch instance; anything from another channel
or an earlier app process is dropped. A message larger than 64 KiB is refused before parsing.

**Why a click is not trusted.** A distributed notification can be posted by any local process, and
a menu can stay open across a settings change. The extension sends the action the user *clicked*
(from the item's `representedObject`), and the app checks it against the live config (feature on,
file type still offered, terminal still enabled) before doing anything. That is the security story
of this feature: the action set is deliberately non-destructive (create an empty file in a folder
the user right-clicked, open a configured terminal, write the clipboard), and the app re-validates
rather than trusting the message.

**Staleness / offline.** The extension syncs every 10 s. If no app message has arrived for 45 s the
model is treated as stale and no items are offered — an idle app is not mistaken for a dead one
because the sync itself keeps the timestamp fresh, and a crash expires the menu instead of leaving
clickable items behind. On an orderly quit the app sends `quit` and the extension clears at once.

**Localization.** All menu titles are localized in the app and shipped as strings, so the extension
has no `.strings`/`.xcstrings` of its own and the three locales cannot drift between the two
processes.

**Signing.** The extension needs `com.apple.security.app-sandbox`; the app does not. Sign
**inside-out**: the extension first, the app last, and the app **without** `--deep`. `codesign --deep`
on the app re-signs nested code and drops that entitlement (measured: the appex comes out with no
entitlements, and Finder then refuses to load it), and it seals the nested bundle by cdhash, so
re-signing the extension afterwards makes the parent invalid ("nested code is modified or invalid").
Inside-out leaves the parent recording the extension by requirement, so `--verify --deep --strict`
passes. `scripts/release.sh` additionally proves the staged appex still carries the entitlement and
shares the app's team.

**Election after an update.** Replacing the app makes macOS disable the extension (the system keeps
the user's election for the plugin instance it discovered, not for the new bundle). The main app
restores it once, through `pluginkit -e use`, but only when it can prove the election belonged to the
user before *our own* update: the SPUUpdaterDelegate install hooks (`willInstallUpdate:`,
`willInstallUpdateOnQuit:`) record a transaction with the appcast item's CFBundleVersion as the
expected target build, the election observed then, and the feature switch; the first launch whose
build equals that target restores it and records the attempt. A cancelled installation (the
install-phase Skip button, `SUInstallationCanceledError`) revokes the transaction; Later/Dismiss keeps
it (the update may still install at quit). Manual replacement, a different build and a missing
transaction are deliberately not restored: the user's post-quit changes are indistinguishable from an
update's, so the app only restores what it can attribute to its own update. The dev channel writes
the same record from `scripts/dev-restart.sh` before it deletes the bundle, sharing the bytes with the
Rust parser through `testdata/finder-ext-transaction.toml`. `!` (the debug election) and unknown
states are left alone. The settings row says "Disabled after the update" when the attempt failed, so
the user still knows to act.

**Rebuilds.** Finder caches the extension process. `scripts/dev-restart.sh` registers the new appex
with `pluginkit`, stops only the process whose executable is *this* worktree's appex, and prints the
election state; restarting Finder (which makes a rebuilt menu appear reliably, but closes every
window) is opt-in as `--reload-finder-extension`. A dev and a release extension enabled at the same
time put two copies of the menu in Finder; the script reports that rather than changing the user's
choice.

**A registered extension is not a working one (measured 2026-10-23).** Finder does not run the
appex itself: it asks launchd for a per-request service instance named
`gui/<uid>/<appex bundle id>.<request uuid>`, and launchd caches that instance's executable path.
A worktree that ran the extension once leaves such an instance pointing at *its* appex; delete the
worktree and every later request re-bootstraps the same bundle id, gets "will use existing" with the
dead path, then `Missing executable detected`, and the spawn fails. PlugInKit still reports the
extension as elected (`+`), System Settings still shows the switch on, and the menu is silently
empty — nothing but the unified log says otherwise. `pluginkit -a` cannot repair that: it adds a
record, it does not replace a pinned instance.

`scripts/lib/finder-extension.sh` (sourced by `dev-restart.sh`, self-tested by
`scripts/finder-extension-selftest.sh`) removes a per-request instance ***only* when the program path
launchd has pinned for it no longer exists on disk, launchd does not report a live process for it, and
its state is not `running`** (the self-test narrows removal to the labels it created through an optional
allowlist, so the production rule can be exercised without touching another app's services) — the one state Finder cannot recover from, and
the shape the incident had. An instance whose program still exists is left running, whoever owns it.
What the script does do, after registering the freshly signed appex, is stop the extension process
of *its own* appex (Finder may have started the previous build while cargo ran). Finder is restarted
only when that appex then has no extension process at all — a restart closes every Finder window —
or when `--reload-finder-extension` asks for one. Two things are never touched: the host app's own
launchd instance (`application.<host bundle id>.<pid>.<pid>`; booting it out leaves the app running
with unlinked log fds, and `--whose-instance` then reports UNVERIFIED) and an extension process of a
different appex (the packaged install, another worktree). The script reports whether the extension
process is up instead of letting "elected" stand for "works".

**Terminal adapters.** Terminals differ in how they accept a directory: a running Terminal.app or
iTerm2 opens a folder URL, while Ghostty/WezTerm/kitty/Alacritty need a fresh instance with their own
argument. `finder_menu/terminals.rs` owns one adapter per known bundle id; `open_arguments` is pure and
unit-tested, and the settings page only offers terminals LaunchServices reports as installed.

## Building and testing

`scripts/dev-restart.sh` is the normal way to run the app during development. It builds the development `.app` and re-assembles and re-signs it only when its inputs change (the built binary, the bundled resources, the signing identity), using the Apple-issued identity when one is installed (`CODESIGN_IDENTITY` overrides the discovery; signing is done without a secure timestamp so a local build never depends on Apple's timestamp service), then launches it through the per-user `launchd` domain. **Only two identities are accepted -- the Apple-issued one and the self-signed one; ad-hoc is refused**, and a failure to sign with the Apple identity fails the build unless `--allow-signing-fallback` asks for the self-signed identity on purpose. A stable signing identity is what keeps the Accessibility, Screen Recording and keychain grants alive across rebuilds; when nothing changed the existing bundle is reused, so a restart does not reset them.

**A launch is verified from the running process, never from the plist on disk.** The script waits for the app's own `[launch] pid=… executable=… version=… bundle=…` record -- written by the process itself, so it can only describe the process that wrote it -- and fails when the pid, the executable path or the version disagree, or when the process exited right after writing it. When the log window is unusable (rotated, truncated, unreadable) the run reports `build timestamp: unverified` instead of passing; that is a report, not a pass. Report the `build-version` the script prints, which is the one the process reported. `--check` verifies such a launch and then stops it, leaving neither the process nor its launchd job behind.

**One build/test/launch transaction at a time, across worktrees.** Every worktree shares one bundle id, one launchd label and one app log -- that is what keeps the TCC grants and the keychain ACL valid across rebuilds -- so two concurrent runs fight over one instance. The script takes a `lockf` claim in the git common directory before it stops anything and waits up to `--lock-wait=<sec>` (default 900) before failing with exit 75. The claim covers the transaction only: the app the script leaves running is not protected, and the next run may replace it. `scripts/dev-lock-selftest.sh` checks the claim offline, including the cross-worktree case.

The unit-test suite is headless-safe by default; clipboard image and history fixtures use isolated temporary directories per process and thread. Run it through `scripts/cargo-test.sh`: a `cargo test` filter that matches nothing exits 0 while printing `running 0 tests ... ok`, so a typo in a filter reads as a pass — the wrapper counts the executed tests and fails the run when the count is zero. It forwards every argument to `cargo test`, so a filter keeps the guard.

The CG/AX **smoke tests** are marked `#[ignore]` and need a GUI session plus an Accessibility grant:

```sh
cargo test -- --ignored
```

On a macOS GUI session, the real AppKit settings smoke test can traverse every settings page and run the post-layout checks without manual clicking:

```sh
cargo build
cargo test settings_layout_smoke -- --ignored
```

It runs the debug binary with `--smoke-settings-layout`, opens the actual settings window on the main thread, visits all eight pages, validates their descendant view frames, and exits.

The Finder Menu settings page has its own smoke (`cargo test finder_menu_page_smoke -- --ignored`, or
`./target/debug/oh-my-tab --smoke-finder-menu`): it opens the real window on that page and asserts the
page's own contract against the live view tree. It is separate from the all-pages layout smoke because
that runner currently stops on an earlier clipboard-page assertion (see `docs/review-backlog.md`).

The overlay runtime path also has an ignored child-process smoke test. Run `cargo build` first, then `cargo test overlay_runtime_smoke -- --ignored`; its `--smoke-overlay` switch bypasses the single-instance guard so an already-running development app cannot turn the test into a false pass.

The **A2 scenarios** live in `scripts/e2e/*.sh` and run through `scripts/e2e/run-all.sh`, which discovers every executable script in that directory and runs the selected ones in order. Each scenario declares the disturbances it causes in its header, one exact line per kind, and each kind has its own switch: `# E2E_STEALS_FOCUS=1` (injects a real global hotkey through the system event stream, or activates the app) needs `--include-focus`; `# E2E_COVERS_SCREEN=1` (pins a full-screen backdrop over the desktop, or captures screen content) needs `--include-screen`; `# E2E_CHANGES_PREFS=1` (touches a system preference or the user's config) needs `--include-prefs`. The gates are independent and stack -- a scenario runs only when every declaration it makes has been authorised, because agreeing to switch the frontmost app is not agreeing to have the desktop covered or a system setting changed. A marker is an exact header line: one with a trailing comment is not a marker. `--list` prints what this run would execute. The app writes the facts a scenario cannot read back through Accessibility — `--e2e-state=<path>` JSON snapshots — and the scripts assert on those plus real WindowServer state; a scenario that asserts encryption must still read the stored file itself, because a self-report is not evidence.

### Localization and layout QA

The Debug app built by `scripts/dev-restart.sh` adds a `[TEST] English x3` option to the language selector. Selecting it repeats every English UI string three times, so long dropdown values and their surrounding rows and cards can be checked in the real settings window. The optimized development package built by `scripts/release-dev.sh` includes the same fixture through the `dev-long-text` Cargo feature; the production release scripts do not enable it. The older `--pseudo-locale` switch is still available for debug-only punctuation-based expansion.

Launch a debug build with `--layout-debug` to enable runtime settings-page assertions; peer controls crossing labels or controls, and frames escaping the document, fail fast with the page name and offending frames. Internal views inside native controls are excluded from peer comparisons.
