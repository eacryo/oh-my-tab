# page_view split: what moved where

`src/settings/page_view/mod.rs` went from **5431 lines to 285**. The file is now the module's
surface: the module doc, the `mod` declarations and the `pub(super) use` re-exports that keep every
call site outside `page_view` unchanged.

## Files

| File | Owns | Lines |
|---|---|---|
| `mod.rs` | module doc, `mod` declarations, facade re-exports | 285 |
| `model.rs` | the description types: `PageInput`, `PageView`, `RowSpec`, `ControlKind`, `ControlId`, `Value`, `RowCopy`, `RowLayout`, `PageHooks` | 366 |
| `identity.rs` | `ControlEntry`, `RegisterError`, `ControlTable`, `PageRegistrations` and the lifecycle rules the table obeys | 181 |
| `layout/mod.rs` | measurement: `PageMetrics`, `BoxRect`, `CardRect`, `BannerRect`, `LayoutInput`, `MeasuredText` | 237 |
| `layout/place.rs` | placement: `PlacedRow`, `PageLayout`, `row_height`, `layout_rows`, `page_sequence` | 493 |
| `rows.rs` | `RowWriter`, `RowDraft` and the four row constructors | 283 |
| `update.rs` | `update_description`, `page_update_description` | 55 |
| `render.rs` | the AppKit renderer: `render_row`, `render_page`, `apply_*`, `row_placement_boxes` | 615 |
| `build.rs` | `build_page`, `BuiltPage`, `view_for`, `page_hooks`, the banner frames | 273 |
| `bind.rs` | `bind_page_*`, `refresh_migrated_pages`, the slot predicates | 279 |
| `smoke.rs` | the six M2 runners and their fixtures | 563 |
| `tests/` | shared helpers plus `description`, `identity`, `layout`, `render_build` | 52 tests |

## Visibility

The rule is the item's ABSOLUTE scope, not the keyword it carried before. An item declared directly
in `page_view` was `pub(super)`, which means `settings`; declared in a child module the same keyword
would mean `page_view`. So:

- reachable from `settings` (the page builder, the settings root, the dispatcher) → `pub(in crate::settings)`;
- reachable only inside the `page_view` subtree → `pub(in crate::settings::page_view)`;
- `layout/place.rs` is one level deeper, so `pub(super)` there stops at `layout` and was never used;
- no production interface was widened to keep a test compiling.

## Tests

The 52 tests are the same 52. Their names gained a module prefix, nothing was lost and nothing was
added. Counts were taken from the compiled set (cargo test --lib -- --list), not from a source
scan, so a test behind #[cfg] that stopped running would have shown as lost.

### Correspondence, one row per test

Taken from the #[test] functions of page_view/mod.rs at the commit before the split (e10f104)
and matched against the compiled test list afterwards:

| Test (unchanged name) | Now in |
|---|---|
| `switcher_page_view_describes_every_row_with_its_identity_and_copy` | `tests/description.rs` |
| `general_page_view_describes_every_row_with_its_identity_and_copy` | `tests/description.rs` |
| `page_view_reads_only_its_input` | `tests/description.rs` |
| `page_view_search_entries_are_values_not_side_effects` | `tests/description.rs` |
| `a_migrated_page_indexes_every_row_the_inline_sweep_used_to` | `tests/render_build.rs` |
| `the_description_groups_are_the_ones_the_renderer_registers` | `tests/description.rs` |
| `the_update_description_carries_the_control_value_and_its_display_separately` | `tests/description.rs` |
| `a_failed_publish_leaves_the_previous_mapping_untouched` | `tests/identity.rs` |
| `a_successful_publish_replaces_the_pages_own_entries` | `tests/identity.rs` |
| `every_settings_ui_slot_a_migrated_field_needs_is_bound_at_publish` | `tests/identity.rs` |
| `collapsing_a_group_moves_every_placed_view_with_its_row` | `tests/layout.rs` |
| `the_banner_children_lie_inside_their_strip` | `tests/layout.rs` |
| `every_view_the_renderer_creates_is_released_exactly_once` | `tests/render_build.rs` |
| `every_migrated_row_uses_the_shared_label_association` | `tests/render_build.rs` |
| `a_rows_current_origin_is_what_a_placement_follows` | `tests/layout.rs` |
| `the_banner_strip_is_placed_in_its_final_parents_coordinates` | `tests/layout.rs` |
| `build_page_lays_the_general_banner_out_from_the_page_context` | `tests/layout.rs` |
| `the_separator_belongs_to_the_row_below_it` | `tests/render_build.rs` |
| `a_description_with_a_duplicate_identity_is_refused_before_any_view_exists` | `tests/description.rs` |
| `the_windows_master_switch_keeps_its_dedicated_callback` | `tests/identity.rs` |
| `layout_rows_places_the_switcher_rows_without_gaps_or_overlaps` | `tests/layout.rs` |
| `layout_rows_places_controls_inside_the_row_band` | `tests/layout.rs` |
| `layout_rows_sizes_a_stacked_row_by_its_measured_caption` | `tests/description.rs` |
| `control_table_refuses_duplicates_of_identity_and_pointer` | `tests/identity.rs` |
| `control_table_unregister_and_cleanup_are_idempotent` | `tests/identity.rs` |
| `a_refused_page_registration_leaves_the_old_mapping_intact` | `tests/identity.rs` |
| `publishing_a_page_replaces_its_old_entries_and_no_stale_pointer_survives` | `tests/identity.rs` |
| `the_update_description_matches_each_row_initial_value_item_by_item` | `tests/description.rs` |
| `every_static_state_control_of_a_migrated_page_appears_in_the_update_description` | `tests/description.rs` |
| `a_field_change_makes_the_in_place_sync_needed` | `tests/identity.rs` |
| `every_pages_description_passes_the_identity_check_the_build_runs` | `tests/description.rs` |
| `every_card_and_row_survives_the_layout_for_every_page` | `tests/layout.rs` |
| `every_described_field_of_every_page_has_a_settings_ui_slot` | `tests/identity.rs` |
| `every_field_with_a_dedicated_handler_still_has_one` | `tests/identity.rs` |
| `page_drawn_rows_are_laid_out_at_their_own_height_and_cards_cover_them` | `tests/layout.rs` |
| `the_sequence_places_every_row_exactly_once` | `tests/layout.rs` |
| `every_described_control_has_a_binding` | `tests/identity.rs` |
| `the_canvas_row_mapping_counts_the_page_drawn_rows_of_every_card` | `tests/layout.rs` |
| `every_card_heading_hangs_above_its_first_row_for_every_page` | `tests/description.rs` |
| `a_described_rows_label_box_sits_inside_its_row_band` | `tests/description.rs` |
| `a_captioned_rows_control_is_centred_in_its_band` | `tests/description.rs` |
| `a_page_drawn_only_card_names_a_translated_heading` | `tests/description.rs` |
| `a_captioned_row_states_which_text_block_it_takes` | `tests/description.rs` |
| `a_rows_control_draws_its_own_copy` | `tests/description.rs` |
| `about_draws_its_value_column` | `tests/description.rs` |
| `an_action_button_draws_its_own_copy` | `tests/description.rs` |
| `no_described_row_duplicates_a_page_drawn_action` | `tests/render_build.rs` |
| `a_value_row_reserves_a_box_the_renderer_fits_the_label_into` | `tests/layout.rs` |
| `dbg_switcher_slider_frames` | `tests/render_build.rs` |
| `a_slider_records_its_own_box_not_its_readouts` | `tests/layout.rs` |
| `a_description_reads_only_its_input_and_capture_reads_the_config_once` | `tests/description.rs` |
| `the_applier_never_parses_a_controls_value_out_of_its_text` | `tests/render_build.rs` |


## Source-reading tests

Five tests read the source they check. Each follows its code, and each failed loudly first:

- `every_view_the_renderer_creates_is_released_exactly_once`, `every_migrated_row_uses_the_shared_label_association`,
  `the_separator_belongs_to_the_row_below_it`, `the_applier_never_parses_a_controls_value_out_of_its_text`
  → now read `render.rs`;
- `a_description_reads_only_its_input_and_capture_reads_the_config_once` → reads `mod.rs`, `model.rs`,
  `rows.rs`, `update.rs`, `layout/mod.rs`, `layout/place.rs` and `general.rs`. The split moved the
  description code out of `mod.rs`, so a scan of `mod.rs` alone would have covered the facade and
  nothing else; a probe planted in `model.rs` confirms the widened scan fails as intended.

Two anchors that located a function's end by the NEXT item's text were replaced with brace matching:
they only worked because the two items shared a file, and a match from the neighbour could have
satisfied them.