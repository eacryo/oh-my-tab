#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed uses this
# mapping to select it; a selftest without a declaration is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/lib/decode-launch-field.py,scripts/dev-launch-codec-selftest.sh,src/logger.rs
# Round-trip check for the launch-record field codec, run against the code the script actually uses.
#
# Why this exists: the Rust test pins what the ENCODER produces, but the defect that survived an
# earlier round was in the CONSUMER (a sed pipeline with the wrong backslash count). A test that
# only asserts the encoder's output passes with that consumer restored, so the pair is exercised
# here: each case is encoded exactly as src/logger.rs encodes it, then decoded by
# scripts/lib/decode-launch-field.py -- the same file dev-restart.sh pipes through.
#
# Usage: scripts/dev-launch-codec-selftest.sh      Exit 0 = every case round-trips.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
decoder="$script_dir/lib/decode-launch-field.py"
failures=0

check() {
    local name="$1" plain="$2" encoded="$3" got
    got=$(printf '%s' "$encoded" | python3 "$decoder")
    if [ "$got" = "$plain" ]; then
        printf 'ok   %s\n' "$name"
    else
        printf 'FAIL %s: encoded %s decoded to %q, expected %q\n' "$name" "$encoded" "$got" "$plain"
        failures=$((failures + 1))
    fi
}

# A path with a space is the case a sed pipeline with the wrong backslash count breaks.
check 'a space' '/Users/me/Oh My Tab.app/Contents/MacOS/oh-my-tab' '/Users/me/Oh\sMy\sTab.app/Contents/MacOS/oh-my-tab'
# A literal backslash must survive: the encoder writes it as a doubled backslash.
check 'a literal backslash' 'C:\dir\bin' 'C:\\dir\\bin'
# Each remaining escape the encoder emits.
check 'an equals sign' 'version=1' 'version\e1'
check 'square brackets' '[launch]' '\olaunch\c'
check 'the empty marker' '' '-'
# Text that already looks like an escape must not be decoded twice, and an unknown sequence is kept.
check 'a literal backslash-s' '\s' '\\s'
check 'an unknown escape' 'a\qb' 'a\qb'

# The counterexample that defeated the sed version: several escapes in one value, resolved in one
# left-to-right pass. A pipeline that replaces them one rule at a time mis-decodes the backslash.
check 'mixed escapes' 'a b\c=d' 'a\sb\\c\ed'

if [ "$failures" -ne 0 ]; then
    printf '\n%d case(s) failed\n' "$failures"
    exit 1
fi
printf '\nall cases round-tripped\n'
