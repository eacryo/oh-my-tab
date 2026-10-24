#!/bin/bash
# Run every offline selftest in scripts/.
#
# Why this exists: nine selftests already guarded the gate helpers (the lock, the Finder extension
# cleanup, the e2e disturbance gates, the reviewer script, the launch codecs), and **nothing ran
# them**. A selftest nobody invokes is documentation: it can rot without anyone noticing, and the
# invariant it guards is then unguarded while looking guarded.
#
# Usage: scripts/selftests.sh
# Exit:  0 = every selftest passed; 1 = at least one failed or could not run.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"

failures=0
ran=0
failed_names=()

for test_script in "$script_dir"/*-selftest.sh; do
    [ -x "$test_script" ] || {
        echo "selftests: $test_script is not executable; a selftest that cannot run is not a pass" >&2
        failures=$((failures + 1))
        failed_names+=("$(basename "$test_script") (not executable)")
        continue
    }
    ran=$((ran + 1))
    name="$(basename "$test_script")"
    echo "== selftest: $name =="
    if "$test_script" </dev/null; then
        echo "selftests: $name ok"
    else
        status=$?
        echo "selftests: $name FAILED (exit $status)" >&2
        failures=$((failures + 1))
        failed_names+=("$name")
    fi
done

if [ "$ran" -eq 0 ]; then
    echo "selftests: no selftest found in $script_dir" >&2
    exit 1
fi

if [ "$failures" -ne 0 ]; then
    echo "selftests: $failures of $ran failed:" >&2
    printf '  %s\n' "${failed_names[@]}" >&2
    exit 1
fi

echo "selftests ok: $ran selftest(s)"
