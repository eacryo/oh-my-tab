# Resolve the path carried by --demo= for a LaunchServices app launch.
# Sourced by scripts/dev-restart.sh and by scripts/demo-path-selftest.sh.

# Print the launch-facing form of a user-typed demo path. Absolute paths pass through unchanged;
# relative paths are prefixed with the caller's PHYSICAL cwd, with the typed relative components
# preserved verbatim. The path must not be canonicalized through `cd`: shell logical-path handling
# can collapse `..` before a symlink is resolved, which may select a different script.
demo_path_for_launch() {
    local spec="$1"
    case "$spec" in
        ''|/*) printf '%s' "$spec" ;;
        *) printf '%s/%s' "$(pwd -P)" "$spec" ;;
    esac
}
