#!/bin/bash
# The development-instance claim: one build-test-launch transaction at a time, across worktrees.
#
# Why a lock at all: every worktree shares one bundle identifier, one launchd label and one app
# log, because those are what keep the TCC grants and the keychain ACL valid across rebuilds.
# Two worktrees building and launching at once therefore fight over the same instance, and the
# loser's bootout stops an instance the other run is still verifying. The claim is a `lockf` lock
# on a file descriptor, held in the git common directory so every worktree of this repository
# sees the same one; the kernel drops it when the last descriptor closes, so a killed run cannot
# leave it held.
#
# The claim covers the transaction -- stop the old instance, build, test, launch, verify -- and
# is released when that returns. It is NOT a claim on the running application: the interactive
# entry point leaves the app running and says so, and the next run may replace it. A running
# instance is not a held lock.
#
# Exit codes: 0 claimed, 75 this transaction is busy, 64 the lock could not be located.

# Sourced, never executed: sourcing must not take the lock, install a trap or write anything.

# Seconds to wait for another worktree's transaction before giving up. An explicit --lock-wait
# overrides it; there is no environment variable, because the running scripts take their
# configuration from argv only.
omt_lock_wait_default=900
omt_lock_poll_seconds=1

# Resolve the shared directory the lock lives in. The git common directory is the one path every
# worktree of this repository shares; a lock that cannot be located is not a lock, so an
# unresolvable repository fails instead of running unlocked.
omt_lock_dir() {
    local common
    if ! common="$(git -C "$1" rev-parse --git-common-dir 2>/dev/null)"; then
        return 1
    fi
    case "$common" in
        /*) ;;
        *) common="$1/$common" ;;
    esac
    if [ -z "$common" ] || [ ! -d "$common" ]; then
        return 1
    fi
    printf '%s/oh-my-tab-dev-lock' "$common"
}

# Take the claim, or wait for the holder to finish.
#
# Usage: omt_lock_acquire <lock-file> <wait-seconds>
# Prints who holds the lock while it waits, so a stuck run is diagnosable from its own output.
# Returns 0 when claimed, 75 on timeout, 64 when the lock file cannot be opened.
omt_lock_acquire() {
    local lock_path="$1"
    local wait_seconds="$2"
    local waited=0

    # The descriptor is opened once and the lock is taken on it; every retry uses the same fd, so
    # there is no window where the file is replaced between attempts.
    if ! exec 9>"$lock_path"; then
        return 64
    fi

    while :; do
        if /usr/bin/lockf -s -t 0 -k 9; then
            local pid started
            pid="$(cat "$lock_path" 2>/dev/null)"
            printf '%s\n' "$$" >"$lock_path"
            printf '%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$lock_path"
            return 0
        fi

        if [ "$waited" -ge "$wait_seconds" ]; then
            local holder
            holder="$(head -2 "$lock_path" 2>/dev/null | tr '\n' ' ')"
            echo "the development-instance claim is still held after ${wait_seconds}s${holder:+ (holder: $holder)}" >&2
            return 75
        fi

        if [ "$waited" -eq 0 ]; then
            local holder
            holder="$(head -2 "$lock_path" 2>/dev/null | tr '\n' ' ')"
            echo "waiting for the development-instance claim${holder:+ (holder: $holder)} ..."
        fi

        sleep "$omt_lock_poll_seconds"
        waited=$((waited + omt_lock_poll_seconds))
    done
}

# Release the claim explicitly, so the length of the claim is readable in the code that takes it.
#
# Closing the descriptor IS the release: BSD `lockf` holds the lock through `flock(2)` on that
# descriptor, so closing it drops the lock. There is no `-u` to call -- macOS `lockf` accepts only
# -k/-n/-s/-w/-t, and `-u` belongs to the unrelated util-linux tool. The previous version invoked
# `lockf -u -k 9` and hid its failure with `2>/dev/null`; it never did anything, which is why the
# `exec 9>&-` below is the only line that matters and must not be dropped as "the redundant one".
#
# The lock file itself is left in place on purpose: the inode is the thing every worktree agrees on,
# and removing it would let two runs lock different inodes.
omt_lock_release() {
    exec 9>&- 2>/dev/null || true
}
