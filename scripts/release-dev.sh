#!/bin/sh
# Build the development update channel. By default this only creates local artifacts;
# pass --push to publish them to the dev R2 prefix and appcast.
#
# Signing identity: the development channel must carry the *same* identity as the bundle
# scripts/dev-restart.sh builds, or every publish flips the identity of the installed app and macOS
# asks for Accessibility and keychain access again (TCC grants and the keychain ACL are judged against
# the signing identity -- measured 2026-10-09). This script therefore resolves the identity itself,
# defaults to the Apple-issued Developer ID Application certificate, and passes it to bundle.sh
# explicitly; bundle.sh's own default stays untouched, so release.sh and direct callers are unaffected.
#
# Timestamp: with a Developer ID identity codesign contacts Apple's timestamp service by default
# (measured); the self-signed identity is not timestamped. Deliberately no explicit --timestamp:
# forcing it on would break the self-signed path (Apple's TSA does not serve non-Apple identities) and
# forcing it off would drop the Developer ID signature's protection. Publishing needs the network; a
# failure aborts and a re-run is cheap, while dev-restart.sh keeps --timestamp=none for the offline
# inner loop.
set -eu

PUSH_R2=0
DRY_RUN=0
WANT_SELF_SIGNED=0
PRINT_IDENTITY=0
IDENTITY_FLAG=""

usage() {
  cat <<'EOF'
Usage: sh scripts/release-dev.sh [--push] [--dry-run] [--self-signed | --identity <name|sha1>] [--print-identity]
  (no flag)         build the dev package locally; never contacts R2
  --push            upload the dev ZIP, DMG, then dev appcast to R2
  --dry-run         print the R2 upload plan without uploading
  --self-signed     sign with the repository's self-signed 'oh-my-tab-sign' certificate instead of
                    the Developer ID one (for machines without the Apple certificate)
  --identity <id>   sign with that identity: its SHA-1, or its name exactly as
                    `security find-identity -v -p codesigning` prints it
  --print-identity  resolve and print the signing identity, then exit without building
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --push) PUSH_R2=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --self-signed) WANT_SELF_SIGNED=1 ;;
    --identity=*)
      IDENTITY_FLAG="${1#--identity=}"
      [ -n "$IDENTITY_FLAG" ] || { echo "❌ --identity needs a name or SHA-1" >&2; usage >&2; exit 2; }
      ;;
    --identity)
      [ $# -ge 2 ] || { echo "❌ --identity needs a name or SHA-1" >&2; usage >&2; exit 2; }
      IDENTITY_FLAG="$2"
      [ -n "$IDENTITY_FLAG" ] || { echo "❌ --identity needs a name or SHA-1" >&2; usage >&2; exit 2; }
      shift
      ;;
    --print-identity) PRINT_IDENTITY=1 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "❌ Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

cd "$(dirname "$0")/.."

if [ "$WANT_SELF_SIGNED" -eq 1 ] && [ -n "$IDENTITY_FLAG" ]; then
  echo "❌ --self-signed and --identity are mutually exclusive; pass one of them." >&2
  usage >&2
  exit 2
fi

# Every valid code-signing identity, one "<sha1>\t<name>" per line.
# `security` is called by name rather than by absolute path so the offline selftest can put a stub
# ahead of it on PATH (the same stub pattern codex-review-selftest.sh uses); everything else here is
# called by absolute path.
list_identities() {
  security find-identity -v -p codesigning 2>/dev/null | awk -F'"' '
    NF >= 2 {
      split($1, head, ")")
      hash = head[2]
      gsub(/[[:space:]]/, "", hash)
      if (hash ~ /^[0-9A-Fa-f]{40}$/) print hash "\t" $2
    }'
}

# Resolve "$1" (empty = discover the Developer ID identity) to "<sha1>\t<name>", printing the reason
# and exiting non-zero when the request is ambiguous or matches nothing.
resolve_identity() {
  requested="$1"
  if [ "$requested" = "-" ]; then
    echo "❌ Ad-hoc signing ('-') is not accepted for the development channel: it has no stable" >&2
    echo "   identity, so every rebuild loses the TCC grants and stops matching the keychain items it" >&2
    echo "   created. Pass --identity <name|sha1>, or --self-signed." >&2
    exit 2
  fi
  identities="$(list_identities)"
  if [ -z "$identities" ]; then
    echo "❌ No valid code-signing identity is installed." >&2
    echo "   Install the Apple-issued Developer ID Application certificate, pass --identity <name|sha1>," >&2
    echo "   or pass --self-signed to use the repository's 'oh-my-tab-sign' certificate." >&2
    exit 1
  fi

  if [ -n "$requested" ]; then
    hex_only=1
    case "$requested" in
      *[!0-9A-Fa-f]*) hex_only=0 ;;
    esac
    if [ "$hex_only" -eq 1 ] && [ "${#requested}" -eq 40 ]; then
      kind="SHA-1"
      matches="$(printf '%s\n' "$identities" \
        | awk -F'\t' -v want="$requested" 'tolower($1) == tolower(want)')"
    else
      kind="name"
      matches="$(printf '%s\n' "$identities" | awk -F'\t' -v want="$requested" '$2 == want')"
    fi
    if [ -z "$matches" ]; then
      echo "❌ No code-signing identity matches the $kind '$requested'." >&2
      echo "   Available identities:" >&2
      printf '%s\n' "$identities" | awk -F'\t' '{ printf "     %s  %s\n", $1, $2 }' >&2
      exit 1
    fi
    # The same name can belong to two certificates after a renewal; a publish must name the exact one
    # by its SHA-1 rather than let codesign pick.
    hash_count="$(printf '%s\n' "$matches" | cut -f1 | sort -u | wc -l | tr -d ' ')"
    if [ "$hash_count" -gt 1 ]; then
      echo "❌ '$requested' matches several certificates with the same name but different SHA-1s:" >&2
      printf '%s\n' "$matches" | awk -F'\t' '{ printf "     %s  %s\n", $1, $2 }' >&2
      echo "   Pass one of those SHA-1s to --identity." >&2
      exit 1
    fi
    printf '%s\n' "$matches" | head -1
    return 0
  fi

  developer_ids="$(printf '%s\n' "$identities" | awk -F'\t' '$2 ~ /^Developer ID Application:/' | sort -u)"
  count="$(printf '%s\n' "$developer_ids" | sed '/^[[:space:]]*$/d' | wc -l | tr -d ' ')"
  if [ "$count" -eq 0 ]; then
    echo "❌ No Apple-issued Developer ID Application identity is installed." >&2
    echo "   Pass --identity <name|sha1>, pass --self-signed for the repository's own certificate," >&2
    echo "   or install the certificate (see docs/releasing-en.md)." >&2
    exit 1
  fi
  if [ "$count" -gt 1 ]; then
    echo "❌ Several Developer ID Application identities are installed, and a publish must name one:" >&2
    printf '%s\n' "$developer_ids" | awk -F'\t' '{ printf "     %s  %s\n", $1, $2 }' >&2
    echo "   Pass one of them to --identity." >&2
    exit 1
  fi
  printf '%s\n' "$developer_ids"
}

# Explicit identity sources must agree; stop instead of picking one when they do not. Each source is
# resolved to its certificate fingerprint *before* the comparison, so a name and the SHA-1 of the same
# certificate are not mistaken for a conflict.
candidates=""
if [ -n "$IDENTITY_FLAG" ]; then candidates="$candidates
$IDENTITY_FLAG"; fi
if [ "$WANT_SELF_SIGNED" -eq 1 ]; then candidates="$candidates
oh-my-tab-sign"; fi
if [ -n "${CODESIGN_IDENTITY:-}" ]; then candidates="$candidates
$CODESIGN_IDENTITY"; fi
if [ -n "${SIGN_IDENTITY:-}" ]; then candidates="$candidates
$SIGN_IDENTITY"; fi
sources="$(printf '%s\n' "$candidates" | sed '/^[[:space:]]*$/d')"
resolved_sources=""
old_ifs="$IFS"
IFS='
'
for source in $sources; do
  resolved_sources="$resolved_sources
$(resolve_identity "$source")"
done
IFS="$old_ifs"
fingerprints="$(printf '%s\n' "$resolved_sources" | sed '/^[[:space:]]*$/d' | cut -f1 | tr 'A-F' 'a-f' | sort -u)"
distinct_count="$(printf '%s\n' "$fingerprints" | sed '/^[[:space:]]*$/d' | wc -l | tr -d ' ')"
if [ "$distinct_count" -gt 1 ]; then
  echo "❌ Conflicting signing identities were requested:" >&2
  printf '%s\n' "$resolved_sources" | sed '/^[[:space:]]*$/d' \
    | awk -F'\t' '{ printf "     %s  %s\n", $1, $2 }' >&2
  echo "   Pass exactly one of --identity / --self-signed, or unset CODESIGN_IDENTITY / SIGN_IDENTITY." >&2
  exit 2
fi
if [ "$distinct_count" -eq 1 ]; then
  identity_line="$(printf '%s\n' "$resolved_sources" | sed '/^[[:space:]]*$/d' | head -1)"
else
  identity_line="$(resolve_identity "")"
fi
sign_hash="$(printf '%s\n' "$identity_line" | cut -f1)"
sign_name="$(printf '%s\n' "$identity_line" | cut -f2)"
echo "signing identity: $sign_name ($sign_hash)"
if [ "$PRINT_IDENTITY" -eq 1 ]; then
  exit 0
fi

# Keep the development channel isolated from production at every level: bundle identity,
# Sparkle feed, R2 prefix, appcast key, and archive basename are all distinct.
APP_BASENAME="${APP_BASENAME:-Oh-My-Tab-Dev}"
BUNDLE_ID="${BUNDLE_ID:-com.eacryo.oh-my-tab.dev}"
BUNDLE_NAME="${BUNDLE_NAME:-Oh-My-Tab Dev}"
CARGO_BUILD_FEATURES="dev-long-text"
RELEASE_DOC_DIR="release_doc_dev"
SPARKLE_FEED_URL="${SPARKLE_FEED_URL:-https://download.oh-my-tab.app/dev_release/appcast.xml}"
R2_RELEASE_PREFIX="${R2_RELEASE_PREFIX:-dev_release}"
R2_APPCAST_KEY="${R2_APPCAST_KEY:-dev_release/appcast.xml}"
R2_ARTIFACT_BASENAME="${R2_ARTIFACT_BASENAME:-$APP_BASENAME}"
R2_APPCAST_PATH="${R2_APPCAST_PATH:-dist/appcast-dev.xml}"
REQUIRE_SPARKLE_UPDATE_SIGNING=0
if [ "$PUSH_R2" -eq 1 ]; then
  REQUIRE_SPARKLE_UPDATE_SIGNING=1
fi

APP="dist/${APP_BASENAME}.app"
ZIP="dist/${APP_BASENAME}.zip"
DMG="dist/${APP_BASENAME}.dmg"

APP_BASENAME="$APP_BASENAME" \
BUNDLE_ID="$BUNDLE_ID" \
BUNDLE_NAME="$BUNDLE_NAME" \
CARGO_BUILD_FEATURES="$CARGO_BUILD_FEATURES" \
DEV_BUILD_PROFILE="release-dev" \
RELEASE_DOC_DIR="$RELEASE_DOC_DIR" \
SPARKLE_FEED_URL="$SPARKLE_FEED_URL" \
SPARKLE_PUBLIC_ED_KEY="${SPARKLE_PUBLIC_ED_KEY:-}" \
REQUIRE_SPARKLE_UPDATE_SIGNING="$REQUIRE_SPARKLE_UPDATE_SIGNING" \
RELEASE_SIGNING=0 \
SIGN_IDENTITY="$sign_hash" \
sh scripts/bundle.sh

if [ ! -f "$APP/Contents/Info.plist" ] || [ ! -f "$ZIP" ] || [ ! -f "$DMG" ]; then
  echo "❌ Build failed: expected $APP, $ZIP, and $DMG" >&2
  exit 1
fi

# The artifact must really carry the identity resolved here: a silent identity change is the defect
# this script exists to prevent, because TCC grants and the keychain ACL are judged against it.
# `codesign` is called by name (as bundle.sh does), so the offline selftest can put tool stubs ahead
# of it on PATH; `security` is called the same way.
if ! codesign --verify --deep --strict "$APP" >/dev/null 2>&1; then
  echo "❌ $APP failed code-signature verification" >&2
  exit 1
fi
if ! codesign --verify --strict -R="certificate leaf = H\"$sign_hash\"" "$APP" >/dev/null 2>&1; then
  actual="$(codesign -d --verbose=4 "$APP" 2>&1 | awk -F= '$1 == "Authority" { print $2; exit }')"
  echo "❌ $APP is not signed by $sign_name ($sign_hash); its leaf authority is '${actual:-unknown}'" >&2
  exit 1
fi
echo "✅ signature verified: $sign_name ($sign_hash)"

VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")
BUILD_VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist")
RELEASE_NOTES="$RELEASE_DOC_DIR/${VERSION}.md"
echo "✅ Dev build ready: $APP (version=$VERSION, build=$BUILD_VERSION)"

if [ "$PUSH_R2" -eq 1 ]; then
  if [ "$DRY_RUN" -eq 0 ]; then
    # Generate/update the feed before publishing. The helper reuses a local feed or fetches the
    # public feed on a clean checkout, then generates URLs matching the R2 object names.
    R2_RELEASE_PREFIX="$R2_RELEASE_PREFIX" \
    R2_ARTIFACT_BASENAME="$R2_ARTIFACT_BASENAME" \
    R2_PUBLIC_BASE_URL="${R2_PUBLIC_BASE_URL:-https://download.oh-my-tab.app}" \
    SPARKLE_FEED_URL="$SPARKLE_FEED_URL" \
    bash scripts/generate-appcast.sh "$R2_APPCAST_PATH" "$ZIP" "$VERSION" "$BUILD_VERSION" "$RELEASE_NOTES"
  fi
  PUBLISH_ARGS="--appcast $R2_APPCAST_PATH --zip $ZIP --dmg $DMG --version $VERSION --build-version $BUILD_VERSION"
  if [ "$DRY_RUN" -eq 1 ]; then
    PUBLISH_ARGS="$PUBLISH_ARGS --dry-run"
  fi
  R2_RELEASE_PREFIX="$R2_RELEASE_PREFIX" \
  R2_APPCAST_KEY="$R2_APPCAST_KEY" \
  R2_ARTIFACT_BASENAME="$R2_ARTIFACT_BASENAME" \
  cargo run --manifest-path tools/r2-publisher/Cargo.toml --release -- $PUBLISH_ARGS
elif [ "$DRY_RUN" -eq 1 ]; then
  echo "ℹ️  --dry-run was provided without --push; no R2 action was needed."
else
  echo "ℹ️  Dev R2 upload skipped (pass --push to upload)."
fi
