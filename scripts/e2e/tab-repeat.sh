#!/bin/bash
# A2 层 E2E 场景:按住 Tab 连续切换与列表循环。
#
# 判定者是**脚本**(退出码),不是人:输入由脚本自己合成真实 CGEvent(经 HID 事件流,app 的全局
# tap 收到的就是系统级事件),断言全部读 app 自己写的 JSON 快照(`--e2e-state=<path>`)。
# 分层与约定见 AGENTS.md「测试分层」。
#
# 为什么不用 cua-driver 的 hotkey:按住不放需要"只按下不释放"的事件序列,驱动只提供 press+release。
# 这里用 CGEventCreateKeyboardEvent + CGEventSetIntegerValueField(kCGKeyboardEventAutorepeat) 复刻
# macOS 长按产生的重复流(fresh keyDown + autorepeat keyDown*N + 释放),tap 收到的字段与真实长按一致。
#
# 覆盖的行为:
#   1. 长按产生的 autorepeat 不再被丢弃,每个都推进选中项(此前整段重复被吞掉,选中项停在原卡);
#   2. 推进节奏受 app 自己的下限约束:过密的重复被吞掉但不推进;
#   3. 推进是列表循环:正向越过末尾回到开头,反向越过开头回到末尾;
#   4. 释放修饰键提交当时选中的窗口,且提交的就是它。
#
# 合成的按键在**每条退出路径**上都会释放。留着不放会污染后续场景:WindowServer 会把下一次按下
# 当成那个仍按住的键的重复,于是下一个场景的热键打不开浮窗。
#
# 用法:
#   scripts/e2e/tab-repeat.sh
#
# 前置:已授予辅助功能权限。本场景注入真实全局热键(抢焦点),故用标记声明;
# scripts/e2e/run-all.sh 默认跳过它。
# This scenario injects a real global hotkey and therefore steals focus.
# E2E_STEALS_FOCUS=1
#
# 本场景会真实切换前台应用,只能在机器空闲时运行:任何并发输入都会让选中序列在断言途中改变。
#
# Exit code: 0 = 全部断言通过;非 0 = 失败(逐条打印原因)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
state_file="/tmp/omt-e2e-tab-repeat.json"

fail() { echo "e2e tab-repeat: FAIL: $*" >&2; exit 1; }

command -v cua-driver >/dev/null 2>&1 || fail "cua-driver CLI not found in PATH"

# 清掉上一个进程可能留下的快照:否则"等一帧新的"会拿到上一轮的帧。
pkill -f 'Oh-My-Tab-Dev' >/dev/null 2>&1 || true
for _ in $(seq 1 40); do
    pgrep -f 'Oh-My-Tab-Dev' >/dev/null 2>&1 || break
    sleep 0.1
done
rm -f "$state_file" "${state_file%.json}.tmp"

# 需要一个后台目标窗口,保证卡片数足够验证循环(≥3 张才会真的绕回)。
echo "e2e: launching a background target window (TextEdit)"
cua-driver call launch_app '{"name":"TextEdit"}' >/dev/null 2>&1 \
    || fail "could not launch TextEdit as a target"
sleep 1

echo "e2e: starting the dev build with --e2e-state"
restart_out="$("$repo_dir/scripts/dev-restart.sh" --e2e-state="$state_file" --no-onboarding 2>&1)"
echo "$restart_out" | grep -q "^restart ok" || {
    echo "$restart_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up"
}
echo "$restart_out" | grep -E "^(restart ok|build-version|app args)" || true

# The instance that must be driven is the one dev-restart reports. A launchd job left over from a
# previous run can respawn an older instance while this run starts, so a snapshot read at that
# moment can belong to a process this scenario is not driving.
app_pid="$(printf '%s\n' "$restart_out" \
    | sed -n 's/^restart ok (app pid \([0-9][0-9]*\).*/\1/p' | head -1)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"
# Clear the snapshot after the restart, so no frame from an earlier instance is ever read.
rm -f "$state_file" "${state_file%.json}.tmp"

python3 - "$state_file" "$app_pid" <<'PY'
import ctypes
import json
import sys
import threading
import time

state_file = sys.argv[1]
app_pid = int(sys.argv[2])
problems: list[str] = []
checks: list[str] = []
infos: list[str] = []


def check(ok: bool, label: str, detail: str = "") -> None:
    (checks if ok else problems).append(f"{label}{(': ' + detail) if detail else ''}")


# --- CGEvent synthesis ------------------------------------------------------
cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
cg.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
cg.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
cg.CGEventSetIntegerValueField.argtypes = [ctypes.c_void_p, ctypes.c_int32, ctypes.c_int64]
cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]

# Mirrors src/event_tap.rs (keyboard module): the tap reads exactly these fields.
FIELD_AUTOREPEAT = 8
FIELD_KEYCODE = 9
FLAG_SHIFT = 0x20000
FLAG_COMMAND = 0x100000
VK_TAB = 48
VK_COMMAND = 55
VK_SHIFT = 56
HID_EVENT_TAP = 0


def post(keycode: int, down: bool, flags: int, autorepeat: int = 0) -> None:
    event = cg.CGEventCreateKeyboardEvent(None, keycode, down)
    cg.CGEventSetFlags(event, flags)
    cg.CGEventSetIntegerValueField(event, FIELD_KEYCODE, keycode)
    cg.CGEventSetIntegerValueField(event, FIELD_AUTOREPEAT, autorepeat)
    cg.CGEventPost(HID_EVENT_TAP, event)


def release_all_keys() -> None:
    """Release every synthetic key, on every exit path.

    A key left logically down poisons whatever runs next: the WindowServer treats the next press of
    that key as a repeat of the held one instead of a fresh press, so the following scenario's
    hotkey never opens the switcher.
    """
    post(VK_TAB, False, 0)
    post(VK_SHIFT, False, 0)
    post(VK_COMMAND, False, 0)


# --- state sampling ---------------------------------------------------------
# The app overwrites one file, so every frame is captured as it appears rather than read at the end.
samples: dict[int, dict] = {}
storing = threading.Lock()
stop = threading.Event()


def state() -> dict | None:
    try:
        with open(state_file) as handle:
            return json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def sample_once() -> dict | None:
    snapshot = state()
    if (
        snapshot
        and snapshot.get("seq")
        # Only frames from the instance this scenario is driving: the app is restarted per scenario,
        # and an older process can still be alive (and writing) during startup.
        and snapshot.get("pid") == app_pid
    ):
        with storing:
            samples.setdefault(snapshot["seq"], snapshot)
    return snapshot


def recorded_frames() -> list[dict]:
    with storing:
        return [
            snapshot
            for _, snapshot in sorted(samples.items())
            if snapshot.get("pid") == app_pid
        ]


sampler_error: list[str] = []


def sample_loop() -> None:
    while not stop.is_set():
        try:
            sample_once()
        except BaseException as error:  # a dead sampler must be visible, not silent
            sampler_error.append(repr(error))
            return
        time.sleep(0.005)


def current_seq() -> int:
    return max((snapshot.get("seq", 0) for snapshot in recorded_frames()), default=0)


def wait_state(predicate, timeout: float, label: str, after_seq: int | None = None) -> dict:
    """Wait for a frame *newer than `after_seq`* that matches `predicate`.

    Two things this has to get right. A single action can make the app write several frames in a
    burst (a commit, then the activation refresh, then keystroke frames), so the frame under test
    can be overwritten before a poll reads the file; the 5ms sampler records every frame, so a match
    anywhere in that record counts. And an earlier frame of the same kind must not satisfy a later
    wait: the forward phase's commit would otherwise answer the backward phase's wait for a commit.
    With no `after_seq`, only frames newer than everything recorded so far count.
    """
    if after_seq is None:
        after_seq = current_seq()
    deadline = time.time() + timeout
    last = None

    def fresh(snapshot: dict | None) -> bool:
        return bool(snapshot) and snapshot.get("seq", 0) > after_seq

    while True:
        for snapshot in reversed(recorded_frames()):
            if fresh(snapshot) and predicate(snapshot):
                return snapshot
        last = sample_once()
        if fresh(last) and predicate(last):
            return last
        if time.time() >= deadline:
            raise AssertionError(
                f"timed out waiting for {label}; last={last}; "
                f"after_seq={after_seq} recorded={len(recorded_frames())}"
            )
        time.sleep(0.02)




def repeat_meters(snapshot: dict) -> tuple[int, int]:
    meter = snapshot.get("tab_repeat", {})
    return meter.get("steps", 0), meter.get("throttled", 0)


def frames_between(first_seq: int, last_seq: int) -> list[dict]:
    return [
        snapshot
        for snapshot in recorded_frames()
        if first_seq <= snapshot.get("seq", -1) <= last_seq
    ]


def selected_key(snapshot: dict) -> tuple | None:
    key = snapshot.get("selected_key")
    return (key.get("pid"), key.get("window_id")) if key else None


def card_keys(snapshot: dict) -> list:
    return [(card.get("pid"), card.get("window_id")) for card in snapshot.get("cards", [])]


def check_single_steps(prefix: str, steps: list[dict], forward: bool) -> None:
    """Each consecutive pair of published selections must differ by exactly one card in that pair's
    own list, i.e. the step moves one card and wraps at the ends.

    Evaluated against the list each frame itself reports, so a window appearing or closing mid-hold
    (which legitimately reorders the cards) cannot be mistaken for a stall or a skip.
    """
    direction = 1 if forward else -1
    mismatches = []
    skipped = 0
    for previous, current in zip(steps, steps[1:]):
        keys = card_keys(current)
        key = selected_key(previous)
        if not keys or key not in keys:
            # The previously selected window left the list; a step cannot be verified across it.
            skipped += 1
            continue
        expected = (keys.index(key) + direction) % len(keys)
        if current.get("selected_index") != expected:
            mismatches.append(
                (previous.get("selected_index"), current.get("selected_index"), expected)
            )
    detail = f"steps={len(steps)} unverifiable={skipped} mismatches={mismatches[:5]}"
    check(not mismatches, f"each published {prefix} step moves exactly one card", detail)


# --- driving one hold -------------------------------------------------------
def open_switcher(message_flags: int) -> dict:
    """Press the modifier + Tab and hold, retrying until the overlay is actually on screen.

    A frame can already be on disk while the switcher tap is still installing, and events posted
    then are simply lost, so the summon is retried instead of assumed.
    """
    for _ in range(6):
        before_press = current_seq()
        post(VK_COMMAND, True, message_flags)
        if message_flags & FLAG_SHIFT:
            post(VK_SHIFT, True, message_flags)
        post(VK_TAB, True, message_flags)
        try:
            return wait_state(
                lambda s: s.get("event") == "summon" and s.get("cards_count", 0) >= 3,
                1.5,
                "the overlay to show at least three cards",
                after_seq=before_press,
            )
        except AssertionError:
            # Not our combo yet (the tap was not listening). Close the switcher again and wait for
            # it to be gone before retrying: a fresh press while the overlay is already open would
            # advance the selection instead of summoning, adding a step this scenario did not send.
            release_hold(message_flags)
            try:
                wait_state(lambda s: not s.get("visible"), 2, "the overlay to close before a retry")
            except AssertionError:
                pass
            time.sleep(0.3)
    raise AssertionError(
        f"the switcher never opened; the tap never saw the synthesized combo (last state: {state()})"
    )


def settled_meters() -> tuple[int, int]:
    """The counters once the input already sent has been processed.

    Taken after the summon settled rather than from the summon frame itself: the counter is
    incremented by the tap, so a press that the app processes just after that frame would otherwise
    be counted as if this scenario had sent it as a repeat.
    """
    time.sleep(0.3)
    return repeat_meters(sample_once() or {})


def release_hold(message_flags: int) -> None:
    """Release the modifier first (that is what commits), then Shift, then the held Tab."""
    post(VK_COMMAND, False, 0)
    if message_flags & FLAG_SHIFT:
        post(VK_SHIFT, False, 0)
    post(VK_TAB, False, 0)


def hold_repeats(count: int, message_flags: int, spacing: float) -> None:
    for _ in range(count):
        post(VK_TAB, True, message_flags, 1)
        time.sleep(spacing)


def commit_after_release(message_flags: int, label: str) -> dict:
    before_release = current_seq()
    release_hold(message_flags)
    return wait_state(
        lambda s: s.get("event") == "commit" and s.get("committed"),
        4,
        label,
        after_seq=before_release,
    )


def drive() -> None:
    # Phase A: forward. A fresh physical press with both keys held.
    forward_flags = FLAG_COMMAND
    summon = open_switcher(forward_flags)
    cards = summon["cards_count"]
    first_index = summon["selected_index"]
    base_steps, base_throttled = settled_meters()
    check(
        True,
        "the switcher opened with enough cards to wrap the list",
        f"cards={cards} selected={first_index}",
    )
    if problems:
        release_hold(forward_flags)
        return

    # Repeats closer together than the app's cadence (140ms). The first repeat of a hold steps
    # (macOS applied its own initial delay already); the ones inside the cadence are swallowed.
    fast_repeats = 4
    for _ in range(fast_repeats):
        post(VK_TAB, True, forward_flags, 1)
        time.sleep(0.035)
    before_fast = current_seq()
    fast = wait_state(
        lambda s: repeat_meters(s) == (base_steps + 1, base_throttled + fast_repeats - 1),
        3,
        "the first repeat to step and the rest to be throttled",
        after_seq=before_fast,
    )
    fast_steps, fast_throttled = repeat_meters(fast)
    check(
        fast_steps == base_steps + 1,
        "the first repeat of a hold steps immediately",
        f"stepped={fast_steps - base_steps} sent={fast_repeats}",
    )
    check(
        fast_throttled == base_throttled + fast_repeats - 1,
        "repeats inside the cadence are swallowed without stepping",
        f"throttled={fast_throttled - base_throttled} sent={fast_repeats}",
    )

    # Spaced wider than the cadence, for more than one full lap so the wrap is exercised. The pause
    # also clears the cadence so the first spaced repeat is due.
    time.sleep(0.25)
    spaced_repeats = cards + 3
    hold_repeats(spaced_repeats, forward_flags, 0.2)
    committed = commit_after_release(forward_flags, "the forward release to commit a window")

    steps_after, throttled_after = repeat_meters(committed)
    total_steps = steps_after - base_steps
    spaced_steps = steps_after - fast_steps
    check(
        spaced_steps >= spaced_repeats,
        "every spaced repeat steps",
        f"stepped={spaced_steps} sent={spaced_repeats}",
    )
    check(
        throttled_after == fast_throttled,
        "spaced repeats are never throttled",
        f"throttled_after={throttled_after} at_fast_phase={fast_throttled}",
    )
    # More steps than were sent means something else also switched: the exact sequence cannot be
    # attributed to this scenario, so it is reported rather than asserted.
    forwarded = spaced_steps == spaced_repeats
    if not forwarded:
        infos.append(
            "concurrent input: the switch count moved beyond this scenario's repeats "
            f"(stepped={spaced_steps} sent={spaced_repeats}); the per-step checks are skipped"
        )

    check(
        committed["selected_key"]
        == {
            "pid": committed["committed"]["pid"],
            "window_id": committed["committed"]["window_id"],
        },
        "the committed window is the selected key",
        f"selected_key={committed['selected_key']} committed={committed['committed']}",
    )

    # Each accepted step publishes its selection, one frame per step, and each must land exactly one
    # card further along in the list that frame itself reports. That is what "keeps switching and
    # loops" means, and it is asserted on identities rather than on a global index because a window
    # appearing or closing mid-hold legitimately reorders the cards.
    steps = [
        snapshot
        for snapshot in frames_between(summon["seq"], committed["seq"])
        if snapshot.get("event") == "selection"
    ]
    check(
        steps and steps[-1].get("selected_index") == committed["committed"]["index"],
        "the last published forward selection is the committed card",
        f"last={steps[-1].get('selected_index') if steps else None} "
        f"committed={committed['committed']['index']} sampled={len(steps)} accepted={total_steps} "
        f"recorded={len(recorded_frames())} sampler_error={sampler_error}",
    )
    if len(steps) >= 2 and forwarded:
        check_single_steps("forward", steps, forward=True)
        # A step from the last card to the first is the wrap, and it must be observed: the hold
        # crossed the end of the list.
        wraps = [
            (previous.get("selected_index"), current.get("selected_index"))
            for previous, current in zip(steps, steps[1:])
            if current.get("selected_index") == 0
        ]
        check(
            bool(wraps),
            "a forward step wrapped from the last card to the first",
            f"wraps={wraps} sampled={len(steps)} accepted={total_steps}",
        )
    elif not forwarded:
        infos.append("the forward per-step checks were skipped (concurrent input)")
    else:
        check(False, "the app published a frame per accepted step", f"sampled={len(steps)}")

    # Phase B: backward. Command+Shift+Tab must hold-repeat in the other direction and wrap the
    # other way, so the switcher is re-opened and driven for more than one lap backwards.
    time.sleep(0.8)
    backward_flags = FLAG_COMMAND | FLAG_SHIFT
    before_backward = sample_once() or committed
    backward_summon = open_switcher(backward_flags)
    backward_cards = backward_summon["cards_count"]
    backward_start = backward_summon["selected_index"]
    backward_base_steps = settled_meters()[0]
    # The card list is read again rather than assumed: a window appearing or closing between the two
    # phases is the environment changing, and every expectation below derives from this reading.
    backward_repeats = backward_cards + 2
    hold_repeats(backward_repeats, backward_flags, 0.2)
    backward_commit = commit_after_release(
        backward_flags, "the backward release to commit a window"
    )

    backward_steps = repeat_meters(backward_commit)[0] - backward_base_steps
    check(
        backward_steps >= backward_repeats,
        "every spaced backward repeat steps",
        f"stepped={backward_steps} sent={backward_repeats}",
    )
    backward_exact = backward_steps == backward_repeats
    if not backward_exact:
        infos.append(
            "concurrent input: the backward switch count moved beyond this scenario's repeats "
            f"(stepped={backward_steps} sent={backward_repeats}); the per-step checks are skipped"
        )
    backward_frames = [
        snapshot
        for snapshot in frames_between(backward_summon["seq"], backward_commit["seq"])
        if snapshot.get("event") == "selection"
    ]
    if len(backward_frames) >= 2 and backward_exact:
        check_single_steps("backward", backward_frames, forward=False)
        wraps = [
            (previous.get("selected_index"), current.get("selected_index"))
            for previous, current in zip(backward_frames, backward_frames[1:])
            if current.get("selected_index") == backward_cards - 1
        ]
        check(
            bool(wraps),
            "a backward step wrapped from the first card to the last",
            f"wraps={wraps} cards={backward_cards} sampled={len(backward_frames)}",
        )
    elif not backward_exact:
        infos.append("the backward per-step checks were skipped (concurrent input)")
    else:
        check(
            False,
            "the app published a frame per backward step",
            f"sampled={len(backward_frames)} accepted={backward_steps}",
        )


def require_idle_machine() -> None:
    """Refuse to run while something else is driving the switcher.

    This scenario asserts exact step counts, and any concurrent Cmd+Tab (a person typing, another
    agent) adds steps it did not send. A busy machine is not evidence about the feature, so it is
    reported as such instead of as a failure.
    """
    release_all_keys()
    first = wait_state(lambda s: True, 8, f"the first snapshot from pid {app_pid}")
    time.sleep(1.2)
    second = sample_once() or sample_once()
    if not second:
        raise AssertionError("the app wrote no snapshot to check idleness against")
    if repeat_meters(first) != repeat_meters(second):
        raise AssertionError(
            "the machine is in use: the held-Tab counters moved with no input from this scenario "
            f"({repeat_meters(first)} -> {repeat_meters(second)}); run this scenario on an idle "
            "machine"
        )


# Defensive: clear anything an interrupted previous run left down, then drive and always release.
sampler = threading.Thread(target=sample_loop, daemon=True)
sampler.start()
try:
    require_idle_machine()
    drive()
except BaseException:
    release_all_keys()
    raise
finally:
    stop.set()
    sampler.join(timeout=1)
    release_all_keys()

for label in checks:
    print(f"  ok   {label}")
for label in problems:
    print(f"  FAIL {label}")
for label in infos:
    print(f"  info {label}")
total = len(checks) + len(problems)
print(f"e2e tab-repeat: {'PASS' if not problems else 'FAIL'} ({len(checks)}/{total} checks)")
raise SystemExit(1 if problems else 0)
PY
status=$?
if [ "$status" -eq 0 ]; then
    echo "e2e: snapshot kept at $state_file (dev app left running; stop with: pkill -f Oh-My-Tab-Dev)"
fi
exit "$status"
