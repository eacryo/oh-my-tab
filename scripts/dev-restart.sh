#!/bin/bash
# 开发重启脚本:优雅退出旧进程 → 编译并组装开发版 .app → 启动 .app → 校验存活。
# 由 agent 在 cargo fmt/check/clippy/test 全绿后执行(见 AGENTS.md 约定)。
# Dev restart script: gracefully quit the old process -> build and assemble the dev .app ->
# start the .app -> verify it is alive. Run by the agent after the
# fmt/check/clippy/test gates pass (see the AGENTS.md convention).

# Resolve paths from this script, not from the caller's current directory. This
# keeps both `./scripts/dev-restart.sh` and an absolute-path invocation working.
# 根据脚本自身位置定位项目根目录,不依赖调用者当前所在的目录。
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"
dev_app="$repo_dir/dist/Oh-My-Tab-Dev.app"
dev_app_binary="$dev_app/Contents/MacOS/oh-my-tab"
dev_bundle_id="com.eacryo.oh-my-tab.dev"

# 移除脚本上一次提交的用户级 launchd 任务,否则 launchd 会在 pkill 后自动拉起旧实例。
# Remove the user-level launchd job submitted by the previous run; otherwise launchd
# would automatically respawn the old instance after pkill.
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

# 杀掉所有 oh-my-tab 实例:开发二进制 + 打包安装的 .app 都会注册同一个全局快捷键,
# 两个进程并存时旧版会抢走 Cmd+Tab(用户曾因此误以为新功能没生效)。
# 精确匹配路径,避免误杀同名进程;无旧进程也不报错。
# Kill every oh-my-tab instance: both the dev binary and the packaged .app register the
# same global shortcut -- with two running, the older one hijacks Cmd+Tab (which once made
# new features look dead). Exact path matches only; no error when nothing is running.
pkill -f 'target/debug/oh-my-tab' 2>/dev/null
pkill -f 'target/release/oh-my-tab' 2>/dev/null
pkill -f "$dev_app_binary" 2>/dev/null
pkill -f '/Applications/Oh-My-Tab.app/Contents/MacOS/oh-my-tab' 2>/dev/null
sleep 0.5

# 每次都删除旧的开发版 .app,避免旧资源或旧 Info.plist 混入新包。
# Remove the previous dev .app every time so stale resources or Info.plist data cannot leak
# into the new bundle. The production dist/Oh-My-Tab.app is never touched.
if [ -d "$dev_app" ]; then
    rm -rf "$dev_app"
fi

# cargo check/clippy/test 不产出主二进制,必须显式 build,否则启动的是旧版。
# cargo check/clippy/test do not produce the main binary; build explicitly or the OLD
# binary would start.
if ! cargo build --manifest-path "$repo_dir/Cargo.toml" 2>&1; then
    echo "restart FAILED: cargo build error (see output above)"
    exit 1
fi

# 组装独立的开发版 .app,让 macOS 按 bundle 身份管理 Accessibility / Screen Recording 授权。
# Assemble a dedicated dev .app so macOS manages Accessibility / Screen Recording grants by
# the bundle identity.
mkdir -p "$dev_app/Contents/MacOS" "$dev_app/Contents/Resources"
cp "$repo_dir/target/debug/oh-my-tab" "$dev_app_binary"
cp "$repo_dir/assets/Info.plist" "$dev_app/Contents/Info.plist"
cp "$repo_dir/assets/AppIcon.icns" "$dev_app/Contents/Resources/AppIcon.icns"
if [ -d "$repo_dir/assets/AppIcon.icon" ]; then
    cp -R "$repo_dir/assets/AppIcon.icon" "$dev_app/Contents/Resources/AppIcon.icon"
fi
# Optional local Sparkle framework. Keeping this optional lets the app run from a clean checkout;
# placing Sparkle.framework under vendor/ enables real update checks in the dev bundle.
sparkle_framework_path="${SPARKLE_FRAMEWORK_PATH:-$repo_dir/vendor/Sparkle.framework}"
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
dev_version="$(awk -F'"' '/^version/ {print $2; exit}' "$repo_dir/Cargo.toml")"
dev_build_version="${SPARKLE_BUILD_VERSION:-$(date -u +%Y%m%d%H%M%S)}"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $dev_version" \
    "$dev_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $dev_build_version" \
    "$dev_app/Contents/Info.plist"
# Keep the feed configurable for local testing without committing a machine-specific appcast URL
# or public key. Sparkle's public key is optional here because the framework itself is optional.
sparkle_feed_url="${SPARKLE_FEED_URL:-https://download.oh-my-tab.app/dev_release/appcast.xml}"
/usr/libexec/PlistBuddy -c "Set :SUFeedURL $sparkle_feed_url" \
    "$dev_app/Contents/Info.plist"
if [ -n "${SPARKLE_PUBLIC_ED_KEY:-}" ]; then
    /usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $SPARKLE_PUBLIC_ED_KEY" \
        "$dev_app/Contents/Info.plist" 2>/dev/null \
        || /usr/libexec/PlistBuddy -c "Set :SUPublicEDKey $SPARKLE_PUBLIC_ED_KEY" \
            "$dev_app/Contents/Info.plist"
fi

# 优先使用固定签名身份,让 TCC 授权跨重建保持稳定;开发设备没有证书时允许回退 ad-hoc。
# Prefer a stable signing identity so TCC grants survive rebuilds; allow ad-hoc fallback on
# development machines that do not have the certificate.
sign_identity="oh-my-tab-sign"
require_stable_signing="${OH_MY_TAB_REQUIRE_STABLE_SIGNING:-0}"
if /usr/bin/codesign --deep --force \
    --sign "$sign_identity" \
    --identifier "$dev_bundle_id" \
    "$dev_app"; then
    echo "dev app signed with $sign_identity"
else
    if [ "$require_stable_signing" = "1" ]; then
        echo "restart FAILED: code signing identity '$sign_identity' is unavailable"
        echo "Create a self-signed Code Signing certificate with this exact name in Keychain Access."
        exit 1
    fi
    echo "warning: '$sign_identity' is unavailable; using ad-hoc signing (TCC grants may not survive rebuilds)"
    if ! /usr/bin/codesign --deep --force \
        --sign - \
        --identifier "$dev_bundle_id" \
        "$dev_app"; then
        echo "restart FAILED: ad-hoc code signing failed"
        exit 1
    fi
fi
if ! /usr/bin/codesign --verify --deep --strict "$dev_app"; then
    echo "restart FAILED: dev app signature verification failed"
    exit 1
fi

# 交给用户级 launchd 托管,脱离当前 Shell/执行器生命周期。
# Submit to the per-user launchd domain so the process survives the shell/executor lifetime.
launch_env_args=()
if [ -n "${OH_MY_TAB_LAYOUT_DEBUG:-}" ]; then
    launch_env_args+=("OH_MY_TAB_LAYOUT_DEBUG=$OH_MY_TAB_LAYOUT_DEBUG")
fi
if [ -n "${OH_MY_TAB_PSEUDO_LOCALE:-}" ]; then
    launch_env_args+=("OH_MY_TAB_PSEUDO_LOCALE=$OH_MY_TAB_PSEUDO_LOCALE")
fi
submit_error="$(launchctl submit -l "$launch_label" -o /dev/null -e /dev/null -- \
    /usr/bin/env OH_MY_TAB_LAUNCHD_WRAPPER=1 "${launch_env_args[@]}" \
    "$repo_dir/scripts/dev-launchd-wrapper.sh" "$launch_label" \
    /usr/bin/open -n -W "$dev_app" 2>&1)"
submit_status=$?
if [ "$submit_status" -ne 0 ] && ! launchctl print "$launch_target" >/dev/null 2>&1; then
    echo "restart FAILED: launchctl submit error"
    [ -n "$submit_error" ] && echo "$submit_error"
    exit 1
fi
if [ "$submit_status" -ne 0 ]; then
    echo "warning: launchctl submit returned $submit_status, but the job is active; continuing"
    [ -n "$submit_error" ] && echo "$submit_error"
fi

# 轮询等待进程存活(最长 5 秒)。
# 从 launchd 服务状态取得 PID,再用 kill -0 确认进程实际存活。
# Read the PID from launchd's service state, then use kill -0 to confirm it is alive.
for _ in 1 2 3 4 5; do
    sleep 1
    new_pid="$(launchctl print "$launch_target" 2>/dev/null \
        | awk '$1 == "pid" && $2 == "=" { print $3; exit }')"
    if [ -n "$new_pid" ] && kill -0 "$new_pid" 2>/dev/null; then
        # 读出本次构建写入的 CFBundleVersion(build-version),便于确认运行的是哪次构建。
        # Read the CFBundleVersion written by this build so it's clear which build is running.
        build_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' \
            "$dev_app/Contents/Info.plist" 2>/dev/null)"
        echo "restart ok (pid $new_pid)"
        echo "build-version: ${build_version:-unknown}"
        exit 0
    fi
done
echo "restart FAILED: process exited within 5s -- read the newest log:"
ls -t "$HOME/Library/Logs/oh-my-tab/oh-my-tab.log" \
    "$HOME/Library/Logs/oh-my-tab/oh-my-tab.log."* \
    "$HOME/Library/Logs/oh-my-tab/oh-my-tab-"*.log 2>/dev/null | head -1
exit 1
