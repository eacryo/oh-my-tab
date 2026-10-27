#!/bin/bash
# Finder-extension restore state, written before a dev rebuild replaces the app bundle.
#
# The production path records this from Sparkle's install lifecycle (src/finder_menu/recovery.rs);
# the dev path has no Sparkle install, so dev-restart.sh writes the same record before it deletes
# the old bundle. The bytes are shared with the Rust parser through
# testdata/finder-ext-transaction.toml: changing the field names or types on either side fails
# scripts/finder-ext-state-selftest.sh (shell side) and the Rust fixture test.
#
# Pure shell: no side effects, safe to source from tests.

# Print the TOML the app stores under its UserDefaults domain for one update transaction.
#
#   omt_finder_ext_transaction_toml <source_build> <target_build> <appex_id> <true|false>
omt_finder_ext_transaction_toml() {
    local source_build="$1" target_build="$2" appex_id="$3" enabled_before="$4"
    [ -n "$source_build" ] && [ -n "$target_build" ] && [ -n "$appex_id" ] || return 1
    case "$enabled_before" in
        true|false) ;;
        *) return 1 ;;
    esac
    printf 'source_build = "%s"\ntarget_build = "%s"\nappex_id = "%s"\nenabled_before = %s\n' \
        "$source_build" "$target_build" "$appex_id" "$enabled_before"
}

# Whether 'pluginkit -m' output for one plugin marks it as elected ('+'), i.e. enabled.
omt_finder_ext_marker_enabled() {
    case "$1" in
        +*) return 0 ;;
        *) return 1 ;;
    esac
}
