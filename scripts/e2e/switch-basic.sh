#!/bin/bash
# A2 层 E2E 场景:真实全局热键 → 浮窗 → 抬窗。
#
# 判定者是**脚本**(退出码),不是人:输入由 cua-driver CLI 注入,断言全部读 app 自己写的
# JSON 快照(`--e2e-state=<path>`)与 WindowServer 的真实状态,不依赖截图目视。
# 分层与约定见 AGENTS.md「测试分层」。
#
# 本场景覆盖的是**真实输入路径**:热键经系统事件流被全局 tap 截获 → 选择 → 抬窗,并让
# macOS 侧对账(前台 pid / WindowServer 里的窗口)。浮窗的"显示帧"是否出现取决于按放间隔:
# 快速按放时 release 落在首帧之前,浮窗不显示(真实世界最常见的快速切换),此时只有 commit 帧;
# 显示路径的布局/控件断言属于 A1 层(`--smoke-overlay` / `--smoke-settings-layout`),不在本场景。
#
# 用法:
#   scripts/e2e/switch-basic.sh [--hotkey=cmd+tab] [--no-launch-target]
#   --hotkey=…            触发键组合,默认 cmd+tab(需与 app 的 switcher 触发键一致)
#   --no-launch-target    不额外拉起 TextEdit 做目标(需要机器上已有 ≥2 个可切换窗口)
#
# 前置:已授予辅助功能权限(否则 app 拿不到事件 tap);cua-driver 在运行(其自身也需要
# 辅助功能 + 录屏权限)。注意:热键走系统事件流,运行期间会真的切换前台 app。
#
# 本场景会注入真实全局热键(抢焦点),故用标记声明;scripts/e2e/run-all.sh 默认跳过它。
# This scenario injects a real global hotkey and therefore steals focus; the marker below makes
# scripts/e2e/run-all.sh skip it unless --include-focus is given.
# E2E_STEALS_FOCUS=1
#
# Exit code: 0 = 全部断言通过;非 0 = 失败(逐条打印原因)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
state_file="/tmp/omt-e2e-switch.json"
hotkey="cmd+tab"
launch_target=1

for arg in "$@"; do
    case "$arg" in
        --hotkey=*) hotkey="${arg#--hotkey=}" ;;
        --no-launch-target) launch_target=0 ;;
        -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

fail() { echo "e2e switch-basic: FAIL: $*" >&2; exit 1; }

command -v cua-driver >/dev/null 2>&1 || fail "cua-driver CLI not found in PATH"
cua-driver status >/dev/null 2>&1 || fail "cua-driver daemon is not running"

# 热键按 "cmd+tab" 写法转成 cua 的 keys 数组:修饰键在前,一个非修饰键在最后。
# Turn "cmd+tab" into cua's keys array: modifiers first, one non-modifier last.
keys_json="$(python3 - "$hotkey" <<'PY'
import json, sys
parts = [p for p in sys.argv[1].lower().replace("-", "+").split("+") if p]
shorthand = {"command": "cmd", "alt": "option", "control": "ctrl"}
print(json.dumps([shorthand.get(p, p) for p in parts]))
PY
)" || fail "could not parse --hotkey=$hotkey"

# 1) 干净起点:快照文件必须不存在,否则第一步断言会读到上一轮的帧。
#    Clean start: the snapshot must not exist, or the first assertion reads a stale frame.
rm -f "$state_file" "${state_file%.json}.tmp"

# 2) 需要 ≥2 个可切换窗口:目标 app 在后台拉起(不抢前台),它自己会开一个窗口。
#    Two switchable windows are required; the target app is launched in the background (no focus
#    steal) and opens its own window.
if [ "$launch_target" = "1" ]; then
    echo "e2e: launching a background target window (TextEdit)"
    cua-driver call launch_app '{"name":"TextEdit"}' >/dev/null 2>&1 \
        || fail "could not launch TextEdit as a target"
    sleep 1
fi

# 3) 带快照开关启动开发版(不带引导,避免模态窗口干扰场景)。
#    Start the dev build with snapshots enabled and the guide suppressed.
echo "e2e: starting the dev build with --e2e-state"
restart_out="$("$repo_dir/scripts/dev-restart.sh" --e2e-state="$state_file" --no-onboarding 2>&1)"
echo "$restart_out" | grep -q "^restart ok" || {
    echo "$restart_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up"
}
echo "$restart_out" | grep -E "^(restart ok|build-version|app args)" || true

# 4) 驱动前记录前台 app,便于断言"确实换了"(否则这次切换什么都没证明)。
#    Record the pre-drive frontmost app so "it actually switched" can be asserted.
pre_active="$(cua-driver call list_apps '{"include_installed":false}' | python3 -c '
import json, sys
apps = json.load(sys.stdin)["apps"]
active = [a for a in apps if a.get("active")]
print(active[0]["pid"] if active else "")
')"

# 5) 注入真实热键。scope=desktop 走系统事件流(route=global_input),不是注入给某个 pid——
#    全局事件 tap 只认系统级事件,注入给 pid 的组合它看不到。
#    Inject the real hotkey. scope=desktop goes through the system event stream
#    (route=global_input) instead of injecting into one pid: a global event tap only sees
#    system-level events.
echo "e2e: sending $hotkey (scope=desktop)"
hotkey_reply="$(cua-driver call hotkey "{\"keys\":$keys_json,\"scope\":\"desktop\",\"delivery_mode\":\"foreground\"}")"
# 用 JSON 解析而不是 grep:裸 grep 依赖序列化的空格,驱动端一改成紧凑输出就会误报。
# Parse the JSON instead of grepping it: a bare grep depends on the exact serialization spacing
# and would misreport as soon as the driver emits compact output.
python3 - "$hotkey_reply" <<'PY' || exit 1
import json, sys
try:
    reply = json.loads(sys.argv[1])
except json.JSONDecodeError:
    print(f"e2e switch-basic: FAIL: hotkey reply is not JSON: {sys.argv[1][:200]}", file=sys.stderr)
    raise SystemExit(1)
if reply.get("route") != "global_input":
    print(f"e2e switch-basic: FAIL: hotkey was not delivered through global_input: {reply}", file=sys.stderr)
    raise SystemExit(1)
PY

# 6) 断言:读快照 + 与 WindowServer 真状态对账(全在 python 里做)。
#    Assertions: read the snapshot and cross-check real WindowServer state (all in python).
python3 - "$state_file" "$pre_active" <<'PY'
import json, subprocess, sys, time

state_file, pre_active = sys.argv[1], sys.argv[2]
problems: list[str] = []
checks: list[str] = []


def check(ok: bool, label: str, detail: str = "") -> None:
    (checks if ok else problems).append(f"{label}{(': ' + detail) if detail else ''}")


def cua(tool: str, args: dict) -> dict:
    out = subprocess.run(
        ["cua-driver", "call", tool, json.dumps(args)], capture_output=True, text=True
    )
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        raise SystemExit(
            f"cua-driver call {tool} returned no JSON: {out.stdout[:200]}{out.stderr[:200]}"
        )


# --- 等快照:轮询到出现 commit 帧为止(不 sleep 猜时间) ---------------------
deadline = time.time() + 5
frames: list[dict] = []
while time.time() < deadline:
    try:
        with open(state_file) as handle:
            frames.append(json.load(handle))
    except (FileNotFoundError, json.JSONDecodeError):
        pass
    if any(f.get("event") == "commit" and f.get("committed") for f in frames):
        break
    time.sleep(0.05)

seen = [f for f in frames if f.get("seq")]
events = [f.get("event") for f in seen]
check(bool(seen), "app wrote a state snapshot", f"events seen: {events}")
if not seen:
    print("\n".join(problems))
    raise SystemExit(1)

# commit 帧自带 cards 与 selected_index,所以它是本场景的主证据;summon 帧只在浮窗真的
# 显示过时出现(按放够慢),有则用它做更强的自洽断言。
# The commit frame carries cards and selected_index, so it is the main evidence here. A summon
# frame only appears when the overlay was actually displayed (a slow enough press-release); when
# present it enables stronger self-consistency assertions.
commits = [f for f in seen if f.get("event") == "commit" and f.get("committed")]
summons = [f for f in seen if f.get("event") == "summon"]
base = commits[-1] if commits else seen[-1]

check(base["cards_count"] >= 2, "at least two cards in the model", f"cards_count={base['cards_count']}")
check(
    base["selected_index"] < base["cards_count"],
    "selection index is inside the card list",
    f"selected_index={base['selected_index']} cards={base['cards_count']}",
)
check(bool(commits), "release committed a window", f"events seen: {events}")

if commits:
    commit = commits[-1]
    target = commit["committed"]
    check(
        target["index"] == commit["selected_index"],
        "the committed index is the one that was selected",
        f"committed={target['index']} selected={commit['selected_index']}",
    )
    check(
        commit["selected_key"] == {"pid": target["pid"], "window_id": target["window_id"]},
        "the committed window is the selected key",
        f"selected_key={commit['selected_key']} committed={target['pid']}:{target['window_id']}",
    )
    shown = {(c["pid"], c["window_id"]) for c in commit["cards"]}
    check(
        (target["pid"], target["window_id"]) in shown,
        "the committed window was one of the cards",
        f"target={target['pid']}:{target['window_id']}",
    )
    # pre_active 为空说明驱动前没记录到前台 app,那么"确实换了"这条会恒真(空断言)。
    # An empty pre_active means no frontmost app was recorded before driving, which makes the
    # "it actually switched" check vacuously true.
    check(bool(pre_active), "recorded the frontmost app before driving")
    check(
        str(target["pid"]) != str(pre_active),
        "the switch changed the frontmost app",
        f"before={pre_active} target={target['pid']}",
    )
    if summons:
        check(
            summons[-1]["visible"],
            "the overlay was visible in the display frame",
            f"selected_index={summons[-1]['selected_index']}",
        )
        check(
            summons[-1]["selected_index"] == target["index"],
            "the highlighted card is the one that got raised",
            f"shown={summons[-1]['selected_index']} committed={target['index']}",
        )

    # --- OS 侧对账:前台 app + WindowServer 里的真实窗口 -------------------
    time.sleep(0.6)
    apps = cua("list_apps", {"include_installed": False})["apps"]
    active = [a for a in apps if a.get("active")]
    active_pid = active[0]["pid"] if active else None
    check(
        active_pid == target["pid"],
        "macOS agrees the committed pid is frontmost",
        f"active={active_pid} committed={target['pid']}",
    )
    windows = cua("list_windows", {})["windows"]
    match = [
        w for w in windows if w["pid"] == target["pid"] and w["window_id"] == target["window_id"]
    ]
    check(
        bool(match) and match[0]["is_on_screen"],
        "the committed window exists in WindowServer and is on screen",
        f"pid windows={[(w['window_id'], w['is_on_screen']) for w in windows if w['pid'] == target['pid']]}",
    )

print(f"  info display frame seen: {bool(summons)} (quick press-release skips the display path)")
for label in checks:
    print(f"  ok   {label}")
for label in problems:
    print(f"  FAIL {label}")
total = len(checks) + len(problems)
print(f"e2e switch-basic: {'PASS' if not problems else 'FAIL'} ({len(checks)}/{total} checks)")
raise SystemExit(1 if problems else 0)
PY
status=$?
if [ "$status" -eq 0 ]; then
    echo "e2e: snapshot kept at $state_file (dev app left running; stop with: pkill -f Oh-My-Tab-Dev)"
fi
exit "$status"
