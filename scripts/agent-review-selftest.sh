#!/usr/bin/env bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed
# uses this mapping to select it; a selftest without one is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/agent-review.sh,scripts/agent-review-selftest.sh
#
# Self-test for scripts/agent-review.sh.
#
# The helper is run against a stub `codex` and a stub `pi` in a throwaway git repository, so no reviewer
# session and no network are involved. It covers the failure modes the helper exists to classify and the
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
#   scripts/agent-review-selftest.sh
#
set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
helper="$script_dir/agent-review.sh"

[[ -f "$helper" ]] || { echo "cannot find $helper" >&2; exit 66; }

# The instrument controls its own environment. A task key the caller exported — a harness that says it once,
# as the helper's documentation suggests — would move every keyless case's state into that task's directory
# while the assertions still read the top-level paths, and the gate would fail for a reason that belongs to
# the caller's shell. The same holds for a model or speed override. The cases that test any of those
# variables set it on the call itself.
unset OMT_REVIEW_TASK OMT_REVIEW_MODEL OMT_REVIEW_FAST OMT_REVIEW_REASONING_EFFORT \
    OMT_REVIEW_REVIEWER OMT_REVIEW_PI_PROVIDER OMT_REVIEW_PI_MODEL OMT_REVIEW_PI_THINKING

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
prompt="${@: -1}"
printf '%s' "$prompt" > "$STUB_DIR/last-prompt.txt"
# The session id carries the task key the prompt named, so a round that resumed another task's
# session shows up in the assertion instead of looking like the right one. Read from the prompt the
# way the helper wrote it; a keyless round names no task.
task_key="$(printf '%s' "$prompt" | sed -n "s/.*belongs to task '\\([a-z0-9._-]*\\)'.*/\\1/p" | head -1)"
echo called >> "$STUB_DIR/calls.txt"
if read -t 1 _; then echo "data" > "$STUB_DIR/stdin.txt"; else echo "rc=$?" > "$STUB_DIR/stdin.txt"; fi

out=""; prev=""; is_resume=0; model=""; resume_id=""; tier=""; reasoning=""
for a in "$@"; do
    [[ "$prev" == "-o" ]] && out="$a"
    [[ "$prev" == "--model" || "$prev" == "-m" ]] && model="$a"
    [[ "$a" == "resume" ]] && is_resume=1
    [[ "$prev" == "resume" ]] && resume_id="$a"
    # The helper passes the speed as `-c service_tier=<tier>` and the thinking level as
    # `-c model_reasoning_effort=<level>`; recorded so a case can assert that both reach the CLI on
    # the first round *and* on a resume, which is where a per-turn setting is otherwise lost.
    [[ "$prev" == "-c" && "$a" == service_tier=* ]] && tier="${a#service_tier=}"
    [[ "$prev" == "-c" && "$a" == model_reasoning_effort=* ]] && reasoning="${a#model_reasoning_effort=}"
    prev="$a"
done
printf '%s' "$model" > "$STUB_DIR/last-model.txt"
printf '%s' "$tier" > "$STUB_DIR/last-tier.txt"
printf '%s' "$reasoning" > "$STUB_DIR/last-reasoning.txt"
printf '%s' "$is_resume" > "$STUB_DIR/last-resume.txt"
# The id it was actually told to resume, not only that it was a resume: the thread id below is built from
# the prompt, so a round that resumed another task's session would otherwise look like the right one.
printf '%s' "$resume_id" > "$STUB_DIR/last-resume-id.txt"

start() {
    if [[ -n "$task_key" ]]; then
        echo "{\"type\":\"thread.started\",\"thread_id\":\"stub-$task_key\"}"
    else
        echo '{"type":"thread.started","thread_id":"stub-session"}'
    fi
    echo '{"type":"turn.started"}'
}
# The real CLI's wording, verbatim, when it will not use the requested service tier: it reports an error
# item and answers at the default speed rather than failing the run. The helper keys its warning and its
# archived `service_tier_rejected` off this sentence. The tier named is the one actually requested, so a
# detection that matched a hard-coded value instead of the round's own would not be flattered by it.
tier_rejected_item() {
    python3 -c 'import json,sys; print(json.dumps({"type":"item.completed","item":{"type":"error","message":"Configured service tier `%s` is not advertised as supported for model `gpt-6.1-sol` and will be omitted from requests." % (sys.argv[1] or "none")}}, ensure_ascii=False))' "${tier:-}"
}
# The real CLI's wording when it will not use the requested reasoning effort: the same shape as the
# tier rejection, naming the level actually requested so a hard-coded match would not be flattered.
reasoning_rejected_item() {
    python3 -c 'import json,sys; print(json.dumps({"type":"item.completed","item":{"type":"error","message":"Configured reasoning effort `%s` is not advertised as supported for model `gpt-6.1-sol` and will be omitted from requests." % (sys.argv[1] or "none")}}, ensure_ascii=False))' "${reasoning:-}"
}
# An unrelated error item: the tier warning must not fire on every error the CLI prints.
unrelated_error_item() {
    python3 -c 'import json; print(json.dumps({"type":"item.completed","item":{"type":"error","message":"stream disconnected before completion"}}, ensure_ascii=False))'
}
# The sharper counter-example for both warnings: the *same* generic phrase about a setting the helper
# does not classify. A match that only looked for "not advertised as supported for model" would
# archive a rejection here.
other_setting_rejected_item() {
    python3 -c 'import json; print(json.dumps({"type":"item.completed","item":{"type":"error","message":"Configured verbosity `loud` is not advertised as supported for model `gpt-6.1-sol` and will be omitted from requests."}}, ensure_ascii=False))'
}
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
    # The real CLI's wording when the thread it was told to resume no longer exists. The helper keys
    # the `lost` event off exactly this sentence, so the stub has to say it verbatim: a paraphrasing
    # stub would let a detection that no longer matches the CLI pass the gate.
    thread-gone)  if [[ "$is_resume" == 1 ]]; then
                      echo "Error: thread/resume: thread/resume failed: no rollout found for thread id $resume_id (code -32600)" >&2
                      exit 1
                  fi
                  start; msg "$(passing)"; turn_done ;;
    # A CLI that dies before it reports a thread.started: the attempt produces events and even a
    # verdict, but no session. Nothing about the round may be recorded as a session existing.
    no-session)   echo '{"type":"turn.started"}'; msg "$(passing)"; turn_done ;;
    # A completed, valid review whose requested speed codex declined: the round still passes, and the
    # warning plus the archived flag are the only evidence that fast mode did not take effect.
    tier-rejected) start; tier_rejected_item; msg "$(passing)"; turn_done ;;
    # A completed, valid review whose requested reasoning effort codex declined.
    reasoning-rejected) start; reasoning_rejected_item; msg "$(passing)"; turn_done ;;
    # The counter-example for that warning: an error item that is not about the service tier.
    error-item)   start; unrelated_error_item; msg "$(passing)"; turn_done ;;
    # The sharper counter-example: the same generic phrase about a *different* setting, which must not be
    # archived as a service-tier rejection.
    other-setting) start; other_setting_rejected_item; msg "$(passing)"; turn_done ;;
    # The shape that made the archive grow without bound: a completed turn whose transcript carries the
    # output of a large command, reported once when the tool ran it and once when it finished.
    big-output)   start
                  python3 -c 'import json; chunk="x"*200000; print(json.dumps({"type":"item.completed","item":{"type":"command_execution","id":"c1","command":"cat big.txt","aggregated_output":chunk,"exit_code":0}}))'
                  msg "$(passing)"; turn_done ;;
    # A failed command: the archive has to say it failed, and keep a bounded tail of what it printed.
    failed-command) start
                  python3 -c 'import json; print(json.dumps({"type":"item.completed","item":{"type":"command_execution","id":"c2","command":"cargo test","status":"failed","exit_code":101,"aggregated_output":"test result: FAILED. 3 passed; 1 failed\n" + "y"*50000}}))'
                  msg "$(passing)"; turn_done ;;
    # The round's answer exists only in its stream and the -o file it was given is empty: the message is
    # still this round's and must be archived, not silently replaced by codex's empty -o result.
    empty-output) start; msg "$(passing)"; turn_done; : > "$out" ;;
esac
exit 0
STUB
chmod +x bin/codex
cat > bin/pi <<'STUB'
#!/bin/bash
# A stand-in for the pi CLI: emits pi's JSON-mode records for the modes the helper classifies,
# records the flags it was passed, and writes the prompt where the assertions read it.
mode="${STUB_MODE:-ok}"
prev=""; provider=""; model=""; thinking=""; tools=""; session_arg=""
for a in "$@"; do
    case "$prev" in
        --provider)    provider="$a" ;;
        --model)       model="$a" ;;
        --thinking)    thinking="$a" ;;
        --tools)       tools="$a" ;;
        --session-id)  session_arg="$a" ;;
    esac
    prev="$a"
done
prompt="${@: -1}"
printf '%s' "$provider" > "$STUB_DIR/pi-provider.txt"
printf '%s' "$model" > "$STUB_DIR/pi-model.txt"
printf '%s' "$thinking" > "$STUB_DIR/pi-thinking.txt"
printf '%s' "$tools" > "$STUB_DIR/pi-tools.txt"
printf '%s' "$session_arg" > "$STUB_DIR/pi-session-arg.txt"
printf '%s' "$prompt" > "$STUB_DIR/last-prompt.txt"
echo called >> "$STUB_DIR/calls.txt"
# The session id is derived from the task key the prompt named, so a round that resumed another
# task's session shows up in the assertion instead of looking like the right one.
task_key="$(printf '%s' "$prompt" | sed -n "s/.*belongs to task '\([a-z0-9._-]*\)'.*/\1/p" | head -1)"
if [[ -n "$session_arg" ]]; then
    sid="$session_arg"
elif [[ -n "$task_key" ]]; then
    sid="pi-$task_key"
else
    sid="pi-session"
fi
echo "{\"type\":\"session\",\"version\":3,\"id\":\"$sid\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"$PWD\"}"
echo '{"type":"agent_start"}'
echo '{"type":"turn_start"}'
msg() {
    python3 -c 'import json,sys; print(json.dumps({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":sys.argv[1]}],"stopReason":"stop"}}, ensure_ascii=False))' "$1"
}
msg_stop() {
    python3 -c 'import json,sys; print(json.dumps({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":sys.argv[1]}],"stopReason":sys.argv[2]}}, ensure_ascii=False))' "$1" "$2"
}
passing() { printf '审查结论：本轮通过，未发现显著问题。\n\nREVIEW-STATUS: PASS'; }
case "$mode" in
    ok)           msg "$(passing)"; echo '{"type":"agent_settled","aborted":false}' ;;
    plan)         msg "$(printf '方案可行，可以照着做。\n\nPLAN-STATUS: AGREED')"; echo '{"type":"agent_settled","aborted":false}' ;;
    not-verdict)  msg "需要我继续吗？"; echo '{"type":"agent_settled","aborted":false}' ;;
    unsettled)    msg "正在审查，先看" ;;
    # A provider truncation or error still reaches agent_settled with exit 0; the stop reason is what
    # separates those from a finished answer.
    truncated)    msg_stop "$(passing)" "length"; echo '{"type":"agent_settled","aborted":false}' ;;
    empty-final)  msg "$(passing)"; msg_stop "" "error"; echo '{"type":"agent_settled","aborted":false}' ;;
    error-then-ok) msg_stop "interrupted" "error"; msg "$(passing)"; echo '{"type":"agent_settled","aborted":false}' ;;
    # pi creates a session under the requested id when it no longer holds one, and warns on stderr.
    session-gone) echo "Warning: No project session found with id '$session_arg'; creating a new session with that id." >&2; msg "$(passing)"; echo '{"type":"agent_settled","aborted":false}' ;;
    fail)         echo "boom: transport reset" >&2; exit 1 ;;
    hang)         sleep 30 ;;
esac
exit 0
STUB
chmod +x bin/pi
export STUB_DIR="$tmp"

# The reviewer key has no default, so the suite starts from a file that names codex; every case that
# tests the resolver rewrites or removes it.
printf 'reviewer = "codex"\n' > agent-review.toml

failures=0

run() {
    local mode="$1"; shift
    set +e
    STUB_MODE="$mode" PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh "$@" >"$tmp/out.txt" 2>"$tmp/err.txt"
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
# Read here, where that round really was the first one: later in the harness the same file holds the
# id of a resume, so asserting it there would be asserting the wrong round.
expect_value "and it really was a first review" "0" "$tmp/last-resume.txt"
expect_value "with Fast mode off by default" "default" "$tmp/last-tier.txt"
expect_value "with no reasoning effort flag by default" "" "$tmp/last-reasoning.txt"
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

echo "a message in the stream must not pass for the -o result"
# An empty -o file is the 'no final message' refusal, whatever the stream says. Promoting the streamed
# text to the round's message turned this refusal into a passing round -- a regression that only a
# counter-example catches, because both halves look like a review.
run empty-output
check "it is refused" 1 "no final message" "$tmp/err.txt"
expect "the streamed text is still shown as evidence" "审查结论" "$tmp/err.txt"
expect "labelled as not the CLI's final message" "not the CLI's final message" "$tmp/err.txt"
if [[ -e .agent-review/rounds/$(basename "$(ls -t .agent-review/rounds/*-outcome.txt | head -1)" -outcome.txt)-review.txt ]]; then
    echo "  FAIL the streamed text was archived as the round's review"
    failures=$((failures + 1))
else
    echo "  ok   and no review file was archived for it"
fi

echo "a message only in the event stream, with no final-message file"
run events-only
check "it is refused" 1 "no final message" "$tmp/err.txt"
expect "the stream message is shown as evidence" "not the CLI's final message" "$tmp/err.txt"
expect "the stream message itself is shown" "审查结论" "$tmp/err.txt"
# A capture file that outlived its round is what let a later round show it as its own evidence. The
# round is archived and its capture files go with it, so this is the regression the archive cleanup
# has to keep: the next row below asserts the reuse itself, this one asserts the reason is gone.
# The stream fallback (round-events.jsonl.stream-message) is deliberately not in this list: the failure
# report reads it after the archive removes the rest, and the next round's start clears it.
for leftover in round-events.jsonl round-events.jsonl.final-message; do
    if [[ -e ".agent-review/$leftover" ]]; then
        echo "  FAIL $leftover survived the round it belonged to"
        failures=$((failures + 1))
    else
        echo "  ok   $leftover is removed once the round is archived"
    fi
done

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
GIT_DIR="$tmp/no-such-git-dir" STUB_MODE=ok PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh >"$tmp/out.txt" 2>"$tmp/err.txt"
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
cp agent-review.toml "$badrepo/" 2>/dev/null || true
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
    STUB_MODE=unfinished PATH="$badrepo/bin:$PATH" ./scripts/agent-review.sh >/dev/null 2>&1 || true
    wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' > "$scratch/calls-before.txt" || echo 0 > "$scratch/calls-before.txt"
    printf 'not an index' > .git/index
    set +e
    STUB_MODE=ok PATH="$badrepo/bin:$PATH" ./scripts/agent-review.sh --retry >"$scratch/out.txt" 2>"$scratch/err.txt"
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
# The case above proved the stream fallback is written. This is the reuse it must never allow: a failed
# round must not print the previous round's message as its own evidence.
run fail
if grep -q "not the CLI's final message" "$tmp/err.txt"; then
    echo "  FAIL a stale streamed message was shown as this round's evidence"
    failures=$((failures + 1))
else
    echo "  ok   the previous round's streamed message is not reused"
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
expect "the refusal points at task keys" "--task <slug>" "$tmp/err.txt"
run ok --finish
check "--finish respects the lock" 75 "cannot be cleared" "$tmp/err.txt"
rmdir .agent-review/lock

echo "a task key keeps a session, a lock and an archive of its own"
run ok --task alpha
check "a named task reviews" 0 - "$tmp/out.txt"
expect_value "its session is its own" "stub-alpha" .agent-review/tasks/alpha/codex-session-id
expect_value "and the keyless session is untouched" "stub-session" .agent-review/codex-session-id
expect "the round says which task it is" "task: alpha (state: .agent-review/tasks/alpha" "$tmp/out.txt"
expect "the reviewer is told which session it is" "belongs to task 'alpha'" "$tmp/last-prompt.txt"
expect "and that the key does not filter what it reviews" "does not select, narrow or filter" "$tmp/last-prompt.txt"
if [[ -f .agent-review/tasks/alpha/round-counter ]]; then
    echo "  ok   the named task has a round counter of its own"
else
    echo "  FAIL the named task kept no counter"
    failures=$((failures + 1))
fi
if ls .agent-review/tasks/alpha/rounds/*-outcome.txt >/dev/null 2>&1; then
    echo "  ok   its round is archived under its own task"
else
    echo "  FAIL the named task's round was not archived"
    failures=$((failures + 1))
fi

echo "each task resumes only its own session"
run ok --task beta
check "a second task reviews" 0 - "$tmp/out.txt"
expect_value "it starts a session of its own" "0" "$tmp/last-resume.txt"
# A new session resumes nothing at all, which is also what gives the check below its teeth: a stub that
# recorded no id could not tell the two cases apart.
expect_value "and resumes nothing" "" "$tmp/last-resume-id.txt"
expect_value "named for that task" "stub-beta" .agent-review/tasks/beta/codex-session-id
run ok --task alpha
check "the first task reviews again" 0 - "$tmp/out.txt"
expect_value "as a resume, not a new session" "1" "$tmp/last-resume.txt"
expect_value "of its own session id, not another task's" "stub-alpha" "$tmp/last-resume-id.txt"
expect_value "of its own session" "stub-alpha" .agent-review/tasks/alpha/codex-session-id

echo "a task key is normalised and validated"
run ok --task Alpha
check "an uppercase key is accepted" 0 - "$tmp/out.txt"
# The resolved key is what is asserted, not the absence of a second directory: on a case-insensitive
# filesystem the two spellings are one directory whatever the script does, which is exactly why the
# normalisation is stated in the output instead of being left to the filesystem.
expect "the resolved key is printed" "task: alpha (state: .agent-review/tasks/alpha" "$tmp/out.txt"
expect "and the session it resumed is the lowercase one" "stub-alpha" "$tmp/out.txt"
for bad in "" "../escape" "Alpha Beta" "tasks" "rounds"; do
    run ok --task "$bad"
    check "the key '${bad:-<empty>}' is refused" 64 "Invalid task key" "$tmp/err.txt"
done
# The raw value is what is validated, so a newline a command substitution would have eaten cannot turn
# a malformed key into a valid one: $'alpha\n' must not become 'alpha' and act on its session.
run ok --task $'alpha\n'
check "a key with a trailing newline is refused" 64 "Invalid task key" "$tmp/err.txt"
expect_value "and the task it would have named is untouched" "stub-alpha" .agent-review/tasks/alpha/codex-session-id
OMT_REVIEW_TASK=$'gamma\n' run ok
check "the same key in the environment is refused" 64 "Invalid task key" "$tmp/err.txt"
if [[ -e .agent-review/tasks/gamma ]]; then
    echo "  FAIL the malformed env key wrote to a task directory"
    failures=$((failures + 1))
else
    echo "  ok   the malformed env key wrote nothing"
fi
long_key="$(printf 'a%.0s' {1..65})"
run ok --task "$long_key"
check "a key longer than 64 characters is refused" 64 "Invalid task key" "$tmp/err.txt"

if [[ -e .agent-review/escape || -e .agent-review/tasks/escape ]]; then
    echo "  FAIL a refused key still made a directory"
    failures=$((failures + 1))
else
    echo "  ok   a refused key creates nothing"
fi
# An empty key must not fall back to the environment either: "no task was named" and "the task named
# is empty" are different statements, and only one of them is true here.
OMT_REVIEW_TASK=gamma run ok --task ""
check "an empty key does not fall back to the env var" 64 "Invalid task key" "$tmp/err.txt"
if [[ -e .agent-review/tasks/gamma ]]; then
    echo "  FAIL the env var's task was used after all"
    failures=$((failures + 1))
else
    echo "  ok   nothing was written to the env var's task"
fi

echo "'default' is the keyless task's label, not a key to take"
run ok --task default
check "--task default is refused" 64 "Invalid task key" "$tmp/err.txt"
expect "the refusal says what that name is" "label of the task" "$tmp/err.txt"
run ok --task DEFAULT
check "in any case" 64 "Invalid task key" "$tmp/err.txt"
run ok --finish --task default
check "--finish cannot be aimed at the keyless task through it" 64 "Invalid task key" "$tmp/err.txt"
expect_value "so the keyless session is untouched" "stub-session" .agent-review/codex-session-id
OMT_REVIEW_TASK=default run ok
check "nor through the environment" 64 "Invalid task key" "$tmp/err.txt"
expect_value "with the keyless session still untouched" "stub-session" .agent-review/codex-session-id

echo "OMT_REVIEW_TASK names the task for a harness"
OMT_REVIEW_TASK=gamma run ok
check "the env var names the task" 0 - "$tmp/out.txt"
expect_value "its session is its own" "stub-gamma" .agent-review/tasks/gamma/codex-session-id
OMT_REVIEW_TASK=gamma run ok --task delta
check "a key on the command line wins" 0 - "$tmp/out.txt"
expect_value "over the env var" "stub-delta" .agent-review/tasks/delta/codex-session-id
expect_value "and leaves the env var's task alone" "stub-gamma" .agent-review/tasks/gamma/codex-session-id

echo "one task's lock is not another's"
mkdir -p .agent-review/tasks/alpha/lock
run ok
check "a task's lock does not block the keyless task" 0 - "$tmp/out.txt"
expect_absent "a keyless prompt names no task" "belongs to task" "$tmp/last-prompt.txt"
run ok --task beta
check "nor another task" 0 - "$tmp/out.txt"
run ok --task alpha
check "but it does block that task" 75 "already running" "$tmp/err.txt"
expect_absent "and the keyed refusal needs no hint" "--task <slug>" "$tmp/err.txt"
rmdir .agent-review/tasks/alpha/lock

echo "two tasks review at the same time"
STUB_MODE=hang PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh --task alpha --timeout 60 >/dev/null 2>&1 &
alpha_pid=$!
for _ in $(seq 1 50); do [[ -d .agent-review/tasks/alpha/lock ]] && break; sleep 0.1; done
if [[ -d .agent-review/tasks/alpha/lock ]]; then
    echo "  ok   the hanging task holds its own lock"
else
    echo "  FAIL the hanging task never took its lock"
    failures=$((failures + 1))
fi
run ok
check "the keyless task reviews while it hangs" 0 - "$tmp/out.txt"
run ok --task beta
check "and so does another task" 0 - "$tmp/out.txt"
kill -TERM "$alpha_pid" 2>/dev/null
set +e
wait "$alpha_pid" 2>/dev/null
set -e
if [[ -d .agent-review/tasks/alpha/lock ]]; then
    echo "  FAIL the interrupted task left its lock behind"
    failures=$((failures + 1))
else
    echo "  ok   the interrupted task released its own lock"
fi

echo "--finish clears one task's session, not the others"
run ok --finish --task alpha
check "--finish takes the key" 0 - "$tmp/out.txt"
expect "and says which task it cleared" "task: alpha" "$tmp/out.txt"
if [[ -f .agent-review/tasks/alpha/codex-session-id ]]; then
    echo "  FAIL the named task's session survived --finish"
    failures=$((failures + 1))
else
    echo "  ok   the named task's session was cleared"
fi
expect_value "another task's session is untouched" "stub-beta" .agent-review/tasks/beta/codex-session-id
expect_value "and so is the keyless one" "stub-session" .agent-review/codex-session-id

echo "--tasks lists the sessions this repository holds"
run ok --tasks
check "the listing is produced" 0 - "$tmp/out.txt"
expect "the keyless task is listed" "^default" "$tmp/out.txt"
expect "with its session" "stub-session" "$tmp/out.txt"
expect "a named task is listed" "^beta" "$tmp/out.txt"
expect "with its own session" "stub-beta" "$tmp/out.txt"
expect_absent "a cleared task is not shown as holding one" "stub-alpha" "$tmp/out.txt"
run ok --tasks --design
check "a listing takes no round of its own" 64 "--tasks lists" "$tmp/err.txt"
run ok --tasks --task beta
check "and no task" 64 "--tasks lists" "$tmp/err.txt"

echo "the session log separates a clean hand-off from a lost session"
# The defect this guards: an outcome file writes `reason=none` both when a task ended on purpose and
# when its session went missing, so after the fact the two are indistinguishable. The log is the record
# that tells them apart, and these cases falsify it from both sides.
log=".agent-review/tasks/beta/session-log.tsv"
if [[ -f "$log" ]]; then
    echo "  ok   the log exists for a task that ran"
else
    echo "  FAIL no session log was written for the task"
    failures=$((failures + 1))
fi
expect "the first round is recorded as a session beginning" "$(printf '\tnew\tstub-beta\t')" "$log"
expect "and a later round as a resume" "$(printf '\tresumed\tstub-beta\t')" "$log"

# A session the CLI no longer holds must be recorded as lost, not as a resume -- this is the one case
# the whole log exists for, so it is asserted with the CLI's own error text.
echo "a resume against a session the CLI no longer holds"
run thread-gone --task beta
check "the round is refused" 1 "is gone" "$tmp/err.txt"
expect "the loss is named in the log" "$(printf '\tlost\tstub-beta\t')" "$log"
expect "and the log still shows the session it tried" "stub-beta" "$log"
# The counter-example: a lost session is not a closure. If `lost` were logged as a hand-off, a task
# whose session vanished would read as deliberately ended and its memory gap would go unnoticed.
expect_absent "a lost session is not recorded as finished" "$(printf '\tfinished\t')" "$log"

# --finish is the one deliberate end, and it must be distinguishable from the loss above.
echo "--finish records the hand-off it performs"
run ok --finish --task beta
check "--finish still takes the key" 0 - "$tmp/out.txt"
expect "the closure is recorded" "$(printf '\tfinished\tstub-beta\t')" "$log"
if [[ -f .agent-review/tasks/beta/codex-session-id ]]; then
    echo "  FAIL the pointer survived --finish"
    failures=$((failures + 1))
else
    echo "  ok   and only then does the pointer go"
fi

echo "--tasks reports whether a task is still on a session"
run ok --tasks
check "the listing is produced" 0 - "$tmp/out.txt"
expect "a closed task reads as closed" "closed" "$tmp/out.txt"
run ok --task gamma
check "a fresh task runs" 0 - "$tmp/out.txt"
run ok --tasks
expect "and reads as open, on its session" "open" "$tmp/out.txt"
run ok --tasks --task gamma
check "a listing still takes no task" 64 "--tasks lists" "$tmp/err.txt"

echo "a session replaced without --finish is not reported as a clean end"
# The second counter-example, and the harder one: a pointer that disappears without a --finish is a
# restart, not a hand-off. Deleting it here stands in for the ways that happens in practice (a failed
# first round that never recorded an id, a session cleared outside the helper).
gamma_log=".agent-review/tasks/gamma/session-log.tsv"
rm -f .agent-review/tasks/gamma/codex-session-id
run ok --task gamma
check "a round after the pointer vanished runs" 0 - "$tmp/out.txt"
expect "it is recorded as a restart" "$(printf '\trestarted\t')" "$gamma_log"
expect_absent "and not as a finished task" "$(printf '\tfinished\t')" "$gamma_log"

echo "a round after --finish is a new session, not an unexplained restart"
# The other half of the same distinction, and the defect the end-to-end probe caught: after a --finish
# the task *has* history, so a "has any history" test called the next round `restarted` and reported a
# deliberate hand-off as a session that went missing. It must be `new`, and it must not be `restarted`.
delta_log=".agent-review/tasks/delta/session-log.tsv"
run ok --task delta
check "the first round runs" 0 - "$tmp/out.txt"
run ok --finish --task delta
check "--finish takes the key" 0 - "$tmp/out.txt"
run ok --task delta
check "the round after --finish runs" 0 - "$tmp/out.txt"
expect "it is recorded as a new session" "$(printf '\tnew\tstub-delta\t')" "$delta_log"
expect_absent "not as a restart" "$(printf '\trestarted\t')" "$delta_log"
if [[ "$(grep -c "$(printf '\tnew\t')" "$delta_log")" == "2" ]]; then
    echo "  ok   both sessions began with a new event"
else
    echo "  FAIL expected two session beginnings: $(cat "$delta_log")"
    failures=$((failures + 1))
fi

echo "a failed attempt that never created a session is not history"
# The third defect from the review: a first round whose CLI dies before thread.started writes an
# outcome with `session=none`. Treating the archive as proof that a session existed made the *next*
# round `restarted`, reporting a replacement of a session that never was. It must be `new`, and the
# no-session attempt must not appear in the log at all -- there is no session to name.
zeta_log=".agent-review/tasks/zeta/session-log.tsv"
run no-session --task zeta
check "an attempt with no session is still archived" 0 - "$tmp/out.txt"
if [[ -f "$zeta_log" ]]; then
    echo "  FAIL a session event was logged for an attempt that had no session: $(cat "$zeta_log")"
    failures=$((failures + 1))
else
    echo "  ok   an attempt that established no session logs no session event"
fi
expect_absent "and the health view does not claim a session" "stub-zeta" "$tmp/out.txt"
run ok --task zeta
check "the next round runs" 0 - "$tmp/out.txt"
expect "it begins a session rather than replacing one" "$(printf '\tnew\tstub-zeta\t')" "$zeta_log"
expect_absent "and is not called a restart" "$(printf '\trestarted\t')" "$zeta_log"

echo "the round number is the same in the log and the archive"
# The second defect from the review: the log read the counter before the round and the archive wrote it
# after, so every entry was one round off and an event could not be matched to the evidence it
# described. The number in the log must name an archived outcome that exists.
num_task="numbers"
num_log=".agent-review/tasks/$num_task/session-log.tsv"
for i in 1 2 3; do
    run ok --task "$num_task"
    check "round $i runs" 0 - "$tmp/out.txt"
done
for n in 1 2 3; do
    if [[ "$(awk -F'\t' -v n="$n" '$4 == n { c++ } END { print c + 0 }' "$num_log")" == "1" ]]; then
        echo "  ok   the log names round $n exactly once"
    else
        echo "  FAIL the log does not name round $n exactly once: $(cat "$num_log")"
        failures=$((failures + 1))
    fi
    if ls ".agent-review/tasks/$num_task/rounds/"*-"$(printf '%03d' "$n")"-outcome.txt >/dev/null 2>&1; then
        echo "  ok   and an archive outcome carries $n"
    else
        echo "  FAIL no archive outcome carries round $n: $(ls ".agent-review/tasks/$num_task/rounds/" | tr '\n' ' ')"
        failures=$((failures + 1))
    fi
done
# The third round of the session is the third the session has run, and the budget warning counts
# attempts, not events: a loss is the second half of a resume and must not be counted twice.
run ok --tasks
expect "the session counts its own rounds" "3" "$tmp/out.txt"

echo "a lost session is counted once, not once per event"
# The fourth defect from the review: `lost` was counted as an extra round on top of the `resumed` it
# belongs to, so the session looked busier than it was and the budget warning fired early.
loss_task="losses"
loss_log=".agent-review/tasks/$loss_task/session-log.tsv"
run ok --task "$loss_task"
check "the first round runs" 0 - "$tmp/out.txt"
run thread-gone --task "$loss_task"
check "the resume fails as a lost session" 1 "is gone" "$tmp/err.txt"
run ok --task "$loss_task"
check "a fresh round runs again" 0 - "$tmp/out.txt"
if [[ "$(grep -c "$(printf '\tlost\t')" "$loss_log")" == "1" ]]; then
    echo "  ok   the loss is recorded"
else
    echo "  FAIL the loss was not recorded once: $(cat "$loss_log")"
    failures=$((failures + 1))
fi
# Two attempts ran (the first round and the retry), so the session has spent two rounds, not three --
# the lost resume and the round after it are one attempt each, and the loss is not a third.
resumes="$(grep -c "$(printf '\tresumed\t')" "$loss_log")"
if [[ "$resumes" == "2" ]]; then
    echo "  ok   the two resumed attempts are on record"
else
    echo "  FAIL expected two resumes, got $resumes: $(cat "$loss_log")"
    failures=$((failures + 1))
fi
# The counting rule itself: new(1) + resumed(2) + lost(not counted) + resumed(3) is three rounds spent
# across two sessions' worth of log lines -- the loss must not make it four.
run ok --tasks
if awk '$1 == "losses" { print $7 }' "$tmp/out.txt" | grep -qx "3"; then
    echo "  ok   the loss is not counted as an extra round"
else
    echo "  FAIL the session's round count is wrong: $(awk '$1 == "losses"' "$tmp/out.txt")"
    failures=$((failures + 1))
fi

echo "an interrupted first round still records that its session began"
# The defect this guards: the log entry for a new session is written after the CLI returns, but a round
# stopped by a signal exits through the handler before reaching that line. Without a record on both
# paths the log would open on a later round's `resumed`, leaving the session's beginning unrecorded.
# A real SIGTERM is used, not --timeout: the timeout returns through the normal path and would pass
# even with the signal path broken, which is exactly how this assertion first failed to bite.
int_task="interrupted-start"
rm -rf ".agent-review/tasks/$int_task"
STUB_MODE=hang PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh --task "$int_task" >/dev/null 2>&1 &
int_pid=$!
for _ in $(seq 1 100); do [[ -s ".agent-review/tasks/$int_task/round-events.jsonl" ]] && break; sleep 0.1; done
for _ in $(seq 1 100); do [[ -s ".agent-review/tasks/$int_task/codex-session-id" ]] && break; sleep 0.1; done
kill -TERM "$int_pid" 2>/dev/null
set +e
wait "$int_pid"
set -e
int_log=".agent-review/tasks/$int_task/session-log.tsv"
expect "the interrupted first round records its session" "$(printf '\tnew\tstub-interrupted-start\t')" "$int_log"
expect_value "and the pointer names the same session it logged" "stub-interrupted-start" \
    ".agent-review/tasks/$int_task/codex-session-id"
# The idempotence half: one round must contribute exactly one event, not one per path.
if [[ "$(grep -c "$(printf '\tnew\t')" "$int_log")" == "1" ]]; then
    echo "  ok   the round is recorded once, not once per path"
else
    echo "  FAIL the round was recorded more than once: $(cat "$int_log")"
    failures=$((failures + 1))
fi

echo "the session-log write is not gated by the in-memory flag"
# The window the review found: with ROUND_SESSION_LOGGED set before the append, a signal landing
# between them loses the entry -- verified by widening that window and delivering SIGTERM inside it,
# which leaves the round archived with no session event at all. The fix removes the dependence on that
# ordering (the log itself is the idempotence key), so the ordering is what is asserted here.
#
# This is a static check on purpose, and the reason is worth stating: the window is two adjacent shell
# statements, so no test can deterministically land a signal inside it -- a timing-based test would be
# flaky, and a flaky gate is worse than none. Reading the ordering off the source fails on the
# defective implementation every time, which is what a regression gate has to do.
log_body="$(sed -n '/^log_round_session()/,/^}/p' ./scripts/agent-review.sh)"
append_line="$(printf '%s\n' "$log_body" | grep -n 'log_session_event' | head -1 | cut -d: -f1)"
guard_line="$(printf '%s\n' "$log_body" | grep -n 'ROUND_SESSION_LOGGED=1' | tail -1 | cut -d: -f1)"
if [[ -n "$append_line" && -n "$guard_line" && "$guard_line" -gt "$append_line" ]]; then
    echo "  ok   the flag is set after the append, never before it"
else
    echo "  FAIL the flag is set at or before the append (flag=$guard_line append=$append_line): a signal"
    echo "       between them would lose the event, which is the defect this fix removed"
    failures=$((failures + 1))
fi
# The other direction, so the check cannot pass on an implementation that never sets it at all.
if printf '%s\n' "$log_body" | grep -q 'log_session_event'; then
    echo "  ok   and the append is what the ordering is about"
else
    echo "  FAIL log_round_session no longer appends an event"
    failures=$((failures + 1))
fi

echo "the round budget warning"
# AGENTS.md budgets three review rounds per task, so the session is over budget when it starts a
# fourth. The warning must stay silent through the first three and fire on the fourth: one that always
# fires is noise, and one that never fires is not a gate. Both sides are asserted here.
budget_task="budget"
for i in 1 2 3 4; do
    run ok --task "$budget_task"
    check "budget round $i runs" 0 - "$tmp/out.txt"
    if [[ "$i" -le 3 ]]; then
        expect_absent "round $i does not warn yet" "budgets 3" "$tmp/err.txt"
    else
        expect "round $i warns that the session is over budget" "budgets 3" "$tmp/err.txt"
        expect "and names the way out" "--finish" "$tmp/err.txt"
    fi
done

echo "a signal sent to the script alone, while the CLI ignores TERM"
rm -f "$tmp/child.pid" "$tmp/script-exit.txt"
STUB_MODE=ignore-term PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh >/dev/null 2>&1 &
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
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh >/dev/null 2>&1 &
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
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh --timeout 2 >/dev/null 2>"$tmp/err.txt" &
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
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh >/dev/null 2>&1 &
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

echo "the model and speed the reviewer runs on, on every round"
# The default case is the file absent, which is the state of this harness: the built-in default model
# with Fast mode off. Both halves are asserted, because "off" is a value the helper sends
# (service_tier=default) rather than the absence of an argument.
expect_value "the reviewer runs on gpt-6.1-sol" "gpt-6.1-sol" "$tmp/last-model.txt"
expect_value "with Fast mode off" "default" "$tmp/last-tier.txt"
expect_value "with no reasoning effort by default" "" "$tmp/last-reasoning.txt"
expect "the round says which settings it used" "reviewer: model=gpt-6.1-sol (the built-in default), fast=off (the built-in default)" "$tmp/out.txt"
expect "and says the effort is unset" "reasoning_effort=- (the built-in default)" "$tmp/out.txt"

# A resume has to repeat the tier: codex resolves it per turn, so a resumed round that omits it runs at
# the default speed even when the first round asked for Fast (measured on codex-cli 0.162.1).
run ok
expect_value "a re-review is a resume" "1" "$tmp/last-resume.txt"
expect_value "and it carries the tier again" "default" "$tmp/last-tier.txt"
expect_value "and no effort flag again" "" "$tmp/last-reasoning.txt"

echo "OMT_REVIEW_MODEL names another model, OMT_REVIEW_FAST another speed"
OMT_REVIEW_MODEL=another-model run ok
check "the run with another model is produced" 0 - "$tmp/out.txt"
expect_value "OMT_REVIEW_MODEL is passed through" "another-model" "$tmp/last-model.txt"
expect "the round says where the model came from" "model=another-model (OMT_REVIEW_MODEL)" "$tmp/out.txt"
expect_value "and the speed is untouched" "default" "$tmp/last-tier.txt"
OMT_REVIEW_FAST=1 run ok
check "the run with Fast mode is produced" 0 - "$tmp/out.txt"
expect_value "OMT_REVIEW_FAST=1 asks for codex's priority tier" "priority" "$tmp/last-tier.txt"
expect "the round says where the speed came from" "fast=on (OMT_REVIEW_FAST)" "$tmp/out.txt"
OMT_REVIEW_FAST=false run ok
expect_value "OMT_REVIEW_FAST=false is off" "default" "$tmp/last-tier.txt"
for bad in maybe 2 yes-please; do
    OMT_REVIEW_FAST="$bad" run ok
    check "OMT_REVIEW_FAST='$bad' is refused" 64 "OMT_REVIEW_FAST" "$tmp/err.txt"
done
OMT_REVIEW_MODEL=$'bad\nmodel' run ok
check "a model with a newline is refused" 64 "OMT_REVIEW_MODEL" "$tmp/err.txt"
OMT_REVIEW_MODEL='a b' run ok
check "a model with a space is refused" 64 "OMT_REVIEW_MODEL" "$tmp/err.txt"

echo "OMT_REVIEW_REASONING_EFFORT names a thinking level"
OMT_REVIEW_REASONING_EFFORT=high run ok
check "the run with high reasoning effort is produced" 0 - "$tmp/out.txt"
expect_value "OMT_REVIEW_REASONING_EFFORT is passed through" "high" "$tmp/last-reasoning.txt"
expect "the round says where the effort came from" "reasoning_effort=high (OMT_REVIEW_REASONING_EFFORT)" "$tmp/out.txt"
OMT_REVIEW_REASONING_EFFORT=low run ok
expect_value "a different level reaches the CLI" "low" "$tmp/last-reasoning.txt"
# The flag belongs on a first round too, not only on a resume: a fresh task starts a new session.
OMT_REVIEW_REASONING_EFFORT=high run ok --task effort-first
check "the first round of a new task runs" 0 - "$tmp/out.txt"
expect_value "and it carries the effort as well" "high" "$tmp/last-reasoning.txt"
expect_value "on a first review, not a resume" "0" "$tmp/last-resume.txt"
for bad in High "has space" 2 '!'; do
    OMT_REVIEW_REASONING_EFFORT="$bad" run ok
    check "OMT_REVIEW_REASONING_EFFORT='$bad' is refused" 64 "OMT_REVIEW_REASONING_EFFORT" "$tmp/err.txt"
done

echo "agent-review.toml configures the model and the speed"
# The file is read from the repository root, which is this throwaway repo. Each case rewrites it; it is
# left in place afterwards, because the reviewer key has no default and a round without one is refused.
write_config() { printf '%s\n' "$@" > agent-review.toml; }
# A fast flag whose value cannot be confused with the file's: every case below that asserts a "default"
# tier would also pass if the flag were dropped.
write_config 'reviewer = "codex"' '[codex]' 'model = "file-model"' 'fast = true'
run ok
check "a configured round runs" 0 - "$tmp/out.txt"
expect_value "the file's model is used" "file-model" "$tmp/last-model.txt"
expect_value "the file's fast = true asks for priority" "priority" "$tmp/last-tier.txt"
expect "the round names the file as the source" "model=file-model (agent-review.toml), fast=on (agent-review.toml)" "$tmp/out.txt"
expect "and names the reviewer it chose" "reviewer: codex (agent-review.toml)" "$tmp/out.txt"
write_config 'reviewer = "codex"' '[codex]' 'fast = true'
run ok
expect_value "a missing model falls back on its own" "gpt-6.1-sol" "$tmp/last-model.txt"
expect_value "while the file's fast still applies" "priority" "$tmp/last-tier.txt"
expect "each setting's source is reported separately" "model=gpt-6.1-sol (the built-in default), fast=on (agent-review.toml)" "$tmp/out.txt"
write_config 'reviewer = "codex"' '[codex]' 'reasoning_effort = "xhigh"'
run ok
expect_value "the file's reasoning_effort is used" "xhigh" "$tmp/last-reasoning.txt"
expect "and the round names the file as its source" "reasoning_effort=xhigh (agent-review.toml)" "$tmp/out.txt"
write_config 'reviewer = "codex"' '[codex]' 'model = "file-model"' 'reasoning_effort = "max"'
run ok
expect_value "a file effort and a file model combine" "max" "$tmp/last-reasoning.txt"
expect_value "the model is still the file's" "file-model" "$tmp/last-model.txt"
expect_value "and an omitted fast falls back on its own" "default" "$tmp/last-tier.txt"
write_config 'reviewer = "codex"' '[codex]' 'model = "file-model"' 'fast = false'
run ok
expect_value "fast = false is off" "default" "$tmp/last-tier.txt"
# The file must not be able to leave the tier unset: an omitted service_tier inherits a Fast mode
# configured in ~/.codex/config.toml or .codex/config.toml, so "off" has to be sent as "default".
OMT_REVIEW_FAST=1 run ok
expect_value "a one-off override wins over the file" "priority" "$tmp/last-tier.txt"
OMT_REVIEW_MODEL=env-model run ok
expect_value "and the model override wins over the file" "env-model" "$tmp/last-model.txt"

echo "reviewer = pi is a sibling table with its own fields"
# A pi round passes its three settings and the read-only tool allowlist; the pi stub records each.
write_config 'reviewer = "pi"' '[codex]' 'model = "file-model"' 'fast = true' \
    '[pi]' 'provider = "command code"' 'model = "pi-model"' 'thinking = "xhigh"'
run ok
check "a configured pi round runs" 0 - "$tmp/out.txt"
expect_value "the pi provider reaches the CLI" "command code" "$tmp/pi-provider.txt"
expect_value "the pi model reaches the CLI" "pi-model" "$tmp/pi-model.txt"
expect_value "the pi thinking level reaches the CLI" "xhigh" "$tmp/pi-thinking.txt"
expect_value "and only the read-only tools are enabled" "read,bash,grep,find,ls" "$tmp/pi-tools.txt"
expect "the round names the pi settings and their source" "reviewer: pi provider=command code (agent-review.toml), model=pi-model (agent-review.toml), thinking=xhigh (agent-review.toml)" "$tmp/out.txt"
expect "a reviewer switch drops the other backend's session" "cannot be resumed by pi" "$tmp/err.txt"
expect_value "the first pi round names no session id" "" "$tmp/pi-session-arg.txt"
run ok
expect_value "the next pi round resumes the session it made" "pi-session" "$tmp/pi-session-arg.txt"
expect "and the review is still produced on a resume" "PASS" "$tmp/out.txt"
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "the pi outcome names the reviewer" "reviewer=pi" "$last_outcome"
expect "and the pi provider" "pi_provider=command code" "$last_outcome"
expect "and the pi model" "pi_model=pi-model" "$last_outcome"
expect "and the pi thinking level" "pi_thinking=xhigh" "$last_outcome"
# The pi classifier completes only on agent_settled with aborted false; these are its counter-examples.
run unsettled
check "a pi round with no agent_settled is refused" 1 "no agent_settled" "$tmp/err.txt"
run fail
check "a pi process that exits non-zero is refused" 1 "pi exited" "$tmp/err.txt"
run not-verdict
check "a pi message that is not a verdict is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
run plan --design
check "a pi design round is accepted" 0 - "$tmp/out.txt"
expect "and kept as a plan" "PLAN-STATUS: AGREED" .agent-review/last-plan.txt
run session-gone
check "a pi resume whose session is gone still answers" 0 - "$tmp/out.txt"
if [[ "$(tail -1 .agent-review/session-log.tsv | cut -f2)" == "lost" ]]; then
    echo "  ok   the lost pi session is recorded"
else
    echo "  FAIL the lost pi session was not recorded"
    failures=$((failures + 1))
fi
expect "and the warning says so" "no longer exists" "$tmp/err.txt"
run truncated
check "a pi answer truncated by the model is refused" 1 "stopped with 'length'" "$tmp/err.txt"
run empty-final
check "an empty final pi message is refused" 1 "stopped with 'error'" "$tmp/err.txt"
expect_absent "and the earlier passing message is not surfaced as evidence" "REVIEW-STATUS: PASS" "$tmp/err.txt"
run error-then-ok
check "a pi run that recovered on a later message passes" 0 - "$tmp/out.txt"
expect "and the recovered final answer is the one used" "本轮通过" "$tmp/out.txt"

echo "a pi round's answer survives the archive"
# pi has no -o file, so its answer exists only in the event stream, and the archive keeps the stream's
# summary rather than the stream. Dropping pi's own event names would leave the answer outside the
# summary; a broken line beside it then makes the summary non-empty, so the raw-file fallback no longer
# applies and the answer is lost with the capture file. This fixture is exactly that case.
pi_events="$(ls -t .agent-review/rounds/*-events.jsonl | head -1)"
expect "the pi answer is archived" "REVIEW-STATUS: PASS" "$pi_events"
expect "the summary keeps pi's session header" "\"type\": \"session\"" "$pi_events"
expect "and its settled boundary" "agent_settled" "$pi_events"
expect "and its final message" "message_end" "$pi_events"

echo "a pi stream with an unparsable line still summarises to its answer"
# The same fixture with one line the summariser cannot read: the answer must still be in the summary,
# which is all the archive keeps once the capture file is removed.
printf '%s\n' '{"type":"session","version":3,"id":"pi-broken"}' 'not json at all' \
    '{"type":"agent_settled","aborted":false}' > "$tmp/pi-broken.jsonl"
# Cut by line numbers rather than by a pattern: the function's body is a heredoc, whose closing PY is
# not an end of function, so a /^}$/ match would slice through the middle of the python.
sum_start="$(grep -n '^summarize_events() {' ./scripts/agent-review.sh | head -1 | cut -d: -f1)"
sum_py="$(awk -v s="$sum_start" 'NR > s && /^PY$/ { print NR; exit }' ./scripts/agent-review.sh)"
# Two lines past the heredoc's closing PY: the command substitution's own `}` is the first.
sum_end="$(awk -v s="$sum_py" 'NR > s && /^}$/ { n++; if (n == 2) { print NR; exit } }' ./scripts/agent-review.sh)"
sed -n "${sum_start},${sum_end}p" ./scripts/agent-review.sh > "$tmp/summarize.sh"
if bash -c "source '$tmp/summarize.sh' && summarize_events '$tmp/pi-broken.jsonl' '$tmp/pi-broken-summary.jsonl'" \
        2>/dev/null \
    && grep -q 'not json at all' "$tmp/pi-broken-summary.jsonl" \
    && grep -q 'agent_settled' "$tmp/pi-broken-summary.jsonl"; then
    echo "  ok   the unparsable line and the boundary both survive the summary"
else
    echo "  FAIL the summary lost the broken line or the boundary"
    failures=$((failures + 1))
fi

echo "the reviewer is required and has no default"
rm -f agent-review.toml
run ok
check "a missing reviewer refuses the round" 64 "No reviewer configured" "$tmp/err.txt"
OMT_REVIEW_REVIEWER=pi run ok
check "pi without its three settings refuses" 64 "provider, model, thinking" "$tmp/err.txt"
OMT_REVIEW_REVIEWER=none run ok
check "reviewer = none is an explicit no-op" 0 "no reviewer is configured" "$tmp/out.txt"
write_config 'reviewer = "codex"'

echo "a configuration that cannot be read refuses the round"
calls_before_config="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
for bad_config in 'reviewer = "codex"
[codex]
model = [' 'reviewer = "codex"
[codex]
fast = "yes"' 'reviewer = "codex"
[codex]
model = 12' 'reviewer = "codex"
[codex]
model = ""' 'reviewer = "codex"
[codex]
reasoning_effort = "High"' 'reviewer = "codex"
[codex]
reasoning_effort = 3' 'reviewer = "codex"
[codex]
reasoning_effort = ""' 'reviewer = "codex"
[codex]
unknown = 1' 'reviewer = "codex"
[other]
model = "x"' 'reviewer = "codex"
[codex]
model = "has
newline"' 'review = 1' 'reviewer = "codex"
[codex]
fast = 1' 'reviewer = "pi"' 'reviewer = "other"' '[pi]
provider = "x"'; do
    printf '%s\n' "$bad_config" > agent-review.toml
    run ok
    check "it is refused: $(printf '%s' "$bad_config" | tr '\n' ' ' | head -c 40)" 64 "agent-review.toml" "$tmp/err.txt"
done
# The missing TOML reader has to be refused in the same way: a bare `python3` is not necessarily 3.11+.
printf '%s\n' 'reviewer = "codex"' '[codex]' 'model = "file-model"' > agent-review.toml
rm -rf "$tmp/nolib"; mkdir -p "$tmp/nolib"
cat > "$tmp/nolib/python3" <<'NOPY'
#!/bin/bash
# A python3 without tomllib (macOS ships 3.9 at /usr/bin/python3), for the version check.
if [[ "${1:-}" == "-" ]]; then
    echo "python3 3.9.6 has no tomllib, so $2 cannot be read; Python 3.11 or newer is needed for that file. Runs without it need no TOML reader." >&2
    exit 1
fi
exec /usr/bin/env python3 "$@"
NOPY
chmod +x "$tmp/nolib/python3"
set +e
STUB_MODE=ok PATH="$tmp/nolib:$tmp/bin:$PATH" ./scripts/agent-review.sh >"$tmp/out.txt" 2>"$tmp/err.txt"
echo $? > "$tmp/exit.txt"
set -e
check "a python3 without tomllib is refused, with the reason" 64 "no tomllib" "$tmp/err.txt"
# The counter-example that makes "the reviewer was not called" mean something: an accepted config does
# call it. Without this, a harness that never calls the stub at all would pass the check below.
write_config 'reviewer = "codex"' '[codex]' 'model = "file-model"'
run ok
check "a readable configuration still runs" 0 - "$tmp/out.txt"
expect "switching back to codex drops the pi session" "cannot be resumed by codex" "$tmp/err.txt"
calls_after_control="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
if [[ "$calls_after_control" -gt "$calls_before_config" ]]; then
    echo "  ok   an accepted configuration does call the reviewer"
else
    echo "  FAIL the reviewer was never called, so 'not called' proves nothing"
    failures=$((failures + 1))
fi
# Every refused case above must have left the CLI alone.
calls_before_bad="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
printf '%s\n' 'reviewer = "codex"' '[codex]' 'fast = "yes"' > agent-review.toml
run ok
check "a bad value is refused" 64 "must be true or false" "$tmp/err.txt"
run ok --task alpha
check "a bad value refuses a named task too" 64 "must be true or false" "$tmp/err.txt"
printf '%s\n' 'reviewer = "codex"' '[codex]' 'reasoning_effort = "High"' > agent-review.toml
run ok
check "an uppercase effort is refused rather than lowercased" 64 "reasoning_effort" "$tmp/err.txt"
calls_after_bad="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
if [[ "$calls_before_bad" == "$calls_after_bad" ]]; then
    echo "  ok   a refused configuration never reaches the reviewer"
else
    echo "  FAIL the reviewer was called $((calls_after_bad - calls_before_bad)) time(s) with a broken configuration"
    failures=$((failures + 1))
fi
# A refused configuration must leave the previous round's evidence where it was.
: > .agent-review/last-status.txt; printf 'concluded=yes\n' > .agent-review/last-status.txt
: > .agent-review/last-round-kind; printf 'review\n' > .agent-review/last-round-kind
run ok
check "the round is refused" 64 "agent-review.toml" "$tmp/err.txt"
expect "the previous status record survives" "concluded=yes" .agent-review/last-status.txt
expect_value "and the recorded kind is untouched" "review" .agent-review/last-round-kind
# A broken configuration must not be able to make a session unclearable.
write_config 'reviewer = "codex"' '[codex]' 'model = ['
run ok --tasks
check "--tasks works with a broken configuration" 0 "task" "$tmp/out.txt"
run ok --finish
check "--finish works with a broken configuration" 0 "session cleared" "$tmp/out.txt"
if [[ -e .agent-review/session-backend || -e .agent-review/codex-session-id ]]; then
    echo "  FAIL --finish left the session pointer behind"
    failures=$((failures + 1))
else
    echo "  ok   --finish clears the session pointer and its backend"
fi
run ok --help
check "--help works with a broken configuration" 0 "usage:" "$tmp/err.txt"
rm -f agent-review.toml

echo "a configuration path that exists but cannot be read is refused, not treated as absent"
# "No configuration file" and "a configuration that cannot be read" are different states, and only the
# first may fall back on the defaults. Treating the second as absent is the silent fallback this
# function exists to prevent: a config reached through a symlink would stop applying, without a word,
# the moment its target went away, and the reviewer would run on a model nobody chose.
config_calls_before="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
rm -f agent-review.toml
mkdir agent-review.toml
run ok
check "a directory where the configuration should be is refused" 64 "not a readable regular file" "$tmp/err.txt"
rmdir agent-review.toml
ln -s "$tmp/no-such-target.toml" agent-review.toml
run ok
check "a dangling symlink is refused" 64 "not a readable regular file" "$tmp/err.txt"
rm -f agent-review.toml
# The absent case is asserted straight after the unreadable ones, so "refused" cannot be passing
# because every state is refused: an absent file refuses for the missing reviewer, a different reason.
run ok
check "an absent configuration refuses for the missing reviewer" 64 "No reviewer configured" "$tmp/err.txt"
# Unreadable to this user, which is not the same as absent either.
printf '%s\n' 'reviewer = "codex"' '[codex]' 'model = "file-model"' > agent-review.toml
chmod 000 agent-review.toml
if [[ ! -r agent-review.toml ]]; then
    run ok
    check "an unreadable file is refused" 64 "not a readable regular file" "$tmp/err.txt"
else
    echo "  ok   (skipped the chmod-000 case: this user can still read the file)"
fi
chmod 644 agent-review.toml 2>/dev/null || true
rm -f agent-review.toml
config_calls_after="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
# Every state between the two snapshots is refused before the reviewer is reached: the directory, the
# dangling symlink, the absent file and the unreadable file.
if [[ "$((config_calls_after - config_calls_before))" -eq 0 ]]; then
    echo "  ok   no broken or absent path reached the reviewer"
else
    echo "  FAIL $((config_calls_after - config_calls_before)) calls happened; a broken path reached the reviewer"
    failures=$((failures + 1))
fi
# The reviewer has no default, so the cases after this one need a file that names one.
write_config 'reviewer = "codex"'

echo "the requested speed is archived with every round"
run ok
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "the outcome names the model" "model=gpt-6.1-sol" "$last_outcome"
expect "and its source" "model_source=the built-in default" "$last_outcome"
expect "and the speed" "fast=off" "$last_outcome"
expect "and the tier actually requested" "service_tier=default" "$last_outcome"
expect "and that nothing was rejected" "service_tier_rejected=no" "$last_outcome"
expect "and the reasoning effort (unset)" "reasoning_effort=-" "$last_outcome"
expect "and the effort's own source" "reasoning_effort_source=the built-in default" "$last_outcome"
expect "and that the effort was not rejected" "reasoning_effort_rejected=no" "$last_outcome"

echo "a speed codex declines is reported and archived, not treated as a failed round"
STUB_MODE=tier-rejected run tier-rejected
check "the review still counts" 0 - "$tmp/out.txt"
# A specific field inside a multi-field file, so grep rather than expect_value (which reads the whole file).
if grep -q '^service_tier_rejected=yes$' .agent-review/last-status.txt; then
    echo "  ok   the status records the rejected tier"
else
    echo "  FAIL the status did not record the rejected tier"
    failures=$((failures + 1))
fi
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "the outcome carries the rejection" "service_tier_rejected=yes" "$last_outcome"
expect "and the effort is not mislabelled by it" "reasoning_effort_rejected=no" "$last_outcome"
expect "and the round still produced its review" "reason=none" "$last_outcome"
echo "an effort codex declines is reported and archived, not treated as a failed round"
OMT_REVIEW_REASONING_EFFORT=extreme run reasoning-rejected
check "the review still counts" 0 - "$tmp/out.txt"
if grep -q '^reasoning_effort_rejected=yes$' .agent-review/last-status.txt; then
    echo "  ok   the status records the rejected effort"
else
    echo "  FAIL the status did not record the rejected effort"
    failures=$((failures + 1))
fi
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "the outcome carries the rejected effort" "reasoning_effort_rejected=yes" "$last_outcome"
expect "and names the level that was requested" "reasoning_effort=extreme" "$last_outcome"
expect "and the round still produced its review" "reason=none" "$last_outcome"
expect "the warning names the level" "did not accept the requested reasoning effort 'extreme'" "$tmp/err.txt"
expect_absent "and it is not misreported as a tier rejection" "did not accept the requested service tier" "$tmp/err.txt"
STUB_MODE=error-item run error-item
check "an unrelated error item does not fail the round" 0 - "$tmp/out.txt"
# The fixture is asserted to have emitted its error: without this, a call that silently ran the happy
# path would satisfy every "no tier rejection" assertion below it. Read from the round's archive, because
# the raw capture file is removed with the round it belonged to.
last_events="$(ls -t .agent-review/rounds/*-events.jsonl | head -1)"
expect "the fixture really emitted an error item" "stream disconnected before completion" "$last_events"
expect_absent "and it is not reported as a tier rejection" "did not accept the requested service tier" "$tmp/err.txt"
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "with no rejection in the archive" "service_tier_rejected=no" "$last_outcome"
expect "and not as an effort rejection either" "reasoning_effort_rejected=no" "$last_outcome"
# The same generic phrase about a different setting: matching it alone would archive a tier rejection
# that never happened, so this is the case that gives the tier-specific match its teeth. `run` takes the
# mode as its *first* argument, so the mode is passed there and not only as a prefix: a prefix naming a
# mode that the call then overrides runs the happy path, and the counter-example proves nothing.
STUB_MODE=other-setting run other-setting
check "a rejection of another setting is not a failed round" 0 - "$tmp/out.txt"
last_events="$(ls -t .agent-review/rounds/*-events.jsonl | head -1)"
expect "the fixture really emitted the other setting's rejection" "verbosity" "$last_events"
expect "and the generic phrase really was in it" "not advertised as supported for model" "$last_events"
expect_absent "and it is not reported as a tier rejection either" "did not accept the requested service tier" "$tmp/err.txt"
expect_absent "and not as an effort rejection either" "did not accept the requested reasoning effort" "$tmp/err.txt"
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "with no tier rejection in the archive either" "service_tier_rejected=no" "$last_outcome"
expect "and no effort rejection in the archive either" "reasoning_effort_rejected=no" "$last_outcome"

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
STUB_MODE=late-session PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh >/dev/null 2>&1 &
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
STUB_MODE=ignore-term PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh --timeout 2 >/dev/null 2>"$tmp/err.txt" &
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
STUB_MODE=ok PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh --timeout 60 2>/dev/null | cat >/dev/null
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

echo "a round's capture files do not outlive the round"
# The raw stream is a work file: it is archived as a summary and removed, so the next round cannot read
# it as its own output. The stream fallback is the one that stays, because the failure report reads it
# after the archive; it is cleared when the next round starts.
run ok
for leftover in round-events.jsonl round-events.jsonl.final-message round-events.jsonl.summary; do
    if [[ -e ".agent-review/$leftover" ]]; then
        echo "  FAIL $leftover outlived its round"
        failures=$((failures + 1))
    else
        echo "  ok   $leftover is removed with its round"
    fi
done
expect "the durable copy of the review is kept" "REVIEW-STATUS: PASS" .agent-review/last-message.txt

echo "the archive keeps the evidence and not the transcript"
# Measured on this repository's own archive: the reviewer's command output was ~97% of the bytes and
# none of it explains the round. The summary keeps the boundaries, the message and the command itself.
run big-output
check "a round with a large transcript still passes" 0 - "$tmp/out.txt"
big_events="$(ls -t .agent-review/rounds/*-events.jsonl | head -1)"
big_size="$(wc -c < "$big_events" | tr -d ' ')"
if [[ "$big_size" -lt 20000 ]]; then
    echo "  ok   the archived summary is $big_size bytes, not the 200 KB the command printed"
else
    echo "  FAIL the command output was archived in full ($big_size bytes)"
    failures=$((failures + 1))
fi
expect "the summary names the command the reviewer ran" "cat big.txt" "$big_events"
expect "and keeps the turn boundary" "turn.completed" "$big_events"
expect "and keeps the final message" "REVIEW-STATUS: PASS" "$big_events"

echo "a failed command's result survives the summary"
# The one thing the summary must not drop about a command is how it ended: a reviewer diagnosing a
# failed test run has nothing to explain it with if exit_code and the tail are cut as 'output'.
run failed-command
check "the round still passes" 0 - "$tmp/out.txt"
failed_events="$(ls -t .agent-review/rounds/*-events.jsonl | head -1)"
expect "the exit code is archived" "\"exit_code\": 101" "$failed_events"
expect "the failure status is archived" "\"status\": \"failed\"" "$failed_events"
expect "and a bounded excerpt of what it printed" "test result: FAILED" "$failed_events"
failed_size="$(wc -c < "$failed_events" | tr -d ' ')"
if [[ "$failed_size" -lt 20000 ]]; then
    echo "  ok   and the tail is bounded ($failed_size bytes, not the 50 KB printed)"
else
    echo "  FAIL the failed command's output was archived in full ($failed_size bytes)"
    failures=$((failures + 1))
fi

echo "an archive that cannot be written does not delete the round's evidence"
# The caller reads the stamp from a command substitution, so a failed copy under errexit is invisible
# inside it. With the archive unwritable the round must still keep its capture file and say so.
mkdir -p .agent-review/rounds && chmod 500 .agent-review/rounds
run fail
chmod 700 .agent-review/rounds
check "the round is reported as failed" 1 "exited 1" "$tmp/err.txt"
expect "and the report says the archive is incomplete" "archive is incomplete" "$tmp/err.txt"
if [[ -s .agent-review/round-events.jsonl ]]; then
    echo "  ok   the capture file was kept"
else
    echo "  FAIL the capture file was deleted although it was not archived"
    failures=$((failures + 1))
fi
kept_copy="$(ls .agent-review/round-events.jsonl 2>/dev/null)"

echo "the next round sets an unarchived capture file aside instead of overwriting it"
# Survival of the file above is only half the promise: the round after it used to delete whatever it
# found. It is moved to unarchived/ under a stamp, the round proceeds, and nothing says it was archived.
run ok
check "the next round still runs after an incomplete archive" 0 - "$tmp/out.txt"
expect "and says it kept the earlier evidence" "Kept an unarchived capture file" "$tmp/err.txt"
kept_stamp="$(ls .agent-review/unarchived/*/round-events.jsonl 2>/dev/null | head -1)"
if [[ -n "$kept_stamp" && -s "$kept_stamp" ]]; then
    echo "  ok   the earlier round's capture file survives under its own stamp"
else
    echo "  FAIL the earlier round's capture file was not set aside ($kept_copy)"
    failures=$((failures + 1))
fi

echo "two sets of evidence preserved in the same second both survive"
# A second-resolution stamp is not unique, and mv overwrites silently: two failures inside one second
# and the second 'kept' call destroyed the first. mkdir serves as the reservation and fails on an
# existing name, so the second set gets a fresh directory.
#
# The collision is made certain rather than hoped for: a date(1) earlier on PATH returns the same
# instant for every call, so both runs land on one stamp whatever the wall clock does. Without that,
# two runs a second apart would never take the suffix branch and this case could pass on a broken
# implementation.
mkdir -p "$tmp/datebin"
# BSD date answers -j/-f, GNU date answers -d; whichever this machine has, the stamp must come out the
# same on every call, so both spellings are tried before giving up on the fixture.
cat > "$tmp/datebin/date" <<'FAKEDATE'
#!/bin/sh
if /bin/date -j -f '%Y-%m-%d %H:%M:%S' '2026-01-01 00:00:00' -u +%Y%m%dT%H%M%SZ 2>/dev/null; then
    exit 0
fi
exec /bin/date -u -d '2026-01-01 00:00:00' +%Y%m%dT%H%M%SZ
FAKEDATE
chmod +x "$tmp/datebin/date"
rm -rf .agent-review/unarchived
printf 'first round\n' > .agent-review/round-events.jsonl
PATH="$tmp/datebin:$tmp/bin:$PATH" STUB_MODE=ok ./scripts/agent-review.sh >"$tmp/out.txt" 2>"$tmp/err.txt"
echo $? > "$tmp/exit.txt"
check "the first preserve runs" 0 - "$tmp/exit.txt"
printf 'second round\n' > .agent-review/round-events.jsonl
PATH="$tmp/datebin:$tmp/bin:$PATH" STUB_MODE=ok ./scripts/agent-review.sh >"$tmp/out.txt" 2>"$tmp/err.txt"
echo $? > "$tmp/exit.txt"
check "the second preserve runs" 0 - "$tmp/exit.txt"
if grep -q 'fakedate\|exec /bin/date' "$tmp/err.txt"; then
    echo "  FAIL the stub date did not run ($(cat "$tmp/err.txt" | head -2))"
    failures=$((failures + 1))
fi
kept_count="$(ls .agent-review/unarchived/*/round-events.jsonl 2>/dev/null | wc -l | tr -d ' ')"
if [[ "$kept_count" -ge 2 ]] \
    && grep -q 'first round' .agent-review/unarchived/*/round-events.jsonl \
    && grep -q 'second round' .agent-review/unarchived/*/round-events.jsonl; then
    echo "  ok   both sets are kept, with their own contents ($kept_count files)"
else
    echo "  FAIL the second preserved set overwrote the first ($kept_count files)"
    failures=$((failures + 1))
fi
if [[ -e .agent-review/round-events.jsonl ]]; then
    echo "  FAIL this round's capture file was not cleared"
    failures=$((failures + 1))
else
    echo "  ok   and this round starts from a clean capture file"
fi
# Refusing beats destroying: when the file cannot be moved, the round does not start at all.
printf 'x\n' > .agent-review/round-events.jsonl
rm -rf .agent-review/unarchived && : > .agent-review/unarchived
run ok
check "a round that cannot preserve the old evidence refuses to start" 74 "could not be set aside" "$tmp/err.txt"
expect_absent "and does not report a review" "REVIEW-STATUS: PASS" "$tmp/out.txt"
rm -f .agent-review/unarchived .agent-review/round-events.jsonl

echo "a draft left in the root belongs to the notes, not the state"
# A brief or a --note-file draft typed straight into .agent-review/ used to sit next to the state files
# for ever. It is moved, never deleted, and a --note-file naming its old path still reads it.
printf '# 方案草稿\n' > .agent-review/brief-selftest.md
run ok
expect_value "the draft was moved under notes/" "# 方案草稿" .agent-review/notes/brief-selftest.md
if [[ -e .agent-review/brief-selftest.md ]]; then
    echo "  FAIL the draft is still in the root"
    failures=$((failures + 1))
else
    echo "  ok   and is no longer in the root"
fi
rm -f .agent-review/notes/brief-selftest.md
printf '# 旧路径\n' > .agent-review/note-old-path.md
run ok --note-file .agent-review/note-old-path.md
check "a --note-file naming the old root path still reads it" 0 - "$tmp/out.txt"
expect "the note reaches the prompt from notes/" "旧路径" "$tmp/last-prompt.txt"

echo "a relative --note-file"
echo "备注内容：只审脚本。" > scripts/note.txt
set +e
(cd scripts && STUB_MODE=ok PATH="$tmp/bin:$PATH" ./agent-review.sh --note-file ./note.txt >/dev/null 2>&1)
echo $? > "$tmp/exit.txt"
set -e
check "it is read from the caller's directory" 0 - "$tmp/out.txt"
expect "the note reaches the prompt" "只审脚本" "$tmp/last-prompt.txt"

echo "a tree with nothing to review"
# From here on the reviewed tree must stay clean, so the output goes to $scratch.
run_clean() {
    set +e
    STUB_MODE="${STUB_MODE:-ok}" PATH="$tmp/bin:$PATH" ./scripts/agent-review.sh "$@" >"$scratch/out.txt" 2>"$scratch/err.txt"
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
    OMT_REVIEW_REVIEWER=codex STUB_MODE=ok PATH="$tmp/bin:$PATH" "$tmp/scripts/agent-review.sh" >"$scratch/out.txt" 2>"$scratch/err.txt"
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
