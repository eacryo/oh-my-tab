#!/bin/bash
# Run the development app under launchd, then remove the submitted job when the
# app exits. launchctl submit infers keepalive, so this prevents a normal menu
# quit from being interpreted as a request to restart the app.

set -u

# This helper is an internal launchd entry point. Refuse direct invocation before touching
# launchd or attempting to execute an empty command.
# 此脚本只供 launchd 内部调用。直接运行时先退出,避免触碰 launchd 或执行空命令。
if [ "${OH_MY_TAB_LAUNCHD_WRAPPER:-}" != "1" ]; then
    echo "error: scripts/dev-launchd-wrapper.sh is an internal helper; run scripts/dev-restart.sh instead" >&2
    exit 64
fi

if [ "$#" -lt 2 ] || [ -z "${1:-}" ]; then
    echo "error: dev-launchd-wrapper.sh requires a launchd label and a command" >&2
    exit 64
fi

launch_label="$1"
shift

# Keep this check separate from the parameter guard so a malformed internal invocation cannot
# reach launchctl cleanup with an empty command.
# 单独检查命令参数,防止内部调用参数异常时仍进入 launchctl 清理流程。
if [ "$#" -eq 0 ]; then
    echo "error: dev-launchd-wrapper.sh received no command" >&2
    exit 64
fi

"$@"
exit_code=$?

launch_domain="gui/$(id -u)"
launchctl bootout "$launch_domain/$launch_label" >/dev/null 2>&1 || true
exit "$exit_code"
