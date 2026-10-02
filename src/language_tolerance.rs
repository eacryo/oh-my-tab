//! Stable inventory for the language-tolerance plan. Content states are explicit because a single
//! rendering call site can display different semantic text (for example, the overlay footer).

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct Site {
    id: &'static str,
    content_state: &'static str,
    source: &'static str,
    runner: &'static str,
}

#[cfg(test)]
const SITES: &[Site] = &[
    Site { id: "LT1-A01", content_state: "settings-sidebar-item", source: "settings/widgets.rs:make_sidebar_button", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A02", content_state: "settings-page-title", source: "settings/widgets.rs:add_page_title", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A03", content_state: "settings-row-label", source: "settings/widgets.rs:add_row_with_label", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A04", content_state: "settings-caption-row-title", source: "settings/widgets.rs:add_described_row", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A05", content_state: "settings-single-line-caption-row-title", source: "settings/widgets.rs:add_captioned_row", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A06", content_state: "settings-link-title", source: "settings/widgets.rs:make_external_link", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A07a", content_state: "settings-action-button-title", source: "settings/widgets.rs:configure_settings_button_wrapping", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A07b", content_state: "update-prompt-stage-button-title", source: "updater.rs:make_custom_update_found_window", runner: "--smoke-update-prompts" },
    Site { id: "LT1-A07c", content_state: "onboarding-action-button-title", source: "settings/widgets.rs:configure_settings_button_wrapping", runner: "--smoke-onboarding-live-apply" },
    Site { id: "LT1-A08", content_state: "settings-tall-row-label", source: "settings/widgets.rs:add_tall_row", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A09", content_state: "clipboard-count", source: "clipboard/text_style.rs:FOOTER_COUNT_LABEL", runner: "--smoke-clipboard" },
    Site { id: "LT1-A10", content_state: "clipboard-action-key", source: "clipboard/text_style.rs:key_label", runner: "--smoke-clipboard" },
    Site { id: "LT1-A11", content_state: "clipboard-legend", source: "clipboard/text_style.rs:hint_ns", runner: "--smoke-clipboard" },
    Site { id: "LT1-A12", content_state: "keystroke-display-text", source: "keystroke_display/panel.rs:add_badge_label", runner: "--smoke-keystroke-display-panel" },
    Site { id: "LT1-A13", content_state: "onboarding-single-line-label", source: "onboarding.rs:add_label", runner: "--smoke-onboarding-live-apply" },
    Site { id: "LT1-A14", content_state: "settings-tooltip", source: "settings/tooltip.rs:SettingsTooltip", runner: "--smoke-settings-layout" },
    Site { id: "LT1-A15", content_state: "overlay-no-windows", source: "overlay.rs:update_status_label", runner: "--smoke-overlay" },
    Site { id: "LT1-B01", content_state: "settings-select-value", source: "settings/select.rs:settings_select_set_title", runner: "--smoke-settings-layout" },
    Site { id: "LT1-B02", content_state: "settings-row-caption", source: "settings/widgets.rs:add_captioned_row", runner: "--smoke-settings-layout" },
    Site { id: "LT1-B03", content_state: "settings-readonly-value", source: "settings/widgets.rs:make_value_label", runner: "--smoke-settings-layout" },
    Site { id: "LT1-B04", content_state: "clipboard-detail-meta", source: "clipboard/text_style.rs:make_meta_footer_attributed", runner: "--smoke-clipboard" },
    Site { id: "LT1-C01", content_state: "overlay-thumbnail-caption", source: "overlay/cards.rs:make_left_label", runner: "--smoke-overlay" },
    Site { id: "LT1-C02", content_state: "overlay-icon-window-title", source: "overlay.rs:make_centered_label", runner: "--smoke-overlay" },
    Site { id: "LT1-C03", content_state: "overlay-icon-app-name", source: "overlay/cards.rs:&w.app_name", runner: "--smoke-overlay" },
    Site { id: "LT1-C04", content_state: "overlay-footer-window-title", source: "overlay.rs:update_status_label", runner: "--smoke-overlay" },
    Site { id: "LT1-D01", content_state: "menu-item-title-prefix", source: "menu.rs:compact_menu_title", runner: "cargo test menu::tests::compact_menu_title_never_splits_a_composed_character" },
    Site { id: "LT1-D02", content_state: "overlay-app-initial", source: "overlay/cards.rs:app_initial", runner: "cargo test ffi::composed_character_tests::composed_character_helpers_keep_grapheme_clusters_intact" },
    Site { id: "LT1-S01", content_state: "all-instantiated-fixed-frame-text", source: "settings/widgets.rs:collect_debug_layout", runner: "graphical-session surface smoke runners" },
];

#[cfg(test)]
mod tests {
    use super::SITES;
    use std::collections::HashSet;

    #[test]
    fn inventory_has_unique_content_states_and_complete_runner_metadata() {
        let mut ids = HashSet::new();
        let mut states = HashSet::new();
        for site in SITES {
            assert!(
                ids.insert(site.id),
                "duplicate language-tolerance ID: {}",
                site.id
            );
            assert!(
                states.insert((site.id, site.content_state)),
                "duplicate site/content-state mapping: {} / {}",
                site.id,
                site.content_state
            );
            assert!(!site.source.is_empty(), "{} has no source mapping", site.id);
            assert!(!site.runner.is_empty(), "{} has no runner", site.id);
        }
    }

    #[test]
    fn inventory_source_anchors_exist() {
        for site in SITES {
            let (path, anchor) = site
                .source
                .split_once(':')
                .unwrap_or_else(|| panic!("{} has malformed source anchor", site.id));
            let source = match path {
                "settings/widgets.rs" => include_str!("settings/widgets.rs"),
                "settings/tooltip.rs" => include_str!("settings/tooltip.rs"),
                "settings/select.rs" => include_str!("settings/select.rs"),
                "clipboard/text_style.rs" => include_str!("clipboard/text_style.rs"),
                "keystroke_display/panel.rs" => include_str!("keystroke_display/panel.rs"),
                "onboarding.rs" => include_str!("onboarding.rs"),
                "overlay.rs" => include_str!("overlay.rs"),
                "overlay/cards.rs" => include_str!("overlay/cards.rs"),
                "menu.rs" => include_str!("menu.rs"),
                "updater.rs" => include_str!("updater.rs"),
                unknown => panic!("{} has unknown source file: {unknown}", site.id),
            };
            assert!(
                source.contains(anchor),
                "{} source anchor not found: {}:{}",
                site.id,
                path,
                anchor
            );
        }
    }

    #[test]
    fn inventory_ids_match_the_plan_table() {
        let plan = include_str!("../docs/language-tolerance-plan.md");
        let mut documented = HashSet::new();
        for line in plan.lines().filter(|line| line.starts_with("| LT1-")) {
            if let Some(id) = line.split('|').nth(1).map(str::trim) {
                documented.insert(id);
            }
        }
        let registered: HashSet<_> = SITES.iter().map(|site| site.id).collect();
        assert_eq!(registered, documented, "plan IDs and code inventory differ");
    }
}
