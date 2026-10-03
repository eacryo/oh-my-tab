#!/bin/bash
# A2 层 E2E 场景:全屏窗口的来源桌面归属组。
#
# 判定者是**脚本**(退出码),不是人:输入由 cua-driver CLI 注入,断言读 app 自己写的 JSON
# 快照(`--e2e-state=<path>`)里真实跑出来的候选卡片。分层与约定见 AGENTS.md「测试分层」。
#
# 覆盖的真实路径:从一个普通桌面把一个窗口切到全屏(WindowServer 1325/1326 + Space 拓扑查询),
# app 必须在该窗口离开普通桌面、加入全屏 Space 后确认来源,然后让来源桌面的普通窗口与这个全屏
# 窗口共享候选组 —— 两个方向都要成立:
#   * 在全屏 Space 里切换 → 能看到来源桌面的普通窗口;
#   * 回到普通桌面切换 → 能看到这个来源明确的全屏窗口。
# 来源未知的全屏 Space 仍然隔离(只显示它自己的窗口),该分支由 `src/space_groups.rs` 的纯测试覆盖,
# 本场景不制造第二个桌面,不把它算作已验证。
#
# 用法:
#   scripts/e2e/space-groups.sh
#
# 前置:已授予辅助功能权限;cua-driver 在运行(自身也需要辅助功能 + 录屏权限)。
# 本场景会真的切到全屏、再切回桌面(抢焦点),故用标记声明;scripts/e2e/run-all.sh 默认跳过它。
# This scenario enters a real fullscreen Space and switches back to the desktop, so it steals focus.
# E2E_STEALS_FOCUS=1
#
# Exit code: 0 = 全部断言通过;非 0 = 失败(逐条打印原因)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
state_file="/tmp/omt-e2e-space-groups.json"
title_fullscreen="omt-space-groups-a.txt"
title_plain="omt-space-groups-b.txt"
file_fullscreen="/tmp/$title_fullscreen"
file_plain="/tmp/$title_plain"

fail() { echo "e2e space-groups: FAIL: $*" >&2; exit 1; }

command -v cua-driver >/dev/null 2>&1 || fail "cua-driver CLI not found in PATH"
cua-driver status >/dev/null 2>&1 || fail "cua-driver daemon is not running"

close_targets() {
    osascript -e "tell application \"TextEdit\" to close (every window whose name is \"$title_fullscreen\") saving no" >/dev/null 2>&1
    osascript -e "tell application \"TextEdit\" to close (every window whose name is \"$title_plain\") saving no" >/dev/null 2>&1
    rm -f "$file_fullscreen" "$file_plain"
}
# 失败也清理:场景真的会改桌面/全屏状态,不能让测试窗口留在用户桌面上。
trap close_targets EXIT

# Kill any earlier dev instance and wait for it to go before clearing the snapshot: the app writes
# the file on its own events, so a still-running process would recreate it right after the rm and a
# scenario waiting for "a newer frame" would then compare against the old process's counter.
pkill -f 'Oh-My-Tab-Dev' >/dev/null 2>&1 || true
for _ in $(seq 1 40); do
    pgrep -f 'Oh-My-Tab-Dev' >/dev/null 2>&1 || break
    sleep 0.1
done
rm -f "$state_file" "${state_file%.json}.tmp"
close_targets

# 两个带唯一标题的窗口:A 稍后进全屏,B 留在普通桌面作为"同组普通窗口"的证物。
# Two uniquely titled windows: A goes fullscreen later, B stays on the desktop as the
# "ordinary window in the same group" witness.
printf 'space-groups fullscreen probe %s\n' "$(date +%s)" >"$file_fullscreen"
printf 'space-groups plain probe %s\n' "$(date +%s)" >"$file_plain"
open -a TextEdit "$file_fullscreen"
sleep 1
open -a TextEdit "$file_plain"
sleep 2

# 带快照开关启动开发版(不带引导,避免模态窗口干扰场景)。
echo "e2e: starting the dev build with --e2e-state"
restart_out="$("$repo_dir/scripts/dev-restart.sh" --e2e-state="$state_file" --no-onboarding 2>&1)"
echo "$restart_out" | grep -q "^restart ok" || {
    echo "$restart_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up"
}
echo "$restart_out" | grep -E "^(restart ok|build-version|app args)" || true

# The instance that must be driven is the one dev-restart reports. A launchd job left over from a
# previous run can respawn an older instance while this run starts, and a snapshot read at that
# moment can belong to a process this scenario is not driving. Clearing the snapshot after the
# restart (rather than before it) also removes any frame the older instance wrote.
app_pid="$(printf '%s\n' "$restart_out" \
    | sed -n 's/^restart ok (app pid \([0-9][0-9]*\).*/\1/p' | head -1)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"
rm -f "$state_file" "${state_file%.json}.tmp"

python3 - "$state_file" "$title_fullscreen" "$title_plain" "$app_pid" <<'PY'
import json
import subprocess
import sys
import time

state_file, title_fullscreen, title_plain, app_pid = sys.argv[1:5]
app_pid = int(app_pid)
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


seen_contexts: list = []


def state() -> dict | None:
    try:
        with open(state_file) as handle:
            snapshot = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    if snapshot.get("pid") != app_pid:
        # Not the instance this scenario started; ignore it rather than let an older process's
        # frame satisfy a wait.
        return None
    mark = [(c.get("space"), c.get("kind")) for c in snapshot.get("space_contexts", [])]
    if mark and mark not in seen_contexts:
        seen_contexts.append(mark)
    return snapshot


def wait_state(predicate, timeout: float, label: str) -> dict:
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = state()
        if last and predicate(last):
            return last
        time.sleep(0.05)
    # This scenario drives the real Space and therefore cannot run while someone else is using the
    # machine. Report that condition explicitly instead of a bare timeout: a machine that moved
    # under the test is not evidence about the feature.
    contexts = None if last is None else last.get("space_contexts")
    frontmost = None if last is None else last.get("frontmost")
    raise SystemExit(
        f"e2e space-groups: FAIL: timed out waiting for {label}; the Space context moved under the "
        f"test or the app did not publish the expected frame. last contexts={contexts} "
        f"frontmost={frontmost} contexts_seen={seen_contexts}"
    )


def hotkey(keys: list[str]) -> None:
    reply = cua("hotkey", {"keys": keys, "scope": "desktop", "delivery_mode": "foreground"})
    if reply.get("route") != "global_input":
        raise SystemExit(f"e2e space-groups: FAIL: hotkey {keys} bypassed global_input: {reply}")


def fullscreen_title(pid: int, title: str) -> None:
    """Press the zoom button of one exact window, which also fronts its Space."""
    windows = cua("list_windows", {"pid": pid})["windows"]
    match = [w for w in windows if w.get("title") == title and w.get("is_on_screen")]
    if len(match) != 1:
        raise SystemExit(
            f"e2e space-groups: FAIL: expected one on-screen window titled {title!r}, "
            f"got {[(w['window_id'], w.get('title')) for w in windows]}"
        )
    window_id = match[0]["window_id"]
    snapshot = cua(
        "get_window_state",
        {"pid": pid, "window_id": window_id, "include_screenshot": False, "max_depth": 2},
    )
    buttons = [
        element
        for element in snapshot.get("elements", [])
        if "AXZoomWindow" in (element.get("actions") or [])
    ]
    if not buttons:
        raise SystemExit(
            f"e2e space-groups: FAIL: no zoom button in window {window_id} of pid {pid}"
        )
    cua(
        "click",
        {
            "element_token": buttons[0]["element_token"],
            "action": "press",
            "pid": pid,
            "window_id": window_id,
        },
    )
    return window_id


def card_title(cards: list[dict], title: str) -> dict | None:
    return next((card for card in cards if card.get("title") == title), None)


def switch_space(keys: list[str], predicate_context, timeout: float) -> dict:
    """Press a Space-switch shortcut until the app itself reports the target context.

    A single press is not dependable: the Space transition's settle window can swallow the first
    one, and the app only writes a snapshot on its own events. Re-pressing until the app agrees is
    what makes this deterministic instead of a timing guess. Pressing a switch that the desktop
    cannot satisfy (already at the leftmost Space) is a no-op, so repeat presses are safe.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        hotkey(keys)
        poll_until = time.time() + 6.0
        while time.time() < poll_until:
            snapshot = state()
            if (
                snapshot
                and snapshot.get("pid") == app_pid
                and predicate_context(snapshot.get("space_contexts", []))
            ):
                return snapshot
            time.sleep(0.1)
    raise SystemExit(
        f"e2e space-groups: FAIL: {'+'.join(keys)} never reached the expected Space context; "
        f"last={state()}"
    )


# --- 等首个快照:Reconcile 到 app 的初始状态 ---------------------------------
base = wait_state(lambda s: s.get("seq", 0) > 0 and s.get("pid"), 10, "the first state snapshot")
# Frames from the process this run started, not a snapshot the previous process left on disk.
check(True, "app wrote a state snapshot", f"pid={app_pid} seq={base.get('seq')} event={base.get('event')}")

# 起始必须在一个普通桌面上:否则"来源桌面"无从谈起,而且会污染后面的上下文断言。
# The probe must start on an ordinary desktop or the origin desktop is undefined.
check(
    base["space_groups"]["unknown_active_fullscreen_spaces"] == 0,
    "started on an ordinary desktop (no active unknown-origin fullscreen Space)",
    f"space_groups={base['space_groups']}",
)
if problems:
    print("\n".join(problems))
    raise SystemExit(1)

# 找到 TextEdit 的两个探针窗口。
apps = cua("list_apps", {"include_installed": False})["apps"]
# Match by bundle id: the display name is localized ("文本编辑" here), the bundle id is not.
textedit = next((a for a in apps if a.get("bundle_id") == "com.apple.TextEdit"), None)
if not textedit or not textedit.get("pid"):
    raise SystemExit("e2e space-groups: FAIL: TextEdit is not running")
pid = textedit["pid"]
# 基线取在进全屏之前:全屏后的 refresh 帧可能早于 transition-settled 帧,用后者的 seq 当
# 基线会把真正要断言的那一帧错过。
# Take the baseline before entering fullscreen: the post-fullscreen refresh frame can precede the
# transition-settled frame, and using the latter's seq would skip the frame under test.
pre_fullscreen_seq = base.get("seq", 0)
window_id = fullscreen_title(pid, title_fullscreen)

def active_fullscreen(contexts: list[dict]) -> dict | None:
    return next(
        (c for c in contexts if c.get("kind") == "fullscreen" and c.get("origin") is not None),
        None,
    )


def active_ordinary(contexts: list[dict]) -> dict | None:
    return next((c for c in contexts if c.get("kind") == "ordinary"), None)


def summoned_cards(predicate_context, seq_before: int, timeout: float, label: str) -> dict:
    """Wait for the applied candidate set the app published for the given context.

    Only an applied frame may be asserted on: `refresh` (the accepted set changed) or
    `refresh_context` (the active Space changed while the set did not, which is what a correct
    grouping switch looks like). Both are written after the set is applied. Other frames
    (`space_transition_settled`, `keystroke_display`) pair the tracker's newer Space context with
    the *previous* card list, so asserting on them compares a context with a set computed for the
    old one. The scenario therefore needs no summon: a summon would steer focus and could itself
    move the Space context under test.
    """
    return wait_state(
        lambda s: s.get("pid") == app_pid
        and s.get("event") in ("refresh", "refresh_context")
        and s.get("seq", 0) > seq_before
        and s.get("cards_count", 0) > 0
        and predicate_context(s.get("space_contexts", [])),
        timeout,
        label,
    )


# 等来源被确认,且当前上下文已经真的落在那个全屏 Space 上。
# Wait for the origin to be confirmed AND the active context to actually be that fullscreen Space.
after_fullscreen = wait_state(
    lambda s: s.get("pid") == app_pid
    and active_fullscreen(s.get("space_contexts", [])) is not None,
    20,
    "the active context to be the origin-linked fullscreen Space",
)
context = active_fullscreen(after_fullscreen["space_contexts"])
check(
    True,
    "the fullscreen Space is active and resolves to its origin desktop",
    f"context={context} space_groups={after_fullscreen['space_groups']}",
)

# 等 app 发布这个上下文下的候选集合并断言。
# Wait for the candidate set the app published for this context and assert it.
seq_before = pre_fullscreen_seq
frame = summoned_cards(
    lambda contexts: active_fullscreen(contexts) is not None,
    seq_before,
    20,
    "a fullscreen-context frame with candidates",
)
cards = frame.get("cards", [])
fullscreen_card = card_title(cards, title_fullscreen)
plain_card = card_title(cards, title_plain)
check(
    fullscreen_card is not None and fullscreen_card.get("fullscreen"),
    "the fullscreen window is a candidate in its own Space",
    f"cards={[(c.get('title'), c.get('fullscreen')) for c in cards]}",
)
check(
    plain_card is not None and not plain_card.get("fullscreen"),
    "the origin desktop's ordinary window shares the fullscreen Space's group",
    f"plain={plain_card}",
)
check(
    (window_id is not None),
    "the probe drove a concrete window id",
    f"window_id={window_id}",
)

# 切回普通桌面:来源明确的全屏窗口必须仍在候选里。
# Back on the ordinary desktop the origin's fullscreen window must stay a candidate.
time.sleep(1.5)
seq_before = frame.get("seq", 0)
on_desktop_context = switch_space(
    ["ctrl", "left"],
    lambda contexts: active_ordinary(contexts) is not None,
    45.0,
)
on_desktop = summoned_cards(
    lambda contexts: active_ordinary(contexts) is not None,
    seq_before,
    20,
    "an ordinary-desktop frame with candidates",
)
desktop_cards = on_desktop.get("cards", [])
check(
    card_title(desktop_cards, title_fullscreen) is not None,
    "the desktop lists the fullscreen window whose origin is this desktop",
    f"space_groups={on_desktop.get('space_groups')} "
    f"contexts={on_desktop.get('space_contexts')} "
    f"titles={[c.get('title') for c in desktop_cards]}",
)
check(
    card_title(desktop_cards, title_plain) is not None,
    "the desktop still lists its own ordinary window",
    f"titles={[c.get('title') for c in desktop_cards]}",
)

for label in checks:
    print(f"  ok   {label}")
for label in problems:
    print(f"  FAIL {label}")
total = len(checks) + len(problems)
print(f"e2e space-groups: {'PASS' if not problems else 'FAIL'} ({len(checks)}/{total} checks)")
raise SystemExit(1 if problems else 0)
PY
status=$?
if [ "$status" -eq 0 ]; then
    echo "e2e: snapshot kept at $state_file (dev app left running; stop with: pkill -f Oh-My-Tab-Dev)"
fi
exit "$status"
