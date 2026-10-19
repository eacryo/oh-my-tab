#!/bin/bash
# Put a user back where a scenario found them -- and only when the scenario is still what moved them.
#
# Why this exists: a scenario that hands the front back unconditionally can yank a person who, during the
# run, deliberately switched to another app: that is a second disturbance rather than a cleanup, and it was
# reported as such in review. The same is true of the pointer. So each restore is conditional: it acts
# only while the machine is still in the state the scenario itself produced, and otherwise keeps what the
# user chose and says so.
#
# Every failure is returned, never swallowed: the caller raises its exit status, because a cleanup that
# silently did not happen is the failure mode this repository treats as a broken gate.
#
# The query command is overridable (OMT_CUA) so scripts/e2e-gate-selftest.sh can drive these functions
# against a stub instead of the real desktop.
#
#   source scripts/lib/e2e-restore.sh
#   omt_front_pid                    -> the active application pid, or empty
#   omt_hand_front_back <before> <test_pid>  -> 0 restored/not needed, 1 could not, 2 kept (user moved on)
#   omt_cursor_px                    -> "x y" in desktop pixels, or empty
#   omt_restore_cursor <x> <y> <expect_x> <expect_y> <tolerance>

omt_cua() {
    "${OMT_CUA:-cua-driver}" "$@"
}

omt_front_pid() {
    # No arguments: the driver's list_apps takes none. It used to accept
    # {"include_installed":false} and now refuses the call outright ("unknown argument"), which made
    # every scenario that takes a focus baseline fail before it started rather than read a wrong
    # answer. Running apps are what carry "active", so the unfiltered list is the one to ask for.
    omt_cua list_apps '{}' 2>/dev/null | python3 -c '
import json, sys
try:
    apps = json.load(sys.stdin).get("apps", [])
except Exception:
    raise SystemExit(1)
active = [a for a in apps if isinstance(a, dict) and a.get("active")]
pid = active[0].get("pid") if active else None
print(pid if isinstance(pid, int) and pid > 0 else "")
' 2>/dev/null
}

# Hand the front back to `before`, but only if the front still belongs to the app this scenario drove.
# The three outcomes are distinct because the caller reports them differently: 0 done or unnecessary,
# 1 a restore that was needed and failed, 2 the user moved on and was left alone.
omt_hand_front_back() {
    local before="$1" test_pid="$2" current
    [ -n "$before" ] || return 1
    current="$(omt_front_pid)"
    if [ -z "$current" ]; then
        echo "omt_hand_front_back: the frontmost application could not be read; handing the front back is unverified" >&2
        return 1
    fi
    if [ "$current" = "$before" ]; then
        return 0
    fi
    if [ -z "$test_pid" ]; then
        # Ownership cannot be established without knowing which app the scenario drove. Forcing the
        # restore then would be an unverified yank, so the front is left alone and the caller is told
        # the restore did not happen.
        echo "omt_hand_front_back: the scenario's controlled app is unknown, so whether the front is its is too; not restoring" >&2
        return 1
    fi
    if [ "$current" != "$test_pid" ]; then
        echo "omt_hand_front_back: pid $current is frontmost and this scenario did not bring it there; keeping the user's choice" >&2
        return 2
    fi
    if ! omt_cua bring_to_front "{\"pid\": $before}" >/dev/null 2>&1; then
        echo "omt_hand_front_back: could not hand the front back to pid $before" >&2
        return 1
    fi
    return 0
}

# The endpoint check for a scenario whose header claims it does not steal focus. It proves the frontmost
# application at the END is the one that was frontmost BEFORE; it does not prove nothing was fronted in
# between, and its message says which of the two failure readings it is (the app under test came to the
# front = the defect; a third app is a person switching, which is ambiguous rather than a defect).
omt_front_unchanged() {
    local before="$1" subject="${2:-}" current
    [ -n "$before" ] || { echo "the frontmost application was never recorded; the no-focus claim is unverified" >&2; return 1; }
    current="$(omt_front_pid)"
    if [ -z "$current" ]; then
        echo "the frontmost application could not be read; the no-focus claim is unverified" >&2
        return 1
    fi
    if [ "$current" = "$before" ]; then
        echo "the frontmost application is unchanged (pid $current)"
        return 0
    fi
    if [ -n "$subject" ] && [ "$current" = "$subject" ]; then
        echo "the app under test became frontmost during the run (pid $current, was $before); the scenario stole focus" >&2
    else
        echo "the frontmost application changed during the run: $before -> $current; if a person switched apps during the run this is that change, not a product defect" >&2
    fi
    return 1
}

omt_cursor_px() {
    local points scale
    points="$(omt_cua get_cursor_position '{}' 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
print(d.get("x", ""), d.get("y", ""))
' 2>/dev/null)" || return 1
    scale="$(omt_cua get_screen_size '{}' 2>/dev/null | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin)["scale_factor"])
except Exception:
    raise SystemExit(1)
' 2>/dev/null)" || return 1
    [ -n "$points" ] && [ -n "$scale" ] || return 1
    # shellcheck disable=SC2086
    set -- $points
    python3 -c 'import sys; print(round(float(sys.argv[1]) * float(sys.argv[3])), round(float(sys.argv[2]) * float(sys.argv[3])))' "$1" "$2" "$scale"
}

# Put the pointer back only while it is still where the scenario left it; a user who moved the pointer
# has made a newer decision than the one the scenario recorded.
omt_restore_cursor() {
    local want_x="$1" want_y="$2" expect_x="$3" expect_y="$4" tolerance="${5:-8}"
    [ -n "$want_x" ] && [ -n "$want_y" ] && [ -n "$expect_x" ] && [ -n "$expect_y" ] || return 1
    local current
    # Split from the declaration: under `set -e`, `local x="$(cmd)"` reports the status of `local`, so a
    # failing query would look like success.
    current="$(omt_cursor_px)" || {
        echo "omt_restore_cursor: the pointer position could not be read; its restore is unverified" >&2
        return 1
    }
    # shellcheck disable=SC2086
    set -- $current
    if ! python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1]) - float(sys.argv[3])) <= float(sys.argv[5]) and abs(float(sys.argv[2]) - float(sys.argv[4])) <= float(sys.argv[5]) else 1)' "$1" "$2" "$expect_x" "$expect_y" "$tolerance"; then
        echo "omt_restore_cursor: the pointer has moved since the scenario left it; keeping the user's position" >&2
        return 2
    fi
    if ! omt_cua move_cursor "{\"scope\": \"desktop\", \"x\": $want_x, \"y\": $want_y}" >/dev/null 2>&1; then
        echo "omt_restore_cursor: the pointer could not be moved back" >&2
        return 1
    fi
    return 0
}
