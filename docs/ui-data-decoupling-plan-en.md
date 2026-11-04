# Separating the data from the rendering

- Status: P0-P4 implemented; all eight settings pages are described (Mouse and Key Bindings included), with page hooks (before_page/extra_row/after_page) covering the controls a description cannot carry. **M1 is green on a clean checkout outside the repository, off any symlinked path** (HEAD 1b29023: selftests, fmt, check, clippy, release-check, cargo-test and m1 all PASSED; the red selftests inside the nested worktree are worktree placement, not this branch -- `docs/review-backlog.md` entry 78). **M2 has been run and compared against master row by row** (same machine, same config, `--full` plus all four disturbance switches): this branch's M2 failures are `toast_bubble_smoke` and six e2e scenarios, and `toast_bubble`, `finder-quick-actions`, `panel-action-keys`, `panel-contrast`, `panel-edge`, `scrollbar-style-flip` and `settings-open-latency` **fail on master (e8ded57) too**; `hud-settings` and `panel-drag` fail on master and pass here. Both `settings_layout_smoke` runners **fail on master and pass here** (they were red on a slider-placement bug this branch fixed). Pre-handoff equivalence check: all eight pages were compared against master page by page and every control row's position matches row for row.
- Date: 2026-11-03
- Task key: `ui-data-decoupling`

## Scope

This plan answers two questions: **where the data and the rendering in oh-my-tab should part**, and
**whether that calls for a crate**. It covers the structure of four user-visible surfaces (the settings
window, the switcher overlay, the clipboard panel and the onboarding guide). It does not cover the
inside of the clipboard encryption, window collection, thumbnails or the input taps.

No user-visible behaviour changes. Every phase is accepted on "the same surface, the same numbers"; a
change that alters the interface is outside this plan.

## Summary

1. **The coupling is "the control is the state store" and "creation is computation", not "the view
   reads the config".** The configuration layer is sound: one `LazyLock<RwLock<Config>>`, per-field
   fallback, a real `ChangeFlags` delta. What breaks down is that a config value copied into an
   NSControl becomes a second source of truth, and the field is then found again through a pointer.
2. **It can be separated, and four bounded moves do it** — not one refactor: `RowSpec` per page with that
   page's control table, the overlay's main-thread layout state collected into a struct, `plan_card`
   extracted as a pure function, and the theme's inputs turned into parameters.
3. **Do not extract a UI crate now** — not because it buys nothing (it does buy a compile-time dependency
   constraint and a visibility boundary), but because there is no reuse demand today that is worth its cost:
   the renderer has one consumer, the module boundary already suffices, and `ffi`, `theme` reading a
   global, `ui_coordinator`'s cross-surface calls and the gate's single-crate assumptions would all have
   to be handled first. What is worth a crate is the **view data**, once it has settled.
4. **A hard prerequisite**: the 14 `CONFIG.read()` calls in `theme.rs` must become one complete snapshot
   taken once per layout or refresh transaction, with the config lock released before any AppKit work. This
   step alone fixes a recorded test-parallelism defect.

## Current state and the evidence

These are the facts the plan rests on; every row has a command that reproduces it.

| Fact | Number | Reproduce with |
| --- | --- | --- |
| Control-pointer fields in `SettingsUi` | 114 `*mut AnyObject` | `sed -n '/^pub(super) struct SettingsUi {/,/^}/p' src/settings.rs \| grep -c '\*mut AnyObject'` |
| Control identity by linear sender comparison | 44 `m(u.x, Field)` arms | `sed -n '/unsafe fn control_field_of/,/^}/p' src/settings/dispatch.rs \| grep -c 'm(u\.'` |
| Control-to-semantic mapping | 65 `ControlField` variants, 78 apply arms | `grep -c '^    [A-Z]'` on the enum body; `grep -c 'ControlField::'` inside `apply_control_field` |
| Value copying in `load_settings_from` | 42 `msg_send` for 48 `cfg.` references | `sed -n '/fn load_settings_from/,/^}/p' src/settings.rs \| grep -c 'msg_send!'`, and `grep -c 'cfg\.'` in the same region |
| Mixture in `create_card_view` | 87 `msg_send` and 4 `CONFIG.read()` in 433 lines | `sed -n '/pub(crate) fn create_card_view/,/^}/p' src/overlay/cards.rs \| grep -c 'msg_send!'` |
| Global reads inside `theme.rs` | 14 `CONFIG.read()` | `grep -c 'CONFIG.read()' src/theme.rs` |
| Overlay layout and view-state statics | the members are the ones listed in P3's table; no total is repeated here | `grep -n '^static .*Mutex' src/overlay.rs`, checked line by line against P3's table |
| Reads of those statics from the real test module | 0. The `#[cfg(test)]` block at line 412 is a test-only helper, not a test | `awk 'NR>=1143' src/overlay.rs \| grep -c 'THUMB_\|CARD_SIGNATURES'`, which must be 0 |
| `CONFIG` accesses in the tree | 212 reads / 78 writes across 48 files (235 `read()` plus `write()` sites) | `grep -rn 'CONFIG\.read()\|CONFIG\.write()' src --include=*.rs \| wc -l` |

Existing precedents with the right shape (the plan reuses their shape):

- `src/finder_menu/model.rs`: `build(&Config, …) -> MenuModel`, a pure function, zero AppKit, rendered
  in another process.
- `src/key_bindings/table.rs`: `BindingTable::build(...)`, replaced wholesale by its consumer through
  `RwLock<Arc<_>>`.
- `src/settings/page_canvas.rs`: `PageCanvas` holds a page's rows, cards and cursor in one struct — the
  most recent successful consolidation of layout ownership.

The costs already paid (which show the problem is real, not a matter of taste):

- `docs/settings-field-checklist.md` maintains "a new setting touches 10 files in 16 steps" as a
  **checklist**; the `show_app_name_in_cards` case it records (steps 13 and 14 missed) is an instance of
  exactly this defect class.
- `src/runtime_config.rs` and `src/settings/dispatch.rs` contain **source-scanning tests**
  (`include_str!`, `fs::read_to_string`). A suite asserts text only when there is no value to assert.
- `docs/review-backlog.md` records that `theme`'s geometry tests must read the global `CONFIG` and
  therefore cannot run in parallel.

## Goals and non-goals

Goals:

1. Make what each surface looks like a value that can be constructed and compared; the renderer's job
   becomes applying it.
2. Make "which control maps to which field" data produced while building, not a hand-maintained pointer
   comparison table.
3. A narrowed goal (narrowed again in round 2): **every declared interactive row must carry a valid
   identity**, and the description, control kind and field identity have one source — so "a control was built
   without an identity" is impossible at compile time. It does **not** guarantee that "a config field has no
   row at all": that needs a completeness list (field set against row set), which this plan does not provide;
   it would be a separate step.
   Nor does it promise the whole config-to-UI chain: the in-place refresh runs in a different function
   (`refresh_switcher_keystroke_and_mouse_controls_from_config`, `src/settings/dispatch.rs:1946`) with its
   own trigger (`needs_in_place_control_sync`). This plan only requires both to **consume one pure update
   description** (see P2), and asserts "a field change triggers the sync" and "the update description
   contains that field" separately.

Non-goals (explicitly out of scope, to bound the work):

- Do not remove the `CONFIG` global. Reading it is not the problem, and removing it would disturb every
  FFI signature.
- Do not rewrite `runtime_config::apply_config_change`. It is a 290-line function with ~35 flags and 21
  call sites, but it is the delta layer; rewriting it before a model exists has no basis.
- Do not change any visible appearance, size, copy or interaction.

## Phases

Each phase is independently reviewable, revertible and shippable. Later phases depend on earlier output,
but stopping after any phase leaves the repository complete and releasable.

### P0 — pass the theme's geometry and palette in as parameters

What: replace the 14 `CONFIG.read()` calls in `theme.rs` with one **complete snapshot**, `ThemeInput`,
passed by the caller. Its fields are every input those functions read today, not a convenient subset: the
dark flag, the custom palette (every colour field of `colors.light` and `colors.dark`), the corner radius,
the text scale, the `fonts` fields (card text size, footer size) and the `layout` fields (thumbnail switch
and scale, `show_app_name_in_cards`, `show_footer_title`). The first implementation step is to list, one by
one, the fields each `CONFIG.read()` in `theme.rs` reads and to put that list in `ThemeInput`'s doc comment:
**an incomplete list means P0 is not done.**

`colors_from_config(dark)` still reads the global itself (`src/theme.rs:620`); this phase turns it into
`colors_from(&ThemeInput, dark)`. It is not a precedent — it is one of the things this phase changes.

Consistency: one layout or refresh transaction takes **one** snapshot, releases the config lock, and only
then does AppKit work. This rule also fixes the `docs/review-backlog.md` entry about geometry tests reading
the global `CONFIG` across calls — once the input is a parameter, the tests no longer need the global.

Transition: `ThemeInput::current()` is the **only** transitional entry point, and its exit condition is
narrow — **only the pure theme, layout and description layers are barred from reading the global**. It does
not require every UI module to stop reading the config: the settings event handlers and refresh entries
legitimately need business config, and that is consistent with the plan's "keep `CONFIG`, decouple
gradually" scope. What replaces `current()` is **each entry taking one snapshot when its transaction
starts** (the settings refresh entry, `show_overlay`, the clipboard panel opening, the onboarding render),
and passing it down.

On using a literal count as the boundary check: `grep -c 'CONFIG.read()'` **cannot be the criterion** —
`text_scale()` in `theme.rs` already splits `CONFIG` and `.read()` across lines, and
`page_builder.rs` has four `crate::config::CONFIG` sites with the same split. If an automated check is
kept, it must cover the multi-line form and the `crate::config::CONFIG` form at least, and prove with those
counter-examples that it cannot pass by accident; otherwise the implementer checks `theme.rs` line by line
and the tests assert the dependency relation only.

How it is verified: **not** by "two calls return the same numbers" — the text scale and the corner radius
legitimately change card geometry. The correct invariant is the dependency relation: changing one field of
the snapshot changes **only the outputs the document lists as its consumers**, and every other output stays
bit-identical. The second assertion is before/after equivalence: a snapshot built from the current `CONFIG`
must make the key geometry functions (`card_w`, `card_h`, `footer_band_h`, `card_title_font_size`,
`status_bar_text_size`) return what they returned before the change. That assertion is written and made to
pass **before** the migration starts, as its baseline.
### P1 — settings pages: a `RowSpec` description, laid out by `PageCanvas`

What: split the "read value + compute frame + create control + bind callback + record search index"
sequence in the six `build_*_page` functions into two stages:

```text
page_view(&PageInput) -> Vec<RowSpec>                            // pure: semantics only, no coordinates
     ↓
layout_rows(&[RowSpec], &LayoutInput) -> Vec<RowLayout>          // pure: the only layout computation
     ↓
render_page(&mut PageCanvas, &SettingsUi, &[RowLayout])          // applies the result only
```

`LayoutInput` is the **complete** layout input, not just font metrics: the content width, the page viewport
height, the page insets (`SettingsLayout`'s `label_x`/`label_w`/`control_x`/`control_w`/`row_h`),
`MeasuredText` (each label's measured size in the current language and text size) and page constants such as
`SETTINGS_DETAIL_INSET`. These are exactly what the existing `SettingsPageBuildContext`
(`src/settings/page_builder.rs:6`) already uses; this phase collects them into one type and hands them to a
pure function.

`RowSpec` carries: its card (section), its label key and description key, the control kind, its **field or
action identity** (see P2), the initial value, the option list, its dependence (which master switch
disables it) and its search-index entry.

**The complete set of page inputs is `PageInput`**, not a bare `Config`: it holds at least the `Config`, the
`ThemeInput` and **the currently selected mouse device**. The last one cannot be dropped — under one config,
switching device changes the scroll, acceleration and other control values (the existing code reads
`current_selected_device()`, `src/settings.rs:1022`, and `mouse::resolve::resolve_from_config` resolves that
device's effective settings). So the device identity (or the resolved device config) is an explicit input of
`page_view`, and the update description uses the same `PageInput`; otherwise the function can only read a
global behind the caller's back or show another device's values.

On coordinates (settled after two rounds): the first draft's "coordinates are not pure" was wrong —
**measure first, then hand the measurement to a pure layout function** is pure. Round 1's fix — "the
`RowSpec` carries definite frames" — overshot: putting semantics and layout results in one type forces
`page_view` to know both the config and the viewport. The final shape is **three types and one layout
owner**: `RowSpec` (semantics, no coordinates), `RowLayout` (frames, produced by `layout_rows`), and
`PageCanvas` (applies them). That matches the division `theme::ThumbFlowLayout` already makes.

Order: the **Switcher** page first, then General, Mouse, Clipboard, Key Bindings, About, Finder Menu,
Keystroke Display.

**One page at a time, migrated completely, never half-wired**: each page completes description → build →
callbacks → `load_settings_from` refill → the `needs_in_place_control_sync` in-place refresh in a single
change, and only then is that page's old construction path deleted. There is no "describe now, callbacks
later" intermediate version — that was the first draft's defect: it made P1 and P2 separate phases while P2
admitted that P1 depended on it, a cycle. So **P2 is not a separate phase; it is part of each page's P1
delivery**, and the section below exists only to state the mechanism.

How it is verified (M1, pure logic): given a fixed `PageInput` (device included) and `LayoutInput`,
`page_view`'s row count, copy keys, control kinds, field identities and dependency relation are asserted, and
`layout_rows`' per-row frames are asserted separately (they answer different questions). One more for the
Mouse page: **one `Config`, two devices with different profiles** must produce different initial values. Real
refills after a device switch or unplug are M2. The real AppKit target/action wiring and the geometry in three languages are not M1: they belong
to that page's existing **M2** runner (`--smoke-settings-layout` and friends). This plan adds no M2, but a
page whose M2 was not run because authorization was not given must be reported as NOT RUN.

### P2 — control identity produced while building (part of each P1 page)

Mechanism: control identity **comes from the description**, not from the control kind — several switches map
to different fields and a button maps to its own action, so a `RowSpec` carries an explicit `ControlId`
(a field identity or an action identity), not "this is a switch". `render_page` registers each control with
its `ControlId` in a `ControlTable` held by `SettingsUi` as it creates it, and `control_field_of` **asks
that table first**: a described row's control is answered by the table instead of the linear comparison.

**What was actually delivered (verified against the code at hand-off, not against the plan's wording):**
the pointer chain is down to **2 arms**, from 65 when this work started. It converged in two steps: the
first deleted 50 arms that a described row had already registered (the table was answering them before
the chain ran); the second made the controls a page draws through `PageHooks` register their own
`ControlId` as they are drawn (`draw_mouse_row` takes the `PageRegistrations` buffer), which removed the
Mouse page's scroll-mode, line-count, tracking-speed and five smooth-scrolling arms -- and the same
review showed the Finder Menu rows are described rows and the clipboard entry limit dispatches through
`text_field_of`, so those five went too.

The two arms that remain are not migration leftovers: they name **two fields no page builds any more**
(`ShowFooterTitle`, `CornerRadius`), whose `SettingsUi` slots are never assigned, so the arms can never
match. The fields and their variants stay in `ControlField`; deleting them reaches into `settings_ui_slot`
and is its own change. See `docs/review-backlog.md`.

So: **on all eight pages every control -- described or hook-drawn -- is identified by the `ControlTable`.**

Lifecycle rules (missing from the first draft and required):

- **Register and unregister in pairs**: rebuilding a page unregisters that page's entries before
  registering again, and a destroyed control unregisters its entry.
- **A duplicate registration aborts that page build**: registering the same `ControlId` or the same pointer
  twice panics in debug; in release it logs and **discards the build**, falling back to the last usable page
  state and keeping it visible. It must **not** log and then finish publishing controls — a control that
  displays correctly but does not respond is exactly the "looks alive but is not wired" defect this plan
  exists to remove.
- **All three destruction paths unregister first**: a local replacement (one row rebuilt), a whole page
  rebuild and a whole window teardown all unregister **before** releasing the old controls, and repeated
  cleanup must be safe (idempotent).
- **Failure fallback does not conflict with unregister-first**: identity validation (a duplicate, a missing
  identity) happens **before** the old page or its registrations are disturbed; when the new build fails,
  the old views and the old mapping are kept **together**, so one cannot vanish while the other stays. A
  first build has no previous state to fall back to, and its behaviour is definite: the page stays empty and
  the failure is logged, with no control published and none responding — better empty than "looks alive but
  is not wired". An M1 assertion covers it: after a failed registration the old mapping still resolves the
  same `ControlId`s.
- **An unknown sender keeps today's fallback**: a miss in the `ControlTable` behaves as the code behaves
  now; nothing is silently dropped.
- **Dynamic rows stay out of this table**: the Key Bindings rows and the Mouse mapping rows have their own
  rebuild and cleanup responsibility (`ROW_CONTROLS` in `settings/bindings.rs`); this phase covers the
  **static config controls only**. That is also the answer to first-draft open question 1: `RowSpec`
  describes the fixed furniture only.
- **The other pointer-keyed global tables are not merged**: the tables in `select.rs`, `tooltip.rs` and
  `sidebar.rs` do different jobs (popover state, tooltip targets, the search index); "the key is a pointer"
  is not a reason to merge them. First-draft open question 2 is closed on that basis.
- **Transient invalid input and recording drafts stay**: a half-typed number field and a shortcut capture
  draft are edit-time state, not a second source of truth, and they stay where they are.

Note: the meaning of `SettingsButton.tag` is unchanged (it still selects the hover/normal palette); no
semantic id goes in a tag.

How it is verified (M1, pure logic): the `ControlTable` is unit-tested with stand-in pointer values —
register, unregister, duplicate refused, the cleanup order and idempotence of the three destruction paths,
unknown sender, no stale entry after a page rebuild. The real `SettingsUi` pointers, target/action bindings
and real cleanup paths are covered by M2 and reported NOT RUN when unrun.

**The refill and the in-place refresh consume one pure update description.** The first draft treated
"compare `load_settings_from` with `page_view`'s initial values" as covering steps 13 and 14, which does not
hold:

- `load_settings_from` performs AppKit work and **cannot** be called from M1;
- the in-place refresh runs in a different function
  (`refresh_switcher_keystroke_and_mouse_controls_from_config`, `src/settings/dispatch.rs:1946`) with its
  own trigger (`needs_in_place_control_sync`).

So both now take "field → value" from one **pure update description** (`Vec<ControlUpdate>`, computed from
`page_view` and `Config`), and M1 asserts three things: (a) the update description's (field, value) sequence
for a given `PageInput` is right; (b) a field change makes `needs_in_place_control_sync` true (a pure
function whose existing tests can be extended); (c) every static state control that needs a config refill
appears in the update description. Action controls (buttons such as "export log") have no config field to
refill; they are verified for identity and target/action wiring separately, rather than being forced to
produce meaningless update entries. The two real write paths are asserted by M2. This needs no change to `apply_config_change`'s
dispatch structure.

**A shared source is not unconditional full execution.** The in-place refresh keeps its existing rules: a
value being edited, a shortcut capture draft, the lazy fill of a page that has not been shown
(`MOUSE_PAGE_DIRTY`) and the skip logic that applies only to the visible page all stay. What is shared is the
**computation** of "field → value", not the policy of "write everything every time".
### P3 — collect the overlay's layout state into `AppState`

What: move the layout-result statics and the scroll/hover/drag statics in `overlay.rs` into a
`SwitcherLayout` (holding `ThumbFlowLayout`, `ScrollState`, `HoverState`, `RenderCache`) carried on
`AppState`. `MainThreadSlot` keeps only what actually holds an ObjC pointer; the two coalescing flags and the
`CARD_SIGNATURES` / `LAST_APPLIED_THEME_DARK` caches stay where they are, for the reasons in the table.

| Category | Statics | Destination |
| --- | --- | --- |
| Layout results | `THUMB_VISIBLE_RANGE`, `THUMB_ROW_RANGES`, `THUMB_MAX_ROWS`, `THUMB_CONTENT_INSET`, `THUMB_PANEL_MAX_H`, `THUMB_TEASER_FITS`, `THUMB_SCROLL_MAX_OFFSET`, `THUMB_DOCUMENT_HEIGHT`, `THUMB_SCROLL_ROW_PITCH`, `THUMB_CAPTURE_TARGET_PX_H` | `SwitcherLayout.flow` |
| View state | `THUMB_SCROLL_ROW`, `THUMB_SCROLL_OFFSET`, `THUMB_SCROLL_DRAG`, `THUMB_SCROLLER_HOVER`, `THUMB_NAV_ANCHOR_X`, `MOUSE_MOVED`, `PENDING_CARD_CLOSE` | `SwitcherLayout.scroll` / `.hover` / `.close` |
| Kept | `OVERLAY_WINDOW`, `CONTAINER`, `CARD_DOCUMENT`, `STATUS_LABEL`, `THUMB_SCROLLER`, `OVERLAY_BACKDROP`, `CARD_CLASS` | stay in `MainThreadSlot` / the class cache |
| Kept | `CARD_INDEX_MAP`, `CARD_KEY_MAP` | view-pointer-to-data maps serving the dynamic ObjC classes (AGENTS.md forbids reading Rust properties through `msg_send!`) |
| Kept | `CARD_SIGNATURES`, `LAST_APPLIED_THEME_DARK`, `DISPLAY_RECONFIG_REFRESH_SCHEDULED`, `DISPLAY_RECONFIG_RELAYOUT_PENDING` | render caches and coalescing flags, not layout decisions; moving them into `AppState` would grow it without adding assertability |
| Kept (cross-thread channel) | `CARD_CLOSE_AX_RESULT` | written from a worker (`overlay/card_close.rs:415`, inside `std::thread::spawn`) and taken by the main-thread callback (same file, :464). It is the hand-off slot between the main thread and the AX close worker, not layout state; putting it in the main-thread-only `AppState` would break that thread boundary. Only the close-animation state **after** the main thread consumes it lives in `PENDING_CARD_CLOSE`, and that part moves with `.close` |

**The borrow and reentrancy boundary (missing from the first draft; this is the key risk of the phase):**
`with_tab_state` borrows `RefCell<Option<AppState>>` mutably (`src/lib.rs:182`), and `refresh_highlight`,
`show_overlay` and friends **re-enter** it (`overlay/cancel.rs:503`, several sites in `cards.rs`). Carrying
the layout on `AppState` therefore requires one hard rule:

> Take a short borrow, read the plan or update the state, **release the borrow**, and only then run AppKit
> work and the callbacks it triggers.

Any code that holds a `state.layout` borrow across a `msg_send!`, `refresh_highlight`, `show_overlay` or
`thumbnail::*` call is a reentrancy panic. `AppState` is chosen over a separate `thread_local` because of the
access boundary (it is already tied to the main-thread convention and `with_tab_state` is the only legal
entry), **not** because of struct size; a separate TLS does not solve reentrancy, it relocates it.
`CARD_CLOSE_AX_RESULT` is the cross-thread hand-off slot and stays where it is (see the table).

Why this is the safest block: the real test module in `overlay.rs` never touches these statics (the
`#[cfg(test)]` block at line 412 is a test-only helper that does). The two existing accessors
(`thumbnail_visible_range`, `thumbnail_scroller_max_offset`) keep their signatures and read the struct.

How it is verified: an M1 assertion that, from one `ThumbFlowLayout` and one pending close, the reflow budget
matches "the layout before the close plus the item count after it".

The borrow boundary has **no** type-system guarantee, and the first draft was wrong to claim one: the outer
closure's `RefCell` borrow can stay alive while an inner function calls `with_tab_state` again; that compiles
and then panics at runtime, and returning a snapshot does not end the outer closure. `snapshot()` is still
worth providing — it turns a read into the result of one short borrow — but its job is **making the rule easy
to follow**, not **making a violation fail to compile**. This phase therefore keeps the short-borrow rule,
checks the actual call boundaries one by one, and adds a regression assertion covering "a nested entry into
`with_tab_state`" panics rather than corrupting state silently (the same treatment `lib.rs` gives
`assert_main_thread`). Note that not every `msg_send!` or `thumbnail::*` call is necessarily a re-entry — only
those that re-enter `with_tab_state` are; list them by call site rather than saying it generally.

### P4 — `plan_card` as a pure function, shared by creation and refresh

What: extract the internal frame computation from `create_card_view` (433 lines) into
`plan_card(card_w, card_h, layout, theme) -> CardPlan`, returning the frames of the close button, the badges,
the title, the preview and the selection ring; **both** `create_card_view` and `refresh_highlight` consume
that one `CardPlan` rather than each computing its own and comparing.

Why "test that the two agree" is not enough: keeping two formulas and asserting they are equal freezes the
drift as a contract. The correct move is for `refresh_highlight` to take its geometry from the `CardPlan`
(or the plan stored when the card was built) and delete the two places where it re-derives them from
`[sv frame]` (`overlay/cancel.rs`).

How it is verified: an M1 assertion that `plan_card` produces definite frames for a given card size, layout
and theme, and agrees with `theme::icon_card_layout`, `thumb_caption_h_for_card` and `thumb_preview_h` in both
layouts. "Creation and refresh share one source" is guaranteed by the code structure (the refresh takes its
geometry from the plan), not by asserting two formulas are equal.
### P5 (optional, not required by this plan) — the view data as a crate

**This is a re-evaluation trigger, not an output of this plan.** The conditions match Decision 1: any one of
them, not all of them, and a second consumer is not required.

1. a second consumer appears that must share the same view data (the Finder extension rendering the same
   settings description, for example);
2. the module boundary has settled and the team wants the compiler to **enforce** the dependency direction
   (that can be a reason on its own);
3. a view-consistency check is needed without starting AppKit.

Two hard constraints also apply when evaluating: `RowSpec`/`CardPlan`/`SwitcherLayout` have settled (P1–P4
done, and at least one later settings change did not force them to change), and `theme` no longer reads the
config itself (P0 done).

What would be extracted is `oh-my-tab-view` (pure data, depending only on `serde` and the like, no `objc2`),
not `oh-my-tab-ui` (rendering). The repository's only successful crate extraction,
`crates/finder-protocol`, extracted a **data contract**.
## Decisions and their reasons

### Decision 1: no UI crate for now (correcting the first draft's overreach)

**The first draft's "it buys nothing" and "four unavoidable costs" are both overstated and are withdrawn.**
The accurate version: a crate **does** buy two things — a compile-time dependency constraint and a
visibility boundary. An internal workspace member's `pub` is **not** a permanent external compatibility
promise: this repository already has several members, and the root app continuing to host integration smoke
does not prove a split is worthless. So the reason is not "impossible" or "useless" but **there is no reuse
demand today worth this cost**:

1. **There is no second consumer that must share the model.** `crates/finder-protocol` exists because the app
   and the sandboxed appex are two binaries that must share one set of types; the rendering layer has one
   consumer. "The compiler must enforce the dependency direction" is also a reason in its own right — but
   there is nothing worth enforcing while the very boundary is still moving.
2. **The module boundary already suffices.** The pure function plus description that P1–P4 need can be
   expressed in one crate and asserted by M1, as `theme::icon_card_layout`, `key_bindings::table` and
   `finder_menu::model` already show. A crate boundary adds **enforcement**, not **capability**, and there is
   nothing worth enforcing while the module boundary is the thing still moving.
3. **The cost depends on the current shape, but it is real.** `ffi.rs`'s `ObjPtr`/`MainThreadSlot` are used
   by 40 files; `theme.rs` reads `CONFIG` (gone after P0); `ui_coordinator.rs` calls each surface's internal
   refresh functions; and the gate's `cargo-test.sh --require-match` and smoke-runner derivation assume one
   crate. None of these is a wall — they are simply not worth paying today.
4. **The ordering argument still holds.** A boundary first and pure functions later leaves implementations
   that still read globals behind `pub` wrappers. Only after the descriptions and pure functions exist can
   the boundary be drawn where it belongs.

The benefit is real. The triggers to re-evaluate (a second consumer is not the only one): a second consumer
appears; **or** the module boundary has settled and the team wants the compiler to hold the dependency
direction; **or** a view-consistency check is needed without starting AppKit. Then extracting a crate is the
right next step — and what it extracts is the **view data** (the
`finder-protocol` shape), not the rendering code. There is no basis for extracting an `ffi` crate on its own
now: it would only make the renderer depend on a smaller package, and the dependency-direction question is
answered by module rules and P0.

### Decision 2: settings before overlay

Settings carries the heaviest coupling (114 control fields, 44 pointer comparisons, 78 apply arms, a 16-step
checklist); the overlay's layout is partly consolidated in `theme::ThumbFlowLayout` already. So settings first.
This is the reverse of "take the easiest block first", and the reason is assertability: each settings page
migrated adds an assertion that cannot be written today, while the overlay block mostly collects an existing
agreement.

If the reviewer prefers P3 first (zero test dependencies, mechanical, low-risk), the order may be swapped;
the plan depends only on P0 coming first, not on the relative order of P1 and P3. **P1 and P2, however,
cannot be separated** — see P1.

### Decision 3: semantics and layout are two types (settled after two rounds)

The first draft said "`RowSpec` carries no coordinates, because row height depends on font metrics so
coordinates are not pure"; that argument is wrong and has been rewritten. The language and the text size are
render-time inputs, and once they are passed in as `MeasuredText`, the coordinates are computable by a pure
function too. `RowSpec` therefore **carries definite frames**, with `PageCanvas` only accumulating and placing
them; that matches `theme::ThumbFlowLayout` and makes "where each row is" an assertable value for the first
time.
## Revision log

| Round | Verdict | Main change |
| --- | --- | --- |
| 1 | CONCERNS | Six findings, each answered below |

### How round 1's findings were handled

| Finding | Severity | Disposition |
| --- | --- | --- |
| The "a missed link fails to compile" promise has no implementation path | HIGH | **Partly accepted**: goal 3 is narrowed to "description, control kind and field identity share one source, so a missed field fails at compile time". The plan no longer promises the whole chain: `needs_in_place_control_sync` and the in-place refill stay on the checklist, but gain an M1 assertion (P2's "refill matches initial values item by item"). |
| P1 and P2 are circular, contradicting "every phase is independently shippable" | HIGH | **Accepted**: P2 is no longer a separate phase but part of each page's P1 delivery; a page is migrated completely (description, build, callbacks, refill, in-place refresh) before its old path is deleted. First-draft open questions 1 and 2 are closed accordingly. |
| P3 leaves the borrow and reentrancy boundary unstated | HIGH | **Accepted**: P3 gains the hard rule "short borrow to take the plan or update state → release → then AppKit work", states that `with_tab_state` is a mutable `RefCell` borrow and that `refresh_highlight` re-enters it, bases the `AppState` choice on lifetime and access boundary, and treats the cross-thread channel separately. |
| `ControlTable` lacks a lifecycle contract | MEDIUM | **Accepted**: paired registration/unregistration, duplicate refused, unknown sender keeps the fallback, dynamic rows stay out, the other pointer tables are not merged, and transient invalid input and capture drafts are explicitly not a "second source of truth". |
| P0's snapshot and verification conditions are incomplete | MEDIUM | **Accepted**: the complete input list (including the custom palettes and the `fonts`/`layout` fields) is spelled out; `colors_from_config(dark)` is acknowledged as still reading the global and is one of the things being changed; verification becomes a dependency assertion plus a before/after equivalence assertion; `ThemeInput::current()`'s exit is held by an M1 assertion. |
| The M1 assertions cannot prove real wiring or visual equivalence | MEDIUM | **Accepted**: M1 covers the pure description, the identity table's logic and the layout decisions alone; real target/action wiring and three-language geometry belong to the existing M2 runners and are reported NOT RUN when unrun. P4 makes creation and refresh share one `CardPlan` instead of asserting two formulas agree. |
| The case against a UI crate is overstated | MEDIUM | **Accepted**: "buys nothing" and "four unavoidable costs" are withdrawn in favour of "no reuse demand worth the cost today", while acknowledging that a crate does provide a dependency constraint and a visibility boundary and that an internal `pub` is not a permanent promise. |
| Quantitative evidence is inconsistent; some reproduce columns are hints, not commands | LOW | **Accepted**: one set of counts (14 theme reads, 114 `SettingsUi` pointer fields, 18 of 31 overlay statics migrated) and a complete copy-pasteable command per row. |

### How round 2's findings were handled

| Finding | Severity | Disposition |
| --- | --- | --- |
| The narrowed compile-time guarantee is still too strong, and the refill test covers the wrong thing | MEDIUM | **Accepted**: goal 3 is narrowed again to "every declared interactive row carries a valid identity", explicitly not guaranteeing "a whole row is missing" (that needs a separate field-completeness list). The refill becomes one pure update description (`Vec<ControlUpdate>`) consumed by both paths; M1 tests that description, `needs_in_place_control_sync` and coverage, and never calls `load_settings_from`. |
| P3's migration table lists a cross-thread result channel as main-thread layout state | HIGH | **Accepted**: `CARD_CLOSE_AX_RESULT` moves to "kept (cross-thread channel)", because `overlay/card_close.rs:415` writes it from a worker and :464 reads it on the main thread; only the consumed close-animation state (`PENDING_CARD_CLOSE`) moves with `.close`. The migrated count and list are corrected together. |
| `&self` and a snapshot cannot provide the promised reentrancy guarantee | MEDIUM | **Accepted**: the "cannot coexist in the type system" claim is withdrawn; the short-borrow rule stays, call boundaries are checked one by one, and a regression assertion covers a nested entry into `with_tab_state`. Not every `msg_send!`/`thumbnail::*` call is described as a re-entry. |
| P0's exit condition widens the scope, and a literal count cannot hold the boundary | MEDIUM | **Accepted**: the exit condition is narrowed to "the pure theme/layout/description layers must not read the global", boundary entries may take a snapshot, and the replacement is named; `grep -c` is demoted to a helper with the `text_scale()` and `page_builder.rs` counter-examples attached. |
| Putting definite frames in `RowSpec` overshoots | MEDIUM | **Accepted**: three types and one layout owner — `RowSpec` (semantics) / `layout_rows(&LayoutInput) -> RowLayout` (frames) / `PageCanvas` (applies); `LayoutInput` lists the content width, the viewport, `SettingsLayout` and `MeasuredText`. |
| Duplicate registration's release behaviour lacks a build-failure path | MEDIUM | **Accepted**: a duplicate aborts that page build and falls back to the last usable state; the three cleanup paths (local replacement, page rebuild, window teardown) and idempotence are required; M1 covers the table operation sequences and M2 the real cleanup paths. |
| "A second consumer is required" should not be a prerequisite for a split | MEDIUM | **Accepted**: it becomes one of three re-evaluation triggers, acknowledging that "the compiler must enforce the dependency direction" can be a reason in its own right. |
### How round 3's findings were handled

| Finding | Severity | Disposition |
| --- | --- | --- |
| Page-failure fallback conflicts with "unregister the old entries first" | MEDIUM | **Accepted**: identity validation happens before the old page or its registrations are disturbed; a failed rebuild keeps the old views and the old mapping **together**; a first build's failure is definite (empty page, logged, nothing published) rather than a promise to fall back. An M1 assertion covers "the old mapping still resolves after a failed registration". No general transaction framework. |
| The Mouse page's pure description lacks the selected device | MEDIUM | **Accepted**: page inputs become `PageInput { Config, ThemeInput, selected device }`, because `current_selected_device()` (`src/settings.rs:1022`) changes the scroll and acceleration values under one config. The update description uses the same `PageInput`; M1 covers "one config, two device profiles, different initial values"; device switch and unplug refills are M2. |
| The completeness assertion wrongly includes action controls | MEDIUM | **Accepted**: limited to "every static state control that needs a config refill"; action controls ("export log" and the like) are verified for identity and target/action wiring separately. Also stated: a shared source is not unconditional full execution — edits in progress, capture drafts, the lazy page fill and visibility skips stay. |
| LOW — the summary disagrees with the body (10+8 vs 10+7 vs 10+6), and P5 still holds the hard "all conditions" and "second consumer" rules | LOW | **Accepted**: the evidence table and P3 state no totals and defer to P3's member table; P5 now lists three independent re-evaluation triggers with two hard constraints, matching Decision 1. |
### How round 4's findings were handled

| Finding | Severity | Disposition |
| --- | --- | --- |
| P1's example signature still says `page_view(&Config, &ThemeInput)` and P3's opening still says "10 + 8", contradicting `PageInput` and the member table | LOW | **Accepted**: fixed in both twins. `page_view(&PageInput)` in the diagram, P3 no longer states a count and defers to its member table. The reviewer asked for no further design round for this. |
## Questions for the reviewer

- Does this plan miss a decoupling point with a better cost/benefit than P1–P4?
- Do P2's lifecycle rules cover the real control rebuild paths (a `PageCanvas` per-row rebuild and a whole
  `SettingsUi` window rebuild)?
- Is P3's borrow rule sufficient — is there any path other than `with_tab_state` that triggers an AppKit
  callback while a layout borrow is held?
- Is `ThemeInput`'s input list complete? (The first implementation step lists every `CONFIG.read()` in
  `theme.rs` line by line.)
- Is the criterion for not extracting a crate now (any one of the three re-evaluation triggers, plus the two
  hard constraints) clear enough to be judged independently later?
- The reviewer noted that theme's input list and P3's call-boundary list can be the first step of each phase
  rather than enumerated here; this plan takes that position. And a nested-borrow panic test proves a
  violation is refused, not that production paths never re-enter — the borrow range is still checked at
  implementation review.