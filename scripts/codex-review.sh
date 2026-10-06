#!/usr/bin/env bash
#
# Independent Codex review of the current working tree.
#
#   scripts/codex-review.sh                     first review, or a re-review in the same session
#   scripts/codex-review.sh --retry             resume after a round that produced no verdict
#   scripts/codex-review.sh --fresh             start a new session when the old one is gone
#   scripts/codex-review.sh --note "<text>"     the implementation agent's own words to the reviewer
#   scripts/codex-review.sh --note-file <path>  the same, read from a file
#   scripts/codex-review.sh --timeout <sec>     bound a run that hangs (default 1800, 0 disables)
#   scripts/codex-review.sh --finish            clear the reviewer session for the next task
#
# The reviewer runs on gpt-6.1-sol; set OMT_REVIEW_MODEL to review with another model.
#
# The reviewer is asked to write its findings in Chinese, with the severity tokens left in English.
#
# A tree with no change relative to HEAD is refused (exit 65, --retry exempt): a round that reviewed
# nothing must not be counted as a review that passed.
#
# A round only counts as a review when Codex actually finished one. Anything else — a transport
# failure, a turn that never completed, no final message, a message that is not a verdict, a timeout,
# an interruption — exits non-zero with the reason, keeps the session, and archives that round's
# evidence, so that a retry resumes the same reviewer instead of starting a second one that remembers
# nothing.
#
#   .agent-review/codex-session-id   the reviewer session, kept across failures and interruptions
#   .agent-review/last-review.txt    the newest readable review (also printed)
#   .agent-review/last-review.jsonl  the newest raw event stream
#   .agent-review/last-message.txt   the CLI's own final-message file for the newest run
#   .agent-review/last-stderr.txt    the CLI's stderr for the newest run
#   .agent-review/last-status.txt    what the classification pass made of the newest run
#   .agent-review/round-counter      the round number the archive names carry
#   .agent-review/rounds/            every attempt's events, message, stderr and outcome

set -euo pipefail

# Job control, so that every background child — the CLI, the watchdog — becomes its own process
# group. Terminating one then reaches the processes it started (the commands the reviewer ran, the
# watchdog's own sleep) and nothing else, which is what makes the bounded termination below and the
# release of the caller's pipes reliable.
set -m

CALLER_DIR="$(pwd)"

# The first git call, so this is where a broken repository has to be reported: without the guard a failure
# here leaves the script under `set -e` with git's own exit code and no explanation.
if ! ROOT="$(git rev-parse --show-toplevel 2>&1)"; then
    echo "Cannot tell whether the tree changed: git failed in this directory." >&2
    echo "$ROOT" >&2
    echo "Nothing was reviewed and no state was touched. Fix the repository (dubious ownership, a corrupt" >&2
    echo "index, a missing HEAD, a stray GIT_DIR) and try again." >&2
    exit 66
fi
cd "$ROOT"

# The reviewer's model. OMT_REVIEW_MODEL is for trying another one without editing this script.
MODEL="${OMT_REVIEW_MODEL:-gpt-6.1-sol}"

STATE_DIR="$ROOT/.agent-review"
SESSION_FILE="$STATE_DIR/codex-session-id"
OUTPUT_FILE="$STATE_DIR/last-review.txt"
JSON_FILE="$STATE_DIR/last-review.jsonl"
MESSAGE_FILE="$STATE_DIR/last-message.txt"
STDERR_FILE="$STATE_DIR/last-stderr.txt"
STATUS_FILE="$STATE_DIR/last-status.txt"

# The conclusion is one authoritative value, so it is replaced, not appended: a signal that arrives while the
# round is being archived must be able to turn this round's `concluded=yes` into `concluded=no`.
set_concluded() {
    local value="$1" tmp
    tmp="$(mktemp)"
    if [[ -f "$STATUS_FILE" ]]; then
        grep -v '^concluded=' "$STATUS_FILE" > "$tmp" || true
    fi
    echo "concluded=$value" >> "$tmp"
    mv "$tmp" "$STATUS_FILE"
}
KIND_FILE="$STATE_DIR/last-round-kind"
ROUNDS_DIR="$STATE_DIR/rounds"
LOCK_DIR="$STATE_DIR/lock"
TIMEOUT_MARKER="$STATE_DIR/timed-out"

usage() {
    cat >&2 <<'EOF'
usage: scripts/codex-review.sh [--design] [--retry] [--fresh] [--note <text>]
                               [--note-file <path>] [--timeout <seconds>] [--finish]

  (no options)        review, or re-review the same session after fixes
  --design            put the approach to the reviewer before any code is written; ends with
                      PLAN-STATUS instead of REVIEW-STATUS and needs no change in the tree
  --retry             the previous round produced no verdict; resume the same session and say so
  --fresh             start a new session (the old one is gone or its memory is to be dropped)
  --note <text>       put the implementation agent's own words to the reviewer
  --note-file <path>  the same, read from a file (both may be given; they are joined)
  --timeout <sec>     bound a run that has not finished in this long (default 1800, 0 = none)
  --finish            clear the reviewer session for the next task

The model is gpt-6.1-sol; OMT_REVIEW_MODEL overrides it.
EOF
}

note=""
note_file=""
finish=0
retry=0
fresh=0
design=0
timeout=1800

while [[ $# -gt 0 ]]; do
    case "$1" in
        --note)
            [[ $# -ge 2 ]] || { usage; exit 64; }
            note="$2"
            shift 2
            ;;
        --note-file)
            [[ $# -ge 2 ]] || { usage; exit 64; }
            note_file="$2"
            shift 2
            ;;
        --timeout)
            [[ $# -ge 2 ]] || { usage; exit 64; }
            # Bounded in digits *and* in value: bash arithmetic is fixed width, so an oversized number wraps
            # (0x10000000000000000 becomes 0) and would silently turn the watchdog off.
            if [[ ! "$2" =~ ^[0-9]{1,9}$ ]]; then
                echo "--timeout takes a whole number of seconds, from 0 to 999999999." >&2
                usage
                exit 64
            fi
            # Force base 10: a leading zero would otherwise make the arithmetic below read it as octal, the
            # comparison would fail and the watchdog would never start, leaving a hung run holding the lock.
            timeout="$((10#$2))"
            shift 2
            ;;
        --retry)
            retry=1
            shift
            ;;
        --fresh)
            fresh=1
            shift
            ;;
        --design)
            design=1
            shift
            ;;
        --finish)
            finish=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            exit 64
            ;;
    esac
done

mkdir -p "$STATE_DIR" "$ROUNDS_DIR"

# A relative --note-file belongs to the directory the caller typed it in, not to the repository root
# this script works from.
if [[ -n "$note_file" && "$note_file" != /* ]]; then
    note_file="$CALLER_DIR/$note_file"
fi

if [[ "$retry" == 1 && "$fresh" == 1 ]]; then
    echo "--retry resumes the session and --fresh replaces it; give only one." >&2
    exit 64
fi

# One review at a time: two runs would overwrite each other's session file and event stream.
release_lock() {
    rmdir "$LOCK_DIR" 2>/dev/null || true
}

if [[ "$finish" == 1 ]]; then
    if [[ -n "$note" || -n "$note_file" || "$design" == 1 || "$retry" == 1 || "$fresh" == 1 ]]; then
        echo "--finish clears the session; it takes no note and no round of its own." >&2
        exit 64
    fi
    if ! mkdir "$LOCK_DIR" 2>/dev/null; then
        echo "A review is running (lock: $LOCK_DIR), so the session cannot be cleared." >&2
        exit 75
    fi
    trap release_lock EXIT
    rm -f "$SESSION_FILE" "$OUTPUT_FILE" "$STATE_DIR/last-plan.txt" "$KIND_FILE" "$JSON_FILE" \
        "$MESSAGE_FILE" "$STATUS_FILE" "$TIMEOUT_MARKER" "$JSON_FILE.stream-message"
    echo "Codex reviewer session cleared."
    exit 0
fi

if ! command -v codex >/dev/null 2>&1; then
    echo "codex CLI not found on PATH; install it or fix PATH before reviewing." >&2
    exit 127
fi

if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    echo "Another review is already running (lock: $LOCK_DIR)." >&2
    echo "If nothing is reviewing, remove that directory and try again." >&2
    exit 75
fi
trap release_lock EXIT

# A retry resumes *that* round, so it keeps that round's kind: the one thing a retry must never do is
# turn a design consultation that was never answered into a code review verdict. Read under the lock,
# next to the session it is about, and never guessed: a missing record is a refusal, not a default,
# because "no record" is not evidence that the last round reviewed code.
if [[ "$retry" == 1 ]]; then
    # The kind file is the only source, and it is the right one because `--finish` clears it: a record
    # that survives the task it belonged to would let an old task's review answer for this session.
    # The archive deliberately is not consulted for this — it outlives the session, and a kind taken
    # from it could turn a review that never happened into one that passed.
    recorded_kind="$(cat "$KIND_FILE" 2>/dev/null || true)"
    # A retry resumes a round that produced *no* conclusion, so the last round's own record decides whether
    # there is anything to resume. `concluded` is the round's outcome (turn completed, CLI clean, not killed),
    # not the shape of its message: a verdict-shaped message from a run that died is exactly what a retry is
    # for. `|| true` inside the substitution, not only outside -- the script runs with `set -euo pipefail`, and
    # a missing status file would otherwise fail the pipeline, kill the script here and skip the refusals below.
    recorded_verdict="$(sed -n 's/^concluded=//p' "$STATUS_FILE" 2>/dev/null | tail -1 || true)"
    if [[ "$recorded_verdict" == "yes" ]]; then
        echo "--retry resumes a round that produced no verdict, and the last round reached one." >&2
        echo "Run the reviewer again for a re-review, or start a new session with --fresh." >&2
        exit 64
    fi
    case "$recorded_kind" in
        plan|review)
            if [[ "$design" == 1 && "$recorded_kind" != "plan" ]]; then
                echo "--retry resumes the round that produced no verdict, and that round was a $recorded_kind round." >&2
                echo "A retry cannot change the kind of round it is resuming." >&2
                exit 64
            fi
            if [[ "$design" != 1 && "$recorded_kind" == "plan" ]]; then
                design=1
                echo "Resuming the interrupted design round (--design is implied by the round being retried)." >&2
            fi
            ;;
        *)
            if [[ "$design" != 1 ]]; then
                echo "Nothing on record says what the interrupted round was, so this cannot resume it." >&2
                echo "Add --design if it was a design round; a review cannot be resumed from a guess, so" >&2
                echo "make the change and run the review, or start a new session with --fresh." >&2
                exit 64
            fi
            echo "Nothing on record says what the interrupted round was; --design was given, so this resumes as one." >&2
            ;;
    esac
fi

# What this round is decides three things: which status line it must end with, which readable file it
# writes, and what its archive entry is called.
if [[ "$design" == 1 ]]; then
    ROUND_KIND="plan"
    MARKER_NAME="PLAN-STATUS"
    MARKER_VALUES="AGREED or CONCERNS"
    OUTPUT_FILE="$STATE_DIR/last-plan.txt"
else
    ROUND_KIND="review"
    MARKER_NAME="REVIEW-STATUS"
    MARKER_VALUES="PASS or FINDINGS"
fi

# Nothing to review is not the same as a review that found nothing. With the index, the working tree
# and the untracked files all matching HEAD there is no change for the reviewer to look at, and a round
# that looked at nothing must not be counted as one that passed. --retry and --design are exempt on
# purpose: a retry resumes a round that was interrupted, and a design round is asked *before* there is
# any code to look at — the tree not having changed is exactly both cases.
# 0: there are changes, 1: there are none, 2: this could not be determined. "Cannot check" is not "the
# precondition holds": review starts only on a proven change, and a git failure says so instead of passing.
have_changes() {
    local diff untracked
    if git rev-parse --verify --quiet HEAD >/dev/null 2>&1; then
        diff="$(git diff HEAD --no-color --no-ext-diff 2>&1)" || return 2
    else
        # A repository with no commit yet has no HEAD. The worktree is compared against the empty tree -- the same
        # "final content" question the HEAD path asks -- instead of concatenating the index and worktree diffs,
        # which cancel each other out for a file that was staged and then deleted.
        local empty_tree
        empty_tree="$(git hash-object -t tree /dev/null 2>&1)" || return 2
        diff="$(git diff "$empty_tree" --no-color --no-ext-diff 2>&1)" || return 2
    fi
    untracked="$(git ls-files --others --exclude-standard 2>&1)" || return 2
    [[ -n "$diff" || -n "$untracked" ]]
}

# Every mode asks whether the repository can be read at all; only the "there is nothing to review" refusal is
# what a retry (and a design round) is exempt from.
if have_changes; then
    changes=0
else
    changes=$?
fi

if [[ "$changes" -eq 2 ]]; then
    echo "Cannot tell whether the tree changed: git failed. Nothing was reviewed and no state was touched." >&2
    echo "Fix the repository (dubious ownership, a corrupt index, a missing HEAD) and try again." >&2
    exit 66
fi

if [[ "$changes" -eq 1 && "$retry" != 1 && "$design" != 1 ]]; then
    echo "Nothing to review: the index, the working tree and the untracked files all match HEAD." >&2
    echo "A round that reviewed no change is not a review, so this refuses rather than reporting one (exit 65)." >&2
    echo "Make the change, run the build and the tests, then review; --finish still works on a clean tree." >&2
    echo "A design round (--design) is the exception: it is asked before there is anything to review." >&2
    exit 65
fi

if [[ -n "$note_file" ]]; then
    if [[ ! -f "$note_file" ]]; then
        echo "--note-file: not a file: $note_file" >&2
        exit 66
    fi
    from_file="$(<"$note_file")"
    if [[ -n "$note" ]]; then
        note="$note"$'\n\n'"$from_file"
    else
        note="$from_file"
    fi
fi

CHILD_PID=""
WATCHDOG_PID=""
REASON=""
CODE=0
ARCHIVED=0

# Whether the process group still has any member. Asking about the group rather than about the
# process that led it is what makes the sweep below complete: the CLI can exit on TERM while a
# command it started, which inherited the group, keeps running and keeps the caller's pipes open.
group_alive() {
    [[ -n "$1" ]] && kill -0 -"$1" 2>/dev/null
}

# TERM, a bounded grace, then KILL — against the whole process group, and the escalation is decided by
# whether the group is still populated, not by the leader's liveness.
terminate_group() {
    local pgid="$1" waited=0
    group_alive "$pgid" || return 0
    kill -TERM -"$pgid" 2>/dev/null || kill -TERM "$pgid" 2>/dev/null || true
    while group_alive "$pgid" && [[ "$waited" -lt 25 ]]; do
        sleep 0.2
        waited=$((waited + 1))
    done
    if group_alive "$pgid"; then
        kill -KILL -"$pgid" 2>/dev/null || kill -KILL "$pgid" 2>/dev/null || true
    fi
    wait "$pgid" 2>/dev/null || true
}

# The watchdog is a process group of its own: killing the group takes its sleep with it, so nothing is
# left holding the caller's stdout or stderr open after this script has returned.
stop_watchdog() {
    [[ -n "$WATCHDOG_PID" ]] || return 0
    kill -TERM -"$WATCHDOG_PID" 2>/dev/null || kill -TERM "$WATCHDOG_PID" 2>/dev/null || true
    wait "$WATCHDOG_PID" 2>/dev/null || true
    WATCHDOG_PID=""
}

# Reads the session id out of the event stream written so far, if it is there yet. Used by the signal
# handler as well as by the normal path: a run that dies still created a session, and the retry has to
# resume it rather than start another one that remembers nothing.
current_session_id() {
    [[ -f "$JSON_FILE" ]] || return 0
    python3 - "$JSON_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    for line in f:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue

        if event.get("type") == "thread.started" and event.get("thread_id"):
            print(event["thread_id"])
            break
PY
}

keep_session() {
    if [[ ! -f "$SESSION_FILE" ]]; then
        local found
        found="$(current_session_id)"
        if [[ -n "$found" ]]; then
            printf '%s\n' "$found" > "$SESSION_FILE"
        fi
    fi
}

# Every attempt's evidence is kept — events, the final message, the CLI's stderr and the outcome — so
# that a failed round cannot take the previous round's review with it, and a failure can be explained
# later from what it actually printed.
archive_round() {
    [[ "$ARCHIVED" == 0 ]] || return 0
    ARCHIVED=1
    local reason="$1" code="$2" stamp round_number
    round_number=1
    if [[ -f "$STATE_DIR/round-counter" ]]; then
        round_number=$(( $(cat "$STATE_DIR/round-counter") + 1 ))
    fi
    printf '%s\n' "$round_number" > "$STATE_DIR/round-counter"
    stamp="$(date -u +%Y%m%dT%H%M%SZ)-$(printf '%03d' "$round_number")"
    [[ -s "$JSON_FILE" ]] && cp "$JSON_FILE" "$ROUNDS_DIR/$stamp-events.jsonl"
    [[ -s "$MESSAGE_FILE" ]] && cp "$MESSAGE_FILE" "$ROUNDS_DIR/$stamp-$ROUND_KIND.txt"
    [[ -s "$STDERR_FILE" ]] && cp "$STDERR_FILE" "$ROUNDS_DIR/$stamp-stderr.txt"
    {
        echo "reason=$reason"
        echo "exit=$code"
        echo "kind=$ROUND_KIND"
        echo "session=$(cat "$SESSION_FILE" 2>/dev/null || echo none)"
    } > "$ROUNDS_DIR/$stamp-outcome.txt"
    echo "$stamp"
}

# Ctrl+C and TERM: stop the watchdog, stop the CLI and everything it started, keep the session,
# archive the interrupted round, and let the EXIT trap release the lock — in that order, so a retry
# cannot start while the first review is still writing.
on_signal() {
    local signal="$1"
    # Further signals are ignored for the duration of the cleanup: a second Ctrl+C must not kill the
    # script before the CLI is gone, the session saved and the round archived. Releasing the lock while
    # the review still runs is what would let the next round write the same files.
    trap '' INT TERM

    stop_watchdog
    terminate_group "$CHILD_PID"
    # Only now, with nothing left writing to the event stream: a CLI that answered the first TERM by
    # creating its session would have its id thrown away if this ran before the grace period.
    keep_session
    REASON="interrupted (signal $signal)"
    CODE="$signal"
    # Record this round as unfinished, so a later `--retry` resumes *it* rather than reading a verdict from an
    # earlier round that did conclude.
    set_concluded no
    rm -f "$OUTPUT_FILE"
    archive_round "$REASON" "$CODE" >/dev/null || true
    exit "$signal"
}

trap 'on_signal 130' INT
trap 'on_signal 143' TERM

if [[ "$fresh" == 1 && -f "$SESSION_FILE" ]]; then
    echo "Discarding the reviewer session (--fresh): its memory of earlier findings is gone." >&2
    rm -f "$SESSION_FILE"
fi

# A new round starts clean: a stale review must never be read as this round's, and the status record belongs to
# this round from here on. Without that, an interrupted round would leave the previous round's `concluded=yes`
# behind, and a retry would refuse to resume a round that in fact never concluded.
rm -f "$OUTPUT_FILE" "$MESSAGE_FILE" "$TIMEOUT_MARKER" "$JSON_FILE.stream-message"
: > "$STDERR_FILE"
{
    echo "concluded=no"
    echo "completed=no"
    echo "round_status=none"
    echo "marker_name=$MARKER_NAME"
} > "$STATUS_FILE"

FIRST_REVIEW_PROMPT=$(cat <<'EOF'
Act strictly as an independent code reviewer.

用中文撰写全部审查意见：严重度、文件与位置、问题是什么、为什么重要、建议怎么改。
严重度标记保留 BLOCKER / HIGH / MEDIUM / LOW 原文，便于检索。

Another agent implemented the current task. Review the implementation in the
current repository.

Inspect the current git diff and surrounding code where necessary.

Do NOT modify files.
Do NOT implement fixes.
Do NOT refactor the code yourself.

Focus on substantive engineering issues, especially:

- correctness
- regressions
- edge cases
- concurrency and async behavior
- memory/resource lifecycle
- error handling
- API misuse
- performance
- architecture
- maintainability
- missing or insufficient tests

Avoid subjective style comments unless they have a meaningful engineering
impact.

For every issue provide:

- Severity: BLOCKER / HIGH / MEDIUM / LOW
- File and location
- What is wrong
- Why it matters
- Recommended fix

If there are no significant issues, explicitly say so.

Remember all findings because you will review fixes in later rounds.
EOF
)

REREVIEW_PROMPT=$(cat <<'EOF'
The implementation agent has updated the code in response to your previous
review.

用中文撰写全部审查意见：严重度、文件与位置、问题是什么、为什么重要、建议怎么改。
严重度标记保留 BLOCKER / HIGH / MEDIUM / LOW 原文，便于检索。

Re-review the current repository.

Do NOT modify files.
Do NOT implement fixes.

First revisit every finding from your previous review and classify it as:

- RESOLVED
- PARTIALLY RESOLVED
- UNRESOLVED
- REJECTED WITH VALID JUSTIFICATION

Do not assume an issue is fixed merely because the code changed.

Then inspect the new/current diff for:

- regressions
- newly introduced bugs
- edge cases
- concurrency problems
- memory/resource lifecycle issues
- error handling problems
- API misuse
- performance regressions
- architectural problems
- missing tests

For every remaining or new issue provide:

- Severity: BLOCKER / HIGH / MEDIUM / LOW
- File and location
- What is wrong
- Why it matters
- Recommended fix

If all significant findings are resolved and you find no new significant
issues, explicitly state that the implementation passes review.
EOF
)

# Nothing in the tree has changed since a round that produced no verdict, so telling the reviewer that
# the code was updated would be a lie, and an answer built on it would describe an update that never
# happened.
RETRY_PROMPT=$(cat <<'EOF'
You were interrupted before you finished your previous review of this repository: the
transport failed, or the run was stopped. Nothing in the working tree has changed since
then.

Continue or redo that review now, under the same rules as before.

用中文撰写全部审查意见：严重度、文件与位置、问题是什么、为什么重要、建议怎么改。
严重度标记保留 BLOCKER / HIGH / MEDIUM / LOW 原文，便于检索。

Do NOT modify files.
Do NOT implement fixes.

For every issue provide:

- Severity: BLOCKER / HIGH / MEDIUM / LOW
- File and location
- What is wrong
- Why it matters
- Recommended fix

If there are no significant issues, explicitly say so.
EOF
)

# The prompts ask for one machine-readable line, so that "did this round reach a verdict" is a
# structure question and not a guess at wording: a message that says it has not finished contains any
# number of reassuring words, and keyword matching accepted exactly those.
DESIGN_PROMPT=$(cat <<'EOF'
The implementation agent is about to build something, and is putting the approach to you
before writing any code.

用中文撰写全部审查意见：严重度、问题是什么、为什么重要、建议怎么改。
严重度标记保留 BLOCKER / HIGH / MEDIUM / LOW 原文，便于检索。

Do NOT modify files.
Do NOT implement anything.
Do NOT write the code for the agent: the agent writes it, you judge the approach.

Nothing has been built yet, so judge the approach and not an implementation. Weigh what
the requester asked for, the repository's own conventions and invariants, and the
alternatives the agent says it considered and rejected.

Work through, in this order:

- Is this approach sound for the stated goal? If it is, say so plainly.
- What could go wrong with it: correctness, concurrency, resource lifetime, error
  handling, performance, migration of existing data or behaviour, and tests.
- Was rejecting the alternatives right, and is there a better approach the agent missed?
- What does the plan leave unverified, and what should the verification cover?
- Which constraints from the repository's guidance does the plan appear to violate?

For every issue: severity, what is wrong with the approach, why it matters, and what you
would do instead. Remember these points: the implementation will come back to you, and
you will be asked whether it followed what you agreed here.
EOF
)

REPO_CONTEXT=$(cat <<'EOF'

本仓库的上下文，评审时按这些规则判断（不要改文件、不要代改代码、不要自行撤销结论）：

- 项目：`oh-my-tab`，macOS 菜单栏窗口切换器。Rust + objc2/AppKit/CoreGraphics 的 FFI，无 Swift bridge。
  重点检查：unsafe/FFI 的生命周期与所有权、`msg_send!` 的选择器与字段名、主线程不变量（AppKit 与动态注册的
  Objective-C 回调必须在主线程）、`Mutex/RwLock` 的所有权与重入、动态 ObjC 类对 Rust 侧字段的依赖。
- 验证分层按 AGENTS.md 的定义（分层依据是**谁判定通过与否**，不是用什么传输）：A1 = 纯 headless 测试 + `--smoke-*`
  跑在真实 AppKit 视图树上；A2 = `scripts/e2e/*.sh`（入口 `scripts/e2e/run-all.sh`），断言 app 自己写出的 JSON
  状态与真实输入/WindowServer 状态；B = MCP + 截图，由人或 agent 判定，**从不作为门禁**。
  晋升规则按 AGENTS.md 原文：B 层发现的 bug 要么落一条 A 层断言，**要么在还不能断言时先补一个使它能被断言的状态字段**
  （两者都合规，不要因为改动选择后者就判违规）。
- 门禁：`cargo fmt --check`、`cargo clippy`、`cargo test`、`cargo check --release`；UI/交互改动另有
  `scripts/dev-restart.sh` 与相应 e2e 场景。评审者看不到"跑过哪些门禁"，需要时请在结论里明确索取。
- 规范文档：`docs/design-style{,-en}.md` 是用户可见外观的规范；代码与文档冲突时**文档为准**，除非同一改动
  里改了文档并给出理由。

以下四类问题在本仓库反复出现，请作为重点检查项：

1. **定量声明必须有可复现的命令**。文档或注释里的任何数字（对比度、比例、坐标、尺寸、耗时）都应能由一条具名
   命令复现；如果测量手段后来被修正，旧数字立即作废，文档与代码注释必须同步。把未验证的数字写成已确认，是
   本仓库已发生过多次的真实缺陷。
2. **没有反例自检的"仪器"不是门禁**。任何验收手段都应带反例回归（例如 `--selftest`），并检查它是否覆盖了
   *曾经击败过它* 的场景；只覆盖顺利用例的仪器不算数。
3. **开发开关不得泄漏**。e2e/场景跑完必须把 app 留在可用状态；一个把面板文字画成透明、或把纯色窗口钉在面板后
   的实例被留给用户，是已经发生过的事故。
4. **文档不得被悄悄放宽或夸大**。检查这一改动是否削弱了文档中的承诺，或把"目标值"写成了"已强制执行"。
EOF
)

REVIEW_STATUS_INSTRUCTION=$(cat <<'EOF'

整个回复的最后一行、整行只能是下面二者之一，脚本只认这一行来判断本轮是否给出了结论。状态词用大写英文；可以整体加粗或用反引号包起来，但不要附加任何别的字符、标点、后缀或说明，也不要把它放进代码块以外的引用里：

REVIEW-STATUS: PASS
REVIEW-STATUS: FINDINGS

PASS 表示没有 BLOCKER / HIGH / MEDIUM 发现（存在 LOW 或纯风格意见也算 PASS）；FINDINGS 表示还有需要修复的发现。这一行必须是最后一行：写在前面、或后面还有别的话，都会被判为「这一轮没有给出结论」。
EOF
)

# The note is the implementation agent's own words, so it goes in verbatim rather than being
# paraphrased into the request.
if [[ -n "$note" ]]; then
    if [[ "$design" == 1 ]]; then
        # The brief *is* the substance of a design round: the goal, the constraints, what was
        # inspected, the approach and the alternatives that were rejected.
        DESIGN_PROMPT="$DESIGN_PROMPT"$'\n\n'"The implementation agent's proposal follows."$'\n\n'"$note"
    else
        FIRST_REVIEW_PROMPT="$FIRST_REVIEW_PROMPT"$'\n\n'"The requester adds the following context."$'\n\n'"$note"
        REREVIEW_PROMPT="$REREVIEW_PROMPT"$'\n\n'"The implementation agent's response to your previous findings follows."$'\n\n'"$note"
        RETRY_PROMPT="$RETRY_PROMPT"$'\n\n'"The requester adds the following context."$'\n\n'"$note"
    fi
fi

if [[ "$fresh" == 1 ]]; then
    # A lost session means the reviewer remembers nothing: say so, so that a finding already fixed is
    # not read as still open and a finding never raised is not assumed settled.
    if [[ "$design" == 1 ]]; then
        FRESH_NOTE="The previous reviewer session was lost, so the approach is put to you without it. Nothing has been built yet."
        DESIGN_PROMPT="$DESIGN_PROMPT"$'\n\n'"$FRESH_NOTE"
    else
        # No claim about earlier findings: the loss of a session is not evidence that anything was fixed, and
        # a reviewer told otherwise could pass over a finding that is still open.
        FRESH_NOTE="The previous reviewer session was lost, so review the current tree from scratch and reach your own conclusion: you have no memory of any earlier finding, and whether one was fixed is for you to judge from the diff."
        FIRST_REVIEW_PROMPT="$FIRST_REVIEW_PROMPT"$'\n\n'"$FRESH_NOTE"
    fi
fi

PLAN_STATUS_INSTRUCTION=$(cat <<'EOF'

整个回复的最后一行、整行只能是下面二者之一，脚本只认这一行来判断这一轮是否给出了结论。状态词用大写英文；可以整体加粗或用反引号包起来，但不要附加任何别的字符、标点、后缀或说明：

PLAN-STATUS: AGREED
PLAN-STATUS: CONCERNS

AGREED 表示这个方案可以照着做；CONCERNS 表示方案有问题需要先改。写在前面、或后面还有别的话，都会被判为「这一轮没有给出结论」。
EOF
)

# Appended last, so the requirement is the final thing in each prompt. A design round ends with a
# different line than a review round, because "the plan is sound" and "the code is sound" are not the
# same claim and must not be able to pass as each other.
if [[ "$design" == 1 ]]; then
    DESIGN_PROMPT="$DESIGN_PROMPT$REPO_CONTEXT$PLAN_STATUS_INSTRUCTION"
else
    FIRST_REVIEW_PROMPT="$FIRST_REVIEW_PROMPT$REPO_CONTEXT$REVIEW_STATUS_INSTRUCTION"
    REREVIEW_PROMPT="$REREVIEW_PROMPT$REPO_CONTEXT$REVIEW_STATUS_INSTRUCTION"
    RETRY_PROMPT="$RETRY_PROMPT$REPO_CONTEXT$REVIEW_STATUS_INSTRUCTION"
fi

# Runs Codex with its output captured, stdin pointed at /dev/null, and a watchdog that can actually end
# it. When stdin is not a terminal the CLI always tries to read "additional input" from it and says so on
# stderr; /dev/null makes that read return end-of-file at once, where an inherited pipe with no writer —
# how an agent usually invokes this script — could otherwise leave it waiting. stderr goes to a file
# rather than through a process substitution, which a sandbox that forbids /dev/fd would refuse.
run_codex() {
    local status=0
    codex "$@" </dev/null > "$JSON_FILE" 2> "$STDERR_FILE" &
    CHILD_PID=$!
    if [[ "$timeout" -gt 0 ]]; then
        # The watchdog escalates by itself: a CLI that ignores TERM would otherwise leave the parent
        # waiting for ever. Its own output goes nowhere, so it can never hold the caller's pipes.
        (
            sleep "$timeout"
            : > "$TIMEOUT_MARKER"
            kill -TERM -"$CHILD_PID" 2>/dev/null || kill -TERM "$CHILD_PID" 2>/dev/null || true
            waited=0
            while kill -0 "$CHILD_PID" 2>/dev/null && [[ "$waited" -lt 25 ]]; do
                sleep 0.2
                waited=$((waited + 1))
            done
            kill -KILL -"$CHILD_PID" 2>/dev/null || kill -KILL "$CHILD_PID" 2>/dev/null || true
        ) >/dev/null 2>&1 &
        WATCHDOG_PID=$!
    fi
    wait "$CHILD_PID" || status=$?
    # The leader exiting is not the group emptying: sweep whatever it left behind before the watchdog
    # is cancelled, because that watchdog is what would otherwise have escalated. When the group is
    # already empty this costs nothing.
    terminate_group "$CHILD_PID"
    stop_watchdog
    CHILD_PID=""
    return "$status"
}

# Written here, not earlier: an invocation that a check turns away must leave the last round's kind
# alone, or a refused call could rewrite what the next --retry believes it is resuming.
printf '%s\n' "$ROUND_KIND" > "$KIND_FILE"

if [[ "$design" == 1 && "$retry" == 1 ]]; then
    # Design recovery wording: the question is the same one, and redoing it is the right answer.
    DESIGN_PROMPT="$DESIGN_PROMPT"$'\n\n'"Your previous answer to this design question was interrupted by a transport failure, and nothing in the repository has changed since. Answer it again."
fi

if [[ "$design" == 1 ]]; then
    FIRST_PROMPT="$DESIGN_PROMPT"
else
    FIRST_PROMPT="$FIRST_REVIEW_PROMPT"
fi

if [[ ! -f "$SESSION_FILE" ]]; then
    echo "Starting new Codex reviewer session..."
    status=0
    run_codex exec --model "$MODEL" --json -o "$MESSAGE_FILE" "$FIRST_PROMPT" || status=$?
else
    SESSION_ID="$(cat "$SESSION_FILE")"
    if [[ "$design" == 1 ]]; then
        echo "Resuming Codex reviewer session (design round):"
        PROMPT="$DESIGN_PROMPT"
    elif [[ "$retry" == 1 ]]; then
        echo "Retrying Codex reviewer session:"
        PROMPT="$RETRY_PROMPT"
    else
        echo "Resuming Codex reviewer session:"
        PROMPT="$REREVIEW_PROMPT"
    fi
    echo "$SESSION_ID"
    echo
    status=0
    run_codex exec resume "$SESSION_ID" --model "$MODEL" --json -o "$MESSAGE_FILE" "$PROMPT" || status=$?
fi

# One pass over the event stream: what happened, and what the final message was. The CLI's own -o file
# is the only source of a final message — it is the CLI's documented contract for one — and a message
# that exists only in the event stream is reported as evidence rather than promoted to a verdict,
# because that is what a turn cut short would look like.
python3 - "$JSON_FILE" "$MESSAGE_FILE" "$STATUS_FILE" "$MARKER_NAME" "$ROUND_KIND" <<'PY'
import json
import re
import sys

events_path, message_path, status_path = sys.argv[1], sys.argv[2], sys.argv[3]
marker_name, round_kind = sys.argv[4], sys.argv[5]

thread_id = ""
completed = False
errors = []
stream_message = ""

try:
    with open(events_path, "r", encoding="utf-8") as f:
        for line in f:
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue

            kind = event.get("type") or ""
            if kind == "thread.started":
                thread_id = event.get("thread_id") or thread_id
            elif kind == "turn.completed":
                completed = True
            elif "error" in kind or "failed" in kind:
                errors.append(json.dumps(event, ensure_ascii=False)[:300])

            if kind == "item.completed":
                item = event.get("item") or {}
                if item.get("type") == "agent_message" and item.get("text"):
                    stream_message = item["text"]
except FileNotFoundError:
    pass

message = ""
try:
    with open(message_path, "r", encoding="utf-8") as f:
        message = f.read().strip()
except FileNotFoundError:
    pass

# The structure the prompts demand, checked as structure: the message must end with the status line,
# and that line must be nothing else. Both weaker checks were wrong — keyword matching accepted "我还没有
# 检查并发问题，需要继续吗？", and searching for the marker anywhere accepted a message that quoted the
# format and then said it had not finished. So: the last non-empty line, whole line, decoration
# tolerated and nothing else. Truncation is not this check's job: turn.completed above already means
# the turn ended by itself.
def concluding_line(text):
    # The verdict is the last non-empty line of the message, whole. A closing code fence is *not* skipped:
    # skipping it made a message that ended with a fenced format example ("```\nREVIEW-STATUS: PASS\n```")
    # read as a verdict, which is exactly the round this check exists to refuse.
    for line in reversed(text.splitlines()):
        stripped = line.strip()
        if stripped:
            return stripped
    return ""

# A design round answers "is the plan sound"; a review round answers "is the code sound". They end
# with different lines on purpose, so neither can be mistaken for the other.
tokens = ("AGREED", "CONCERNS") if round_kind == "plan" else ("PASS", "FINDINGS")
last_line = concluding_line(message)
# The prompt allows the line to be wrapped for emphasis, so the decoration must be a *pair* -- `X` or
# **X** -- and it allows nothing else: no lowercase, no trailing period, no unpaired marker. A wider match
# accepts messages the prompt itself calls invalid.
# The prompt allows the line to be wrapped for emphasis, so the decoration must be a *pair* -- `X` or **X** --
# and nothing else is tolerated: no lowercase, no trailing period, no unpaired marker. A wider match accepts
# messages the prompt itself calls invalid, which would let a round with no verdict count as a passed one.
verdict_line = last_line
for decoration in ("**", "`"):
    if (
        verdict_line.startswith(decoration)
        and verdict_line.endswith(decoration)
        and len(verdict_line) > 2 * len(decoration)
    ):
        verdict_line = verdict_line[len(decoration) : -len(decoration)].strip()
        break
status_line = re.fullmatch(
    r"%s:\s*(%s)" % (re.escape(marker_name), "|".join(tokens)),
    verdict_line,
)
round_status = status_line.group(1).upper() if status_line else "none"
looks_like_verdict = status_line is not None

with open(status_path, "w", encoding="utf-8") as f:
    f.write("thread_id=%s\n" % thread_id)
    f.write("completed=%s\n" % ("yes" if completed else "no"))
    f.write("error=%s\n" % (errors[0] if errors else ""))
    f.write("message_chars=%d\n" % len(message))
    f.write("looks_like_verdict=%s\n" % ("yes" if looks_like_verdict else "no"))
    f.write("round_status=%s\n" % round_status)
    f.write("marker_name=%s\n" % marker_name)
    f.write("last_line=%s\n" % (last_line[:120] if last_line else ""))

if stream_message and not message:
    with open(events_path + ".stream-message", "w", encoding="utf-8") as f:
        f.write(stream_message + "\n")
PY

status_field() {
    sed -n "s/^$1=//p" "$STATUS_FILE" | tail -1
}

completed="$(status_field completed)"
message_chars="$(status_field message_chars)"
looks_like_verdict="$(status_field looks_like_verdict)"
cli_error="$(status_field error)"
# The classification above reports the *shape* of the message. A verdict-shaped message still does not make a
# round: the turn has to have completed, the CLI has to have exited cleanly and the run must not have been
# killed. A retry resumes a round that reached none of that, so it tests this field, not the shape.

# Keep the session whatever happened: the CLI may have created it before dying, and a retry has to
# resume that one rather than start another that remembers nothing.
keep_session

if [[ -f "$TIMEOUT_MARKER" ]]; then
    REASON="the run did not finish within ${timeout}s and was terminated"
    CODE=124
elif [[ "$status" -ne 0 ]]; then
    REASON="codex exec exited $status"
    CODE="$status"
elif [[ ! -s "$JSON_FILE" ]]; then
    REASON="codex printed no events"
    CODE=1
elif [[ "$completed" != "yes" ]]; then
    REASON="the turn did not finish (no turn.completed in the event stream)"
    CODE=1
elif [[ "$message_chars" -eq 0 ]]; then
    REASON="the turn produced no final message"
    CODE=1
elif [[ "$looks_like_verdict" != "yes" ]]; then
    REASON="the message does not end with the $MARKER_NAME line ($MARKER_VALUES)"
    CODE=1
fi

set_concluded "$([[ "$CODE" -eq 0 ]] && echo yes || echo no)"

stamp="$(archive_round "${REASON:-none}" "$CODE")"

if [[ "$CODE" -eq 0 ]]; then
    cp "$MESSAGE_FILE" "$OUTPUT_FILE"
    echo
    cat "$OUTPUT_FILE"
    exit 0
fi

echo >&2
echo "This round produced no review: $REASON." >&2
echo "Exit $CODE (codex itself exited $status). Evidence: $ROUNDS_DIR/$stamp-*" >&2
if [[ -n "$cli_error" ]]; then
    echo "Codex reported: $cli_error" >&2
fi
if [[ -s "$STDERR_FILE" ]]; then
    echo "--- codex stderr (last 5 lines) ---" >&2
    tail -5 "$STDERR_FILE" >&2
fi
if [[ -s "$MESSAGE_FILE" ]]; then
    echo "--- the final message, as it arrived (unverified) ---" >&2
    cat "$MESSAGE_FILE" >&2
elif [[ -s "$JSON_FILE.stream-message" ]]; then
    echo "--- a message in the event stream, not the CLI's final message (unverified) ---" >&2
    cat "$JSON_FILE.stream-message" >&2
fi
echo >&2
if [[ -f "$SESSION_FILE" ]]; then
    if [[ "$ROUND_KIND" == "plan" ]]; then
        echo "The design round produced no answer: retry it with --retry (same session, still a design round)." >&2
    else
        echo "The reviewer session was kept: retry with --retry (same session) once the cause is gone." >&2
    fi
    if [[ "$retry" != 1 ]]; then
        echo "Nothing in the tree changed, so --retry says that instead of claiming an update." >&2
    fi
else
    echo "No reviewer session was created; run again when the cause is gone." >&2
fi
if [[ "$status" -ne 0 && "$CODE" != 124 ]]; then
    echo "If resuming keeps failing, the session is gone: --fresh starts a new one (its memory is lost)." >&2
fi
exit "$CODE"
