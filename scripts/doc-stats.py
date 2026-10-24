#!/usr/bin/env python3
"""Print the measured facts this repository's documentation cites.

Why this exists: docs/doc-style-en.md rule 10.1 says every quantitative claim needs one
named command that reproduces it. The guide itself makes four counts (rules per file,
twin parity, glossary size, checklist items), and a hand count is a number nobody can
re-check. This script makes each of them reproducible and exits non-zero when a twin
pair disagrees, so the claim can be a gate instead of a sentence.

Usage:
  scripts/doc-stats.py                    # every metric, one line each
  scripts/doc-stats.py --check            # only the paired-file parity check; exit 1 on a mismatch
  scripts/doc-stats.py --help

Exit status:
  0  every requested check passed
  1  a twin pair disagrees (--check), or a document named on the command line is missing
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = os.path.join(ROOT, "docs")

# docs/<stem>.md is Chinese and docs/<stem>-en.md is English; the English twin is normative
# (docs/doc-style-en.md 4.6). The pairs are derived from the file names, so a new twin is
# compared without this script being edited; a transcribed table would drift exactly the way the
# rule it checks is meant to prevent.
RULE = re.compile(r"^([0-9]+\.[0-9]+) (MUST|SHOULD)\b")
RULE_ZH = re.compile(r"^([0-9]+\.[0-9]+)\s*【(必|宜)】")
LEVEL = {"MUST": "必", "SHOULD": "宜", "必": "必", "宜": "宜"}
CHECKLIST = re.compile(r"^- \[ \] ")


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read().splitlines()


def rules(path, zh):
    """Rule numbers and levels, keyed by number.

    A repeated number raises instead of overwriting: a document that numbers two rules 4.2 has
    two rules where a reader sees one, and the comparison against its twin would silently agree
    with whichever came last. A parse that disagrees with itself is not evidence of agreement.
    """
    found = {}
    for line_no, line in enumerate(read(path), start=1):
        m = (RULE_ZH if zh else RULE).match(line)
        if m:
            number = m.group(1)
            if number in found:
                raise ValueError("%s:%d: rule %s appears twice" % (path, line_no, number))
            found[number] = LEVEL[m.group(2)]
    return found


def checklist_items(path):
    return sum(1 for line in read(path) if CHECKLIST.match(line))


def glossary_terms(path):
    """Rows of the one six-column table whose header names a banned substitute.

    The header is found from its text, not from a line number, so the count survives an edit
    that adds a section above the table. Every other table in the file has a different header.
    """
    lines = read(path)
    start = None
    for i, line in enumerate(lines):
        if line.startswith("| Term") and "Do not write" in line:
            start = i + 2  # the header and its separator
            break
    if start is None:
        return 0
    count = 0
    for line in lines[start:]:
        if not line.startswith("| "):
            break
        count += 1
    return count


def docs_pairs():
    """Every existing twin pair under docs/, derived from the file names, not transcribed."""
    names = sorted(os.listdir(DOCS))
    pairs = []
    for name in names:
        if name.endswith("-en.md"):
            twin = name[: -len("-en.md")] + ".md"
            if twin in names:
                pairs.append((twin, name))
    return pairs


def report():
    print("root: " + ROOT)
    for zh_name, en_name in docs_pairs():
        zh_path = os.path.join(DOCS, zh_name)
        en_path = os.path.join(DOCS, en_name)
        zh_rules, en_rules = rules(zh_path, True), rules(en_path, False)
        same = zh_rules == en_rules
        print(
            "pair %-34s en_rules=%3d zh_rules=%3d parity=%s"
            % (zh_name + "/" + en_name, len(en_rules), len(zh_rules), "same" if same else "DIFFERENT")
        )
        if not same:
            only_en = sorted(set(en_rules) - set(zh_rules))
            only_zh = sorted(set(zh_rules) - set(en_rules))
            level = sorted(n for n in set(en_rules) & set(zh_rules) if en_rules[n] != zh_rules[n])
            if only_en:
                print("    only in en: " + ", ".join(only_en))
            if only_zh:
                print("    only in zh: " + ", ".join(only_zh))
            if level:
                print("    level differs: " + ", ".join(level))
    # The guide's own counts are printed only where the guide exists: a scratch tree has no
    # doc-style.md, and a report about a document that is not there is not a fact about the repo.
    style_en = os.path.join(DOCS, "doc-style-en.md")
    style_zh = os.path.join(DOCS, "doc-style.md")
    if os.path.exists(style_en) and os.path.exists(style_zh):
        en_rules = rules(style_en, False)
        zh_rules = rules(style_zh, True)
        print("doc-style: rules en=%d zh=%d checklist_en=%d checklist_zh=%d glossary_terms=%d"
              % (len(en_rules), len(zh_rules), checklist_items(style_en), checklist_items(style_zh),
                 glossary_terms(style_en)))
    return 0


# A document whose twin exists must parse to at least this many rules. A twin pair that parses to
# zero is prose (a README), and a pair where only ONE side parses is a broken document, not a skip:
# the rule marker of one language has stopped matching, which is exactly the failure this check
# exists to catch. The number stops a one-rule stub from standing in for a 108-rule guide.
GUIDE_RULE_FLOOR = 2


def check():
    failures = 0
    pairs = docs_pairs()
    if not pairs:
        print("check: no twin pair found under docs/; that cannot be right", file=sys.stderr)
        return 1
    try:
        parsed = [(zh, en, rules(os.path.join(DOCS, zh), True), rules(os.path.join(DOCS, en), False))
                  for zh, en in pairs]
    except ValueError as exc:
        print("FAIL " + str(exc), file=sys.stderr)
        return 1
    # The guide itself is the one pair this repository cannot afford to skip: both files must exist,
    # both must parse, and neither may be hidden behind another pair that happens to be healthy.
    guide_zh = os.path.join(DOCS, "doc-style.md")
    guide_en = os.path.join(DOCS, "doc-style-en.md")
    for guide_path in (guide_en, guide_zh):
        if not os.path.exists(guide_path):
            print("FAIL the guide is missing: " + guide_path, file=sys.stderr)
            failures += 1
    parsed_guide = [p for p in pairs if p[0] == "doc-style.md"]
    if not parsed_guide and os.path.exists(guide_en) and os.path.exists(guide_zh):
        print("FAIL doc-style.md and doc-style-en.md exist but are not a pair", file=sys.stderr)
        failures += 1
    for zh_name, en_name, zh_rules, en_rules in parsed:
        if not zh_rules and not en_rules:
            # A twin pair with no numbered rules on either side is prose, not a rule document.
            continue
        if not zh_rules or not en_rules:
            print("FAIL %s / %s: numbered rules parse on one side only (en=%d, zh=%d)"
                  % (zh_name, en_name, len(en_rules), len(zh_rules)), file=sys.stderr)
            failures += 1
            continue
        if len(en_rules) < GUIDE_RULE_FLOOR:
            print("FAIL %s: %d rule(s) parsed, fewer than the %d a rule document needs"
                  % (en_name, len(en_rules), GUIDE_RULE_FLOOR), file=sys.stderr)
            failures += 1
            continue
        if zh_rules != en_rules:
            print("FAIL %s / %s: rule numbers or levels differ" % (zh_name, en_name), file=sys.stderr)
            failures += 1
        else:
            print("ok   %s / %s: %d rules" % (zh_name, en_name, len(en_rules)))
    guide_rules = [p for p in parsed if p[0] == "doc-style.md"]
    if guide_rules and (not guide_rules[0][2] or not guide_rules[0][3]):
        print("FAIL the guide parses to zero rules in %s"
              % ("zh" if not guide_rules[0][2] else "en"), file=sys.stderr)
        failures += 1
    if not any(parsed_pair[2] or parsed_pair[3] for parsed_pair in parsed):
        # Every pair is prose. For this repository that means the guide's own markers are gone.
        print("FAIL no document under docs/ parsed a single numbered rule", file=sys.stderr)
        failures += 1
    return 1 if failures else 0


def main(argv):
    if any(a in ("-h", "--help") for a in argv):
        print(__doc__)
        return 0
    unknown = [a for a in argv if a != "--check"]
    if unknown:
        print("doc-stats: unknown argument: " + " ".join(unknown), file=sys.stderr)
        return 2
    return check() if "--check" in argv else report()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))