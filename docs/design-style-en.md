# Design Style

The normative UI style for every surface oh-my-tab draws itself. Read this before changing
anything a user can see. The Chinese version is `docs/design-style.md`.

The rules below are adapted from Meta's [Astryx](https://astryx.atmeta.com/docs/principles) design
system (semantic tokens, a single scale per dimension, layer-by-layer surfaces, motion with a
purpose) and from the macOS conventions the app already follows. Where this document disagrees with
an existing implementation, this document wins and the implementation is a bug.

Deviating is allowed only when the platform forces it — AppKit draws the control, the OS supplies
the value, or the data is the user's own content. Say why in a comment next to the code.

---

## 1. Scope

| In scope | Out of scope |
| --- | --- |
| Settings window (sidebar, pages, cards, rows, controls, footer) | Native AppKit chrome we do not draw: traffic lights, menu bar menus, system panels |
| Switcher overlay (cards, captions, selection ring, footer, scrollbar, close/cancel affordances) | User data rendered as-is: window titles, app names, clipboard contents, file paths |
| Clipboard picker and detail panel | Third-party window thumbnails |
| Onboarding, update notice, toasts, tooltips | Anything behind an unmodified `NSAlert` / `NSPanel` system template |
| HUDs: keystroke display, pointer locator, quick-action feedback | |

Everything in the left column is "chrome" and must use the tokens below. Text and images in the
right column are data: they are laid out by the rules, but their content is never restyled or
rewritten.

## 2. Principles

1. **Semantic tokens, never raw values at call sites.** A view asks for "secondary text on a card",
   not for `#73737A`.
2. **One scale per dimension.** Type, spacing and radius each have a fixed, short ladder. A value
   that is not on the ladder is a bug, not a special case.
3. **Hierarchy comes from surfaces and borders, not from shadow.** Elevation means "this floats
   above other content" — nothing else.
4. **Motion explains a change; it never delays one.** Animate entrances and exits, not hover or
   high-frequency state.
5. **Text stays readable.** Contrast minimums are measured, not assumed.
6. **Everything visible is localized and human-readable.** No internal identifiers or config keys
   on screen; no hardcoded English inside a localized UI. A user-owned color may show its hex value
   only as the color well's caption.

## 3. Color

All colors live in `src/theme.rs` (`Colors` for the overlay, `UiPalette` for settings, HUDs and
auxiliary panels). Never inline a hex value in a view builder.

### 3.1 Roles

| Token | Light | Dark | Used for |
| --- | --- | --- | --- |
| `window_bg` | `#F6F7F9` | `#1C1C1E` | Window / detail pane background |
| `detail_bg` | `#F6F7F9` | `#1C1C1E` | Settings detail pane |
| `sidebar_bg` | `#F1F2F4` | `#242426` | Sidebar column |
| `sidebar_text` | `#68686F` | `#9E9EA6` | Sidebar item labels |
| `card_bg` | `#FFFFFF` | `#2C2C2E` | Cards, grouped rows, popover panels |
| `card_border` | `rgba(0,0,0,.10)` | `rgba(255,255,255,.10)` | Card and container outline; controls currently reuse it (see §3.3) |
| `separator` | `rgba(0,0,0,.07)` | `rgba(255,255,255,.09)` | Row dividers inside a card |
| `field_bg` | `rgba(118,118,128,.10)` | `rgba(255,255,255,.07)` | Text fields, read-only value boxes |
| `button_bg` | `#FFFFFF` | `#2C2C2E` | Secondary settings actions |
| `button_text` | `#2C2C30` | `#F5F5F7` | Text on secondary settings actions |
| `footer_button_bg` | `#FFFFFF` | `#2C2C2E` | Settings footer actions |
| `text_primary` | `#2C2C30` | `#F5F5F7` | Row labels, values, card titles |
| `text_secondary` | `#4A4A52` | `#C7C7CC` | Section headers, descriptions, links' labels |
| `text_muted` | `#68686F` | `#9E9EA6` | Version strings, subtitles, empty-state text |
| `text_disabled` | `#9B9BA2` | `#7C7C84` | Disabled control labels |
| `accent` | `#0A84FF` | `#0A84FF` | Switches, sliders, primary action, selection ring |
| `accent_hover` | `#0077ED` | `#3D9BFF` | Pressed/hover state of the accent |
| `destructive` | `#FF3B30` | `#FF453A` | Destructive actions and destructive-confirmation UI |
| `destructive_hover` | `#D70015` | `#D93630` | Hover state for destructive actions |
| `symbol_shadow` | `rgba(0,0,0,.70)` | `rgba(0,0,0,.70)` | Visibility glyph shadow over arbitrary thumbnails |
| `badge_scrim` | `rgba(0,0,0,.55)` | `rgba(0,0,0,.55)` | Circular chip behind overlay thumbnail corner badges |
| `accent_text` | `#FFFFFF` | `#FFFFFF` | Text on accent and destructive fills |
| `keycap_accent_bg` | `rgba(10,132,255,.22)` | `rgba(10,132,255,.22)` | Modifier and indicator keycaps |
| `keycap_accent_border` | `rgba(10,132,255,.69)` | `rgba(10,132,255,.69)` | Modifier and indicator keycap outline |
| `keycap_accent_text` | `#2C2C30` | `#F8F9FA` | Modifier and indicator keycap text |
| `switch_on_disabled_track` | `rgba(10,132,255,.45)` | `rgba(10,132,255,.45)` | Disabled on-state switch track |
| `switch_off_track` | `#C7C7CC` | `#636366` | Off-state switch track |
| `switch_off_disabled_track` | `rgba(199,199,204,.45)` | `rgba(99,99,102,.45)` | Disabled off-state switch track |
| `switch_knob` | `rgba(255,255,255,.96)` | `rgba(245,245,247,.96)` | Switch knob |
| `selection_bg` | `rgba(10,132,255,.08)` | `rgba(10,132,255,.22)` | Selected sidebar row, selected list tile |
| `hover_bg` | `rgba(118,118,128,.12)` | `rgba(255,255,255,.10)` | Hovered row / list item |
| `shadow` | `rgba(0,0,0,.06)` | `rgba(0,0,0,.35)` | Elevation shadows (see §7) |

Status colors (`success`, `warning`, `error`) are semantic, not decorative: use them for status
text/icons and permission state only, never to label a category. Permission-granted text is
success-green; permission-missing text is warning/error — never the accent.

| Status text | Light | Dark |
| --- | --- | --- |
| `success_text` | `#176B3A` | `#30D158` |
| `warning_text` | `#A63D0A` | `#FF9F0A` |
| `error_text` | `#B42318` | `#FF6961` |

Against `window_bg`, measured contrast is respectively 6.12:1 / 8.42:1, 5.95:1 / 8.28:1, and
6.13:1 / 6.03:1. The keycap accent text over its translucent fill composited on `card_bg` measures
10.61:1 in light mode and 10.22:1 in dark mode.

### 3.2 Surface hierarchy

`window_bg` → `sidebar_bg` → `card_bg` → popover panel. Each level is visually distinct from the one
behind it, and no level is used for two purposes. A group of related rows on a window background is
a card, not a differently-shaded window.

### 3.3 Contrast minimums

Measured with the composited (post-alpha) color against the surface the element actually sits on.

**Text**

| Role | Minimum | Measured, light | Measured, dark |
| --- | --- | --- | --- |
| `text_primary` | 12:1 | 12.97:1 on `window_bg` | 15.63:1 |
| `text_secondary` | 7:1 | 8.19:1 on `window_bg` | 10.10:1 |
| `text_muted` | 4.5:1 | 5.16:1 on `window_bg`, 5.53:1 on `card_bg` | 6.40:1 / 5.24:1 |
| `text_disabled` | 2.5:1 | 2.58:1 on `window_bg`, 2.76:1 on `card_bg` | 4.11:1 / 3.37:1 |

`text_primary` has the smallest margin in the palette. Re-measure it whenever the primary color or
`window_bg` changes; do not darken it for its own sake.

**Non-text**

Two clauses, because they answer different questions.

1. **State carried by color alone — floor 3:1.** Focus rings, selection rings, status dots, error and
   warning outlines, and any indicator whose meaning is lost when the color is removed. The accent is
   `#0A84FF`: **3.65:1** on `card_bg`, 3.40:1 on `window_bg`. A new state indicator is measured before
   it ships.
2. **Structural boundaries — floor 1.2:1, documented deviation.** Card outlines, control outlines,
   dividers and the switch track reinforce information that fill, shape, position and label already
   carry. They sit below WCAG 1.4.11's 3:1 on purpose: a 3:1 outline on `card_bg` requires
   `rgba(0,0,0,.42)`, i.e. `#949494`, which reads as a heavy, non-native border. Measured:
   `card_border` **1.25:1** on `card_bg`; switch off-track `#C7C7CC` (light) **1.68:1**, `#636366`
   (dark) **2.33:1**.

   The deviation holds only while the boundary is **not the sole identifier** of the control. If a
   control would be unrecognizable without its outline — no fill, no label, no shape, no adjacent
   icon — its outline must reach 3:1, and that is a new `control_border` token, not a change to
   `card_border`.

The switch is the model case for clause 2: off versus on is carried by the knob's position and by the
accent fill, so the track color is never the only signal.

When adding a token, measure it; do not eyeball it. A translucent color is checked after compositing,
not by its alpha.

### 3.4 Translucency

Window and card backgrounds may be translucent so the desktop shows through the glass. The settings
sidebar uses an opaque `sidebar_bg` surface so its color remains stable. **Text-bearing surfaces must
remain opaque enough that the contrast table above still holds on the worst-case backdrop**. The
overlay's glass is the exception: it is a transient surface over arbitrary content, and its text
colors are chosen for the darkest and lightest backdrops it can ever cover.

## 4. Typography

Four sizes. Anything else is a bug.

| Role | Size | Weight | Color | Notes |
| --- | --- | --- | --- | --- |
| `page-title` | 26 | 700 | primary | Settings page heading, one per page; tracking `-0.4` |
| `sidebar-title` | 20 | 700 | primary | App name at the top of the sidebar |
| `section-header` | 12 | 600 | secondary | Card group title above a card |
| `row-label` | 14 | 400 | primary | Settings row label, list primary text |
| `control` | 14 | 400 | primary | Select value, field text, button title, switch/row secondary value |
| `caption` | 12 | 400 | muted | Row description, version, empty state, footnote |

- Built-in weights use 400 / 600 / 700. Semibold is for section headers; bold is for titles.
  Explicit `fonts.*_weight` values are advanced overlay preferences and are preserved; legacy 500
  defaults migrate to semibold.
- Numeric readouts (slider values, counts, byte sizes) use tabular numerals so digits do not jitter.
- Line height: 1.35 for 12pt, 1.4 for 14pt, 1.2 for titles. Never set a line height manually where a
  role already defines one.
- Do not shrink text to fit a control. Truncate or move the content instead (§9).
- The version string in the About page is `caption`, not a row: it belongs in the page subtitle, not
  in a value column.

### 4.1 Overlay text

The card title, card app name and footer are the only user-scalable type in the app. The two card
caption roles share `layout.card_text_size`; the footer uses its dedicated `fonts.status_bar_size`
setting directly.

| Role | Uses `layout.card_text_size` | Effective size |
| --- | --- | --- |
| Card title (`fonts.title_size`) | yes | 13…20pt after scaling |
| Card app name (`fonts.app_name_size`) | yes | 13…20pt after scaling |
| Footer (`fonts.status_bar_size`) | no | configured 13…20pt |

- **Card captions share one multiplier.** Changing `layout.card_text_size` scales the title and app
  name together while preserving their relative sizes.
- **The clamp applies to the result**, not to the multiplier and not to the configured base. A
  card-caption `fonts.*_size` value is a base, not a final size: with the 12pt title base the title
  lands on `card_text_size`; the app-name base scales proportionally and both are clamped to 13…20pt.
- **The footer is independent.** Its size is exactly `fonts.status_bar_size` after the 13…20pt
  validation clamp; card-caption scaling does not change it.
- **The rendered line height of any caption row is at most 1/3 of its card's height.**
- **A caption is one line.** When it does not fit, it truncates with an ellipsis — it never wraps to a
  second line and never pushes the thumbnail out of its card.

Markdown release notes use `sidebar-title` bold for level-one headings and `control` semibold for
level-two headings. Overlay base sizes, shared scaling, and result clamping are implemented in
`theme.rs`; advanced explicit weight overrides remain user preferences.

## 5. Spacing

Base unit **4px**. Allowed steps: `4 8 12 16 24 32`. A 2px step is allowed only *inside* a control's
own geometry (knob inset, hairline offset), never between siblings.

Named layout metrics for the settings window (all on the grid):

| Metric | Value | Where |
| --- | --- | --- |
| Sidebar width | 220 | `SETTINGS_SIDEBAR_WIDTH` |
| Sidebar item inset | 12 | `settings/sidebar.rs` (`btn_w = card_w - 24`) |
| Page top padding | 48 | `SettingsPageHeader::TOP_PADDING` |
| Page bottom padding | 72 | `SettingsPageHeader::BOTTOM_PADDING` |
| Section header gap | 24 | `SETTINGS_SECTION_HEADER_GAP` |
| Header → card gap | 4 | `SETTINGS_SECTION_CARD_GAP` |
| Row gap | 8 | `SettingsLayout::row_gap` |
| Rows | 52 tall, control 32 tall, vertically centred | `SettingsLayout` |
| Control column | 200 wide, right-aligned, 16 from the card's right edge | `SettingsLayout` |
| Card horizontal padding | 16 | row builders (`label_x`, `SETTINGS_CONTROL_TRAILING_INSET`) |
| Card bottom inset | 8 | `SettingsLayout::card_bottom_inset` |
| Divider above a row | 4 below the previous row's content | `SEPARATOR_ABOVE_ROW_GAP` |
| Slider readout | 44 wide, 8 gap | `SLIDER_READOUT_*` |

Rule of thumb: **related things 8, groups 24, page edges 32–72.** Use the smaller step for internal
density and the larger one to separate sections; never mix `12` and `14` for the same kind of gap.

## 6. Radius

| Token | Value | Applied to |
| --- | --- | --- |
| `radius-control` | 8 | Buttons, text fields, selects, tiles, list-item highlight, switch track inner geometry |
| `radius-card` | 12 | Cards, grouped row containers, list tiles, popover items |
| `radius-panel` | 16 | Tooltips, dropdown panels, HUDs, notification cards |
| `radius-full` | 9999 | Pills: badges, status dots, switch, scrollbar knob |
| Window radius | platform-dependent (measured; currently 26) | The settings window itself |

**Concentric rule.** A rounded container with padding gives its inner element
`max(0, outer_radius - padding)`. A selection ring drawn around a card is
`card_radius + ring_inset` so the two stay parallel; never pick the ring radius by hand.
The switcher selection ring uses a 3pt `ring_inset`.

The overlay's card radius follows `appearance.corner_radius` up to `radius-panel` (16pt). Larger
values remain available to glass and window surfaces, but do not make switcher cards excessively
round. The captured window thumbnail itself keeps square corners; derive the close-button and
selection-ring radii from the effective card radius so a settings change cannot leave stale geometry
behind. The small app icon in the caption uses `radius-control`.

## 7. Elevation

Four levels. Pick by **how far the surface sits from the page**, not by how much shadow looks good.

| Level | Shadow | Use |
| --- | --- | --- |
| `none` | — (border only) | Default. Rows, cards, inline banners, buttons |
| `low` | `0 1px 2px rgba(0,0,0,.06)` | In-flow surfaces that must read as distinct (a card that needs emphasis) |
| `med` | `0 4px 12px rgba(0,0,0,.10)` + border | Floating above page content: popovers, dropdown menus, tooltips |
| `high` | `0 12px 32px rgba(0,0,0,.18)` | Above the whole UI: dialogs, onboarding, the switcher panel over a dimmed backdrop |

The medium elevation is represented in `theme.rs` by one shared black shadow color, opacity, blur
radius, and vertical offset. Use those constants for both dropdowns and tooltips. Thumbnail corner
badges (fullscreen, minimized/hidden) are a circular `badge_scrim` chip carrying a white glyph; the
chip uses the separately named `symbol_shadow` role and its shared geometry constants — that small
contrast aid is not surface elevation. Its opacity is 0.85, blur radius 2pt, and vertical offset
−1pt. The white glyph on the chip composited over a pure-white thumbnail measures 4.74:1, above the
3:1 non-text floor.

- Exactly one level per surface. Never stack shadows to make a surface "more elevated".
- Cards in the settings page are `none`: hierarchy comes from `card_bg` + `card_border`. A large
  soft shadow on a card inside a window is a bug — it implies the card floats over content it does
  not float over.
- Borders are `card_border`, not a shadow and not a darker background.
- An inset accent ring is for input focus (2pt, as defined in §10); it is not elevation. A card or
  tile selection may use an outer accent ring; derive its outer corner radius from the card radius
  and the ring inset described in §6.

## 8. Motion

| Token | Value | Use |
| --- | --- | --- |
| `duration-fast` | 175 ms | Micro-interactions except the switch's component-specific press response |
| `duration-medium` | 380 ms | Spatial change: panel open/close, expand/collapse, page transition |
| `ease-standard` | `cubic-bezier(0.24, 1, 0.4, 1)` | Every animation unless there is a documented reason |
| exit duration | 0.75 × the entrance duration, same curve | Closing panels, dismissing dialogs |

Rules:

- **Animate the entrance, not the exit, of contextual UI.** Tooltips, hover cards and dropdown
  menus may disappear instantly; panels, dialogs and expanding rows animate both ways, and the exit
  retraces the entrance direction.
- **Never animate** hover fills, row highlights, selection while the user is moving, scrolling, or
  anything the user triggers several times a minute. Those must be instant.
- Motion must never block input: content that is already interactive stays interactive while it
  animates.
- Springs are allowed only where a physical feel is the point (switch knob, card close, panel
  bounce). Use the single documented spring per component; do not tune stiffness/damping per call
  site. Two components must not use different springs for the same gesture.
- **Reduce Motion is mandatory and checked at animation time**, not cached at launch:
  `NSWorkspace.accessibilityDisplayShouldReduceMotion` → replace the animation with an instant state
  change. Every animated surface must do this, including the overlay, page transitions and the
  settings shell.

## 9. Components

**Settings row.** Label on the left (14/primary), control on the right, right-aligned to the control
column. Supports an optional `caption` line under the label (12/muted). One concept per row.

- The control is exactly one line tall (32pt). A value that does not fit is truncated with an
  ellipsis and exposes the full value as a tooltip — **a control never wraps to two lines.**
- A row's label is a noun phrase, not a sentence. Explanations go in the caption line.
- Rows are separated by a full-bleed 1px `separator` inside the card; the first and last rows have no
  divider at the card's edges.

**Card.** Groups related rows. `radius-card`, `card_bg`, `card_border`, elevation `none`. A card has
one section header above it. Do not wrap a single row in a card when it belongs to the neighbouring
group.

**Section header.** 12/600/secondary, 24 above the card, 4 below. Sentence case, no trailing colon.

**Buttons.** Three roles: primary (accent fill, white text), secondary (card bg + border), destructive
(red text/border, red fill only on the confirming action). One primary per view. In-row action
buttons are 32pt tall and right-aligned to the control column. A button's hover/normal palette must
not be encoded in its `tag` (see `AGENTS.md`).

**Switch.** 38×22 with an 18pt knob; on = `accent`, off = `switch_off_track`. The
whole row is not clickable; only the switch is. The knob uses its spring settling duration with an
180 ms minimum; its press response lasts 220 ms with the keyframe's default timing. These
component-specific values override the general fast-duration and easing tokens. The knob follows
the pointer only within its 2pt inset.

**Slider.** Continuous — **no tick marks**. The filled portion is `accent`; the knob is white with a
1px border. It is always paired with a right-aligned tabular readout of the current value, and that
readout is the same width on every row of a group.

**Select.** 200pt wide, one line, ellipsis on overflow, chevron on the right. The dropdown panel is
`radius-panel` at elevation `med`; items use `radius-control` (8) with a `hover_bg` fill. The
displayed option text is localized and human-readable; if the raw value is meaningful to a
developer but not to a user (VID/PID, preset key), show a display name and move the raw value to the
caption line.

**Text field.** `field_bg`, `radius-control`, 32pt tall, focus shown by an inset accent ring.

**Color well.** A swatch with a border plus the hex value as a `caption`. Never a bare gradient
shape.

**Tooltip.** `radius-panel`, elevation `med`, 14pt text, appears instantly on hover-delay and
disappears instantly.

**Empty state.** Centered `caption` text in `text_muted`; no illustration unless the surface is a
first-run surface (onboarding).

## 10. Interaction states

Every custom-drawn control needs all six states, and each uses the token above, not a new color:

| State | Treatment |
| --- | --- |
| normal | base tokens |
| hover | `hover_bg` fill (instant, no transition) |
| pressed | accent at ~85% for filled controls; `hover_bg` + slight inset for bordered ones |
| selected | `selection_bg` fill, or an accent ring for cards and tiles |
| disabled | `text_disabled`, 50% opacity on the control's fill, no hover feedback |
| focus (keyboard) | inset accent ring, 2pt, always visible against both `card_bg` and `window_bg` |

Hit targets are at least 28×28; a text-only affordance needs padding to reach that. Hover and
selection never change the layout (no size, no weight change) — only color.

## 11. Accessibility and localization

- Contrast: see §3.3. Re-measure whenever a palette value changes.
- Reduce Motion: see §8. Mandatory.
- Every custom-drawn control sets an accessibility label and role through AppKit, so VoiceOver reads
  the same information the pixels show. A control whose value is a number reads that number.
- Every user-visible string goes through `t()` / `tf()` / `t_count()` and has a key in **all**
  locales (`en`, `zh-Hans`, `zh-Hant`). A missing key renders the key itself, which is never
  acceptable on screen.
- Never put a config key, an enum name, a hex value, a VID/PID pair or an internal preset identifier
  in a label or control value. A color well's caption is the sole exception for a user-owned color.
- Layout must survive the pseudo-locale (`--pseudo-locale`) and the long-text fixture: no clipping,
  no overlap, no truncation of a label that the user needs to read.
- Text scaling: a user-configurable size never changes the *structure* of a row (no row grows a
  second line because of it).

## 12. Adding a token or a value

1. Prefer an existing token. If two roles need slightly different colors, the roles are wrong — fix
   the roles first.
2. If a genuinely new value is needed, add it to `src/theme.rs` next to its siblings, give it a
   semantic name, and fill in both light and dark.
3. Measure the contrast of any new text color and record it in the table above.
4. Update this document in the same change. A value that is not in this document does not exist.

The font-weight values used by AppKit are named in `theme.rs`: regular `0.0`, semibold `0.3`, and
bold `0.4`. The medium elevation constants correspond to the `med` row above. The switcher ring
inset is 3pt as specified in §6.

## 13. Review checklist

- [ ] No new literal color, font size, radius, spacing or duration in a view builder.
- [ ] New/changed text roles meet their contrast minimum on every surface they can appear on.
- [ ] Every new string has keys in `en`, `zh-Hans`, `zh-Hant`; no raw identifier on screen.
- [ ] Controls are single-line; long values truncate and expose the full value.
- [ ] Cards use border, not a large shadow; elevation level matches what the surface floats over.
- [ ] Radius comes from the three-step scale; derived radii are computed, not hardcoded.
- [ ] Animations use the two durations and the standard curve, and honor Reduce Motion.
- [ ] New code paths are covered by the appropriate tier-A test (see `AGENTS.md`).
