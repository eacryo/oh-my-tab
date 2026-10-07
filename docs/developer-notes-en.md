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

When running the bare binary with `cargo run` for diagnostics, the overlay may occasionally show oh-my-tab's own card as an initial-letter placeholder instead of its application icon, and the problem may persist until the icon cache is cleared manually. The icon cache is keyed by bundle ID and uses the executable **mtime** as its invalidation fingerprint. Each development build relinks the binary and changes its mtime, invalidating the running instance's cache entry. Packaged `.app` builds are unaffected because the installed binary's mtime remains stable. For normal development runs, use `scripts/dev-restart.sh`; if the diagnostic binary shows this issue, use the *Clear Icon Cache* menu item or delete `~/Library/Caches/oh-my-tab-icons/`.

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
machinery and left the destination without a menu bar when leaving a full-screen Space. This project
therefore tries app activation first and applies the front-switch as the rescue when the window has
not joined the active desktop within `OTHER_DESKTOP_ACTIVATION_BUDGET`; the wait is bounded by
`OTHER_DESKTOP_SETTLE_BUDGET` because the transition is animated, and a window that only arrives while
the AX phase is already running gets one late re-raise (`OTHER_DESKTOP_LATE_SETTLE_BUDGET`). An earlier
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

After roughly 60 seconds, the app writes its first `[mem]` sample, followed by one every 5 minutes. The log includes the active feature profile, process footprint/RSS, sampled footprint peak, thread count, and estimated thumbnail, clipboard, and window ledgers. `footprint` is macOS's physical-footprint metric and corresponds to the Activity Monitor Memory column; it is the primary number for assessing memory pressure. `rss` is current resident memory and may fall as macOS compresses or reclaims pages. `footprint_peak_sampled` is sampled by the app, while `rss_peak_kernel` is the kernel's process-lifetime high-water mark.

The clipboard ledger separates text, preview, and metadata estimates; original image bytes in the disk cache are not counted as resident memory. Logs contain no clipboard contents or window imagery. Debug logs record only `Tab`, `Command`, `Option`, and the summon-combination name from the switcher's key tap. All other keys are logged as `Other`, without keycodes or modifier details.

## Device identification details

To identify a mouse or trackpad, the app checks whether the device conforms to Generic Desktop Pointer (1,1), Mouse (1,2), or Trackpad (1,5) usages. It calls the public `IOHIDServiceClientConformsTo` API against the complete `DeviceUsagePairs` instead of relying on a single `PrimaryUsage` value.

This matters because some mice report an incorrect primary usage. For example, **ATK A9 SE** (a Nearlink mouse) reports `PrimaryUsage = 6 (Keyboard)` and appears as a keyboard in System Settings, while its `DeviceUsagePairs` also declares Mouse (1,2). Checking the complete usage pairs recognizes the device as a mouse; relying on `PrimaryUsage` alone would send its events to the “recently used” profile.

Bluetooth keyboards are excluded even when their HID descriptors advertise pointer usages; Kzzi-i75, for example, declares a complete Mouse collection. The device picker cross-checks the Bluetooth **GAP Appearance** value (`0x03C1` = keyboard) against the cache written by `bluetoothd` to NVRAM, matching entries by Bluetooth address. macOS uses the same source for the Bluetooth panel icon. Devices that are absent from the cache, such as newly paired devices, and non-Bluetooth devices fall back to the HID-only check.

The device picker refreshes when devices connect or disconnect. These events are debounced, and delayed rechecks cover short BLE sleep and wake cycles.

## Building and testing

`scripts/dev-restart.sh` is the normal way to run the app during development. It builds and assembles a separately signed development `.app`, then launches it through the per-user `launchd` domain. This keeps Accessibility and Screen Recording permissions associated with the development bundle and starts the binary produced by that build.

The unit-test suite is headless-safe by default; clipboard image and history fixtures use isolated temporary directories per process and thread. The CG/AX **smoke tests** are marked `#[ignore]` and need a GUI session plus an Accessibility grant:

```sh
cargo test -- --ignored
```

On a macOS GUI session, the real AppKit settings smoke test can traverse every settings page and run the post-layout checks without manual clicking:

```sh
cargo build
cargo test settings_layout_smoke -- --ignored
```

It runs the debug binary with `--smoke-settings-layout`, opens the actual settings window on the main thread, visits all seven pages, validates their descendant view frames, and exits.

The overlay runtime path also has an ignored child-process smoke test. Run `cargo build` first, then `cargo test overlay_runtime_smoke -- --ignored`; its `--smoke-overlay` switch bypasses the single-instance guard so an already-running development app cannot turn the test into a false pass.

### Localization and layout QA

The Debug app built by `scripts/dev-restart.sh` adds a `[TEST] English x3` option to the language selector. Selecting it repeats every English UI string three times, so long dropdown values and their surrounding rows and cards can be checked in the real settings window. The optimized development package built by `scripts/release-dev.sh` includes the same fixture through the `dev-long-text` Cargo feature; the production release scripts do not enable it. The older `--pseudo-locale` switch is still available for debug-only punctuation-based expansion.

Launch a debug build with `--layout-debug` to enable runtime settings-page assertions; peer controls crossing labels or controls, and frames escaping the document, fail fast with the page name and offending frames. Internal views inside native controls are excluded from peer comparisons.
