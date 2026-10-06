#!/usr/bin/env bash
#
# Self-test for scripts/codex-review.sh.
#
# The helper is run against a stub `codex` in a throwaway git repository, so no reviewer session and
# no network are involved. It covers the failure modes the helper exists to classify and the
# behaviour it promises around them:
#
#   - a run that succeeds, and one whose CLI exits non-zero
#   - a run that exits 0 without a completed turn (a transport that died mid-answer)
#   - a completed turn with no final message, and one whose message is not a verdict
#   - a run that hangs, terminated by --timeout
#   - the --retry framing, which must not claim the code was updated
#   - a resume that fails, and the --fresh recovery from it
#   - the lock that keeps two reviews from interleaving
#   - the archive that keeps every attempt, including the failed ones
#   - a relative --note-file, and stdin closed so the CLI cannot read input
#
#   scripts/codex-review-selftest.sh
#
set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
helper="$script_dir/codex-review.sh"

[[ -f "$helper" ]] || { echo "cannot find $helper" >&2; exit 66; }

tmp="$(mktemp -d)"
# The pre-check below asks whether the tree is clean, so the runs that test it must keep the harness's
# own output files outside the repository: shell redirection truncates them before the script starts,
# which would dirty the tree and make the check unsatisfiable.
scratch="$(mktemp -d)"
trap 'rm -rf "$tmp" "$scratch"' EXIT
mkdir -p "$tmp/scripts" "$tmp/bin"
cp "$helper" "$tmp/scripts/"
cd "$tmp"
git init -q .
git config user.email "selftest@example.invalid"
git config user.name "self-test"

cat > bin/codex <<'STUB'
#!/bin/bash
# A stand-in for the CLI: emits the event shapes the helper classifies, writes the -o file when it
# has a final message, and records what it was asked and what its stdin looked like.
mode="${STUB_MODE:-ok}"
printf '%s' "${@: -1}" > "$STUB_DIR/last-prompt.txt"
echo called >> "$STUB_DIR/calls.txt"
if read -t 1 _; then echo "data" > "$STUB_DIR/stdin.txt"; else echo "rc=$?" > "$STUB_DIR/stdin.txt"; fi

out=""; prev=""; is_resume=0; model=""
for a in "$@"; do
    [[ "$prev" == "-o" ]] && out="$a"
    [[ "$prev" == "--model" || "$prev" == "-m" ]] && model="$a"
    [[ "$a" == "resume" ]] && is_resume=1
    prev="$a"
done
printf '%s' "$model" > "$STUB_DIR/last-model.txt"
printf '%s' "$is_resume" > "$STUB_DIR/last-resume.txt"

start() { echo '{"type":"thread.started","thread_id":"stub-session"}'; echo '{"type":"turn.started"}'; }
msg() {
    [[ -n "$out" ]] && printf '%s\n' "$1" > "$out"
    python3 -c 'import json,sys; print(json.dumps({"type":"item.completed","item":{"type":"agent_message","text":sys.argv[1]}}, ensure_ascii=False))' "$1"
}
turn_done() { echo '{"type":"turn.completed","usage":{}}'; }
passing() { printf '审查结论：本轮通过，未发现显著问题。\n\nREVIEW-STATUS: PASS'; }

case "$mode" in
    ok)           start; msg "$(passing)"; turn_done ;;
    no-completed) start; msg "正在审查，先看" ;;
    no-message)   start; turn_done ;;
    not-verdict)  start; msg "需要我继续吗？"; turn_done ;;
    unfinished)   start; msg "$STUB_TEXT"; turn_done ;;
    fail)         start; echo "boom: transport reset" >&2; exit 1 ;;
    verdict-then-fail) start; msg "$(passing)"; echo "boom after answering" >&2; exit 1 ;;
    hang)         start; sleep 30 ;;
    ignore-term)  start; echo $$ > "$STUB_DIR/child.pid"; trap '' TERM; sleep 30 ;;
    child-survives) start
                  echo $$ > "$STUB_DIR/child.pid"
                  ( trap '' TERM; : > "$STUB_DIR/child-ready"; sleep 30 ) &
                  echo $! > "$STUB_DIR/grandchild.pid"
                  sleep 30 ;;
    late-session) trap '' TERM
                  : > "$STUB_DIR/late-ready"
                  ( while [[ ! -f "$STUB_DIR/late-go" ]]; do sleep 0.05; done
                    echo '{"type":"thread.started","thread_id":"late-session"}'
                    sleep 30 ) &
                  sleep 30 ;;
    events-only)  start; msg "$(passing)"; turn_done; : > "$out" ;;
    unclear)      start; msg "审查尚未完成，需要我继续吗？"; turn_done ;;
    plan)         start; msg "$(printf '方案可行，可以照着做。\n\nPLAN-STATUS: AGREED')"; turn_done ;;
    plan-fail)    start; echo "boom: the transport went away" >&2; exit 1 ;;
    resume-fail)  if [[ "$is_resume" == 1 ]]; then echo "session not found" >&2; exit 1; fi
                  start; msg "$(passing)"; turn_done ;;
esac
exit 0
STUB
chmod +x bin/codex
export STUB_DIR="$tmp"

failures=0

run() {
    local mode="$1"; shift
    set +e
    STUB_MODE="$mode" PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh "$@" >"$tmp/out.txt" 2>"$tmp/err.txt"
    echo $? > "$tmp/exit.txt"
    set -e
}

# Waits for a child to be ready *and* leaves behind a usable pid, reported through AS_PID. It must not
# be called in a command substitution: the failure would be recorded in the subshell and the error text
# would be captured as if it were a pid. So it returns a status, writes diagnostics to stderr, and the
# caller counts the failure in its own shell and skips the liveness check when AS_PID is empty.
# `tries` exists so this helper's own failure paths can be exercised quickly.
AS_PID=""
await_child() {
    local ready="$1" pidfile="$2" label="$3" tries="${4:-100}" i pid
    AS_PID=""
    for ((i = 0; i < tries; i++)); do
        if [[ -f "$ready" && -s "$pidfile" ]]; then break; fi
        sleep 0.1
    done
    if [[ ! -f "$ready" ]]; then
        echo "  FAIL $label: the child never signalled readiness" >&2
        return 1
    fi
    pid="$(cat "$pidfile" 2>/dev/null || true)"
    if [[ ! "$pid" =~ ^[0-9]+$ ]]; then
        echo "  FAIL $label: no usable child pid (got '${pid:-none}')" >&2
        return 1
    fi
    AS_PID="$pid"
    return 0
}

check() {
    local label="$1" expected="$2" needle="$3" file="$4" got
    got="$(cat "${exit_file:-$tmp/exit.txt}")"
    if [[ "$got" != "$expected" ]]; then
        echo "  FAIL $label: exit $got, wanted $expected"
        failures=$((failures + 1))
        return
    fi
    if [[ "$needle" != "-" ]] && ! grep -q -e "$needle" "$file"; then
        echo "  FAIL $label: '$needle' not found in $(basename "$file"); it said: $(head -c 200 "$file" | tr '\n' ' ')"
        failures=$((failures + 1))
        return
    fi
    echo "  ok   $label (exit $got)"
}

expect_value() {
    local label="$1" expected="$2" file="$3" got
    got="$(cat "$file" 2>/dev/null || true)"
    if [[ "$got" == "$expected" ]]; then
        echo "  ok   $label"
    else
        echo "  FAIL $label: got '$got', wanted '$expected'"
        failures=$((failures + 1))
    fi
}

expect() {
    local label="$1" needle="$2" file="$3"
    if [[ -f "$file" ]] && grep -q -e "$needle" "$file"; then
        echo "  ok   $label"
    else
        echo "  FAIL $label: '$needle' not found in $(basename "$file")"
        failures=$((failures + 1))
    fi
}

expect_absent() {
    local label="$1" needle="$2" file="$3"
    if grep -q -e "$needle" "$file"; then
        echo "  FAIL $label: '$needle' is present in $(basename "$file")"
        failures=$((failures + 1))
    else
        echo "  ok   $label"
    fi
}

echo "a successful round"
run ok
check "the review is produced" 0 - "$tmp/out.txt"
echo "  session: $(cat .agent-review/codex-session-id)  stdin: $(cat "$tmp/stdin.txt")"
check "the CLI cannot read stdin" 0 "rc=1" "$tmp/stdin.txt"

echo "the model the reviewer runs on"
expect_value "the first review asks for gpt-6.1-sol" "gpt-6.1-sol" "$tmp/last-model.txt"
expect_value "and it really was a first review" "0" "$tmp/last-resume.txt"
echo "a run that exited 0 without finishing its turn"
run no-completed
check "it is refused" 1 "turn.completed" "$tmp/err.txt"

echo "a completed turn with no final message"
run no-message
check "it is refused" 1 "no final message" "$tmp/err.txt"

echo "a completed turn whose message is not a verdict"
run not-verdict
check "it is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"

echo "a message that merely mentions review"
run unclear
check "it is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"

echo "a message only in the event stream, with no final-message file"
run events-only
check "it is refused" 1 "no final message" "$tmp/err.txt"
expect "the stream message is shown as evidence" "not the CLI's final message" "$tmp/err.txt"
expect "the stream message itself is shown" "审查结论" "$tmp/err.txt"

echo "messages that say the review has not finished"
counter=0
for text in "无法确认是否存在问题，需要继续审查。" \
            "我还没有检查并发问题，需要继续吗？" \
            "请先确认测试通过后，我再继续审查。"; do
    counter=$((counter + 1))
    STUB_TEXT="$text" run unfinished
    check "counter-example $counter is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
done

echo "status lines that are not the conclusion"
STUB_TEXT="$(printf '```\nREVIEW-STATUS: PASS\n```\n审查尚未完成，需要我继续吗？')" run unfinished
check "a status line inside a code block, with prose after it, is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf 'REVIEW-STATUS: PASS 或 FINDINGS\n我还没有完成审查。')" run unfinished
check "a quoted format line is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '审查结论：本轮通过，未发现显著问题。\n\n**REVIEW-STATUS: PASS**')" run unfinished
check "a decorated but genuine status line is accepted" 0 - "$tmp/out.txt"

echo "git cannot say whether the tree changed"
# "Cannot check" must not read as "the precondition holds": the round is refused and the reviewer is never
# called. GIT_DIR pointing at nothing is a git failure that no repository state can mask.
calls_before_git_failure="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
set +e
GIT_DIR="$tmp/no-such-git-dir" STUB_MODE=ok PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >"$tmp/out.txt" 2>"$tmp/err.txt"
echo $? > "$tmp/exit.txt"
set -e
check "a git failure refuses the round" 66 "git failed" "$tmp/err.txt"
calls_after_git_failure="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
if [[ "$calls_before_git_failure" == "$calls_after_git_failure" ]]; then
    echo "  ok   the reviewer was not called"
else
    echo "  FAIL the reviewer was called $((calls_after_git_failure - calls_before_git_failure)) time(s) even though the tree could not be checked"
    failures=$((failures + 1))
fi

echo "a retry still needs a readable repository"
# Run in its own repository: corrupting the index of the harness's own repo would break every case after it.
# The root query succeeds there; the diff read is what fails. A retry is exempt from "there is nothing to
# review", never from "the repository cannot be read".
badrepo="$scratch/unreadable-repo"
mkdir -p "$badrepo/scripts" "$badrepo/bin"
cp "$helper" "$badrepo/scripts/"
cp -R "$tmp/bin/." "$badrepo/bin/"
cp -R .agent-review "$badrepo/" 2>/dev/null || true
(
    cd "$badrepo"
    git init -q .
    git config user.email b@c.i
    git config user.name b
    printf 'x\n' > f.txt
    git add -A >/dev/null 2>&1
    git commit -q -m base || true
    printf 'y\n' > f.txt
    # An interrupted round first, so the retry is a legitimate one and reaches the repository check. Its own
    # reviewer call is legitimate, so the baseline for "was the reviewer called" is taken after it.
    STUB_MODE=unfinished PATH="$badrepo/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 || true
    wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' > "$scratch/calls-before.txt" || echo 0 > "$scratch/calls-before.txt"
    printf 'not an index' > .git/index
    set +e
    STUB_MODE=ok PATH="$badrepo/bin:$PATH" ./scripts/codex-review.sh --retry >"$scratch/out.txt" 2>"$scratch/err.txt"
    echo $? > "$scratch/exit.txt"
    set -e
)
calls_before_unreadable="$(cat "$scratch/calls-before.txt" 2>/dev/null || echo 0)"
calls_after_unreadable="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
exit_file="$scratch/exit.txt"
check "a retry with an unreadable repository is refused" 66 "git failed" "$scratch/err.txt"
exit_file="$tmp/exit.txt"
if [[ "$calls_before_unreadable" == "$calls_after_unreadable" ]]; then
    echo "  ok   the reviewer was not called ($calls_after_unreadable call(s) so far)"
else
    echo "  FAIL the reviewer was called $((calls_after_unreadable - calls_before_unreadable)) time(s) on a repository it could not read"
    failures=$((failures + 1))
fi

echo "the call counter can tell that the reviewer ran"
# Without this, "the counter did not move" could also mean "the counter never moves".
calls_before_control="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
: > counter-control.txt
run ok
check "the control round runs" 0 - "$tmp/out.txt"
calls_after_control="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
if [[ "$calls_after_control" -gt "$calls_before_control" ]]; then
    echo "  ok   a real call moves the counter ($calls_before_control -> $calls_after_control)"
else
    echo "  FAIL a real call did not move the counter, so 'not called' proves nothing"
    failures=$((failures + 1))
fi

echo "the verdict must be the last line, whole"
# The helper used to skip a closing code fence, so a message that ended with a *format example* was read as a
# verdict -- the round this check exists to refuse.
STUB_TEXT="$(printf '审查尚未完成，以下只是格式示例：\n\n```\nREVIEW-STATUS: PASS\n```')" run unfinished
check "a fenced format example as the last line is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '本轮未完成。\n\nREVIEW-STATUS: pass')" run unfinished
check "a lowercase status token is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '本轮未完成。\n\nREVIEW-STATUS: PASS.')" run unfinished
check "a trailing period is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '本轮未完成。\n\n**REVIEW-STATUS: PASS')" run unfinished
check "an unpaired decoration is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '本轮通过。\n\n`REVIEW-STATUS: PASS`')" run unfinished
check "a backtick-paired status line is accepted" 0 - "$tmp/out.txt"

echo "a timeout written with a leading zero"
# `08` is not octal for a human, but bash arithmetic reads it as one: the comparison failed and the watchdog
# was never armed, so a hung run held the lock for ever.
run hang --timeout 08
check "the watchdog still ends it" 124 "did not finish within 8s" "$tmp/err.txt"

echo "an older round's streamed message is not this round's evidence"
# The stream fallback file was not cleared, so a failed round could print the previous round's message as its
# own evidence.
run events-only
check "the stream-only round is refused" 1 "no final message" "$tmp/err.txt"
if [[ -f .agent-review/last-review.jsonl.stream-message ]]; then
    run fail
    if grep -q "not the CLI's final message" "$tmp/err.txt"; then
        echo "  FAIL a stale streamed message was shown as this round's evidence"
        failures=$((failures + 1))
    else
        echo "  ok   the previous round's streamed message is not reused"
    fi
else
    echo "  FAIL no streamed message was written, so the reuse check could not run"
    failures=$((failures + 1))
fi

echo "a timeout too large for bash arithmetic"
# 0x10000000000000000 wraps to 0 in a 64-bit shell, which used to turn the watchdog off instead of refusing.
run hang --timeout 1000000000
check "an oversized timeout is refused" 64 "0 to 999999999" "$tmp/err.txt"

echo "a CLI that exits non-zero"
run fail
check "it is refused" 1 "exited 1" "$tmp/err.txt"
expect "the session is kept" "stub-session" .agent-review/codex-session-id

echo "a run that hangs"
run hang --timeout 2
check "the watchdog ends it" 124 "did not finish within 2s" "$tmp/err.txt"
expect "the session is kept" "stub-session" .agent-review/codex-session-id

echo "the --retry framing"
run ok --retry
check "the retry succeeds" 0 - "$tmp/out.txt"
expect "it says the run was interrupted" "You were interrupted" "$tmp/last-prompt.txt"
expect_value "the resumed review keeps the model" "gpt-6.1-sol" "$tmp/last-model.txt"
expect_value "and it really was a resume" "1" "$tmp/last-resume.txt"
expect_absent "it does not claim the code changed" "has updated the code" "$tmp/last-prompt.txt"

echo "a resume that fails"
run resume-fail
check "it is refused" 1 "exited 1" "$tmp/err.txt"
expect "it points at --fresh" "--fresh" "$tmp/err.txt"

echo "--fresh after a lost session"
run ok --fresh
check "the fresh review succeeds" 0 - "$tmp/out.txt"
expect "it warns that the memory is lost" "memory of earlier findings is gone" "$tmp/err.txt"

echo "the lock"
mkdir -p .agent-review/lock
run ok
check "a second review is refused" 75 "already running" "$tmp/err.txt"
run ok --finish
check "--finish respects the lock" 75 "cannot be cleared" "$tmp/err.txt"
rmdir .agent-review/lock

echo "a signal sent to the script alone, while the CLI ignores TERM"
rm -f "$tmp/child.pid" "$tmp/script-exit.txt"
STUB_MODE=ignore-term PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 &
script_pid=$!
for _ in $(seq 1 50); do [[ -s "$tmp/child.pid" ]] && break; sleep 0.1; done
child_pid="$(cat "$tmp/child.pid" 2>/dev/null || echo none)"
before=$(ls .agent-review/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d ' ')
kill -TERM "$script_pid" 2>/dev/null
set +e
wait "$script_pid"
echo $? > "$tmp/script-exit.txt"
set -e
echo "  exit $(cat "$tmp/script-exit.txt") (want 143), CLI pid was $child_pid"
if [[ "$(cat "$tmp/script-exit.txt")" == "143" ]]; then echo "  ok   the script reports the interruption"; else echo "  FAIL exit $(cat "$tmp/script-exit.txt")"; failures=$((failures+1)); fi
if kill -0 "$child_pid" 2>/dev/null; then echo "  FAIL the CLI is still alive"; failures=$((failures+1)); else echo "  ok   the CLI was stopped too"; fi
if [[ -d .agent-review/lock ]]; then echo "  FAIL the lock is still held"; failures=$((failures+1)); else echo "  ok   the lock was released"; fi
after=$(ls .agent-review/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d ' ')
if [[ "$after" -gt "$before" ]]; then echo "  ok   the interrupted round was archived"; else echo "  FAIL nothing archived"; failures=$((failures+1)); fi

echo "a group whose leader yields but whose child ignores TERM"
rm -f "$tmp/child.pid" "$tmp/grandchild.pid" "$tmp/child-ready"
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 &
script_pid=$!
grandchild=""
await_child "$tmp/child-ready" "$tmp/grandchild.pid" "the signal case handshake" || failures=$((failures + 1))
grandchild="$AS_PID"
kill -TERM "$script_pid" 2>/dev/null
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
check "the interruption is reported" 143 - "$tmp/exit.txt"
if [[ -z "$grandchild" ]]; then
    echo "  ok   (no usable pid, so the liveness check is skipped; the handshake failure above counts)"
elif kill -0 "$grandchild" 2>/dev/null; then
    echo "  FAIL the child that ignored TERM is still alive"
    failures=$((failures + 1))
else
    echo "  ok   the child that ignored TERM was killed with the group"
fi

echo "the same, on the timeout path"
rm -f "$tmp/child.pid" "$tmp/grandchild.pid" "$tmp/child-ready"
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh --timeout 2 >/dev/null 2>"$tmp/err.txt" &
script_pid=$!
grandchild=""
await_child "$tmp/child-ready" "$tmp/grandchild.pid" "the timeout case handshake" || failures=$((failures + 1))
grandchild="$AS_PID"
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
check "the timeout is reported" 124 "did not finish within 2s" "$tmp/err.txt"
if [[ -z "$grandchild" ]]; then
    echo "  ok   (no usable pid, so the liveness check is skipped; the handshake failure above counts)"
elif kill -0 "$grandchild" 2>/dev/null; then
    echo "  FAIL the child that ignored TERM survived the timeout"
    failures=$((failures + 1))
else
    echo "  ok   the timeout killed the whole group"
fi

echo "a second signal during the cleanup"
rm -f "$tmp/child.pid" "$tmp/grandchild.pid" "$tmp/child-ready"
# Job control for the launch, or the backgrounded script inherits SIGINT as ignored (a non-interactive
# shell's asynchronous children do) and the second signal below would be a no-op — which is exactly how
# an earlier version of this case passed without testing anything.
set -m
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 &
script_pid=$!
set +m
grandchild=""
await_child "$tmp/child-ready" "$tmp/grandchild.pid" "the cross-signal handshake" || failures=$((failures + 1))
grandchild="$AS_PID"
before=$(ls .agent-review/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d ' ')
kill -TERM "$script_pid" 2>/dev/null
sleep 0.3
kill -INT "$script_pid" 2>/dev/null
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
check "the first signal decides the exit" 143 - "$tmp/exit.txt"
if [[ -z "$grandchild" ]]; then
    echo "  ok   (no usable pid, so the liveness check is skipped; the handshake failure above counts)"
elif kill -0 "$grandchild" 2>/dev/null; then
    echo "  FAIL the CLI outlived the cleanup"
    failures=$((failures + 1))
else
    echo "  ok   the CLI was still stopped"
fi
if [[ -d .agent-review/lock ]]; then
    echo "  FAIL the lock was left behind"
    failures=$((failures + 1))
else
    echo "  ok   the lock was released"
fi
after=$(ls .agent-review/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d ' ')
if [[ "$after" -gt "$before" ]]; then
    echo "  ok   the round was archived despite the second signal"
else
    echo "  FAIL the second signal skipped the archive"
    failures=$((failures + 1))
fi

echo "the model override"
OMT_REVIEW_MODEL=another-model run ok
check "the run with another model is produced" 0 - "$tmp/out.txt"
expect_value "OMT_REVIEW_MODEL is passed through" "another-model" "$tmp/last-model.txt"

echo "the handshake helper's own failure paths"
rm -f "$tmp/nope-ready" "$tmp/bad.pid"
: > "$tmp/bad.pid"; echo "not-a-pid" > "$tmp/bad.pid"
if await_child "$tmp/nope-ready" "$tmp/bad.pid" "missing readiness" 2 2>/dev/null; then
    echo "  FAIL a missing readiness file was accepted"
    failures=$((failures + 1))
else
    echo "  ok   a missing readiness file is refused"
fi
: > "$tmp/ok-ready"
if await_child "$tmp/ok-ready" "$tmp/bad.pid" "non-numeric pid" 2 2>/dev/null; then
    echo "  FAIL a non-numeric pid was accepted"
    failures=$((failures + 1))
else
    echo "  ok   a non-numeric pid is refused"
fi
if await_child "$tmp/ok-ready" "$tmp/grandchild.pid" "a real pid" 2 2>/dev/null; then
    echo "  ok   a real pid is accepted"
else
    echo "  FAIL a real pid was refused"
    failures=$((failures + 1))
fi

echo "a session created while the CLI was being stopped"
rm -f "$tmp/child.pid" "$tmp/late-ready" "$tmp/late-go" .agent-review/codex-session-id
STUB_MODE=late-session PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 &
script_pid=$!
for _ in $(seq 1 100); do [[ -f "$tmp/late-ready" ]] && break; sleep 0.1; done
kill -TERM "$script_pid" 2>/dev/null
: > "$tmp/late-go"
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
check "the interruption is reported" 143 - "$tmp/exit.txt"
if [[ "$(cat .agent-review/codex-session-id 2>/dev/null)" == "late-session" ]]; then
    echo "  ok   the session created during the grace period was kept"
else
    echo "  FAIL the late session was lost: $(cat .agent-review/codex-session-id 2>/dev/null || echo none)"
    failures=$((failures + 1))
fi

echo "a timeout against a CLI that ignores TERM"
rm -f "$tmp/child.pid"
STUB_MODE=ignore-term PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh --timeout 2 >/dev/null 2>"$tmp/err.txt" &
script_pid=$!
for _ in $(seq 1 50); do [[ -s "$tmp/child.pid" ]] && break; sleep 0.1; done
child_pid="$(cat "$tmp/child.pid" 2>/dev/null || echo none)"
started=$SECONDS
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
elapsed=$((SECONDS - started))
check "the watchdog ends it" 124 "did not finish within 2s" "$tmp/err.txt"
echo "  it took ${elapsed}s after the timeout to come back (want < 15)"
if [[ "$elapsed" -lt 15 ]]; then echo "  ok   the termination is bounded"; else echo "  FAIL it held on for ${elapsed}s"; failures=$((failures+1)); fi
if kill -0 "$child_pid" 2>/dev/null; then echo "  FAIL the CLI survived KILL"; failures=$((failures+1)); else echo "  ok   the CLI was killed"; fi

echo "a captured stdout must reach EOF when the script returns"
started=$SECONDS
set +e
STUB_MODE=ok PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh --timeout 60 2>/dev/null | cat >/dev/null
set -e
elapsed=$((SECONDS - started))
echo "  the pipeline closed after ${elapsed}s (want < 20, not the 60s timeout)"
if [[ "$elapsed" -lt 20 ]]; then echo "  ok   no process keeps the pipe open"; else echo "  FAIL the pipe was held for ${elapsed}s"; failures=$((failures+1)); fi

echo "the archive"
echo "  outcomes: $(ls .agent-review/rounds/*-outcome.txt | wc -l | tr -d ' ')  events: $(ls .agent-review/rounds/*-events.jsonl | wc -l | tr -d ' ')  reviews: $(ls .agent-review/rounds/*-review.txt | wc -l | tr -d ' ')  stderr: $(ls .agent-review/rounds/*-stderr.txt | wc -l | tr -d ' ')"
if grep -q -e "turn.completed" .agent-review/rounds/*-outcome.txt; then
    echo "  ok   the failed round's reason is archived"
else
    echo "  FAIL the failed round's reason is not archived"
    failures=$((failures + 1))
fi
if grep -q -e "boom: transport reset" .agent-review/rounds/*-stderr.txt; then
    echo "  ok   the transport error text is archived, not left to be overwritten"
else
    echo "  FAIL the CLI's stderr was not archived"
    failures=$((failures + 1))
fi
if grep -q -e "interrupted (signal" .agent-review/rounds/*-outcome.txt; then
    echo "  ok   the interrupted round says so"
else
    echo "  FAIL the interruption was not recorded"
    failures=$((failures + 1))
fi

echo "a relative --note-file"
echo "备注内容：只审脚本。" > scripts/note.txt
set +e
(cd scripts && STUB_MODE=ok PATH="$tmp/bin:$PATH" ./codex-review.sh --note-file ./note.txt >/dev/null 2>&1)
echo $? > "$tmp/exit.txt"
set -e
check "it is read from the caller's directory" 0 - "$tmp/out.txt"
expect "the note reaches the prompt" "只审脚本" "$tmp/last-prompt.txt"

echo "a tree with nothing to review"
# From here on the reviewed tree must stay clean, so the output goes to $scratch.
run_clean() {
    set +e
    STUB_MODE="${STUB_MODE:-ok}" PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh "$@" >"$scratch/out.txt" 2>"$scratch/err.txt"
    echo $? > "$scratch/exit.txt"
    set -e
}
exit_file="$scratch/exit.txt"
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline"
run_clean
check "a clean tree is refused" 65 "Nothing to review" "$scratch/err.txt"

echo "a tree whose final content matches HEAD, but whose index does not"
# `git status --porcelain` reports `AD` here, so the status-based check let this through; the final diff is
# empty and there is nothing for a reviewer to look at.
: > "$scratch/placeholder-ad" 2>/dev/null || true
printf 'x\n' > "$tmp/ad-file.txt"
git add "$tmp/ad-file.txt" >/dev/null 2>&1
rm -f "$tmp/ad-file.txt"
status_before="$(git status --porcelain --untracked-files=normal)"
final_diff="$(git diff HEAD --no-color)"
run_clean
if [[ -n "$status_before" && -z "$final_diff" ]]; then
    check "a staged-then-deleted file is refused as no change" 65 "Nothing to review" "$scratch/err.txt"
else
    # A counterexample that was never set up is not a passing counterexample.
    echo "  FAIL could not stage an AD state (status='${status_before//$'\n'/ }' diff='${final_diff:0:20}')"
    failures=$((failures + 1))
fi
git reset -q HEAD "$tmp/ad-file.txt" >/dev/null 2>&1 || true
run_clean --finish
check "--finish still works on a clean tree" 0 - "$scratch/out.txt"

echo "a design round is asked before there is anything to review"
STUB_MODE=plan run_clean --design
check "the design round is produced" 0 - "$scratch/out.txt"
expect "it is kept as a plan" "PLAN-STATUS: AGREED" .agent-review/last-plan.txt
expect "the design prompt forbids writing the code" "Do NOT write the code" "$tmp/last-prompt.txt"
expect_absent "it does not claim the code changed" "has updated the code" "$tmp/last-prompt.txt"

echo "a design round cannot be answered with a review verdict"
STUB_MODE=ok run_clean --design
check "it is refused" 1 "PLAN-STATUS" "$scratch/err.txt"

echo "--finish takes no round of its own"
STUB_MODE=plan run_clean --finish --design
check "it is refused" 64 - "$scratch/err.txt"

echo "a design round that died is retried as a design round"
STUB_MODE=plan-fail run_clean --design
check "the design round fails" 1 "design round produced no answer" "$scratch/err.txt"
expect_value "the round kind is kept for the retry" "plan" .agent-review/last-round-kind
STUB_MODE=ok run_clean --retry
check "a review verdict cannot answer the retried design round" 1 "PLAN-STATUS" "$scratch/err.txt"
STUB_MODE=plan run_clean --retry
check "the retried design round is answered as one" 0 - "$scratch/out.txt"
expect "and kept as a plan" "PLAN-STATUS: AGREED" .agent-review/last-plan.txt

echo "a refused call does not rewrite what the last round was"
git add -A >/dev/null 2>&1
# A baseline marker, not an assertion: the round files may be byte-identical to the previous commit, and
# `run_clean` leaves errexit on, so a failing commit would end the whole self-test silently.
git commit -q -m "self-test baseline before the refused call" || true
STUB_MODE=ok run_clean
check "the call is refused for having nothing to review" 65 "Nothing to review" "$scratch/err.txt"
expect_value "the recorded kind is untouched" "plan" .agent-review/last-round-kind
# A retry resumes a round that produced no verdict. That design round already answered (PLAN-STATUS), so a
# retry here is refused -- and the refusal must leave the recorded kind alone, which is what this case is for.
STUB_MODE=ok run_clean --retry
check "a retry after a concluded round is refused" 64 "the last round reached one" "$scratch/err.txt"
expect_value "and the recorded kind is still untouched" "plan" .agent-review/last-round-kind

echo "a lost kind record is refused, not guessed"
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline before the unknown kind" || true
# A retry only resumes a round that produced no verdict, so the state it resumes from has to be one: a round
# that answered would be refused for a different (and correct) reason and never reach the kind record. The round
# needs a change to look at, or the no-change check refuses it before it can produce anything.
: > interrupted.txt
STUB_MODE=unfinished run_clean
check "the interrupted round produced no verdict" 1 "produced no review" "$scratch/err.txt"
rm -f .agent-review/last-round-kind
STUB_MODE=ok run_clean --retry
check "the retry is refused" 64 "Nothing on record" "$scratch/err.txt"

echo "a verdict-shaped message from a failed run is not a concluded round"
# `concluded` is the round's outcome, not the shape of its message: this run prints a valid status line and then
# dies, so it must stay retryable instead of being refused as "the last round reached one".
: > verdict-shaped.txt
STUB_MODE=verdict-then-fail run_clean
check "the round is reported as failed" 1 "exited 1" "$scratch/err.txt"
expect "and recorded as not concluded" "concluded=no" .agent-review/last-status.txt
STUB_MODE=ok run_clean --retry
check "so it can be retried" 0 - "$scratch/out.txt"

echo "an interrupted round does not inherit the previous round's verdict"
# The status record belongs to the round that starts: after a concluded round, an interrupted new round must
# still be retryable.
STUB_MODE=ok run_clean
check "the first round concludes" 0 - "$scratch/out.txt"
: > interrupted-after-success.txt
STUB_MODE=no-completed run_clean
check "the second round produces no conclusion" 1 "produced no review" "$scratch/err.txt"
STUB_MODE=ok run_clean --retry
check "and it is retryable, not refused with the first round's verdict" 0 - "$scratch/out.txt"

echo "a retry after a round that already concluded"
# `--retry` means "resume a round that produced no verdict". Allowing it after a verdict both lies to the
# reviewer (the round was interrupted) and bypasses the no-change refusal on a clean tree.
STUB_MODE=ok run_clean
check "the round concludes" 0 - "$scratch/out.txt"
run_clean --retry
check "a retry after a concluded round is refused" 64 "the last round reached one" "$scratch/err.txt"
run_clean --fresh
check "--fresh still starts a new round" 0 - "$scratch/out.txt"
STUB_MODE=unfinished run_clean --design
check "an interrupted design round produces no verdict" 1 "produced no review" "$scratch/err.txt"
STUB_MODE=plan run_clean --retry --design
check "saying --design resumes it safely" 0 - "$scratch/out.txt"

echo "an archive left by an older task cannot answer for this one"
# The archive keeps rounds from finished tasks; a retry must not read a kind out of it.
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline before the finished task"
: > finished-task-change.txt
STUB_MODE=ok run_clean
check "a review runs with a change in the tree" 0 - "$scratch/out.txt"
expect_value "its kind is recorded for its own session" "review" .agent-review/last-round-kind
run_clean --finish
check "--finish clears the session" 0 - "$scratch/out.txt"
expect_value "and the kind with it" "" .agent-review/last-round-kind
expect_value "and any verdict record with it" "" .agent-review/last-status.txt
STUB_MODE=ok run_clean --retry
check "a retry then has nothing on record to resume" 64 "Nothing on record" "$scratch/err.txt"

echo "a retry cannot change the kind of round it resumes"
: > switched-kind.txt
STUB_MODE=unfinished run_clean
check "the review produced no verdict" 1 "produced no review" "$scratch/err.txt"
expect_value "so it is recorded as a review round" "review" .agent-review/last-round-kind
STUB_MODE=plan run_clean --retry --design
check "switching a review retry into a design round is refused" 64 "cannot change the kind" "$scratch/err.txt"

echo "an untracked file is a change"
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline again"
: > untracked-change.txt
run_clean
check "an untracked file is enough to review" 0 - "$scratch/out.txt"

echo "a repository with no commit yet"
# The no-HEAD branch asks the same question as the HEAD path. Concatenating the index and worktree diffs made a
# staged-then-deleted file look like a change; the worktree is compared with the empty tree instead. The helper
# and the stub live outside the repository here, and its own state directory is excluded through
# .git/info/exclude, so the tree really has nothing to review.
norepo="$scratch/no-commit-repo"
mkdir -p "$norepo"
(
    cd "$norepo"
    git init -q .
    git config user.email n@c.i
    git config user.name n
    printf '.agent-review/\n' > .git/info/exclude
    printf 'x\n' > staged.txt
    git add staged.txt
    rm -f staged.txt
    set +e
    STUB_MODE=ok PATH="$tmp/bin:$PATH" "$tmp/scripts/codex-review.sh" >"$scratch/out.txt" 2>"$scratch/err.txt"
    echo $? > "$scratch/exit.txt"
    set -e
)
check "a staged-then-deleted file in an uncommitted repository is refused" 65 "Nothing to review" "$scratch/err.txt"

echo "--retry is exempt from the pre-check"
# It resumes a round that produced no verdict, and that is also the case where the tree legitimately has not
# changed since: the first half here records the interrupted round, the second retries it on a clean tree.
: > interrupted-again.txt
STUB_MODE=unfinished run_clean
check "the interrupted review produced no verdict" 1 "produced no review" "$scratch/err.txt"
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline once more" || true
run_clean --retry
check "a retry runs even on a clean tree" 0 - "$scratch/out.txt"

echo
if [[ "$failures" -eq 0 ]]; then
    echo "self-test passed"
    exit 0
fi
echo "self-test failed: $failures case(s)"
exit 1
