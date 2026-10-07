#!/bin/bash
# A2 层 E2E 场景:跨 macOS 桌面(其他 Space)的窗口开关。
#
# 判定者是**脚本**(退出码),不是人:输入走真实全局热键(Option+Tab 按住/步进/释放),断言全部读
# app 自己写的 JSON 快照(`--e2e-state=<path>`)与 WindowServer 的真实状态。分层与约定见 AGENTS.md
# 「测试分层」。
#
# 覆盖的真实路径:
#   * 关闭开关(默认):另一桌面的窗口**不得**出现在候选里 —— 这是改动前的既有语义;
#   * 打开开关:该窗口出现,且标记 other_desktop;
#   * 它**不得**进入缩略图候选集(另一桌面抓不到图,只能退化为图标卡);
#   * 选中它必须真的把桌面切过去,并落到那个精确窗口:提交身份、前台 pid、当前 Space、目标窗口
#     on_current_space 四项对账;app 自己发布的 other_desktop_raise 必须绑定本次目标且 onscreen/ax_matched;
#     最终焦点窗口由独立的 AX 读取核对(能力探测失败时才记为 NOT RUN)。
#
# 前置:
#   * 已授予辅助功能与屏幕录制权限;cua-driver 在运行。
#   * 机器上**存在第二个桌面**且其上有普通窗口,否则本场景打印 NOT RUN 并以 0 退出 ——
#     按 AGENTS.md,「未运行」不等于「通过」,报告时必须如实标注。
#
# 本场景会注入真实全局热键并真实切换桌面(抢焦点),故用标记声明;run-all.sh 默认跳过。
# This scenario injects a real global hotkey and switches desktops, so it steals focus.
# E2E_STEALS_FOCUS=1
#
# 本场景会临时改写用户配置里的 windows.show_other_desktops,并在 trap 里恢复原文件与重启 app。
# Exit code: 0 = 断言全部通过,或环境不具备时的 NOT RUN;非 0 = 失败(逐条打印原因)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
state_file="/tmp/omt-e2e-space-desktops.json"
config="$HOME/.config/oh-my-tab/config.toml"
config_backup="/tmp/omt-e2e-space-desktops-config.bak"

fail() { echo "e2e space-desktops: FAIL: $*" >&2; exit 1; }
note() { echo "e2e space-desktops: $*"; }

# The shared verdict helper (and its own regression cases) must work before anything else runs.
repo_dir_verdict="$repo_dir/scripts/e2e/lib"
python3 -B "$repo_dir_verdict/verdict.py" >/dev/null || fail "verdict.py selftest failed"
python3 -B "$repo_dir_verdict/space_kinds.py" >/dev/null || fail "space_kinds.py selftest failed"

command -v cua-driver >/dev/null 2>&1 || fail "cua-driver CLI not found in PATH"
cua-driver status >/dev/null 2>&1 || fail "cua-driver daemon is not running"
[ -f "$config" ] || fail "no config at $config (run the app once first)"

# Restore in every exit path: the user's config, a usable app, and the desktop they started on.
front_app_before=""
restore() {
    if [ -f "$config_backup" ]; then
        cp "$config_backup" "$config"
    fi
    pkill -f 'Oh-My-Tab-Dev' >/dev/null 2>&1 || true
    for _ in $(seq 1 40); do
        pgrep -f 'Oh-My-Tab-Dev' >/dev/null 2>&1 || break
        sleep 0.1
    done
    "$repo_dir/scripts/dev-restart.sh" --no-onboarding >/dev/null 2>&1 || true
    if [ -n "$front_app_before" ]; then
        cua-driver bring_to_front "{\"pid\": $front_app_before}" >/dev/null 2>&1 || true
    fi
}
trap restore EXIT

cp "$config" "$config_backup"
rm -f "$state_file" "${state_file%.json}.tmp"


front_app_before="$(cua-driver list_apps '{"include_installed":false}' | python3 -c '
import json, sys
apps = json.load(sys.stdin)["apps"]
active = [a for a in apps if a.get("active")]
print(active[0]["pid"] if active else "")
')"

# --- phase 1: the switch off (the shipped default) --------------------------
# Prints the app pid on stdout and the restart report on stderr: the caller captures only the pid.
start_app() {
    rm -f "$state_file" "${state_file%.json}.tmp"
    local out
    out="$("$repo_dir/scripts/dev-restart.sh" --e2e-state="$state_file" --no-onboarding 2>&1)"
    printf '%s\n' "$out" | grep -q "^restart ok" || {
        printf '%s\n' "$out" | tail -20 >&2
        fail "dev-restart.sh did not bring the app up"
    }
    printf '%s\n' "$out" | grep -E "^(restart ok|build-version|app args)" >&2 || true
    printf '%s\n' "$out" | sed -n 's/^restart ok (app pid \([0-9][0-9]*\).*/\1/p' | head -1
}

python3 - "$config" remove <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
lines = [l for l in path.read_text().splitlines(keepends=True) if not l.startswith("show_other_desktops")]
path.write_text("".join(lines))
PY

app_pid="$(start_app)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"

# Pick the cross-desktop target from the real WindowServer state **and the app's Space kinds**: a
# window whose Space list holds an ORDINARY Space that is not the active one. A window on a fullscreen
# Space is excluded on purpose: the app resolves that Space's origin from the native Space order, so it
# belongs to the current desktop's group and is not "another desktop" for this scenario.
target_json="$(python3 - "$state_file" "$app_pid" <<'TARGET_PY'
import json
import subprocess
import sys
import time

state_file, app_pid = sys.argv[1], int(sys.argv[2])


def state():
    try:
        with open(state_file) as handle:
            snapshot = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return snapshot if snapshot.get("pid") == app_pid else None


# The Space kinds come from the app, so wait for the snapshot the app just wrote.
deadline = time.time() + 10
snapshot = None
while time.time() < deadline:
    snapshot = state()
    if snapshot and snapshot.get("spaces"):
        break
    time.sleep(0.05)
if not snapshot or not snapshot.get("spaces"):
    print("null")
    raise SystemExit(0)
kinds = {space["id"]: space["kind"] for space in snapshot["spaces"]}
windows = json.loads(
    subprocess.run(["cua-driver", "list_windows", "{}"], capture_output=True, text=True).stdout
)
current = windows.get("current_space_id")
# An app that has a window on the current Space answers AX about that window, so its other-desktop
# window is treated as a secondary surface until that desktop has been visited once (the narrowing
# documented in docs/fullscreen-space-groups-plan.md). Prefer a window whose app lives entirely on
# another desktop: that one is admitted as soon as the switch is on, which is what phase 2 asserts.
pids_here = {
    window["pid"]
    for window in windows["windows"]
    if window.get("space_ids") and current in window["space_ids"]
}
targets = [
    window
    for window in windows["windows"]
    if window.get("space_ids")
    and current not in window["space_ids"]
    and not window.get("on_current_space")
    and window.get("title")
    and window["pid"] not in pids_here
    and any(kinds.get(space) == "ordinary" for space in window["space_ids"])
]
targets.sort(key=lambda window: window["window_id"])
targets.sort(key=lambda window: window["window_id"])
print(json.dumps(targets[0] if targets else None))
TARGET_PY
)" || fail "could not pick the cross-desktop target"
case "$target_json" in
    null)
        note "NOT RUN: no window on another ordinary macOS desktop on this machine (unrun is not passed)"
        exit 0
        ;;
esac
target_pid="$(printf '%s' "$target_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pid"])')"
target_wid="$(printf '%s' "$target_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["window_id"])')"
target_app="$(printf '%s' "$target_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["app_name"])')"
note "target: $target_app pid=$target_pid wid=$target_wid"

python3 -B - "$state_file" "$app_pid" "$target_wid" "$target_pid" "$config" "$repo_dir_verdict" <<'PY'
import ctypes
import json
import pathlib
import subprocess
import sys
import time

state_file, app_pid, target_wid, target_pid, config, repo_dir_verdict = (
    sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5], sys.argv[6],
)
sys.path.insert(0, repo_dir_verdict)
from space_kinds import (  # noqa: E402  (the path above makes it importable)
    fullscreen_current,
    fullscreen_window_ids,
    pick_fullscreen_target,
)
notes: list[str] = []
problems: list[str] = []
checks: list[str] = []


def check(ok: bool, label: str, detail: str = "") -> None:
    (checks if ok else problems).append(f"{label}{(': ' + detail) if detail else ''}")


# --- CGEvent synthesis, as scripts/e2e/tab-repeat.sh does -------------------
cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
cg.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
cg.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
cg.CGEventSetIntegerValueField.argtypes = [ctypes.c_void_p, ctypes.c_int32, ctypes.c_int64]
cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]

FIELD_AUTOREPEAT = 8
FIELD_KEYCODE = 9
FLAG_OPTION = 0x80000
VK_TAB = 48
VK_OPTION = 58
VK_ESCAPE = 53
HID_EVENT_TAP = 0


def post(keycode: int, down: bool, flags: int, autorepeat: int = 0) -> None:
    event = cg.CGEventCreateKeyboardEvent(None, keycode, down)
    cg.CGEventSetFlags(event, flags)
    cg.CGEventSetIntegerValueField(event, FIELD_KEYCODE, keycode)
    cg.CGEventSetIntegerValueField(event, FIELD_AUTOREPEAT, autorepeat)
    cg.CGEventPost(HID_EVENT_TAP, event)


def release_all() -> None:
    post(VK_TAB, False, 0)
    post(VK_OPTION, False, 0)


def read_state():
    try:
        with open(state_file) as handle:
            snapshot = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return snapshot if snapshot.get("pid") == app_pid else None


def seq() -> int:
    snapshot = read_state()
    return snapshot.get("seq", 0) if snapshot else 0


def wait(predicate, timeout: float, label: str, after_seq: int = 0):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        snapshot = read_state()
        if snapshot and snapshot.get("seq", 0) > after_seq:
            last = snapshot
            if predicate(snapshot):
                return snapshot
        time.sleep(0.02)
    raise AssertionError(f"timeout waiting for {label}; last={json.dumps(last)[:300] if last else None}")


def summon():
    """Hold Option+Tab until the overlay reports cards; retried because a press can precede the tap."""
    for _ in range(6):
        before = seq()
        post(VK_OPTION, True, FLAG_OPTION)
        post(VK_TAB, True, FLAG_OPTION)
        try:
            return wait(
                lambda s: s.get("event") == "summon" and s.get("cards_count", 0) >= 2,
                1.5, "the overlay to summon", after_seq=before,
            )
        except AssertionError:
            release_all()
            try:
                wait(lambda s: not s.get("visible"), 2, "the overlay to close", after_seq=before)
            except AssertionError:
                pass
            time.sleep(0.3)
    raise AssertionError("the switcher never opened")


def cua(tool: str, args: dict) -> dict:
    out = subprocess.run(["cua-driver", tool, json.dumps(args)], capture_output=True, text=True)
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"cua-driver {tool} returned no JSON: {out.stdout[:200]}")


def card_of(frame, wid: int):
    return next((c for c in frame.get("cards", []) if c["window_id"] == wid), None)


try:
    # --- phase 1: switch off -> the cross-desktop window must not be a candidate --
    frame = summon()
    cards = frame.get("cards", [])
    check(frame.get("space_filter", {}).get("show_other_desktops") is False,
          "the switch reads off by default", json.dumps(frame.get("space_filter")))
    # Since 2026-10-07 a fullscreen window is a candidate whatever the switch says (it has no AX
    # element while it is on its own Space, so its reachability cannot depend on the switch). Two
    # facts follow, and both depend on the live context, so each is only asserted when its premise
    # holds:
    #   - while a fullscreen Space is current the contract widens to every desktop, so an ordinary
    #     cross-desktop window IS a card then;
    #   - a fullscreen window is a card even with the switch off -- pinned on one exact window, so
    #     losing one of several fullscreen windows cannot pass.
    contexts = frame.get("space_contexts") or []
    current_kinds = {context["display"]: context["kind"] for context in contexts}
    if fullscreen_current(contexts):
        notes.append(
            "with the switch off the ordinary cross-desktop window is not a card: NOT RUN "
            f"(a fullscreen Space is current, so the contract widens to every desktop) contexts={current_kinds}"
        )
    else:
        check(card_of(frame, target_wid) is None,
              "with the switch off the other-desktop window is not a card",
              f"cards={[c['window_id'] for c in cards]} contexts={current_kinds}")

    fullscreen_wids = fullscreen_window_ids(
        frame.get("spaces"), cua("list_windows", {}).get("windows", [])
    )
    # One exact window of a fullscreen Space, preferring a titled and substantial one so the choice is
    # a real window rather than one of its app's helper surfaces.
    fullscreen_target = pick_fullscreen_target(
        cua("list_windows", {}).get("windows", []), fullscreen_wids
    )
    if fullscreen_target is None:
        notes.append(
            "with the switch off a fullscreen window is still a card: NOT RUN "
            "(no titled, substantial window on a fullscreen Space on this machine)"
        )
    else:
        check(
            card_of(frame, fullscreen_target["window_id"]) is not None,
            "with the switch off a fullscreen window is still a card",
            f"want={fullscreen_target['window_id']} ({fullscreen_target.get('title')!r}) "
            f"cards={[c['window_id'] for c in cards]} fullscreen={sorted(fullscreen_wids)}",
        )
    # Cancel (Escape) rather than release the modifier: a release would commit a raise.
    post(VK_ESCAPE, True, FLAG_OPTION)
    post(VK_ESCAPE, False, FLAG_OPTION)
    time.sleep(0.3)

    # --- phase 2: switch on --------------------------------------------------------------
    path = pathlib.Path(config)
    text = path.read_text()
    marker = "[windows]\n"
    if marker not in text:
        raise AssertionError("the config has no [windows] section to add the switch to")
    path.write_text(text.replace(marker, marker + "show_other_desktops = true\n", 1))
finally:
    release_all()

print("\n".join(checks))
for note in notes:
    print(note)
if problems:
    print("\n".join(problems), file=sys.stderr)
    raise SystemExit(1)
PY
[ $? -eq 0 ] || fail "phase 1 (switch off) assertions failed"

app_pid="$(start_app)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"

phase2_log="/tmp/omt-e2e-space-desktops-phase2.log"
# reference_pid: a pid that has a focused window right now, used only to prove the AX read
# works at all before a missing focus read is judged a failure instead of a NOT RUN.
reference_pid="${front_app_before:-$target_pid}"
python3 -B - "$state_file" "$app_pid" "$target_wid" "$target_pid" "$target_app" "$reference_pid" "$repo_dir_verdict" <<'PY' >"$phase2_log"
import ctypes
import json
import subprocess
import sys
import time

state_file, app_pid, target_wid, target_pid, target_app, reference_pid, repo_dir_verdict = (
    sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5], int(sys.argv[6]), sys.argv[7],
)
sys.path.insert(0, repo_dir_verdict)
from verdict import unrun_or_fail  # noqa: E402  (the path above is what makes it importable)
from space_kinds import fullscreen_window_ids, target_rejection_reason  # noqa: E402
problems: list[str] = []
checks: list[str] = []
notes: list[str] = []


def check(ok: bool, label: str, detail: str = "") -> None:
    (checks if ok else problems).append(f"{label}{(': ' + detail) if detail else ''}")


cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
cg.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
cg.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
cg.CGEventSetIntegerValueField.argtypes = [ctypes.c_void_p, ctypes.c_int32, ctypes.c_int64]
cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]

FIELD_AUTOREPEAT = 8
FIELD_KEYCODE = 9
FLAG_OPTION = 0x80000
VK_TAB = 48
VK_OPTION = 58
VK_ESCAPE = 53
HID_EVENT_TAP = 0


def post(keycode, down, flags, autorepeat=0):
    event = cg.CGEventCreateKeyboardEvent(None, keycode, down)
    cg.CGEventSetFlags(event, flags)
    cg.CGEventSetIntegerValueField(event, FIELD_KEYCODE, keycode)
    cg.CGEventSetIntegerValueField(event, FIELD_AUTOREPEAT, autorepeat)
    cg.CGEventPost(HID_EVENT_TAP, event)


def release_all():
    post(VK_TAB, False, 0)
    post(VK_OPTION, False, 0)


# --- AX read-only probe: the app's real focused window -----------------------
# Request acceptance and the SLPS/AX return codes cannot show which window actually holds focus, so
# the scenario reads it from the system: the app's AXFocusedWindow, mapped back to a CGWindowID
# through the same private _AXUIElementGetWindow the app uses. Without Accessibility permission for
# this script the read fails, and the caller reports the check as NOT RUN instead of passing it.
app_services = ctypes.CDLL(
    "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices"
)
hiservices = ctypes.CDLL(
    "/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices"
)
core_foundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
app_services.AXUIElementCreateApplication.restype = ctypes.c_void_p
app_services.AXUIElementCreateApplication.argtypes = [ctypes.c_int32]
app_services.AXUIElementCopyAttributeValue.restype = ctypes.c_int32
app_services.AXUIElementCopyAttributeValue.argtypes = [
    ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)
]
app_services.AXUIElementSetMessagingTimeout.argtypes = [ctypes.c_void_p, ctypes.c_float]
hiservices._AXUIElementGetWindow.restype = ctypes.c_int32
hiservices._AXUIElementGetWindow.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
core_foundation.CFStringCreateWithCString.restype = ctypes.c_void_p
core_foundation.CFStringCreateWithCString.argtypes = [
    ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32
]
core_foundation.CFRelease.argtypes = [ctypes.c_void_p]
CF_STRING_UTF8 = 0x08000100


def ax_focused_window_id(pid):
    key = core_foundation.CFStringCreateWithCString(None, b"AXFocusedWindow", CF_STRING_UTF8)
    if not key:
        return None
    try:
        app = app_services.AXUIElementCreateApplication(pid)
        if not app:
            return None
        try:
            app_services.AXUIElementSetMessagingTimeout(app, 0.25)
            element = ctypes.c_void_p()
            if app_services.AXUIElementCopyAttributeValue(app, key, ctypes.byref(element)) != 0:
                return None
            if not element.value:
                return None
            try:
                wid = ctypes.c_uint32(0)
                if hiservices._AXUIElementGetWindow(element, ctypes.byref(wid)) != 0:
                    return None
                return wid.value or None
            finally:
                core_foundation.CFRelease(element)
        finally:
            core_foundation.CFRelease(app)
    finally:
        core_foundation.CFRelease(key)


def read_state():
    try:
        with open(state_file) as handle:
            snapshot = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return snapshot if snapshot.get("pid") == app_pid else None


def seq():
    snapshot = read_state()
    return snapshot.get("seq", 0) if snapshot else 0


def wait(predicate, timeout, label, after_seq=0):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        snapshot = read_state()
        if snapshot and snapshot.get("seq", 0) > after_seq:
            last = snapshot
            if predicate(snapshot):
                return snapshot
        time.sleep(0.02)
    raise AssertionError(f"timeout waiting for {label}; last={json.dumps(last)[:300] if last else None}")


def summon():
    for _ in range(6):
        before = seq()
        post(VK_OPTION, True, FLAG_OPTION)
        post(VK_TAB, True, FLAG_OPTION)
        try:
            return wait(lambda s: s.get("event") == "summon" and s.get("cards_count", 0) >= 2,
                        1.5, "the overlay to summon", after_seq=before)
        except AssertionError:
            release_all()
            try:
                wait(lambda s: not s.get("visible"), 2, "the overlay to close", after_seq=before)
            except AssertionError:
                pass
            time.sleep(0.3)
    raise AssertionError("the switcher never opened")


def cua(tool, args):
    out = subprocess.run(["cua-driver", tool, json.dumps(args)], capture_output=True, text=True)
    return json.loads(out.stdout)


# Capability first, before anything moves: reading a reference app's focused window proves the AX
# read works at all, so a later empty or wrong read is a failure rather than environment trouble.
# The reference is the app the scenario recorded as frontmost at start; if it is gone by now the
# probe result is reported as NOT RUN for the focus check only.
def check_failed_app_cards_are_visible(frame, label):
    """A card of an app whose AX query failed must be a window we can see.

    With no AX answer there is nothing to tell a real window from a closed menu-bar panel, so the CG
    fallback covers only what is on screen. Measured 2026-10-07: freezing Stats so its AX read timed
    out put it in `ax_failed_pids`, and the fallback admitted both its visible settings window and
    its closed "Combined modules" panel -- the pair the user reported.
    """
    failed = set(frame.get("ax_failed_pids") or [])
    offenders = [
        (c["pid"], c["app"], c["window_id"], c["title"], c.get("on_screen"))
        for c in frame.get("cards", [])
        if c["pid"] in failed and c.get("on_screen") is not True
    ]
    check(not offenders,
          f"{label}: a card of an AX-failed app is an on-screen window",
          f"offenders={offenders}")


def check_cards_have_ax_evidence(frame, label):
    """Every card's app must have answered AX about some window -- or failed to answer at all.

    `kAXWindows` is Space-filtered, so a real window of another desktop is absent from it by
    construction; only an app AX named *something* about (its published list or its key/main slots)
    can be reasoned about that way. An app AX answered about with nothing at all has no switchable
    window to show, and its CG entries are panels: Stats keeps a closed 280x800 menu-bar panel
    titled "Combined modules" that used to become a card on its own desktop *and* across Spaces.
    A failed AX query is a different state (nothing is known), so those pids are exempt.
    """
    published = set(frame.get("ax_published_pids") or [])
    recovered = set(frame.get("ax_recovered_pids") or [])
    failed = set(frame.get("ax_failed_pids") or [])
    offenders = [
        (c["pid"], c["app"], c["window_id"], c["title"], c.get("other_desktop"))
        for c in frame.get("cards", [])
        if c["pid"] not in published and c["pid"] not in recovered and c["pid"] not in failed
    ]
    check(not offenders,
          f"{label}: every card's app answered AX about a window",
          f"offenders={offenders}")


def check_ax_identity_invariant(frame, label):
    """An app whose `kAXWindows` answer was empty must not have windows invented from the CG list.

    This is the rule the 微信 duplicate violated: with its list Space-filtered but its key/main slots
    still naming the real window, its off-screen second window (never named by AX) appeared beside it
    as a second, dead card. The condition mirrors the implementation exactly: it applies only to apps
    *without* a published window this pass (`ax_published_pids`), and a window on a fullscreen Space
    is exempt, because the contract shows those whatever the switch says (2026-10-07).
    """
    published = set(frame.get("ax_published_pids") or [])
    recovered = set(frame.get("ax_recovered_pids") or [])
    fullscreen = fullscreen_window_ids(
        frame.get("spaces"), cua("list_windows", {}).get("windows", [])
    )
    by_pid: dict[int, list[dict]] = {}
    for card in frame.get("cards", []):
        by_pid.setdefault(card["pid"], []).append(card)
    for pid, group in by_pid.items():
        # Mirror the implementation exactly: the rule applies when the published list is empty AND
        # the key/main slots named a window. Without that second condition AX has no evidence at all
        # and the CG fallback is the intended behaviour, so nothing is asserted.
        if pid in published or pid not in recovered:
            continue
        identified = [c for c in group if c.get("ax_identified")]
        if not identified:
            continue
        unverified = [
            c for c in group if not c.get("ax_identified") and c["window_id"] not in fullscreen
        ]
        check(not unverified,
              f"{label}: no window invented for an app whose AX list is Space-filtered",
              f"pid={pid} identified={[c['window_id'] for c in identified]} "
              f"invented={[(c['window_id'], c['title']) for c in unverified]} "
              f"fullscreen={sorted(fullscreen)}")



# Probe several apps, not one: a single reference can be a process that legitimately has no focused
# window (a menu-bar app, or one that just exited), and treating that as "this script cannot read
# AX" would silently downgrade a real focus assertion to NOT RUN.
probe_pids = [reference_pid]
try:
    for window in cua("list_windows", {}).get("windows", []):
        if window.get("is_on_screen") and window.get("pid") not in probe_pids:
            probe_pids.append(window["pid"])
except Exception:
    pass
ax_capable = any(ax_focused_window_id(pid) is not None for pid in probe_pids[:6])
if not ax_capable:
    notes.append(
        "final-focus check NOT RUN: AXFocusedWindow is unreadable for the reference app and for "
        f"every on-screen app tried ({probe_pids[:6]}) -- Accessibility permission for this script?"
    )

try:
    frame = summon()
    check(frame.get("space_filter", {}).get("show_other_desktops") is True,
          "the switch reads on after the setting is written", json.dumps(frame.get("space_filter")))
    check_ax_identity_invariant(frame, "origin desktop")
    check_cards_have_ax_evidence(frame, "origin desktop")
    check_failed_app_cards_are_visible(frame, "origin desktop")
    target = next((c for c in frame.get("cards", []) if c["window_id"] == target_wid), None)
    # A window that is on the *origin* desktop right now: committing it later is how the scenario
    # goes back, which is also what gives the target its cached thumbnail.
    origin_card = next(
        (c for c in frame.get("cards", [])
         if not c["other_desktop"] and not c["minimized"] and c["window_id"] != target_wid),
        None,
    )
    target_problems_before = len(problems)
    check(target is not None, "the other-desktop window is a card once the switch is on",
          f"cards={[(c['window_id'], c['other_desktop']) for c in frame.get('cards', [])]}")
    if target is None:
        # Unrun only on the app's own recorded reason for *this* window: `ax_excluded` is the accepted
        # narrowing (the app's AX answer named a window on the target's own Space and never named the
        # target, so its desktop has not been visited in this process). Every other reason -- space,
        # title, shape, unpaired -- or no recorded decision at all keeps the failure, so a real
        # admission regression cannot hide behind NOT RUN.
        reason = target_rejection_reason(frame, target_wid)
        if reason == "ax_excluded":
            del problems[target_problems_before:]
        # Without the narrowing evidence the target check's own failure stays in `problems`, so the
        # helper exits non-zero instead of reporting an unrun phase.
        unrun_or_fail(
            checks,
            problems,
            "the chosen cross-desktop window is not admitted yet (its desktop has not been visited "
            f"in this process); reason={reason} "
            f"cards={[(c['window_id'], c['other_desktop']) for c in frame.get('cards', [])]}",
        )
    check(target["other_desktop"], "its card is marked as another desktop",
          f"other_desktop={target['other_desktop']}")
    check(target["pid"] == target_pid, "the card keeps the exact window identity",
          f"pid={target['pid']} wid={target['window_id']}")
    check(bool(target["title"]), "the card carries the CG title", repr(target["title"]))

    # Capture eligibility, asserted on the summon's own candidate set rather than on the card flag.
    # The set is computed after the summon frame is written, so wait for it to appear; when it never
    # does (thumbnails disabled, or Screen Recording not granted) the check cannot be made and is
    # reported as NOT RUN instead of being counted as a pass.
    workset_frame = None
    deadline = time.time() + 3
    while time.time() < deadline:
        snapshot = read_state() or {}
        if snapshot.get("thumbnail_workset"):
            workset_frame = snapshot
            break
        time.sleep(0.05)
    if workset_frame is None:
        notes.append(
            "capture-eligibility check NOT RUN: no summon-time capture set "
            "(thumbnails disabled or Screen Recording not granted)"
        )
    else:
        workset_keys = [k[1] for k in workset_frame["thumbnail_workset"]]
        check(target_wid not in workset_keys,
              "the other-desktop window is never a capture candidate",
              f"workset={sorted(workset_keys)}")
        other_desktop_wids = {c["window_id"] for c in frame.get("cards", []) if c["other_desktop"]}
        check(not (other_desktop_wids & set(workset_keys)),
              "no other-desktop card is a capture candidate",
              f"other_desktop={sorted(other_desktop_wids)} workset={sorted(workset_keys)}")

    # --- step the selection onto its card, then commit with the modifier release -----------
    cards = frame.get("cards", [])
    steps = 0
    # Repeats inside the app's 140ms hold cadence are deliberately swallowed: space them out.
    while steps <= len(cards) + 2:
        current = read_state()
        if current.get("selected_index") == target["index"]:
            break
        post(VK_TAB, True, FLAG_OPTION, 1)
        time.sleep(0.2)
        steps += 1
    selected = read_state()
    check(selected.get("selected_index") == target["index"],
          "the selection reached the other-desktop card",
          f"selected={selected.get('selected_index')} want={target['index']}")

    before = seq()
    raise_state_before = (read_state() or {}).get("other_desktop_raise") or {}
    raise_count_before = raise_state_before.get("count", 0)
    post(VK_OPTION, False, 0)
    post(VK_TAB, False, 0)
    commit = wait(lambda s: s.get("event") == "commit" and s.get("committed"),
                  3, "the commit frame", after_seq=before)
    committed = commit.get("committed") or {}
    check(committed.get("window_id") == target_wid and committed.get("pid") == target_pid,
          "the committed window is the exact selected one", json.dumps(committed))

    # The raise happens after the commit frame; its counters are the assertable record (a dedicated
    # frame can be overwritten by the keystroke stream, so read the counters from a later frame).
    # The count must move past what this run had already recorded, so a stale counter from an
    # earlier commit in the same process cannot satisfy the check.
    deadline = time.time() + 5
    raise_state = None
    while time.time() < deadline:
        snapshot = read_state() or {}
        state = snapshot.get("other_desktop_raise") or {}
        if (
            state.get("count", 0) > raise_count_before
            and state.get("pid") == target_pid
            and state.get("window_id") == target_wid
        ):
            raise_state = state
            break
        time.sleep(0.05)
    check(bool(raise_state),
          "the app recorded a cross-desktop raise bound to this exact target",
          f"count_before={raise_count_before}")
    if raise_state:
        check(raise_state.get("onscreen") is True,
              "the target window was on the active desktop before the exact raise",
              json.dumps(raise_state))
        check(raise_state.get("ax_matched") is True,
              "the exact window was matched and raised through AX", json.dumps(raise_state))
        # Which attempt moved the Space is published. Either branch is legitimate here (macOS can
        # refuse activation, or accept it and still land the switch late), so this only requires
        # that something was attempted; the deterministic rescue-only coverage is phase 4.
        check(raise_state.get("activation") is True
              or raise_state.get("rescue_attempted") is True,
              "the record says which attempt moved the Space", json.dumps(raise_state))
        notes.append(
            "branch used: activation=%s rescue_attempted=%s onscreen=%s"
            % (raise_state.get("activation"), raise_state.get("rescue_attempted"),
               raise_state.get("onscreen"))
        )

    # --- reconcile against real WindowServer and AX state -----------------------------------
    time.sleep(1.0)
    windows = cua("list_windows", {})
    target_window = next((w for w in windows["windows"] if w["window_id"] == target_wid), None)
    check(target_window is not None, "the target window is still in the WindowServer list")
    if target_window:
        check(target_window.get("on_current_space") is True,
              "the active desktop is now the target's desktop",
              f"current_space={windows.get('current_space_id')} spaces={target_window.get('space_ids')}")
    apps = cua("list_apps", {"include_installed": False}).get("apps", [])
    active = [a for a in apps if a.get("active")]
    check(bool(active) and active[0]["pid"] == target_pid,
          "the target app is frontmost", f"active={active[0] if active else None}")

    # The exact window, not just its app or its desktop: read the app's real focused window through
    # AX and map it back to a CGWindowID. Request acceptance and API return codes cannot show this.
    #
    # Read the *capability* first: without Accessibility permission for this script every read fails,
    # which is a NOT RUN; with it, a focus read that comes back empty or wrong is a real failure and
    # must not be excused as environment trouble.
    if not ax_capable:
        pass  # already reported as NOT RUN before the run started
    else:
        focused_wid = ax_focused_window_id(target_pid)
        check(focused_wid is not None,
              "the target app reports a focused window",
              f"focused={focused_wid}")
        check(focused_wid == target_wid,
              "the app's focused window is the exact selected window",
              f"focused={focused_wid} want={target_wid}")

    # --- phase 3: back to the origin desktop, and the target keeps its thumbnail ---------------
    # Reaching this point means the app was on the target's desktop long enough to capture it there
    # (it is a current-desktop card there). Coming back makes that card an other-desktop one again,
    # and the bug this pins was that such a card rendered no thumbnail although the frame existed.
    if origin_card is None:
        notes.append("thumbnail-kept check NOT RUN: no non-minimized origin-desktop card to switch back to")
    else:
        # Coming back is incidental to the bug under test: which card the app commits depends on
        # the list order at release time, and a commit inside a Space transition can be discarded.
        # Retry the return until one lands, then assert the two things that matter below.
        returned = False
        for _ in range(3):
            back = summon()
            back_target = next(
                (c for c in back.get("cards", []) if c["window_id"] == origin_card["window_id"]),
                None,
            )
            if back_target is None:
                post(VK_ESCAPE, True, FLAG_OPTION)
                post(VK_ESCAPE, False, FLAG_OPTION)
                time.sleep(0.5)
                continue
            steps_back = 0
            while steps_back <= len(back.get("cards", [])) + 2:
                current = read_state()
                if current.get("selected_index") == back_target["index"]:
                    break
                post(VK_TAB, True, FLAG_OPTION, 1)
                time.sleep(0.2)
                steps_back += 1
            before_back = seq()
            post(VK_OPTION, False, 0)
            post(VK_TAB, False, 0)
            try:
                wait(lambda s: s.get("event") == "commit" and s.get("committed"),
                     3, "the return commit frame", after_seq=before_back)
                returned = True
                break
            except AssertionError:
                time.sleep(0.5)
        check(returned, "a return commit landed")
        if returned:
            time.sleep(1.0)
            # Back on the origin desktop: confirm the Space really returned, then re-summon and
            # check the *rendered* thumbnail. `thumbnail_ready` alone would still pass if the card
            # refused the cached frame -- the cache entry survives that refusal, and that is exactly
            # the defect the user reported. `thumbnail_rendered` is written by the renderer's own
            # branch.
            returned_windows = cua("list_windows", {})
            origin_window = next(
                (w for w in returned_windows["windows"]
                 if w["window_id"] == origin_card["window_id"]),
                None,
            )
            check(origin_window is not None and origin_window.get("on_current_space") is True,
                  "the scenario is back on the origin desktop",
                  f"current_space={returned_windows.get('current_space_id')}")
            # The Space context can lag the switch by a refresh or two, so a first summon may still
            # classify the target as current-group. Retry (cancelling each attempt) until the
            # classification settles; the thumbnail assertions below need the same card.
            after_card = None
            for _ in range(6):
                frame_after = summon()
                check_ax_identity_invariant(frame_after, "returned origin desktop")
                check_cards_have_ax_evidence(frame_after, "returned origin desktop")
                check_failed_app_cards_are_visible(frame_after, "returned origin desktop")
                after_card = next(
                    (c for c in frame_after.get("cards", []) if c["window_id"] == target_wid), None
                )
                if after_card is not None and after_card.get("other_desktop") is True:
                    break
                post(VK_ESCAPE, True, FLAG_OPTION)
                post(VK_ESCAPE, False, FLAG_OPTION)
                time.sleep(0.5)
            check(after_card is not None and after_card.get("other_desktop") is True,
                  "the target is an other-desktop card again", f"card={after_card}")
            rendered = False
            # The cached frame of a cross-Space window is attached on the app's own schedule, so give
            # it a second summon before judging: one frame that had not rendered yet used to fail the
            # phase even though the next summon rendered it.
            for attempt in range(2):
                deadline = time.time() + 5
                while time.time() < deadline:
                    snapshot = read_state() or {}
                    card = next(
                        (c for c in snapshot.get("cards", []) if c["window_id"] == target_wid), None
                    )
                    if card is not None and card.get("thumbnail_rendered"):
                        rendered = True
                        break
                    time.sleep(0.1)
                if rendered:
                    break
                # Dismiss and summon again, so a fresh show pass can attach the cached frame.
                post(VK_ESCAPE, True, FLAG_OPTION)
                post(VK_ESCAPE, False, FLAG_OPTION)
                time.sleep(0.4)
                before_retry = seq()
                post(VK_OPTION, True, FLAG_OPTION)
                post(VK_TAB, True, FLAG_OPTION)
                try:
                    wait(
                        lambda s: s.get("event") == "summon" and s.get("cards_count", 0) >= 2,
                        3,
                        "the overlay to summon again",
                        after_seq=before_retry,
                    )
                except AssertionError:
                    break
            snapshot = read_state() or {}
            final_card = next(
                (c for c in snapshot.get("cards", []) if c["window_id"] == target_wid), None
            )
            post(VK_ESCAPE, True, FLAG_OPTION)
            post(VK_ESCAPE, False, FLAG_OPTION)
            time.sleep(0.2)
            check(rendered,
                  "the other-desktop card renders the thumbnail captured on its own desktop",
                  f"ready={final_card.get('thumbnail_ready') if final_card else None} "
                  f"rendered={final_card.get('thumbnail_rendered') if final_card else None}")


finally:
    release_all()

print("\n".join(checks))
for note in notes:
    print(note)
if problems:
    print("\n".join(problems), file=sys.stderr)
    raise SystemExit(1)
PY
phase2_status=$?
cat "$phase2_log"
[ "$phase2_status" -eq 0 ] || fail "phase 2 (switch on) assertions failed"

# The final line must not claim more than was checked: a sub-check that could not run (no summon-time
# capture set, no Accessibility permission for the focus read) is reported as NOT RUN, and an unrun
# check is not a passed check.
if grep -q "NOT RUN" "$phase2_log"; then
    note "PASS with NOT RUN sub-checks: the lines above say which; an unrun check is not a passed check"
else
    # --- phase 4: the front-switch rescue, with app activation suppressed -------------------------
# macOS refusing `activateWithOptions:` is the state that left the user's card doing nothing, and it
# cannot be reproduced on demand; the development switch makes that branch deterministic instead. If
# the front-switch rescue cannot move the Space by itself, this phase fails.
echo "e2e: restarting with --other-desktop-no-activation"
rm -f "$state_file" "${state_file%.json}.tmp"
rescue_out="$("$repo_dir/scripts/dev-restart.sh" --other-desktop-no-activation \
    --e2e-state="$state_file" --no-onboarding 2>&1)"
printf '%s\n' "$rescue_out" | grep -q "^restart ok" || {
    printf '%s\n' "$rescue_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up for the rescue phase"
}
printf '%s\n' "$rescue_out" | grep -E "^(restart ok|build-version)" >&2 || true
app_pid="$(printf '%s\n' "$rescue_out" | sed -n 's/^restart ok (app pid \([0-9][0-9]*\).*/\1/p' | head -1)"
[ -n "$app_pid" ] || fail "could not read the app pid for the rescue phase"
rm -f "$state_file" "${state_file%.json}.tmp"

python3 - "$state_file" "$app_pid" "$target_wid" "$target_pid" <<'PY' >"$phase2_log"
import ctypes
import json
import subprocess
import sys
import time

state_file, app_pid, target_wid, target_pid = (
    sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]),
)
problems: list[str] = []
checks: list[str] = []


def check(ok, label, detail=""):
    (checks if ok else problems).append(f"{label}{(': ' + detail) if detail else ''}")


cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
cg.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
cg.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
cg.CGEventSetIntegerValueField.argtypes = [ctypes.c_void_p, ctypes.c_int32, ctypes.c_int64]
cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]

FIELD_KEYCODE = 9
FIELD_AUTOREPEAT = 8
FLAG_OPTION = 0x80000
VK_TAB = 48
VK_OPTION = 58
VK_ESCAPE = 53
HID_EVENT_TAP = 0


def post(keycode, down, flags, autorepeat=0):
    event = cg.CGEventCreateKeyboardEvent(None, keycode, down)
    cg.CGEventSetFlags(event, flags)
    cg.CGEventSetIntegerValueField(event, FIELD_KEYCODE, keycode)
    cg.CGEventSetIntegerValueField(event, FIELD_AUTOREPEAT, autorepeat)
    cg.CGEventPost(HID_EVENT_TAP, event)


def release_all():
    post(VK_TAB, False, 0)
    post(VK_OPTION, False, 0)


def read_state():
    try:
        with open(state_file) as handle:
            snapshot = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return snapshot if snapshot.get("pid") == app_pid else None


def seq():
    snapshot = read_state()
    return snapshot.get("seq", 0) if snapshot else 0


def wait(predicate, timeout, label, after_seq=0):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        snapshot = read_state()
        if snapshot and snapshot.get("seq", 0) > after_seq:
            last = snapshot
            if predicate(snapshot):
                return snapshot
        time.sleep(0.02)
    raise AssertionError(f"timeout waiting for {label}; last={json.dumps(last)[:300] if last else None}")


def summon():
    for _ in range(6):
        before = seq()
        post(VK_OPTION, True, FLAG_OPTION)
        post(VK_TAB, True, FLAG_OPTION)
        try:
            return wait(lambda s: s.get("event") == "summon" and s.get("cards_count", 0) >= 2,
                        1.5, "the overlay to summon", after_seq=before)
        except AssertionError:
            release_all()
            try:
                wait(lambda s: not s.get("visible"), 2, "the overlay to close", after_seq=before)
            except AssertionError:
                pass
            time.sleep(0.3)
    raise AssertionError("the switcher never opened")


def cua(tool, args):
    out = subprocess.run(["cua-driver", tool, json.dumps(args)], capture_output=True, text=True)
    return json.loads(out.stdout)


try:
    frame = summon()
    target = next((c for c in frame.get("cards", []) if c["window_id"] == target_wid), None)
    check(target is not None, "the other-desktop window is a card", f"cards={frame.get('cards')}")
    if target is None:
        raise SystemExit(1)
    before_state = (read_state() or {}).get("other_desktop_raise") or {}
    count_before = before_state.get("count", 0)
    steps = 0
    # Step until the selection reaches the target, waiting for each step to land instead of sleeping a
    # fixed 0.2s, and re-reading the target's index from the *latest* frame each time: with more cards
    # (fullscreen windows are candidates too since 2026-10-07) the target sits further along, and a
    # refresh during the walk can also shift its index -- chasing a stale index made the loop release
    # the modifier before the target was selected, which then never produced a commit frame.
    limit = 2 * len(frame.get("cards", [])) + 6
    while steps <= limit:
        current = read_state() or {}
        current_cards = current.get("cards") or []
        current_target = next(
            (c for c in current_cards if c["window_id"] == target_wid), None
        )
        if current_target is None:
            raise AssertionError(
                f"the target left the card list while stepping; cards={[c['window_id'] for c in current_cards]}"
            )
        if current.get("selected_index") == current_target["index"]:
            break
        previous_index = current.get("selected_index")
        post(VK_TAB, True, FLAG_OPTION, 1)
        steps += 1
        step_deadline = time.time() + 1.0
        while time.time() < step_deadline:
            now = read_state()
            if now and now.get("selected_index") != previous_index:
                break
            time.sleep(0.02)
    before = seq()
    post(VK_OPTION, False, 0)
    post(VK_TAB, False, 0)
    wait(lambda s: s.get("event") == "commit" and s.get("committed"),
         5, "the commit frame", after_seq=before)
    deadline = time.time() + 8
    raise_state = None
    while time.time() < deadline:
        snapshot = read_state() or {}
        state = snapshot.get("other_desktop_raise") or {}
        if (state.get("count", 0) > count_before
                and state.get("pid") == target_pid and state.get("window_id") == target_wid):
            raise_state = state
            break
        time.sleep(0.05)
    check(bool(raise_state), "the rescue phase recorded its raise", json.dumps(raise_state))
    if raise_state:
        check(raise_state.get("activation") is False,
              "app activation was suppressed for this phase", json.dumps(raise_state))
        check(raise_state.get("rescue_attempted") is True,
              "the front-switch rescue ran", json.dumps(raise_state))
        check(raise_state.get("rescue") is True,
              "the rescue reported success", json.dumps(raise_state))
        check(raise_state.get("onscreen") is True,
              "the rescue alone made the target part of the active desktop",
              json.dumps(raise_state))
        check(raise_state.get("ax_matched") is True,
              "the exact window was then matched and raised", json.dumps(raise_state))
    time.sleep(1.0)
    windows = cua("list_windows", {})
    window = next((w for w in windows["windows"] if w["window_id"] == target_wid), None)
    check(window is not None and window.get("on_current_space") is True,
          "the active desktop is the target's desktop after the rescue",
          f"current_space={windows.get('current_space_id')}")
finally:
    post(VK_ESCAPE, True, FLAG_OPTION)
    post(VK_ESCAPE, False, FLAG_OPTION)
    release_all()

print("\n".join(checks))
if problems:
    print("\n".join(problems), file=sys.stderr)
    raise SystemExit(1)
PY
rescue_status=$?
cat "$phase2_log"
[ "$rescue_status" -eq 0 ] || fail "phase 4 (front-switch rescue) assertions failed"

note "PASS: with the switch off the other desktop stays hidden; with it on the card appears, is never a capture candidate, and commits to the exact window on its own desktop"
fi
