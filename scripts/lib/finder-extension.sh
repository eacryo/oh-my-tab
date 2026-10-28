#!/bin/bash
# Finder Sync extension hygiene: the launchd records that decide whether Finder can start it.
# Finder Sync 扩展的 launchd 卫生检查。
#
# A pure shell library: scripts/dev-restart.sh sources it to clear the records before it registers
# the rebuilt extension, and scripts/finder-extension-selftest.sh exercises every decision offline
# (the launchd-facing ones against throwaway services it creates and removes itself). Nothing here
# reads the app's config or touches the repository.
#
# The failure this exists for, observed on 2026-10-23:
#   - a worktree under .agents/worktrees/finder-menu/ ran the extension once, which bootstrapped a
#     launchd service instance named `<appex bundle id>.<request uuid>` and pinned to THAT worktree's
#     appex path;
#   - the worktree was deleted, and the pinned path went with it;
#   - every later Finder request re-bootstrapped the SAME bundle id, launchd answered "will use
#     existing" with the dead path and then "Missing executable detected"; the spawn failed;
#   - PlugInKit still reported `+` (elected) and System Settings still showed the switch on, so the
#     registered extension never started and its menu stayed silently empty.
# `pluginkit -a` cannot fix that: it adds a record, it does not replace an instance pinned to a path
# that no longer exists.
#
# What may and may not be removed:
#   - a per-environment instance whose PINNED PROGRAM no longer exists on disk is removed for any
#     appex. It can never start again, and Finder bootstraps a fresh instance per request, so
#     removing it costs nothing. This is the only shape the incident had.
#   - an instance whose program still exists is left alone, whoever owns it: a live extension of the
#     packaged install or of another worktree is Finder's to start and stop, not this script's.
#   - an instance whose program launchd does not state is left alone (a dead path cannot be proven).
#   - a bare service name with no `.uuid` suffix is refused: no record from the incident had that
#     shape, and what booting it out would do to the next request is not measured.
#   - the host APPLICATION's instance (`application.<host bundle id>.<pid>.<pid>`) is never touched.
#     Booting such an instance out on 2026-10-23 left the app running with its log writers destroyed
#     (the log fds were unlinked) and `--whose-instance` reporting UNVERIFIED.
#   - an extension PROCESS of any appex other than the one this run assembled is never signalled.

# Whether a launchd label is a per-environment Finder extension instance of `appex_id`.
#
# Pure, so the decisions below are asserted without a launchd: `label` is what
# `launchctl print gui/<uid>` listed, and `appex_id` is the extension bundle id as read from the
# assembled appex.
#
# The label is `<appex_id>.<request uuid>`; the suffix must look like a UUID, so the shared service
# name (no suffix), the application instance (`application.<host>.…`, where `<host>` would have to be
# `<appex_id-minus-.FinderSync>`) and anything else are refused. The `.FinderSync` component is what
# separates the extension's namespace from the host app's, which is the same rule the repository
# applies elsewhere.
omt_finder_ext_is_instance_label() {
    local label="$1"
    local appex_id="$2"
    [ -n "$appex_id" ] || return 1
    case "$label" in
        "$appex_id".*) ;;
        *) return 1 ;;
    esac
    local suffix="${label#"$appex_id".}"
    case "$suffix" in
        ''|*[!0-9A-Fa-f-]*) return 1 ;;
    esac
    case "$suffix" in
        [0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f]'-'*) ;;
        *) return 1 ;;
    esac
    # Eight hex groups separated by dashes: the shape launchd uses for a request identity.
    printf '%s\n' "$suffix" | grep -Eq '^[0-9A-Fa-f]{8}(-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}$'
}

# Whether `pid` is a live process running exactly `binary`.
#
# Identity here is the path, not the start time. A start-time pair cannot survive the round trip
# this needs -- `ps -o lstart=` pads the field with trailing spaces, and a `read`-split line (or a
# word-split one) loses them, so the comparison silently failed and the stop became a no-op. A path
# re-read from the kernel immediately before each signal is what a killed-and-reused pid can never
# fake: the recycled process would have to be running the same appex, which is the extension this
# run is replacing anyway.
omt_finder_ext_pid_is() {
    local pid="$1"
    local binary="$2"
    [ -n "$pid" ] && [ -n "$binary" ] || return 1
    [ "$(ps -p "$pid" -o comm= 2>/dev/null)" = "$binary" ]
}

# Whether Finder should be restarted so it asks launchd for the extension again.
#
# Pure in its inputs. A restart closes every Finder window, so it is made only when the extension is
# provably not running: `alive` is empty, and `observed` (an extension process seen before this
# run's cleanup that belonged to this same appex) is empty too.
omt_finder_ext_should_restart_finder() {
    local alive="$1"
    local observed="$2"
    [ -z "$alive" ] || return 1
    [ -z "$observed" ]
}

# Extension process ids whose executable is exactly `binary`, one per line.
#
# "Is the extension running" is never answered with `pgrep -f oh-my-tab-finder-ext`: the packaged
# install and another worktree's build share that executable name. Only the exact path counts.
omt_finder_ext_own_pids() {
    local binary="$1"
    [ -n "$binary" ] || return 0
    local pid exe
    for pid in $(/usr/bin/pgrep -f 'oh-my-tab-finder-ext' 2>/dev/null || true); do
        exe="$(ps -p "$pid" -o comm= 2>/dev/null)"
        [ "$exe" = "$binary" ] || continue
        printf '%s\n' "$pid"
    done
}

# The first such pid, or nothing.
#
# Answered by looking for the pid, not by filtering the list above through `head -1`: the filter
# form writes every match into a reader that leaves after the first one, so a later match wrote into
# a closed pipe -- printf failed, SIGPIPE took the loop down, and bash printed
# `printf: write error: Broken pipe` on the caller's stderr. dev-restart.sh printed exactly that
# between its stop step and `Compiling` whenever the extension had more than one process. Returning
# at the first match sends no later write, and needs no reader.
omt_finder_ext_alive_pid() {
    local binary="$1"
    [ -n "$binary" ] || return 0
    local pid exe
    for pid in $(/usr/bin/pgrep -f 'oh-my-tab-finder-ext' 2>/dev/null || true); do
        exe="$(ps -p "$pid" -o comm= 2>/dev/null)"
        [ "$exe" = "$binary" ] || continue
        printf '%s\n' "$pid"
        return 0
    done
}

# The extension id a per-environment instance label carries, or nothing for a label that is not one.
#
# Pure. The id contains dots (reverse-DNS) and may contain dashes, and so does the uuid, so neither
# character can cut the two apart; the shape check fixes where the uuid begins. Refused, and so
# never removed: a bare service name (`...FinderSync`, no uuid), an application namespace label
# (`application.<host bundle id>.<pid>.<pid>`), and anything whose trailing dot-separated component
# is not an 8-4-4-4-12 uuid.
omt_finder_ext_id_from_label() {
    local label="$1"
    local uuid_re='\.[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$'
    local uuid_prefix id
    case "$label" in
        application.*) return 1 ;;
    esac
    printf '%s\n' "$label" | grep -Eq "$uuid_re" || return 1
    uuid_prefix="$(printf '%s\n' "$label" | grep -oE "$uuid_re")"
    id="${label%%"$uuid_prefix"}"
    id="${id%.}"
    [ -n "$id" ] || return 1
    printf '%s\n' "$id"
}

# Per-environment instance labels of Finder extension services in the current user's domain, one per
# line, each followed by the executable path launchd has pinned for it (`<label> <program>`, or just
# the label when launchd does not state one).
#
# `launchctl print gui/<uid>` is the list launchd itself answers from. The extension id is
# discovered from the label instead of being required up front, so a record inherited from a build
# that is no longer on this machine still shows up.
#
# Returns 2 (and prints nothing) when launchd itself cannot be asked: "the query failed" must never
# be read as "there is nothing to clean", because that is exactly the state this library exists to
# repair.
omt_finder_ext_instance_lines() {
    local domain="gui/$(id -u)"
    local listing label
    if ! listing="$(launchctl print "$domain" 2>/dev/null)"; then
        return 2
    fi
    # Collected through a file rather than a pipe into `while`: in a pipeline the loop is a subshell,
    # so nothing here is guaranteed to be in the caller's shell, and a grep that finds no match would
    # otherwise end the pipeline with a status no caller can interpret.
    local collected
    if ! collected="$(mktemp "${TMPDIR:-/tmp}/omt-finder-ext.XXXXXX")"; then
        return 2
    fi
    # The collection status decides: a failed sort or a disk error must not read as "no services",
    # which is the state this library exists to repair. `pipefail` alone would be overwritten by the
    # loop that follows, so it is checked here and turned into a return value.
    # grep's "no match" (status 1) is an empty list, not a failure; every other non-zero status is
    # a real error. The pipeline status cannot be used for that distinction: `set -o pipefail` makes
    # a no-match grep 1 the pipeline's status even when sort succeeded.
    printf '%s\n' "$listing" \
        | grep -oE '[A-Za-z0-9._-]+\.FinderSync\.[0-9A-Fa-f]{8}(-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}' \
        > "$collected.labels"
    local grep_status=$?
    if [ "$grep_status" -gt 1 ] || ! sort -u "$collected.labels" > "$collected"; then
        rm -f "$collected" "$collected.labels"
        return 2
    fi
    rm -f "$collected.labels"
    while IFS= read -r label; do
        [ -n "$label" ] || continue
        omt_finder_ext_id_from_label "$label" >/dev/null || continue
        printf '%s %s\n' "$label" \
            "$(launchctl print "$domain/$label" 2>/dev/null \
                | sed -n 's/^[[:space:]]*program = //p' | head -1)"
    done < "$collected"
    rm -f "$collected"
    return 0
}

# Just the labels.
omt_finder_ext_stale_instance_labels() {
    omt_finder_ext_instance_lines | awk '{ print $1 }'
}

# Whether a pinned program path points at an appex that no longer exists on disk.
#
# Pure decision on a path string, so the incident's shape is assertable without a launchd: the
# worktree that owned the appex was deleted, leaving the instance pointing into nothing.
omt_finder_ext_program_is_missing() {
    local program="$1"
    [ -n "$program" ] || return 1
    case "$program" in
        /*) ;;
        *) return 1 ;;
    esac
    [ ! -e "$program" ]
}

# launchd's own observations about `label`, as it printed them.
#
# The shape differs by service type -- a submitted job may show `pid = N`, another may show only
# `active count = N` -- so the caller judges the whole text instead of relying on one field.
omt_finder_ext_instance_observations() {
    launchctl print "gui/$(id -u)/$1" 2>/dev/null
}

# Whether launchd reports any live process for a service, from its printed observations or from a
# `pid =` line alone. Empty observations mean "could not ask", which is reported as live.
#
# `active count` is launchd's own count of the processes this service has running, nested entries
# included; the top-level `pid =` line alone has been absent for every submitted service measured on
# 2026-10-24, so it cannot be the only evidence used.
omt_finder_ext_instance_live() {
    local label="$1"
    local pid="$2"
    local observations="$3"
    [ -z "$observations" ] && return 0
    [ -n "$pid" ] && return 0
    local own_count
    own_count="$(printf '%s\n' "$observations" \
        | sed -n 's/^[[:space:]]*active count = \([0-9][0-9]*\).*/\1/p' | head -1)"
    # Field order and a missing `active count` line both make this "cannot tell", which is live.
    [ -n "$own_count" ] || return 0
    [ "$own_count" -gt 0 ]
}

# launchd's own state word for `label` (`running`, `not running`, `spawn scheduled`, ...).
omt_finder_ext_instance_state() {
    local output
    output="$(omt_finder_ext_instance_observations "$1")"
    # Only the first `state =` line is this service's own; the ones nested under its endpoints and
    # timers are theirs.
    printf '%s\n' "$output" | sed -n 's/^[[:space:]]*state = //p' | head -1
}

# Whether removing a service is permitted, given its pinned program, launchd's own state for it, and
# whether launchd reports any live process for it.
#
# A deleted executable does NOT mean the process stopped: a running process keeps executing after its
# file is unlinked, and another worktree rebuilding its bundle briefly removes the old appex. So the
# program must be gone, launchd must not report a live process, AND its state must be one that cannot
# still be retrying or starting something (`running` means a live process; `spawn scheduled` means it
# is retrying a spawn and is not idle). Pure over its three inputs.
omt_finder_ext_instance_removable() {
    local program="$1"
    local state="$2"
    local observations="$3"
    omt_finder_ext_program_is_missing "$program" || return 1
    if [ "$state" = "running" ]; then
        return 1
    fi
    omt_finder_ext_instance_live "" "" "$observations" && return 1
    return 0
}

# Remove a launchd service (or one of its per-environment instances) for the current user.
#
# Bootout of a label that is not there (exit 3 / "no such process") is the ordinary case and is not
# a failure. Returns 0 when the label is gone afterwards, 1 when a bootout was needed but the label
# survived it.
omt_finder_ext_bootout() {
    local label="$1"
    local domain="gui/$(id -u)"
    launchctl print "$domain/$label" >/dev/null 2>&1 || return 0
    launchctl bootout "$domain/$label" >/dev/null 2>&1 || true
    local _
    for _ in $(seq 1 20); do
        launchctl print "$domain/$label" >/dev/null 2>&1 || return 0
        sleep 0.1
    done
    launchctl print "$domain/$label" >/dev/null 2>&1 || return 0
    return 1
}

# Wait for `pid` to exit, as long as it is still running `binary`.
#
# Returns 0 when the process is gone OR the pid now runs something else (the original is gone and
# nothing further should be signalled), 1 when it is still the same live extension.
omt_finder_ext_wait_pid_gone() {
    local pid="$1"
    local binary="$2"
    local _
    for _ in $(seq 1 25); do
        omt_finder_ext_pid_is "$pid" "$binary" || return 0
        sleep 0.2
    done
    omt_finder_ext_pid_is "$pid" "$binary" || return 0
    return 1
}

# Stop the extension at `pid`, re-checking its path before TERM and before KILL.
#
# Prints what it did; returns 0 when the observed process is gone.
omt_finder_ext_stop_process() {
    local pid="$1"
    local binary="$2"
    omt_finder_ext_pid_is "$pid" "$binary" || return 0
    kill -TERM "$pid" 2>/dev/null || true
    if omt_finder_ext_wait_pid_gone "$pid" "$binary"; then
        printf 'stopped %s\n' "$pid"
        return 0
    fi
    omt_finder_ext_pid_is "$pid" "$binary" || { printf 'stopped %s\n' "$pid"; return 0; }
    kill -KILL "$pid" 2>/dev/null || true
    if omt_finder_ext_wait_pid_gone "$pid" "$binary"; then
        printf 'stopped %s (forced)\n' "$pid"
        return 0
    fi
    printf 'could not stop %s\n' "$pid"
    return 1
}

# Whether a label is in a newline-separated allowlist. An empty allowlist means "no restriction".
#
# Pure, so the restriction is asserted without a launchd: an allowlist is matched by EXACT line, never
# by prefix -- a prefix match would let `...FinderSync.<uuid>` also authorise `...FinderSync.<uuid>.x`
# and, worse, would make an allowlist look safe without being it.
omt_finder_ext_label_allowed() {
    local label="$1"
    local allowlist="$2"
    [ -n "$allowlist" ] || return 0
    printf '%s\n' "$allowlist" | grep -Fxq -- "$label"
}

# Remove the launchd records that can only be the incident's, and reap this run's own extension
# process.
#
# What is removed: a per-environment instance whose pinned program is a path that no longer exists.
# That is precisely the state Finder cannot recover from on its own, and it is safe to remove for
# any appex -- the instance can never start again, and Finder bootstraps a fresh one per request.
# What is left alone: an instance whose program still exists (a live extension of another appex or
# another worktree -- Finder's to start and stop, not this script's), the host app's own instance
# (the `application.*` namespace is refused outright), and any label without a request uuid.
#
# `own_binary` is the appex this run assembled: its extension process is stopped so the next Finder
# request starts the newly built one. An empty value stops no process -- the pre-rebuild cleanup
# passes it, because stopping the extension there only lets Finder start the old build again while
# cargo runs. A process of any other appex is never signalled either way.
#
# `only_labels` (optional) is a newline-separated allowlist: when it is non-empty, an instance whose
# label is not on it is reported as `skipped` and never booted out. The launchd list is still read --
# that is what the self-test exercises against a real launchd -- and only the removal decision is
# narrowed. Production passes no allowlist; the self-test passes exactly the labels it created.
#
# Prints one line per decision (including `skipped` for an instance outside the allowlist). Returns
# 0 when it removed nothing and skipped nothing; 1 when it removed something OR skipped something
# (both mean a production run would have acted differently, which the self-test asserts); 2 when
# something refused to go. No fourth code: a skipped record is not an error, it is a difference.
omt_finder_ext_clear_instances() {
    local own_binary="$1"
    local only_labels="${2:-}"
    local line label program state observations removed=0 skipped=0
    local status=0

    local listing
    if ! listing="$(omt_finder_ext_instance_lines)"; then
        printf 'could not read the launchd service list (gui/%s)\n' "$(id -u)"
        return 2
    fi

    while IFS= read -r line; do
        [ -n "$line" ] || continue
        # Lines are `<label> <program>`, or `<label>` alone when launchd states no program. Splitting
        # with ${line%% *} up front would make the one-word case indistinguishable from a genuine one.
        label="${line%% *}"
        program="${line#"$label"}"
        program="${program# }"
        # The enumeration above is deliberately the REAL launchd list even when an allowlist is given:
        # reading it is where the platform behaviour lives (field formats, `pipefail`, `state`), and a
        # caller that restricted removal must not lose that coverage. Only the decision to remove is
        # restricted.
        if ! omt_finder_ext_label_allowed "$label" "$only_labels"; then
            printf 'skipped %s (not in the allowlist for this caller)\n' "$label"
            skipped=1
            continue
        fi
        if [ -z "$program" ]; then
            printf 'left %s (launchd does not state its program, so a dead path cannot be proven)\n' "$label"
            continue
        fi
        if ! omt_finder_ext_program_is_missing "$program"; then
            printf 'left %s (its program still exists: %s)\n' "$label" "$program"
            continue
        fi
        observations="$(omt_finder_ext_instance_observations "$label")"
        state="$(printf '%s\n' "$observations" | sed -n 's/^[[:space:]]*state = //p' | head -1)"
        if ! omt_finder_ext_instance_removable "$program" "$state" "$observations"; then
            printf 'left %s (program gone, launchd state: %s)\n' "$label" "${state:-unknown}"
            continue
        fi
        if omt_finder_ext_bootout "$label"; then
            printf 'removed %s (pinned to a missing appex: %s)\n' "$label" "$program"
            removed=1
        else
            printf 'could not remove %s\n' "$label"
            status=2
        fi
    done <<EOF
$listing
EOF

    if [ -n "$own_binary" ]; then
        local pid
        while IFS= read -r pid; do
            [ -n "$pid" ] || continue
            if ! omt_finder_ext_stop_process "$pid" "$own_binary"; then
                status=2
                continue
            fi
            removed=1
        done <<EOF
$(omt_finder_ext_own_pids "$own_binary")
EOF
    fi

    # An `[ ... ] && return X` line that is NOT the last statement leaves the function returning
    # that test's status: the reported "removed something" was lost behind a successful re-read.
    if [ "$status" -ne 0 ]; then
        return "$status"
    fi
    if [ "$removed" = "1" ] || [ "$skipped" = "1" ]; then
        return 1
    fi
    return 0
}
