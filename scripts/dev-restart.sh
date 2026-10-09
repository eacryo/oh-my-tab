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
# 开关只在本次启动生效:脚本每次先 pkill 旧实例,不会残留。
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
# Switches apply to this launch only: the script pkills old instances first, nothing sticks.
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
            echo "  -- ARGS    forward ARGS to the app executable, verbatim"
            echo "  --FLAG     every other --flag[=value] is forwarded to the app as well, e.g."
            echo "             scripts/dev-restart.sh --open-settings=about --force-onboarding"
            echo "  app switches in use: --open-settings[=<general|about|0..6>], --force-onboarding,"
            echo "             --onboarding=reset, --no-onboarding, --fake-permissions=ax:0,sr:0,"
            echo "             --pseudo-locale, --layout-debug, --test-update-notice[=available],"
            echo "             --smoke-update-prompts (opens 12 key windows and takes focus; run only with consent),"
            echo "             --show-clipboard (open the clipboard picker without its hotkey),"
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
if [ "$build_profile" = "dev-opt" ]; then
    cargo_profile_args=(--profile dev-opt)
fi
build_target_dir="$repo_dir/target/$build_profile"
dev_app="$repo_dir/dist/Oh-My-Tab-Dev.app"
dev_app_binary="$dev_app/Contents/MacOS/oh-my-tab"
dev_bundle_id="com.eacryo.oh-my-tab.dev"
dev_version="$(awk -F'"' '/^version/ {print $2; exit}' "$repo_dir/Cargo.toml")"
dev_release_doc="$repo_dir/release_doc_dev/${dev_version}.md"
if [ ! -s "$dev_release_doc" ]; then
    echo "restart FAILED: release notes not found for version $dev_version: $dev_release_doc"
    exit 1
fi

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

# 杀掉所有 oh-my-tab 实例:开发二进制 + 任意位置的打包 .app 都会注册同一个全局快捷键,
# 两个进程并存时旧版会抢走 Cmd+Tab(用户曾因此误以为新功能没生效)。
# 按完整进程名匹配,覆盖 /Applications、dist 和其它副本;无旧进程也不报错。
# Kill every oh-my-tab instance: both the dev binary and the packaged .app register the
# same global shortcut -- with two running, the older one hijacks Cmd+Tab (which once made
# new features look dead). Match the complete process name so copies under /Applications,
# dist, and other locations are all covered; no error when nothing is running.
pkill -x 'oh-my-tab' 2>/dev/null
sleep 0.5

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
signed_with=""
sign_error=""
if [ -n "$sign_identity" ]; then
    # Say WHICH failure this is: an identity that is installed but cannot sign (a locked keychain, or a
    # denied key access) is a different problem from having no Apple identity at all, and falling back
    # changes the bundle's identity -- which invalidates the keychain item's grant and the TCC grants
    # that were recorded for the Apple identity.
    if sign_error="$(/usr/bin/codesign --deep --force --timestamp=none \
        --sign "$sign_identity" \
        --identifier "$dev_bundle_id" \
        "$dev_app" 2>&1)"; then
        signed_with="$sign_identity"
    else
        echo "restart FAILED: signing with '$sign_identity' failed:"
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
        if /usr/bin/codesign --deep --force --timestamp=none \
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
    if /usr/bin/codesign --deep --force \
        --sign "oh-my-tab-sign" \
        --identifier "$dev_bundle_id" \
        "$dev_app" 2>/dev/null; then
        signed_with="oh-my-tab-sign"
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
submit_error="$(launchctl submit -l "$launch_label" -o "$launch_output" -e "$launch_output" -- \
    "$repo_dir/scripts/dev-launchd-wrapper.sh" --from-launchd "$launch_label" \
    "${open_command[@]}" 2>&1)"
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

# 轮询等待真实应用进程存活(最长 5 秒)。
# 不能只检查 wrapper PID:它可能仍在等待 `open -W`,即使应用本身已经退出。
# Poll for the real app process (up to 5 seconds). Checking only the wrapper PID is a
# false positive because it can remain blocked in `open -W` after the app exits.
find_dev_app_pid() {
    /usr/bin/pgrep -f "$dev_app_binary" 2>/dev/null | awk 'NR == 1 { print; exit }'
}

for _ in 1 2 3 4 5; do
    sleep 1
    app_pid="$(find_dev_app_pid)"
    if [ -n "$app_pid" ] && kill -0 "$app_pid" 2>/dev/null; then
        wrapper_pid="$(launchctl print "$launch_target" 2>/dev/null \
        | awk '$1 == "pid" && $2 == "=" { print $3; exit }')"
        # 读出本次构建写入的 CFBundleVersion(build-version),便于确认运行的是哪次构建。
        # Read the CFBundleVersion written by this build so it's clear which build is running.
        build_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' \
            "$dev_app/Contents/Info.plist" 2>/dev/null)"
        echo "restart ok (app pid $app_pid${wrapper_pid:+, wrapper pid $wrapper_pid})"
        echo "build-version: ${build_version:-unknown}"
        echo "build-profile: $build_profile"
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
exit 1
