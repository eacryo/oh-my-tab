#!/bin/bash
# 开发重启脚本:优雅退出旧进程 → 编译最新主二进制 → 后台启动新进程 → 校验存活。
# 由 agent 在 cargo fmt/check/clippy/test 全绿后执行(见 AGENTS.md 约定)。
# Dev restart script: gracefully quit the old process -> build the fresh binary ->
# start it in the background -> verify it is alive. Run by the agent after the
# fmt/check/clippy/test gates pass (see the AGENTS.md convention).

# Resolve paths from this script, not from the caller's current directory. This
# keeps both `./scripts/dev-restart.sh` and an absolute-path invocation working.
# 根据脚本自身位置定位项目根目录,不依赖调用者当前所在的目录。
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"

# 移除脚本上一次提交的用户级 launchd 任务,否则 launchd 会在 pkill 后自动拉起旧实例。
# Remove the user-level launchd job submitted by the previous run; otherwise launchd
# would automatically respawn the old instance after pkill.
launch_label="oh-my-tab-dev"
launch_domain="gui/$(id -u)"
launchctl bootout "$launch_domain/$launch_label" 2>/dev/null || true

# 杀掉所有 oh-my-tab 实例:开发二进制 + 打包安装的 .app 都会注册同一个全局快捷键,
# 两个进程并存时旧版会抢走 Cmd+Tab(用户曾因此误以为新功能没生效)。
# 精确匹配路径,避免误杀同名进程;无旧进程也不报错。
# Kill every oh-my-tab instance: both the dev binary and the packaged .app register the
# same global shortcut -- with two running, the older one hijacks Cmd+Tab (which once made
# new features look dead). Exact path matches only; no error when nothing is running.
pkill -f 'target/debug/oh-my-tab' 2>/dev/null
pkill -f 'target/release/oh-my-tab' 2>/dev/null
pkill -f '/Applications/Oh-My-Tab.app/Contents/MacOS/oh-my-tab' 2>/dev/null
sleep 0.5

# cargo check/clippy/test 不产出主二进制,必须显式 build,否则启动的是旧版。
# cargo check/clippy/test do not produce the main binary; build explicitly or the OLD
# binary would start.
if ! cargo build --manifest-path "$repo_dir/Cargo.toml" 2>&1; then
    echo "restart FAILED: cargo build error (see output above)"
    exit 1
fi

# 交给用户级 launchd 托管,脱离当前 Shell/执行器生命周期。
# Submit to the per-user launchd domain so the process survives the shell/executor lifetime.
if ! launchctl submit -l "$launch_label" -o /dev/null -e /dev/null -- \
    "$repo_dir/scripts/dev-launchd-wrapper.sh" "$launch_label" "$repo_dir/target/debug/oh-my-tab"; then
    echo "restart FAILED: launchctl submit error"
    exit 1
fi

# 轮询等待进程存活(最长 5 秒)。
# 从 launchd 服务状态取得 PID,再用 kill -0 确认进程实际存活。
# Read the PID from launchd's service state, then use kill -0 to confirm it is alive.
for _ in 1 2 3 4 5; do
    sleep 1
    new_pid="$(launchctl print "$launch_domain/$launch_label" 2>/dev/null \
        | awk '$1 == "pid" && $2 == "=" { print $3; exit }')"
    if [ -n "$new_pid" ] && kill -0 "$new_pid" 2>/dev/null; then
        echo "restart ok (pid $new_pid)"
        exit 0
    fi
done
echo "restart FAILED: process exited within 5s -- read the newest log:"
ls -t "$HOME/Library/Logs/oh-my-tab/"oh-my-tab-*.log 2>/dev/null | head -1
exit 1
