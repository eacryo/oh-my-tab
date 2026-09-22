#!/bin/bash
# A2 层 E2E 场景:系统把滚动条切成占宽的 legacy 之后,设置页**不变形、不被裁**。
#
# 回归来源(2026-09-22 已修,方案见 src/scroller.rs):系统切成 legacy 时 clip 少 17pt;以前 document
# 是 NSViewWidthSizable,文档跟着缩,里面宽度可伸缩的自绘控件被挤压 —— 开关宽 38 → 21("开关变形"),
# 行内按钮右缘整体左移 17pt。
#
# 修复 = ① document 不再随宽度自适应(见 widgets.rs)+ ② 每次窗口变 key / app 激活都实测滚动条占位,
# 变了就按**实际可视宽度**重排内容(见 scroller.rs 的 resync)。
#
# 明确不做什么:不重申 `setScrollerStyle(.overlay)`。实测(2026-09-22)它运行中只瞬时有效(占位
# 17 → 0 只用 8ms),AppKit 会在下一个 display/scroll pass 按系统偏好改回 legacy,所以"强制 overlay"
# 做不到;本场景因此允许 legacy 样式与 17pt 占位,只断言不变形、不被裁。
#
# 本场景会**临时修改全局偏好** `AppleShowScrollBars`(模拟"插上鼠标/改成始终显示滚动条"),所以
# 默认被 run-all.sh 跳过,必须显式 --include-prefs;退出时(含失败/中断)一定还原。
# E2E_CHANGES_PREFS=1
#
# Exit code: 0 = 通过;非 0 = 失败(逐条打印原因)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
state_file="/tmp/omt-e2e-settings.json"
dev_app="$repo_dir/dist/Oh-My-Tab-Dev.app"

case "${1:-}" in
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    "") ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
esac

fail() { echo "e2e scrollbar-style-flip: FAIL: $*" >&2; exit 1; }

had_pref=0
original_pref=""
if original_pref="$(defaults read -g AppleShowScrollBars 2>/dev/null)"; then
    had_pref=1
fi

restore_pref() {
    if [ "$had_pref" = "1" ]; then
        defaults write -g AppleShowScrollBars "$original_pref"
    else
        defaults delete -g AppleShowScrollBars 2>/dev/null || true
    fi
}
# 无论成功失败或中断,都必须把调用者的偏好还原。
# Restore the caller's preference on any exit path, including failure and interruption.
trap restore_pref EXIT INT TERM

command -v cua-driver >/dev/null 2>&1 || fail "cua-driver CLI not found in PATH"
cua-driver status >/dev/null 2>&1 || fail "cua-driver daemon is not running"

rm -f "$state_file" "${state_file%.json}.tmp"
restart_out="$("$repo_dir/scripts/dev-restart.sh" --open-settings=about \
    --e2e-state="$state_file" --no-onboarding 2>&1)"
echo "$restart_out" | grep -q "^restart ok" || {
    echo "$restart_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up"
}
echo "$restart_out" | grep -E "^(restart ok|build-version)" || true

# 热键用于逼 app 写一条新的几何快照(浮窗是 nonactivating 面板,按热键不会激活本 app)。
# The hotkey forces a fresh geometry snapshot; the overlay is a nonactivating panel, so pressing the
# hotkey does not activate this app.
force_frame() {
    cua-driver call hotkey '{"keys":["cmd","tab"],"scope":"desktop","delivery_mode":"foreground"}' \
        >/dev/null 2>&1 || true
}

python3 - "$state_file" "$dev_app" <<'PY'
import json, subprocess, sys, time

state_file, dev_app = sys.argv[1], sys.argv[2]
problems: list[str] = []
checks: list[str] = []


def check(ok: bool, label: str, detail: str = "") -> None:
    (checks if ok else problems).append(f"{label}{(': ' + detail) if detail else ''}")


def sh(command: str) -> None:
    subprocess.run(command, shell=True, capture_output=True, text=True)


def frame(deadline: float = 6.0) -> dict:
    """等到出现比当前更新的帧。Waits for a frame newer than the last one seen."""
    global last_seq
    end = time.time() + deadline
    while time.time() < end:
        try:
            with open(state_file) as handle:
                data = json.load(handle)
            if data.get("seq") != last_seq and data.get("pages"):
                last_seq = data.get("seq")
                return data
        except (FileNotFoundError, json.JSONDecodeError):
            pass
        time.sleep(0.05)
    return {}


def observe(label: str) -> dict:
    """按一次热键拿一帧,并返回本页滚动几何与可伸缩控件的实测值。"""
    sh("cua-driver call hotkey '{\"keys\":[\"cmd\",\"tab\"],\"scope\":\"desktop\",\"delivery_mode\":\"foreground\"}'")
    data = frame()
    if not data:
        raise SystemExit(f"e2e scrollbar-style-flip: FAIL: no geometry frame after {label}")
    page = next(entry for entry in data["pages"] if entry["root"] == "page_6_about")
    switches = [
        (round(n["frame"][2], 1), round(n["frame"][3], 1))
        for n in data["views"]
        if n["root"] == "page_6_about" and n["class"] == "OhMyTabHtmlSwitch"
    ]
    return {
        "label": label,
        "style": page["styles"][0],
        "footprint": round(page["self"][2] - page["clip"][2], 1),
        "switches": switches,
    }


last_seq = 0


def content_right_edge(data: dict) -> float:
    """页面里最靠右的内容(行内按钮 / 自绘开关 / 卡片分隔线)的右缘,用于判断有没有被裁。"""
    views = data["views"]
    edges = []
    for node in views:
        if node["root"] != "page_6_about":
            continue
        if node["class"] == "NSButtonTextField":
            parent = views[node["parent"]]
            if parent["class"] == "OhMyTabHtmlActionButton" and parent["frame"][2] <= 140:
                edges.append(parent["frame"][0] + parent["frame"][2])
        if node["class"] == "OhMyTabHtmlSwitch":
            edges.append(node["frame"][0] + node["frame"][2])
        if node["frame"][3] <= 1.5 and node["frame"][2] > 300:
            edges.append(node["frame"][0] + node["frame"][2])
    return round(max(edges), 1) if edges else 0.0


def observe(label: str) -> dict:
    """按一次热键拿一帧,返回本页滚动几何与可伸缩控件的实测值。"""
    sh("cua-driver call hotkey '{\"keys\":[\"cmd\",\"tab\"],\"scope\":\"desktop\",\"delivery_mode\":\"foreground\"}'")
    data = frame()
    if not data:
        raise SystemExit(f"e2e scrollbar-style-flip: FAIL: no geometry frame after {label}")
    page = next(entry for entry in data["pages"] if entry["root"] == "page_6_about")
    return {
        "label": label,
        "style": page["styles"][0],
        "clip_w": page["clip"][2],
        "footprint": round(page["self"][2] - page["clip"][2], 1),
        "switches": [
            (round(n["frame"][2], 1), round(n["frame"][3], 1))
            for n in data["views"]
            if n["root"] == "page_6_about" and n["class"] == "OhMyTabHtmlSwitch"
        ],
        "content_right": content_right_edge(data),
    }


baseline = observe("baseline")
print(f"  info baseline: {baseline}")
check(
    baseline["style"] == 1 and baseline["footprint"] == 0.0,
    "the app starts with overlay scrollers (no layout width taken)",
    f"{baseline}",
)
check(
    all(width >= 1.5 * height for width, height in baseline["switches"]),
    "baseline self-drawn switches have a normal capsule aspect",
    f"switch w/h={baseline['switches']}",
)

# 把系统切成 legacy(等价于"鼠标成为最后输入设备"),再走一遍真实路径:改完偏好回到设置窗口。
# Switch the system to legacy (equivalent to a mouse becoming the last input device), then take the
# real path: change the preference and return to the settings window.
sh("defaults write -g AppleShowScrollBars Always")
time.sleep(1.2)

# 先量"翻转后、尚未激活"这一刻 —— 这正是用户看到"滚动条变粗、开关被挤变形"的状态。
# 断言它已经不再变形:document 不再随宽度自适应(①),所以 AppKit 的 legacy tile 牵不动内容。
# Measure the moment after the flip but before activation -- the state in which the user sees "a thick
# scrollbar and a deformed switch". It must already be deformation-free: the document is no longer
# width-sizable (fix 1), so AppKit's legacy tiling cannot drag the content along.
flipped = observe("after flip, before activation")
print(f"  info after flip: {flipped}")
check(
    all(width >= 1.5 * height for width, height in flipped["switches"]),
    "no self-drawn switch is squeezed while the system is in legacy mode",
    f"switch w/h={flipped['switches']} (baseline {baseline['switches']}) style={flipped['style']}",
)
check(
    flipped["switches"] == baseline["switches"],
    "switch geometry is unchanged by the legacy tiling",
    f"flipped={flipped['switches']} baseline={baseline['switches']}",
)
check(
    flipped["content_right"] <= flipped["clip_w"],
    "nothing is clipped while the system is in legacy mode",
    f"content_right={flipped['content_right']} clip_w={flipped['clip_w']}",
)

sh(f"open -a {dev_app}")
time.sleep(2.0)
after = observe("after activation")
print(f"  info after activation: {after}")

check(
    after["footprint"] <= 20.0,
    "the scroller footprint never exceeds a normal legacy width",
    f"footprint={after['footprint']} style={after['style']}",
)
check(
    all(width >= 1.5 * height for width, height in after["switches"]),
    "no self-drawn switch is squeezed after the style flip",
    f"switch w/h={after['switches']} (baseline {baseline['switches']})",
)
check(
    after["switches"] == baseline["switches"],
    "switch geometry matches the baseline after the flip",
    f"after={after['switches']} baseline={baseline['switches']}",
)
check(
    after["content_right"] <= after["clip_w"],
    "the right-most content stays inside the visible width (nothing clipped)",
    f"content_right={after['content_right']} clip_w={after['clip_w']}",
)
# 机制断言:同步逻辑必须真的跑过(激活通知到达并执行),否则上面几条可能只是"没变化"。
# Mechanism check: the resync must actually have run, otherwise the checks above could pass simply
# because nothing happened.
log_path = subprocess.run("echo ~/Library/Logs/oh-my-tab/oh-my-tab.log", shell=True,
                          capture_output=True, text=True).stdout.strip()
log_tail = open(log_path, errors="ignore").read().splitlines()[-400:]
resync_ran = any("activation resync" in line for line in log_tail)
check(
    resync_ran,
    "the activation resync ran (delivered notification + handler)",
    "" if resync_ran else "no [scroller] activation resync line in the log tail",
)

for label in checks:
    print(f"  ok   {label}")
for label in problems:
    print(f"  FAIL {label}")
total = len(checks) + len(problems)
print(f"e2e scrollbar-style-flip: {'PASS' if not problems else 'FAIL'} ({len(checks)}/{total} checks)")
raise SystemExit(1 if problems else 0)
PY
