#!/bin/bash
# A2 层 E2E 场景:切换浮窗内的 Cmd+W / Cmd+M / Cmd+H 动作键。
#
# 判定者是**脚本**(退出码):输入由 Python + CoreGraphics 注入真实全局键盘事件,断言读 app 自己
# 写的 JSON 快照(--e2e-state=<path>)。分层与约定见 AGENTS.md「测试分层」。
#
# 本场景证明 A1 无法证明的那一段:**系统事件流 → key window 的 performKeyEquivalent:
# → 动作执行**。A1 的 --smoke-panel-keys 只直接驱动窗口对象,绕过了系统分发。
#
# 为什么不用 cua-driver 的 hotkey:按住不放需要"只按下不释放"的事件序列,驱动只提供
# press+release。这里与 tab-repeat.sh 一致,用 CGEventCreateKeyboardEvent 自己发。
#
# 本场景会注入真实全局热键(抢焦点),故用标记声明;run-all.sh 默认跳过它。
# E2E_STEALS_FOCUS=1
# E2E_REPLACES_INSTANCE=1
#
# 受控目标:注入真实热键后高亮项由 MRU 决定,不可控,因此本场景**只操作自己创建的窗口**:
# 两个由本脚本打开的临时文档(TextEdit)。若 TextEdit 已在运行则拒绝运行——Cmd+H 会隐藏整个
# app,可能连用户自己的文档一起隐藏掉。
#
# 前置:已授予辅助功能权限;TextEdit 未在运行。app 以 --panel-action-keys 启动开关开启面板动作
# (不改用户配置),退出时 trap 恢复默认实例。
#
# 用法:
#   scripts/e2e/panel-action-keys.sh [--hotkey=cmd+tab]
#
# Exit code: 0 = 全部断言通过;非 0 = 失败(逐条打印原因)。

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
state_file="/tmp/omt-e2e-panel-keys.json"
hotkey="cmd+tab"
close_file="/tmp/omt-e2e-target-close.txt"
min_file="/tmp/omt-e2e-target-minimize.txt"
hide_file="/tmp/omt-e2e-target-hide.txt"

for arg in "$@"; do
    case "$arg" in
        --hotkey=*) hotkey="${arg#--hotkey=}" ;;
        -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

fail() { echo "e2e panel-action-keys: FAIL: $*" >&2; exit 1; }
controlled_pid=""

# 触发键支持 Command / Option / Control 三种模式,与 app 的 keyboard.modifier 一致。
case "$hotkey" in
    cmd+tab|command+tab) message_flags=1048576; modifier_vk=55 ;;
    alt+tab|option+tab)   message_flags=524288;  modifier_vk=58 ;;
    ctrl+tab|control+tab) message_flags=262144;  modifier_vk=59 ;;
    *) fail "unsupported --hotkey=$hotkey (use cmd+tab, option+tab or ctrl+tab)" ;;
esac

rm -f "$state_file" "${state_file%.json}.tmp"

# TextEdit 必须独占:Cmd+H 隐藏的是整个 app。已在运行就拒绝,绝不碰用户的文档。
if pgrep -x TextEdit >/dev/null 2>&1; then
    fail "TextEdit is already running; quit it first (this scenario hides the whole app to test Cmd+H)"
fi

# cleanup 在**改动任何状态之前**装好:否则任一早退都会留下开着测试开关的实例或残留的临时文档。
cleanup() {
    local status=$?
    if ! "$repo_dir/scripts/dev-restart.sh" --no-onboarding >/dev/null 2>&1; then
        echo "e2e panel-action-keys: FAIL: could not restore a default instance; check the app by hand" >&2
        status=1
    fi
    # The app was hidden by the Cmd+H step, and a hidden app's scripted "close" does not reach the
    # documents; un-hide it first, then close, then quit. Without this the scenario leaves TextEdit
    # and the user's next launch inherits a stray process.
    osascript >/dev/null 2>&1 <<'APPLESCRIPT' || true
tell application "TextEdit"
    if it is running then
        activate
        try
            repeat with d in (every document whose name contains "omt-e2e-target")
                close d saving no
            end repeat
        end try
        quit
    end if
end tell
APPLESCRIPT
    # By the pid this scenario recorded, not by name: the scenario refuses to start when TextEdit is
    # already running, so the pid it captured IS the TextEdit it opened -- but naming the pid keeps the
    # cleanup honest if that ever changes (a name-based kill would reach an editor the user opened later).
    if [ -n "${controlled_pid:-}" ] && kill -0 "$controlled_pid" 2>/dev/null; then
        kill -TERM "$controlled_pid" 2>/dev/null || true
    fi
    rm -f "$close_file" "$min_file" "$hide_file" "$state_file" "${state_file%.json}.tmp"
    exit $status
}
trap cleanup EXIT

# 受控目标:两个干净的临时文档。**必须已保存**(open 打开磁盘文件),否则关闭未命名文档会弹出
# 保存对话框,窗口不关、卡片不减——那是测试目标的选取问题,不是被测行为。
printf 'oh-my-tab e2e target (close)\n' > "$close_file"
printf 'oh-my-tab e2e target (minimize)\n' > "$min_file"
printf 'oh-my-tab e2e target (hide)\n' > "$hide_file"
echo "e2e: opening two controlled target documents"
open -a TextEdit "$close_file" "$min_file" "$hide_file" || fail "could not open the controlled target documents"
sleep 3
controlled_pid="$(pgrep -x TextEdit | head -1)"
[ -n "$controlled_pid" ] || fail "could not resolve the controlled target's pid"
echo "e2e: controlled target pid=$controlled_pid"

# 启动开发版:开启面板动作开关(argv 透传,不改用户配置)。
echo "e2e: starting the dev build with the panel action keys on"
restart_out="$("$repo_dir/scripts/dev-restart.sh" --e2e-state="$state_file" --no-onboarding \
    -- --panel-action-keys=close,minimize,hide 2>&1)"
echo "$restart_out" | grep -q "^restart ok" || {
    echo "$restart_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up"
}
echo "$restart_out" | grep -E "^(restart ok|build-version|app args)" || true

app_pid="$(printf '%s\n' "$restart_out" \
    | sed -n 's/^restart ok (app pid \([0-9][0-9]*\).*/\1/p' | head -1)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"

sleep 1

python3 - "$state_file" "$app_pid" "$message_flags" "$modifier_vk" "$controlled_pid" <<'PY' || exit 1
import ctypes, json, sys, time

state_file, app_pid, message_flags, modifier_vk, controlled_pid = (
    sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5]),
)

cg = ctypes.CDLL('/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics')
cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
cg.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
cg.CGEventSetFlags.restype = None
cg.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
cg.CGEventSetIntegerValueField.restype = None
cg.CGEventSetIntegerValueField.argtypes = [ctypes.c_void_p, ctypes.c_int32, ctypes.c_int64]
cg.CGEventPost.restype = None
cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]

# 与 src/event_tap.rs 的 keyboard 模块一致:tap 读的正是这些字段。
FIELD_AUTOREPEAT = 8
FIELD_KEYCODE = 9
FLAG_COMMAND = 1048576
VK_TAB = 48
VK_W = 13
VK_M = 46
VK_H = 4
VK_ESC = 53
HID_EVENT_TAP = 0

def post(keycode, down, flags, autorepeat=0):
    event = cg.CGEventCreateKeyboardEvent(None, keycode, down)
    cg.CGEventSetFlags(event, flags)
    cg.CGEventSetIntegerValueField(event, FIELD_KEYCODE, keycode)
    cg.CGEventSetIntegerValueField(event, FIELD_AUTOREPEAT, autorepeat)
    cg.CGEventPost(HID_EVENT_TAP, event)

def release_all():
    """每个退出路径都释放:一个逻辑上仍按下的键会毒化后面的运行。"""
    for vk in (VK_W, VK_M, VK_H, VK_TAB, modifier_vk):
        post(vk, False, 0)

def state():
    try:
        with open(state_file) as handle:
            snapshot = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    if snapshot.get('pid') != app_pid:
        return None
    return snapshot

def cards(snapshot):
    # 快照里这个数组叫 cards;早期版本读 windows,恒为 0,导致所有计数断言形同虚设。
    return snapshot.get('cards') or []

def panel(snapshot):
    return snapshot.get('panel_actions') or {}

def wait_for(predicate, timeout, label):
    deadline = time.time() + timeout
    while time.time() < deadline:
        snapshot = state()
        if snapshot is not None and predicate(snapshot):
            return snapshot
        time.sleep(0.1)
    # 只打印断言真正读到的字段,绝不倾倒整份快照(约 140KB,会把失败原因埋掉)。
    current = state() or {}
    summary = {
        'seq': current.get('seq'),
        'visible': panel(current).get('visible'),
        'claims': panel(current).get('claims'),
        'cards': len(cards(current)),
    }
    raise AssertionError(f'timed out waiting for {label} (last: {summary})')

def selected_card(snapshot):
    key = snapshot.get('selected_key') or {}
    for card in cards(snapshot):
        if card.get('pid') == key.get('pid') and card.get('window_id') == key.get('window_id'):
            return card
    return None

def card_with_prefix(snapshot, title_prefix):
    for card in cards(snapshot):
        if str(card.get('title') or '').startswith(title_prefix):
            return card
    return None

def navigate_to(title_prefix, label):
    """把高亮移到受控目标的那张卡。选中项由 MRU 决定,不能假定一开始就在目标上。"""
    # 目标卡片要等后台刷新把它收进来(TextEdit 开窗后 AX 列表更新有延迟),所以先等它出现;
    # 早期版本在这里直接断言,偶发地把"还没刷新到"当成导航失败。
    deadline = time.time() + 8
    snapshot = state() or {}
    while card_with_prefix(snapshot, title_prefix) is None and time.time() < deadline:
        time.sleep(0.2)
        snapshot = state() or {}
    if card_with_prefix(snapshot, title_prefix) is None:
        titles = [c.get('title') for c in cards(snapshot)][:6]
        raise AssertionError(
            f'the {label} is not in the list at all (first titles: {titles})')
    for _ in range(len(cards(snapshot)) + 4):
        snapshot = state() or {}
        card = selected_card(snapshot)
        if card is not None and str(card.get('title') or '').startswith(title_prefix):
            return card
        post(VK_TAB, True, message_flags)
        post(VK_TAB, False, message_flags)
        time.sleep(0.25)
    raise AssertionError(f'could not navigate the highlight to {label}')

def controlled_hidden(snapshot):
    """受控 app 是否处于隐藏状态。

    不能只用"卡片消失":那在按下之前就可能成立(卡片本就不在列表里)。快照对每个应用会给出
    app_hidden_source,但对已隐藏且被过滤掉的卡片它不会出现,所以这里用两个信号合取:
      * 受控 pid 一张卡都不在;且
      * 该 pid 曾出现在列表过(由调用方在按下前已确认)。
    """
    return [c for c in cards(snapshot) if c.get('pid') == controlled_pid] == []

def controlled_only(card, what):
    """安全闸:只允许操作本脚本创建的窗口。"""
    if card.get('pid') != controlled_pid:
        raise SystemExit(
            f'e2e panel-action-keys: FAIL: the highlighted card is pid {card.get("pid")}, not the '
            f'controlled target {controlled_pid}; refusing to {what} a window this scenario did '
            'not create')

failures = []

try:
    start = wait_for(lambda s: panel(s).get('close') is True, 25,
                     'the panel action switches to be on')
    assert panel(start).get('claims') is False, 'claims must start false'

    # 召唤:按住触发修饰键 + Tab。合成事件在 tap 装好前会丢,故重试。
    visible = False
    for _ in range(6):
        post(modifier_vk, True, message_flags)
        post(VK_TAB, True, message_flags)
        try:
            wait_for(lambda s: panel(s).get('visible') is True, 1.5,
                     'the panel to become visible')
            visible = True
            break
        except AssertionError:
            release_all()
            time.sleep(0.3)
    assert visible, 'the panel never became visible; the tap never saw the synthesized combo'
    wait_for(lambda s: panel(s).get('claims') is True, 2, 'the panel to claim its chords')

    cards_before = len(cards(state() or {}))
    assert cards_before >= 2, 'need >= 2 cards to prove the action hit the highlighted one'

    # --- Cmd+W:关闭受控的 close 文档 ---
    close_card = navigate_to('omt-e2e-target-close', 'the close target')
    controlled_only(close_card, 'close')
    close_pid, close_wid = close_card['pid'], close_card['window_id']
    post(VK_W, True, message_flags | FLAG_COMMAND)
    post(VK_W, False, message_flags | FLAG_COMMAND)
    try:
        after_close = wait_for(
            lambda s: not any(c.get('pid') == close_pid and c.get('window_id') == close_wid
                              for c in cards(s)), 6,
            'cmd+w to close the HIGHLIGHTED controlled window')
        if panel(after_close).get('visible') is not True:
            failures.append('the panel closed after cmd+w; it must stay open')
    except AssertionError as exc:
        failures.append(str(exc))

    # --- Cmd+M:最小化受控的 minimize 文档 ---
    # show_minimized 关闭时(默认)正确的最小化会让卡片从列表消失;开启时会留下 minimized=true。
    # 两种策略都接受,但必须是目标那张卡发生了变化——"有任意卡片 minimized"是坏判据。
    min_card = navigate_to('omt-e2e-target-minimize', 'the minimize target')
    controlled_only(min_card, 'minimize')
    min_pid, min_wid = min_card['pid'], min_card['window_id']
    post(VK_M, True, message_flags | FLAG_COMMAND)
    post(VK_M, False, message_flags | FLAG_COMMAND)

    def minimize_happened(snapshot):
        for card in cards(snapshot):
            if card.get('pid') == min_pid and card.get('window_id') == min_wid:
                return bool(card.get('minimized'))
        return True  # 卡片消失 = 已最小化且 show_minimized 关闭

    try:
        after_min = wait_for(minimize_happened, 6,
                             'cmd+m to minimize the HIGHLIGHTED controlled window')
        if panel(after_min).get('visible') is not True:
            failures.append('the panel closed after cmd+m; it must stay open')
    except AssertionError as exc:
        failures.append(str(exc))

    # --- Cmd+H:隐藏受控 app(TextEdit 是脚本独占的) ---
    #
    # ⌘W 与 ⌘M 已让前两个受控文档离开列表,选中项此刻可能落在别的应用上,所以隐藏前必须**重新
    # 导航并核对目标**(早期版本直接发 ⌘H,可能隐藏用户的应用)。同时判据不能只靠"受控 pid 的
    # 卡片消失"——那在按下之前就可能成立;必须观察到该应用**从未隐藏变为隐藏**。
    hide_card = navigate_to('omt-e2e-target-hide', 'the hide target')
    controlled_only(hide_card, 'hide')
    hidden_before = bool(controlled_hidden(state() or {}))
    assert not hidden_before, 'the controlled app must be visible before cmd+h'
    post(VK_H, True, message_flags | FLAG_COMMAND)
    post(VK_H, False, message_flags | FLAG_COMMAND)
    try:
        after_hide = wait_for(lambda s: bool(controlled_hidden(s)), 6,
                              'cmd+h to hide the controlled app')
        if panel(after_hide).get('visible') is not True:
            failures.append('the panel closed after cmd+h; it must stay open')
    except AssertionError as exc:
        failures.append(str(exc))

    # --- 释放触发修饰键:本会话是 sticky 的,释放**不得**提交切换 ---
    commit_before = ((state() or {}).get('last_commit') or {}).get('count', 0)
    post(VK_TAB, False, 0)
    post(modifier_vk, False, 0)
    time.sleep(1.0)
    after_release = state() or {}
    if panel(after_release).get('visible') is not True:
        failures.append('releasing the trigger committed the switch; a sticky session must stay open')
    commit_after = (after_release.get('last_commit') or {}).get('count', 0)
    if commit_after != commit_before:
        failures.append(f'releasing the trigger committed (last_commit {commit_before} -> {commit_after})')

    # --- Esc:会话结束,claims 清空 ---
    post(VK_ESC, True, 0)
    post(VK_ESC, False, 0)
    try:
        dismissed = wait_for(lambda s: panel(s).get('visible') is False, 5,
                             'esc to dismiss the panel')
        if panel(dismissed).get('claims') is not False:
            failures.append('esc left the panel claiming chords; the claim leaked past the session')
    except AssertionError as exc:
        failures.append(str(exc))
finally:
    release_all()

if failures:
    for failure in failures:
        print(f'e2e panel-action-keys: FAIL: {failure}', file=sys.stderr)
    raise SystemExit(1)
# PASS 必须带证据:一个跳过所有断言的场景本会看起来一模一样。
print('e2e panel-action-keys: PASS '
      f'(cards {cards_before} -> {len(cards(state() or {}))}; '
      f'cmd+w closed {close_pid}:{close_wid} with the panel still open; '
      f'cmd+m acted on {min_pid}:{min_wid}; '
      f'cmd+h hid the controlled app; sticky held over the release: commit {commit_before} -> '
      f'{commit_after}; esc ended the session with claims='
      f'{panel(state() or {}).get("claims")})')
PY
