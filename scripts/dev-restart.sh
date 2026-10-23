#!/bin/bash
# 开发重启脚本:优雅退出旧进程 → 编译并组装开发版 .app → 启动 .app → 校验存活。
# 由 agent 在 cargo fmt/check/clippy/test 全绿后执行(见 AGENTS.md 约定)。
# 默认 debug 构建(迭代快、断言全开);`--opt` 走 dev-opt profile(优化接近 release,
# 但保留 debug 断言),用于滚动/动画等体感与性能验证。
#
# 传递参数给应用(用于验证只在特定状态下出现的功能,如首次引导/权限分支):
#   scripts/dev-restart.sh -- --force-onboarding   # `--` 之后的 argv 原样透传给应用
#   scripts/dev-restart.sh --force-onboarding      # 脚本不认识的 --* 参数也照样透传
#   scripts/dev-restart.sh --no-onboarding         # 跳过本次启动的引导
# 开关只在本次启动生效:脚本每次先按身份停掉旧实例,不会残留。
# 为什么不用环境变量:开发版由 launchd 启动,而 launchd 任务不继承调用者环境;以前靠白名单转发
# OH_MY_TAB_*,一旦过滤写漏就会把整份环境(云凭证、代理)灌进任务与日志 —— 2026-09-22 出过这次
# 事故。现在只有 argv 一个通道:本脚本**不读、不转发、不回显任何环境变量**,也不要再加回来;
# 其它开发开关在应用侧解析 `--` 参数即可,通用透传逻辑会负责转发。
# Dev restart script: gracefully quit the old process -> build and assemble the dev .app ->
# start the .app -> verify it is alive. Run by the agent after the
# fmt/check/clippy/test gates pass (see the AGENTS.md convention).
# Default is the fast debug build (full assertions); `--opt` uses the dev-opt profile
# (optimized close to release while keeping debug assertions) for feel/perf validation.
#
# Passing arguments to the app (for verifying features that only appear in a specific state,
# e.g. first-run onboarding or permission branches):
#   scripts/dev-restart.sh -- --force-onboarding   # argv after `--` is forwarded verbatim
#   scripts/dev-restart.sh --force-onboarding      # any `--*` argument the script does not own
#   scripts/dev-restart.sh --no-onboarding         # suppress the guide for this launch
# Switches apply to this launch only: the script stops old instances by identity first, nothing
# sticks. Stopping an instance is a transaction under a cross-worktree claim (scripts/lib/dev-lock.sh);
# the claim is released when this script returns, so the window it leaves is NOT protected.
#   scripts/dev-restart.sh --lock-wait=60           # seconds to wait for another worktree's transaction
# Why no environment variables: the dev build is started by launchd, and a launchd job does not
# inherit the caller's environment. The allowlist that used to forward OH_MY_TAB_* dumped the whole
# environment (cloud credentials, proxies) into the job and the logs whenever the filter was wrong --
# which happened once, on 2026-09-22. Argv is the only channel now: this script reads, forwards and
# echoes no environment variable at all, and one must not be reintroduced. Other development
# switches are parsed from `--` arguments on the app side and use the generic passthrough below.

# Resolve paths from this script, not from the caller's current directory. This
# keeps both `./scripts/dev-restart.sh` and an absolute-path invocation working.
# 根据脚本自身位置定位项目根目录,不依赖调用者当前所在的目录。
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"

# 可选参数解析:决定构建 profile,以及透传给应用的 argv。
# Optional flag parsing: chooses the build profile, plus the argv forwarded to the app.
build_profile="debug"
cargo_profile_args=()
app_args=()
after_separator=0
require_stable_signing=0
allow_signing_fallback=0
lock_wait_seconds=900
check_mode=0
reload_finder_extension=0
ownership_probe=0
ownership_expect_pid=""
ownership_expect_start=""
for arg in "$@"; do
    if [ "$after_separator" = "1" ]; then
        app_args+=("$arg")
        continue
    fi
    case "$arg" in
        --) after_separator=1 ;;
        --opt) build_profile="dev-opt" ;;
        --require-stable-signing) require_stable_signing=1 ;;
        --allow-signing-fallback) allow_signing_fallback=1 ;;
        --no-onboarding) app_args+=("$arg") ;;
        --reload-finder-extension)
            # Finder caches the extension's process and menu; restarting Finder is the only reliable
            # way to make a rebuilt extension visible, and it closes every Finder window, so it is
            # an explicit opt-in.
            reload_finder_extension=1
            ;;
        --check)
            # Verify a launch, then stop it: the shape an automated check wants, which is the
            # opposite of the interactive default. It leaves neither this run's process nor its
            # launchd job behind, and makes no claim about config, clipboard or cache state.
            check_mode=1
            ;;
        --whose-instance)
            # A read-only query: no build, no launch, no stop, and no claim (see
            # scripts/lib/dev-instance-ownership.sh for why the claim would be the wrong tool).
            ownership_probe=1
            ;;
        --identity-expects=*)
            # Re-check a snapshot an earlier --whose-instance returned: "is pid X, started at T,
            # still the instance running this worktree's build?" A UI conclusion belongs to that
            # identity, so it is only valid while the identity still holds.
            ownership_probe=1
            ownership_expect_pid="${arg#--identity-expects=}"
            ownership_expect_start=""
            case "$ownership_expect_pid" in
                *,*) ownership_expect_start="${ownership_expect_pid#*,}"; ownership_expect_pid="${ownership_expect_pid%%,*}" ;;
            esac
            case "$ownership_expect_pid" in
                ''|*[!0-9]*)
                    echo "restart FAILED: --identity-expects needs <pid> or <pid>,<started>: $arg"
                    exit 2
                    ;;
            esac
            ;;
        --lock-wait=*)
            lock_wait_seconds="${arg#--lock-wait=}"
            case "$lock_wait_seconds" in
                ''|*[!0-9]*)
                    echo "restart FAILED: --lock-wait needs a whole number of seconds: $arg"
                    exit 2
                    ;;
            esac
            ;;
        -h|--help)
            echo "Usage: scripts/dev-restart.sh [--opt] [--require-stable-signing] [--allow-signing-fallback] [--no-onboarding] [-- <app args>...]"
            echo "  (no flag)  debug build: fast iteration, complete debug assertions"
            echo "  --opt      dev-opt profile: optimized, debug assertions kept (feel/perf)"
            echo "  --require-stable-signing  require the Apple-issued identity: refuse the self-signed"
            echo "  --allow-signing-fallback  permit the self-signed fallback when the Apple-issued"
            echo "                            identity cannot sign (default: that is a build failure, because a"
            echo "                            fallback bundle has a DIFFERENT identity and loses the keychain"
            echo "                            and TCC grants recorded for the Apple identity)"
            echo "  --no-onboarding  forward to the app and suppress the guide for this launch"
            echo "  --check    verify the launch, then stop it and its launchd job (leaves nothing running)"
            echo "  --whose-instance  print which build is running and whether it came from this worktree;"
            echo "             builds, launches and stops nothing, and takes no claim. Exit: 0 mine,"
            echo "             1 not mine (or a stale build of this tree), 3 no instance, 4 unverified,"
            echo "             5 contested by another instance, 6 this worktree has no built bundle."
            echo "             Any non-zero verdict means a UI conclusion drawn now is not this tree's build."
            echo "  --identity-expects=<pid>[,<started>]  re-check that pid is STILL this worktree's instance"
            echo "             (same exit codes); use it after a UI check to show the instance was not replaced"
            echo "  --lock-wait=<sec>  wait this long for another worktree's build/test/launch transaction"
            echo "                     (default 900); the claim is released when this script returns, so the"
            echo "                     window it leaves is NOT protected from the next run"
            echo "  -- ARGS    forward ARGS to the app executable, verbatim"
            echo "  --FLAG     every other --flag[=value] is forwarded to the app as well, e.g."
            echo "             scripts/dev-restart.sh --open-settings=about --force-onboarding"
            echo "  app switches in use: --open-settings[=<page>|<0..6>|1] (page = general|switcher|"
            echo "             mouse|clipboard|key-bindings|keystroke-display|about; bare/1 = about),"
            echo "             --force-onboarding,"
            echo "             --onboarding=reset, --no-onboarding, --fake-permissions=ax:0,sr:0,"
            echo "             --pseudo-locale, --layout-debug, --test-update-notice[=available],"
            echo "             --smoke-update-prompts (opens 12 key windows and takes focus; run only with consent),"
            echo "             --show-clipboard (open the clipboard picker without its hotkey),"
            echo "             --no-settings-prewarm (skip the startup settings warm-up; the cold-open"
            echo "             counter-example for scripts/e2e/settings-open-latency.sh),"
            echo "             --e2e-state=<path> (JSON state snapshots for scripts/e2e/*.sh)"
            echo "No environment variable is read, forwarded or echoed (see the header)."
            exit 0
            ;;
        --*)
            # 应用自己的开发开关:脚本不认识也照样透传,新增开关不需要改这里。
            # The app's own development switches: unknown to this script, forwarded anyway, so a
            # new switch never needs a change here.
            app_args+=("$arg")
            ;;
        *)
            echo "restart FAILED: unknown argument: $arg"
            echo "hint: app switches start with '--' (e.g. scripts/dev-restart.sh --force-onboarding)"
            exit 2
            ;;
    esac
done
if [ "$require_stable_signing" = "1" ] && [ "$allow_signing_fallback" = "1" ]; then
    echo "restart FAILED: --require-stable-signing and --allow-signing-fallback contradict each other"
    echo "hint: --require-stable-signing demands the Apple-issued identity; --allow-signing-fallback"
    echo "hint: permits the self-signed one when that identity cannot sign."
    exit 2
fi
# A query and a transaction are different answers to different questions; asking for both in one
# run would either build under a flag that promises not to, or return a verdict nobody asked for.
if [ "$ownership_probe" = "1" ] && { [ "$check_mode" = "1" ] || [ "${#app_args[@]}" -gt 0 ]; }; then
    echo "restart FAILED: --whose-instance/--identity-expects cannot be combined with --check or app args"
    echo "hint: run --whose-instance on its own; it builds, launches and stops nothing."
    exit 2
fi
if [ "$build_profile" = "dev-opt" ]; then
    cargo_profile_args=(--profile dev-opt)
fi
build_target_dir="$repo_dir/target/$build_profile"
dev_app="$repo_dir/dist/Oh-My-Tab-Dev.app"
dev_app_binary="$dev_app/Contents/MacOS/oh-my-tab"
# The Finder Sync app extension: its own nested bundle, its own id (derived from the app's) and its
# own sandbox entitlement.
dev_appex="$dev_app/Contents/PlugIns/Oh-My-Tab-Finder.appex"
dev_appex_binary="$dev_appex/Contents/MacOS/oh-my-tab-finder-ext"
dev_bundle_id="com.eacryo.oh-my-tab.dev"
dev_appex_bundle_id="$dev_bundle_id.FinderSync"

# The ownership query runs BEFORE anything that could touch the repository or an instance: no
# release-note requirement, no claim, no build. It answers a question about the machine's current
# state, and a state that another run may be changing this very moment is exactly what it is for;
# taking the claim would make it wait for the replacement it is trying to detect.
if [ "$ownership_probe" = "1" ]; then
    source "$repo_dir/scripts/lib/dev-instance-ownership.sh"
    ownership_log="$HOME/Library/Logs/oh-my-tab/oh-my-tab.log"
    omt_ownership_probe "$dev_app_binary" "$ownership_log" \
        "$ownership_expect_pid" "$ownership_expect_start"
    exit $?
fi

dev_version="$(awk -F'"' '/^version/ {print $2; exit}' "$repo_dir/Cargo.toml")"
dev_release_doc="$repo_dir/release_doc_dev/${dev_version}.md"
if [ ! -s "$dev_release_doc" ]; then
    echo "restart FAILED: release notes not found for version $dev_version: $dev_release_doc"
    exit 1
fi

# 开发实例的互斥声明:同一时刻只允许一个 构建-测试-启动 事务,跨 worktree 生效。
# The development-instance claim: one build-test-launch transaction at a time, across worktrees.
# Every worktree shares the bundle id, launchd label and app log below, so two concurrent runs
# fight over one instance and the loser's bootout kills an instance the other run is verifying.
# The claim covers the transaction only -- the app this script leaves running is NOT protected,
# and the next run may replace it (see the note printed on success).
source "$repo_dir/scripts/lib/dev-lock.sh"
lock_path="$(omt_lock_dir "$repo_dir")"
if [ -z "$lock_path" ]; then
    echo "restart FAILED: cannot locate the development lock (is this a git repository?)"
    exit 1
fi
omt_lock_acquire "$lock_path" "$lock_wait_seconds"
lock_status=$?
if [ "$lock_status" -eq 75 ]; then
    echo "restart FAILED: another worktree is mid-transaction and did not release within ${lock_wait_seconds}s"
    echo "hint: wait for it, or raise the wait with --lock-wait=<seconds>"
    exit 75
fi
if [ "$lock_status" -ne 0 ]; then
    echo "restart FAILED: cannot take the development lock at $lock_path"
    exit 1
fi
# The claim is ours from here, so this is the one place that must release it however the script ends:
# even a failure before any cleanup state exists below.
trap 'omt_lock_release' EXIT

# --- run lifecycle ----------------------------------------------------------------
#
# What this run owns and what it concluded. Cleanup is decided from these values and nowhere else,
# so a failure, an interruption and a normal finish all release exactly what this run created --
# never an instance a later run has taken over.
#
# `owned_job` is set the moment the launchd job is submitted; `owned_pid` when the process is found.
# The gap between them is real: the app can be running before the first poll sees it, so cleanup
# must be able to find the instance it started even when no pid was ever recorded.
owned_pid=""
owned_job=0
owned_launch_attempted=0
app_pid_start=""
# Whether a successful finish should announce the stop. Only --check stops what it started.
announce_stop=0
run_failed=0
verified=1

# The pid this run started, when the poll had not recorded one before an interruption.
#
# Booted out of launchd is not stopped: the job removal stops the job, not the process it started.
# The instance is found the same way the launcher finds it -- by the executable of this build -- so
# cleanup still ends it, and the search never reaches another worktree's binary.
#
# This is called more than once on purpose. A job can start the app between one lookup and the
# bootout that follows it, so a single lookup before bootout proves nothing: the lookup after the
# job is gone is the one that sees a late instance.
adopt_started_instance() {
    if [ -n "$owned_pid" ]; then
        return 0
    fi
    [ "$owned_launch_attempted" -eq 1 ] || return 0
    local candidate
    candidate="$(find_dev_app_pid)"
    [ -n "$candidate" ] || return 0
    owned_pid="$candidate"
    app_pid_start="$(ps -p "$candidate" -o lstart= 2>/dev/null)"
}

# Ask the app to quit the way a user does, then stop it only if that did not work.
#
# Returns 0 when the instance is gone after an orderly exit, 1 when the stop had to escalate.
# The quit goes through `request_app_quit` rather than a call site so the offline selftest can
# replace it: a hard-coded /usr/bin/osascript reaches the host's AppleEvent path even under a
# function of the same name, which is exactly what made an earlier selftest claim an isolation
# it did not have.
request_app_quit() {
    /usr/bin/osascript -e "tell application id \"$dev_bundle_id\" to quit" >/dev/null 2>&1 || true
}

quit_then_stop() {
    local pid="$1"
    kill -0 "$pid" 2>/dev/null || return 0
    request_app_quit
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.2
    done
    if kill -0 "$pid" 2>/dev/null; then
        # The graceful path did not take. Stop it anyway -- leaving it running would keep a
        # development switch alive on the user's machine -- but report the escalation.
        stop_pid_by_identity "$pid" "$dev_app_binary" >/dev/null 2>&1 || true
        sleep 0.2
        kill -0 "$pid" 2>/dev/null || return 1
        return 1
    fi
    return 0
}
# End the run: release what this run owns, then the claim, then exit with the original status.
#
# The cleanup order matters. A launchd job left loaded respawns the app the moment this script exits,
# so it is booted out first. The app is then asked to quit the way a user quits it, because the app
# runs its orderly cleanup (clipboard history, pointer settings) from its termination path; a signal
# skips that path, so it is the escalation, not the method. A forced stop is reported as an abnormal
# cleanup and turns a success into a failure -- the run must not claim a clean stop it did not get.
finish_run() {
    local status="$1"
    local forced=0

    # Booted out FIRST, and whenever a submit was attempted -- not only when one was confirmed: an
    # interrupted submit may have created the job without this run ever learning so. Removing the
    # job before looking for the process is what closes the late-start window: the job is the thing
    # that can start a new instance, so it goes away before the last lookup.
    if [ "$owned_job" -eq 1 ] || [ "$owned_launch_attempted" -eq 1 ]; then
        launchctl bootout "$launch_target" >/dev/null 2>&1 || true
        wait_for_job_gone || true
    fi

    # Now discover what to stop. A job that started the app between an earlier lookup and the bootout
    # left a process this run never recorded, so keep looking for a moment rather than once: the
    # process may still be starting when the job disappears.
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        adopt_started_instance
        [ -n "$owned_pid" ] && break
        sleep 0.2
    done

    if [ -n "$owned_pid" ]; then
        quit_then_stop "$owned_pid" || forced=1
    fi

    if [ "$owned_job" -eq 1 ] && launchctl print "$launch_target" >/dev/null 2>&1; then
        echo "cleanup FAILED: launchd job $launch_label is still loaded"
        status=1
    fi

    # The residue question is asked of the machine, not of the pid this run happened to save: a
    # process it never recorded still has to be gone before the run may say it cleaned up.
    if [ "$owned_launch_attempted" -eq 1 ]; then
        # Give an already-signalled process a moment to leave before calling it residue.
        for _ in 1 2 3 4 5 6 7 8; do
            [ -n "$(find_all_dev_app_pids)" ] || break
            sleep 0.25
        done
        residue="$(find_all_dev_app_pids)"
        if [ -n "$residue" ]; then
            # A process that appeared only now was never recorded above; it is still this run's to
            # stop. It goes through the SAME orderly path: quitting first is what lets the app run
            # its own cleanup, and a signal is the escalation, never the method. Whatever this
            # path has to escalate is recorded in `forced`, so the run cannot report a clean stop it
            # did not achieve -- the verdict below is decided after every stop has happened.
            for pid in $residue; do
                quit_then_stop "$pid" || forced=1
            done
            residue="$(find_all_dev_app_pids)"
        fi
        if [ -n "$residue" ]; then
            echo "cleanup FAILED: pid $residue of this build is still running"
            status=1
        fi
    fi

    # Decided last, after every stop attempt: a stop that had to escalate is an abnormal cleanup.
    if [ "$forced" -eq 1 ] && [ "$status" -eq 0 ]; then
        echo "check FAILED: an instance was force-stopped, so its orderly exit cleanup did not run"
        status=1
    fi

    # The stop is announced only here, after every check above passed: a message printed before the
    # cleanup ran could describe a state this run never reached.
    if [ "$status" -eq 0 ] && [ "$check_mode" -eq 1 ] && [ "$announce_stop" -eq 1 ]; then
        echo "instance: stopped (--check leaves neither this run's process nor its launchd job)"
    fi

    omt_lock_release
    exit "$status"
}

# Installed only now that finish_run exists: a trap that names a function defined later would fail
# when it fires (`bash: finish_run: command not found`) and let the run continue building and
# launching -- worse than no trap at all. The claim itself is released by the EXIT trap installed
# when it was taken, which needs no state.
trap 'finish_run 130' INT
trap 'finish_run 143' TERM


# 移除脚本上一次提交的用户级 launchd 任务,否则 launchd 会在停掉实例后自动拉起旧实例。
# Remove the user-level launchd job submitted by the previous run; otherwise launchd
# would automatically respawn the old instance after it is stopped.
launch_label="oh-my-tab-dev"
launch_domain="gui/$(id -u)"
launch_target="$launch_domain/$launch_label"

# bootout can return before the submitted service disappears from the domain. Wait for the
# label to be gone before reusing it, otherwise the following submit may collide with the old job.
# bootout 返回后任务可能还短暂存在于域中。复用标签前等待旧任务消失,避免 submit 发生冲突。
wait_for_job_gone() {
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        if ! launchctl print "$launch_target" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

bootout_error="$(launchctl bootout "$launch_target" 2>&1)"
bootout_status=$?
if [ "$bootout_status" -ne 0 ] && launchctl print "$launch_target" >/dev/null 2>&1; then
    echo "restart FAILED: could not remove existing launchd job"
    [ -n "$bootout_error" ] && echo "$bootout_error"
    exit 1
fi
if ! wait_for_job_gone; then
    echo "restart FAILED: old launchd job did not exit"
    exit 1
fi

# 停实例:先枚举,再逐个核对可执行路径与 bundle 身份,确认后才 TERM,顽固的才 KILL。
# Stop instances by identity, never by name: enumerate, verify each candidate's executable path
# and the containing bundle's identifier, terminate that PID individually, and force-kill only a
# verified PID that refuses to exit. A broad `pkill -x` would also reach a copy this run cannot
# account for, and a stopped PID nobody named is a silent change to the user's machine.
#
# The dev instance is stopped without asking: it is what this script replaces. A packaged install
# under /Applications registers the same global shortcut, and with two running the older one
# hijacks Command+Tab (which once made new features look dead), so it is stopped too -- but its
# PIDs are named in the output, because that is the user's own app.
# 本次构建的应用进程:按可执行路径匹配,只返回第一个。
# The process running the binary this script assembled. Matching the exact executable path keeps the
# search inside this worktree's build, so it can never name another worktree's instance.
find_dev_app_pid() {
    /usr/bin/pgrep -f "$dev_app_binary" 2>/dev/null | awk 'NR == 1 { print; exit }'
}

# Every process running this build. The residue check asks whether ANY survived, so it cannot use
# the first-match helper above: leftover instances are exactly the case where one pid is not the
# whole answer.
find_all_dev_app_pids() {
    /usr/bin/pgrep -f "$dev_app_binary" 2>/dev/null | tr '\n' ' ' | sed 's/ $//'
}

stop_pid_by_identity() {
    local pid="$1"
    local expected_binary="$2"
    local actual
    actual="$(ps -p "$pid" -o comm= 2>/dev/null)"
    [ "$actual" = "$expected_binary" ] || return 1
    kill -TERM "$pid" 2>/dev/null || return 1
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.2
    done
    if kill -0 "$pid" 2>/dev/null; then
        kill -KILL "$pid" 2>/dev/null || return 1
        sleep 0.2
    fi
    return 0
}

# Classify a candidate before touching it. Its executable path alone is not proof of ownership: the
# same name is what two different bundles share. The containing .app bundle is what decides -- the
# dev bundle this script replaces, a packaged install that shares the global shortcut, or something
# this run cannot account for, which is reported and stops the launch instead of being killed.
bundle_identifier_of() {
    local exe="$1"
    local app=""
    case "$exe" in
        *.app/Contents/MacOS/*) app="${exe%%/Contents/MacOS/*}" ;;
    esac
    [ -n "$app" ] && [ -f "$app/Contents/Info.plist" ] || return 1
    /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist" 2>/dev/null
}

# The unbundled install (scripts/install-unbundled.sh) runs this same binary as a plain executable
# with no containing .app, so it has no bundle id for the loop below to classify it by, and it would
# otherwise land in "an instance this run cannot account for" and stop every launch. It is identified
# by the executable its launchd agent records instead, and parked in its own list: this script may
# stop it like the packaged install, but it must not leave it restarted behind the dev build.
unbundled_label="com.eacryo.oh-my-tab.unbundled"
unbundled_domain="gui/$(id -u)"
unbundled_plist="$HOME/Library/LaunchAgents/$unbundled_label.plist"
unbundled_bin=""
if [ -f "$unbundled_plist" ]; then
    # PlistBuddy prints "Error Reading File: ..." on stdout when the plist is unreadable, so the exit
    # status decides; otherwise that message would become the path this run compares against.
    if ! unbundled_bin="$(/usr/libexec/PlistBuddy -c 'Print :ProgramArguments:0' "$unbundled_plist" 2>/dev/null)"; then
        unbundled_bin=""
    fi
fi

stopped_dev_pids=()
stopped_dev_owners=()
stopped_packaged_pids=()
parked_unbundled_pids=()
unknown_instances=()
while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    candidate_exe="$(ps -p "$candidate" -o comm= 2>/dev/null)"
    [ -n "$candidate_exe" ] || continue
    # A bare binary under this repository's own build directories is this worktree's build.
    case "$candidate_exe" in
        "$dev_app_binary")
            # This worktree's own instance: reported through the same list as every other dev stop,
            # so the most common case -- restarting back to back -- names the pid it replaced.
            if stop_pid_by_identity "$candidate" "$dev_app_binary"; then
                stopped_dev_pids+=("$candidate")
                stopped_dev_owners+=("pid $candidate from this worktree ($repo_dir)")
            else
                unknown_instances+=("pid $candidate $candidate_exe (could not be stopped)")
            fi
            continue
            ;;
        *) ;;
    esac
    # The unbundled install's own binary, checked before the bundle-id cases below so it is not
    # reported as an instance this run cannot account for.
    if [ -n "$unbundled_bin" ] && [ "$candidate_exe" = "$unbundled_bin" ]; then
        parked_unbundled_pids+=("$candidate")
        continue
    fi
    candidate_bundle="$(bundle_identifier_of "$candidate_exe" || true)"
    case "$candidate_bundle" in
        "$dev_bundle_id")
            # Another dev build: the same bundle id by design, so it is what this run replaces. The
            # owning worktree is read from the verified executable path, so the user can tell which
            # checkout the instance that just disappeared belonged to.
            owner="${candidate_exe%%/dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab}"
            if [ "$owner" = "$candidate_exe" ]; then
                owner="an unrecognised location"
            fi
            if stop_pid_by_identity "$candidate" "$candidate_exe"; then
                stopped_dev_pids+=("$candidate")
                stopped_dev_owners+=("pid $candidate from $owner")
            else
                unknown_instances+=("pid $candidate $candidate_exe (could not be stopped)")
            fi
            ;;
        com.eacryo.oh-my-tab)
            if stop_pid_by_identity "$candidate" "$candidate_exe"; then
                stopped_packaged_pids+=("$candidate")
            else
                unknown_instances+=("pid $candidate $candidate_exe (could not be stopped)")
            fi
            ;;
        *)
            unknown_instances+=("pid $candidate $candidate_exe (bundle: ${candidate_bundle:-none})")
            ;;
    esac
done < <(pgrep -x 'oh-my-tab' 2>/dev/null || true)

# The unbundled install is unloaded before the unknown-instance check below, because that check is
# what decides whether this run may continue: an unbundled instance that will not go away has to
# stop the launch like any other instance this run cannot clear.
if [ ${#parked_unbundled_pids[@]} -gt 0 ]; then
    # SIGTERM is not a successful exit for this app, and the agent holds it up with
    # KeepAlive.SuccessfulExit=false, so killing the process alone only earns a fresh copy a moment
    # later -- one that would grab the instance lock while the dev build is being verified. Unloading
    # the agent first is the only way to keep it down.
    launchctl bootout "$unbundled_domain/$unbundled_label" 2>/dev/null || true
    for _ in $(seq 1 50); do
        launchctl print "$unbundled_domain/$unbundled_label" >/dev/null 2>&1 || break
        sleep 0.1
    done
    if launchctl print "$unbundled_domain/$unbundled_label" >/dev/null 2>&1; then
        echo "restart FAILED: could not unload the unbundled install's agent ($unbundled_label)"
        echo "hint: run: scripts/install-unbundled.sh stop"
        exit 1
    fi
    # bootout normally ends the process itself, so a pid that is already gone counts as stopped --
    # reporting it as a failure would skip the success message on the ordinary path.
    unbundled_stopped_pids=()
    for candidate in "${parked_unbundled_pids[@]}"; do
        if kill -0 "$candidate" 2>/dev/null; then
            if stop_pid_by_identity "$candidate" "$unbundled_bin"; then
                unbundled_stopped_pids+=("$candidate")
            else
                unknown_instances+=("pid $candidate $unbundled_bin (could not be stopped)")
            fi
        else
            unbundled_stopped_pids+=("$candidate")
        fi
    done
    if [ ${#unbundled_stopped_pids[@]} -gt 0 ]; then
        echo "stopped a running unbundled install (pid ${unbundled_stopped_pids[*]}) -- it shares the global shortcut"
        echo "  unloaded its agent ($unbundled_label); start it again with: scripts/install-unbundled.sh start"
    fi
fi

if [ ${#unknown_instances[@]} -gt 0 ]; then
    # Refusing beats killing: an instance this script cannot identify is not this run's to stop,
    # and two instances would fight over the global shortcut, so the launch stops here.
    echo "restart FAILED: an oh-my-tab instance this run does not own is running:"
    for entry in "${unknown_instances[@]}"; do
        echo "  $entry"
    done
    echo "hint: quit it yourself, then run this script again"
    exit 1
fi

if [ ${#stopped_dev_pids[@]} -gt 0 ]; then
    echo "stopped the previous dev instance:"
    for entry in "${stopped_dev_owners[@]}"; do
        echo "  $entry"
    done
fi
if [ ${#stopped_packaged_pids[@]} -gt 0 ]; then
    echo "stopped a running packaged install (pid ${stopped_packaged_pids[*]}) -- it shares the global shortcut"
fi
sleep 0.3

# 每次都删除旧的开发版 .app,避免旧资源或旧 Info.plist 混入新包。
# 若现有包是 release-dev.sh 产出的发布渠道包,先提示会被本地开发包覆盖(两者共用同一
# dist/Oh-My-Tab-Dev.app 路径)。
# Remove the previous dev .app every time so stale resources or Info.plist data cannot leak
# into the new bundle. The production dist/Oh-My-Tab.app is never touched. When the existing
# bundle came from release-dev.sh's channel package, warn first that this local dev build
# replaces it (both share dist/Oh-My-Tab-Dev.app).
dev_profile_marker="$dev_app/Contents/Resources/dev-build-profile.txt"
if [ -s "$dev_profile_marker" ] && [ "$(cat "$dev_profile_marker")" = "release-dev" ]; then
    echo "note: replacing a release-dev.sh package at $dev_app with a local $build_profile build"
fi

# cargo check/clippy/test 不产出主二进制,必须显式 build,否则启动的是旧版。
# cargo check/clippy/test do not produce the main binary; build explicitly or the OLD
# binary would start.
if ! cargo build --manifest-path "$repo_dir/Cargo.toml" "${cargo_profile_args[@]}" 2>&1; then
    echo "restart FAILED: cargo build error (see output above)"
    exit 1
fi

# Optional local Sparkle framework. Keeping this optional lets the app run from a clean checkout;
# placing Sparkle.framework under vendor/ enables real update checks in the dev bundle.
sparkle_framework_path="${SPARKLE_FRAMEWORK_PATH:-$repo_dir/vendor/Sparkle.framework}"
# Keep the feed configurable for local testing without committing a machine-specific appcast URL
# or public key. Sparkle's public key is optional here because the framework itself is optional.
sparkle_feed_url="${SPARKLE_FEED_URL:-https://download.oh-my-tab.app/dev_release/appcast.xml}"

# 签名身份:优先 Apple 签发的 Developer ID(与发布渠道同一个身份)。判定按"创建钥匙串项的
# 那个应用的签名身份"做——实测由 Developer ID 构建创建的项跨重建仍被承认(静默),而自签名/
# ad-hoc 构建创建的项一换二进制就不再被承认(重编即弹)。TCC 授权同理按身份判定。因此
# Developer ID 是默认,自签名只作回退。
# CODESIGN_IDENTITY 是显式命名的打包输入,可覆盖自动发现。
# Signing identity: prefer the Apple-issued Developer ID (the same identity the release channel
# uses). Its designated requirement -- anchored to Apple plus that certificate -- is stable across
# rebuilds, and the keychain ACL *and* the TCC grants are judged against it, so one authorization
# survives every later code change. A self-signed certificate has a stable DR *string*, but the
# keychain ACL is measured to judge it by the binary, so it stays a fallback only.
sign_identity="${CODESIGN_IDENTITY:-}"
if [ -n "$sign_identity" ] && [ "$sign_identity" = "-" ]; then
    echo "restart FAILED: CODESIGN_IDENTITY=- asks for ad-hoc signing, which is not accepted for"
    echo "               development builds: it has no stable identity, so every rebuild loses the TCC"
    echo "               grants and stops matching keychain items it created."
    echo "hint: use an Apple-issued identity, or the self-signed 'oh-my-tab-sign' certificate."
    exit 2
fi
if [ -z "$sign_identity" ]; then
    # `security find-identity` prints: <hash> "Developer ID Application: Name (TEAM)"
    sign_identity="$(/usr/bin/security find-identity -v -p codesigning 2>/dev/null \
        | /usr/bin/awk -F'"' '/"Developer ID Application:/ { print $2; exit }')"
fi


# 输入指纹:构建产物 + 所有会拷进包的内容。指纹相同就整个复用现有包,不重签。
# 每次重签都会换掉代码签名,而钥匙串项的 ACL 按"创建该项的应用的身份"判定:由 ad-hoc 构建
# 创建的项,一重签下次启动就又要输一次钥匙串密码(2026-10-08 实测)。所以输入没变就不重打包。
# Inputs fingerprint: the built binary plus everything copied into the bundle. An identical
# fingerprint reuses the existing bundle, signature included. A re-sign replaces the code signature,
# and a keychain item's ACL is judged against the identity of the app that CREATED it: an item
# created by an ad-hoc build asked for the keychain password again after every re-sign (measured
# 2026-10-08). So skip the whole assembly when nothing that goes into the bundle changed.
dev_inputs_fingerprint() {
    {
        shasum -a 256 "$build_target_dir/oh-my-tab" | awk '{ print $1 }'
        # The extension is a second binary and a second bundle; both decide what gets signed.
        shasum -a 256 "$build_target_dir/oh-my-tab-finder-ext" | awk '{ print $1 }'
        find "$repo_dir/assets/FinderExtension" -type f 2>/dev/null | sort | while IFS= read -r asset; do
            shasum -a 256 "$asset"
        done
        printf 'profile=%s\nversion=%s\nfeed=%s\nkey=%s\nidentity=%s\n' \
            "$build_profile" "$dev_version" "$sparkle_feed_url" "${SPARKLE_PUBLIC_ED_KEY:-}" "$sign_identity"
        find "$repo_dir/assets/Info.plist" "$repo_dir/assets/AppIcon.icns" \
            "$repo_dir/assets/Resources" "$repo_dir/assets/AppIcon.icon" \
            -type f 2>/dev/null | sort | while IFS= read -r asset; do
            shasum -a 256 "$asset"
        done
        if [ -d "$sparkle_framework_path" ]; then
            find "$sparkle_framework_path" -type f | sort | while IFS= read -r lib; do
                stat -f '%N %z %m' "$lib"
            done
        fi
    } | shasum -a 256 | awk '{ print $1 }'
}

# What the signature actually is, asked of the signed bundle rather than trusted from the request:
# Apple-issued identities carry a team identifier and an Apple authority line; the self-signed
# certificate and ad-hoc carry neither.
# Whether this run expects an Apple-issued identity on the bundle. The self-signed identity is what the
# docs accept, but only when it is the one *explicitly requested* (or when the fallback is allowed):
# treating every non-empty CODESIGN_IDENTITY as "must be Apple" made a deliberate
# `CODESIGN_IDENTITY=oh-my-tab-sign` rebuild on every start.
apple_identity_expected=0
if [ "$require_stable_signing" = "1" ]; then
    apple_identity_expected=1
elif [ "$allow_signing_fallback" != "1" ] && [ -n "$sign_identity" ] && [ "$sign_identity" != "oh-my-tab-sign" ]; then
    apple_identity_expected=1
fi

bundle_signature_is_adhoc() {
    /usr/bin/codesign -d --verbose=4 "$dev_app" 2>&1 | grep -q "Signature=adhoc"
}

bundle_has_apple_identity() {
    local signature
    signature="$(/usr/bin/codesign -d --verbose=4 "$dev_app" 2>&1)"
    printf '%s\n' "$signature" | grep -q "TeamIdentifier=not set" && return 1
    printf '%s\n' "$signature" | grep -qE '^Authority=(Apple Development|Developer ID Application):'
}

# 现有包能否复用:签名可验证、满足 --require-stable-signing 的要求、输入指纹一致。
# 签名验证不过的包不是缓存:它必须重建;--require-stable-signing 要求 Apple 签发的身份,
# 所以自签名(或更早留下的 ad-hoc)包不能从这个捷径溜过去。
# Whether the existing bundle may be reused: its signature verifies, it satisfies
# `--require-stable-signing`, and its inputs fingerprint matches. A bundle whose signature does not
# verify is not a cache -- it has to be rebuilt -- and the flag demands an Apple-issued identity, so
# a self-signed (or older ad-hoc) bundle must not slip through this shortcut.
bundle_reusable() {
    [ -d "$dev_app" ] && [ -x "$dev_app_binary" ] && [ -s "$dev_inputs_file" ] || return 1
    /usr/bin/codesign --verify --deep --strict "$dev_app" >/dev/null 2>&1 || return 1
    # Ad-hoc is not a development identity at all (no stable identity: every rebuild loses the TCC
    # grants and stops matching keychain items it created), so a bundle carrying it is never a cache.
    bundle_signature_is_adhoc && return 1
    # The fingerprint records the identity that was REQUESTED -- it has to be written before signing, to
    # be inside the seal -- so ask the bundle what it actually carries: a run whose policy is the
    # Apple-issued identity must not reuse a bundle signed with anything else. A bundle produced by an
    # explicit `--allow-signing-fallback` (or on a machine without an Apple identity) is accepted, so the
    # fallback does not rebuild on every start.
    if [ "$apple_identity_expected" = "1" ] && ! bundle_has_apple_identity; then
        return 1
    fi
    [ "$(cat "$dev_inputs_file")" = "$(dev_inputs_fingerprint)" ]
}

dev_inputs_file="$dev_app/Contents/Resources/dev-inputs.txt"
reuse_bundle=0
if bundle_reusable; then
    reuse_bundle=1
    echo "dev bundle unchanged; reusing its signature (keychain and TCC grants stay valid)"
fi

if [ "$reuse_bundle" -eq 0 ]; then
if [ -d "$dev_app" ]; then
    rm -rf "$dev_app"
fi

# 组装独立的开发版 .app,让 macOS 按 bundle 身份管理 Accessibility / Screen Recording 授权。
# Assemble a dedicated dev .app so macOS manages Accessibility / Screen Recording grants by
# the bundle identity.
mkdir -p "$dev_app/Contents/MacOS" "$dev_app/Contents/Resources"
cp "$build_target_dir/oh-my-tab" "$dev_app_binary"
cp "$repo_dir/assets/Info.plist" "$dev_app/Contents/Info.plist"
cp "$repo_dir/assets/AppIcon.icns" "$dev_app/Contents/Resources/AppIcon.icns"
# 声明本地化的 .lproj 锚点:没有它们,系统面板(NSSavePanel 等)的按钮在中文系统上
# 会显示英文(见 assets/Info.plist 的 CFBundleLocalizations 注释)。放在 codesign 前。
# Localized .lproj anchors: without them, system panels (NSSavePanel etc.) show English
# buttons on a Chinese system (see the CFBundleLocalizations note in assets/Info.plist).
# Copied before codesign so they are covered by the signature.
cp -R "$repo_dir/assets/Resources/." "$dev_app/Contents/Resources/"
# Finder Sync app extension (Contents/PlugIns/Oh-My-Tab-Finder.appex). Assembled unsigned: the
# extension is signed inside-out before the app (see sign_dev_appex below), and the app is signed
# without --deep so its seal records the extension by requirement rather than by cdhash.
mkdir -p "$dev_appex/Contents/MacOS"
cp "$build_target_dir/oh-my-tab-finder-ext" "$dev_appex_binary"
cp "$repo_dir/assets/FinderExtension/Info.plist" "$dev_appex/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $dev_appex_bundle_id" \
    "$dev_appex/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :OMTHostBundleIdentifier $dev_bundle_id" \
    "$dev_appex/Contents/Info.plist"
if [ -d "$repo_dir/assets/AppIcon.icon" ]; then
    cp -R "$repo_dir/assets/AppIcon.icon" "$dev_app/Contents/Resources/AppIcon.icon"
fi
# 记录本包由哪个 profile 产出,供下次 dev-restart 识别是否覆盖了 release-dev 产物。
# 必须在 codesign 之前写入,否则会被排除在签名之外。
# Record which profile produced this bundle so the next dev-restart can tell whether it is
# replacing a release-dev.sh artifact. Written before codesign so it is covered by the signature.
printf '%s\n' "$build_profile" > "$dev_app/Contents/Resources/dev-build-profile.txt"
if [ -d "$sparkle_framework_path" ]; then
    mkdir -p "$dev_app/Contents/Frameworks"
    cp -R "$sparkle_framework_path" "$dev_app/Contents/Frameworks/Sparkle.framework"
    echo "Sparkle: embedded $sparkle_framework_path"
else
    echo "warning: Sparkle.framework not found at $sparkle_framework_path; update checks will be unavailable"
fi
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $dev_bundle_id" \
    "$dev_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleName Oh-My-Tab Dev" \
    "$dev_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName Oh-My-Tab Dev" \
    "$dev_app/Contents/Info.plist"
# Keep the dev bundle's display version aligned with Cargo and give each build a fresh UTC
# timestamp build number. SPARKLE_BUILD_VERSION remains available for deterministic tests.
dev_build_version="${SPARKLE_BUILD_VERSION:-$(date -u +%Y%m%d%H%M%S)}"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $dev_version" \
    "$dev_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $dev_build_version" \
    "$dev_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $dev_version" \
    "$dev_appex/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $dev_build_version" \
    "$dev_appex/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :SUFeedURL $sparkle_feed_url" \
    "$dev_app/Contents/Info.plist"
if [ -n "${SPARKLE_PUBLIC_ED_KEY:-}" ]; then
    /usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $SPARKLE_PUBLIC_ED_KEY" \
        "$dev_app/Contents/Info.plist" 2>/dev/null \
        || /usr/libexec/PlistBuddy -c "Set :SUPublicEDKey $SPARKLE_PUBLIC_ED_KEY" \
            "$dev_app/Contents/Info.plist"
fi

# 记录本次输入指纹,供下次判断能否复用这个包;必须在 codesign 之前写入。
# Record this bundle's inputs fingerprint for the next run; written before codesign so the
# signature covers it.
dev_inputs_fingerprint > "$dev_app/Contents/Resources/dev-inputs.txt"

# 签名身份:优先 Apple 签发的 Developer ID(与发布渠道同一个身份),这样指定要求
# (designated requirement)是"锚定 Apple + 该证书",跨重建稳定——钥匙串 ACL 与 TCC 授权
# 都按它判定,所以授权一次之后改代码重编也不再弹框、也不再掉权限。自签名证书的 DR 虽然
# 字符串稳定,但实测钥匙串 ACL 对它是按二进制判定的(重编即失效),因此只作为回退。
# CODESIGN_IDENTITY 是显式命名的打包输入,可覆盖自动发现。
# Signing identity: prefer the Apple-issued Developer ID (the same identity the release channel
# uses). Its designated requirement -- anchored to Apple plus that certificate -- is stable across
# rebuilds, and the keychain ACL *and* the TCC grants are judged against it, so one authorization
# survives every later code change. A self-signed certificate has a stable DR *string* but the
# keychain ACL is measured to judge it by the binary, so it stays a fallback only.
# Sign the Finder extension. It must happen BEFORE the app that contains it: `codesign --deep`
# re-signs nested code (dropping the extension's sandbox entitlement, which Finder then refuses) and
# seals the nested bundle by cdhash, so re-signing the extension afterwards invalidates the parent
# ("nested code is modified or invalid"). Signing inside-out, with the app signed without --deep,
# satisfies both: the parent records the extension by requirement and the entitlement survives.
sign_dev_appex() {
    local identity="$1"
    [ -d "$dev_appex" ] || return 0
    if ! /usr/bin/codesign --force --options runtime --timestamp=none \
        --sign "$identity" \
        --identifier "$dev_appex_bundle_id" \
        --entitlements "$repo_dir/assets/FinderExtension/FinderExtension.entitlements" \
        "$dev_appex"; then
        echo "restart FAILED: signing the Finder extension failed"
        return 1
    fi
    # The entitlement is what makes Finder accept the extension at all; verify it is on the binary
    # instead of trusting the signing command's exit status.
    if ! /usr/bin/codesign -d --entitlements - "$dev_appex" 2>&1 | grep -q 'app-sandbox'; then
        echo "restart FAILED: the Finder extension is missing its sandbox entitlement"
        return 1
    fi
}

signed_with=""
sign_error=""
if [ -n "$sign_identity" ]; then
    # Say WHICH failure this is: an identity that is installed but cannot sign (a locked keychain, or a
    # denied key access) is a different problem from having no Apple identity at all, and falling back
    # changes the bundle's identity -- which invalidates the keychain item's grant and the TCC grants
    # that were recorded for the Apple identity.
    # The extension is signed first, inside-out. Its failure is one of the reasons to fall back to
    # the self-signed identity, so it must not exit here: the same fallback path below covers it.
    appex_signed=1
    sign_dev_appex "$sign_identity" || appex_signed=0
    if [ "$appex_signed" = "1" ] && sign_error="$(/usr/bin/codesign --force --timestamp=none \
        --sign "$sign_identity" \
        --identifier "$dev_bundle_id" \
        "$dev_app" 2>&1)"; then
        signed_with="$sign_identity"
    else
        echo "restart FAILED: signing with '$sign_identity' failed:"
        if [ -z "$sign_error" ]; then
            echo "    the Finder extension could not be signed with this identity"
        fi
        printf '%s\n' "$sign_error" | sed 's/^/    /'
        if [ "$allow_signing_fallback" != "1" ]; then
            # Deliberately not a warning: a fallback bundle has a DIFFERENT identity, so the keychain
            # items the Apple-signed build created stop being recognized and the Accessibility /
            # Screen Recording grants have to be given again. Losing that silently is worse than a
            # failed build -- unlock the login keychain and retry, or ask for the fallback on purpose.
            echo "hint: check that the login keychain is unlocked and the identity is installed"
            echo "hint: (security find-identity -v -p codesigning), then retry -- or pass"
            echo "hint: --allow-signing-fallback if a differently-identified build is what you want."
            exit 1
        fi
        echo "warning: --allow-signing-fallback: falling back to the self-signed identity, which has a"
        echo "warning: DIFFERENT identity than the Apple-signed build: keychain items it created are no"
        echo "warning: longer recognized, and the Accessibility / Screen Recording grants must be given"
        echo "warning: again."
        if [ "$require_stable_signing" = "1" ]; then
            echo "restart FAILED: --require-stable-signing demands the Apple-issued identity, and it"
            echo "               could not sign (see above)."
            exit 1
        fi
        if ! sign_dev_appex "oh-my-tab-sign"; then
            sign_error="the self-signed fallback could not sign the Finder extension"
            signed_with=""
        elif /usr/bin/codesign --force --timestamp=none \
            --sign "oh-my-tab-sign" \
            --identifier "$dev_bundle_id" \
            "$dev_app" 2>/dev/null; then
            signed_with="oh-my-tab-sign"
        else
            sign_error="the self-signed fallback also failed"
            signed_with=""
        fi
    fi
fi
if [ -z "$signed_with" ] && [ -z "$sign_error" ]; then
    if [ "$require_stable_signing" = "1" ]; then
        echo "restart FAILED: --require-stable-signing demands the Apple-issued identity, and none is installed"
        echo "hint: sign in with an Apple ID in Xcode (or set CODESIGN_IDENTITY to an Apple-issued identity)."
        exit 1
    fi
    echo "note: no Apple-issued signing identity is installed; using the self-signed 'oh-my-tab-sign'"
    echo "note: a self-signed build's keychain grant does NOT survive a rebuild - see docs/clipboard-encryption-plan.md fact 8"
    if sign_dev_appex "oh-my-tab-sign" && /usr/bin/codesign --force \
        --sign "oh-my-tab-sign" \
        --identifier "$dev_bundle_id" \
        "$dev_app" 2>/dev/null; then
        signed_with="oh-my-tab-sign"
    else
        echo "restart FAILED: the self-signed identity could not sign the extension and the app"
        exit 1
    fi
fi
if [ -z "$signed_with" ]; then
    # Ad-hoc is deliberately NOT a development identity any more: it has no stable identity at all, so
    # every rebuild loses the TCC grants and (with an item it created) the keychain grant too. The
    # accepted identities are the Apple-issued one and the self-signed one.
    echo "restart FAILED: no usable code signing identity"
    echo "hint: sign in with an Apple ID in Xcode (an Apple-issued identity is preferred), or create a"
    echo "hint: self-signed Code Signing certificate named oh-my-tab-sign in Keychain Access."
    echo "hint: ad-hoc signing is not accepted for development builds."
    exit 1
fi
# Verify what the bundle actually carries, so an override value (CODESIGN_IDENTITY=oh-my-tab-sign, or a
# '-' sneaking through any path) cannot quietly produce a build the project does not accept.
if bundle_signature_is_adhoc; then
    echo "restart FAILED: the signed bundle is ad-hoc, which is not accepted for development builds"
    echo "hint: use an Apple-issued identity, or the self-signed 'oh-my-tab-sign' certificate."
    exit 2
fi
if [ "$require_stable_signing" = "1" ] && ! bundle_has_apple_identity; then
    echo "restart FAILED: --require-stable-signing demands an Apple-issued identity, but the bundle"
    echo "               is signed with '$signed_with'."
    echo "hint: sign in with an Apple ID in Xcode, or set CODESIGN_IDENTITY to an Apple-issued identity."
    exit 2
fi
echo "dev app signed with $signed_with"

# 指纹在签名前写入(必须在密封范围内),所以这里再验一次签名:验证不过的包不会被下次复用。
# The fingerprint is written before signing (it has to be inside the seal), so verify the
# signature here: a bundle that fails verification is never reused by the next run.
if ! /usr/bin/codesign --verify --deep --strict "$dev_app"; then
    echo "restart FAILED: dev app signature verification failed"
    exit 1
fi
fi


# 应用参数:`--` 之后的 argv 通过 open --args 原样交给应用(open 会把它后面的一切都
# 当作被启动应用的参数)。
# App arguments: the argv after `--` reaches the app through `open --args` (open treats
# everything after --args as arguments for the opened application).
# Finder Sync extension registration. Finder only offers a plugin it has discovered, so the
# assembled .appex is registered on every rebuild. --reload-finder-extension restarts Finder (which
# drops the cached extension process); the default only reports the state.
register_finder_extension() {
    [ -d "$dev_appex" ] || return 0
    # Stop only processes running THIS bundle's executable: another worktree's build and the packaged
    # install share the extension name, and killing by name would reach them too.
    local pid exe
    for pid in $(/usr/bin/pgrep -f 'oh-my-tab-finder-ext' 2>/dev/null || true); do
        exe="$(/bin/ps -p "$pid" -o comm= 2>/dev/null || true)"
        if [ "$exe" = "$dev_appex_binary" ]; then
            /bin/kill "$pid" 2>/dev/null || true
            echo "stopped the previous Finder extension process (pid $pid)"
        fi
    done
    # LaunchServices has to know the containing app before PlugInKit will accept the nested bundle.
    /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
        -f "$dev_app" >/dev/null 2>&1 || true
    /usr/bin/pluginkit -a "$dev_appex" >/dev/null 2>&1 || true
    if ! /usr/bin/pluginkit -m -A -D -p com.apple.FinderSync -i "$dev_appex_bundle_id" 2>/dev/null \
        | /usr/bin/grep -q "$dev_appex_bundle_id"; then
        echo "warning: the Finder extension is not registered with PlugInKit"
    fi
    local state
    state="$(/usr/bin/pluginkit -m -p com.apple.FinderSync -i "$dev_appex_bundle_id" 2>/dev/null | head -1)"
    case "$state" in
        +*|\!*) echo "finder extension: enabled ($dev_appex_bundle_id)" ;;
        -*)
            echo "finder extension: DISABLED by the user -- enable it in System Settings > General >"
            echo "                 Login Items & Extensions > Finder Extensions"
            ;;
        *) echo "finder extension: not elected yet -- enable it in System Settings > General >"
           echo "                 Login Items & Extensions > Finder Extensions" ;;
    esac
    local other
    other="$(/usr/bin/pluginkit -m -p com.apple.FinderSync 2>/dev/null | /usr/bin/grep -v "$dev_appex_bundle_id" | /usr/bin/grep 'oh-my-tab' || true)"
    if [ -n "$other" ]; then
        echo "note: another oh-my-tab Finder extension is also registered:"
        printf '%s\n' "$other" | /usr/bin/sed 's/^/      /'
    fi
    if [ "$reload_finder_extension" = "1" ]; then
        /usr/bin/killall Finder 2>/dev/null || true
        echo "Finder restarted (--reload-finder-extension)"
    else
        echo "note: Finder caches the extension; pass --reload-finder-extension to restart Finder"
    fi
}

register_finder_extension

open_command=(/usr/bin/open -W "$dev_app")
if [ "${#app_args[@]}" -gt 0 ]; then
    open_command+=(--args "${app_args[@]}")
fi
# Keep LaunchServices' diagnostics separate from the app log. `open -W` is the process
# launchd waits on, so an alive wrapper alone is not proof that the app executable started.
# 保存 LaunchServices 的诊断,不要再丢到 /dev/null。`open -W` 是 launchd 等待的进程,
# 包装脚本存活本身不能证明真正的应用二进制已经启动。
launch_output_dir="$HOME/Library/Logs/oh-my-tab"
mkdir -p "$launch_output_dir"
launch_output="$launch_output_dir/dev-launchd-open.log"

# The log window this launch may be judged by: only bytes written after this point can describe it.
# Taken BEFORE the job is submitted -- a fast launch can write its only record within the first
# second, and a window opened afterwards would exclude exactly the line it is looking for.
# The inode and size together identify the window: a rotated log is a different file, and a
# truncated one is shorter than the offset, so neither can pass for "nothing written since".
dev_log_path="$HOME/Library/Logs/oh-my-tab/oh-my-tab.log"
dev_log_offset=0
dev_log_inode=""
if [ -f "$dev_log_path" ]; then
    dev_log_offset="$(/usr/bin/stat -f %z "$dev_log_path" 2>/dev/null || echo 0)"
    dev_log_inode="$(/usr/bin/stat -f %i "$dev_log_path" 2>/dev/null || echo "")"
fi

# Both flags are set BEFORE the submit call, and owned_job is withdrawn only when the submit
# provably created nothing. The order is the point: a submission can succeed and be interrupted
# before any line after it runs, and a cleanup that trusts a flag assigned afterwards would skip
# bootout and leave a job that later starts the app. Over-claiming costs one bootout of a label this
# run never created; under-claiming leaves a development instance running.
owned_launch_attempted=1
owned_job=1
submit_error="$(launchctl submit -l "$launch_label" -o "$launch_output" -e "$launch_output" -- \
    "$repo_dir/scripts/dev-launchd-wrapper.sh" --from-launchd "$launch_label" \
    "${open_command[@]}" 2>&1)"
submit_status=$?
if [ "$submit_status" -ne 0 ] && ! launchctl print "$launch_target" >/dev/null 2>&1; then
    # The submit failed and the label is absent, so nothing of this run exists to clean up.
    owned_job=0
    owned_launch_attempted=0
    echo "restart FAILED: launchctl submit error"
    [ -n "$submit_error" ] && echo "$submit_error"
    finish_run 1
fi
if [ "$submit_status" -ne 0 ]; then
    echo "warning: launchctl submit returned $submit_status, but the job is active; continuing"
    [ -n "$submit_error" ] && echo "$submit_error"
fi

# Read the launch identity record this run's process wrote, from this launch's window only.
#
# The record is written by the app itself, so it can only describe the process that wrote it.
# The checks below are deliberately separate rather than one grep: each one excludes a different
# way of being wrong -- a stale line, another process, another build -- and a check that passes for
# the wrong reason is worse than no check.
launch_record_pid=""; launch_record_exe=""; launch_record_version=""; launch_record_bundle=""
read_launch_record() {
    local pid="$1"
    local line
    # A rotated or truncated log cannot express "after this point", so the window is unusable and
    # the caller must report the launch as unverified rather than as verified.
    local size
    size="$(/usr/bin/stat -f %z "$dev_log_path" 2>/dev/null || echo 0)"
    # A rotated file is a different inode, and a short one is a truncated window: neither can
    # express "bytes written after this point", so the window is unusable and the caller reports
    # the launch as unverified rather than reading whatever history the new file happens to hold.
    if [ -n "$dev_log_inode" ]; then
        current_inode="$(/usr/bin/stat -f %i "$dev_log_path" 2>/dev/null || echo "")"
        [ "$current_inode" = "$dev_log_inode" ] || return 2
    fi
    if [ "$size" -lt "$dev_log_offset" ]; then
        return 2
    fi
    [ -r "$dev_log_path" ] || return 2
    line="$(tail -c +$((dev_log_offset + 1)) "$dev_log_path" 2>/dev/null \
        | grep -a "^.*\[launch\] pid=${pid} " | tail -1)"
    [ -n "$line" ] || return 1
    launch_record_pid="$pid"
    launch_record_exe="$(printf '%s\n' "$line" | sed -n 's/.* executable=\([^ ]*\) .*/\1/p')"
    launch_record_version="$(printf '%s\n' "$line" | sed -n 's/.* version=\([^ ]*\) .*/\1/p')"
    launch_record_bundle="$(printf '%s\n' "$line" | sed -n 's/.* bundle=\([^ ]*\).*/\1/p')"
    return 0
}

# Undo the field escaping exactly as src/logger.rs applies it. A hand-written sed pipeline gets the
# backslash count wrong, which makes a path containing a space stop matching the built binary, so
# the rules live beside the encoder and the running decoder is a small helper the codec test and
# the selftest both exercise.
unescape_field() {
    printf '%s' "$1" | python3 "$repo_dir/scripts/lib/decode-launch-field.py"
}

for _ in 1 2 3 4 5; do
    sleep 1
    app_pid="$(find_dev_app_pid)"
    if [ -n "$app_pid" ] && kill -0 "$app_pid" 2>/dev/null; then
        owned_pid="$app_pid"
        # Capture the process start time now. Identity is this value, not the path: a pid can be
        # reused, and the same path can later run a different build, so "the same pid is alive" and
        # "the path matches" each prove less than "this same process is still the one asked".
        app_pid_start="$(ps -p "$app_pid" -o lstart= 2>/dev/null)"
        wrapper_pid="$(launchctl print "$launch_target" 2>/dev/null \
        | awk '$1 == "pid" && $2 == "=" { print $3; exit }')"
        # 本脚本刚构建出的版本,用来和进程自报的版本比对。
        # The version this run just built, used only to compare against what the process reports.
        expected_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' \
            "$dev_app/Contents/Info.plist" 2>/dev/null)"
        # Give the app a moment to write its launch record; it is emitted during startup.
        record_status=1
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            if read_launch_record "$app_pid"; then
                record_status=0
                break
            elif [ "$?" -eq 2 ]; then
                record_status=2
                break
            fi
            sleep 0.5
        done
        case "$record_status" in
            0)
                # Identity, path and version must all agree, and the process must still be alive:
                # a launch record followed by an immediate exit is not a running build.
                record_exe_plain="$(unescape_field "$launch_record_exe")"
                launch_problem=""
                current_start="$(ps -p "$app_pid" -o lstart= 2>/dev/null)"
                if ! kill -0 "$app_pid" 2>/dev/null; then
                    launch_problem="the process exited after its launch record"
                elif [ -z "$current_start" ] || [ "$current_start" != "$app_pid_start" ]; then
                    launch_problem="pid $app_pid is no longer the process this run started"
                elif [ "$record_exe_plain" != "$dev_app_binary" ]; then
                    launch_problem="the record names $record_exe_plain, not $dev_app_binary"
                elif [ -z "$expected_version" ]; then
                    # Without the version this run built there is nothing to compare against, so the
                    # launch cannot be confirmed -- reported as unverified, never assumed to match.
                    launch_problem=""
                    verified=0
                elif [ "$launch_record_version" != "$expected_version" ]; then
                    launch_problem="the process reports version $launch_record_version, this build is $expected_version"
                fi
                if [ -n "$launch_problem" ]; then
                    echo "restart FAILED: the running process does not match this build: $launch_problem"
                    echo "-- newest app log: $dev_log_path"
                    run_failed=1
                    break
                fi
                if [ "$verified" -eq 1 ]; then
                    echo "restart ok (app pid $app_pid${wrapper_pid:+, wrapper pid $wrapper_pid})"
                    echo "build-version: ${launch_record_version:-unknown} (reported by the running process)"
                else
                    echo "restart ok (app pid $app_pid${wrapper_pid:+, wrapper pid $wrapper_pid})"
                    echo "build timestamp: unverified (this run could not read the version it built)"
                    echo "build-version: unknown (the comparison had no expected value)"
                fi
                ;;
            1)
                # No record in this window. That is "no evidence", not "the app failed to start": the
                # app may be logging to a path this script does not read (logging.file_path), and the
                # process being alive is what distinguishes that from a launch that died. A dead
                # process is a failure; a live one is unverified.
                if kill -0 "$app_pid" 2>/dev/null; then
                    verified=0
                    echo "restart ok (app pid $app_pid${wrapper_pid:+, wrapper pid $wrapper_pid})"
                    echo "build timestamp: unverified (no launch record in this log window)"
                    echo "build-version: unknown (the app may be writing to a configured log path)"
                else
                    echo "restart FAILED: pid $app_pid exited before writing a launch record"
                    echo "-- newest app log: $dev_log_path"
                    run_failed=1
                    break
                fi
                ;;
            *)
                # The log cannot express "after this point" (rotated, truncated, unreadable), so the
                # launch is unverified -- reported as unverified, never as verified.
                verified=0
                echo "restart ok (app pid $app_pid${wrapper_pid:+, wrapper pid $wrapper_pid})"
                echo "build timestamp: unverified (the app log window is unusable: $dev_log_path)"
                echo "build-version: unknown (read from the log, which could not be used)"
                ;;
        esac
        echo "build-profile: $build_profile"
        if [ "$check_mode" -eq 1 ]; then
            # --check owns this instance, so it reports what it actually established. Three outcomes,
            # decided once: the launch failed, it could not be verified, or it was verified.
            # The stop is claimed only after cleanup has actually succeeded, so the message can never
            # describe a state the run did not reach: finish_run prints its own failures and exits.
            if [ "$run_failed" -ne 0 ]; then
                finish_run 1
            fi
            if [ "$verified" -eq 0 ]; then
                # Cleanup still happens, but a verification that could not decide is not a pass: an
                # automated caller must never read "unverified" as "passed".
                echo "check FAILED: the launch could not be verified, so this run does not report success"
                finish_run 1
            fi
            announce_stop=1
            finish_run 0
        fi
        echo "instance: running, not protected -- the claim was released; the next run may replace it"
        # 把本次透传的 argv 回显出来,便于确认验证开关真的生效了。argv 是调用者自己写下的
        # 内容,回显它不涉及任何环境变量。
        # Echo this launch's forwarded argv so it is obvious the verification switches took effect.
        # The argv is what the caller typed; echoing it involves no environment variable.
        if [ "${#app_args[@]}" -gt 0 ]; then
            echo "app args: ${app_args[*]}"
        fi
        exit 0
    fi
done
echo "restart FAILED: app executable was not alive within 5s -- launch diagnostics:"
if [ -s "$launch_output" ]; then
    tail -40 "$launch_output"
else
    echo "(no LaunchServices output in $launch_output)"
fi
echo "-- newest app log:"
ls -t "$HOME/Library/Logs/oh-my-tab/oh-my-tab.log" \
    "$HOME/Library/Logs/oh-my-tab/oh-my-tab.log."* \
    "$HOME/Library/Logs/oh-my-tab/oh-my-tab-"*.log 2>/dev/null | head -1
finish_run 1
