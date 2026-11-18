#!/usr/bin/env bash
#
# Independent review of the current working tree (codex, pi or none).
#
#   scripts/agent-review.sh                     first review, or a re-review in the same session
#   scripts/agent-review.sh --retry             resume after a round that produced no verdict
#   scripts/agent-review.sh --fresh             start a new session when the old one is gone
#   scripts/agent-review.sh --note "<text>"     the implementation agent's own words to the reviewer
#   scripts/agent-review.sh --note-file <path>  the same, read from a file
#   scripts/agent-review.sh --task <slug>       this task's reviewer session (see below)
#   scripts/agent-review.sh --tasks             list the sessions this repository holds
#   scripts/agent-review.sh --timeout <sec>     bound a run that hangs (default 1800, 0 disables)
#   scripts/agent-review.sh --finish            clear this task's reviewer session for the next task
#
# One reviewer session belongs to one task. Two tasks reviewed in this repository at the same time
# must not resume each other's session, read each other's findings, clear each other's session or wait
# on each other's lock, so everything kept below is keyed by task: --task <slug>, or OMT_REVIEW_TASK
# for a harness that wants to say it once. A task key is a short slug for the work (the branch name,
# or a one-word task name); it is lowercased, so a key is not two keys on a case-insensitive
# filesystem. Every round of a task — the review, the re-reviews after fixes, the retries, the design
# round, and the --finish that ends it — must pass the same key.
#
# The keyless case is the single-agent flow it always was: it keeps the top-level paths. A named task
# gets .agent-review/tasks/<key>/ with a session, a lock, an archive and a round counter of its own.
# Naming separates what the reviewer remembers; it does not separate the diff, so two agents sharing
# one working tree still put both tasks' changes in front of the reviewer — a tree per task is what
# makes two reviews independent.
#
# The reviewer and its settings are named by agent-review.toml at the repository root. The top-level
# `reviewer` key is required and has no default: "codex", "pi" or "none". codex settings live in
# [codex] (model / fast / reasoning_effort) and pi settings in [pi] (provider / model / thinking),
# required when reviewer = "pi". `fast = true` asks Codex for Fast mode, which it spells service_tier
# "priority"; `fast = false` sends service_tier "default" explicitly, so a Fast mode configured in
# ~/.codex/config.toml or .codex/config.toml cannot leak into a review. An omitted codex
# reasoning_effort sends no model_reasoning_effort flag, so the model default or a globally configured
# level stays in force. pi runs non-interactively in JSON mode with only read/bash/grep/find/ls tools
# and resumes one session per task. `none` is an explicit no-op that exits 0. Every setting has an
# environment override (OMT_REVIEW_REVIEWER, OMT_REVIEW_MODEL, OMT_REVIEW_FAST,
# OMT_REVIEW_REASONING_EFFORT, OMT_REVIEW_PI_PROVIDER, OMT_REVIEW_PI_MODEL, OMT_REVIEW_PI_THINKING)
# for a harness that wants to try another backend or setting without editing a tracked file. The
# per-turn settings are chosen again on a resumed round.
#
# The reviewer is asked to write its findings in Chinese, with the severity tokens left in English.
#
# A tree with no change relative to HEAD is refused (exit 65, --retry exempt): a round that reviewed
# nothing must not be counted as a review that passed.
#
# A round only counts as a review when the selected reviewer actually finished one. Anything else — a transport
# failure, a turn that never completed, no final message, a message that is not a verdict, a timeout,
# an interruption — exits non-zero with the reason, keeps the session, and archives that round's
# evidence, so that a retry resumes the same reviewer instead of starting a second one that remembers
# nothing.
#
#   .agent-review/codex-session-id   the active reviewer session (codex or pi), kept across failures
#                                    and interruptions
#   .agent-review/session-backend    which backend issued that session id, so a switch drops it
#   .agent-review/last-review.txt    the newest readable review (also printed)
#   .agent-review/last-message.txt   the CLI's own final-message file for the newest run
#   .agent-review/last-stderr.txt    the CLI's stderr for the newest run
#   .agent-review/last-status.txt    what the classification pass made of the newest run
#   .agent-review/round-counter      the round number the archive names carry
#   .agent-review/session-log.tsv    every session's beginning, resumption, loss and deliberate end
#   .agent-review/rounds/            every attempt's summary events, message, stderr and outcome
#   .agent-review/notes/             design briefs and --note-file drafts, kept out of the root
#
# Each round's outcome names the reviewer and the settings that round ran under, with each setting's
# origin, so the configuration of an old round survives the next round overwriting last-status.txt.
#
# Those are the paths of the keyless task. A named task holds the same set under
# .agent-review/tasks/<key>/, so its session, lock, counter and archive are its own and a listing of
# .agent-review/rounds/ keeps meaning the keyless task's rounds.

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

# The reviewer and its settings are resolved further down, from agent-review.toml and the environment
# overrides. They are deliberately not read here: --help, --tasks and --finish must keep working when
# that file is broken, and those three return before the resolver runs. The values below are what a
# signal arriving before the resolver sees, so that the archive of an interrupted round never reads an
# unset variable. The reviewer has no default: it comes from the file or OMT_REVIEW_REVIEWER, and an
# empty value refuses the round rather than quietly picking one.
REVIEWER=""
REVIEWER_SOURCE=""
DEFAULT_MODEL="gpt-6.2-sol"
MODEL="$DEFAULT_MODEL"
MODEL_SOURCE="the built-in default"
FAST=0
FAST_SOURCE="the built-in default"
SERVICE_TIER="default"
SERVICE_TIER_REJECTED="no"
# Unset by default, and an unset effort is sent as no flag at all rather than as a level: codex has no
# neutral value that would override a globally configured level, and inventing one would run the review
# at an effort the file never stated. The archive records the literal "-" for it, which no valid effort
# can collide with.
REASONING_EFFORT=""
REASONING_EFFORT_SOURCE="the built-in default"
REASONING_EFFORT_REJECTED="no"
# The pi backend's settings. Empty means "no flag", so pi falls back to its own configured default for
# that one setting; the file names all three so a review cannot silently run on a machine-wide default.
PI_PROVIDER=""
PI_PROVIDER_SOURCE="the built-in default"
PI_MODEL=""
PI_MODEL_SOURCE="the built-in default"
PI_THINKING=""
PI_THINKING_SOURCE="the built-in default"
CONFIG_FILE="$ROOT/agent-review.toml"

BASE_DIR="$ROOT/.agent-review"
TASKS_DIR="$BASE_DIR/tasks"
# Drafts that --note-file reads belong to the task they describe, not to the root of this directory:
# a reader that lists the root shallowly should see the state, not every brief of every past task.
NOTES_DIR="$BASE_DIR/notes"

# The conclusion is one authoritative value, so it is replaced, not appended: a signal that arrives while the
# round is being archived must be able to turn this round's `concluded=yes` into `concluded=no`.
# One path per field is set below, once the task key has decided which directory this round works in; the
# function reads $STATUS_FILE when it is called, never here.
set_concluded() {
    local value="$1" tmp
    tmp="$(mktemp)"
    if [[ -f "$STATUS_FILE" ]]; then
        grep -v '^concluded=' "$STATUS_FILE" > "$tmp" || true
    fi
    echo "concluded=$value" >> "$tmp"
    mv "$tmp" "$STATUS_FILE"
}

# The session log is append-only, and it answers the one question an outcome file cannot: whether the
# session changed because the task ended on purpose (--finish) or because the old one was lost. Both
# leave `reason=none` in the outcome, so without this record a silent loss and a clean hand-off are
# indistinguishable after the fact. Timestamps are UTC so they sort with the archive's round stamps.
#   timestamp  event  session  round  kind
# `event` is one of new, resumed, restarted, lost, finished, discarded.
log_session_event() {
    local dir="$1" event="$2" session="$3" round="$4" kind="$5"
    printf '%s\t%s\t%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$event" "${session:--}" \
        "${round:--}" "${kind:--}" >> "$dir/session-log.tsv"
}

# How many rounds of one kind the *current* session has already run. The log is the only place that
# knows: codex-session-id says which session, not how much of its context is already spent. Legacy
# state directories predate the log and answer 0, which is the honest answer for "not recorded".
#
# Only events that stand for a round attempt advance the count. `lost` is skipped because it is the
# second half of a `resumed` that already counted -- the attempt is one round, not two -- and a
# `finished`/`discarded` resets to zero because the session it counted is over.
session_rounds_so_far() {
    local dir="$1" kind="$2" n=0 event k
    [[ -f "$dir/session-log.tsv" ]] || { echo 0; return; }
    while IFS=$'\t' read -r _ event _ _ k; do
        case "$event" in
            finished|discarded) n=0; continue ;;
            new|restarted)      n=0 ;;
            lost)               continue ;;
        esac
        [[ "$k" == "$kind" ]] && n=$((n + 1))
    done < "$dir/session-log.tsv"
    echo "$n"
}

# The number this round will carry in both the log and the archive. Decided once, before the CLI runs,
# because the two used to derive it separately and disagree: the log read the counter before the round
# and the archive wrote it after, so every entry pointed one round off -- a session event could not be
# matched to the outcome, stderr and events of the attempt it described. `history_recorded` keeps a
# legacy directory's first round numbered from where its archive really is.
next_round_number() {
    local counter="$STATE_DIR/round-counter"
    if [[ -f "$counter" ]]; then
        echo $(( $(cat "$counter") + 1 ))
    else
        echo 1
    fi
}

# Whether a session began and was never deliberately ended. This is what separates a first round
# ("new") from a session replaced without explanation ("restarted").
#
# The log answers it when it exists. For a state directory that predates the log, the archive does: a
# round's outcome names the session it ran on, so an outcome with a real id is evidence that a session
# existed. An attempt whose CLI died before reporting thread.started leaves `session=none`, which is
# evidence of nothing -- and treating it as evidence is the defect: it would make the *next* real
# session look like it replaced a session that never was.
has_unfinished_session() {
    local dir="$1" last
    if [[ -f "$dir/session-log.tsv" ]]; then
        last="$(tail -1 "$dir/session-log.tsv" 2>/dev/null | cut -f2 || true)"
        case "$last" in
            finished|discarded) return 1 ;;
            *)                  return 0 ;;
        esac
    fi
    # No log: the archive is the only evidence, and only a recorded session counts. `session=none` is
    # what an attempt that never established one wrote, so it proves nothing; a real id anywhere is
    # proof that a session existed, even alongside such attempts. Counted rather than pattern-matched,
    # because a pattern that excludes "none" by its first letter would also exclude a real id.
    [[ -d "$dir/rounds" ]] || return 1
    local total real
    total="$(grep -h '^session=' "$dir"/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d '[:space:]')"
    real="$(grep -h '^session=none$' "$dir"/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d '[:space:]')"
    [[ "$total" -gt "$real" ]]
}

usage() {
    cat >&2 <<'EOF'
usage: scripts/agent-review.sh [--task <slug>] [--design] [--retry] [--fresh] [--note <text>]
                               [--note-file <path>] [--timeout <seconds>] [--finish] [--tasks]

  (no options)        review, or re-review the same session after fixes
  --task <slug>       this task's reviewer session: its own session, lock, counter and archive under
                      .agent-review/tasks/<slug>/. Use the same slug for every round of the task.
                      Lowercased; letters, digits, '.', '_' and '-'. OMT_REVIEW_TASK sets the
                      same thing for a harness that wants to say it once; --task wins over it.
                      Without either, the task is the keyless one and keeps the top-level paths;
                      'default' is the label that task is listed under and is not a key to take.
  --tasks             list the sessions this repository holds: key, session, kind, rounds, running,
                      state (open/lost/closed, from the session log) and rounds spent in that session
  --design            put the approach to the reviewer before any code is written; ends with
                      PLAN-STATUS instead of REVIEW-STATUS and needs no change in the tree
  --retry             the previous round produced no verdict; resume the same session and say so
  --fresh             start a new session (the old one is gone or its memory is to be dropped)
  --note <text>       put the implementation agent's own words to the reviewer
  --note-file <path>  the same, read from a file (both may be given; they are joined)
  --timeout <sec>     bound a run that has not finished in this long (default 1800, 0 = none)
  --finish            clear this task's reviewer session: the user's call, not the agent's

The reviewer is chosen by the top-level `reviewer` key in agent-review.toml at the repository root:
"codex", "pi" or "none". There is no default; a file that does not name one, or a missing file,
refuses the round rather than guessing. "none" is an explicit no-op that exits 0 (the rest of the file is still validated). codex settings live
in [codex] (model / fast / reasoning_effort); pi settings live in [pi] (provider / model / thinking),
and all three are required when reviewer = "pi". A bad value, an unknown key or an unreadable file
refuses the round instead of being ignored, because a review that silently ran on another reviewer or
other settings is not the review its configuration describes. --tasks and --finish never read it, so a
broken file cannot make a session unclearable. OMT_REVIEW_REVIEWER, OMT_REVIEW_MODEL, OMT_REVIEW_FAST,
OMT_REVIEW_REASONING_EFFORT, OMT_REVIEW_PI_PROVIDER, OMT_REVIEW_PI_MODEL and OMT_REVIEW_PI_THINKING
override one setting each and take precedence over the file.

The session is kept until the user says the task is done, so that the reviewer remembers its own
findings across the rounds of one task. --finish is the agent's only way to end it early and it is
therefore run on the user's word, never on the agent's judgement; an agent that clears it on its own
loses the reviewer's memory of findings that are still open. Running past the task's three-round
budget makes every round more expensive, so the round that starts over budget says so.
EOF
}

note=""
note_file=""
task_arg=""
task_given=0
list_tasks=0
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
        --task)
            [[ $# -ge 2 ]] || { usage; exit 64; }
            task_arg="$2"
            # Recorded separately from the value: an empty --task is a malformed key, not "no task".
            task_given=1
            shift 2
            ;;
        --tasks)
            list_tasks=1
            shift
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

# A listing of the sessions this repository holds, for the question a keyed workflow raises: whose
# session is which, and is one of them running. Read-only, so it takes no lock and needs no CLI — and
# it is answered before any key is resolved, because a listing is not a round and must not be refused
# on account of a malformed key in the environment.
if [[ "$list_tasks" == 1 ]]; then
    if [[ -n "$note" || -n "$note_file" || "$task_given" == 1 || "$design" == 1 || "$retry" == 1 \
        || "$fresh" == 1 || "$finish" == 1 ]]; then
        echo "--tasks lists the sessions this repository holds; it takes no task and no round of its own." >&2
        exit 64
    fi
    task_row() {
        local key="$1" dir="$2" session kind rounds running health last_event in_session
        session="$(cat "$dir/codex-session-id" 2>/dev/null || true)"
        kind="$(cat "$dir/last-round-kind" 2>/dev/null || true)"
        rounds=0
        if [[ -d "$dir/rounds" ]]; then
            rounds="$(find "$dir/rounds" -name '*-outcome.txt' 2>/dev/null | wc -l | tr -d '[:space:]')"
        fi
        running="no"
        [[ -d "$dir/lock" ]] && running="yes"
        # The health column is what the outcome files cannot say: whether the task is still on a
        # session, whether that session is a replacement, and how much context it has already spent.
        # `restarted` is open because a replacement session was created; `lost` is not, because the
        # resume failed and the remembered session is gone.
        health="none"
        last_event=""
        if [[ -f "$dir/session-log.tsv" ]]; then
            last_event="$(tail -1 "$dir/session-log.tsv" 2>/dev/null | cut -f2 || true)"
            case "$last_event" in
                new|resumed|restarted) health="open" ;;
                lost)                  health="lost" ;;
                finished|discarded)    health="closed" ;;
            esac
        elif [[ -n "$session" ]]; then
            # A state directory that predates the log: the pointer is the only evidence there is, and
            # reading it as closed would hide a session that is genuinely still open -- which is the
            # question the column exists to answer before a cmsg.
            health="open"
        fi
        in_session="-"
        if [[ -f "$dir/session-log.tsv" ]]; then
            in_session=0
            in_session="$(session_rounds_so_far "$dir" "${kind:-review}")"
        fi
        printf '%-20s %-38s %-7s %-7s %-7s %-9s %s\n' "$key" "${session:-none}" "${kind:--}" \
            "$rounds" "$running" "$health" "$in_session"
    }
    printf '%-20s %-38s %-7s %-7s %-7s %-9s %s\n' "task" "session" "kind" "rounds" "running" "state" "in-session"
    task_row "default" "$BASE_DIR"
    for dir in "$TASKS_DIR"/*/; do
        [[ -d "$dir" ]] || continue
        task_row "$(basename "$dir")" "${dir%/}"
    done
    exit 0
fi

# Which reviewer session this round belongs to. The key is what keeps two tasks apart in one
# repository, so it is resolved before anything is read or written: inside a key every round of that
# task finds its own session, and across keys no round can find another task's.
if [[ "$task_given" == 1 ]]; then
    TASK_KEY="$task_arg"
    TASK_SOURCE="--task"
elif [[ -n "${OMT_REVIEW_TASK:-}" ]]; then
    TASK_KEY="$OMT_REVIEW_TASK"
    TASK_SOURCE="OMT_REVIEW_TASK"
else
    TASK_KEY="default"
    TASK_SOURCE=""
fi
# The value is validated as it was given, before anything is normalised. A command substitution
# strips trailing newlines and a shell may strip them again on the way in, so a key like $'alpha\n' is
# checked here as the two-name string it is: accepting it would silently act on the task named 'alpha',
# which is the mix-up the keys exist to prevent. Uppercase is allowed at this point and only for a key
# about to be lowercased.
if [[ ! "$TASK_KEY" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ ]]; then
    echo "Invalid task key '${TASK_KEY}' (from ${TASK_SOURCE:-the default}): a task key is a short slug" >&2
    echo "of letters, digits, '.', '_' and '-', starting with a letter or digit, at most 64 characters." >&2
    exit 64
fi
# Lowercased, because a case-insensitive filesystem would otherwise map two spellings onto one
# directory and merge two tasks without saying so.
TASK_KEY="$(printf '%s' "$TASK_KEY" | tr '[:upper:]' '[:lower:]')"

# 'default' is the name the keyless task is listed under, so it is not a key a caller may take: --task
# default would resume, block on or clear the session of the agent that named nothing, which is exactly
# the interference the keys are here to stop. The other two are the state directory's own names.
if [[ -n "$TASK_SOURCE" ]]; then
    case "$TASK_KEY" in
        default|tasks|rounds)
            echo "Invalid task key '$TASK_KEY' (from $TASK_SOURCE): 'default' is the label of the task" >&2
            echo "that names no key, and 'tasks' and 'rounds' are the state directory's own names." >&2
            echo "Give this task a slug of its own, or omit the key to work in the keyless task." >&2
            exit 64
            ;;
    esac
fi

if [[ "$TASK_KEY" == "default" ]]; then
    STATE_DIR="$BASE_DIR"
    STATE_LABEL=".agent-review"
else
    STATE_DIR="$TASKS_DIR/$TASK_KEY"
    STATE_LABEL=".agent-review/tasks/$TASK_KEY"
fi

SESSION_FILE="$STATE_DIR/codex-session-id"
# The backend that made the remembered session. A session id only means something to the CLI that
# issued it, so a reviewer switch has to drop the pointer rather than hand it to the other backend.
SESSION_BACKEND_FILE="$STATE_DIR/session-backend"
OUTPUT_FILE="$STATE_DIR/last-review.txt"
# The round's own capture file, not a `last-*` pointer: nothing is meant to read a finished round's
# stream from here, so it is named for the round and removed with it.
JSON_FILE="$STATE_DIR/round-events.jsonl"
MESSAGE_FILE="$STATE_DIR/last-message.txt"
STDERR_FILE="$STATE_DIR/last-stderr.txt"
STATUS_FILE="$STATE_DIR/last-status.txt"
KIND_FILE="$STATE_DIR/last-round-kind"
ROUNDS_DIR="$STATE_DIR/rounds"
LOCK_DIR="$STATE_DIR/lock"
TIMEOUT_MARKER="$STATE_DIR/timed-out"
SESSION_LOG="$STATE_DIR/session-log.tsv"

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

# One review at a time *per task*: two runs of one task would overwrite each other's session file and
# event stream. Two tasks are keyed apart, so their reviews may run side by side, which is the point of
# the keys.
release_lock() {
    rmdir "$LOCK_DIR" 2>/dev/null || true
}

# The key every round of this task has to name, printed where it can be read back out of a transcript:
# a round that resumed a session nobody expected is the failure this makes visible.
announce_task() {
    [[ -n "$TASK_SOURCE" ]] || return 0
    echo "task: $TASK_KEY (state: $STATE_LABEL, from $TASK_SOURCE)"
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
    # Recorded before the pointer is removed, and with the id it named: --finish is the *only* way a
    # session ends by decision, so it is the event that tells a later reader this hand-off was on
    # purpose rather than a session that went missing. Logged only when a session was actually held:
    # running --finish twice must not invent a second closure.
    if [[ -f "$SESSION_FILE" ]]; then
        # The round the session last ran, read from the counter rather than ROUND_NUMBER: --finish is
        # not a round, so numbering the closure with the next round would point at an attempt that does
        # not exist. It names the session being closed, which is what a later reader looks up.
        log_session_event "$STATE_DIR" finished "$(cat "$SESSION_FILE")" \
            "$(cat "$STATE_DIR/round-counter" 2>/dev/null || echo -)" "$(cat "$KIND_FILE" 2>/dev/null || echo -)"
    fi
    rm -f "$SESSION_FILE" "$SESSION_BACKEND_FILE" "$OUTPUT_FILE" "$STATE_DIR/last-plan.txt" "$KIND_FILE" \
        "$JSON_FILE" "$JSON_FILE.final-message" "$MESSAGE_FILE" "$STATUS_FILE" "$TIMEOUT_MARKER" \
        "$JSON_FILE.stream-message"
    if [[ -n "$TASK_SOURCE" ]]; then
        echo "Reviewer session cleared (task: $TASK_KEY)."
    else
        echo "Reviewer session cleared."
    fi
    exit 0
fi

if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    echo "Another review is already running (lock: $LOCK_DIR)." >&2
    echo "If nothing is reviewing, remove that directory and try again." >&2
    if [[ -z "$TASK_SOURCE" ]]; then
        echo "Two agents sharing this repository need a task key each: --task <slug>, or OMT_REVIEW_TASK." >&2
    fi
    exit 75
fi
trap release_lock EXIT

announce_task

# The reviewer and its settings, resolved here: after --tasks and --finish have returned (so a broken
# file cannot make a session unclearable), and before --fresh drops a session or this round's status
# and evidence files are truncated (so a refused configuration leaves the previous round intact).
#
# The reviewer is explicit and has no default. An unknown key, a wrong type, a missing reviewer, an
# unreadable file or a missing TOML reader refuses the round rather than falling back: "the reviewer
# ran on gpt-6.2-sol" is a claim about the review that a silent fallback would make false. Every
# message names the file and the offending key.
#
# A path that is *present* but is not a readable regular file -- a directory, a dangling or looping
# symlink -- is refused for the same reason. Treating it as "no configuration" is the silent fallback
# this function exists to prevent: a config delivered by symlink would stop applying, without a word,
# the moment its target went away.
MODEL_RE='^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'
# Lowercase only, because codex spells the levels lowercase and silently accepting "High" as "high"
# would mean the review ran at a level the file did not state. The value is checked for shape only;
# whether a model supports it is codex's answer, and a rejection is reported and archived, not ignored.
REASONING_RE='^[a-z][a-z0-9_-]{0,31}$'
# A provider name is pi's own label and may contain a space ("command code"); a pi model may be a
# provider/id or a glob pattern, so its name is checked for shape and no whitespace, not for existence.
PROVIDER_RE='^[A-Za-z0-9][A-Za-z0-9 ._:-]{0,127}$'
PI_MODEL_RE='^[A-Za-z0-9*][A-Za-z0-9._:/*-]{0,127}$'

resolve_settings() {
    if [[ -e "$CONFIG_FILE" || -L "$CONFIG_FILE" ]]; then
        # -e is false for a dangling symlink, so -L is asked as well: either way the path exists as
        # *something*, and anything that is not a readable regular file is an error rather than a default.
        if [[ ! -f "$CONFIG_FILE" || ! -r "$CONFIG_FILE" ]]; then
            echo "$CONFIG_FILE exists but is not a readable regular file (a directory, a dangling or" >&2
            echo "looping symlink, or a file this user cannot read), so its settings cannot be applied." >&2
            echo "Fix or remove it: a review must not silently run on defaults its configuration did not ask for." >&2
            exit 64
        fi
        local parsed status=0
        # The parser is a separate program because bash cannot read TOML. `2>&1` deliberately folds a
        # parser error into the captured text: it is printed verbatim below and never parsed as settings.
        parsed="$(python3 - "$CONFIG_FILE" "$DEFAULT_MODEL" <<'PY' 2>&1
import re
import sys

path, default_model = sys.argv[1], sys.argv[2]

try:
    import tomllib
except ModuleNotFoundError:
    print(
        "python3 %d.%d has no tomllib, so %s cannot be read; Python 3.11 or newer is needed for that "
        "file. Runs without it need no TOML reader." % (sys.version_info[0], sys.version_info[1], path)
    )
    raise SystemExit(1)

try:
    with open(path, "rb") as f:
        data = tomllib.load(f)
except (OSError, tomllib.TOMLDecodeError) as exc:
    print("cannot read %s: %s" % (path, exc))
    raise SystemExit(1)

# Only the documented keys are accepted: the top-level reviewer plus one table per backend. A typo
# here would change which reviewer runs, or with what settings, without saying so.
allowed_top = {"reviewer", "codex", "pi"}
allowed_codex = {"model", "fast", "reasoning_effort"}
allowed_pi = {"provider", "model", "thinking"}
unknown = sorted(set(data) - allowed_top)
if unknown:
    print(
        "unknown key %s in %s (only reviewer, [codex] and [pi] are read)"
        % (", ".join(repr(k) for k in unknown), path)
    )
    raise SystemExit(1)

reviewer = ""
if "reviewer" in data:
    reviewer = data["reviewer"]
    if not isinstance(reviewer, str) or reviewer not in ("codex", "pi", "none"):
        print("reviewer in %s must be one of codex, pi or none; got %r" % (path, reviewer))
        raise SystemExit(1)

codex = data.get("codex", {})
if not isinstance(codex, dict):
    print("[codex] in %s must be a table" % path)
    raise SystemExit(1)
unknown = sorted(set(codex) - allowed_codex)
if unknown:
    print(
        "unknown key %s in [codex] of %s (only model, fast and reasoning_effort are read)"
        % (", ".join(repr(k) for k in unknown), path)
    )
    raise SystemExit(1)

model, model_from_file = default_model, False
if "model" in codex:
    model = codex["model"]
    model_from_file = True
    if not isinstance(model, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,127}", model):
        print(
            "model in [codex] of %s must be a model name of letters, digits, '.', '_', ':' and '-', "
            "at most 128 characters; got %r" % (path, model)
        )
        raise SystemExit(1)

fast, fast_from_file = False, False
if "fast" in codex:
    fast = codex["fast"]
    fast_from_file = True
    if not isinstance(fast, bool):
        print("fast in [codex] of %s must be true or false; got %r" % (path, fast))
        raise SystemExit(1)

reasoning, reasoning_from_file = "", False
if "reasoning_effort" in codex:
    reasoning = codex["reasoning_effort"]
    reasoning_from_file = True
    if not isinstance(reasoning, str) or not re.fullmatch(r"[a-z][a-z0-9_-]{0,31}", reasoning):
        print(
            "reasoning_effort in [codex] of %s must be a lowercase effort name of letters, digits, "
            "'_' and '-' (for example low, medium, high, xhigh or max); got %r" % (path, reasoning)
        )
        raise SystemExit(1)

pi = data.get("pi", {})
if not isinstance(pi, dict):
    print("[pi] in %s must be a table" % path)
    raise SystemExit(1)
unknown = sorted(set(pi) - allowed_pi)
if unknown:
    print(
        "unknown key %s in [pi] of %s (only provider, model and thinking are read)"
        % (", ".join(repr(k) for k in unknown), path)
    )
    raise SystemExit(1)

provider, provider_from_file = "", False
if "provider" in pi:
    provider = pi["provider"]
    provider_from_file = True
    if not isinstance(provider, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 ._:-]{0,127}", provider):
        print(
            "provider in [pi] of %s must be a provider name of letters, digits, space, '.', '_', ':' "
            "and '-', at most 128 characters; got %r" % (path, provider)
        )
        raise SystemExit(1)

pi_model, pi_model_from_file = "", False
if "model" in pi:
    pi_model = pi["model"]
    pi_model_from_file = True
    if not isinstance(pi_model, str) or not re.fullmatch(r"[A-Za-z0-9*][A-Za-z0-9._:/*-]{0,127}", pi_model):
        print(
            "model in [pi] of %s must be a model id or pattern of letters, digits, '.', '_', ':', "
            "'/', '*' and '-', at most 128 characters; got %r" % (path, pi_model)
        )
        raise SystemExit(1)

# pi spells this setting thinking; the level set belongs to pi, and it is checked here because a
# value pi does not know must refuse rather than reach the CLI as a silent no-op.
thinking, thinking_from_file = "", False
if "thinking" in pi:
    thinking = pi["thinking"]
    thinking_from_file = True
    if thinking not in ("off", "minimal", "low", "medium", "high", "xhigh", "max"):
        print(
            "thinking in [pi] of %s must be one of off, minimal, low, medium, high, xhigh, max; got %r"
            % (path, thinking)
        )
        raise SystemExit(1)

if reviewer:
    print("reviewer\t%s\tfile" % reviewer)
print("model\t%s\t%s" % (model, "file" if model_from_file else "default"))
print("fast\t%s\t%s" % ("1" if fast else "0", "file" if fast_from_file else "default"))
if reasoning_from_file:
    print("reasoning_effort\t%s\tfile" % reasoning)
if provider_from_file:
    print("pi_provider\t%s\tfile" % provider)
if pi_model_from_file:
    print("pi_model\t%s\tfile" % pi_model)
if thinking_from_file:
    print("pi_thinking\t%s\tfile" % thinking)
PY
)" || status=$?
        if [[ "$status" -ne 0 ]]; then
            printf '%s\n' "$parsed" >&2
            echo "Refusing to review with settings that cannot be read; fix $CONFIG_FILE or remove it." >&2
            exit 64
        fi
        local key value origin
        while IFS=$'\t' read -r key value origin; do
            case "$key" in
                reviewer)
                    REVIEWER="$value"
                    REVIEWER_SOURCE="agent-review.toml"
                    ;;
                model)
                    MODEL="$value"
                    [[ "$origin" == file ]] && MODEL_SOURCE="agent-review.toml" || MODEL_SOURCE="the built-in default"
                    ;;
                fast)
                    [[ "$value" == 1 ]] && FAST=1 || FAST=0
                    [[ "$origin" == file ]] && FAST_SOURCE="agent-review.toml" || FAST_SOURCE="the built-in default"
                    ;;
                reasoning_effort)
                    REASONING_EFFORT="$value"
                    REASONING_EFFORT_SOURCE="agent-review.toml"
                    ;;
                pi_provider)
                    PI_PROVIDER="$value"
                    PI_PROVIDER_SOURCE="agent-review.toml"
                    ;;
                pi_model)
                    PI_MODEL="$value"
                    PI_MODEL_SOURCE="agent-review.toml"
                    ;;
                pi_thinking)
                    PI_THINKING="$value"
                    PI_THINKING_SOURCE="agent-review.toml"
                    ;;
            esac
        done <<< "$parsed"
    fi

    # The environment overrides one setting each and wins over the file, which is what makes the selftest
    # and a one-off comparison possible without editing a tracked file. They are validated like any other
    # value: an override that silently does nothing would be a lie about the review that ran.
    if [[ -n "${OMT_REVIEW_REVIEWER:-}" ]]; then
        case "$OMT_REVIEW_REVIEWER" in
            codex|pi|none) REVIEWER="$OMT_REVIEW_REVIEWER" ;;
            *)
                echo "OMT_REVIEW_REVIEWER takes codex, pi or none; got '$OMT_REVIEW_REVIEWER'." >&2
                exit 64
                ;;
        esac
        REVIEWER_SOURCE="OMT_REVIEW_REVIEWER"
    fi
    if [[ -n "${OMT_REVIEW_MODEL:-}" ]]; then
        if [[ ! "$OMT_REVIEW_MODEL" =~ $MODEL_RE ]]; then
            echo "OMT_REVIEW_MODEL is not a model name: '$OMT_REVIEW_MODEL'" >&2
            exit 64
        fi
        MODEL="$OMT_REVIEW_MODEL"
        MODEL_SOURCE="OMT_REVIEW_MODEL"
    fi
    if [[ -n "${OMT_REVIEW_FAST:-}" ]]; then
        case "$OMT_REVIEW_FAST" in
            1|true|yes|on)   FAST=1 ;;
            0|false|no|off)  FAST=0 ;;
            *)
                echo "OMT_REVIEW_FAST takes 1/0, true/false or yes/no; got '$OMT_REVIEW_FAST'." >&2
                exit 64
                ;;
        esac
        FAST_SOURCE="OMT_REVIEW_FAST"
    fi
    if [[ -n "${OMT_REVIEW_REASONING_EFFORT:-}" ]]; then
        if [[ ! "$OMT_REVIEW_REASONING_EFFORT" =~ $REASONING_RE ]]; then
            echo "OMT_REVIEW_REASONING_EFFORT is not a reasoning effort name: '$OMT_REVIEW_REASONING_EFFORT'" >&2
            exit 64
        fi
        REASONING_EFFORT="$OMT_REVIEW_REASONING_EFFORT"
        REASONING_EFFORT_SOURCE="OMT_REVIEW_REASONING_EFFORT"
    fi
    if [[ -n "${OMT_REVIEW_PI_PROVIDER:-}" ]]; then
        if [[ ! "$OMT_REVIEW_PI_PROVIDER" =~ $PROVIDER_RE ]]; then
            echo "OMT_REVIEW_PI_PROVIDER is not a provider name: '$OMT_REVIEW_PI_PROVIDER'" >&2
            exit 64
        fi
        PI_PROVIDER="$OMT_REVIEW_PI_PROVIDER"
        PI_PROVIDER_SOURCE="OMT_REVIEW_PI_PROVIDER"
    fi
    if [[ -n "${OMT_REVIEW_PI_MODEL:-}" ]]; then
        if [[ ! "$OMT_REVIEW_PI_MODEL" =~ $PI_MODEL_RE ]]; then
            echo "OMT_REVIEW_PI_MODEL is not a model id: '$OMT_REVIEW_PI_MODEL'" >&2
            exit 64
        fi
        PI_MODEL="$OMT_REVIEW_PI_MODEL"
        PI_MODEL_SOURCE="OMT_REVIEW_PI_MODEL"
    fi
    if [[ -n "${OMT_REVIEW_PI_THINKING:-}" ]]; then
        case "$OMT_REVIEW_PI_THINKING" in
            off|minimal|low|medium|high|xhigh|max) ;;
            *)
                echo "OMT_REVIEW_PI_THINKING takes off, minimal, low, medium, high, xhigh or max; got '$OMT_REVIEW_PI_THINKING'." >&2
                exit 64
                ;;
        esac
        PI_THINKING="$OMT_REVIEW_PI_THINKING"
        PI_THINKING_SOURCE="OMT_REVIEW_PI_THINKING"
    fi

    # The reviewer is explicit: there is no default. A file or an environment that names none refuses
    # the round rather than quietly running one, which is what a fallback would do without saying so.
    if [[ -z "$REVIEWER" ]]; then
        echo "No reviewer configured: set reviewer = \"codex\", \"pi\" or \"none\" at the top of $CONFIG_FILE," >&2
        echo "or set OMT_REVIEW_REVIEWER. The reviewer is explicit and has no default." >&2
        exit 64
    fi
    # pi takes one flag per setting, and a missing one would silently fall back to the machine-wide pi
    # configuration, so all three are required when pi is the reviewer.
    if [[ "$REVIEWER" == "pi" ]]; then
        missing=""
        [[ -n "$PI_PROVIDER" ]] || missing="provider"
        [[ -n "$PI_MODEL" ]] || missing="${missing:+$missing, }model"
        [[ -n "$PI_THINKING" ]] || missing="${missing:+$missing, }thinking"
        if [[ -n "$missing" ]]; then
            echo "reviewer = \"pi\" in $CONFIG_FILE needs [pi] $missing (or the OMT_REVIEW_PI_* overrides); it was not set." >&2
            exit 64
        fi
    fi

    # Fast mode is codex's service_tier "priority". Off is sent as an explicit "default" rather than
    # omitted, because an omitted setting inherits a Fast mode configured in ~/.codex/config.toml or in
    # the project's .codex/config.toml -- measured on codex-cli 0.162.1, where a global
    # `service_tier = "priority"` reaches the request with no -c flag given.
    if [[ "$FAST" == 1 ]]; then
        SERVICE_TIER="priority"
    else
        SERVICE_TIER="default"
    fi
}

resolve_settings
echo "reviewer: $REVIEWER ($REVIEWER_SOURCE)"
if [[ "$REVIEWER" == "pi" ]]; then
    echo "reviewer: pi provider=$PI_PROVIDER ($PI_PROVIDER_SOURCE), model=$PI_MODEL ($PI_MODEL_SOURCE), thinking=$PI_THINKING ($PI_THINKING_SOURCE)"
elif [[ "$REVIEWER" == "codex" ]]; then
    echo "reviewer: model=$MODEL ($MODEL_SOURCE), fast=$([[ "$FAST" == 1 ]] && echo on || echo off) ($FAST_SOURCE)"
    echo "reviewer: reasoning_effort=${REASONING_EFFORT:--} ($REASONING_EFFORT_SOURCE)"
fi

# reviewer = "none" is an explicit no-op and not a review: it starts nothing, records nothing and
# succeeds, so a caller that deliberately configured no reviewer is not failed for it. It runs before
# the CLI check and before the work-tree check, because neither applies when no reviewer is configured.
if [[ "$REVIEWER" == "none" ]]; then
    echo "reviewer = none: no reviewer is configured, so this round ran nothing."
    exit 0
fi

# The CLI is checked only now, once the reviewer that needs it is known. The earlier return points
# (--help, --tasks, --finish) and the none no-op never reach here, so a missing CLI cannot make a
# session unclearable and cannot turn a no-op into a failure.
if ! command -v "$REVIEWER" >/dev/null 2>&1; then
    echo "$REVIEWER CLI not found on PATH; install it or fix PATH before reviewing." >&2
    exit 127
fi

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

# This round's number, decided here and used by both the session log and archive_round, so a session
# event and the attempt it describes carry the same number. Set before the refusals below on purpose:
# a refused call archives nothing and logs nothing, and a number it never used costs nothing.
ROUND_NUMBER="$(next_round_number)"

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

# Briefs and note drafts typed straight into .agent-review/ belong to the task they describe, not to the
# root, which otherwise keeps one file per round of every past task. Moved rather than deleted: they are
# the author's own words, and --note-file still reads them from their new place. Only *.md files are
# touched, so no name the script writes (the last-* pointers, the counter, the session log) can move, and
# an existing notes/ file is never overwritten. bash 3.2 is why the listing is a heredoc and not a
# mapfile: macOS ships that shell, and an empty file list has to come out as an empty list, not as one
# entry named "".
move_root_drafts() {
    [[ -d "$BASE_DIR" ]] || return 0
    local -a drafts=()
    local f
    while IFS= read -r f; do
        [[ -n "$f" ]] && drafts+=("$f")
    done <<list
$(cd "$BASE_DIR" && find . -maxdepth 1 -type f -name '*.md' 2>/dev/null | sed 's|^\./||' | sort)
list
    [[ "${#drafts[@]}" -gt 0 ]] || return 0
    mkdir -p "$NOTES_DIR"
    for f in "${drafts[@]}"; do
        [[ -e "$NOTES_DIR/$f" ]] && continue
        mv "$BASE_DIR/$f" "$NOTES_DIR/$f" 2>/dev/null || true
    done
}
move_root_drafts

if [[ -n "$note_file" ]]; then
    # A draft named at the root that the move above just put under notes/ is read from there: the caller
    # typed the path from habit. Its 66 names the path as typed, so the message still makes sense to
    # the caller. Compared after resolving both sides, because git reports the repository through any
    # symlinked directory (/private/var vs /var on macOS) and a plain prefix test would then miss the
    # file it had just moved.
    note_typed="$note_file"
    note_resolved="$note_file"
    base_real="$(cd "$BASE_DIR" 2>/dev/null && pwd -P || printf '%s' "$BASE_DIR")"
    note_real_dir="$(cd "$(dirname "$note_resolved")" 2>/dev/null && pwd -P || true)"
    if [[ -n "$note_real_dir" && "$note_real_dir" == "$base_real" && ! -e "$note_resolved" ]]; then
        moved="$NOTES_DIR/$(basename "$note_resolved")"
        [[ -f "$moved" ]] && note_resolved="$moved"
    fi
    if [[ ! -f "$note_resolved" ]]; then
        echo "--note-file: not a file: $note_typed" >&2
        exit 66
    fi
    note_file="$note_resolved"
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
# Set when archive_round could not archive this round: the failure report must then point at the capture
# files it left in $STATE_DIR rather than at evidence that does not exist.
ARCHIVE_FAILED=0

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
    python3 - "$JSON_FILE" "$REVIEWER" <<'PY'
import json
import sys

path, reviewer = sys.argv[1], sys.argv[2]

with open(path, "r", encoding="utf-8") as f:
    for line in f:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue

        if reviewer == "pi":
            if event.get("type") == "session" and event.get("id"):
                print(event["id"])
                break
        elif event.get("type") == "thread.started" and event.get("thread_id"):
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
            printf '%s\n' "$REVIEWER" > "$SESSION_BACKEND_FILE"
            # The round created this session, so its event goes with the pointer, not after the CLI
            # returns: the capture file is removed once the round is archived, and a later read of it
            # would find nothing. Still idempotent -- log_round_session reads the log, not a flag.
            log_round_session
        fi
    fi
}

# Records which session this round began or resumed, once per round. Idempotent because two paths
# reach it: the normal one after the CLI returns, and the signal handler, which `exit`s before the
# normal path could log. An interrupted first round is exactly the case that needs it -- without this
# the log would open on a later round's `resumed` and the session's beginning would be unrecorded.
# The id is read from the pointer, not passed in, so both paths name the same session: the CLI's own
# thread.started, kept by keep_session by the time either path logs.
ROUND_SESSION_LOGGED=0
log_round_session() {
    [[ "$ROUND_SESSION_LOGGED" == 0 ]] || return 0
    [[ -n "${INTENDED_EVENT:-}" ]] || return 0
    local session
    # The pointer first: on a first round it does not exist until the CLI has answered, and the log
    # entry is worthless without the session it names. keep_session is a no-op when it already exists.
    keep_session
    session="$(cat "$SESSION_FILE" 2>/dev/null || true)"
    # No id means no session was established -- a first round whose CLI died before thread.started.
    # Recording an event for it would report a session that never existed as open and unclosable
    # (--finish has no pointer to clear, so the health view would never say closed). The attempt itself
    # is still archived under rounds/, which is where an attempt that had no session belongs.
    [[ -n "$session" ]] || return 0
    # Idempotence is read off the log, not off a flag set before or after the write. A flag set first
    # loses the entry when a signal lands between the flag and the append; a flag set last writes it
    # twice when the signal lands between the append and the flag, because the handler then sees the
    # round as unrecorded. This round's event pair is what decides: it can appear at most once.
    if [[ -f "$SESSION_LOG" ]] && awk -F'\t' -v e="$INTENDED_EVENT" -v r="$ROUND_NUMBER" \
        '$2 == e && $4 == r { found = 1 } END { exit !found }' "$SESSION_LOG"; then
        ROUND_SESSION_LOGGED=1
        return 0
    fi
    log_session_event "$STATE_DIR" "$INTENDED_EVENT" "$session" "$ROUND_NUMBER" "$ROUND_KIND"
    ROUND_SESSION_LOGGED=1
}

# What in a round's capture file is worth keeping. The stream is mostly the reviewer's own work -- the
# output of every command it ran, once the tool ran it and once it reported the result -- and none of
# that is evidence about the review: measured over this repository's own archive it was about 97% of
# the bytes. The round boundaries, the session, the errors and the final message are what explains a
# round after the fact, so those are what the summary keeps. A line that will not parse is kept as it
# is, because an unreadable event may be exactly the thing that went wrong.
summarize_events() {
    local src="$1" dst="$2"
    python3 - "$src" "$dst" <<'PY'
import json
import sys

src, dst = sys.argv[1], sys.argv[2]
# codex's turn boundaries and pi's run boundaries. pi has no -o file, so its answer exists only in the
# stream: dropping message_end would leave an empty summary, which then falls back to the raw file --
# and a single unparsable line next to it would make the summary non-empty and take the answer with it.
KEEP_TYPES = {
    "thread.started", "thread.completed", "turn.started", "turn.completed", "turn.failed",
    "item.started", "item.updated", "error",
    "session", "agent_start", "agent_end", "turn_start", "turn_end", "agent_settled",
    "message_start", "message_end", "auto_retry_end",
}

with open(src, "r", encoding="utf-8", errors="replace") as r, open(dst, "w", encoding="utf-8") as w:
    for line in r:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            w.write(line)
            continue
        keep = False
        if not isinstance(event, dict):
            keep = True
        else:
            kind = event.get("type") or ""
            if kind in KEEP_TYPES or "error" in kind or "failed" in kind:
                keep = True
            else:
                item = event.get("item")
                # The tool call without what the tool printed: the command names what the reviewer
                # did, its output can be hundreds of kilobytes, and the same output appears again in
                # item.completed.
                if kind == "item.completed" and isinstance(item, dict):
                    if item.get("type") == "agent_message":
                        keep = True
                    elif item.get("type") == "error":
                        keep = True
                    elif item.get("type") == "command_execution":
                        # The command and how it ended. The result is what tells a reviewer's failed
                        # test run from a passing one after the fact: without exit_code the archive
                        # cannot explain the failure the round was diagnosing.
                        event["item"] = {
                            k: item[k]
                            for k in ("type", "command", "status", "exit_code")
                            if k in item
                        }
                        keep = True
                        if item.get("exit_code") not in (None, 0) and isinstance(item.get("aggregated_output"), str):
                            # A bounded head of what it printed, kept only for a failed command. The head,
                            # not the tail: a failure is reported where it starts (the first failing test,
                            # the error line), and repeated progress output is exactly what a tail would keep.
                            event["item"]["output_tail"] = item["aggregated_output"][:2000]
        if keep:
            w.write(json.dumps(event, ensure_ascii=False) + "\n")
PY
}

# Never archives an empty summary: a capture file the summary left with nothing in it is copied as it
# is, so "no events" cannot be mistaken for "the summary dropped them". A summary that could not be
# written at all is a failed copy, not an empty one: that reports failure, so the raw capture file is
# kept rather than quietly replaced by a copy that may also have failed.
archive_events() {
    local dst="$1"
    if ! summarize_events "$JSON_FILE" "$JSON_FILE.summary" 2>/dev/null; then
        rm -f "$JSON_FILE.summary"
        return 1
    fi
    if [[ -s "$JSON_FILE.summary" ]]; then
        mv "$JSON_FILE.summary" "$dst"
    else
        rm -f "$JSON_FILE.summary"
        cp "$JSON_FILE" "$dst"
    fi
}

# Every attempt's evidence is kept — a summary of its events, the final message, the CLI's stderr and
# the outcome — so that a failed round cannot take the previous round's review with it, and a failure
# can be explained later from what it actually printed. The round's own capture files are removed once
# they have been archived: a capture file that outlived its round would be read by the next one as if
# it were new output (this repository's empty stderr files were exactly that).
archive_round() {
    [[ "$ARCHIVED" == 0 ]] || return 0
    ARCHIVED=1
    local reason="$1" code="$2" stamp status=0
    # The number this round was already logged under, so the archive and the session log name the same
    # attempt. Recomputed only if a caller ever archives without one, which keeps the counter honest
    # rather than writing a stamp with an empty number.
    local round_number="${ROUND_NUMBER:-$(next_round_number)}"
    printf '%s\n' "$round_number" > "$STATE_DIR/round-counter"
    stamp="$(date -u +%Y%m%dT%H%M%SZ)-$(printf '%03d' "$round_number")"
    # Each copy is checked, not assumed. A failed copy used to be invisible -- the caller reads the
    # stamp from a command substitution, which does not inherit errexit -- so the capture files were
    # removed and the round reported success with nothing archived. Nothing is removed below unless
    # every copy succeeded.
    if [[ -s "$JSON_FILE" ]] && ! archive_events "$ROUNDS_DIR/$stamp-events.jsonl"; then
        status=1
    fi
    # The round's own final message, not the durable copy: last-message.txt holds the newest message of
    # any round, so copying it would archive another round's message under this round's stamp.
    if [[ -s "$JSON_FILE.final-message" ]] && ! cp "$JSON_FILE.final-message" "$ROUNDS_DIR/$stamp-$ROUND_KIND.txt"; then
        status=1
    fi
    # Copied only when it has content: the CLI writes its "reading additional input from stdin"
    # notice on every run, and an archive of that notice, once per round, explains nothing.
    if [[ -s "$STDERR_FILE" ]] && ! cp "$STDERR_FILE" "$ROUNDS_DIR/$stamp-stderr.txt"; then
        status=1
    fi
    {
        echo "reason=$reason"
        echo "exit=$code"
        echo "kind=$ROUND_KIND"
        echo "session=$(cat "$SESSION_FILE" 2>/dev/null || echo none)"
        # The configuration this round ran under, with each setting's origin: last-status.txt is
        # overwritten by the next round, so without this the model and the speed of a past round would
        # be unknowable. Read from the variables resolved for this round, never re-read from the file,
        # which may have changed in between.
        echo "reviewer=$REVIEWER"
        if [[ "$REVIEWER" == "pi" ]]; then
            echo "pi_provider=$PI_PROVIDER"
            echo "pi_provider_source=$PI_PROVIDER_SOURCE"
            echo "pi_model=$PI_MODEL"
            echo "pi_model_source=$PI_MODEL_SOURCE"
            echo "pi_thinking=$PI_THINKING"
            echo "pi_thinking_source=$PI_THINKING_SOURCE"
        else
            echo "model=$MODEL"
            echo "model_source=$MODEL_SOURCE"
            echo "fast=$([[ "$FAST" == 1 ]] && echo on || echo off)"
            echo "fast_source=$FAST_SOURCE"
            echo "service_tier=$SERVICE_TIER"
            echo "reasoning_effort=${REASONING_EFFORT:--}"
            echo "reasoning_effort_source=$REASONING_EFFORT_SOURCE"
            echo "reasoning_effort_rejected=${REASONING_EFFORT_REJECTED:-no}"
            echo "service_tier_rejected=${SERVICE_TIER_REJECTED:-no}"
        fi
    } > "$ROUNDS_DIR/$stamp-outcome.txt" || status=1
    # The stream fallback is the one capture file that survives its round: the failure report reads it
    # *after* the round is archived, and it describes a message the round did not confirm as final,
    # which is never archived. The next round's start removes it, so it cannot outlive its purpose.
    # last-message.txt stays too: the durable copy of the newest review, which --finish clears.
    if [[ "$status" -eq 0 ]]; then
        rm -f "$JSON_FILE" "$JSON_FILE.summary" "$JSON_FILE.final-message"
    fi
    echo "$stamp"
    if [[ "$status" -ne 0 ]]; then
        # Said on stderr, named by the stamp, so the round is not reported as archived when it is not.
        echo "The archive of round $stamp could not be written in full; the round's capture files are" >&2
        echo "kept at $STATE_DIR so nothing is lost. Check that $ROUNDS_DIR is writable." >&2
        return 1
    fi
    return 0
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
    log_round_session
    REASON="interrupted (signal $signal)"
    CODE="$signal"
    # Record this round as unfinished, so a later `--retry` resumes *it* rather than reading a verdict from an
    # earlier round that did conclude.
    set_concluded no
    rm -f "$OUTPUT_FILE"
    # Whatever the round's -o file holds was not counted as a completed message, but it is the last thing
    # the reader has of this round: promote it so the failure report and the retry can show it. This is
    # the one place outside the classification pass that writes last-message.txt, and it is the one place
    # where "unverified" is the point.
    [[ -s "$JSON_FILE.final-message" ]] && cp "$JSON_FILE.final-message" "$MESSAGE_FILE"
    archive_round "$REASON" "$CODE" >/dev/null || ARCHIVE_FAILED=1
    exit "$signal"
}

trap 'on_signal 130' INT
trap 'on_signal 143' TERM

if [[ "$fresh" == 1 && -f "$SESSION_FILE" ]]; then
    echo "Discarding the reviewer session (--fresh): its memory of earlier findings is gone." >&2
    # Distinct from `finished`: --fresh drops a session mid-task on purpose, so the log has to say the
    # memory was deliberately thrown away rather than that the task ended. Without the id the old
    # session would be unrecoverable from this record alone -- its rollout outlives the pointer, and
    # `codex exec resume <id>` can still reach it.
    log_session_event "$STATE_DIR" discarded "$(cat "$SESSION_FILE")" \
        "$(cat "$STATE_DIR/round-counter" 2>/dev/null || echo -)" "$(cat "$KIND_FILE" 2>/dev/null || echo -)"
    rm -f "$SESSION_FILE" "$SESSION_BACKEND_FILE"
fi

# A session id belongs to the backend that issued it: codex would report no rollout for a pi id, and pi
# would create a fresh session under a codex id, both silently. After an explicit reviewer switch the
# pointer is dropped on purpose, so the round starts a new session and the log says the old memory was
# thrown away rather than losing it without a word.
if [[ -f "$SESSION_FILE" ]]; then
    recorded_backend="codex"
    [[ -f "$SESSION_BACKEND_FILE" ]] && recorded_backend="$(cat "$SESSION_BACKEND_FILE")"
    if [[ "$recorded_backend" != "$REVIEWER" ]]; then
        echo "The remembered $recorded_backend session cannot be resumed by $REVIEWER; starting a new one." >&2
        log_session_event "$STATE_DIR" discarded "$(cat "$SESSION_FILE")" \
            "$(cat "$STATE_DIR/round-counter" 2>/dev/null || echo -)" "$(cat "$KIND_FILE" 2>/dev/null || echo -)"
        rm -f "$SESSION_FILE" "$SESSION_BACKEND_FILE"
    fi
fi

# A new round starts clean: a stale review must never be read as this round's, and the status record belongs to
# this round from here on. Without that, an interrupted round would leave the previous round's `concluded=yes`
# behind, and a retry would refuse to resume a round that in fact never concluded.
# last-message.txt is not among them: the CLI writes to its capture file, and only the classification
# pass copies a final message into last-message.txt, so a failed round cannot leave this round's
# message there before that pass has read one.
#
# A capture file still here belongs to a round whose archive failed (a successful archive removes it).
# Overwriting it would destroy the only copy of that round's evidence, so it is set aside in a
# directory of its own and the round refuses to start if even that is impossible. Only then are this
# round's names cleared.
#
# mkdir, not mv, makes the name unique: a second-resolution stamp is not unique (two failures within
# one second are easy to produce with --retry), and mv overwrites its destination silently. A directory
# mkdir refuses to create if it exists, so three quick failures leave three sets of evidence.
for stale in "$JSON_FILE" "$JSON_FILE.final-message" "$JSON_FILE.stream-message"; do
    [[ -e "$stale" ]] || continue
    dir="$STATE_DIR/unarchived/$(date -u +%Y%m%dT%H%M%SZ)-$(basename "$stale")"
    mkdir -p "$STATE_DIR/unarchived" 2>/dev/null || true
    n=0
    while [[ -e "$dir" ]]; do
        n=$((n + 1))
        [[ "$n" -gt 100 ]] && break
        dir="$STATE_DIR/unarchived/$(date -u +%Y%m%dT%H%M%SZ)-$(basename "$stale").$n"
    done
    keep="$dir/$(basename "$stale")"
    if ! mkdir "$dir" 2>/dev/null || ! mv "$stale" "$keep"; then
        echo "A previous round's capture file could not be set aside, and starting this round would" >&2
        echo "overwrite it: $stale" >&2
        echo "Copy or remove that file, or fix $dir, then run again. Nothing was reviewed." >&2
        exit 74
    fi
    echo "Kept an unarchived capture file from an earlier round: $keep" >&2
done
rm -f "$OUTPUT_FILE" "$TIMEOUT_MARKER"
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

本仓库的上下文：AGENTS.md 全文已在本会话开头给出，它是权威规范，评审按它判断——特别是 Testing tiers
（分层依据是**谁判定通过与否**，不是用什么传输；B 层发现必须晋升为 A 层断言，或在还不能断言时先补一个
可断言的状态字段）、Review budget and triage（3 轮预算、按影响而非标签分级）、以及 *What the reviewer is
asked to weigh here* 的四条检查项。不要改文件、不要代改代码、不要自行撤销结论。

只有这几点需要额外记住：

- 项目：`oh-my-tab`，macOS 菜单栏窗口切换器，Rust + objc2/AppKit/CoreGraphics 的 FFI，无 Swift bridge。
  重点检查：unsafe/FFI 生命周期与所有权、`msg_send!` 的选择器与字段名、主线程不变量（AppKit 与动态注册的
  Objective-C 回调必须在主线程）、`Mutex/RwLock` 的所有权与重入、动态 ObjC 类对 Rust 侧字段的依赖。
- 门禁是 `cargo fmt --check`、`cargo clippy`、`cargo test`、`cargo check --release`；评审者看不到跑了哪些
  门禁，需要时请在结论里明确索取。
- 代码与 `docs/design-style{,-en}.md` 冲突时**文档为准**，除非同一改动里改了文档并给出理由。

省着用上下文：优先 `git diff` 与定点读取，不要整文件读取——工具输出会一直留在上下文里，越读越贵。
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

# A named task says which one it is, so that a misuse — a round resuming a session nobody expected —
# is visible in the transcript. It names the session and nothing else: the key is a slug, not a scope,
# so the prompt must not ask the reviewer to sort the tree's changes by it. What is in scope for a task
# is what the note says, and the tree is what the reviewer reviews.
# It goes in before the status instruction, which has to stay the last thing in the prompt.
if [[ -n "$TASK_SOURCE" ]]; then
    TASK_NOTE="This review session belongs to task '$TASK_KEY'. The key names the session only: it does not select, narrow or filter which changes you review, and the working tree may hold work belonging to other tasks."
    if [[ "$design" == 1 ]]; then
        DESIGN_PROMPT="$DESIGN_PROMPT"$'\n\n'"$TASK_NOTE"
    else
        FIRST_REVIEW_PROMPT="$FIRST_REVIEW_PROMPT"$'\n\n'"$TASK_NOTE"
        REREVIEW_PROMPT="$REREVIEW_PROMPT"$'\n\n'"$TASK_NOTE"
        RETRY_PROMPT="$RETRY_PROMPT"$'\n\n'"$TASK_NOTE"
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

# Runs the selected reviewer CLI with its output captured, stdin pointed at /dev/null, and a watchdog
# that can actually end it. When stdin is not a terminal the CLI always tries to read "additional input"
# from it and says so on stderr; /dev/null makes that read return end-of-file at once, where an inherited
# pipe with no writer — how an agent usually invokes this script — could otherwise leave it waiting.
# stderr goes to a file rather than through a process substitution, which a sandbox that forbids /dev/fd
# would refuse. The program is an argument so codex and pi share one watchdog, one kill path and one sweep.
run_cli() {
    local program="$1"
    shift
    local status=0
    "$program" "$@" </dev/null > "$JSON_FILE" 2> "$STDERR_FILE" &
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

# Which session this round starts on, recorded *before* the CLI runs: the log is about what the round
# intended to do, and a round that dies on a lost session still has to say the session was lost. The
# `lost` case can only be told apart from a working resume after the CLI answers, so the intent is
# written here and corrected below when the CLI reports that the thread does not exist.
INTENDED_EVENT=""
if [[ -f "$SESSION_FILE" ]]; then
    INTENDED_EVENT="resumed"
elif has_unfinished_session "$STATE_DIR"; then
    # No pointer but the log shows a session that was never deliberately ended: it was replaced rather
    # than finished, and that unexplained gap is what this event names.
    INTENDED_EVENT="restarted"
else
    INTENDED_EVENT="new"
fi

# The budget warning. AGENTS.md budgets a task at three review rounds; a session that has already run
# that many is carrying a context that grows every round, and the round that finally hits --timeout
# loses its work. This only says so: ending a task is the user's call, so the round proceeds.
BUDGET_ROUNDS=3
if [[ "$ROUND_KIND" == "review" && "$INTENDED_EVENT" == "resumed" ]]; then
    spent="$(session_rounds_so_far "$STATE_DIR" review)"
    if [[ "$spent" -ge "$BUDGET_ROUNDS" ]]; then
        echo "Note: this session has already run $spent review rounds (AGENTS.md budgets $BUDGET_ROUNDS)." >&2
        echo "Its context grows every round, and a round that hits --timeout loses its work. When the task is" >&2
        echo "done, --finish closes it; to close it now, --finish then start again and hand the still-open" >&2
        echo "findings over in --note." >&2
    fi
fi

if [[ ! -f "$SESSION_FILE" ]]; then
    echo "Starting new $REVIEWER reviewer session..."
    status=0
    # `set --` rather than an array: macOS ships bash 3.2, where expanding an empty array under
    # `set -u` is an error. The argument list is built once per branch so both backends share the same
    # watchdog and the same capture files; the prompt is behind `--` so a leading dash is not an option.
    if [[ "$REVIEWER" == "pi" ]]; then
        set -- --mode json --print --no-approve --no-extensions --no-mcp --no-skills \
            --tools read,bash,grep,find,ls --provider "$PI_PROVIDER" --model "$PI_MODEL" \
            --thinking "$PI_THINKING" -- "$FIRST_PROMPT"
        run_cli pi "$@" || status=$?
    else
        set -- exec --model "$MODEL" -c service_tier="$SERVICE_TIER"
        if [[ -n "$REASONING_EFFORT" ]]; then
            set -- "$@" -c "model_reasoning_effort=$REASONING_EFFORT"
        fi
        # The turn's final message goes to a private file rather than the durable last-message.txt: only
        # the classification pass may make a message from this round official, so a round that never
        # completed leaves the previous round's last-message.txt alone. The capture files are removed
        # once the round has been archived.
        set -- "$@" --json -o "$JSON_FILE.final-message" "$FIRST_PROMPT"
        run_cli codex "$@" || status=$?
    fi
    # Logged after the CLI ran, because only then does the new session have an id: the record has to
    # name the session it began, or a later reader cannot follow it to its rollout. A round whose CLI
    # never created one is recorded with no id, which is the honest description of that attempt.
    log_round_session
else
    SESSION_ID="$(cat "$SESSION_FILE")"
    if [[ "$design" == 1 ]]; then
        echo "Resuming $REVIEWER reviewer session (design round):"
        PROMPT="$DESIGN_PROMPT"
    elif [[ "$retry" == 1 ]]; then
        echo "Retrying $REVIEWER reviewer session:"
        PROMPT="$RETRY_PROMPT"
    else
        echo "Resuming $REVIEWER reviewer session:"
        PROMPT="$REREVIEW_PROMPT"
    fi
    echo "$SESSION_ID"
    echo
    # Through the same idempotent entry point the signal path uses: a resume that is interrupted must
    # not end up recorded twice, once here and once by the handler.
    log_round_session
    status=0
    # A per-turn setting does not stick: codex resolves the tier and the effort per turn, and pi is
    # resumed explicitly, so every round states the settings again rather than assuming they carried over.
    if [[ "$REVIEWER" == "pi" ]]; then
        set -- --mode json --print --no-approve --no-extensions --no-mcp --no-skills \
            --tools read,bash,grep,find,ls --provider "$PI_PROVIDER" --model "$PI_MODEL" \
            --thinking "$PI_THINKING" --session-id "$SESSION_ID" -- "$PROMPT"
        run_cli pi "$@" || status=$?
    else
        set -- exec resume "$SESSION_ID" --model "$MODEL" -c service_tier="$SERVICE_TIER"
        if [[ -n "$REASONING_EFFORT" ]]; then
            set -- "$@" -c "model_reasoning_effort=$REASONING_EFFORT"
        fi
        set -- "$@" --json -o "$JSON_FILE.final-message" "$PROMPT"
        run_cli codex "$@" || status=$?
    fi
    # A resume can silently lose the memory it promised: codex fails the run when it no longer holds the
    # thread, while pi's --session-id creates a new session under the same id and only says so on stderr.
    # Both have to be recorded as `lost` rather than `resumed`, or the memory gap stays invisible.
    if [[ "$REVIEWER" == "codex" && "$status" -ne 0 ]] \
        && grep -qi 'no rollout found for thread id' "$STDERR_FILE" 2>/dev/null; then
        # Same round number as the `resumed` this attempt already logged: it is the second half of one
        # attempt, and session_rounds_so_far skips it for the same reason -- the round was tried once.
        log_session_event "$STATE_DIR" lost "$SESSION_ID" "$ROUND_NUMBER" "$ROUND_KIND"
        echo "The reviewer session $SESSION_ID is gone: the CLI has no rollout for it." >&2
        echo "The log records it as lost. --fresh starts a new one (its memory is lost); a retry cannot." >&2
    elif [[ "$REVIEWER" == "pi" ]] \
        && grep -qi 'No project session found with id' "$STDERR_FILE" 2>/dev/null; then
        log_session_event "$STATE_DIR" lost "$SESSION_ID" "$ROUND_NUMBER" "$ROUND_KIND"
        echo "The pi session $SESSION_ID no longer exists; pi started a new one under that id." >&2
        echo "Its memory of earlier findings is gone. The log records it as lost; --fresh drops it." >&2
    fi
fi

# One pass over the event stream: what happened, and what the final message was. codex's -o file is the
# authoritative message for that backend; pi has no -o file, so its last assistant message_end is, per
# its JSON-mode documentation, the authoritative final message and is written to the same file. A
# message that exists only in codex's event stream stays evidence rather than being promoted to a verdict.
#
# The last two arguments are the round's capture file for -o and the durable last-message.txt. The
# backend writes only the first; the second is written here, from whatever the round itself produced,
# so the failure report and a retry show this round's message rather than the previous round's. Keeping
# the backend off the durable path is what stops a round that never reached the CLI from erasing the
# last review, and it stops a stale last-message.txt being read back as this round's -o result.
python3 - "$REVIEWER" "$JSON_FILE" "$JSON_FILE.final-message" "$MESSAGE_FILE" "$STATUS_FILE" \
    "$MARKER_NAME" "$ROUND_KIND" <<'PY'
import json
import os
import re
import sys

reviewer, events_path, message_path, durable_path = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
status_path, marker_name, round_kind = sys.argv[5], sys.argv[6], sys.argv[7]

thread_id = ""
completed = False
errors = []
stream_message = ""
tier_rejected = False
reasoning_rejected = False
message = ""
incomplete = ""
# What the backend put in its -o file. "none" when there is no file at all: pi has none by design, so
# the stream's message is the round's; codex having none is the ordinary missing-message case.
final_message = "none"


def one_line(value):
    # A status file is key=value lines, so a provider error must not carry its own newlines into it.
    return " ".join(str(value).split())[:300]


if reviewer == "pi":
    # pi JSON mode names the run in a `session` header, ends a run with `agent_settled` (the one event
    # that says no retry, compaction recovery or queued message remains), and carries the authoritative
    # answer in the last assistant `message_end`. There is no -o file, so the message is written here and
    # the rest of the round treats both backends the same.
    settled = False
    aborted = False
    # The last assistant message decides, even when its text is empty: a truncated or errored final
    # answer must not be hidden by an earlier message that happened to carry the verdict.
    last_stop = ""
    saw_assistant = False
    try:
        # errors="replace": one malformed byte must cost that byte, not the whole round's classification.
        with open(events_path, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue

                kind = event.get("type") or ""
                if kind == "session":
                    thread_id = event.get("id") or thread_id
                elif kind == "agent_settled":
                    settled = True
                    aborted = bool(event.get("aborted"))
                elif kind == "auto_retry_end" and event.get("success") is False:
                    errors.append(one_line(event.get("finalError") or "the automatic retry failed"))
                elif kind == "message_update":
                    detail = event.get("assistantMessageEvent") or {}
                    if detail.get("type") == "error":
                        errors.append(one_line(detail.get("error")))
                elif kind == "message_end":
                    msg = event.get("message") or {}
                    if msg.get("role") == "assistant":
                        parts = [
                            block.get("text", "")
                            for block in (msg.get("content") or [])
                            if isinstance(block, dict) and block.get("type") == "text"
                        ]
                        stream_message = "".join(parts)
                        last_stop = str(msg.get("stopReason") or "")
                        saw_assistant = True
    except FileNotFoundError:
        pass
    # pi documents stopReason as pending | stop | length | toolUse | error | aborted | deferred. Only a
    # clean `stop` on the last assistant message is a finished answer: the other values are a failure, a
    # truncation, or a turn that still had work to do, and pi can reach agent_settled with any of them.
    if not settled:
        incomplete = "the pi turn did not finish (no agent_settled in the event stream)"
    elif aborted:
        incomplete = "the pi turn was aborted"
    elif not saw_assistant:
        incomplete = "the pi turn produced no assistant message"
    elif last_stop != "stop":
        incomplete = "the pi turn did not finish (the last assistant message stopped with '%s')" % (last_stop or "no stop reason")
    completed = settled and not aborted and saw_assistant and last_stop == "stop"
    message = stream_message.strip()
else:
    # codex reports a completed turn as an event, keeps a declined tier or effort as an error item, and
    # writes the final message to its own -o file, which is the authoritative source for one.
    try:
        # The same tolerance as the pi branch: undecodable bytes are replaced, never fatal.
        with open(events_path, "r", encoding="utf-8", errors="replace") as f:
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
                    if item.get("type") == "error":
                        # Both halves are required: the generic tail alone ("not advertised as supported
                        # for model") could belong to another setting, and attributing that to the service
                        # tier would archive a rejection that never happened. codex's own wording for this
                        # case is "Configured service tier `X` is not advertised as supported for model `Y`".
                        detail = str(item.get("message") or "")
                        if "service tier" in detail and "not advertised as supported for model" in detail:
                            tier_rejected = True
                        if "reasoning effort" in detail and "not advertised as supported for model" in detail:
                            reasoning_rejected = True
    except FileNotFoundError:
        pass

    try:
        with open(message_path, "r", encoding="utf-8") as f:
            message = f.read().strip()
    except FileNotFoundError:
        pass
    if message:
        final_message = message

# What the round's own -o file holds becomes the durable last-message.txt, so the failure report and a
# retry show this round's message rather than the previous round's. Writing it here, and not by pointing
# the CLI at the durable path, is what keeps a round that never reached the CLI from erasing the last
# review; what the message is *worth* is the verdict check below, not this copy.
if message:
    with open(durable_path, "w", encoding="utf-8") as f:
        f.write(message + "\n")
else:
    # No message of its own: the previous round's copy is not this round's evidence. Without this, a
    # round that produced nothing would have the last review printed as the message that arrived.
    try:
        os.remove(durable_path)
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
    f.write("incomplete=%s\n" % incomplete)
    f.write("error=%s\n" % (errors[0] if errors else ""))
    f.write("service_tier_rejected=%s\n" % ("yes" if tier_rejected else "no"))
    f.write("reasoning_effort_rejected=%s\n" % ("yes" if reasoning_rejected else "no"))
    f.write("message_chars=%d\n" % len(message))
    f.write("looks_like_verdict=%s\n" % ("yes" if looks_like_verdict else "no"))
    f.write("round_status=%s\n" % round_status)
    f.write("marker_name=%s\n" % marker_name)
    f.write("last_line=%s\n" % (last_line[:120] if last_line else ""))

# What the round's answer is, and therefore what may be archived as it. codex: the -o file and nothing
# else -- its event stream is evidence, never the verdict. pi has no -o file, so its last assistant
# message is the answer by the backend's own documentation.
if reviewer == "pi":
    if message:
        final_message = message
else:
    message = final_message.strip()

# A transcript-only message is shown, never archived as the answer. An empty -o file is the
# 'no final message' case whatever the stream says: treating the stream as the -o result turned a
# refused round into a passing one, which is the verdict check's whole job.
if stream_message.strip() and stream_message.strip() != final_message:
    # Read by the failure report below, and removed with the round's capture files once it is archived:
    # this was a durable last-review.jsonl.stream-message, which the next round had to clear to avoid
    # showing the previous round's message as this round's evidence.
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
# Whether codex declined the requested speed. It does not fail such a run, so this is the only evidence
# that "fast = on" did not actually take effect. Read before archive_round so the outcome carries it.
SERVICE_TIER_REJECTED="$(status_field service_tier_rejected)"
# The same conservative treatment for the effort: a run whose effort codex declined still produced a
# valid review, so the round keeps its verdict and its exit code and only the mismatch is surfaced.
REASONING_EFFORT_REJECTED="$(status_field reasoning_effort_rejected)"
# Why a pi round did not complete, in words the failure message can use directly (empty for codex).
INCOMPLETE="$(status_field incomplete)"
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
    REASON="$REVIEWER exited $status"
    CODE="$status"
elif [[ ! -s "$JSON_FILE" ]]; then
    REASON="$REVIEWER printed no events"
    CODE=1
elif [[ "$completed" != "yes" ]]; then
    if [[ -n "$INCOMPLETE" ]]; then
        REASON="$INCOMPLETE"
    else
        REASON="the turn did not finish (no turn.completed in the event stream)"
    fi
    CODE=1
elif [[ "$message_chars" -eq 0 ]]; then
    REASON="the turn produced no final message"
    CODE=1
elif [[ "$looks_like_verdict" != "yes" ]]; then
    REASON="the message does not end with the $MARKER_NAME line ($MARKER_VALUES)"
    CODE=1
fi

set_concluded "$([[ "$CODE" -eq 0 ]] && echo yes || echo no)"

stamp="$(archive_round "${REASON:-none}" "$CODE")" || ARCHIVE_FAILED=1

# The speed was requested, not delivered. The review is still valid -- codex answered at its default
# speed -- so the round keeps its verdict and its exit code, but the mismatch is never left silent.
if [[ "$SERVICE_TIER_REJECTED" == "yes" ]]; then
    echo >&2
    if [[ "$FAST" == 1 ]]; then
        echo "Note: codex did not accept the requested service tier '$SERVICE_TIER' for model '$MODEL' and" >&2
        echo "ran this round at its default speed. Fast mode (fast = true in agent-review.toml, from" >&2
        echo "$FAST_SOURCE) is therefore NOT confirmed for this round; the catalogue of tiers a model" >&2
        echo "supports is \`codex debug models\`." >&2
    else
        echo "Note: codex did not accept the requested service tier '$SERVICE_TIER'." >&2
    fi
fi

# The effort was requested, not delivered, and the review is still valid. Warned and archived rather
# than failed, and only reachable when one was actually configured: an unset effort sends no flag.
if [[ "$REASONING_EFFORT_REJECTED" == "yes" ]]; then
    echo >&2
    echo "Note: codex did not accept the requested reasoning effort '$REASONING_EFFORT' for model '$MODEL'" >&2
    echo "and ran this round without it. The level requested via $REASONING_EFFORT_SOURCE is therefore NOT" >&2
    echo "confirmed for this round; the levels a model supports are listed by \`codex debug models\`." >&2
fi

if [[ "$CODE" -eq 0 ]]; then
    # The classification pass has already written this round's completed message to last-message.txt, so
    # the readable copy is named from there: last-plan.txt for a design round, last-review.txt for a
    # review. One copy, under the name a reader expects.
    cp "$MESSAGE_FILE" "$OUTPUT_FILE"
    echo
    cat "$OUTPUT_FILE"
    exit 0
fi

echo >&2
echo "This round produced no review: $REASON." >&2
if [[ "$ARCHIVE_FAILED" == 1 ]]; then
    echo "Exit $CODE ($REVIEWER itself exited $status). The archive is incomplete; the round's capture" >&2
    echo "files are in $STATE_DIR and last-message.txt is unchanged." >&2
else
    echo "Exit $CODE ($REVIEWER itself exited $status). Evidence: $ROUNDS_DIR/$stamp-*" >&2
fi
if [[ -n "$cli_error" ]]; then
    echo "$REVIEWER reported: $cli_error" >&2
fi
if [[ -s "$STDERR_FILE" ]]; then
    echo "--- $REVIEWER stderr (last 5 lines) ---" >&2
    tail -5 "$STDERR_FILE" >&2
fi
if [[ -s "$MESSAGE_FILE" ]]; then
    if [[ -s "$JSON_FILE.final-message" && "$(cat "$MESSAGE_FILE")" != "$(cat "$JSON_FILE.final-message")" ]]; then
        echo "--- a message in the event stream, not the CLI's final message (unverified) ---" >&2
    else
        echo "--- the final message, as it arrived (unverified) ---" >&2
    fi
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
