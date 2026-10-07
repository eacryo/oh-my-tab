# Development Notes

These notes cover issues that affect source builds and debugger-launched binaries, not users of Homebrew installations or packaged `.app` builds. The normal development path is `scripts/dev-restart.sh`; a bare `cargo run` is reserved for low-level diagnostics. The README links here for development-only issues.

## Icons may be incorrect in development mode

When running the bare binary with `cargo run` for diagnostics, the overlay may occasionally show oh-my-tab's own card as an initial-letter placeholder instead of its application icon, and the problem may persist until the icon cache is cleared manually. The icon cache is keyed by bundle ID and uses the executable **mtime** as its invalidation fingerprint. Each development build relinks the binary and changes its mtime, invalidating the running instance's cache entry. Packaged `.app` builds are unaffected because the installed binary's mtime remains stable. For normal development runs, use `scripts/dev-restart.sh`; if the diagnostic binary shows this issue, use the *Clear Icon Cache* menu item or delete `~/Library/Caches/oh-my-tab-icons/`.

## Mouse control may fail when launched from a debugger

When the app is launched in Debug mode through RustRover or another debugger, frequent mouse activity during startup may cause scroll reversal and per-device mouse settings to stop working. In that state, the app stops receiving mouse events, scrolling returns to the system default, and pointer-acceleration changes no longer apply until the app restarts. This issue has been observed with unsigned development builds launched by a debugger and appears related to macOS 26 restrictions on HID-layer event monitoring. Packaged `.app` builds and binaries launched from a terminal have not shown the same behavior.

## Fullscreen Space grouping

The switcher treats fullscreen Spaces as part of their source desktop only after observing a matching WindowServer leave/join pair and confirming the target Space type and display. A fullscreen window already present at launch, an event gap, or ambiguous display/source evidence remains isolated to its actual Space. Space membership notifications are consumed in order. A Space switch that grouping handles correctly leaves the candidate set unchanged, so `--e2e-state` publishes a separate `refresh_context` frame when the active Space context changes; scripts must assert only on applied frames (`refresh` / `refresh_context`), since other frames pair the new context with the previous card list. Only a known event loss (queue overflow) clears pending transition evidence; a membership event whose owner cannot be resolved merely downgrades the diagnostic continuity flag, because a fullscreen transition emits such events for auxiliary windows of the same app and clearing on them destroyed the transition being learned. The pair window is 3s rather than 1s: the 1325 join can land while SkyLight still types the new Space ordinary, and the pair must survive until a later query corrects the type. Legacy membership fallback admits only positively onscreen windows and never treats fullscreen bounds as a cross-Space exemption. These private SkyLight event/query paths are best-effort and may be unavailable on future macOS releases.

### Windows on other desktops

By default the candidate scope above is the whole rule: a window on another macOS desktop is not a
candidate. `windows.show_other_desktops` widens admission to managed Spaces outside every display's
current group, and only through that switch: a window the switch admits carries no AX element
(`kAXWindows` is filtered by the current Space), so it keeps the CG window name as its title and has
no readable minimized state. Admission stays under AX authority. When an app's `kAXWindows` is empty but its key/main slots
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
cannot tell another desktop from an off-screen window. The minimized option still applies first.

Thumbnails: such a card never *requests* a capture (the WindowServer cannot capture a window off the
active desktop), but it does show a frame the cache already holds from when the window was on its own
desktop -- the producers filter it out while the renderer keeps reading the cache, and the A2 snapshot
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
