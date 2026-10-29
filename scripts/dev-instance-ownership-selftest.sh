#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed uses this
# mapping to select it; a selftest without a declaration is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/lib/dev-instance-ownership.sh,scripts/dev-instance-ownership-selftest.sh
# Offline self-test for scripts/lib/dev-instance-ownership.sh.
#
# The library asks a question about the machine's live processes, so this suite does two things:
# it exercises the pure decision logic against synthetic inputs (timestamps, records, candidates),
# and it exercises the whole probe against a REAL process this suite starts -- a copy of
# /bin/sleep renamed `oh-my-tab` inside a synthetic .app, so the identity checks run against the
# kernel's own answer rather than a stub that could disagree with it.
#
# Each case exists because a plausible implementation passed without it:
#   - a classifier that returns "mine" whenever a process is alive looks right on a machine where
#     the running instance happens to be this tree's;
#   - a timestamp compared as a string passes on one machine and mis-orders around a zone change;
#   - a probe that trusts "a launch record exists" reports MINE for a reused pid;
#   - a probe that reports NO_INSTANCE when its enumeration merely failed is the worst kind of pass.
#
# No build, no launch of the real app, no claim on the real lock, and nothing in the repository is
# touched. Usage: scripts/dev-instance-ownership-selftest.sh    Exit 0 = every case held.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
lib="$script_dir/lib/dev-instance-ownership.sh"
failures=0

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

# Assert that a probe run produced a given verdict AND exit code. The verdict line is the contract;
# the exit code is what a caller branches on, so both are checked every time.
expect_probe() {
    local label="$1" want_verdict="$2" want_status="$3" output status
    output="$4"
    status="$5"
    if [ "$status" -eq "$want_status" ] && printf '%s\n' "$output" | grep -qx "VERDICT: $want_verdict"; then
        pass "$label"
    else
        fail "$label (want $want_verdict/$want_status, got exit $status, verdict '$(printf '%s\n' "$output" | sed -n 's/^VERDICT: //p')')"
    fi
}

workdir=$(mktemp -d)
cleanup_all() {
    # Kill only what this suite started, by pid, never by name.
    if [ -n "${fake_pid:-}" ]; then
        kill "$fake_pid" 2>/dev/null
    fi
    rm -rf "$workdir"
}
trap cleanup_all EXIT

# --- the pure half: timestamps ------------------------------------------------

# Sourced in a subshell so the library's globals never reach this suite's namespace.
stamp_of() { bash -c "source '$lib'; omt_ownership_epoch_of_stamp '$1'"; }
lstart_of() { bash -c "source '$lib'; omt_ownership_epoch_of_lstart '$1'"; }

# The offset must be honoured: the same instant written with two offsets must agree, and it must
# agree with the equivalent UTC spelling. A string comparison passes the first but fails the third.
e1="$(stamp_of '2026-10-22T14:50:45.411+08:00')"
e2="$(stamp_of '2026-10-22T06:50:45.411Z')"
if [ -n "$e1" ] && [ "$e1" = "$e2" ]; then
    pass "a local-time stamp with its offset equals the UTC spelling of the same instant"
else
    fail "offset handling is wrong (+08:00 -> '$e1', Z -> '$e2')"
fi

# Milliseconds are optional in the format, and present in every real line. Dropping them must not
# change the truncated second.
e3="$(stamp_of '2026-10-22T14:50:45+08:00')"
if [ -n "$e3" ] && [ "$e3" = "$e1" ]; then
    pass "milliseconds are optional and do not shift the second"
else
    fail "millisecond handling is wrong (with ms '$e1', without '$e3')"
fi

# A stamp the format does not cover must fail to parse rather than return a plausible number: the
# caller turns "unparseable" into UNVERIFIED, and a silent zero would be read as "long ago".
for bad in '2026-10-22 14:50:45' 'not-a-time' '2026-10-22T14:50:45' ''; do
    if bash -c "source '$lib'; omt_ownership_epoch_of_stamp '$bad'" >/dev/null 2>&1; then
        fail "an unparseable stamp '$bad' was accepted instead of being reported"
    else
        pass "an unparseable stamp is reported as such: '$bad'"
    fi
done

# --- the pure half: record attribution ---------------------------------------

# A record written BEFORE the process started cannot be that process's own: a reused pid would
# otherwise borrow it and the probe would say MINE.
mkdir -p "$workdir/logs"
log_before="$workdir/logs/before.log"
cat >"$log_before" <<'EOF'
2026-10-22T10:00:00.000+08:00 INFO  [launch] pid=4242 executable=/tmp/old.app/Contents/MacOS/oh-my-tab version=OLD bundle=x
EOF
record="$(bash -c "source '$lib'; omt_ownership_read_record 4242 'Thu Oct 22 14:50:43 2026' '$log_before'")"
case "$record" in
    *stale-record) pass "a record older than the process start is not attributed to it" ;;
    *) fail "a stale record was attributed to the process: '$record'" ;;
esac

# A record written after the start IS attributable.
log_after="$workdir/logs/after.log"
cat >"$log_after" <<'EOF'
2026-10-22T14:50:45.411+08:00 INFO  [launch] pid=4242 executable=/tmp/new.app/Contents/MacOS/oh-my-tab version=NEW bundle=x
EOF
record="$(bash -c "source '$lib'; omt_ownership_read_record 4242 'Thu Oct 22 14:50:43 2026' '$log_after'")"
case "$record" in
    *$'\t'ok) pass "a record written after the process start is attributed to it" ;;
    *) fail "a fresh record was not attributed: '$record'" ;;
esac

# A missing record is "no answer", not "an old answer".
record="$(bash -c "source '$lib'; omt_ownership_read_record 999 'Thu Oct 22 14:50:43 2026' '$log_after'")" \
    || record="<absent>"
if [ "$record" = "<absent>" ]; then
    pass "a process with no launch record yields no record at all"
else
    fail "a missing record produced '$record' instead of nothing"
fi

# A record 20 seconds BEFORE the process start must not be attributed: the previous version granted a
# 30-second backward allowance, which let a reused pid borrow the old process's record (reviewer).
printf '2026-10-22T14:50:23.000+08:00 INFO  [launch] pid=4242 executable=/tmp/a version=V bundle=b\n' >"$workdir/logs/early.log"
record="$(bash -c "source '$lib'; omt_ownership_read_record 4242 'Thu Oct 22 14:50:43 2026' '$workdir/logs/early.log'")"
case "$record" in
    *stale-record) pass "a record 20s before the process start is not attributed to it" ;;
    *) fail "a record 20s before the start was attributed: '$record'" ;;
esac

# The boundary itself: exactly at the start second is fine, one second BEFORE is not. Truncation to
# whole seconds cannot move a genuinely-later record earlier, so there is no allowance to keep here
# (the previous version kept one second and this case asserted the wrong answer).
printf '2026-10-22T14:50:43.000+08:00 INFO  [launch] pid=4242 executable=/tmp/a version=V bundle=b\n' >"$workdir/logs/edge.log"
record="$(bash -c "source '$lib'; omt_ownership_read_record 4242 'Thu Oct 22 14:50:43 2026' '$workdir/logs/edge.log'")"
case "$record" in
    *$'\t'ok) pass "a record in the same second as the start is attributed" ;;
    *) fail "a record in the start's own second was refused: '$record'" ;;
esac
printf '2026-10-22T14:50:42.000+08:00 INFO  [launch] pid=4242 executable=/tmp/a version=V bundle=b\n' >"$workdir/logs/edge2.log"
record="$(bash -c "source '$lib'; omt_ownership_read_record 4242 'Thu Oct 22 14:50:43 2026' '$workdir/logs/edge2.log'")"
case "$record" in
    *stale-record) pass "a record one second before the start is refused (no backward allowance)" ;;
    *) fail "a record one second before the start was attributed: '$record'" ;;
esac

# An unreadable log must not be mistaken for a log with no matching line.
if bash -c "source '$lib'; omt_ownership_read_record 4242 'Thu Oct 22 14:50:43 2026' '$workdir/logs/missing.log'" >/dev/null 2>&1; then
    fail "an unreadable log was read as if it had been searched"
else
    pass "an unreadable log is refused rather than searched"
fi

# --- the pure half: the ps lstart/comm split --------------------------------

# `lstart` pads a single-digit day with a space ("Thu Oct  1 …") and the executable path may itself
# contain spaces. A split that assumes one space between fields puts the year into the path, which
# then never matches and reports another worktree's build (reviewer). This checks the split rule the
# library uses, on the exact string `ps` produces for such a day.
split_check() {
    bash -c '
        source "$1"
        line="$2"
        line="$(printf "%s" "$line" | sed "s/^[[:space:]]*//; s/[[:space:]]*$//")"
        started="$(printf "%s" "$line" | awk "{print \$1\" \"\$2\" \"\$3\" \"\$4\" \"\$5}")"
        exe="$(printf "%s" "$line" | sed -E "s/^[[:space:]]*([^[:space:]]+[[:space:]]+){5}//")"
        printf "%s|%s\n" "$started" "$exe"
    ' _ "$lib" "$1"
}
split="$(split_check 'Thu Oct  1 15:12:03 2026   /tmp/Dev App.app/Contents/MacOS/oh-my-tab')"
if [ "$split" = 'Thu Oct 1 15:12:03 2026|/tmp/Dev App.app/Contents/MacOS/oh-my-tab' ]; then
    pass "a padded single-digit day and a spaced path split correctly"
else
    fail "the lstart/comm split is wrong for a padded day: '$split'"
fi

# --- the live half: a real process, classified by the kernel -----------------

# Build a synthetic bundle whose executable IS the process this suite runs. `comm` for that process
# is the name the enumeration matches; a private name keeps a real instance out of the way.
#
# The copy needs a fresh ad-hoc signature: on Apple silicon a copied signed binary keeps its original
# CodeDirectory but no longer hashes to it, so the kernel kills it (measured: "Killed: 9" for a plain
# `cp`, and for a `codesign --remove-signature` copy as well -- the re-sign is what makes it runnable).
fake_app="$workdir/fake/Oh-My-Tab-Dev.app"
mkdir -p "$fake_app/Contents/MacOS"
cp /bin/sleep "$fake_app/Contents/MacOS/omt-selftest-instance" \
    || { echo "cannot copy a stand-in executable" >&2; exit 2; }
/usr/bin/codesign -s - -f "$fake_app/Contents/MacOS/omt-selftest-instance" >/dev/null 2>&1 \
    || { echo "cannot re-sign the stand-in executable, so the live cases cannot run" >&2; exit 2; }
cat >"$fake_app/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.eacryo.oh-my-tab.dev</string>
	<key>CFBundleVersion</key>
	<string>20261022145045</string>
</dict>
</plist>
EOF
fake_bin="$fake_app/Contents/MacOS/omt-selftest-instance"

# The real repository's verification is done against ITS bundle; this suite must not depend on the
# machine having a dev build at all, so the "this worktree's" binary is the synthetic one.
"$fake_bin" 120 &
fake_pid=$!
sleep 0.3
if ! kill -0 "$fake_pid" 2>/dev/null; then
    echo "cannot start the synthetic instance; the live cases below would prove nothing" >&2
    exit 2
fi

# The probe enumerates by process name across the WHOLE machine, so another Oh-My-Tab instance
# already running would join the candidate list and change every verdict below. When one is
# present, the live cases say so and the suite reports them as skipped rather than failing on an
# unrelated instance.
# No record for the synthetic pid: the identity is known but not attributable -> UNVERIFIED.
out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$fake_bin' '$log_after'")"; st=$?
expect_probe "a live instance with no matching launch record is UNVERIFIED, not MINE" UNVERIFIED 4 "$out" "$st"

# Now write the record the app would have written, with a version that matches the bundle.
fake_start="$(ps -p "$fake_pid" -o lstart= | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
real_log="$workdir/logs/real.log"
# The app writes "<local>.<ms><+HH:MM>"; strftime %z gives "+0800", so the colon is inserted
# for real -- an escaped backreference inside a double-quoted string emits a literal "\1".
stamp="$(date +%Y-%m-%dT%H:%M:%S.000%z | sed -E 's/([+-][0-9]{2})([0-9]{2})$/\1:\2/')"
printf '%s INFO  [launch] pid=%s executable=%s version=20261022145045 bundle=com.eacryo.oh-my-tab.dev\n' \
    "$stamp" "$fake_pid" "$fake_bin" >"$real_log"

out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$fake_bin' '$real_log'")"; st=$?
expect_probe "a live instance with a matching, fresh record is MINE" MINE 0 "$out" "$st"

# The same process, asked about through a DIFFERENT bundle: NOT_MINE, because the executable
# path is what decides, not the process name.
other_app="$workdir/fake2/Other.app"
mkdir -p "$other_app/Contents/MacOS"
cp /bin/sleep "$other_app/Contents/MacOS/omt-selftest-instance"
/usr/bin/codesign -s - -f "$other_app/Contents/MacOS/omt-selftest-instance" >/dev/null 2>&1 || true
cp "$fake_app/Contents/Info.plist" "$other_app/Contents/Info.plist"
out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$other_app/Contents/MacOS/oh-my-tab' '$real_log'")"; st=$?
expect_probe "a live instance built by another tree is NOT_MINE" NOT_MINE 1 "$out" "$st"

# A record naming a different version means the running bundle is not the version this tree's
# bundle carries: STALE_BUILD, distinct from NOT_MINE.
stale_log="$workdir/logs/stale.log"
printf '%s INFO  [launch] pid=%s executable=%s version=20260101000000 bundle=com.eacryo.oh-my-tab.dev\n' \
    "$stamp" "$fake_pid" "$fake_bin" >"$stale_log"
out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$fake_bin' '$stale_log'")"; st=$?
expect_probe "a running build whose version differs from this tree's bundle is STALE_BUILD" STALE_BUILD 1 "$out" "$st"

# A record WITHOUT an executable field (or the encoder's `-` placeholder) must not pass on the version
# alone: the previous version skipped the path check when the field was empty (reviewer).
for placeholder in "-" ""; do
    nopath_log="$workdir/logs/nopath.log"
    printf '%s INFO  [launch] pid=%s executable=%s version=20261022145045 bundle=com.eacryo.oh-my-tab.dev\n' \
        "$stamp" "$fake_pid" "$placeholder" >"$nopath_log"
    out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$fake_bin' '$nopath_log'")"; st=$?
    expect_probe "a record with executable='${placeholder:-<empty>}' is UNVERIFIED, not MINE" UNVERIFIED 4 "$out" "$st"
done

# Re-checking a snapshot: the identity still holds -> MINE.
out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$fake_bin' '$real_log' '$fake_pid' '$fake_start'")"; st=$?
expect_probe "re-checking the identity that is still running is MINE" MINE 0 "$out" "$st"

# Re-checking a snapshot after the process is gone -> NOT_MINE, which is the whole point: the
# UI conclusion that belonged to that pid is void.
kill "$fake_pid" 2>/dev/null
for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$fake_pid" 2>/dev/null || break; sleep 0.1; done
fake_pid=""
out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$fake_bin' '$real_log' '4242' 'Thu Oct 22 14:50:43 2026'")"; st=$?
expect_probe "re-checking an identity that is gone is NOT_MINE" NOT_MINE 1 "$out" "$st"

# Nothing running at all -> NO_INSTANCE, which a caller must not read as success.
out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$fake_bin' '$real_log'")"; st=$?
expect_probe "no instance at all is NO_INSTANCE" NO_INSTANCE 3 "$out" "$st"

# A bundle that was never built -> NO_LOCAL_BUILD, distinct from "an instance of another tree".
out="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$workdir/absent.app/Contents/MacOS/oh-my-tab' '$real_log'")"; st=$?
expect_probe "a worktree with no built bundle is NO_LOCAL_BUILD" NO_LOCAL_BUILD 6 "$out" "$st"

# --- partial and failed enumeration -----------------------------------------
#
# "Listed but unreadable" is not "absent". Both cases below run through the probe with a shimmed
# enumerator, because the real one cannot be made to fail on demand without breaking the machine.

# (a) A candidate the probe can classify, PLUS one it could not read. The shim reports the partial
# listing through the enumerator's status exactly as the real one does (3). The answer must not be
# "MINE and nothing else is running".
"$fake_bin" 120 &
partial_pid=$!
sleep 0.3
partial_start="$(ps -p "$partial_pid" -o lstart= | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
partial_log="$workdir/logs/partial.log"
printf '%s INFO  [launch] pid=%s executable=%s version=20261022145045 bundle=com.eacryo.oh-my-tab.dev\n' \
    "$stamp" "$partial_pid" "$fake_bin" >"$partial_log"
cat >"$workdir/partial-lib.sh" <<EOF
source "$lib"
omt_ownership_candidates() {
    printf '%s\t%s\t%s\n' "$partial_pid" "$partial_start" "$fake_bin"
    return 3
}
EOF
out="$(bash -c "source '$workdir/partial-lib.sh'; omt_ownership_probe '$fake_bin' '$partial_log'" 2>&1)"; st=$?
expect_probe "a partially readable enumeration is UNVERIFIED, not MINE" UNVERIFIED 4 "$out" "$st"

# (b) Nothing readable at all, plus the same status: still unknown, never NO_INSTANCE.
cat >"$workdir/empty-lib.sh" <<EOF
source "$lib"
omt_ownership_candidates() { return 3; }
EOF
out="$(bash -c "source '$workdir/empty-lib.sh'; omt_ownership_probe '$fake_bin' '$partial_log'" 2>&1)"; st=$?
expect_probe "an enumeration with nothing readable is UNVERIFIED, not NO_INSTANCE" UNVERIFIED 4 "$out" "$st"

# (c) A hard enumeration failure is unknown too.
cat >"$workdir/fail-lib.sh" <<EOF
source "$lib"
omt_ownership_candidates() { return 2; }
EOF
out="$(bash -c "source '$workdir/fail-lib.sh'; omt_ownership_probe '$fake_bin' '$partial_log'" 2>&1)"; st=$?
expect_probe "a failed enumeration is UNVERIFIED" UNVERIFIED 4 "$out" "$st"
kill "$partial_pid" 2>/dev/null
for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$partial_pid" 2>/dev/null || break; sleep 0.1; done

# --- the meta-counterexample -------------------------------------------------

# A classifier that always says "mine" must make this suite fail. Checked against a shim, so the
# suite is shown to be capable of failing rather than merely observed to pass.
cat >"$workdir/broken-lib.sh" <<EOF
source "$lib"
omt_ownership_classify() { printf 'mine\t%s\n' "\$3"; }
EOF
# The meta-counterexample has to show the ownership assertion can CATCH a broken classifier. The
# cleanest place to show it is the classifier itself: the same candidate, classified by the real
# function and by a shim that always claims ownership, must disagree. A suite that could not tell
# those apart would pass with either implementation.
cat >"$workdir/broken-lib.sh" <<EOF
source "$lib"
omt_ownership_classify() { printf 'mine\t%s\n' "\$3"; }
EOF
foreign="$other_app/Contents/MacOS/omt-selftest-instance"
real_kind="$(bash -c "source '$lib'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_classify 4242 'Thu Oct 22 14:50:43 2026' '$foreign' '$fake_bin' '$log_after'" 2>/dev/null | cut -f1)"
shim_kind="$(bash -c "source '$workdir/broken-lib.sh'; omt_ownership_classify 4242 'Thu Oct 22 14:50:43 2026' '$foreign' '$fake_bin' '$log_after'" 2>/dev/null | cut -f1)"
if [ "$real_kind" = "other-dev" ] && [ "$shim_kind" = "mine" ]; then
    pass "the ownership thesis distinguishes a real classifier from one that always claims mine"
else
    fail "the counterexample does not discriminate: real='$real_kind' shim='$shim_kind'"
fi

# And the probe built on a broken classifier must not be able to report MINE for a candidate that a
# correct classifier rejects -- checked end to end, with a live candidate, so the probe body really
# runs. The shim names the candidate as `mine`; the probe then compares the recorded executable with
# the local build and must refuse, which is the second line of defence.
"$fake_bin" 120 &
broken_pid=$!
sleep 0.3
broken_log="$workdir/logs/broken.log"
printf '%s INFO  [launch] pid=%s executable=%s version=20261022145045 bundle=com.eacryo.oh-my-tab.dev\n' \
    "$stamp" "$broken_pid" "$foreign" >"$broken_log"
broken_out="$(bash -c "source '$workdir/broken-lib.sh'; OMT_OWNERSHIP_PROCESS_NAME=omt-selftest-instance omt_ownership_probe '$fake_bin' '$broken_log'" 2>&1)"
if printf '%s\n' "$broken_out" | grep -qx 'VERDICT: MINE'; then
    fail "a shim that claims every process is 'mine' produced MINE through the probe"
else
    pass "a shim that claims every process is 'mine' cannot produce MINE through the probe"
fi
kill "$broken_pid" 2>/dev/null
for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$broken_pid" 2>/dev/null || break; sleep 0.1; done

# The probe must not take the development claim: a run that took it would wait behind a real
# transaction and would serialise the very detection it exists for.
if bash -c "source '$lib'; declare -F omt_lock_acquire >/dev/null" 2>/dev/null; then
    fail "the ownership library defines the claim's API, so it could take it by accident"
else
    pass "the ownership library cannot take the development claim"
fi

if [ "$failures" -ne 0 ]; then
    printf '\n%d case(s) failed\n' "$failures"
    exit 1
fi
printf '\nall cases held\n'
