# Read each ignored smoke test's disturbance declaration, from src/ text only.
#
# Only a fn that carries #[test] is a test. The smoke entry points (app_menu_smoke_runner and
# friends) are helpers the test spawns; they must never appear here, or the aggregator would filter
# for a name libtest does not have and report a runner it never ran.
#
# Output: <fn>|<disturbances>|<status>, where status is ok, none, or conflicting.
# A second SMOKE_DISTURBS line in the same header block is a conflict, not an override: taking the
# last one would let a later 'focus' hide an earlier 'screen'.
{
    if (index($0, "SMOKE_DISTURBS=") > 0) {
        v = $0
        sub(/.*SMOKE_DISTURBS=/, "", v)
        sub(/[[:space:]]+$/, "", v)
        if (dists == "") { dists = v }
        else if (dists != v) { conflict = 1 }
        next
    }
    if ($0 ~ /#\[test\]/) { is_test = 1; next }
    if ($0 ~ /^[[:space:]]*\/\//) { next }
    if ($0 ~ /^[[:space:]]*#/) { next }
    if ($0 ~ /^[[:space:]]*$/) { reset(); next }
    {
        line = $0
        if (is_test && match(line, /fn [A-Za-z0-9_]+\(/)) {
            name = substr(line, RSTART + 3, RLENGTH - 4)
            st = (conflict ? "conflicting" : (dists == "" ? "none" : "ok"))
            print name "|" dists "|" st
        }
        reset()
    }
}
function reset() { is_test = 0; dists = ""; conflict = 0 }
