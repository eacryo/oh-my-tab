#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed uses this
# mapping to select it; a selftest without a declaration is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/doc-stats.py,scripts/doc-stats-selftest.sh,docs/
# Offline self-test for scripts/doc-stats.py: no network, no build, no app.
#
# Why each case exists: the statistics are the instrument behind the quantitative claims in
# docs/doc-style-en.md, so they need counter-examples — a check that only sees a twin in agreement
# cannot tell agreement from a parser that reads nothing at all.
#   - an empty parse reports "same" for two documents with no rules, which is the failure mode a
#     comparison of two empty sets hides;
#   - a missing rule, a relabelled rule and a renumbered rule must each be caught;
#   - the twin pair must be derived from the file names, so the check follows a new twin by itself;
#   - the script must refuse an unknown argument instead of silently doing something else.
#
# Usage: scripts/doc-stats-selftest.sh        Exit 0 = every case held.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(dirname -- "$script_dir")
failures=0

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

# A throwaway tree laid out like the repository: docs/ holds one twin pair.
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
mkdir -p "$workdir/scripts" "$workdir/docs"
cp "$script_dir/doc-stats.py" "$workdir/scripts/doc-stats.py"
chmod +x "$workdir/scripts/doc-stats.py"

write_pair() {
    cat >"$workdir/docs/demo-en.md" <<'EOF'
# Demo

1.1 MUST do the first thing.
1.2 SHOULD do the second thing.
EOF
    cat >"$workdir/docs/demo.md" <<'EOF'
# 示例

1.1【必】做第一件事。
1.2【宜】做第二件事。
EOF
}

reset_tree() {
    rm -rf "$workdir/docs"
    mkdir -p "$workdir/docs"
}
write_guide() {
    printf '# Documentation Style\n\n1.1 MUST do the first thing.\n1.2 SHOULD do the second thing.\n' >"$workdir/docs/doc-style-en.md"
    printf '# 文档写作规范\n\n1.1【必】做第一件事。\n1.2【宜】做第二件事。\n' >"$workdir/docs/doc-style.md"
}
start() {
    reset_tree
    write_guide
    write_pair
}

run() { ( cd "$workdir" && python3 scripts/doc-stats.py "$@" ); }

start
# Every case tree carries a healthy guide pair, because the guide is required: a tree without it would
# fail for the wrong reason. The cases that break the guide do so explicitly below.
if run --check >/dev/null 2>&1; then
    pass "an agreeing twin pair passes"
else
    fail "an agreeing twin pair was refused"
fi
counted=$(run | sed -n 's/^pair demo\.md\/demo-en\.md .*en_rules= *\([0-9]*\).*/\1/p' | tr -d '[:space:]')
if [ "$counted" = "2" ]; then
    pass "both rules are counted"
else
    fail "the counter read '$counted' rules, not 2: an empty parse would agree with an empty parse"
fi

# A rule that exists in only one twin.
printf '# 示例\n\n1.1【必】做第一件事。\n' >"$workdir/docs/demo.md"
if run --check >/dev/null 2>&1; then
    fail "a rule missing from the Chinese twin was not caught"
else
    pass "a rule missing from one twin is caught"
fi

# The same numbers, a different level: the rule means something else now.
start
printf '# 示例\n\n1.1【宜】做第一件事。\n1.2【宜】做第二件事。\n' >"$workdir/docs/demo.md"
if run --check >/dev/null 2>&1; then
    fail "a rule whose required level changed was not caught"
else
    pass "a level change in one twin is caught"
fi

# A pair with no numbered rules is prose, not a rule document: it must be skipped, not failed.
write_pair
printf '# Demo\n\nNo numbered rules here.\n' >"$workdir/docs/prose-en.md"
printf '# 说明\n\n这里没有编号条款。\n' >"$workdir/docs/prose.md"
if run --check >/dev/null 2>&1; then
    pass "a prose twin pair is skipped"
else
    fail "a prose twin pair failed the check"
fi

# The pair list comes from the file names: a new twin is compared without the script changing.
if run | grep -q 'prose.md/prose-en.md'; then
    pass "a twin pair is found from its file names"
else
    fail "a twin pair present on disk was not found"
fi

if run --nonsense >/dev/null 2>&1; then
    fail "an unknown argument was accepted"
else
    pass "an unknown argument is refused"
fi

# A rule number used twice: two rules where a reader sees one, so the comparison would agree with
# whichever copy came last. The check must refuse the document rather than compare it.
write_pair
printf '# Demo\n\n1.1 MUST do the first thing.\n1.1 SHOULD do it again.\n' >"$workdir/docs/demo-en.md"
if run --check >/dev/null 2>&1; then
    fail "a document that numbers two rules the same way was accepted"
else
    pass "a repeated rule number is refused"
fi

# Only one side parses: the other language's rule marker has stopped matching. That is a broken
# document, not a pair to skip.
write_pair
printf '# 示例\n\n第一条：做第一件事。\n' >"$workdir/docs/demo.md"
if run --check >/dev/null 2>&1; then
    fail "a twin whose rules parse on one side only was accepted"
else
    pass "a one-sided parse is refused, not skipped"
fi

# A single rule is a stub standing in for a rule document: below the floor. The guide stays healthy, so
# the case can only fail for the floor -- without it, the missing guide would fail it for another reason.
start
printf '# Demo\n\n1.1 MUST do the first thing.\n' >"$workdir/docs/demo-en.md"
printf '# 示例\n\n1.1【必】做第一件事。\n' >"$workdir/docs/demo.md"
if run --check >/dev/null 2>&1; then
    fail "a one-rule pair satisfied the parity check"
else
    pass "a rule document below the floor is refused"
fi

# No pair at all is not "nothing to compare", it is a tree this check cannot read.
reset_tree
mkdir -p "$workdir/docs"
if run --check >/dev/null 2>&1; then
    fail "a docs directory with no twin pair passed the check"
else
    pass "a docs directory with no twin pair fails"
fi
write_pair

# The guide pair absent altogether, with a healthy prose pair present: the guide is required, so this is
# a failure rather than "nothing to check".
start
rm "$workdir/docs/doc-style.md" "$workdir/docs/doc-style-en.md"
if run --check >/dev/null 2>&1; then
    fail "a tree without the guide pair passed"
else
    pass "a tree without the guide pair fails"
fi

# The guide itself, broken while another pair is healthy: the check must not be satisfied by the
# healthy pair. Two cases, because the two failures are different -- the guide's own files emptied out,
# and the guide's Chinese file gone altogether.
rm -rf "$workdir/docs"
mkdir -p "$workdir/docs"
printf '# 示例\n\n1.1【必】做第一件事。\n1.2【宜】做第二件事。\n' >"$workdir/docs/demo.md"
printf '# Demo\n\n1.1 MUST do the first thing.\n1.2 SHOULD do the second thing.\n' >"$workdir/docs/demo-en.md"
printf '# Documentation Style\n\nNo rules here.\n' >"$workdir/docs/doc-style-en.md"
printf '# 文档写作规范\n\n这里没有条款。\n' >"$workdir/docs/doc-style.md"
if run --check >/dev/null 2>&1; then
    fail "an empty guide passed while another pair was healthy"
else
    pass "an empty guide fails even when another pair is healthy"
fi
rm "$workdir/docs/doc-style.md"
if run --check >/dev/null 2>&1; then
    fail "a guide with its Chinese file removed passed while another pair was healthy"
else
    pass "a guide missing one language fails even when another pair is healthy"
fi

# The real repository: the guide's own parity claim must hold there, or the number it cites is false.
if ( cd "$repo_dir" && python3 scripts/doc-stats.py --check >/dev/null 2>&1 ); then
    pass "the repository's own twin pairs agree"
else
    fail "the repository's own twin pairs disagree"
fi

if [ "$failures" -ne 0 ]; then
    printf '%d case(s) failed\n' "$failures" >&2
    exit 1
fi
echo "doc-stats selftest ok"