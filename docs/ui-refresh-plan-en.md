# UI Refresh Plan

How to bring the current UI in line with `docs/design-style-en.md` (Chinese: `docs/ui-refresh-plan.md`,
style spec: `docs/design-style.md` / `docs/design-style-en.md`). This is a plan, not a spec: the spec
is what the UI must become, this file is how and in what order we get there.

Each phase is independently shippable and independently reviewable. Nothing in this plan changes
behaviour a user would call a feature — it changes how the app looks and how it reads.

## 1. Ground rules

- Before handing off any phase, run the full gate from `AGENTS.md`:
  `cargo fmt` → `cargo check` → `cargo clippy` → `cargo test`, then `scripts/dev-restart.sh --no-onboarding`.
- Layout-touching phases also run `--smoke-settings-layout`; overlay-touching phases also run
  `--smoke-overlay`; clipboard-touching phases also run `--smoke-clipboard`.
- **Promotion rule**: every problem this review found is either fixed with an assertion in tier A, or
  with a state field that makes it assertable. A fix without an assertion is not done.
- If a phase changes a value that the layout validator or a geometry test hardcodes, update the
  constant and the test together; never loosen the assertion to make it pass.
- No phase may leave the tree in a state that renders a raw translation key, a raw identifier, or a
  value that is off the scale it belongs to.

## 2. Measured baseline

What the app does today, with the literal's location. This is the "before" column of the plan.

| Dimension | Current | Where |
| --- | --- | --- |
| Font sizes | `12, 13, 13.5, 14, 20, 24, 30` | `settings/widgets.rs:826,2099,2132`, `settings/select.rs:130`, `settings/page_builder.rs:1131,1784,1807`, `settings/sidebar.rs:145` |
| Radii | `1, 4, 5, 6, 7, 8, 9, 10, 12, 14, 16, 18, 26` | `settings/*.rs`, `overlay/cards.rs:295,329`, `clipboard/*.rs`, `keystroke_display/panel.rs:1183` |
| Spacing off the 4px grid | `3, 6, 10, 14, 17, 18, 28, 34, 46, 50, 54` | `settings/components.rs:19,56,60,87,88,103,156,1827,1828`, `settings.rs:341`, `settings/sidebar.rs:178,184,199` |
| Light `text_primary` | `#2C2C30` → 12.97:1 on `window_bg` — already compliant, **not changed** | `theme.rs:121` |
| Light `text_secondary` | `#73737A` → **4.39:1** | `theme.rs:122` |
| Light `text_muted` | `#9B9BA2` → **2.76:1** (card) | `theme.rs:124` |
| Light `text_disabled` | `#AEAEB5` → **2.21:1** | `theme.rs:125` |
| Card definition | 7% border + 36pt soft shadow | `theme.rs:118`, `settings/widgets.rs:2196` |
| Preset label | raw key `mouse_smooth_preset_ease_in` | `mouse/smooth/presets.rs:95–105`, `settings/page_builder.rs:899` |
| Hardcoded English | `Regular`, `Clear`, `Debug`, `Info` | `settings/page_builder.rs:558,574,658` |
| Select overflow | wraps to two lines | `settings/select.rs:26` |
| Sliders | integer sliders draw tick marks | `settings/widgets.rs:1536` |
| Motion | 0.16–0.58s, four hand-tuned springs | `settings/components.rs:807–816`, `overlay.rs:71` |
| Reduce Motion | honoured in 4 surfaces only | `settings/tooltip.rs:342`, `settings/components.rs:1431`, `clipboard/notifications.rs:1044`, `updater.rs:689` |

## 3. Phase 0 — correctness

Small, high-impact, low-risk. These are the defects the review found, not taste.

### P0-1 · Live preview is invisible in light mode — resolved by removal

- **Now**: the General page no longer draws a live preview of the switcher and clipboard panels, so
  there is nothing to make legible. The two mock blocks were the only content in the app that
  *simulated* a panel instead of being one; their fill was white at 0x48–0x90 alpha over a white card,
  which is why they were invisible in light mode in the first place.
- **Target reached**: nothing to style. A preview that misrepresents the real surface is worse than no
  preview, and the material it was meant to preview is now judged from the panels themselves.
- **Files**: `settings/glass_preview.rs` (deleted), `settings/page_builder.rs` (block and its metrics
  removed), `settings.rs`/`settings/window.rs` (handles, init and the preview check removed),
  `theme.rs` (`PREVIEW_TILE_*`, the stage color and the contrast helper removed), `e2e_state.rs`
  (the `settings_preview` frame field removed with its readers).
- **Acceptance**: the surface is gone, so the check is structural rather than visual: no `preview`
  symbols remain under `src/settings/`, and `--smoke-settings-layout` still passes on all three
  locales with the shortened Appearance card. The look of the switcher and clipboard panels is
  covered by their own panels' checks (`--smoke-keystroke-display-panel`, the overlay smoke).

### P0-2 · Preset dropdown shows a raw translation key

- **Now**: `SmoothPreset::label_key()` returns keys without the `settings.` prefix while the locale
  files store them under `[settings]`, so `t()` falls through to the key itself.
- **Target**: the dropdown reads 缓入 / Ease In and never renders a key.
- **Files**: `mouse/smooth/presets.rs`, `settings/page_builder.rs:897–901`.
- **Acceptance**: a unit test asserting every `label_key()` resolves in all three locales (a
  regression test for the whole `label_key` family, not just the one preset); plus a general
  `i18n` test that no call site passes a key with no `settings.`-prefixed counterpart. Consider a
  debug assertion that `t()` never returns its own input for keys containing `_`.

### P0-3 · Hardcoded English in a localized UI

- **Now**: glass style options and log level options are literal `["Regular","Clear"]` /
  `["Debug","Info"]` (`settings/page_builder.rs:558,574,658`); several other control values are
  backend tokens (`Auto`, device VID/PID, preset keys).
- **Target**: every option label is a `t()` key present in all locales; enum-backed values map to
  display strings; raw values move to the caption line.
- **Files**: `settings/page_builder.rs`, `locales/*.toml`.
- **Acceptance**: an i18n test that every value a control can display resolves through `t()`; the
  pseudo-locale run shows no untranslated ASCII in the settings window.

### P0-4 · Controls wrap to two lines

- **Now**: the old `settings_select_needs_wrap` function has been removed. The existing
  `settings_select_centers_single_line_inside_the_control` test checks only the label's vertical
  centering; there is still no assertion for one-line height or the longest device/preset names in
  all three locales with a complete-value tooltip. This item remains **unverified, not complete**.
- **Target**: one line, ellipsis, full value in the tooltip.
- **Files**: `settings/select.rs` (remove the wrap path; keep the label truncation path).
- **Acceptance**: a layout check that a control's text frame height never exceeds one line; a case
  with the longest possible device name and preset name in all three locales.

### P0-5 · Sliders draw tick marks

- **Now**: integer sliders set tick marks (`widgets.rs:1536`).
- **Target**: continuous, no ticks, right-aligned tabular readout.
- **Files**: `settings/widgets.rs`, callers in `settings/page_builder.rs`.
- **Acceptance**: the slider's `numberOfTickMarks` is 0 on every slider; existing readout tests keep
  passing.

## 4. Phase 1 — the token layer

This is the structural phase: introduce the scales the spec defines, then map existing call sites
onto them. The visible result is a UI that stops looking almost-aligned.

### P1-1 · Type scale 12 / 14 / 20 / 26

- Replace the 13 / 13.5 / 14 / 24 / 30 set. `13.5` disappears; page titles go 30 → 26; row labels
  and control values go 13.5 → 14.
- **Files**: `settings/select.rs:130,786,1552,1601`, `settings/widgets.rs:661,826,1775,2099,2132`,
  `settings/page_builder.rs:1131,1784,1807`, `settings/sidebar.rs:145,166`, `settings/tooltip.rs:538`.
  Some sizes are computed into a local `font` before `setFont:`; find those by grepping
  `systemFontOfSize|boldSystemFontOfSize` rather than trusting this list.
- **Risk**: a 14pt row label with a fixed 200pt control column truncates more than 13.5pt did. Check
  every control value in `zh-Hans` and under `--pseudo-locale`; widen the control column or shorten
  the label set if it clips. Never shrink the font back.
- **Acceptance**: a test enumerating the font sizes used by settings views and asserting the set is
  exactly `{12,14,20,26}`.

### P1-2 · Radius scale 8 / 12 / 16 + concentric derivation

- Collapse the 13 radii. Selection rings become `card_radius + ring_inset`; the overlay computes its
  tile/close radii from `appearance.corner_radius`.
- **Files**: `settings/components.rs:926,955,1065,1104`, `settings/widgets.rs:116,1223,1954,2001,2385,2625`,
  `settings/select.rs:345,858,1059`, `settings/sidebar.rs:188`, `settings/tooltip.rs:438,496`,
  `settings/page_builder.rs:1106,1118`, `overlay/cards.rs:129,150,295,329`,
  `overlay/cancel.rs:722`, `overlay/card_close.rs:751`, `clipboard/*.rs`.
- **Acceptance**: a test asserting that every `setCornerRadius:` value in the settings surface comes
  from a named constant; the ring radius equals the card radius plus the documented inset.

### P1-3 · Snap spacing to 4px

- `SEPARATOR_ABOVE_ROW_GAP` 3 → 4, `SLIDER_READOUT_GAP` 6 → 8, `SETTINGS_CONTROL_TRAILING_INSET`
  17 → 16, row `label_x` 12 → 16, sidebar item inset 14 → **12** (`btn_w = card_w - 28` →
  `card_w - 24`; hoist the literal out of `sidebar.rs:184,199` into one named constant), sidebar
  `LABEL_X` 46 → 48, `TOP_PADDING` 50 → 48, `card_bottom_inset` 10 → 8, row height 54 → 52, control
  height 34 → 32, in-row action button 28 → 32, slider readout 40 → 44.
- **Files**: `settings/components.rs`, `settings.rs`, `settings/widgets.rs`.
- **Risk**: page document heights change; the layout smoke validator and any hardcoded geometry in
  tests must be updated in the same change. Land it as one self-contained change so the diff is
  reviewable as "the same layout, snapped".
- **Acceptance**: `--smoke-settings-layout` passes on all eight pages; a test asserting the named
  metrics are multiples of 4.

### P1-4 · Palette contrast

Light (`theme.rs:121–125`):

| Role | Now | Target | Measured after (window / card) |
| --- | --- | --- | --- |
| `text_primary` | `#2C2C30` | **unchanged** | 12.97:1 / 13.91:1 — already above the 12:1 floor; do not darken it for its own sake |
| `text_secondary` | `#73737A` | `#4A4A52` | 8.19:1 / 8.78:1 |
| `text_muted` | `#9B9BA2` | `#68686F` | 5.16:1 / 5.53:1 |
| `text_disabled` | `#AEAEB5` | `#9B9BA2` | 2.58:1 / 2.76:1 |

`#9B9BA2` moves from `text_muted` to `text_disabled`: it fails the 4.5:1 text floor but clears the
2.5:1 disabled floor with margin, so the old value is reused rather than discarded.

Dark: check that `secondary` / `muted` / `disabled` clear the same floors (the spec table carries the
measured values); raise any that do not.

- **Files**: `theme.rs` (`ui_palette`), plus whichever surface shares the palette helper.
- **Risk**: `hex_to_ns_color` is shared; changing a role changes every panel at once. Re-check the
  onboarding window, tooltips and HUDs after the change.
- **Acceptance (text)**: a unit test computing the contrast of every text role against **both**
  surfaces it can sit on (`window_bg` and `card_bg`) and asserting the spec minimums. A role that only
  passes on one of the two fails — the muted/disabled pair is exactly where that mistake hides.
- **Acceptance (non-text)**: a test asserting the accent clears 3:1 on both surfaces, plus a
  regression test pinning the structural-boundary values (`card_border` 1.25:1; switch off-track
  1.68:1 light, 2.33:1 dark). They sit below WCAG 1.4.11's 3:1 **by design** (spec §3.3 clause 2), so
  pin them at the documented 1.2:1 floor: dropping under the floor must fail, and "fixing" the
  deviation without a stated reason must not pass review.

### P1-5 · Cards defined by border, not shadow

- Settings cards go to elevation `none`: border at 10%, no shadow;
  `SETTINGS_CARD_SHADOW_INSET = 36` disappears.
- **Files**: `settings/widgets.rs:2196`, `theme.rs` (`shadow`).
- **Acceptance**: a test asserting settings cards have no shadow; visual
  check on both palettes.

## 5. Phase 2 — motion

### P2-1 · Two durations, one curve

- Reduce the four springs and the scattered durations to `duration-fast = 175ms`,
  `duration-medium = 380ms`, `ease-standard = cubic-bezier(0.24,1,0.4,1)`, and keep a single
  documented spring for the switch knob and card close. Preserve the switch's 220ms press response
  and spring settling duration as documented component-specific exceptions.
- **Files**: `settings/components.rs:807–816`, `overlay.rs:71`, `settings/widgets.rs:1239–1242`,
  `clipboard/picker.rs` (detail panel duration).
- **Acceptance**: a test asserting animation durations use named tokens or documented component
  exceptions, and that hover/selection transitions have no duration at all.

### P2-2 · Reduce Motion everywhere

- Extend the check to the overlay, settings page transitions, expand/collapse and HUDs; read it at
  animation time rather than caching it.
- **Files**: the four existing implementations plus `overlay/*`, `settings/components.rs` callers.
- **Acceptance**: a forced-reduce-motion dev flag (or reuse of the existing smoke hook) that lets a
  test assert every animated surface takes the instant path.

## 6. Phase 3 — polish

| ID | Change | Files |
| --- | --- | --- |
| P3-1 | Sidebar "restore defaults" reads as a button (fill + border), and the global vs per-page scope is stated | `settings/sidebar.rs`, `settings/page_builder.rs` |
| P3-2 | About page uses the product name as its title; version appears once, as the page subtitle | `settings/page_builder.rs:1784–1808` |
| P3-3 | Sentence-length row labels move their parenthetical to the caption line (e.g. the clipboard "delete after paste" row) | `settings/page_builder.rs`, `locales/*.toml` |
| P3-4 | Device dropdown shows a short display name; VID/PID moves to the caption | `settings/page_builder.rs`, `mouse/device.rs` |
| P3-5 | Overlay text roles: card title and app name scale with `layout.card_text_size`; footer uses `fonts.status_bar_size` directly; the card `13…20pt` clamp applies after scaling; every caption row's line height ≤ 1/3 of its card; derived radii follow the effective card radius | `theme.rs`, `overlay/cards.rs` |
| P3-6 | Empty states and the About subtitle use `text_muted` at the new contrast | `settings/page_builder.rs` |

Each P3 item is independent and can ship on its own.

## 7. Verification per phase

```sh
cargo fmt && cargo check && cargo clippy && cargo test
scripts/dev-restart.sh --no-onboarding            # functional handoff
scripts/dev-restart.sh --no-onboarding --pseudo-locale   # long-text layout
scripts/dev-restart.sh --opt --no-onboarding      # feel/perf check for Phase 2
# GUI smoke entries (each exits non-zero on failure)
./dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab --smoke-settings-layout
./dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab --smoke-overlay
./dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab --smoke-clipboard
# A2 end-to-end (asks before stealing focus)
scripts/e2e/run-all.sh
```

Phase 1 changes page geometry, so `--smoke-settings-layout` is the phase's primary gate. Phase 2 is
about feel, so validate it with `--opt` and a real scroll/summon rather than by reading constants.

## 8. Risks

| Risk | Mitigation |
| --- | --- |
| Snapping spacing changes document heights and breaks layout assertions | Do P1-3 as one self-contained change; update the validator and its fixtures together |
| A 14pt control value truncates existing labels | Check `zh-Hans`, `zh-Hant`, `--pseudo-locale` before merging P1-1; widen the control column rather than shrinking text |
| The shared palette helper leaks a change into onboarding/HUDs/overlay | Re-verify those surfaces in both palettes after P1-4 |
| Derived radii drift again after the refactor | P1-2's assertion is what prevents this; do not skip it |
| Motion consolidation regresses perceived snappiness | Compare with `--opt` before/after; the goal is fewer distinct timings, not slower ones |
| Overlay changes interact with thumbnails/permissions | Keep overlay work to P3-5 and run `--smoke-overlay` plus a manual summon |

## 9. Done criteria

- Every row of §2 is either resolved or recorded as an accepted deviation with a reason in the code.
- `docs/design-style-en.md` §13's checklist passes for every surface.
- Tier A contains an assertion for each P0 item and for the P1 palette and scale invariants.
- No user-visible string renders a raw key, and no control wraps to two lines.
