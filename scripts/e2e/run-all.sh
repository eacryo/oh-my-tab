#!/bin/bash
# A2 层 E2E 聚合入口:按顺序跑 scripts/e2e/ 下的所有场景,汇总退出码。
#
# 为什么需要它:AGENTS.md 把 A2 列为交接/发版前的 gate,但如果只能逐个手敲脚本,这条 gate 迟早
# 会被漏掉一项。新增场景只要往 scripts/e2e/ 放一个可执行脚本即可,本入口自动发现。
#
# 抢焦点的场景默认**跳过**:它们会注入真实全局热键(走系统事件流,真的切换前台 app),必须由
# 使用者明确同意才跑。场景在头部用标记声明自己抢焦点:
#   # E2E_STEALS_FOCUS=1
#
# 第三类打扰是**屏幕**:场景会在用户的屏幕上钉一个覆盖全屏的背景板,或抓取屏幕内容。它既不是
# "切换前台 app"也不是"改系统偏好",所以有自己的标记与开关,三者可以叠加:
#   # E2E_COVERS_SCREEN=1      -> 默认跳过,需要 --include-screen
# 同意其中一件不等于同意另外两件,所以门是分开的:一个场景全部满足才跑。
#
# Why this exists: AGENTS.md lists A2 as a pre-handoff/release gate, and a gate you have to invoke
# script by script will eventually be run incompletely. Any new executable script dropped into
# scripts/e2e/ is discovered automatically. Scenarios that steal focus declare it in their header
# with `# E2E_STEALS_FOCUS=1` and are skipped unless --include-focus is given. Scenarios that
# temporarily change a system preference declare `# E2E_CHANGES_PREFS=1` and are skipped unless
# --include-prefs is given (they are never run implicitly: they touch global settings). Scenarios that
# pin a full-screen backdrop over the user's desktop or capture the screen declare
# `# E2E_COVERS_SCREEN=1` and are skipped unless --include-screen is given.
#
# 用法:
#   scripts/e2e/run-all.sh                  # 只跑不抢焦点、不盖屏、不改偏好的场景
#   scripts/e2e/run-all.sh --include-focus  # 连抢焦点的场景一起跑(会真的切换前台 app)
#   scripts/e2e/run-all.sh --include-screen # 连会盖住屏幕/抓屏的场景一起跑
#   scripts/e2e/run-all.sh --include-prefs  # 连会改系统偏好/用户配置的场景一起跑
#   scripts/e2e/run-all.sh --include-focus --include-prefs  # 运行抢焦点且改系统偏好的场景
#   scripts/e2e/run-all.sh --list           # 只看本次会跑哪些场景
#
# Exit code: 0 = 所有被选中场景通过;非 0 = 至少一个失败(仍然会跑完剩下的,便于一次看全)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
e2e_dir="$repo_dir/scripts/e2e"
include_focus=0
include_prefs=0
include_screen=0
include_instance=0
gate_mode=0
list_only=0

for arg in "$@"; do
    case "$arg" in
        --include-focus) include_focus=1 ;;
        --include-prefs) include_prefs=1 ;;
        --include-screen) include_screen=1 ;;
        --include-instance) include_instance=1 ;;
        --list) list_only=1 ;;
        --gate) gate_mode=1 ;;
        # The range tracks the header above: it ends where the prose ends and the script begins.
        -h|--help) sed -n '2,33p' "$0"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# A scenario declares every disturbance it causes, and the gates are independent: one scenario may need
# two or three of them, and passing one switch must not admit a scenario that also needs another. The
# declarations are read from the script's own header, one exact line each, so a marker that merely
# mentions the name in prose cannot turn a gate on or off.
scenario_marks() {  # <script> -> prints "focus prefs screen instance" as 0/1
    local script="$1" focus=0 prefs=0 screen=0 instance=0
    grep -q '^# E2E_STEALS_FOCUS=1$' "$script" && focus=1
    grep -q '^# E2E_CHANGES_PREFS=1$' "$script" && prefs=1
    grep -q '^# E2E_COVERS_SCREEN=1$' "$script" && screen=1
    grep -q '^# E2E_REPLACES_INSTANCE=1$' "$script" && instance=1
    printf '%s %s %s %s\n' "$focus" "$prefs" "$screen" "$instance"
}

# The gates, in one place, so the aggregate and the offline selftest cannot disagree about them. The
# order the refusal is named in is the order a person would want to be told: the most disruptive first.
gate_refusal() {  # <focus> <prefs> <screen> <instance> -> the switch that would admit it, or nothing
    local focus="$1" prefs="$2" screen="$3" instance="$4"
    if [ "$focus" = "1" ] && [ "$include_focus" != "1" ]; then
        echo "--include-focus"
    elif [ "$screen" = "1" ] && [ "$include_screen" != "1" ]; then
        echo "--include-screen"
    elif [ "$prefs" = "1" ] && [ "$include_prefs" != "1" ]; then
        echo "--include-prefs"
    elif [ "$instance" = "1" ] && [ "$include_instance" != "1" ]; then
        # Last because it is the least visible consequence, not the least important: a scenario that
        # restarts the app replaces whatever instance the person is using, and nothing on screen
        # says it happened.
        echo "--include-instance"
    fi
}

scenarios=()
nonexec_exec=0
skipped=()
skipped_prefs=()
skipped_screen=()
skipped_instance=()
for script in "$e2e_dir"/*.sh; do
    name="$(basename "$script")"
    # 聚合器自己不是场景。
    # The aggregator is not a scenario.
    [ "$name" = "run-all.sh" ] && continue
    [ -x "$script" ] || {
        if [ "$gate_mode" = "1" ]; then
            # Under the gate a scenario that cannot run is not a scenario that passed.
            echo "e2e run-all: $name is not executable; a scenario the gate cannot run is a failure" >&2
            nonexec_exec=1
        else
            echo "warning: $name is not executable; skipping" >&2
        fi
        continue
    }
    read -r mark_focus mark_prefs mark_screen mark_instance <<<"$(scenario_marks "$script")"
    refusal="$(gate_refusal "$mark_focus" "$mark_prefs" "$mark_screen" "$mark_instance")"
    case "$refusal" in
        --include-focus) skipped+=("$name") ;;
        --include-screen) skipped_screen+=("$name") ;;
        --include-prefs) skipped_prefs+=("$name") ;;
        --include-instance) skipped_instance+=("$name") ;;
        "") scenarios+=("$script") ;;
    esac
done

# Nothing selected is not the same as nothing existing. Every scenario in this directory declares
# at least one disturbance, so a bare run legitimately selects none; that is a refusal, not a failure
# of the tier, and it must say which switch would admit what.
if [ "${#scenarios[@]}" -eq 0 ] && [ "${#skipped[@]}" -eq 0 ] && [ "${#skipped_screen[@]}" -eq 0 ] \
    && [ "${#skipped_prefs[@]}" -eq 0 ] && [ "${#skipped_instance[@]}" -eq 0 ]; then
    echo "e2e run-all: no scenario found in $e2e_dir" >&2
    exit 1
fi

if [ "${#scenarios[@]}" -eq 0 ]; then
    echo "e2e run-all: no scenario selected: every scenario declares a disturbance (focus/screen/prefs/instance)" >&2
    for name in "${skipped[@]:-}"; do [ -n "$name" ] && echo "  $name needs --include-focus"; done
    for name in "${skipped_screen[@]:-}"; do [ -n "$name" ] && echo "  $name needs --include-screen"; done
    for name in "${skipped_prefs[@]:-}"; do [ -n "$name" ] && echo "  $name needs --include-prefs"; done
    for name in "${skipped_instance[@]:-}"; do [ -n "$name" ] && echo "  $name needs --include-instance"; done
    if [ "$gate_mode" = "1" ]; then
        echo "e2e run-all: GATE INCOMPLETE (no scenario was authorized)" >&2
        exit 75
    fi
    exit 0
fi

if [ "$list_only" = "1" ]; then
    printf 'run: %s\n' "${scenarios[@]##*/}"
    [ "${#skipped[@]}" -gt 0 ] && printf 'skip (steals focus): %s\n' "${skipped[@]}"
    [ "${#skipped_screen[@]}" -gt 0 ] && printf 'skip (covers the screen): %s\n' "${skipped_screen[@]}"
    [ "${#skipped_prefs[@]}" -gt 0 ] && printf 'skip (changes system prefs): %s\n' "${skipped_prefs[@]}"
    [ "${#skipped_instance[@]}" -gt 0 ] && printf 'skip (restarts or replaces the running app): %s\n' "${skipped_instance[@]}"
    exit 0
fi

for name in "${skipped[@]:-}"; do
    [ -n "$name" ] && echo "e2e run-all: skipping $name (injects a real global hotkey and steals focus; use --include-focus)"
done
for name in "${skipped_screen[@]:-}"; do
    [ -n "$name" ] && echo "e2e run-all: skipping $name (pins a full-screen backdrop or captures the screen; use --include-screen)"
done
for name in "${skipped_prefs[@]:-}"; do
    [ -n "$name" ] && echo "e2e run-all: skipping $name (temporarily changes a system preference; use --include-prefs)"
done
for name in "${skipped_instance[@]:-}"; do
    [ -n "$name" ] && echo "e2e run-all: skipping $name (restarts the app or replaces the instance you are running; use --include-instance)"
done

failures=0
passed=0
not_run=0
for script in "${scenarios[@]}"; do
    name="$(basename "$script")"
    echo "=== e2e $name ==="
    # A scenario that cannot make its assertions in this environment says so instead of failing, and
    # the aggregate must not then count it among the passes: an unrun check is not a passed check.
    script_log="$(mktemp)"
    if "$script" 2>&1 | tee "$script_log"; then
        if grep -q "NOT RUN" "$script_log"; then
            not_run=$((not_run + 1))
            echo "e2e run-all: $name NOT RUN (its own NOT RUN lines above; not counted as a pass)"
        else
            passed=$((passed + 1))
        fi
    else
        failures=$((failures + 1))
        echo "e2e run-all: $name FAILED"
    fi
    rm -f "$script_log"
done

# The suite must leave a usable app. Scenarios relaunch it with development switches, two of which break it
# for the person using it: `--clipboard-blank-text` draws every panel string that goes through
# `glass::panel_ink` in a transparent ink (row titles, filter labels, keycap legends, the search placeholder),
# while the accents that do not go through it -- the red "clear all" -- keep drawing, so the panel reads as an
# empty white sheet; and `--panel-backdrop` pins a solid black or white window behind it. A scenario that
# forgets to restore hands the user that state, which happened once; every scenario restores in a trap, and
# this is the check that they did. `--panel-outline=off` and `--panel-shadow=off` are on the list for the same
# reason: they are the counter-example frames the edge measurements diff against, so leaving one running hands
# the user a panel with no outline or no shadow.
# Every process with that name, not just the first: the release and development builds are both `oh-my-tab`, so
# checking one of them can pass while the other still carries the switches (reproduced with a second process of
# the same name: the check passed).
leftover=""
for pid in $(pgrep -x oh-my-tab || true); do
    switches="$(ps -o command= -p "$pid" | tr ' ' '\n' | grep -E '^--(clipboard-blank-text|panel-backdrop|panel-outline|panel-shadow)' || true)"
    if [ -n "$switches" ]; then
        leftover="$leftover
  pid $pid: $(ps -o command= -p "$pid")"
    fi
done
if [ -n "$leftover" ]; then
    echo "e2e run-all: the app is still running with a switch that breaks it:"
    echo "$leftover"
    failures=$((failures + 1))
fi

skipped_total=$(( ${#skipped[@]} + ${#skipped_screen[@]} + ${#skipped_prefs[@]} + ${#skipped_instance[@]} ))
echo "e2e run-all: $passed passed, $not_run not run, $failures failed, $skipped_total skipped (focus/screen/prefs/instance)"

if [ "$failures" -ne 0 ] || [ "$nonexec_exec" -ne 0 ]; then
    exit 1
fi
if [ "$gate_mode" = "1" ]; then
    # Under the gate "incomplete" is its own outcome: nothing failed, but scenarios that declare a
    # disturbance were skipped for want of consent, or a scenario reported NOT RUN. Counting that as
    # success is how a bare run could pass while a third of the tier never ran.
    if [ "$skipped_total" -gt 0 ] || [ "$not_run" -gt 0 ]; then
        echo "e2e run-all: GATE INCOMPLETE ($skipped_total scenario(s) require --include-*, $not_run did not run)"
        exit 75
    fi
fi
exit 0
