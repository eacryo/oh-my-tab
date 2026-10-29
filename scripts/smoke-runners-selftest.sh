#!/bin/bash
# SELFTEST_INPUTS=scripts/smoke-runners.sh,scripts/smoke-runners-selftest.sh,scripts/lib/smoke-cells.awk,src/
# Offline selftest for scripts/smoke-runners.sh admission: the M1/M2 declaration reader.
#
# Why it exists: the reader decides which runners the daily gate may run. If it guesses when a
# declaration is missing, borrowed or contradictory, a disturbing runner ends up in M1 and the daily
# gate starts launching windows. The reader's own default is "refuse", and this checks that it refuses
# for the right reasons -- against synthetic source files, with no cargo, no app and no GUI.
#
#   scripts/smoke-runners-selftest.sh
#
# Exit code: 0 = every case behaved as specified; non-zero when a case did not.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
failures=0
cases=0

fail() { echo "smoke-runners-selftest: FAIL: $*" >&2; failures=$((failures + 1)); }
ok() { cases=$((cases + 1)); }

# The functions are extracted from the shipped script, not restated: a selftest with its own copy
# tests the copy.
extract_function() {  # <file> <name>
    awk -v name="$2" '
        $0 ~ "^" name "\\(\\)" { printing = 1 }
        printing { print }
        printing && /^\}/ { exit }
    ' "$1"
}
reader="$(extract_function "$script_dir/smoke-runners.sh" runner_cell)"
if [ -z "$reader" ]; then
    echo "smoke-runners-selftest: FAIL: could not extract runner_cell" >&2
    exit 1
fi

work="$(mktemp -d -t smoke-runners-selftest)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src"
cd "$work"

# cell_of <short> -> whatever the shipped reader prints, with the working directory as "src/".
cell_of() {
    local short="$1"
    ( eval "$reader"; runner_cell "tests::$short" )
}

write_src() {  # <file> <content>
    printf '%s\n' "$2" > "src/$1"
}

# --- exactly one declaration, directly above the definition, is read -------------------------
write_src a.rs '# SMOKE_CELL=M1
#[test]
#[ignore]
fn one_m1() {}'
[ "$(cell_of one_m1)" = "M1" ] && ok || fail "a single M1 declaration must read as M1 (got $(cell_of one_m1))"

write_src b.rs '# SMOKE_CELL=M2
#[test]
#[ignore]
fn two_m2() {}'
[ "$(cell_of two_m2)" = "M2" ] && ok || fail "a single M2 declaration must read as M2 (got $(cell_of two_m2))"

# --- a missing declaration is refused (nothing printed), never guessed ----------------------
write_src c.rs '#[test]
#[ignore]
fn none() {}'
[ -z "$(cell_of none)" ] && ok || fail "a runner with no declaration must read as nothing (got $(cell_of none))"

# --- two different declarations above one fn are contradictory, not "the last one wins" -----
write_src d.rs '// SMOKE_CELL=M1
// SMOKE_CELL=M2
#[test]
#[ignore]
fn conflicting() {}'
[ "$(cell_of conflicting)" = "AMBIGUOUS" ] && ok || fail "M1 and M2 above one fn must be AMBIGUOUS (got $(cell_of conflicting))"

# --- an unknown value is not silently read as M1 or M2 --------------------------------------
write_src e.rs '// SMOKE_CELL=M10
#[test]
#[ignore]
fn unknown() {}'
case "$(cell_of unknown)" in
    M1|M2) fail "an unknown cell must never be admitted as a real cell (got $(cell_of unknown))" ;;
    *) ok ;;
esac

# --- a declaration with an unknown suffix is not its prefix ----------------------------------
for bad in M1_M2 M1-invalid M1x M1.invalid M2.0; do
    write_src "suffix-$bad.rs" "// SMOKE_CELL=$bad
#[test]
#[ignore]
fn suffixed_runner() {}"
    case "$(cell_of suffixed_runner)" in
        M1|M2) fail "SMOKE_CELL=$bad must not be read as $(cell_of suffixed_runner)" ;;
        *) ok ;;
    esac
    rm -f "src/suffix-$bad.rs"
done

# --- the disturbance reader and its strictness -------------------------------------------------
reader_d="$script_dir/lib/smoke-cells.awk"
write_src dist.rs '// SMOKE_CELL=M2
// SMOKE_DISTURBS=focus,screen
#[test]
#[ignore]
fn captures_runner() {}

// SMOKE_CELL=M1
#[test]
#[ignore]
fn plain_runner() {}'
out="$(awk -f "$reader_d" src/dist.rs)"
case "$out" in
    *"captures_runner|focus,screen|ok"*) ok ;;
    *) fail "the disturbance reader must report focus,screen for captures_runner (got $out)" ;;
esac
# A runner with no declaration is reported as such, never guessed into a cell.
case "$out" in
    *"plain_runner||none"*) ok ;;
    *) fail "an undeclared runner must be reported with status none (got $out)" ;;
esac
# Two different declarations above one fn are a conflict, not the last one winning.
write_src conflict.rs '// SMOKE_DISTURBS=screen
// SMOKE_DISTURBS=focus
#[test]
#[ignore]
fn conflicting_runner() {}'
case "$(awk -f "$reader_d" src/conflict.rs)" in
    *"conflicting_runner|"*"|conflicting"*) ok ;;
    *) fail "conflicting declarations must be reported as conflicting (got $(awk -f "$reader_d" src/conflict.rs))" ;;
esac
# A helper that spawns the smoke must not be mistaken for a test: only a fn carrying #[test] is one.
write_src helper.rs '// SMOKE_DISTURBS=focus
pub(crate) fn helper_runner() -> bool { true }

#[test]
#[ignore]
fn real_smoke() {}'
case "$(awk -f "$reader_d" src/helper.rs)" in
    *helper_runner*) fail "a non-#[test] helper must not appear in the reader output" ;;
    *) ok ;;
esac

# A typo in a marker is not "nothing to authorize": it must be rejected, or a screen capture would run
# under a focus-only authorization.
check_d="$(extract_function "$script_dir/smoke-runners.sh" disturbances_well_formed)"
if [ -z "$check_d" ]; then
    fail "could not extract disturbances_well_formed"
else
    for good in focus screen prefs instance focus,screen; do
        ( eval "$check_d"; disturbances_well_formed "$good" ) && ok || fail "\"$good\" must be well formed"
    done
    for bad in focus,scren focus,focus "" focus, ,focus focusscreen; do
        if ( eval "$check_d"; disturbances_well_formed "$bad" ); then
            fail "\"$bad\" must be rejected"
        else
            ok
        fi
    done
fi

# --- a declaration that belongs to the neighbouring fn must not be borrowed -----------------
write_src f.rs '#[test]
#[ignore]
// SMOKE_CELL=M1
fn declared_runner() {}

#[test]
#[ignore]
fn neighbour_runner() {}'
[ "$(cell_of declared_runner)" = "M1" ] && ok || fail "the declared fn must read M1 (got $(cell_of declared_runner))"
[ -z "$(cell_of neighbour_runner)" ] && ok || fail "a fn must not borrow its neighbour's declaration (got $(cell_of neighbour_runner))"

# --- a longer name is a different runner ----------------------------------------------------
write_src g.rs '#[test]
#[ignore]
fn base_runner() {}

// SMOKE_CELL=M2
#[test]
#[ignore]
fn base_runner_extra() {}'
[ -z "$(cell_of base_runner)" ] && ok || fail "base_runner must not absorb base_runner_extra's declaration (got $(cell_of base_runner))"

# --- a declaration inside the fn body is not the header contract ---------------------------
write_src h.rs '#[test]
#[ignore]
fn inside() {
    // SMOKE_CELL=M1
}'
[ -z "$(cell_of inside)" ] && ok || fail "a declaration inside the body must not be read (got $(cell_of inside))"

# --- a runner defined in no file is reported, not silently skipped -------------------------
[ "$(cell_of nowhere)" = "UNDEFINED" ] && ok || fail "a runner with no definition must read UNDEFINED (got $(cell_of nowhere))"

if [ "$failures" -ne 0 ]; then
    echo "smoke-runners-selftest: $failures failure(s) in $cases case(s)" >&2
    exit 1
fi
echo "smoke-runners-selftest ok: $cases case(s)"
