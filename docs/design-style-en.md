# Design Style

The normative UI style for every surface oh-my-tab draws itself. Read this before changing
anything a user can see. The Chinese version is `docs/design-style.md`.

The rules below are adapted from Meta's [Astryx](https://astryx.atmeta.com/docs/principles) design
system (semantic tokens, a single scale per dimension, layer-by-layer surfaces, motion with a
purpose) and from the macOS conventions the app already follows. Where this document disagrees with
an existing implementation, this document wins and the implementation is a bug.

The language-tolerance rules in §11 are adapted from Rene Wang's
[Build Interfaces That Survive Translation](https://rene.wang/essay/build-interfaces-that-survive-translation).

A deviation is allowed for one of two reasons, and both are recorded. Either the platform forces it —
AppKit draws the control, the OS supplies the value, or the data is the user's own content — and then
say why in a comment next to the code. Or the project deliberately trades a rule away, and then it is
written where it applies as a **documented deviation**, with its boundary and its reason; §3.3 clause
2 and §11.2 are the current instances. An unrecorded deviation is a bug either way.

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
7. **A rule that holds in one language only is not a rule.** Every layout decision is checked against
   all the languages we ship (`en`, `zh-Hans`, `zh-Hant`) and against the ones we might add: widths
   come from measurement, never from counting characters (§11).

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
| `scroll_indicator` | `rgba(0,0,0,.35)` | `rgba(255,255,255,.35)` | Custom scrollbar knobs |
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

`scroll_indicator` carries no meaning on its own, so it is not held to a text floor, but it must
stay on the visible side of the surface in both modes: darker than a light panel, lighter than a
dark one. A knob fixed to one mode's neutral is invisible in the other (a black knob measures
1.10:1 against the dark `window_bg`).

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

The floating panels (switcher, clipboard, keystroke display) offer three material families via the
`panel_material` setting:

| Material | Surface | Notes |
| --- | --- | --- |
| `liquid-glass` (default) | `NSGlassEffectView`, `glass_style`/`glass_tint` sub-options | Degrades to frost on macOS < 26 |
| `frost` | `NSVisualEffectView`, behind-window, themed material, plus the theme-surface wash below | Neutral system blur; the glass sub-options do not apply |
| `opaque` | `window_bg` fill, no blur | Also the forced fallback while the system's Reduce Transparency accessibility setting is on |

Material only ever changes which surface sits behind the panel content — text colors, spacing,
radius, and the palette remain exactly as specified here.

**A translucent surface must be held to that.** Both blurring materials drift toward mid gray on
their own: measured in dark mode, plain frost landed at `#868585` and liquid glass at `#6E6E6E`,
against the `#1C1C1E` the palette assumes, which drops `text_primary`/`text_secondary`/`text_muted`
from 15.63/10.10/6.40:1 to 3.38/2.18/1.38:1 and 4.68/3.03/1.92:1. No text color rescues a mid-gray
surface — the best any single color achieves there is 5.71:1, under this table's own 12:1 and 7:1
floors — so the **surface** is what gets pinned:

- `frost` composites `window_bg` over the blur at an opacity that keeps the worst case legible
  (a white backdrop still lands at gray 51 or darker).
- `liquid-glass` takes **hue and saturation only** from `glass_tint`; lightness comes from the
  theme and opacity has a floor, so a near-white tint is still a dark glass in dark mode. The
  historical `eeeeee66` default was a light-mode value that also failed in light mode (7.17:1),
  which is why the tint no longer decides lightness in either mode.

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
- Do not shrink text to fit a control. Widen the container or move the content instead (§9, §11.2).
- **12pt is the floor.** Do not introduce a smaller size for a badge, a keycap or a dense label. CJK
  glyphs carry their distinguishing detail in interior strokes at high spatial frequency (未/末,
  己/已/巳), so they need more pixels than Latin before they resolve at all; for this palette and
  these surfaces, 12pt is where that floor sits.
- **Hierarchy uses size, weight and color only.** Never let case, italic, small caps or letterspacing
  carry meaning: Chinese has no case, synthetic italic degrades dense glyphs, and weight is the
  weakest of those signals on CJK glyphs, because bolding fills the counters instead of increasing
  contrast against the surrounding whitespace. Where a Latin design reaches for case or italic, use
  color and enclosure instead. The `page-title` tracking of `-0.4` is optical, not semantic, and is
  the only letterspacing in the app.
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
| `radius-legend-chip` | 5 | The chip behind a key symbol in a read-only shortcut legend |
| `radius-card` | 12 | Cards, grouped row containers, list tiles, popover items |
| `radius-panel` | 16 | Tooltips, dropdown panels, HUDs, notification cards |
| `radius-full` | 9999 | Pills: badges, status dots, switch, scrollbar knob |
| Window radius | platform-dependent (measured; currently 26) | The settings window itself |

**Concentric rule.** A rounded container with padding gives its inner element
`max(0, outer_radius - padding)`. A selection ring drawn around a card is
`card_radius + ring_inset` so the two stay parallel; never pick the ring radius by hand.
The switcher selection ring uses a 3pt `ring_inset`.

A small chip derives its radius from its own height rather than taking `radius-control`:
`radius-control` is sized for 32pt controls, and on a 19pt chip it consumes 42% of the height and
the shape reads as a **pill** instead of a softened rectangle. `radius-legend-chip` (5pt) is 26% of
19pt. `radius-full` is never right for a chip either — a pill is a different shape, not a rounder
rectangle.

## 6.1 Legends are not controls

A **legend** states which key does what (`↵ 输入选中条目`). A **control** is something the user
operates. They are drawn differently on purpose, and the difference is not decoration:

| | Legend | Control |
| --- | --- | --- |
| Border | **none** | 1pt `card_border` |
| Fill | a wash lighter than `field_bg` | `field_bg` / `button_bg` |
| Pointer states | none | hover, pressed, focus, disabled (§10) |
| Hit target | none — it is not clickable | ≥ 28×28 |

The clipboard footer's shortcut symbols are the case that motivated this section. They carry no
target/action at all, yet a design-system pass had given them `field_bg` **plus a 1pt border** —
the exact visual contract of a pressable control. That is a promise the UI cannot keep: the user
learns that bordered chips are clickable and then finds these are not. The accent keycap palette
(`keycap_accent_*`) does **not** apply here either — it means "this modifier is currently held",
which is a live state, and the keystroke display is where a live state belongs.

So a legend keeps only what it needs to be read: a fill subtle enough not to imply interaction,
whose sole job is binding the symbol to its label so a row of legends stays scannable. What it
must **not** do is borrow the border, the radius scale, or the pointer states of a control.

"As subtle as possible" is bounded from below, not free: the fill must still be distinguishable
from the surface it sits on, in both modes. It is applied as a wash (darkening a light surface,
lightening a dark one) rather than a fixed gray, so one value works in both.

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
- **A row's label reads in full in every shipped locale.** Truncation is not the fix for a label that
  does not fit: widen the label column, shorten the string, or move detail to the caption line —
  never shrink the type (§4).
- **Truncation has an allow-list.** Only these may truncate by default, and each must expose its
  full value (tooltip, or the caption line): a control's value (select, text field, read-only value),
  a `caption`, and user data (window titles, app names, clipboard content). A label, a section header,
  a button title, a permission status and an error cause must fit — or be rewritten (§11.2).
- **Do not fix an ambiguous design with a word.** If two actions need "only", "just" or "also" to be
  told apart, the grouping or the ordering is wrong: separate them, or move the secondary action
  away. An explanatory word belongs in the caption line or an accessibility label, never as the
  thing that makes a control unambiguous.
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

### 11.1 Rules that always apply

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

The rest of this section is about **language tolerance**: what a string, a box or a keystroke costs in
a language other than the one the surface was drawn in. A rule marked as a recorded gap in §11.7 is a
gate for new and changed work, not a licence to leave an old surface as it is.

### 11.2 Wrapping, minimum widths and truncation

Every writing system has an atom, and the atom decides how narrow a box can get. English's atom is the
word: variable-length and unbreakable, so a box stops shrinking at its longest word (`min-content` of
"Internationalization"). CJK's atom is a single character, so the same box keeps going and a string's
width comes close to a linear function of its character count. A design system built for the first
case fills up with `min-width`, `max-width` and truncation policy — none of it free, and all of it a
workaround for one writing system.

- **Wrap before you truncate.** Let a label take the lines the string needs, and let the row, the list
  and the pane grow to hold it. A truncating row is exactly as tall in German as it was in English
  because it is not absorbing the extra length — it is throwing it away, and height is the only thing
  a row has to spend.
- **State a minimum width as a visual width.** A hard-coded width must be a measured (or
  measured-then-rounded) point value — `72–280pt` for menu titles, `160pt` for the onboarding status
  column — never a character count. `ch` is the advance width of `0` and `ex` is the x-height: both
  are defined against Latin letterforms, and both are wrong for CJK.
- **Never let a hard-coded width be the only thing holding a label.** Where the string is the only
  content, the container derives from the measured string, not the other way round.
- **Do not assume the platform breaks CJK on word boundaries.** AppKit's public strategies are
  `none`, `pushOut` (avoids an orphan on the paragraph's last line — the proportions half),
  `standard`, and `hangulWordPriority`, which is word priority for **Korean** and has no Chinese or
  Japanese equivalent; the default also differs by field kind (non-editable/selectable text fields use
  `standard`, editable ones use `none`), and we set no strategy of our own. Nothing therefore
  guarantees that a Chinese word is not cut. What we control is the space: the width and the line
  count the string needs, never a character count that decides where the cut lands (§11.5). Where a
  cut word would change the reading — a short label, a heading — read the rendered result in that
  locale instead of assuming it.

**Running text (§11.4) cannot get the same treatment today.** The text system will not do the
dictionary half for us: no strategy gives Chinese or Japanese word priority, and we set none. That is
not the same as impossible. Apple publishes word segmenters for Chinese and Japanese
(`CFStringTokenizer`, `NLTokenizer`), and both layout-manager delegates — TextKit 1's
`layoutManager:shouldBreakLineByWordBeforeCharacterAtIndex:` and TextKit 2's
`textLayoutManager:shouldBreakLineBeforeLocation:hyphenating:` — are asked to allow or prevent each
candidate soft break, so a segmenter could steer break points without pre-inserting separators.
Whether that is safe and affordable in these views is unverified, which makes this a recorded gap
rather than a platform limit.

**What stands in until that gap is closed.** Running text keeps its container's full content width, and
a narrower measure for body copy needs a reason rather than being a default. A long CJK paragraph is
read in the rendered UI at **each width its container can take**, not assumed to be fine; today that is
one width per surface — the settings window pins its own width with min = max, and the panels are
fixed-size — but a container that becomes width-resizable brings its whole range into the check. The
one place we already break lines ourselves — the clipboard's code soft wrap, which prefers a structural
break (comma, operator, member access, whitespace) and falls back to an arbitrary character boundary —
is a monospaced-code model and must not be reused for prose, which would need a segmenter, not a column
count (§11.5).

**Documented deviation — the fixed settings grid.** Read literally, the rule above would make every
settings row grow to hold its longest translation. This app keeps the settings grid instead (52pt
rows, 32pt controls, §5) and truncates, because density is the product. The deviation is bounded:

- only what §9's truncation allow-list permits may truncate; a label the user has to read may not;
- anything that truncates exposes its full value — as a tooltip, or in the caption line;
- when a localized label does not fit, the fix is a wider column, a shorter string or a moved detail.
  Never a smaller type size, and never "it truncates, that is the policy".

### 11.3 Hierarchy has to exist in the content language

Latin letterforms encode rank inside the glyph: case gives three levels for free, then italic, weight,
small caps and letterspacing. CJK has weight — the weakest of them, because bolding a dense glyph
fills its counters instead of adding contrast against the surrounding whitespace — and no case at all.
Where a Latin design signals importance with case or italic, a Chinese build has nothing left to
signal it with.

- Build hierarchy only from the channels the content language grants. §4 fixes the allowed set: size,
  weight, color. No case, no italic, no small caps, no meaningful letterspacing.
- When those channels run out, prefer enclosure and spacing over adding another color. A dense screen
  that looks busy is usually a screen whose hierarchy had to be bought with color.
- Never let a channel that disappears in translation carry meaning on its own.

### 11.4 Density is spent in chrome, not in running text

CJK is denser per character, and density buys area, not time: measured reading rates converge across
languages (around 39 bit/s), so a dense script delivers the same meaning in less space rather than
faster. The area advantage is real in chrome and close to nothing in running text, because dense
glyphs carry their distinguishing detail in interior strokes at high spatial frequency (未/末,
己/已/巳) and need more pixels before they resolve at all.

- Spend the density advantage in chrome: row labels, sidebar items, section headers, keycaps, card
  captions, footer text.
- Do not bank it in running text. Onboarding body copy, release notes, error explanations and
  clipboard detail keep their size and their full line height; a screen that fits more controls in
  Chinese does not get to compress a paragraph.
- 12pt is the floor for every locale, not only for the Latin ones. See §4.

### 11.5 Direction, units and input

- **Mirror the relationship, not the position.** Anything that encodes sequence mirrors — back
  arrows, progress, sliders, step flows; anything depicting a convention or a physical object holds
  still — clocks, playback controls, checkmarks. Numbers never mirror: Arabic numerals stay
  left-to-right inside a right-to-left run, so one line can carry two directions at once and cursor
  movement and selection stop meaning what you assumed. No RTL locale ships today, so new surfaces use
  logical leading/trailing instead of left/right, and the audit stays cheap when one arrives (§11.7).
- **Distrust character counts.** Widths come from measurement or from a measured constant (§11.2).
  The same caution applies away from layout: never derive a truncation budget, a validation limit, a
  sort order or a search decision from a character count — `len()`, `toUpperCase()` and "this field
  fits 24 characters" all assume the language they were designed in. Length limits on user input count
  grapheme clusters, and a counted budget is shown as a hint, never enforced as a silent cut.
- **Price the input.** Eight Latin keystrokes produce eight glyphs; six pinyin keystrokes produce a
  candidate menu that makes the user look away from the sentence to judge and pick one. So: text entry
  goes through the platform text system and never consumes Return while an input method may be
  composing; CJK search matches on the string itself, not on a Latin tokenization of it; and no
  surface assumes typing is equally fast for both users.

### 11.6 Voice

Geometry has coordinates in every language; voice has none. Register, idiom and rhythm do not survive
translation, and the stronger the voice, the more it loses.

- **Split strings in two.** *Functional* strings — labels, field names, error causes, settings rows,
  permission states, button titles — are translated and held to the rules above. A string that would
  embarrass you coming back flat is *written fresh* in each locale instead of translated.
- **Our voice is carried by wording, not by type.** Every surface is drawn in the system face, so a
  translated build cannot silently lose a display font's missing CJK cut. Do not introduce a display
  face for a localized string.
- **Round-trip the loudest strings** — `en → zh-Hans → en` and `en → zh-Hant → en` — when a surface
  introduces a new voice, and read what comes back in a language you can judge. Chinese is two shipped
  locales, not one, and neither can be read off the other: Traditional is *not* uniformly longer (in
  the shipped strings it is longer in fewer than a tenth of the keys, equal in most, and shorter in
  some), but where it is longer it is often a short UI string that doubles via 應用程式 — `应用` →
  `應用程式`, `立即重启` → `立即重新啟動` (a button title). Measure the locale; do not assume which one
  overflows. If you cannot read the locale the product ships into, you cannot feel what arrived.

### 11.7 Recorded gaps

Three rules above are not yet satisfied by every existing surface. They are gates for new and changed
work; leaving an old surface as it is needs a reason, not silence.

- **Per-locale label fit.** Nothing asserts that every settings row label, button title and section
  header fits in `zh-Hans`, `zh-Hant` and the long-text fixture without truncating; the §11.1
  pseudo-locale rule is verified by eye. The assertion belongs with the next change that touches such
  a row.
- **RTL.** No RTL locale ships and nothing is mirrored; §11.5 states the forward rule.
- **CJK word-boundary breaking.** The platform gives us no CJK word priority and no breaker parameter.
  A segmenter (`CFStringTokenizer`, `NLTokenizer`) plus a layout-manager delegate could supply one, but
  whether that is safe and affordable in these views is unverified; until it is, §11.2's per-width
  reading rule is what we have.

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
- [ ] Every new or changed label, button title and section header fits in `en`, `zh-Hans` and
      `zh-Hant` without truncation, and nothing truncates that is not on the §9 allow-list.
- [ ] No new width, truncation budget or input limit is derived from a character count (§11.2, §11.5).
- [ ] New hierarchy uses size, weight and color only — no case, italic or letterspacing carries
      meaning, and no size below 12pt is introduced (§4, §11.3).
- [ ] New text entry leaves Return and unhandled commands to the platform text system, so an input
      method can compose (§11.5).
- [ ] Loud new strings are round-tripped; voice strings are written fresh, not translated (§11.6).
- [ ] New CJK copy was read in the rendered UI — short labels at every supported width, a long
      paragraph at each width its container can take — and not assumed to break on word boundaries
      (§11.2).
- [ ] Cards use border, not a large shadow; elevation level matches what the surface floats over.
- [ ] Radius comes from the three-step scale; derived radii are computed, not hardcoded.
- [ ] Animations use the two durations and the standard curve, and honor Reduce Motion.
- [ ] New code paths are covered by the appropriate tier-A test (see `AGENTS.md`).
