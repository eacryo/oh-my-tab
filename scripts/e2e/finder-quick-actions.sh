#!/bin/bash
# A2 层 E2E 场景:两个 Finder 快捷操作(最大化 / 小窗口)的真实全局热键 → 新建窗口 → 收敛后的最终尺寸。
#
# 判定者是**脚本**(退出码),不是人:热键经系统事件流注入(cua-driver),窗口尺寸从 AX 真状态读取
# (scripts/e2e/lib/ax_focus.py),期望值来自 app 自己写出的 --e2e-state 字段 finder_quick_action
# (它给出该动作实际 shape 的窗口 CGWindowID、使用的 visibleFrame 与请求的 target)。被测窗口由 app
# 记录的 window_id 指认,而不是从窗口集合的差集推断;baseline 只用于核对这个 id 确实是热键新建的。
# 分层与约定见 AGENTS.md「测试分层」。
#
# 覆盖范围:真实的 chord → 新 Finder 窗口 → 最终 frame;分别断言最大化填满记录的 visible、小窗口
# 等于 small_window_frame(记录的 visible)。不声称覆盖"首次写入未变化、随后延迟收敛"这一分支——
# 那是 A1 层 snap_follow_up/make_pending_snap 的断言。
#
# 前提:已授予辅助功能权限;cua-driver 在运行(自身也需要辅助功能)。本场景会注入真实全局热键并
# 切换前台 app,因此用标记声明;scripts/e2e/run-all.sh 默认跳过,只有 --include-focus 才跑。
#
# 用法:
#   scripts/e2e/finder-quick-actions.sh [config_path]
#
# 退出码:0 = 全部断言通过;非 0 = 失败(逐条打印原因)。
# This scenario injects real global hotkeys and therefore steals focus.
# E2E_STEALS_FOCUS=1
# E2E_REPLACES_INSTANCE=1
# It also rewrites the real ~/.config/oh-my-tab/config.toml (key bindings) and restores it in
# a trap. Restoring afterwards is not the same as being authorised to change it: the two
# disturbances need their own switches, so this declares the second one too.
# E2E_CHANGES_PREFS=1

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
config_file="$HOME/.config/oh-my-tab/config.toml"
if [ "$#" -gt 0 ] && [ -n "$1" ]; then
    config_file="$1"
fi
state_file="/tmp/omt-e2e-finder-quick-actions.json"
state_tmp="/tmp/omt-e2e-finder-quick-actions.json.tmp"
created_windows="/tmp/omt-e2e-finder-created.$$"
max_chord="control+option+e"
small_chord="control+option+shift+e"
backup="$(mktemp /tmp/omt-e2e-config.XXXXXX)"
had_config=0
config_touched=0

fail() { echo "e2e finder-quick-actions: FAIL: $*" >&2; exit 1; }

command -v cua-driver >/dev/null 2>&1 || fail "cua-driver CLI not found in PATH"
cua-driver status >/dev/null 2>&1 || fail "cua-driver daemon is not running"

finder_pid="$(pgrep -x Finder | head -1)"
[ -n "$finder_pid" ] || fail "Finder is not running"

# Cleanup is installed before anything modifies state. config_touched is the phase latch: a failure
# while backing up or before the rewrite leaves the user's config alone; once it is set, every exit
# path stops the test instance, restores the backup and leaves a normal instance behind.
cleanup() {
    local status=$?
    local cleanup_failed=0

    if [ "$config_touched" = "1" ]; then
        # Stop the test instance and confirm it is gone: an orderly exit would write its in-memory
        # temporary config back to disk and overwrite the restore below.
        # Stop this build under the launcher's own transaction claim (bootout, identity check, TERM, and a
        # --check relaunch that leaves nothing running). A name-based kill here would also reach the
        # packaged install under /Applications, which shares the executable name.
        if ! "$repo_dir/scripts/dev-restart.sh" --check >/dev/null 2>&1; then
            echo "e2e finder-quick-actions: the test instance did not stop; leaving the config untouched" >&2
            cleanup_failed=1
        fi
        if pgrep -f "$repo_dir/dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab" >/dev/null 2>&1; then
            echo "e2e finder-quick-actions: the test instance did not exit; leaving the config untouched" >&2
            cleanup_failed=1
        else
            if [ "$had_config" = "1" ]; then
                cp "$backup" "$config_file" || { echo "e2e finder-quick-actions: config restore failed" >&2; cleanup_failed=1; }
            else
                rm -f "$config_file"
            fi
            if ! "$repo_dir/scripts/dev-restart.sh" --no-onboarding >/dev/null 2>&1; then
                echo "e2e finder-quick-actions: could not restart the normal instance" >&2
                cleanup_failed=1
            fi
        fi
    fi

    # Only the windows this scenario registered (absent from a *complete* baseline) are closed.
    if [ -s "$created_windows" ]; then
        python3 -B - "$repo_dir" "$created_windows" "$finder_pid" <<'PY'
import os, sys
sys.path.insert(0, os.path.join(sys.argv[1], "scripts/e2e/lib"))
import ax_focus

created_path, finder_pid = sys.argv[2], int(sys.argv[3])
for raw in open(created_path):
    raw = raw.strip()
    if not raw:
        continue
    window_id = int(raw)
    if ax_focus.window_frame(finder_pid, window_id) is None:
        continue
    if ax_focus.close_window(finder_pid, window_id):
        continue
    print("e2e finder-quick-actions: could not close created window " + str(window_id), file=sys.stderr)
    raise SystemExit(1)
PY
        [ "$?" -eq 0 ] || cleanup_failed=1
    fi
    rm -f "$created_windows" "$state_file" "$state_tmp"

    if [ "$status" -ne 0 ] || [ "$cleanup_failed" -ne 0 ]; then
        echo "e2e finder-quick-actions: FAIL (config restored: $([ "$cleanup_failed" -eq 0 ] && echo yes || echo no))" >&2
        echo "e2e finder-quick-actions: config backup kept at $backup" >&2
        exit 1
    fi
    rm -f "$backup"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# 1) 备份用户 config,并只重写 [key_bindings](其余 section 原样保留)。
if [ -f "$config_file" ]; then
    cp "$config_file" "$backup" || fail "could not back up $config_file"
    had_config=1
else
    mkdir -p "$(dirname "$config_file")"
fi
# 阶段闩在任何可能修改配置的写之前置位:即使 INT/TERM 恰好落在重写中间,cleanup 也必须恢复。
config_touched=1
if [ "$had_config" = "0" ]; then
    : > "$config_file"
fi
python3 -B "$repo_dir/scripts/e2e/lib/config_switch.py" --set-bindings "$config_file" \
    "quick.open_finder=$max_chord" "quick.open_finder_small=$small_chord" \
    || fail "could not write the test bindings into $config_file"

# 2) 启动开发版(不带引导);pid 用于把快照限定在本次实例。
rm -f "$state_file" "$state_tmp" "$created_windows"
restart_out="$("$repo_dir/scripts/dev-restart.sh" --no-onboarding --e2e-state="$state_file" 2>&1)"
echo "$restart_out" | grep -q "^restart ok" || {
    echo "$restart_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up"
}
echo "$restart_out" | grep -E "^(restart ok|build-version|app args)" || true
app_pid="$(printf '%s' "$restart_out" | sed -n 's/^restart ok (app pid \([0-9][0-9]*\).*/\1/p' | head -1)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"
rm -f "$state_file" "$state_tmp"

# 3) 注入真实热键并断言最终尺寸。cua 的 scope=desktop 走系统事件流(route=global_input);
#    全局事件 tap 只认系统级事件,注入给某个 pid 的组合它看不到。
python3 -B - "$repo_dir" "$created_windows" "$finder_pid" "$max_chord" "$small_chord" "$state_file" "$app_pid" <<'PY'
import json, os, subprocess, sys, time

(
    repo_dir,
    created_path,
    finder_pid,
    max_chord,
    small_chord,
    state_file,
    app_pid,
) = (
    sys.argv[1],
    sys.argv[2],
    int(sys.argv[3]),
    sys.argv[4],
    sys.argv[5],
    sys.argv[6],
    int(sys.argv[7]),
)
sys.path.insert(0, os.path.join(repo_dir, "scripts/e2e/lib"))
import ax_focus  # noqa: E402
import state_json  # noqa: E402

# The same 0.6 / 480x320 contract the app implements in small_window_frame.
_FRACTION, _MIN_W, _MIN_H = 0.6, 480.0, 320.0
_TOLERANCE = 2.0


def small_window_frame(visible):
    x, y, w, h = visible
    target_w = min(max(w * _FRACTION, _MIN_W), w)
    target_h = min(max(h * _FRACTION, _MIN_H), h)
    return (x + (w - target_w) / 2.0, y + (h - target_h) / 2.0, target_w, target_h)


def rect_of(raw):
    return (raw["x"], raw["y"], raw["w"], raw["h"])


def send_hotkey(chord):
    shorthand = {"command": "cmd", "alt": "option", "control": "ctrl"}
    keys = [shorthand.get(part, part) for part in chord.lower().split("+") if part]
    reply = subprocess.run(
        ["cua-driver", "call", "hotkey",
         json.dumps({"keys": keys, "scope": "desktop", "delivery_mode": "foreground"})],
        capture_output=True, text=True,
    )
    try:
        payload = json.loads(reply.stdout)
    except json.JSONDecodeError:
        raise SystemExit(
            "e2e finder-quick-actions: FAIL: hotkey reply is not JSON: "
            + reply.stdout[:200] + reply.stderr[:200]
        )
    if payload.get("route") != "global_input":
        raise SystemExit(
            "e2e finder-quick-actions: FAIL: hotkey was not delivered through global_input: "
            + json.dumps(payload)
        )


def read_action():
    try:
        with open(state_file) as handle:
            snapshot = state_json.load_state(handle)
    except (FileNotFoundError, json.JSONDecodeError, ValueError):
        return None
    if snapshot.get("pid") != app_pid:
        return None
    return snapshot.get("finder_quick_action")


def wait_action(mode, timeout=10.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        action = read_action()
        if action and action.get("mode") == mode:
            return action
        time.sleep(0.1)
    raise SystemExit("e2e finder-quick-actions: FAIL: no " + mode + " state appeared")


def wait_recorded_window(action, baseline, timeout=10.0):
    # The app recorded the exact CGWindowID it shaped, so the window under test is named rather than
    # inferred from a set difference. The baseline is still checked: an id the hotkey did not create
    # would aim every later assertion at the wrong window.
    window_id = int(action.get("window_id") or 0)
    if window_id == 0:
        raise SystemExit("e2e finder-quick-actions: FAIL: the app recorded no window id")
    if window_id in baseline:
        raise SystemExit("e2e finder-quick-actions: FAIL: the recorded window id predates the hotkey")
    deadline = time.time() + timeout
    while time.time() < deadline:
        if window_id in ax_focus.window_ids(finder_pid):
            with open(created_path, "a") as handle:
                handle.write(str(window_id) + "\n")
            return window_id
        time.sleep(0.15)
    raise SystemExit("e2e finder-quick-actions: FAIL: the recorded window id never appeared")


def wait_frame(window_id, expected, timeout=12.0):
    # Settled means the last two reads both match the target, not merely that they are equal:
    # an animation pause can produce equal intermediate reads (see ax_focus.target_reached).
    deadline = time.time() + timeout
    readings = []
    while time.time() < deadline:
        frame = ax_focus.window_frame(finder_pid, window_id)
        if frame is not None:
            readings.append(frame)
            readings = readings[-2:]
            if ax_focus.target_reached(readings, expected, _TOLERANCE):
                return frame
        time.sleep(0.15)
    raise SystemExit("e2e finder-quick-actions: FAIL: the window never reached its target frame")


baseline, complete = ax_focus.window_ids_checked(finder_pid)
if not complete:
    raise SystemExit("e2e finder-quick-actions: FAIL: could not establish a complete Finder baseline")

send_hotkey(max_chord)
max_action = wait_action("maximized")
max_id = wait_recorded_window(max_action, baseline)
max_visible = rect_of(max_action["visible"])
max_target = rect_of(max_action["target"])
max_frame = wait_frame(max_id, max_target)
if not ax_focus.rect_close(max_target, max_visible, _TOLERANCE):
    raise SystemExit("e2e finder-quick-actions: FAIL: maximize target does not fill the work area")
if not ax_focus.rect_close(max_frame, max_target, _TOLERANCE):
    raise SystemExit("e2e finder-quick-actions: FAIL: maximized window frame != recorded target")

baseline, complete = ax_focus.window_ids_checked(finder_pid)
if not complete:
    raise SystemExit("e2e finder-quick-actions: FAIL: could not re-establish the Finder baseline")

send_hotkey(small_chord)
small_action = wait_action("small")
small_id = wait_recorded_window(small_action, baseline)
small_visible = rect_of(small_action["visible"])
small_target = rect_of(small_action["target"])
small_frame = wait_frame(small_id, small_target)

expected = small_window_frame(small_visible)
if not ax_focus.rect_close(small_target, expected, _TOLERANCE):
    raise SystemExit("e2e finder-quick-actions: FAIL: recorded target != small_window_frame(visible)")
if not ax_focus.rect_close(small_frame, small_target, _TOLERANCE):
    raise SystemExit("e2e finder-quick-actions: FAIL: small window frame != recorded target")
if small_target[2] > small_visible[2] + _TOLERANCE or small_target[3] > small_visible[3] + _TOLERANCE:
    raise SystemExit("e2e finder-quick-actions: FAIL: small target exceeds the work area")
if small_visible[2] > _MIN_W and small_visible[3] > _MIN_H:
    if not (small_target[2] < small_visible[2] and small_target[3] < small_visible[3]):
        raise SystemExit("e2e finder-quick-actions: FAIL: small target is not strictly smaller")

print("e2e: maximize " + str(max_id) + " visible=" + str(max_visible) + " frame=" + str(max_frame))
print("e2e: small    " + str(small_id) + " visible=" + str(small_visible) + " frame=" + str(small_frame))
print("e2e finder-quick-actions: ok")
PY
status=$?
[ "$status" -eq 0 ] || exit "$status"
