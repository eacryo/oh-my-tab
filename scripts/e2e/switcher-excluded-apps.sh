#!/bin/bash
# A2: an application on `[windows] excluded_apps` never shows a card in the switcher -- in a cold
# snapshot, in the directed refresh that follows focusing it, and in what a fast press-release
# commits.
#
# Judged by this script (exit code), not by a person: the assertions read the app's own
# `--e2e-state=<path>` snapshot and the real frontmost application. See AGENTS.md's tiers.
#
# Phase structure: each phase starts its own instance (the exclusion list is a configuration policy,
# and a cold snapshot is what proves the collector applied it). The directed-refresh phase then
# focuses the excluded application in the SAME instance, because a focus change is what can put its
# cards back, and the restore phase proves the list is read live again once the entry is gone.
#
# The exclusion card's own edit path (Add/Remove buttons) is covered by M1 -- the handlers' config
# mutation, the pure add/remove rules, the change-flag/apply path, the description rows and the
# action-id -> selector table -- plus an M2 layout-smoke assertion on the rendered Remove button's
# identifier. The same-instance edit phase is recorded in docs/review-backlog.md: neither real entry
# currently has a scriptable trigger (settings rows and menu items report no screen rectangles in
# `--e2e-state`, and the layout smoke renders against the user's real config).
#
# Usage: scripts/e2e/switcher-excluded-apps.sh
# Requires: Accessibility permission for the app, cua-driver running, a config file that exists (run
# the app once first), and at least two switchable windows owned by applications with bundle ids.
#
# Injecting the real global hotkey and activating the target application steals focus, and the run
# rewrites the user's config (restored in a trap); the markers below make run-all.sh require the
# corresponding authorizations.
# E2E_STEALS_FOCUS=1
# E2E_CHANGES_PREFS=1
# E2E_REPLACES_INSTANCE=1
#
# Exit code: 0 = every assertion passed, or NOT RUN when the machine cannot support them; non-zero =
# failed, with the reason printed.

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
source "$repo_dir/scripts/lib/e2e-restore.sh"

config="$HOME/.config/oh-my-tab/config.toml"
lib_dir="$repo_dir/scripts/e2e/lib"

# Every path is unique to this run: a fixed one lets a stale file from an earlier run satisfy an
# assertion or be restored over the user's config.
state_file="$(mktemp -t omt-e2e-excluded-state).json"
target_file="$(mktemp -t omt-e2e-excluded-target).json"
backup_tmp="$(mktemp -t omt-e2e-excluded-config.tmp)"
config_backup="$(mktemp -t omt-e2e-excluded-config.bak)"
rm -f "$state_file" "$state_file.tmp" "$target_file"

fail() { echo "e2e switcher-excluded-apps: FAIL: $*" >&2; exit 1; }
note() { echo "e2e switcher-excluded-apps: $*"; }

command -v cua-driver >/dev/null 2>&1 || fail "cua-driver CLI not found in PATH"
cua-driver status >/dev/null 2>&1 || fail "cua-driver daemon is not running"
[ -f "$config" ] || fail "no config at $config (run the app once first)"
python3 -B "$lib_dir/config_switch.py" --selftest >/dev/null || fail "config_switch.py selftest failed"
python3 -B "$lib_dir/hotkey.py" >/dev/null || fail "hotkey.py selftest failed"

front_app_before=""
app_pid=""
backup_ready=0
restore_failed=0

restore() {
    local status=$?
    if [ "$backup_ready" = "1" ]; then
        # The backup was completed and verified before it was allowed to exist under its final name,
        # so a partially written copy can never be restored over the user's file.
        if ! cp "$config_backup" "$config"; then
            echo "e2e switcher-excluded-apps: restoring $config FAILED" >&2
            restore_failed=1
        fi
    else
        # Nothing was backed up, so nothing may be concluded about the file; the phases below never
        # got that far either. Report rather than overwrite.
        echo "e2e switcher-excluded-apps: no verified config backup was taken" >&2
    fi
    local relaunch_rc=0
    "$repo_dir/scripts/dev-restart.sh" --no-onboarding >/dev/null 2>&1 || relaunch_rc=$?
    if [ "$relaunch_rc" -ne 0 ]; then
        echo "e2e switcher-excluded-apps: the clean relaunch FAILED (rc=$relaunch_rc)" >&2
        restore_failed=1
    elif ! cmp -s "$config_backup" "$config"; then
        # Checked AFTER the clean instance started: loading the config may repair or migrate it and
        # write the file back, so a pre-launch comparison would certify a file the app then changed.
        echo "e2e switcher-excluded-apps: $config does not match the backup after the clean launch" >&2
        restore_failed=1
    fi
    # A development switch left running breaks the user's instance; every run restores. Also require
    # that an instance IS running: a loop over an empty process list would pass by saying nothing.
    local pid count=0
    for pid in $(pgrep -x oh-my-tab || true); do
        count=$((count + 1))
        if ps -o command= -p "$pid" | tr ' ' '\n' | grep -qE '^--(e2e-state|show-overlay)'; then
            echo "e2e switcher-excluded-apps: pid $pid still runs a development switch after the restore" >&2
            restore_failed=1
        fi
    done
    if [ "$count" -eq 0 ]; then
        echo "e2e switcher-excluded-apps: no oh-my-tab instance is running after the restore" >&2
        restore_failed=1
    fi
    if [ -n "$front_app_before" ]; then
        local front_rc=0
        omt_hand_front_back "$front_app_before" "${app_pid:-}" || front_rc=$?
        if [ "$front_rc" -eq 1 ]; then
            echo "e2e switcher-excluded-apps: could not hand the front back to pid $front_app_before" >&2
            restore_failed=1
        fi
    fi
    rm -f "$state_file" "$state_file.tmp" "$target_file" "$backup_tmp"
    if [ "$restore_failed" -ne 0 ]; then
        [ "$status" -ne 0 ] || status=1
    fi
    exit "$status"
}
trap restore EXIT

front_app_before="$(cua-driver list_apps '{}' | python3 -c '
import json, sys
apps = json.load(sys.stdin)["apps"]
active = [a for a in apps if a.get("active")]
print(active[0]["pid"] if active else "")
')"

# Back up atomically: copy to a private name, prove the copy matches the source, and only then give
# it the name the restore path may use. A `cp` that fails halfway must never look like a backup.
cp "$config" "$backup_tmp" || fail "could not copy $config for backup"
cmp -s "$config" "$backup_tmp" || fail "the config backup does not match the original"
mv "$backup_tmp" "$config_backup" || fail "could not install the config backup"
backup_ready=1

# Start the dev build and print its pid; clears any stale snapshot first so a frame from an older
# instance cannot satisfy an assertion.
start_app() {
    rm -f "$state_file" "$state_file.tmp"
    local out
    out="$("$repo_dir/scripts/dev-restart.sh" --e2e-state="$state_file" --no-onboarding 2>&1)"
    printf '%s\n' "$out" | grep -q "^restart ok" || {
        printf '%s\n' "$out" | tail -20 >&2
        fail "dev-restart.sh did not bring the app up"
    }
    printf '%s\n' "$out" | grep -E "^(restart ok|build-version)" >&2 || true
    printf '%s\n' "$out" | sed -n 's/^restart ok (app pid \([0-9][0-9]*\).*/\1/p' | head -1
}

app_pid="$(start_app)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"

python_lib="$lib_dir"
export python_lib

# --- phase 1: a clean snapshot, and the window this scenario will exclude ---------------------
# The target must be a REAL card the app collected: excluding an application that was never a
# candidate proves nothing. The wait requires an ACCEPTED FULL result of THIS instance (the startup
# frame can carry `last_refresh: null`, and the lifecycle backstop is what supplies the pass), so a
# null or stale frame is skipped instead of crashing the assertion.
phase1_log="$(mktemp)"
python3 -B - "$state_file" "$app_pid" "$target_file" >"$phase1_log" 2>&1 <<'PY'
import json
import sys
import time

state_file, app_pid, target_file = sys.argv[1], int(sys.argv[2]), sys.argv[3]


def wait_frame(predicate, deadline=15.0):
    """The first accepted frame of THIS instance that satisfies `predicate`, or None.

    A timeout is a miss, never the last frame: a caller that treated the last frame as satisfied
    would pass on a pass that was never accepted (or on the startup frame, whose `last_refresh` is
    null until a pass has been applied)."""
    end = time.time() + deadline
    while time.time() < end:
        try:
            with open(state_file) as handle:
                snapshot = json.load(handle)
        except (FileNotFoundError, json.JSONDecodeError):
            time.sleep(0.05)
            continue
        if snapshot.get("pid") != app_pid:
            time.sleep(0.05)
            continue
        if predicate(snapshot):
            return snapshot
        time.sleep(0.05)
    return None


problems = []


def check(ok, what, detail):
    if not ok:
        problems.append(f"{what}: {detail}")


snapshot = wait_frame(lambda s: (s.get("last_refresh") or {}).get("kind") == "full")
check(snapshot is not None, "the app applied a full candidate pass", "no accepted full frame in 15s")
if snapshot is None:
    print("\n".join(problems))
    raise SystemExit(1)

cards = snapshot.get("cards") or []
# Preconditions are a NOT RUN, not a failure: this machine may simply not have two switchable
# windows, or two applications. Waiting for the accepted pass FIRST is what lets a legitimate
# EMPTY pass reach this branch instead of timing out as a failure.
target = next(
    (
        card
        for card in cards
        if card.get("bundle") and card.get("window_id") and card.get("pid") != app_pid
    ),
    None,
)
other = next(
    (
        card
        for card in cards
        if card.get("bundle")
        and card.get("pid") != app_pid
        and (target is None or card.get("pid") != target.get("pid"))
    ),
    None,
)
if target is None or other is None:
    print(
        "NOT RUN: no suitable pair of applications (need two switchable windows owned by two "
        "different applications with bundle ids); "
        f"cards={[(c.get('app'), c.get('bundle'), c.get('pid')) for c in cards]}"
    )
    raise SystemExit(75)
with open(target_file, "w") as handle:
    json.dump(
        {"pid": target["pid"], "window_id": target["window_id"], "bundle": target["bundle"]},
        handle,
    )
check(bool(target.get("title")), "the target card carries a window title", f"target={target}")
if problems:
    print("\n".join(problems))
    raise SystemExit(1)
print(f"phase 1 ok: target pid={target['pid']} window={target['window_id']} bundle={target['bundle']}")
PY
phase1_rc=$?
if [ "$phase1_rc" -eq 75 ]; then
    grep -E "^NOT RUN" "$phase1_log" >&2 || true
    rm -f "$phase1_log"
    note "NOT RUN (the machine has no suitable target window)"
    exit 0
fi
if [ "$phase1_rc" -ne 0 ]; then
    cat "$phase1_log" >&2
    rm -f "$phase1_log"
    fail "phase 1 (baseline candidates) failed"
fi
cat "$phase1_log" >&2
rm -f "$phase1_log"
note "phase 1 (baseline) ok"

# --- phase 2: the exclusion applies to the real candidate list, and to a directed refresh -------
target_bundle="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["bundle"])' "$target_file")"
[ -n "$target_bundle" ] || fail "could not read the target bundle id"
python3 -B - "$config" "$target_bundle" "$lib_dir" <<'PY' || fail "could not write windows.excluded_apps"
import sys

sys.path.insert(0, sys.argv[3])
from config_switch import apply_list  # noqa: E402  (path set above)

apply_list(sys.argv[1], "windows", "excluded_apps", [sys.argv[2]])
PY
note "wrote windows.excluded_apps = [\"$target_bundle\"]"

app_pid="$(start_app)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"

phase2_log="$(mktemp)"
python3 -B - "$state_file" "$app_pid" "$target_file" "$lib_dir" "$config" >"$phase2_log" 2>&1 <<'PY'
import json
import subprocess
import sys
import time

state_file, app_pid, target_file, lib_dir, config = (
    sys.argv[1],
    int(sys.argv[2]),
    sys.argv[3],
    sys.argv[4],
    sys.argv[5],
)
with open(target_file) as handle:
    target = json.load(handle)
target_pid = target["pid"]
target_wid = target["window_id"]

sys.path.insert(0, lib_dir)
from hotkey import modifier_value  # noqa: E402  (path set above)


def wait_frame(predicate, deadline=15.0):
    """The first accepted frame of THIS instance satisfying `predicate`, or None (a timeout is a
    miss, not the last frame seen)."""
    end = time.time() + deadline
    while time.time() < end:
        try:
            with open(state_file) as handle:
                snapshot = json.load(handle)
        except (FileNotFoundError, json.JSONDecodeError):
            time.sleep(0.05)
            continue
        if snapshot.get("pid") != app_pid:
            time.sleep(0.05)
            continue
        if predicate(snapshot):
            return snapshot
        time.sleep(0.05)
    return None


problems = []


def check(ok, what, detail):
    if not ok:
        problems.append(f"{what}: {detail}")


def card_keys(snapshot):
    return [
        (card.get("pid"), card.get("window_id")) for card in snapshot.get("cards") or []
    ]


def has_target(snapshot):
    return (target_pid, target_wid) in card_keys(snapshot)


def exclusion_reason(snapshot):
    return (target_wid, "excluded_app") in {
        (entry.get("window_id"), entry.get("reason"))
        for entry in snapshot.get("rejected_windows") or []
    }


def cua(tool, args):
    """Run one cua-driver call and return its decoded stdout, or None on failure."""
    completed = subprocess.run(
        ["cua-driver", "call", tool, json.dumps(args)],
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        return None
    try:
        return json.loads(completed.stdout or "null")
    except json.JSONDecodeError:
        return None


def frontmost_pid():
    reply = cua("list_apps", {})
    if not isinstance(reply, dict):
        return None
    active = [app for app in reply.get("apps") or [] if app.get("active")]
    return active[0].get("pid") if active else None


def wait_frontmost(pid, deadline=2.5):
    end = time.time() + deadline
    while time.time() < end:
        if frontmost_pid() == pid:
            return True
        time.sleep(0.1)
    return False


# 1. The cold snapshot: an ACCEPTED FULL pass of this instance must carry no target card and must
#    state the target window's own exclusion reason.
snapshot = wait_frame(
    lambda s: (s.get("last_refresh") or {}).get("kind") == "full" and not has_target(s)
)
check(snapshot is not None, "the excluded snapshot came from a full pass", "no accepted full frame in 15s")
if snapshot is None:
    print("\n".join(problems))
    raise SystemExit(1)
cards = snapshot.get("cards") or []
check(
    all(card.get("pid") != target_pid for card in cards),
    "no card belongs to the excluded application",
    f"cards={[(c.get('app'), c.get('pid'), c.get('window_id')) for c in cards]}",
)
check(len(cards) >= 1, "the other applications still have cards", f"cards={len(cards)}")
check(exclusion_reason(snapshot), "the app recorded the target window's exclusion reason", f"rejected={snapshot.get('rejected_windows')}")

# 2. Focus the excluded application: the directed refresh for it must be ACCEPTED (a result newer
#    than this input), carry no cards, and carry its own exclusion reason. The OTHER application is
#    made frontmost first -- a `launch_app` of an already-frontmost target may produce no focus
#    change at all, and then "a directed refresh was accepted" would describe an older frame.
other = next(
    (
        card
        for card in (snapshot.get("cards") or [])
        if card.get("pid") != target_pid and card.get("bundle")
    ),
    None,
)
if other is None:
    problems.append("no other application to drive the focus transition from")
seq_before = (snapshot.get("seq") or 0)
if other is not None:
    parked = cua("launch_app", {"bundle_id": other["bundle"]})
    check(parked is not None, "parking on the other application succeeded", "launch_app failed")
    check(
        wait_frontmost(other["pid"]),
        "the other application is frontmost before the target is focused",
        f"frontmost={frontmost_pid()} expected={other['pid']}",
    )
    latest = wait_frame(lambda s: True, deadline=2.0) or snapshot
    seq_before = latest.get("seq") or seq_before
launched = cua("launch_app", {"bundle_id": target["bundle"]})
check(launched is not None, "focusing the excluded application succeeded", "launch_app failed")
if launched is not None:
    check(
        wait_frontmost(target_pid),
        "the excluded application is frontmost",
        f"frontmost={frontmost_pid()} expected={target_pid}",
    )
    directed = wait_frame(
        lambda s: (s.get("seq") or 0) > seq_before
        and (s.get("last_refresh") or {}).get("kind") == "directed"
        and (s.get("last_refresh") or {}).get("pid") == target_pid
    )
    check(directed is not None, "a directed refresh for the target was accepted", "no newer directed frame in 15s")
    if directed is not None:
        check(
            all(card.get("pid") != target_pid for card in directed.get("cards") or []),
            "the directed refresh did not put the excluded cards back",
            f"cards={[(c.get('app'), c.get('pid')) for c in directed.get('cards') or []]}",
        )
        check(
            exclusion_reason(directed),
            "the accepted directed pass recorded the target window's exclusion reason",
            f"rejected={directed.get('rejected_windows')}",
        )

# 3. A fast press-release commits without ever displaying the panel: it must not commit the excluded
#    application, and the baseline must be a CONFIRMED other application (a commit that picks the
#    excluded target cannot hide behind "it was frontmost anyway").
if other is not None:
    activated = cua("launch_app", {"bundle_id": other["bundle"]})
    check(activated is not None, "activating the other application succeeded", "launch_app failed")
    check(
        wait_frontmost(other["pid"]),
        "the other application is frontmost before the chord",
        f"frontmost={frontmost_pid()} expected={other['pid']}",
    )
    modifier = modifier_value(open(config, encoding="utf-8", errors="replace").read())
    cua_modifier = {"command": "cmd", "option": "option", "control": "ctrl"}[modifier]
    latest = wait_frame(lambda s: True, deadline=2.0) or snapshot
    before_commit = (latest.get("last_commit") or {}).get("count", 0)
    chord = subprocess.run(
        [
            "cua-driver",
            "call",
            "hotkey",
            json.dumps(
                {
                    "keys": [cua_modifier, "tab"],
                    "scope": "desktop",
                    "delivery_mode": "foreground",
                }
            ),
        ],
        capture_output=True,
        text=True,
    )
    check(chord.returncode == 0, "the global chord was injected", f"cua-driver rc={chord.returncode}")
    committed = wait_frame(lambda s: ((s.get("last_commit") or {}).get("count", 0)) > before_commit)
    check(committed is not None, "the fast release committed a card", "no commit frame in 15s")
    if committed is not None:
        last_commit = committed.get("last_commit") or {}
        check(
            last_commit.get("pid") != target_pid,
            "the fast release did not commit the excluded application",
            f"last_commit={last_commit}",
        )
        check(
            last_commit.get("window_id") != target_wid,
            "the fast release did not commit the excluded window",
            f"last_commit={last_commit}",
        )

if problems:
    print("\n".join(problems))
    raise SystemExit(1)
print("phase 2 ok: no card, an excluded_app reason, an accepted directed refresh, and a safe commit")
PY
phase2_rc=$?
if [ "$phase2_rc" -ne 0 ]; then
    cat "$phase2_log" >&2
    rm -f "$phase2_log"
    fail "phase 2 (exclusion applied) failed"
fi
cat "$phase2_log" >&2
rm -f "$phase2_log"
note "phase 2 (exclusion applied, directed refresh, fast release) ok"

# --- phase 3: removing the entry brings the window back --------------------------------------
python3 -B "$lib_dir/config_switch.py" --unset "$config" windows excluded_apps \
    || fail "could not remove windows.excluded_apps"
app_pid="$(start_app)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"

phase3_log="$(mktemp)"
python3 -B - "$state_file" "$app_pid" "$target_file" >"$phase3_log" 2>&1 <<'PY'
import json
import sys
import time

state_file, app_pid, target_file = sys.argv[1], int(sys.argv[2]), sys.argv[3]
with open(target_file) as handle:
    target = json.load(handle)


def wait_frame(predicate, deadline=15.0):
    end = time.time() + deadline
    while time.time() < end:
        try:
            with open(state_file) as handle:
                snapshot = json.load(handle)
        except (FileNotFoundError, json.JSONDecodeError):
            time.sleep(0.05)
            continue
        if snapshot.get("pid") != app_pid:
            time.sleep(0.05)
            continue
        if predicate(snapshot):
            return snapshot
        time.sleep(0.05)
    return None


def has_target(snapshot):
    return any(
        card.get("pid") == target["pid"] and card.get("window_id") == target["window_id"]
        for card in snapshot.get("cards") or []
    )


snapshot = wait_frame(
    lambda s: (s.get("last_refresh") or {}).get("kind") == "full" and has_target(s)
)
if snapshot is None or not has_target(snapshot):
    print(
        "the removed entry did not bring its window back: "
        f"cards={[(c.get('app'), c.get('pid'), c.get('window_id')) for c in (snapshot or {}).get('cards') or []]}"
    )
    raise SystemExit(1)
print("phase 3 ok: the window is a card again after the entry was removed")
PY
phase3_rc=$?
if [ "$phase3_rc" -ne 0 ]; then
    cat "$phase3_log" >&2
    rm -f "$phase3_log"
    fail "phase 3 (entry removed) failed"
fi
cat "$phase3_log" >&2
rm -f "$phase3_log"
note "phase 3 (entry removed) ok"

note "PASS"
exit 0
