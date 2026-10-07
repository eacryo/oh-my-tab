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
# Why this exists: AGENTS.md lists A2 as a pre-handoff/release gate, and a gate you have to invoke
# script by script will eventually be run incompletely. Any new executable script dropped into
# scripts/e2e/ is discovered automatically. Scenarios that steal focus declare it in their header
# with `# E2E_STEALS_FOCUS=1` and are skipped unless --include-focus is given. Scenarios that
# temporarily change a system preference declare `# E2E_CHANGES_PREFS=1` and are skipped unless
# --include-prefs is given (they are never run implicitly: they touch global settings).
#
# 用法:
#   scripts/e2e/run-all.sh                  # 只跑不抢焦点的场景
#   scripts/e2e/run-all.sh --include-focus  # 连抢焦点的场景一起跑(会真的切换前台 app)
#   scripts/e2e/run-all.sh --include-focus --include-prefs  # 运行抢焦点且改系统偏好的场景
#   scripts/e2e/run-all.sh --list           # 只看本次会跑哪些场景
#
# Exit code: 0 = 所有被选中场景通过;非 0 = 至少一个失败(仍然会跑完剩下的,便于一次看全)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
e2e_dir="$repo_dir/scripts/e2e"
include_focus=0
include_prefs=0
list_only=0

for arg in "$@"; do
    case "$arg" in
        --include-focus) include_focus=1 ;;
        --include-prefs) include_prefs=1 ;;
        --list) list_only=1 ;;
        -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

scenarios=()
skipped=()
skipped_prefs=()
for script in "$e2e_dir"/*.sh; do
    name="$(basename "$script")"
    # 聚合器自己不是场景。
    # The aggregator is not a scenario.
    [ "$name" = "run-all.sh" ] && continue
    [ -x "$script" ] || {
        echo "warning: $name is not executable; skipping" >&2
        continue
    }
    if grep -q '^# E2E_STEALS_FOCUS=1' "$script" && [ "$include_focus" != "1" ]; then
        skipped+=("$name")
        continue
    fi
    if grep -q '^# E2E_CHANGES_PREFS=1' "$script" && [ "$include_prefs" != "1" ]; then
        skipped_prefs+=("$name")
        continue
    fi
    scenarios+=("$script")
done

if [ "${#scenarios[@]}" -eq 0 ]; then
    echo "e2e run-all: no scenarios selected (nothing in $e2e_dir?)" >&2
    exit 1
fi

if [ "$list_only" = "1" ]; then
    printf 'run: %s\n' "${scenarios[@]##*/}"
    [ "${#skipped[@]}" -gt 0 ] && printf 'skip (steals focus): %s\n' "${skipped[@]}"
    [ "${#skipped_prefs[@]}" -gt 0 ] && printf 'skip (changes system prefs): %s\n' "${skipped_prefs[@]}"
    exit 0
fi

for name in "${skipped[@]:-}"; do
    [ -n "$name" ] && echo "e2e run-all: skipping $name (injects a real global hotkey and steals focus; use --include-focus)"
done
for name in "${skipped_prefs[@]:-}"; do
    [ -n "$name" ] && echo "e2e run-all: skipping $name (temporarily changes a system preference; use --include-prefs)"
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
# this is the check that they did.
# Every process with that name, not just the first: the release and development builds are both `oh-my-tab`, so
# checking one of them can pass while the other still carries the switches (reproduced with a second process of
# the same name: the check passed).
leftover=""
for pid in $(pgrep -x oh-my-tab || true); do
    switches="$(ps -o command= -p "$pid" | tr ' ' '\n' | grep -E '^--(clipboard-blank-text|panel-backdrop)' || true)"
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

echo "e2e run-all: $passed passed, $not_run not run, $failures failed, $(( ${#skipped[@]} + ${#skipped_prefs[@]} )) skipped (focus/prefs)"
[ "$failures" -eq 0 ]
