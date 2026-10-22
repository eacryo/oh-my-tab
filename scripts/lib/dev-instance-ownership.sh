#!/bin/bash
# Instance-ownership query: which build is the running Oh-My-Tab instance, and is it this worktree's?
#
# Why this exists: the development claim in scripts/lib/dev-lock.sh serialises the
# build/test/launch TRANSACTION, and is released when dev-restart.sh returns. The window the
# script leaves running is therefore NOT protected (AGENTS.md), so between one worktree's
# "restart ok" and that worktree's UI check, another worktree may legitimately replace the
# instance. The two agents then each believe they verified their own build, and the second one
# silently verified the first one's.
#
# This library answers one question instead of preventing that: WHAT is running right now, with
# an identity strong enough to be checked again later. It never builds, launches, stops, or takes
# the claim, and it never decides that a UI conclusion is valid -- that is the caller's job, from
# the verdict and the identity this returns.
#
# What the verdict promises, and what it does not:
#   * It is a SNAPSHOT. `MINE` means "at the instant of the query, that process is running that
#     build from this worktree". The instance can be replaced a millisecond later, so the caller
#     must bind its conclusions to `pid` + `started` and re-check with --identity-expects before
#     trusting anything it observed.
#   * It does NOT prove the running build is the newest build of this worktree. A stale process
#     from an earlier build of the same tree has a different version and is reported as
#     STALE_BUILD, not as MINE.
#   * It does NOT prove the app's own config/clipboard/cache state, or that a window belongs to a
#     process. Only the process identity.
#
# Exit codes (the caller must not read "not MINE" as the only failure: 4 is UNVERIFIED, which is
# neither a pass nor a known-negative):
#   0 MINE           this worktree's build, and no competing instance
#   1 NOT_MINE       a dev build from another worktree (or an unrecognised location)
#   2 (reserved)     argument/usage errors, decided by the entry point before this library
#   3 NO_INSTANCE    no Oh-My-Tab process at all
#   4 UNVERIFIED     an instance exists but its identity could not be established. Never a pass.
#   5 CONTESTED      this worktree's build is running, but another instance shares the shortcut
#   6 NO_LOCAL_BUILD this worktree has no built bundle to compare against
#
# Sourced, never executed: sourcing must not query, print or change anything.

# --- candidate enumeration ---------------------------------------------------

# Resolve a path through symlinks and trailing slashes, so two spellings of one binary compare
# equal. `$1` need not exist: an unresolvable tail is kept verbatim.
omt_ownership_canonical() {
    local path="$1"
    local resolved
    if resolved="$(cd -P -- "$(dirname -- "$path")" 2>/dev/null && pwd)"; then
        printf '%s/%s' "$resolved" "$(basename -- "$path")"
        return 0
    fi
    printf '%s' "$path"
}

# The bundle identifier of the .app containing an executable, or empty.
omt_ownership_bundle_of_exe() {
    local exe="$1" app=""
    case "$exe" in
        *.app/Contents/MacOS/*) app="${exe%%/Contents/MacOS/*}" ;;
        *) return 1 ;;
    esac
    [ -f "$app/Contents/Info.plist" ] || return 1
    /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist" 2>/dev/null
}

# One line per live candidate: pid<TAB>started<TAB>executable. `comm` is the kernel's own record of
# what the process is executing, which is why it -- not a command-line scan -- is what an identity is
# built from: an argv can say anything.
#
# Enumeration is by exact process name and then filtered, never by a bare `pkill`/`killall`: this
# function only reads, but the same list is what a person is told to act on.
omt_ownership_candidates() {
    local pid started exe raw probe_status=0 unreadable=0
    # The process name the app runs under. It is a variable so the self-test can point the same code
    # at a synthetic process name; a caller in the real script never sets it, and the production name
    # is the default.
    local process_name="${OMT_OWNERSHIP_PROCESS_NAME:-oh-my-tab}"
    # The list and its exit status are captured in ONE call: pgrep exits 1 for "no process matched"
    # and >1 for an actual failure, and collapsing both into an empty list would report "nothing is
    # running" for a probe that never answered.
    raw="$(/usr/bin/pgrep -x "$process_name" 2>/dev/null)" || probe_status=$?
    if [ "$probe_status" -gt 1 ]; then
        return 2
    fi

    local -a pids=()
    while IFS= read -r pid; do
        [ -n "$pid" ] || continue
        pids+=("$pid")
    done <<< "$raw"

    for pid in ${pids[@]+"${pids[@]}"}; do
        # One ps call for both fields: reading them separately can straddle a pid reuse and pair one
        # process's start time with another's executable.
        local line
        # A `ps` that fails or prints nothing is a candidate that COULD NOT BE READ, not a candidate
        # that is not there: continuing silently would drop a competitor and let the caller report
        # "nothing else is running".
        line="$(ps -p "$pid" -o lstart=,comm= 2>/dev/null)" || line=""
        if [ -z "$line" ]; then
            unreadable=$(( unreadable + 1 ))
            continue
        fi
        line="$(printf '%s' "$line" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
        # `lstart` is five whitespace-separated fields and may contain PADDED spaces ("Thu Oct  1
        # 15:12:03 2026"), while the executable path may itself contain spaces. Splitting has to be
        # done ONCE with a rule that keeps both faithful: remove exactly five leading fields plus the
        # whitespace that separates them, and take the start time from the same split.
        started="$(printf '%s' "$line" | awk '{print $1" "$2" "$3" "$4" "$5}')"
        exe="$(printf '%s' "$line" | sed -E 's/^[[:space:]]*([^[:space:]]+[[:space:]]+){5}//')"
        if [ -z "$started" ] || [ -z "$exe" ]; then
            # Listed by pgrep but not readable: this is NOT "no process". Counting it as absent would
            # let a competitor that failed to read back be reported as "nothing else is running".
            unreadable=$(( unreadable + 1 ))
            continue
        fi
        printf '%s\t%s\t%s\n' "$pid" "$started" "$exe"
    done
    # Reported through the exit status so the caller can tell "listed but unreadable" from "absent".
    [ "$unreadable" -eq 0 ] || return 3
}

# --- build identity ----------------------------------------------------------

# The CFBundleVersion this worktree's own bundle carries. Read here only: nothing is built, so a
# missing bundle is a distinct answer (6), not "unknown".
omt_ownership_local_version() {
    local plist="$1/Contents/Info.plist"
    [ -f "$plist" ] || return 1
    /usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$plist" 2>/dev/null
}

# --- launch record -----------------------------------------------------------

# The `[launch]` record this pid wrote, from the app log, as "version<TAB>executable<TAB>verdict".
#
# The record is written by the app itself, so it can only describe a process that actually started;
# what it cannot say by itself is WHICH process. The pid in the line is the only link, and pids are
# reused. Two independent things are therefore required before the record is attributed:
#
#   1. the record's own timestamp lies no earlier than the process start (minus a small allowance),
#      so a record written before this process existed cannot be its own;
#   2. the recorded executable equals the executable the kernel reports for that pid right now.
#
# Either one failing leaves the attribution UNVERIFIED, never "probably mine": a gate that cannot
# decide is not a pass.
#
# The timestamp format is the one src/logger.rs writes: local wall clock, milliseconds, then the
# UTC offset, e.g. 2026-10-22T15:24:47.271+08:00. It is parsed with its offset rather than compared
# as a string, because the log and `ps` spell local time differently and a string compare would
# silently mis-order them around a DST boundary.
omt_ownership_read_record() {
    local pid="$1" started="$2" log_path="$3"
    [ -r "$log_path" ] || return 1

    local line
    # The last record for this pid: the newest launch is the one a reused pid would have written.
    line="$(grep -a "\[launch\] pid=${pid} " "$log_path" 2>/dev/null | tail -1)"
    [ -n "$line" ] || return 1

    local version exe stamp
    version="$(printf '%s\n' "$line" | sed -n 's/.* version=\([^ ]*\) .*/\1/p')"
    exe="$(printf '%s\n' "$line" | sed -n 's/.* executable=\([^ ]*\) .*/\1/p')"
    # The stamp is everything up to the last offset token on the line. It is captured separately
    # rather than trusted to be well-formed: a rotated or foreign log is reported as unparseable,
    # never silently assumed to be this process's.
    stamp="$(printf '%s\n' "$line" | sed -n 's/^\([0-9][0-9-]*T[0-9:.]*[0-9][Z+-][0-9:]*\).*/\1/p')"
    [ -n "$version" ] || return 1

    if [ -z "$stamp" ]; then
        printf '%s\t%s\tno-timestamp\n' "$version" "$exe"
        return 0
    fi

    local record_epoch start_epoch
    record_epoch="$(omt_ownership_epoch_of_stamp "$stamp")" || record_epoch=""
    start_epoch="$(omt_ownership_epoch_of_lstart "$started")" || start_epoch=""
    if [ -z "$record_epoch" ] || [ -z "$start_epoch" ]; then
        printf '%s\t%s\tunparseable-time\n' "$version" "$exe"
        return 0
    fi
    # Any record that sorts BEFORE the process start is rejected, with no allowance at all. The app
    # writes this record after it starts, so a record earlier than the process can only belong to a
    # previous process that had the same pid. Truncation to whole seconds cannot manufacture that
    # case: both readings are floored in the direction that keeps a later record later, so a
    # record genuinely written after the start never compares earlier than the start.
    if [ "$record_epoch" -lt "$start_epoch" ]; then
        printf '%s\t%s\tstale-record\n' "$version" "$exe"
        return 0
    fi
    printf '%s\t%s\tok\n' "$version" "$exe"
}

# Epoch seconds for an ISO-8601 stamp written by src/logger.rs: local wall clock, optional
# milliseconds, then the offset (Z or +HH:MM). The milliseconds are dropped and the offset's colon
# removed, because `date -j`'s %z wants `+0800`.
#
# The offset is honoured rather than ignored: the log and `ps` both print local time for this
# machine, but they do not have to, and ignoring the offset would mis-order the two around a DST
# boundary or on a machine whose log is copied between zones.
omt_ownership_epoch_of_stamp() {
    local stamp="$1" base offset sign hhmm
    case "$stamp" in
        *Z) ;;
        *[+-][0-9][0-9]:[0-9][0-9]) ;;
        *) return 1 ;;
    esac
    case "$stamp" in
        *Z)
            base="${stamp%Z}"
            offset="+0000"
            ;;
        *)
            base="${stamp%[+-][0-9][0-9]:[0-9][0-9]}"
            offset="${stamp#"$base"}"
            sign="${offset:0:1}"
            hhmm="${offset:1:2}${offset:4:2}"
            offset="$sign$hhmm"
            ;;
    esac
    # Drop the fractional seconds if present: "%Y-%m-%dT%H:%M:%S%z" takes none.
    case "$base" in
        *.*) base="${base%.*}" ;;
    esac
    TZ=UTC date -j -f "%Y-%m-%dT%H:%M:%S%z" "${base}${offset}" +%s 2>/dev/null
}

# Epoch seconds for a `ps -o lstart=` value (local time, no offset). `ps` prints local wall clock,
# which is the same zone a log line was written in unless the log is foreign, so the comparison is
# made in this machine's local zone.
omt_ownership_epoch_of_lstart() {
    local started="$1"
    date -j -f "%a %b %d %T %Y" "$started" +%s 2>/dev/null
}

# --- verdict -----------------------------------------------------------------

# Classify one candidate line. Prints "kind<TAB>detail" where kind is one of
# mine | other-dev | packaged | unbundled | unknown.
omt_ownership_classify() {
    local pid="$1" started="$2" exe="$3" local_exe="$4" log_path="$5"

    local canon
    canon="$(omt_ownership_canonical "$exe")"
    local local_canon
    local_canon="$(omt_ownership_canonical "$local_exe")"

    local bundle
    bundle="$(omt_ownership_bundle_of_exe "$exe" || true)"
    local local_bundle
    local_bundle="$(omt_ownership_bundle_of_exe "$local_exe" || true)"

    if [ "$canon" = "$local_canon" ]; then
        printf 'mine\t%s\n' "$canon"
        return 0
    fi
    if [ -n "$bundle" ] && [ "$bundle" = "$local_bundle" ] && [ -n "$local_bundle" ]; then
        printf 'other-dev\t%s\n' "$canon"
        return 0
    fi
    case "$bundle" in
        com.eacryo.oh-my-tab|com.eacryo.oh-my-tab.unbundled)
            printf 'packaged\t%s\n' "$canon"
            return 0
            ;;
        '')
            # No containing bundle: the unbundled install runs this same binary as a plain
            # executable, so it is classified by name rather than reported as unrecognised.
            case "$exe" in
                */oh-my-tab) printf 'unbundled\t%s\n' "$canon"; return 0 ;;
            esac
            printf 'unknown\t%s\n' "$canon"
            return 0
            ;;
        *)
            printf 'unknown\t%s\n' "$canon"
            return 0
            ;;
    esac
}

# Run the whole query. Every line of output is diagnostic except the final VERDICT, which is the
# contract; the exit status matches the table at the top of this file.
#
# Usage: omt_ownership_probe <local-binary> <app-log> [<expect-pid> <expect-start>]
# The last two, when given, re-check a previously returned identity (see --identity-expects).
omt_ownership_probe() {
    local local_exe="$1"
    local log_path="$2"
    local expect_pid="${3:-}"
    local expect_start="${4:-}"

    local candidates status=0
    # Declared here, before anything assigns it: a later `local ... partial_listing=0` would reset
    # the flag the enumeration just set, silently turning "a candidate could not be read" back into
    # "nothing else is running".
    local partial_listing=0
    candidates="$(omt_ownership_candidates)" || status=$?
    case "$status" in
        0) ;;
        3)
            # Processes were listed but some could not be read back. Whatever else the answer says,
            # "nothing competes with mine" is not one of the things it can say, so a partial list is
            # never enough to produce MINE -- it is reported as UNVERIFIED below.
            echo "warning: some Oh-My-Tab processes could not be read back; their ownership is unknown" >&2
            partial_listing=1
            ;;
        *)
            echo "VERDICT: UNVERIFIED"
            echo "reason: the process enumeration itself failed, so 'nothing is running' cannot be concluded"
            return 4
            ;;
    esac

    local local_version
    # The bundle is the .app containing the executable, at <app>/Contents/MacOS/<binary>.
    local local_bundle_dir=""
    case "$local_exe" in
        *.app/Contents/MacOS/*) local_bundle_dir="${local_exe%%/Contents/MacOS/*}" ;;
    esac
    [ -n "$local_bundle_dir" ] || local_bundle_dir="$local_exe"
    if ! local_version="$(omt_ownership_local_version "$local_bundle_dir")"; then
        if [ -n "$candidates" ]; then
            echo "instances:"
            printf '%s\n' "$candidates" | awk -F'\t' '{printf "  pid %s, started %s\n    %s\n", $1, $2, $3}'
            echo "VERDICT: NO_LOCAL_BUILD"
            echo "reason: this worktree has no built bundle, so no instance can be attributed to it"
            return 6
        fi
        echo "VERDICT: NO_LOCAL_BUILD"
        echo "reason: this worktree has no built bundle at $local_exe"
        return 6
    fi

    if [ -z "$candidates" ]; then
        echo "this worktree's build version: $local_version"
        # Nothing readable came back. When the enumeration ALSO reported unreadable candidates, the
        # machine is not known to be idle -- saying NO_INSTANCE would answer a question the probe
        # could not actually answer.
        if [ "$partial_listing" -ne 0 ]; then
            echo "VERDICT: UNVERIFIED"
            echo "reason: Oh-My-Tab processes were listed but none could be read back, so whether any"
            echo "        is running cannot be concluded"
            return 4
        fi
        # A re-check of a snapshot asks "is THAT identity still mine". Once nothing is running, the
        # answer to that question is no -- the instance it described is gone. Reporting NO_INSTANCE
        # instead would make a caller that only tests for "mine" read a vanished instance as "the
        # question does not apply", which is how a stale UI conclusion survives.
        if [ -n "$expect_pid" ]; then
            echo "VERDICT: NOT_MINE"
            echo "reason: pid $expect_pid is no longer running, so it cannot be this worktree's instance"
            return 1
        fi
        echo "VERDICT: NO_INSTANCE"
        echo "reason: no Oh-My-Tab process is running"
        return 3
    fi

    local mine_pid="" mine_start="" mine_canon="" competitors=0
    local packaged_pids="" unknown_pids="" other_dev_paths=""
    local saw_candidate=0

    while IFS=$'\t' read -r pid started exe; do
        [ -n "$pid" ] || continue
        saw_candidate=1
        local kind detail
        kind="$(omt_ownership_classify "$pid" "$started" "$exe" "$local_exe" "$log_path" | cut -f1)"
        detail="$(omt_ownership_classify "$pid" "$started" "$exe" "$local_exe" "$log_path" | cut -f2)"
        case "$kind" in
            mine)
                if [ -n "$mine_pid" ]; then
                    # Two processes of this same build: the identity of "the" instance is ambiguous,
                    # so this is a competition, not a match.
                    competitors=1
                    continue
                fi
                mine_pid="$pid"
                mine_start="$started"
                mine_canon="$detail"
                ;;
            other-dev) competitors=1; other_dev_paths="$other_dev_paths $detail" ;;
            packaged) competitors=1; packaged_pids="$packaged_pids $pid" ;;
            unbundled) competitors=1; packaged_pids="$packaged_pids $pid" ;;
            unknown) competitors=1; unknown_pids="$unknown_pids $pid" ;;
        esac
    done <<< "$candidates"

    if [ "$saw_candidate" -eq 0 ]; then
        echo "VERDICT: UNVERIFIED"
        echo "reason: processes were listed but none could be read back; the answer is unknown, not empty"
        return 4
    fi

    # A re-check of an earlier snapshot: the identity must still be the same process.
    if [ -n "$expect_pid" ] && [ -n "$mine_pid" ]; then
        if [ "$mine_pid" != "$expect_pid" ] || { [ -n "$expect_start" ] && [ "$mine_start" != "$expect_start" ]; }; then
            echo "VERDICT: NOT_MINE"
            echo "reason: pid $expect_pid is no longer the instance running this worktree's build"
            return 1
        fi
    fi

    if [ -n "$mine_pid" ]; then
        local record version rec_exe attribution
        record="$(omt_ownership_read_record "$mine_pid" "$mine_start" "$log_path" || true)"
        if [ -n "$record" ]; then
            version="$(printf '%s' "$record" | cut -f1)"
            rec_exe="$(printf '%s' "$record" | cut -f2)"
            attribution="$(printf '%s' "$record" | cut -f3)"
        else
            version=""; rec_exe=""; attribution="no-record"
        fi

        echo "instance: pid $mine_pid, started $mine_start"
        echo "binary:   $mine_canon"
        echo "this worktree's build version: $local_version"
        if [ -n "$version" ]; then
            echo "reported version: $version"
        fi

        # The attribution -- not merely "a record exists" -- decides whether this is this build.
        if [ "$attribution" != "ok" ]; then
            case "$attribution" in
                no-record)
                    echo "VERDICT: UNVERIFIED"
                    echo "reason: no [launch] record for pid $mine_pid in $log_path"
                    ;;
                stale-record)
                    echo "VERDICT: UNVERIFIED"
                    echo "reason: the newest [launch] record for pid $mine_pid predates that process, so a"
                    echo "        reused pid cannot be ruled out"
                    ;;
                unparseable-time|no-timestamp)
                    echo "VERDICT: UNVERIFIED"
                    echo "reason: the [launch] record's timestamp could not be read, so it cannot be tied"
                    echo "        to this process"
                    ;;
                *)
                    echo "VERDICT: UNVERIFIED"
                    echo "reason: the [launch] record was not attributable ($attribution)"
                    ;;
            esac
            return 4
        fi
        if [ "$version" != "$local_version" ]; then
            echo "VERDICT: STALE_BUILD"
            echo "reason: the running build is version $version; this worktree's bundle is $local_version"
            return 1
        fi
        # The second piece of evidence: what the record says it executed. It is REQUIRED, not
        # checked-when-present -- a record without an executable, or with the encoder's "unknown"
        # placeholder `-` (written when the app could not resolve its own path), proves nothing and
        # would otherwise let the version check alone carry the verdict.
        if [ -z "$rec_exe" ] || [ "$rec_exe" = "-" ]; then
            echo "VERDICT: UNVERIFIED"
            echo "reason: the [launch] record carries no executable path, so the process cannot be tied"
            echo "        to the build it claims"
            return 4
        fi
        local rec_plain
        if ! rec_plain="$(printf '%s' "$rec_exe" | python3 "$(dirname -- "${BASH_SOURCE[0]}")/decode-launch-field.py" 2>/dev/null)"; then
            echo "VERDICT: UNVERIFIED"
            echo "reason: the [launch] record's executable field could not be decoded"
            return 4
        fi
        if [ -z "$rec_plain" ]; then
            echo "VERDICT: UNVERIFIED"
            echo "reason: the [launch] record's executable field decodes to nothing, so it cannot be compared"
            return 4
        fi
        if [ "$(omt_ownership_canonical "$rec_plain")" != "$mine_canon" ]; then
            echo "VERDICT: UNVERIFIED"
            echo "reason: the [launch] record names $rec_plain, but pid $mine_pid runs $mine_canon"
            return 4
        fi
        if [ "$competitors" -ne 0 ]; then
            [ -n "$other_dev_paths" ] && echo "competitor dev instance(s):$other_dev_paths"
            [ -n "$packaged_pids" ] && echo "competitor packaged/unbundled pid(s):$packaged_pids"
            [ -n "$unknown_pids" ] && echo "unrecognised pid(s) of the same name:$unknown_pids"
            echo "VERDICT: CONTESTED"
            echo "reason: this worktree's build is running, but another instance shares the shortcut and may"
            echo "        be the one receiving input"
            return 5
        fi
        # An unreadable candidate could be the competitor, so "nothing else is running" cannot be
        # claimed. This is checked before MINE, not merged into it.
        if [ "$partial_listing" -ne 0 ]; then
            echo "VERDICT: UNVERIFIED"
            echo "reason: this worktree's build is running, but other Oh-My-Tab processes could not be"
            echo "        read back, so whether one competes for the shortcut is unknown"
            return 4
        fi
        # Re-confirm the identity immediately before reporting success. The pid, start time and
        # executable were read BEFORE the plist read and the log scan; the process may have exited or
        # been replaced since, and a verdict describing a process that is no longer there is exactly
        # the stale answer this command exists to prevent.
        local confirm_line confirm_start confirm_exe
        confirm_line="$(ps -p "$mine_pid" -o lstart=,comm= 2>/dev/null)" || confirm_line=""
        confirm_line="$(printf '%s' "$confirm_line" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
        confirm_start="$(printf '%s' "$confirm_line" | awk '{print $1" "$2" "$3" "$4" "$5}')"
        confirm_exe="$(printf '%s' "$confirm_line" | sed -E 's/^[[:space:]]*([^[:space:]]+[[:space:]]+){5}//')"
        if [ -z "$confirm_start" ] || [ "$confirm_start" != "$mine_start" ] \
            || [ "$(omt_ownership_canonical "$confirm_exe")" != "$mine_canon" ]; then
            echo "VERDICT: UNVERIFIED"
            echo "reason: pid $mine_pid changed or exited during the query, so the answer describes a"
            echo "        process that is no longer there; ask again"
            return 4
        fi
        echo "VERDICT: MINE"
        echo "note: this is a snapshot. Bind conclusions to pid and started, and re-check with"
        echo "      --identity-expects before trusting what you observed."
        return 0
    fi

    if [ -n "$other_dev_paths" ]; then
        echo "instances from other worktrees:$other_dev_paths"
    fi
    [ -n "$packaged_pids" ] && echo "packaged/unbundled pid(s):$packaged_pids"
    [ -n "$unknown_pids" ] && echo "unrecognised pid(s) of the same name:$unknown_pids"
    echo "this worktree's build version: $local_version"
    # NOT_MINE is still a definite answer about THIS worktree (nothing of its build is running), so an
    # unreadable candidate does not change it: a competitor that could not be read is not this
    # worktree's instance either. It is named so the reader is not told the machine is clean.
    if [ "$partial_listing" -ne 0 ]; then
        echo "note: some other Oh-My-Tab processes could not be read back and are not classified here"
    fi
    echo "VERDICT: NOT_MINE"
    echo "reason: the running dev build does not come from $local_exe"
    return 1
}
