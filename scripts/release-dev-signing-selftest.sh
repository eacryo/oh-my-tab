#!/usr/bin/env bash
#
# Self-test for the signing-identity resolution and the release gates in scripts/release-dev.sh.
#
# Fully offline: `security`, `codesign` and `cargo` are stubs on PATH, and the packaging script
# (`bundle.sh`) is a stub inside a throwaway scaffold, so no real certificate, no network, no R2 and
# no `dist/` of the repository is touched. The stubs are deliberately consistent: the identity the
# `security` stub reports is the identity the packaging stub stamps into the artifact, so the happy
# paths exercise the real assertion code in release-dev.sh.
#
# What this file cannot decide is whether real codesign output matches those stubs. That is the
# integration check, run by hand on a machine with the certificates:
#   APP_BASENAME=Oh-My-Tab-Dev-SigCheck sh scripts/release-dev.sh --push --dry-run
# then verify the .app, the ZIP and the DMG (docs/releasing-en.md, "Development update channel").
#
#   scripts/release-dev-signing-selftest.sh
#
set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
subject="$script_dir/release-dev.sh"
[[ -f "$subject" ]] || { echo "cannot find $subject" >&2; exit 66; }

# The instrument controls its own environment: this machine exports CODESIGN_IDENTITY for the release
# work, and an inherited value would turn every case into a conflict case.
unset CODESIGN_IDENTITY SIGN_IDENTITY

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

stub="$tmp/bin"
scaffold="$tmp/repo"
mkdir -p "$stub" "$scaffold/scripts" "$scaffold/dist"
cp "$subject" "$scaffold/scripts/release-dev.sh"

identities="$tmp/identities.tsv"
bundle_log="$tmp/bundle.log"
publish_log="$tmp/publish.log"
output="$tmp/output.txt"

# --- stubs ---------------------------------------------------------------------------------------

# `security find-identity -v -p codesigning`, shaped exactly like the real tool, from a controlled list.
cat > "$stub/security" <<'STUB'
#!/bin/sh
[ "${1:-}" = "find-identity" ] || exit 0
index=0
while IFS="$(printf '\t')" read -r hash name; do
  [ -n "$hash" ] || continue
  index=$((index + 1))
  printf '  %d) %s "%s"\n' "$index" "$hash" "$name"
done < "$SELFTEST_IDENTITIES"
printf '     %d valid identities found\n' "$index"
STUB

# codesign answers from the marker the packaging stub leaves inside the artifact: the same code paths
# in release-dev.sh run, against a controlled "signature".
cat > "$stub/codesign" <<'STUB'
#!/bin/sh
app=""
requirement=""
verify=0
for arg in "$@"; do
  case "$arg" in
    --verify) verify=1 ;;
    -R=*) requirement="${arg#-R=}" ;;
    -*) ;;
    *) app="$arg" ;;
  esac
done
marker="$app/Contents/.selftest-identity"
[ -f "$marker" ] || exit 1
if [ "$verify" -eq 0 ]; then
  awk -F'\t' '{ print "Authority=" $2 }' "$marker"
  exit 0
fi
[ "$(cat "$app/Contents/.selftest-integrity" 2>/dev/null || echo 1)" = "1" ] || exit 1
case "$requirement" in
  *'certificate leaf = H"'*)
    want="$(printf '%s' "$requirement" | sed 's/.*H"\([^"]*\)".*/\1/' | tr 'A-F' 'a-f')"
    have="$(cut -f1 "$marker" | tr 'A-F' 'a-f')"
    [ "$want" = "$have" ] || exit 1
    ;;
esac
exit 0
STUB

# The publisher is reached through `cargo run --manifest-path tools/r2-publisher/...`.
cat > "$stub/cargo" <<'STUB'
#!/bin/sh
printf '%s\n' "$*" >> "$SELFTEST_PUBLISH_LOG"
exit 0
STUB

chmod +x "$stub/security" "$stub/codesign" "$stub/cargo"

# Records only the explicitly named inputs the packaging call is expected to carry -- never the
# caller's whole environment -- then fabricates the three artifacts and their signature marker.
cat > "$scaffold/scripts/bundle.sh" <<'STUB'
#!/bin/sh
set -eu
{
  printf 'SIGN_IDENTITY=%s\n' "${SIGN_IDENTITY:-}"
  printf 'RELEASE_SIGNING=%s\n' "${RELEASE_SIGNING:-}"
  printf 'APP_BASENAME=%s\n' "${APP_BASENAME:-}"
  printf 'DEV_BUILD_PROFILE=%s\n' "${DEV_BUILD_PROFILE:-}"
  printf 'REQUIRE_SPARKLE_UPDATE_SIGNING=%s\n' "${REQUIRE_SPARKLE_UPDATE_SIGNING:-}"
} >> "$SELFTEST_BUNDLE_LOG"
[ "${SELFTEST_BUILD_FAILS:-0}" = "0" ] || { echo "selftest packaging stub: failing on purpose" >&2; exit 7; }
app="dist/${APP_BASENAME}.app"
mkdir -p "$app/Contents/MacOS"
printf '%s\t%s\n' "${SELFTEST_SIGNED_HASH:-}" "${SELFTEST_SIGNED_NAME:-}" > "$app/Contents/.selftest-identity"
printf '%s' "${SELFTEST_INTEGRITY:-1}" > "$app/Contents/.selftest-integrity"
printf '%s\n' \
  '<?xml version="1.0" encoding="UTF-8"?>' \
  '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
  '<plist version="1.0"><dict>' \
  '  <key>CFBundleIdentifier</key><string>com.eacryo.selftest</string>' \
  '  <key>CFBundleExecutable</key><string>selftest</string>' \
  '  <key>CFBundleName</key><string>selftest</string>' \
  '  <key>CFBundleShortVersionString</key><string>9.9.9</string>' \
  '  <key>CFBundleVersion</key><string>999</string>' \
  '</dict></plist>' > "$app/Contents/Info.plist"
: > "dist/${APP_BASENAME}.zip"
: > "dist/${APP_BASENAME}.dmg"
STUB
printf '#!/bin/sh\nexit 0\n' > "$scaffold/scripts/generate-appcast.sh"

# --- harness -------------------------------------------------------------------------------------

failures=0
checks=0

check() { # check <label> <condition-command...>
  label="$1"
  shift
  checks=$((checks + 1))
  if "$@"; then
    printf '  ok   %s\n' "$label"
  else
    printf '  FAIL %s\n' "$label"
    failures=$((failures + 1))
  fi
}

contains() { grep -qF -- "$2" "$1"; }
lacks() { ! grep -qF -- "$2" "$1"; }

# run_case <build-fails> <signed-hash> <signed-name> <integrity> <args...>
# Leaves $status, $output, $bundle_log and $publish_log for the assertions.
run_case() {
  SELFTEST_BUILD_FAILS="$1"
  SELFTEST_SIGNED_HASH="$2"
  SELFTEST_SIGNED_NAME="$3"
  SELFTEST_INTEGRITY="$4"
  shift 4
  : > "$bundle_log"
  : > "$publish_log"
  env CODESIGN_IDENTITY="$CASE_CODESIGN_IDENTITY" SIGN_IDENTITY="$CASE_SIGN_IDENTITY" \
    PATH="$stub:$PATH" \
    SELFTEST_IDENTITIES="$identities" \
    SELFTEST_BUNDLE_LOG="$bundle_log" \
    SELFTEST_PUBLISH_LOG="$publish_log" \
    SELFTEST_BUILD_FAILS="$SELFTEST_BUILD_FAILS" \
    SELFTEST_SIGNED_HASH="$SELFTEST_SIGNED_HASH" \
    SELFTEST_SIGNED_NAME="$SELFTEST_SIGNED_NAME" \
    SELFTEST_INTEGRITY="$SELFTEST_INTEGRITY" \
    sh "$scaffold/scripts/release-dev.sh" "$@" > "$output" 2>&1
  status=$?
  CASE_CODESIGN_IDENTITY=""
  CASE_SIGN_IDENTITY=""
}

write_identities() { # write_identities <hash> <name> [<hash> <name> ...]
  : > "$identities"
  while [ $# -ge 2 ]; do
    printf '%s\t%s\n' "$1" "$2" >> "$identities"
    shift 2
  done
}

# CASE_CODESIGN_IDENTITY / CASE_SIGN_IDENTITY are the explicit environment sources of the next case;
# the runner clears them, so every other case runs with both unset.
CASE_CODESIGN_IDENTITY=""
CASE_SIGN_IDENTITY=""

signed_hash=0123456789abcdef0123456789abcdef01234567
signed_name="Developer ID Application: Selftest (CONTROL00001)"
other_hash=fedcba9876543210fedcba9876543210fedcba98
other_name="oh-my-tab-sign"

echo "release-dev.sh signing identity:"

# 1. The default resolves the only Developer ID identity and passes its SHA-1 (not the name) into the
#    packaging call, with the release branch explicitly off.
write_identities "$signed_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1
check "default resolves the Developer ID identity" test "$status" -eq 0
check "default prints the resolved identity" contains "$output" "signing identity: $signed_name ($signed_hash)"
check "default passes SIGN_IDENTITY as the SHA-1" contains "$bundle_log" "SIGN_IDENTITY=$signed_hash"
check "default forces RELEASE_SIGNING=0" contains "$bundle_log" "RELEASE_SIGNING=0"
check "default does not publish without --push" test ! -s "$publish_log"

# 2. --self-signed resolves the repository certificate by name.
write_identities "$signed_hash" "$signed_name" "$other_hash" "$other_name"
run_case 0 "$other_hash" "$other_name" 1 --self-signed
check "--self-signed resolves oh-my-tab-sign" test "$status" -eq 0
check "--self-signed passes its SHA-1" contains "$bundle_log" "SIGN_IDENTITY=$other_hash"

# 3. --identity accepts a SHA-1 and an exact name, and both land in the packaging call.
write_identities "$signed_hash" "$signed_name" "$other_hash" "$other_name"
run_case 0 "$other_hash" "$other_name" 1 --identity "$other_hash"
check "--identity <sha1> resolves" test "$status" -eq 0
check "--identity <sha1> passes it" contains "$bundle_log" "SIGN_IDENTITY=$other_hash"
run_case 0 "$signed_hash" "$signed_name" 1 --identity "$signed_name"
check "--identity <name> resolves" test "$status" -eq 0
check "--identity <name> passes the SHA-1" contains "$bundle_log" "SIGN_IDENTITY=$signed_hash"

# 4. --print-identity resolves and stops before building.
write_identities "$signed_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1 --print-identity
check "--print-identity exits 0" test "$status" -eq 0
check "--print-identity prints the identity" contains "$output" "signing identity: $signed_name ($signed_hash)"
check "--print-identity does not build" test ! -s "$bundle_log"

# 5. An artifact carrying a different certificate is refused, and the publish step is never reached.
write_identities "$signed_hash" "$signed_name"
run_case 0 "$other_hash" "$other_name" 1 --push --dry-run
check "wrong certificate fails the release" test "$status" -ne 0
check "wrong certificate names the mismatch" contains "$output" "is not signed by $signed_name ($signed_hash)"
check "wrong certificate does not publish" test ! -s "$publish_log"

# 6. A tampered artifact fails the integrity check.
write_identities "$signed_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 0 --push --dry-run
check "tampered artifact fails verification" test "$status" -ne 0
check "tampered artifact says verification failed" contains "$output" "failed code-signature verification"
check "tampered artifact does not publish" test ! -s "$publish_log"

# 7. A failing packaging step stops the release before any gate or publish.
write_identities "$signed_hash" "$signed_name"
run_case 1 "$signed_hash" "$signed_name" 1 --push --dry-run
check "failing packaging exits non-zero" test "$status" -ne 0
check "failing packaging does not publish" test ! -s "$publish_log"

# 8. The happy path with --push --dry-run does reach the publisher (the counter-example to 5-7).
write_identities "$signed_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1 --push --dry-run
check "verified artifact reaches the publisher" test "$status" -eq 0
check "verified artifact calls the publisher" test -s "$publish_log"
check "verified artifact reports the signature" contains "$output" "signature verified: $signed_name ($signed_hash)"

# 9. No identity at all.
: > "$identities"
run_case 0 "$signed_hash" "$signed_name" 1
check "no identity fails" test "$status" -ne 0
check "no identity offers --self-signed" contains "$output" "--self-signed"
check "no identity does not build" test ! -s "$bundle_log"

# 10. --self-signed together with --identity.
write_identities "$signed_hash" "$signed_name" "$other_hash" "$other_name"
run_case 0 "$other_hash" "$other_name" 1 --self-signed --identity "$other_hash"
check "conflicting flags fail" test "$status" -ne 0
check "conflicting flags explain themselves" contains "$output" "mutually exclusive"

# 11. A missing or empty --identity value.
write_identities "$signed_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1 --identity
check "--identity without a value fails" test "$status" -ne 0
check "--identity without a value explains itself" contains "$output" "needs a name or SHA-1"
run_case 0 "$signed_hash" "$signed_name" 1 --identity=
check "--identity= fails" test "$status" -ne 0

# 12. Ad-hoc signing is refused with its own reasoning.
write_identities "$signed_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1 --identity -
check "ad-hoc is refused" test "$status" -ne 0
check "ad-hoc explains itself" contains "$output" "Ad-hoc"

# 13. Ambiguity: two Developer ID identities, and a name that belongs to two certificates.
write_identities "$signed_hash" "$signed_name" "$other_hash" "Developer ID Application: Other (CONTROL00002)"
run_case 0 "$signed_hash" "$signed_name" 1
check "two Developer ID identities fail the default" test "$status" -ne 0
check "two Developer ID identities ask for --identity" contains "$output" "must name one"
write_identities "$signed_hash" "$signed_name" "$other_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1 --identity "$signed_name"
check "one name with two certificates fails" test "$status" -ne 0
check "one name with two certificates asks for the SHA-1" contains "$output" "different SHA-1s"
write_identities "$signed_hash" "$signed_name" "$signed_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1 --identity "$signed_name"
check "a duplicated record with the same SHA-1 is resolved" test "$status" -eq 0

# 14. An unknown identity names the available ones.
write_identities "$signed_hash" "$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1 --identity nope
check "unknown identity fails" test "$status" -ne 0
check "unknown identity lists the available ones" contains "$output" "$signed_name"

# 15. Different representations of one certificate agree; different certificates do not.
write_identities "$signed_hash" "$signed_name" "$other_hash" "$other_name"
CASE_CODESIGN_IDENTITY="$signed_name"
run_case 0 "$signed_hash" "$signed_name" 1 --identity "$signed_hash"
check "a name and the SHA-1 of one certificate do not conflict" test "$status" -eq 0
check "the canonical SHA-1 is what gets passed on" contains "$bundle_log" "SIGN_IDENTITY=$signed_hash"

CASE_SIGN_IDENTITY="$(printf '%s' "$signed_hash" | tr 'a-f' 'A-F')"
run_case 0 "$signed_hash" "$signed_name" 1 --identity "$signed_hash"
check "an upper-case SHA-1 resolves" test "$status" -eq 0
check "an upper-case SHA-1 is canonicalised" contains "$bundle_log" "SIGN_IDENTITY=$signed_hash"

CASE_CODESIGN_IDENTITY="$other_name"
run_case 0 "$signed_hash" "$signed_name" 1 --identity "$signed_hash"
check "two different certificates still conflict" test "$status" -ne 0
check "the conflict names the resolved certificates" contains "$output" "Conflicting signing identities"

CASE_CODESIGN_IDENTITY="$signed_name"
run_case 0 "$other_hash" "$other_name" 1 --self-signed
check "--self-signed conflicts with a different CODESIGN_IDENTITY" test "$status" -ne 0

CASE_CODESIGN_IDENTITY="$other_name"
run_case 0 "$other_hash" "$other_name" 1 --self-signed
check "--self-signed accepts CODESIGN_IDENTITY naming the same certificate" test "$status" -eq 0
check "--self-signed still passes its SHA-1" contains "$bundle_log" "SIGN_IDENTITY=$other_hash"

echo
if [ "$failures" -eq 0 ]; then
  echo "release-dev.sh signing identity selftest: $checks checks passed"
  exit 0
fi
echo "release-dev.sh signing identity selftest: $failures of $checks checks FAILED" >&2
exit 1
