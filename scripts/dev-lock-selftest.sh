#!/bin/bash
# Offline self-test for scripts/lib/dev-lock.sh: no build, no app, no lock on the real repository.
#
# Each case below exists because a plausible implementation passed without it:
#   - a lock that is never actually held (every acquire succeeds) looks fine in the happy path;
#   - a lock taken on a file a second process never opens does not exclude anyone;
#   - a released descriptor that keeps the claim blocks the next run for ever;
#   - an unresolvable repository must fail rather than run unlocked.
#
# Usage: scripts/dev-lock-selftest.sh        Exit 0 = every case held.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
lib="$script_dir/lib/dev-lock.sh"
failures=0

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

make_repo() {
    local root="$1"
    git -C "$root" init -q 2>/dev/null || return 1
    git -C "$root" config user.email t@example.invalid
    git -C "$root" config user.name t
    : >"$root/keep"
    git -C "$root" add keep
    git -C "$root" commit -qm init
}

repo="$workdir/repo"
mkdir -p "$repo"
make_repo "$repo" || { echo "cannot create the test repository" >&2; exit 2; }
lock_path=$(bash -c "source $lib; omt_lock_dir $repo")
# Compare physical directories: /var and /private/var are one directory here, and git reports a
# relative --git-common-dir inside a repository, so a raw string comparison fails on a correct
# implementation.
lock_dir=$(CDPATH= cd -P -- "$(dirname -- "$lock_path")" && pwd)
common_dir=$(CDPATH= cd -P -- "$repo/.git" && pwd)
if [ "$lock_dir" = "$common_dir" ]; then
    pass "the lock lives in the git common directory"
else
    fail "the lock resolved outside the common directory: $lock_path"
fi

wt="$workdir/wt"
git -C "$repo" worktree add -q "$wt" -b side 2>/dev/null \
    || { echo "cannot create the test worktree" >&2; exit 2; }
wt_lock=$(bash -c "source $lib; omt_lock_dir $wt")
wt_lock_dir=$(CDPATH= cd -P -- "$(dirname -- "$wt_lock")" && pwd)
if [ "$wt_lock_dir" = "$lock_dir" ]; then
    pass "a worktree shares the parent repository lock"
else
    fail "a worktree got its own lock ($wt_lock); two worktrees would not exclude each other"
fi

# The holder must CONFIRM it took the claim before the competitor starts. Without this check the
# whole suite passes while acquire fails every time: the competitor would see "busy" for the wrong
# reason, and the later positive cases would report the release, not the acquire.
holder_log="$workdir/holder.log"
( bash -c "source $lib; omt_lock_acquire $lock_path 5 || exit 9; echo held; sleep 3" >"$holder_log" 2>&1 ) &
holder_pid=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do
    grep -q held "$holder_log" 2>/dev/null && break
    sleep 0.2
done
if grep -q held "$holder_log" 2>/dev/null; then
    pass "the first acquisition actually held the claim"
else
    fail "the holder never held the claim, so the competition case below proves nothing"
fi
bash -c "source $lib; omt_lock_acquire $lock_path 1" >/dev/null 2>&1
second_status=$?
if [ "$second_status" -eq 75 ]; then
    pass "a held claim makes the second run time out (75), it does not proceed"
else
    fail "a held claim returned $second_status instead of 75; two runs would share the instance"
fi
wait "$holder_pid" 2>/dev/null

# Each acquisition is asserted on its own, so a release can never stand in for a failed acquire.
bash -c "source $lib; omt_lock_acquire $lock_path 5 || exit 9; omt_lock_release" >/dev/null 2>&1
after_release=$?
if [ "$after_release" -eq 0 ]; then
    pass "the claim is available again after release"
else
    fail "the claim stayed held after release, or the acquisition failed (exit $after_release)"
fi

bash -c "source $lib; omt_lock_acquire $lock_path 1 || exit 9; omt_lock_release" >/dev/null 2>&1
leak_status=$?
if [ "$leak_status" -eq 0 ]; then
    pass "a released claim does not leak into the next run"
else
    fail "a released claim was still held by the next run, or the acquisition failed (exit $leak_status)"
fi

# The release must not depend on a `lockf` flag this platform does not have. `lockf -u` is the
# util-linux spelling of "unlock"; BSD/macOS `lockf` accepts only -k/-n/-s/-w/-t, so a release built
# on `lockf -u` silently does nothing and passes only because the descriptor close also releases the
# lock. Asserting the flag directly keeps that mistake from being reintroduced as "a redundant line
# someone tidied away".
if /usr/bin/lockf -u -k 9 >/dev/null 2>&1; then
    fail "this platform's lockf accepts -u, so the release's assumptions need re-checking"
else
    pass "this platform's lockf rejects -u, so the release must close the descriptor"
fi
# Matched only in executable lines: the comment above explains the flag, and the explanation must
# not read as the mistake it warns about.
if grep -E '^[[:space:]]*[^#[:space:]].*lockf -u' "$lib" >/dev/null; then
    fail "the library still calls 'lockf -u', which fails on this platform and hides its error"
else
    pass "the release does not call the non-existent 'lockf -u'"
fi

# The meta-counterexample: an acquire that always fails must make this suite fail. It is checked by
# running the positive case against a shim that cannot take the claim.
cat >"$workdir/broken-lib.sh" <<EOF
source "$lib"
omt_lock_acquire() { return 1; }
EOF
bash -c "source $workdir/broken-lib.sh; omt_lock_acquire $lock_path 1 || exit 9" >/dev/null 2>&1
broken_status=$?
if [ "$broken_status" -eq 9 ]; then
    pass "a failing acquisition is visible to the caller, so a suite cannot pass on it"
else
    fail "a failing acquisition returned $broken_status instead of 9; the suite could pass on it"
fi

not_a_repo="$workdir/plain"
mkdir -p "$not_a_repo"
bash -c "source $lib; omt_lock_dir $not_a_repo" >/dev/null 2>&1
resolve_status=$?
if [ "$resolve_status" -ne 0 ]; then
    pass "an unresolvable repository refuses instead of running unlocked"
else
    fail "an unresolvable repository resolved a lock; the run would proceed unprotected"
fi

if [ "$failures" -ne 0 ]; then
    printf '\n%d case(s) failed\n' "$failures"
    exit 1
fi
printf '\nall cases held\n'
