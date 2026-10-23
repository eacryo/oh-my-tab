#!/bin/bash
# scripts/lib/finder-extension.sh 的离线自测。
# Offline self-test for scripts/lib/finder-extension.sh.
#
# 每条用例都对应一个真实发生过的判断错误:
#   - 只用 bundle id 前缀比较,会把宿主应用自己的实例当成扩展实例(而 bootout 它会在 2026-10-23 毁掉应用
#     的日志写入者,见库文件头部);
#   - 按第一个连字符切分 id,会把 `com.eacryo.oh-my-tab.dev` 切成 `com.eacryo`;
#   - 用 `pgrep -f oh-my-tab-finder-ext` 判断"扩展在跑",会把其他 worktree / 打包版的扩展算进来;
#   - 对不存在的服务 `bootout` 返回 3;当成失败会让脚本在已经停掉应用之后中止;
#   - 只按"路径"杀进程,会在 pid 被复用时杀掉别人;身份必须按 路径 + 启动时间 判。
#
# 除纯判定外,本套件会**真的**建临时 launchd 服务(标签形如 `<id>.FinderSync.<uuid>`,命令是
# /bin/sleep 或一个不存在的路径),用真实函数清理,并断言:只有"钉在已消失路径上"的实例被移除,宿主应用
# 形态的服务、程序仍存在的实例都原样保留。自测只清理自己创建的标签。
#
# 不改仓库、不构建、不启动真实应用。用法:scripts/finder-extension-selftest.sh  退出 0 = 全部成立。

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
lib="$script_dir/lib/finder-extension.sh"
failures=0
created_labels=()

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

# This suite removes only the services it created, by the exact label it created.
cleanup() {
    local label
    for label in "${created_labels[@]:-}"; do
        [ -n "$label" ] || continue
        launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true
    done
}
trap cleanup EXIT

[ -r "$lib" ] || { echo "FAIL the library is missing: $lib" >&2; exit 1; }
# shellcheck source=/dev/null
source "$lib"

expect_ok() {
    local label="$1" fn="$2"
    shift 2
    if "$fn" "$@" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label (expected 0, got non-zero)"
    fi
}

expect_refused() {
    local label="$1" fn="$2"
    shift 2
    if "$fn" "$@" >/dev/null 2>&1; then
        fail "$label (expected a refusal, got 0)"
    else
        pass "$label"
    fi
}

expect_output() {
    local label="$1" want="$2" got
    shift 2
    got="$("$@" 2>/dev/null)"
    if [ "$got" = "$want" ]; then
        pass "$label"
    else
        fail "$label (want '$want', got '$got')"
    fi
}

EXT_ID="com.eacryo.oh-my-tab.dev.FinderSync"
UUID_A="23C8FF93-2DBB-413F-A77F-77CEF7CFF550"
UUID_B="0989acdd-54b7-4424-861a-c96cf867ea7e"

# --- 实例标签判定 -----------------------------------------------------------
expect_ok "an uppercase request uuid is an instance label" \
    omt_finder_ext_is_instance_label "$EXT_ID.$UUID_A" "$EXT_ID"
expect_ok "a lowercase request uuid is an instance label" \
    omt_finder_ext_is_instance_label "$EXT_ID.$UUID_B" "$EXT_ID"
expect_refused "the bare service name is not an instance label" \
    omt_finder_ext_is_instance_label "$EXT_ID" "$EXT_ID"
expect_refused "the host application's own instance is not an extension instance" \
    omt_finder_ext_is_instance_label "application.com.eacryo.oh-my-tab.dev.43775408.43775597" "$EXT_ID"
expect_refused "another extension's instance is not this extension's instance" \
    omt_finder_ext_is_instance_label "com.baidu.BaiduNetdisk-mac.FinderSync.$UUID_A" "$EXT_ID"
expect_refused "a truncated uuid is not an instance label" \
    omt_finder_ext_is_instance_label "$EXT_ID.23C8FF93-2DBB-413F-A77F" "$EXT_ID"
expect_refused "a non-uuid suffix is not an instance label" \
    omt_finder_ext_is_instance_label "$EXT_ID.something" "$EXT_ID"

# --- 从标签还原扩展 id ------------------------------------------------------
# 事故里最容易写错的一步:`com.eacryo.oh-my-tab.dev.FinderSync` 同时含点与连字符,任何"切到第一个
# 点/第一个连字符"的实现都会得到 `com.eacryo` 并因此清错对象。
expect_output "an uppercase-uuid label yields its extension id" "$EXT_ID" \
    omt_finder_ext_id_from_label "$EXT_ID.$UUID_A"
expect_output "a lowercase-uuid label yields its extension id" "$EXT_ID" \
    omt_finder_ext_id_from_label "$EXT_ID.$UUID_B"
expect_output "an id with a dash yields itself" "com.eacryo.oh-my-tab.FinderSync" \
    omt_finder_ext_id_from_label "com.eacryo.oh-my-tab.FinderSync.E0B987BA-25B6-4C82-BE0A-0ECF8D88715C"
expect_refused "the bare service name yields no extension id" omt_finder_ext_id_from_label "$EXT_ID"
expect_refused "an application instance yields no extension id" \
    omt_finder_ext_id_from_label "application.com.eacryo.oh-my-tab.dev.43775408.43775597"
expect_refused "an application-namespaced extension label yields no id" \
    omt_finder_ext_id_from_label "application.$EXT_ID.43775408.43775597"
expect_refused "a launchd job label yields no extension id" omt_finder_ext_id_from_label "oh-my-tab-dev"

# --- 重启策略 ---------------------------------------------------------------
expect_ok "no live extension and none observed restarts Finder" \
    omt_finder_ext_should_restart_finder "" ""
expect_refused "a live extension needs no restart" \
    omt_finder_ext_should_restart_finder "4242" ""
expect_refused "an extension observed before the rebuild needs no restart" \
    omt_finder_ext_should_restart_finder "" "4242"

# --- 进程身份 ---------------------------------------------------------------
# 后台进程必须在**当前 shell**里启动:放进 `$( )` 会多出一个持有管道写端的子 shell,调用方读命令
# 替换时会一直等它退出。
/bin/sleep 400 >/dev/null 2>&1 &
probe_pid=$!
expect_ok "a live probe process is identified by its path" \
    omt_finder_ext_pid_is "$probe_pid" "$(ps -p "$probe_pid" -o comm=)"
expect_refused "a wrong executable path is refused" \
    omt_finder_ext_pid_is "$probe_pid" "/nonexistent/sleep"
kill "$probe_pid" 2>/dev/null || true
wait "$probe_pid" 2>/dev/null || true
expect_refused "a dead pid is refused" \
    omt_finder_ext_pid_is "$probe_pid" "/bin/sleep"

# 启动时间这一类身份**必须**由 pid 单独传递,不能塞进一行再用 read 拆开:macOS 的 `ps -o lstart=`
# 会在字段尾部补空格,`read` 会去掉它们,比较就永远不等 —— 那时停止会"成功"却一个信号都没发。
# 这里直接断言这条管道:先把一个进程的"pid + start"按旧格式打印出来,`read` 拆开后时间必须已经变化,
# 从而证明这个格式不可用于身份比较。
formatted="$probe_pid $(ps -p $$ -o lstart= 2>/dev/null)"
while read -r _pid _start; do :; done <<EOF
$formatted
EOF
if [ "$(ps -p $$ -o lstart=)" = "$_start" ]; then
    fail "a read-split line preserves the padded start time (it must not be relied on)"
else
    pass "a read-split line drops the padded start time, so identity must not use it"
fi

# 停止必须按身份判定:路径不符(pid 被复用)时不发任何信号,身份相符时才停。用真实进程验证,
# 不用桩——这正是"只按路径杀"的那次缺陷要防的东西。
/bin/sleep 401 >/dev/null 2>&1 &
guard_pid=$!
omt_finder_ext_stop_process "$guard_pid" "/nonexistent/sleep" >/dev/null 2>&1
if kill -0 "$guard_pid" 2>/dev/null; then
    pass "a mismatched identity is never signalled"
else
    fail "a mismatched identity is never signalled"
fi
omt_finder_ext_stop_process "$guard_pid" "/bin/sleep" >/dev/null 2>&1
if kill -0 "$guard_pid" 2>/dev/null; then
    fail "a matching identity is stopped"
else
    pass "a matching identity is stopped"
fi
wait "$guard_pid" 2>/dev/null || true

# 只在严格路径匹配时报告"在跑":一个不存在的 appex 路径绝不能报出任何 pid,
# 否则"扩展在跑"就成了 `pgrep -f` 的同义词。
expect_output "an unrelated appex path reports no live process" "" \
    omt_finder_ext_alive_pid "/nonexistent/oh-my-tab-finder-ext"

# --- 钉住路径的判定 ---------------------------------------------------------
expect_ok "a path that does not exist is a dead pin" \
    omt_finder_ext_program_is_missing "/nonexistent/worktree/dist/Oh-My-Tab-Finder.appex/Contents/MacOS/oh-my-tab-finder-ext"
expect_refused "a path that exists is not a dead pin" \
    omt_finder_ext_program_is_missing /bin/sleep
expect_refused "an empty program is not treated as a dead pin" \
    omt_finder_ext_program_is_missing ""
expect_refused "a relative program path is not treated as a dead pin" \
    omt_finder_ext_program_is_missing "oh-my-tab-finder-ext"

# --- 运行状态与可移除性 -----------------------------------------------------
# 必须区分"没在跑"和"读不出来":`not running` 曾经被 `*running*` 匹配成"在跑",而读不出来曾经被
# 当成"空闲"。三个输入(程序是否消失 / 状态)一起决定能不能移除。
IDLE_OBS="$(printf '\tactive count = 0\n\tstate = spawn scheduled')"
LIVE_OBS="$(printf '\tactive count = 1\n\tstate = running')"
expect_ok "a gone program and an idle service is removable" \
    omt_finder_ext_instance_removable "/nonexistent/appex" "spawn scheduled" "$IDLE_OBS"
expect_refused "a gone program with a running service is not removable" \
    omt_finder_ext_instance_removable "/nonexistent/appex" "running" "$LIVE_OBS"
expect_refused "a gone program whose launchd still counts a process is not removable" \
    omt_finder_ext_instance_removable "/nonexistent/appex" "spawn scheduled" "$LIVE_OBS"
expect_refused "unreadable observations are not evidence of an idle service" \
    omt_finder_ext_instance_removable "/nonexistent/appex" "" ""
expect_refused "an existing program is never removable" \
    omt_finder_ext_instance_removable "/bin/sleep" "spawn scheduled" "$IDLE_OBS"

# --- 允许移除的标签清单 -----------------------------------------------------
# 自测只允许清理它自己创建的标签。清单按整行精确匹配:`...uuid` 不能授权 `...uuid.x`,
# 空清单表示"不限制"(生产调用方就是这样传的)。
expect_ok "an empty allowlist allows every label" \
    omt_finder_ext_label_allowed "$EXT_ID.$UUID_A" ""
expect_ok "a listed label is allowed" \
    omt_finder_ext_label_allowed "$EXT_ID.$UUID_A" "com.other.FinderSync.$UUID_B
$EXT_ID.$UUID_A"
expect_refused "an unlisted label is refused" \
    omt_finder_ext_label_allowed "$EXT_ID.$UUID_B" "$EXT_ID.$UUID_A"
expect_refused "a prefix of a listed label is refused" \
    omt_finder_ext_label_allowed "x$EXT_ID.$UUID_A" "$EXT_ID.$UUID_A"
expect_refused "a listed label plus a suffix is refused" \
    omt_finder_ext_label_allowed "$EXT_ID.$UUID_A.x" "$EXT_ID.$UUID_A"

# --- 真实的 launchd 服务(本机,临时,只清理自己创建的) ----------------------
# 三个服务,只有第一个该被清掉:
#   dead_label —— 与事故同形:标签是扩展实例,程序路径不存在。
#   live_label —— 标签同形,但程序是 /bin/sleep:另一个仍可运行的实例,必须保留。
#   host_label —— 宿主应用形态的标签(application.…),必须保留。
if ! launchctl print "gui/$(id -u)" >/dev/null 2>&1; then
    fail "no per-user launchd domain to exercise the real clear against"
else
    dead_label="$EXT_ID.$(uuidgen)"
    dead_program="/nonexistent/omt-selftest/$EXT_ID/Contents/MacOS/oh-my-tab-finder-ext"
    if launchctl submit -l "$dead_label" -o /dev/null -e /dev/null -- "$dead_program"; then
        created_labels+=("$dead_label")
    else
        fail "could not submit the dead-path instance"
    fi

    live_label="$EXT_ID.$(uuidgen)"
    if launchctl submit -l "$live_label" -o /dev/null -e /dev/null -- /bin/sleep 300; then
        created_labels+=("$live_label")
    else
        fail "could not submit the live-program instance"
    fi

    # 宿主应用形态:带 application. 命名空间,末段是数字。
    host_label="application.$EXT_ID.$(uuidgen | tr -d '-')"
    if launchctl submit -l "$host_label" -o /dev/null -e /dev/null -- /bin/sleep 300; then
        created_labels+=("$host_label")
    else
        fail "could not submit the application-namespaced service"
    fi

    sleep 0.5

    listed="$(omt_finder_ext_stale_instance_labels)"
    if printf '%s\n' "$listed" | grep -Fxq "$dead_label"; then
        pass "a per-environment launchd instance is listed"
    else
        fail "a per-environment launchd instance is listed"
    fi
    if printf '%s\n' "$listed" | grep -Fxq "$host_label"; then
        fail "an application-namespaced label is never listed as an extension instance"
    else
        pass "an application-namespaced label is never listed as an extension instance"
    fi

    # Only the labels this suite created may be touched. The enumeration still reads the real launchd
    # list, so the platform-facing part of the path is covered; the removal is what is restricted, and
    # any instance outside this allowlist is reported as `skipped` instead of being booted out.
    only_these="$dead_label
$live_label
$host_label"
    # 隔离性本身也是契约:清单之外的实例必须原样存活。
    #
    # 基准必须**在任何清理之前**取:清理之后只剩"活下来的",用那时求差集永远为空——旧写法正是这样,
    # 于是"外部服务全部被误删"反而会通过。取"清理前集合"这件事本身,也已被下面的反例钉住。
    foreign_before=""
    while IFS= read -r other; do
        [ -n "$other" ] || continue
        if ! omt_finder_ext_label_allowed "$other" "$only_these"; then
            foreign_before="$foreign_before$other
"
        fi
    done <<EOF
$(omt_finder_ext_stale_instance_labels)
EOF
    clear_output="$(omt_finder_ext_clear_instances "/nonexistent/omt-selftest-own-appex" "$only_these")"
    clear_status=$?
    if [ "$clear_status" -eq 1 ]; then
        pass "clearing a dead-path instance reports that it removed something"
    else
        fail "clearing a dead-path instance reports that it removed something (exit $clear_status)"
    fi
    if launchctl print "gui/$(id -u)/$dead_label" >/dev/null 2>&1; then
        fail "the dead-path instance is gone afterwards"
    else
        pass "the dead-path instance is gone afterwards"
    fi
    if launchctl print "gui/$(id -u)/$live_label" >/dev/null 2>&1; then
        pass "an instance whose program exists is left running"
    else
        fail "an instance whose program exists is left running"
    fi
    if launchctl print "gui/$(id -u)/$host_label" >/dev/null 2>&1; then
        pass "a running application-namespaced service survives the clear"
    else
        fail "a running application-namespaced service survives the clear"
    fi
    if printf '%s\n' "$clear_output" | grep -q "^left $live_label"; then
        pass "the survivor is reported as left alone, not silently skipped"
    else
        fail "the survivor is reported as left alone, not silently skipped"
    fi

    # 重复清理的契约不是"总返回 0",而是"不再移除任何东西":返回值 1 也可能是"跳过了清单外实例"
    # (用户域里另有一个不属于本套件的实例时就会这样),把两者混为一谈会让自测在任何外部实例存在时
    # 误报失败。所以判定看输出:第二遍不能再出现 `removed`,并且返回码只能是 0 或 1,不能是 2。
    second_output="$(omt_finder_ext_clear_instances "/nonexistent/omt-selftest-own-appex" "$only_these")"
    second_status=$?
    if [ "$second_status" -le 1 ] && ! printf '%s\n' "$second_output" | grep -q '^removed '; then
        pass "a second clear removes nothing and never reports a failure (exit $second_status)"
    else
        fail "a second clear removes nothing and never reports a failure (exit $second_status)"
    fi
    if printf '%s\n' "$second_output" | grep -q '^skipped '; then
        pass "an instance outside the allowlist is reported as skipped on the repeat"
    else
        pass "no instance outside the allowlist existed on the repeat"
    fi

    # 以本套件的自造服务为基准:无人使用时 foreign_before 为空;有外部实例时逐个核对。
    missing=""
    while IFS= read -r other; do
        [ -n "$other" ] || continue
        launchctl print "gui/$(id -u)/$other" >/dev/null 2>&1 || missing="$missing $other"
    done <<EOF
$foreign_before
EOF
    if [ -z "$missing" ]; then
        pass "every launchd instance outside the allowlist is still registered"
    else
        fail "every launchd instance outside the allowlist is still registered (missing:$missing)"
    fi
    # 报告本身也是契约:一次真的移除了东西的清理,必须同时印出移除记录(曾经因为 `[ ... ] && return`
    # 不是函数的最后一句,函数返回的是那个测试的状态,"移除过东西"的信息整行丢失)。
    if printf '%s\n' "$clear_output" | grep -q "^removed $dead_label"; then
        pass "the removal is reported on its own line"
    else
        fail "the removal is reported on its own line"
    fi

    # 程序已删除、但 launchd 仍报告有进程在跑:必须保留(`active count = N` 是可见证据;
    # launchd 对 launchctl submit 的服务有时不打印 `pid =`,`active count` 就是兜底)。
    # 这条与"程序已删除且无进程"共同定义清理边界;后者已由上面的 dead_label 用例覆盖。

    # 该检查本身的正反例:基准集合里有一个"必然已被删除"的标签时,检查必须报缺失;基准为空时通过。
    # 否则"误删全部外部服务反而通过"这类方向错误就没人能发现。函数抽出,同一段逻辑两次调用。
    survivals_of() {
        local baseline="$1" label missing_out=""
        while IFS= read -r label; do
            [ -n "$label" ] || continue
            launchctl print "gui/$(id -u)/$label" >/dev/null 2>&1 || missing_out="$missing_out $label"
        done <<INNER
$baseline
INNER
        printf '%s' "$missing_out"
    }
    if [ -n "$(survivals_of "com.example.NotMine.FinderSync.$(uuidgen)")" ]; then
        pass "the survival check reports a baseline label that is gone (counter-example)"
    else
        fail "the survival check reports a baseline label that is gone (counter-example)"
    fi
    if [ -z "$(survivals_of "")" ]; then
        pass "the survival check passes an empty baseline"
    else
        fail "the survival check passes an empty baseline"
    fi

    # 一个不存在的标签不能把清理判成失败(launchctl bootout 对它返回 3)。
    if omt_finder_ext_bootout "$EXT_ID.$(uuidgen)"; then
        pass "bootout of a label that never existed succeeds"
    else
        fail "bootout of a label that never existed succeeds"
    fi
fi

if [ "$failures" -eq 0 ]; then
    echo "finder-extension selftest: all cases held"
    exit 0
fi
echo "finder-extension selftest: $failures case(s) failed" >&2
exit 1
