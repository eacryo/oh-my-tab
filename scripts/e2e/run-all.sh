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
# with `# E2E_STEALS_FOCUS=1` and are skipped unless --include-focus is given.
#
# 用法:
#   scripts/e2e/run-all.sh                  # 只跑不抢焦点的场景
#   scripts/e2e/run-all.sh --include-focus  # 连抢焦点的场景一起跑(会真的切换前台 app)
#   scripts/e2e/run-all.sh --list           # 只看本次会跑哪些场景
#
# Exit code: 0 = 所有被选中场景通过;非 0 = 至少一个失败(仍然会跑完剩下的,便于一次看全)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
e2e_dir="$repo_dir/scripts/e2e"
include_focus=0
list_only=0

for arg in "$@"; do
    case "$arg" in
        --include-focus) include_focus=1 ;;
        --list) list_only=1 ;;
        -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

scenarios=()
skipped=()
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
    scenarios+=("$script")
done

if [ "${#scenarios[@]}" -eq 0 ]; then
    echo "e2e run-all: no scenarios selected (nothing in $e2e_dir?)" >&2
    exit 1
fi

if [ "$list_only" = "1" ]; then
    printf 'run: %s\n' "${scenarios[@]##*/}"
    [ "${#skipped[@]}" -gt 0 ] && printf 'skip (steals focus): %s\n' "${skipped[@]}"
    exit 0
fi

for name in "${skipped[@]:-}"; do
    [ -n "$name" ] && echo "e2e run-all: skipping $name (injects a real global hotkey and steals focus; use --include-focus)"
done

failures=0
passed=0
for script in "${scenarios[@]}"; do
    name="$(basename "$script")"
    echo "=== e2e $name ==="
    if "$script"; then
        passed=$((passed + 1))
    else
        failures=$((failures + 1))
        echo "e2e run-all: $name FAILED"
    fi
done

echo "e2e run-all: $passed passed, $failures failed, ${#skipped[@]} skipped"
[ "$failures" -eq 0 ]
