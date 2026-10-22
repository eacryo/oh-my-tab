# ARCHITECTURE.md

Module layout and ownership rules for oh-my-tab. Read this before adding a source file, moving a
responsibility between files, or wiring a new feature into an existing surface. It answers "which module
owns this?"; `docs/developer-notes-en.md` answers "how does that subsystem behave in detail?", and
`docs/design-style-en.md` is the normative style for anything the app draws.

The crate is Rust-only: AppKit, CoreGraphics, and ApplicationServices are reached through `objc2` FFI, there
is no Swift bridge and no Rust UI framework. `src/lib.rs` declares every top-level module; `src/main.rs` is a
thin bin that calls `run()`, and `run()` is the only place startup wiring, the single-instance lock, the smoke
process mode, and the run loop meet.

## Tree

Listed file by file rather than by glob, for the reason a glob states: it lets a new file join a directory
without anything naming it. When a file is added, renamed, or removed, change this tree in the same edit.

```text
src/
├── main.rs                      # thin bin: calls oh_my_tab::run()
├── lib.rs                       # module declarations + run(): startup wiring, instance lock, run loop
├── app_identity.rs              # bundle id / exec path / pid identity, shared by collector + icon cache
├── autostart.rs                 # launch-at-login through SMAppService
├── callback_guard.rs            # panic boundaries for Rust callbacks entered from C/ObjC
├── clipboard.rs                 # clipboard subsystem root: state, filter, history model, tests
│   └── clipboard/               # (see "Clipboard subsystem" below)
├── clipboard_highlight.rs       # clipboard text classification, code wrapping, display mapping
├── config.rs                    # Config: the TOML schema, per-field defaults, debounced persistence
├── dev_backdrop.rs              # dev-only controlled backdrops for A2 pixel instruments
├── dev_flags.rs                 # argv-only development switches (never the environment)
├── e2e_state.rs                 # --e2e-state JSON snapshot of internal state for tier A2
├── event_monitor.rs             # switcher shortcut matching (Cmd/Opt/Ctrl+Tab) over event_tap
├── event_tap.rs                 # shared CGEventTap leaf: types, FFI, semantic constants, start helper
├── ffi.rs                       # CF/CG declarations, pointer wrappers, NSString, color/layer helpers
├── glass.rs                     # shared rounded material backdrop for passive floating panels
├── hash.rs                      # the one FNV-1a 64 implementation
├── i18n.rs                      # t()/tf()/t_count(), locale tables, language switching
├── icon_cache.rs                # app-icon disk cache + in-memory LRU
├── input_monitor.rs             # Accessibility permission supervision for every global input tap
├── key_bindings/                # data-driven global keyboard bindings (see below)
├── keystroke_display/           # opt-in on-screen keystroke display (see below)
├── language_tolerance.rs        # stable inventory of what each locale's UI must tolerate
├── logger.rs                    # logging, rotation, retention
├── mem.rs                       # periodic memory sampler (footprint + app ledgers)
├── menu.rs                      # status-bar menu: item state, actions, title refresh
├── mouse.rs                     # mouse enhancement root: scroll modes, profiles, feature gate
│   ├── mouse/                   # event tap, devices, scrolling, smoothing, pointer, shortcuts (see below)
├── onboarding.rs                # first-run onboarding window
├── overlay.rs                   # switcher overlay root: statics, callbacks, show/hide/refresh, raise
│   ├── overlay/                 # cards, hover, cancel, card close, footer, panel keys (see below)
├── panel_actions.rs             # in-overlay action keys (⌘W/⌘M/⌘H/⌘Q) on the highlighted card
├── performance.rs               # scheduling/QoS helpers for latency-sensitive threads
├── pointer_locator.rs           # dim the desktop around the pointer until it moves
├── quick_actions.rs             # quick-action bodies behind the binding actions
├── restart.rs                   # controlled relaunch after Accessibility is restored
├── runtime_config.rs            # applies a config change to every live surface (change source + deltas)
├── scroll_indicator_math.rs     # pure scroll-thumb math shared by both custom indicators
├── scroller.rs                  # overlay-style scroll view handling (scroller-style re-tiling)
├── settings.rs                  # settings window root: SettingsUi, controls, live-apply, restore
│   ├── settings/                # pages, components, dispatch, sidebar, import/export (see below)
├── single_instance.rs           # instance lock (GUI launches share one; smoke processes do not)
├── skylight.rs                  # the one entry point for private-framework dlopen/dlsym
├── space_groups.rs              # which desktop owns a fullscreen Space
├── space_swipe.rs               # synthetic Dock swipe used to commit a Space switch
├── space_transition.rs          # Space-change settling state shared by collection/refresh/capture
├── theme.rs                     # design tokens: colors, type scale, spacing, radii, durations
├── thumbnail.rs                 # window thumbnails: SkyLight capture, memory-only LRU, five producers
│   ├── thumbnail/               # capture, cache, pregen, summon, blank-frame (see below)
├── ui_coordinator.rs            # cross-surface refresh (theme/locale changes reach menu + overlay + settings)
├── ui/                          # shared UI mechanics used by more than one surface
│   ├── mod.rs
│   └── corner_glyph.rs          # the corner-glyph button family (footer gear, picker header glyphs)
├── update_notice.rs             # post-install "update installed" notification
├── updater.rs                   # Sparkle 2 integration, loaded dynamically
├── window_collector.rs          # switchable-window discovery + raising (see below)
│   ├── window_collector/
├── window_management.rs         # Option+arrow tiling/moving window behaviours
├── window_refresh.rs            # window-snapshot refresh pipeline (merge, generation, apply)
└── window_server.rs             # WindowServer lifecycle/focus notification adapter
```

## Layering

The dependency direction is one-way; a cycle is a design error, not a style nit.

```text
config / i18n / theme / hash / ffi / event_tap / skylight      leaves
        ↓
clipboard / overlay / settings / thumbnail / mouse / window_collector
        ↓
runtime_config / ui_coordinator / menu / onboarding                orchestration
        ↓
lib.rs (run)                                                      wiring + run loop
```

- **Leaves** carry no policy: `ffi.rs`, `skylight.rs`, `event_tap.rs`, `hash.rs`, and `callback_guard.rs` are
  the pieces every subsystem would otherwise implement twice, and `config.rs`/`i18n.rs`/`theme.rs` are the
  three shared vocabularies (data, words, design tokens).
- **Subsystems** own their state and their windows. They may read leaves and `config`, never each other's
  internals; the few legitimate cross-surface actions go through `runtime_config.rs` (config deltas) or
  `ui_coordinator.rs` (theme/locale refresh), both of which call public refresh APIs rather than reaching in.
- `app_identity.rs` exists because `window_collector` and `icon_cache` otherwise formed a cycle; both depend
  on it one-way.

## Threads and the main thread

- AppKit views and every dynamically registered ObjC callback run on the main thread. Global input and device
  observers run on dedicated threads and marshal back to main; `performance.rs` keeps their QoS class honest
  for an accessory app.
- `callback_guard.rs` is the mandatory boundary for a Rust function entered from C or ObjC: a panic cannot
  unwind through `extern "C"`, so each callback shape has a conservative fallback.
- Raw pointer and CF lifetimes are preserved as the existing code has them: `ffi.rs` supplies the
  `Send`/`Sync` wrappers and `MainThreadSlot`s, and a slot is the sanctioned way to publish main-thread state
  to another thread. A cross-thread decision that must be read by a tap is published as one immutable snapshot
  (`panel_actions.rs`, `key_bindings::engine::SELF_FRONTMOST`) rather than a lock both sides share.
- Pure functions are the rule wherever a layout or timing decision can be asserted headlessly:
  `scroll_indicator_math.rs`, `overlay/cards.rs`'s layout helpers, `mouse/smooth/*`, `key_bindings/validate.rs`,
  `window_collector/window_state.rs`, `space_transition.rs`, and the frame helpers in `clipboard/model.rs`
  exist for that reason. A decision that a test re-derives is not tested at all.

## Switcher

- `overlay.rs` owns the overlay surface: statics, the card↔index map, show/hide/refresh, card building,
  theme application, and `activate_and_raise`.
- `overlay/cards.rs` builds and lays out cards (icon and thumbnail, greyed icons);
  `overlay/callbacks.rs` holds the keyboard-session state machine; `overlay/hover.rs` owns hit-testing, the
  hover tick, and deferred scroll hover; `overlay/cancel.rs` is click-outside and focus-loss dismissal;
  `overlay/card_close.rs` is the close-button pipeline (animation, async AX close, commit);
  `overlay/footer_settings.rs` and `overlay/panel_keys.rs` own the footer gear and the in-panel action keys.
- Dynamic card classes must not depend on Rust-side properties reached through `msg_send!`; use the existing
  card-index/key maps.
- `event_monitor.rs` matches the switching gestures and emits `GlobalEvent`; `event_tap.rs` is the shared tap
  plumbing. `key_bindings/engine.rs` owns a separate session tap for every binding-driven shortcut, so the two
  never claim the same press (see `key_bindings/table.rs` for the precedence table).
- `window_refresh.rs` is the snapshot pipeline: request merging (summon wins), the background collector, the
  generation guard, and main-thread application (MRU merge, selection anchoring).

## Window collection and raising

- The **Accessibility window list is authoritative** for what may be switched to; stored AX titles are what
  activation uses. Presentation-only fallbacks belong in the UI layer, never in this module's answer.
- `window_collector/collect.rs` gathers per-PID AX windows (parallelizable); `raiser.rs` runs the AX phase on a
  dedicated thread under a generation guard; `raise.rs` performs exact per-window raising through SLPS private
  APIs; `skylight.rs` supplies window→Space membership, and a missing private symbol makes the caller fall back
  to its legacy policy instead of failing; `raise_diagnostics.rs` holds the pure decisions (which activation API,
  whether the target desktop was reached, how a raise ended); `space_state_probe.rs` is the state-matrix
  instrument and `window_state.rs` the pure decode of one window's state evidence.
- `space_groups.rs`, `space_swipe.rs`, and `space_transition.rs` own Space bookkeeping: which desktop owns a
  fullscreen Space, how a switch is committed, and how long a Space change is considered settling.

## Thumbnails

- Capture goes through the private `SLSHWCaptureWindowList`; frames live in a **memory-only LRU on purpose** —
  plaintext screen content under `~/Library/Caches` is a privacy risk.
- `thumbnail/capture.rs` is the queue and its single rate-limited worker plus focused prewarm and geometry
  retries; `thumbnail/cache.rs` the LRU core; `thumbnail/pregen.rs` the AX observer that pre-generates frames on
  window creation; `thumbnail/summon.rs` the summon-time refresh; `thumbnail/blank_frame.rs` detects blank
  frames from background-suspended WKWebView windows.
- Thumbnails require Screen Recording and must degrade to icons/fallbacks without it. Keep layout helpers pure.

## Clipboard subsystem

Optional and off by default. `clipboard.rs` is the root: filter state, the entry model, history limits, and the
tests that assert the pure parts; `clipboard/` holds one file per responsibility:

```text
clipboard/
├── model.rs            # entries, grouping, filters, row/document geometry helpers
├── monitor.rs          # pasteboard change polling
├── pasteboard.rs       # pasteboard reads and writes
├── picker.rs           # the history overlay: window, rows, keyboard navigation
├── detail.rs           # the detail pane, header chrome, and the filter/options popover
├── drag.rs             # dragging the panel by any blank region
├── pin.rs              # the panel's "stays put" state, frozen height, deferred dismiss
├── paste_handback.rs   # the synthesized Cmd+V when the panel is pinned
├── search.rs           # search-field cell drawing and layout
├── text_style.rs       # text measurement and attributed-style helpers
├── notifications.rs    # pasteboard notification observers + the scroll capsules
├── image_cache.rs      # on-disk image bytes and preview cache
├── persist.rs          # encrypted history write, migration, load, purge
├── crypto.rs           # the on-disk envelope and the master key
├── keyring.rs          # where the master key comes from
├── keyring_acl.rs      # what the keychain item's ACL says about this build
├── storage.rs          # the session's storage state, its generation, and the purge transaction
└── smoke.rs            # isolated smoke mode (see "Testing")
```

- History is written **encrypted** (AES-256-GCM under a random key in the login keychain): the history TOML, the
  original image bytes, and both preview files. The key is never deleted; turning the switch off deletes the
  records instead, and `clear_on_quit` clears the history and cached images on an orderly exit.
- **Never fall back to plaintext.** When the key cannot be read the feature is *unavailable*: nothing is
  recorded (no in-memory fallback either), and the settings banner, the opened panel, and a system notification
  each say so. Deletion is a verified transaction — an unreadable record set, a failed delete, or an unrevoked
  purge marker blocks loading and writing rather than overwriting anything. See
  `docs/clipboard-encryption-plan.md`.
- `clipboard/smoke.rs` switches the whole subsystem into an isolated session (fixed test key, per-process temp
  directory, no pasteboard observers) for **every** `--smoke-*` launch, so a smoke process never prompts for the
  keychain or records the developer's clipboard.
- UI chrome conventions here are the same as everywhere else: geometry comes from `model.rs` frame helpers, the
  palette from `theme.rs` through `clipboard_palette`, and anything drawn on a layer is asserted in the GUI
  smoke runner rather than only in pure tests.

## Settings

- `settings.rs` is the root: `SettingsUi`, control builders, window build/show, the live-apply dispatcher
  (every change writes the in-memory `CONFIG` and persists debounced), and restore-defaults.
- `settings/window.rs` + `settings/window/chrome.rs` own the window and its native classes; `sidebar.rs` the
  two-pane background; `page_builder.rs`/`page_canvas.rs` the page context and its vertical layout;
  `components.rs` the semantic `SettingsSection`/`SettingsCard`/`SettingsRow`/`SettingsControl`/`SettingsButton`
  family; `widgets.rs` the concrete control classes; `select.rs` the custom select; `tooltip.rs` the
  disabled-setting hint; `dispatch.rs` the control-event dispatch and dependent enable/refresh;
  `mapping.rs` the mouse-mapping rows and side-button recorder; `bindings.rs` the key-bindings page.
- `restore.rs` is restore/reset, `portability.rs` is config import/export, `i18n`/`theme` reloads invalidate the
  cached window through `settings.rs`.
- Configuration is loaded **per field**: an invalid value falls back individually without discarding valid
  settings, and a runtime reload refreshes exactly the surfaces that changed (`runtime_config.rs`).
- Add a settings field through `docs/settings-field-checklist.md`: config struct, locale keys, page builder,
  dispatch table, runtime change flags, renderer. That chain is invisible to the type system because ObjC
  dispatch and the control-pointer tables are, so "where is this field read" is answered with `grep`.
- A `SettingsButton`'s `tag` **selects its hover/normal palette**; never carry an action id in `setTag:` — map
  the sender pointer to an action id instead, or the colour sticks after the pointer leaves.

## Global input

- `event_tap.rs` is the shared tap leaf; `event_monitor.rs` (switcher), `key_bindings/engine.rs` (bindings),
  `mouse/event_tap.rs` (mouse) and `keystroke_display/tap.rs` (read-only key display) each run their own tap.
- `input_monitor.rs` supervises Accessibility permission for **all** of them: revocation closes a creation gate
  and disables the taps, restoration restarts them, and `restart.rs` handles the one case that needs a relaunch.

### Key bindings

One data-driven engine replacing the old per-feature hotkey code: `engine.rs` owns the session tap,
`table.rs` the immutable lookup table the tap reads, `trigger.rs` the chord/double-tap triggers, `action.rs` the
action registry, `execute.rs` main-thread execution, and `validate.rs` the pure validation (conflicts included).
`quick_actions.rs` and `window_management.rs` now hold only **bodies** an action can run.

### Keystroke display

Opt-in. `tap.rs` reads key events (queued, never logged), `state.rs` is the pure badge state machine over virtual
time, `panel.rs` the non-activating keycap panel and its timer, `mapping.rs` key labels and a main-thread layout
cache, `hover.rs` the pure "keep the active panel visible" state, and `secure.rs` Secure Input polling plus a
short-lived Accessibility cache. **Never log key text**; `--e2e-state` includes badges only when asked for.

## Mouse

`mouse.rs` is the root; profiles match by VID/PID. `mouse/event_tap.rs` is the mouse tap (separate from the
switcher's), `device.rs`/`ffi.rs` the IOHID reverse lookup, `resolve.rs` config resolution ("All Mice" merged
with per-device profiles), `scrolling.rs` the scroll modes, `smooth/` the pure LinearMouse-derived wheel engine
(`engine.rs`, `delivery.rs`, `presets.rs`) plus `transformer.rs`, which posts from the tap's own run-loop thread,
`pointer.rs` pointer acceleration, `shortcut.rs` shortcut parsing/display, `keysim.rs` synthesized shortcuts,
and `system_action.rs` Dock/Exposé actions. Reconnects must reapply pointer settings.

## Text, theme, and shared UI

- Every user-visible string goes through `t()`/`tf()`/`t_count()` with keys in every locale under `locales/`;
  developer logs stay English; a raw key, config key, enum name, hex value, VID/PID or preset id must never reach
  the screen (`docs/design-style-en.md` §11).
- `theme.rs` holds the token set: semantic colors with contrast floors, the type scale (`FONT_SECTION_TITLE`
  11 … `FONT_PAGE_TITLE` 26, converging on the 12/14/20/26 scale `docs/ui-refresh-plan-en.md` tracks), the 4px
  spacing grid (`4 8 12 16 24 32`, `docs/design-style-en.md` §spacing), the radius scale (`RADIUS_CONTROL` 8,
  `RADIUS_CARD` 12, `RADIUS_PANEL` 18), the elevations, and the animation durations. Never put a literal color,
  size, radius, spacing or duration at a call site: add the token, document it, use it.
- `glass.rs` is the shared material backdrop for passive floating panels; `ui/corner_glyph.rs` the corner-glyph
  button family, so the footer gear and the picker header glyphs cannot drift apart; `scroller.rs` the
  overlay-style scroll handling; `scroll_indicator_math.rs` the pure thumb math both custom indicators share.
- Cards and rows are defined by border and surface, not shadow; hover and selection never change layout;
  every animated surface honours Reduce Motion.

## Dev, testing, and diagnostics modules

These exist for the gates, not for the product, and are linked into release unless a flag gates them:

- `e2e_state.rs` writes the `--e2e-state` JSON that tier A2 asserts on, because AX cannot express CALayer
  content or "which card is selected".
- `dev_backdrop.rs` pins a controlled backdrop for pixel instruments.
- `dev_flags.rs` is the **only** runtime switch channel, and it is argv-only: the app must never read the
  environment as configuration (a launchd job does not inherit the caller's shell, and a filter mistake once
  dumped the whole environment into a log).
- `language_tolerance.rs` is the inventory behind the language-tolerance checks.

## Where a change belongs

- Fix the module that already owns the responsibility; do not add a second copy next to it. If two surfaces
  share a mechanic, it belongs in `ui/`, `glass.rs`, `theme.rs`, or `scroll_indicator_math.rs` — not copied.
- A user-visible decision (a frame, a threshold, a layout rule) that can be pure **must** be, so a tier-A test
  can assert it; the view then only applies the answer. A number in a comment is a claim: give it a command or a
  test that reproduces it.
- A new subsystem gets a directory with a `mod.rs` root and files named by responsibility, not by size; a new
  file must be added to this tree.
- Layout and appearance follow `docs/design-style-en.md`; settings fields follow
  `docs/settings-field-checklist.md`; verification follows the tiers in `AGENTS.md` (`scripts/cargo-test.sh`,
  the `--smoke-*` GUI runners, `scripts/e2e/run-all.sh`, then `scripts/dev-restart.sh`).
