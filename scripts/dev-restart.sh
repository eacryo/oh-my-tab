#!/bin/bash
# 开发重启脚本:优雅退出旧进程 → 编译最新主二进制 → 后台启动新进程 → 校验存活。
# 由 agent 在 cargo fmt/check/clippy/test 全绿后执行(见 AGENTS.md 约定)。
# Dev restart script: gracefully quit the old process -> build the fresh binary ->
# start it in the background -> verify it is alive. Run by the agent after the
# fmt/check/clippy/test gates pass (see the AGENTS.md convention).

# 精确匹配裸二进制路径(cargo run 场景),避免误杀同名进程;无旧进程也不报错。
# Match the raw binary path exactly (cargo run workflow); no error when nothing is running.
pkill -f 'target/debug/oh-my-tab' 2>/dev/null
sleep 0.5

# cargo check/clippy/test 不产出主二进制,必须显式 build,否则启动的是旧版。
# cargo check/clippy/test do not produce the main binary; build explicitly or the OLD
# binary would start.
if ! cargo build 2>&1; then
    echo "restart FAILED: cargo build error (see output above)"
    exit 1
fi

# 后台启动,丢弃 stdout/stderr(应用自身日志走 ~/Library/Logs/oh-my-tab/,与终端无关)。
# Start in the background, discarding stdout/stderr (the app's own logs go to
# ~/Library/Logs/oh-my-tab/, unrelated to the terminal).
nohup ./target/debug/oh-my-tab >/dev/null 2>&1 &

# 轮询等待进程存活(最长 5 秒)。
# Poll for the process to stay alive (up to 5 seconds).
for _ in 1 2 3 4 5; do
    sleep 1
    if pgrep -f 'target/debug/oh-my-tab' >/dev/null; then
        echo "restart ok (pid $(pgrep -f 'target/debug/oh-my-tab' | head -1))"
        exit 0
    fi
done
echo "restart FAILED: process exited within 5s -- read the newest log:"
ls -t "$HOME/Library/Logs/oh-my-tab/"oh-my-tab-*.log 2>/dev/null | head -1
exit 1
