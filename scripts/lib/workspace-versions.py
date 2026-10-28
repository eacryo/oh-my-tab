#!/usr/bin/env python3
"""Check that every workspace member carries the ROOT Cargo.toml's version.

The command-line twin of src/version_consistency.rs, for the scripts that must answer this before
any Rust test can run: scripts/dev-restart.sh, so a development restart cannot launch a build
whose embedded crates claim another version, and scripts/bundle.sh, which until now carried its
own copy of the comparison.

Manifests are parsed with a real TOML tokenizer instead of being scanned for `version = "..."`,
for the reason src/version_consistency.rs gives: a hand-rolled scan reads a version inside a
multi-line string, a comment body or an array element as if it were the package's own version,
and then reports agreement for a tree that disagrees.

The tokenizer is tomllib, which python3 only has from 3.11 on -- macOS still ships 3.9 at
/usr/bin/python3. A python without it must NOT read as disagreement: the caller would report
"workspace versions disagree" for a workspace that is fine. It exits 2 with the interpreter's
version and what is missing, the same shape scripts/agent-review.sh uses for its config file.

Members come from the root manifest's [workspace] members list, the same list cargo builds. A
glob there is refused rather than expanded by guesswork: silently checking a subset would report
agreement for members it never read (the list this repository ships is explicit).

Usage: workspace-versions.py [--root <dir>]
On a disagreement it prints `<manifest>\t<found>\t<expected>` per line on stderr.
Exit: 0 every member agrees with the root; 1 at least one member disagrees, or states a version
      this check cannot compare; 2 the check itself could not be completed (an unreadable,
      malformed or version-less manifest, a `members` glob, no members, no tomllib). 2 is never
      reported as agreement and never as disagreement.
"""

import os
import sys

try:
    import tomllib
except ModuleNotFoundError:  # python3 older than 3.11: macOS still ships 3.9 at /usr/bin/python3
    print(
        "python3 %d.%d has no tomllib, so the workspace versions cannot be checked; "
        "Python 3.11 or newer is needed for that check."
        % (sys.version_info[0], sys.version_info[1]),
        file=sys.stderr,
    )
    raise SystemExit(2)


def _fail(message):
    print("error: " + message, file=sys.stderr)
    return 2


class Unreadable(Exception):
    """The manifest does not state a literal version this check can compare."""


class Disagrees(Exception):
    """The manifest states a version, and it is not the root one."""


def package_version(path):
    """The version in a manifest's own [package] table, as a literal string.

    Three outcomes, kept apart because the caller reports them differently:
      * a literal version            -> returned
      * a version it cannot compare  -> Disagrees (an inherited version is a real difference in
        the shipped metadata, not a manifest this check failed to read)
      * a manifest it cannot read    -> Unreadable (nothing was decided)
    """
    with open(path, "rb") as handle:
        parsed = tomllib.load(handle)
    if "package" not in parsed:
        raise Unreadable("no [package] table (a virtual manifest has no version)")
    package = parsed["package"]
    if "version" not in package:
        raise Unreadable("the [package] table has no version")
    version = package["version"]
    if isinstance(version, dict):
        raise Disagrees("the version is inherited (version.workspace = true), not a literal")
    if not isinstance(version, str):
        raise Disagrees("the version is not a string")
    if not version.strip():
        raise Disagrees("the version is empty")
    return version


def member_paths(root, manifest):
    """Workspace member directories, in the order the root manifest lists them."""
    with open(manifest, "rb") as handle:
        parsed = tomllib.load(handle)
    workspace = parsed.get("workspace")
    if workspace is None:
        raise ValueError("the root manifest has no [workspace] table")
    members = workspace.get("members") or []
    if not isinstance(members, list) or not members:
        raise ValueError("the [workspace] table lists no members, so there is nothing to check")
    for entry in members:
        if not isinstance(entry, str):
            raise ValueError("a [workspace] members entry is not a string")
        if any(ch in entry for ch in "*?["):
            raise ValueError("the [workspace] members entry " + repr(entry) + " is a glob; list the members explicitly")
    return members


def main(argv):
    root = os.getcwd()
    args = list(argv[1:])
    while args:
        arg = args.pop(0)
        if arg == "--root" and args:
            root = os.path.abspath(args.pop(0))
        else:
            return _fail("unknown argument: " + arg)

    root_manifest = os.path.join(root, "Cargo.toml")
    # Every way the root can fail is exit 2, never a disagreement: without a basis to compare
    # against, nothing was decided, and saying "members disagree" would be a false accusation that
    # also tells the caller to bump versions it may not need to touch. Unreadable and Disagrees are
    # listed explicitly here -- both can be raised by package_version, and letting either escape
    # would print a traceback and exit 1.
    try:
        expected = package_version(root_manifest)
        members = member_paths(root, root_manifest)
    except Disagrees as err:
        return _fail("the root manifest states no usable version: " + str(err))
    except (OSError, Unreadable) as err:
        return _fail("cannot read the root manifest: " + str(err))
    except (ValueError, tomllib.TOMLDecodeError) as err:
        return _fail("cannot read the root manifest: " + str(err))

    root_real = os.path.realpath(root_manifest)
    checked = 0
    disagreements = 0
    unreadable = 0
    for member in members:
        manifest = os.path.normpath(os.path.join(root, member, "Cargo.toml"))
        if os.path.realpath(manifest) == root_real:
            continue
        relative = os.path.relpath(manifest, root)
        checked += 1
        try:
            found = package_version(manifest)
        except Disagrees as err:
            # Readable, but it states something other than a literal version: a real difference in
            # the shipped metadata, reported as a disagreement.
            print(relative + "\t" + str(err) + "\t" + expected, file=sys.stderr)
            disagreements += 1
            continue
        except (OSError, Unreadable, tomllib.TOMLDecodeError) as err:
            # Nothing was decided: exit 2, so a typo never reads as a version conflict.
            print(relative + "\t" + "cannot be read: " + str(err) + "\t" + expected, file=sys.stderr)
            unreadable += 1
            continue
        if found != expected:
            print(relative + "\t" + found + "\t" + expected, file=sys.stderr)
            disagreements += 1

    if disagreements:
        return 1
    if unreadable:
        return 2
    if checked == 0:
        # Nothing was compared, which must never read as agreement.
        return _fail("no workspace member was checked")
    print("workspace versions agree at " + expected)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except tomllib.TOMLDecodeError as err:
        sys.exit(_fail("malformed TOML: " + str(err)))
