"""The verdict a focus scenario reaches when it cannot get to its subject.

An unrun phase must never outrank a failure. The failure this guards against: the cross-desktop
scenario could not find its target card, printed the checks, and exited 0 as "unrun" -- which also
swallowed assertions that had already failed (a switch that never turned on, an AX-identity
invariant, a missing card). The suite then recorded a skipped phase instead of a broken one.

`python3 scripts/e2e/lib/verdict.py` runs the regression cases.
"""

from __future__ import annotations

import sys


def unrun_or_fail(checks, problems, reason, exit_=SystemExit):
    """Print `checks`, then: an already-failed assertion keeps the failure; otherwise unrun, exit 0."""
    print("\n".join(checks))
    if problems:
        print("\n".join(problems), file=sys.stderr)
        raise exit_(1)
    print(f"NOT RUN: {reason}")
    raise exit_(0)


def _selftest() -> None:
    import contextlib
    import io

    def run(checks, problems):
        out, err = io.StringIO(), io.StringIO()
        code = None
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                unrun_or_fail(checks, problems, "the reason")
            except SystemExit as exit_:
                code = exit_.code
        return code, out.getvalue(), err.getvalue()

    # No failed assertion: unrun, and the reason is in the log for the suite to report.
    code, out, _ = run(["a check"], [])
    assert code == 0, code
    assert "NOT RUN: the reason" in out, out

    # A failed assertion: the failure survives, and its text reaches stderr.
    code, _, err = run(["a check"], ["a broken assertion"])
    assert code == 1, code
    assert "a broken assertion" in err, err

    # The checks are printed either way, so the log always says what was verified.
    code, out, _ = run(["a check"], ["a broken assertion"])
    assert "a check" in out, out
    print("verdict selftest: ok")


if __name__ == "__main__":
    _selftest()
    sys.exit(0)
