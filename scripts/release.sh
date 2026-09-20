#!/bin/bash
# Production release flow: build and submit for notarization, check its status, then publish.
# 生产发布分为三步：构建并提交公证、查询公证状态、公证通过后推送。
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/release.sh [--notarize | --check [submission-id] | --push [--dry-run]]

  (no flag)       Build local artifacts and generate the Homebrew cask; never upload to R2.
  --notarize      Build with Developer ID signing and submit to Apple without waiting.
  --check [id]    Query the saved notarization submission (or recover with its submission ID).
  --push          Require Accepted status, staple the app, package it, and publish to R2.
  --dry-run       With --push, prepare the release and print the R2 upload plan without uploading.

Set CODESIGN_IDENTITY to a Developer ID Application identity for --notarize.
Set NOTARY_PROFILE to the notarytool Keychain profile (default: oh-my-tab-notary).
EOF
}

MODE="build"
DRY_RUN=0
CHECK_ID=""
MODE_SELECTED=0

select_mode() {
  if [ "$MODE_SELECTED" -ne 0 ]; then
    echo "error: choose only one of --notarize, --check, or --push" >&2
    exit 2
  fi
  MODE="$1"
  MODE_SELECTED=1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --notarize)
      select_mode notarize
      ;;
    --check)
      select_mode check
      if [ "$#" -gt 1 ]; then
        case "$2" in
          -* ) ;;
          * ) CHECK_ID="$2"; shift ;;
        esac
      fi
      ;;
    --push)
      select_mode push
      ;;
    --dry-run)
      DRY_RUN=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [ "$DRY_RUN" -eq 1 ] && [ "$MODE" != "push" ]; then
  echo "error: --dry-run can only be used with --push" >&2
  exit 2
fi
if [ -n "$CHECK_ID" ] && [ "$MODE" != "check" ]; then
  echo "error: a submission ID can only follow --check" >&2
  exit 2
fi

# The script lives in scripts/; run all project commands from the repository root.
cd "$(dirname "$0")/.."

APP="dist/Oh-My-Tab.app"
DMG="dist/Oh-My-Tab.dmg"
ZIP="dist/Oh-My-Tab.zip"
OUT="dist/oh-my-tab.rb"
NOTARY_ROOT="dist/.notarization"
PENDING_DIR="$NOTARY_ROOT/pending"
STAGED_APP="$PENDING_DIR/Oh-My-Tab.app"
NOTARY_ZIP="$PENDING_DIR/Oh-My-Tab-notarization.zip"
SUBMISSION_FILE="$PENDING_DIR/submission-id"
NOTARY_PROFILE="${NOTARY_PROFILE:-oh-my-tab-notary}"
APPCAST_PATH="${R2_APPCAST_PATH:-dist/appcast.xml}"

validate_submission_id() {
  case "$1" in
    ""|*[!0123456789abcdefABCDEF-]*)
      echo "error: invalid notarization submission ID: $1" >&2
      exit 2
      ;;
  esac
}

load_submission_id() {
  local saved_id=""
  if [ -s "$SUBMISSION_FILE" ]; then
    saved_id="$(tr -d '\r\n' < "$SUBMISSION_FILE")"
  fi

  if [ -n "$CHECK_ID" ]; then
    validate_submission_id "$CHECK_ID"
    if [ -n "$saved_id" ] && [ "$saved_id" != "$CHECK_ID" ]; then
      echo "error: $CHECK_ID does not match the pending submission $saved_id" >&2
      exit 2
    fi
    saved_id="$CHECK_ID"
    printf '%s\n' "$saved_id" > "$SUBMISSION_FILE"
  fi

  if [ -z "$saved_id" ]; then
    echo "error: no notarization submission ID found in $PENDING_DIR" >&2
    echo "       Use --check <submission-id> if Apple accepted the submission but the ID was not saved." >&2
    exit 1
  fi
  validate_submission_id "$saved_id"
  printf '%s\n' "$saved_id"
}

query_notary_status() {
  local submission_id="$1"
  local status_file="$PENDING_DIR/status.plist"
  xcrun notarytool info "$submission_id" \
    --keychain-profile "$NOTARY_PROFILE" \
    --output-format plist > "$status_file"
  /usr/libexec/PlistBuddy -c 'Print :status' "$status_file"
}

show_notary_log() {
  local submission_id="$1"
  local log_file="$PENDING_DIR/notary-log.json"
  if xcrun notarytool log "$submission_id" \
    --keychain-profile "$NOTARY_PROFILE" > "$log_file"; then
    echo "Notarization log saved to $log_file"
  else
    echo "warning: could not fetch the notarization log" >&2
  fi
}

require_accepted_submission() {
  local submission_id="$1"
  local status=""
  status="$(query_notary_status "$submission_id")"
  echo "Notarization status: $status"
  case "$status" in
    Accepted)
      ;;
    "In Progress"|Submitted|"Waiting for Export Compliance")
      echo "Notarization is not complete. Check again later with: scripts/release.sh --check" >&2
      exit 3
      ;;
    *)
      show_notary_log "$submission_id"
      echo "error: notarization status is '$status'; release will not be pushed" >&2
      exit 1
      ;;
  esac
}

generate_cask() {
  local version="$1"
  local dmg_path="$2"
  local sha=""
  sha="$(shasum -a 256 "$dmg_path" | awk '{print $1}')"

  cat > "$OUT" <<EOF
cask "oh-my-tab" do
  depends_on macos: :ventura
  depends_on arch: :arm64
  version "$version"
  sha256 "$sha"
  url "https://github.com/eacryo/oh-my-tab/releases/download/v#{version}/Oh-My-Tab.dmg"
  name "Oh-My-Tab"
  desc "macOS window switcher (Cmd+Tab alternative)"
  homepage "https://github.com/eacryo/oh-my-tab"
  app "Oh-My-Tab.app"

  zap trash: [
    "~/Library/Caches/oh-my-tab-icons",
    "~/Library/Logs/oh-my-tab",
    "~/.config/oh-my-tab",
  ]
end
EOF
  echo "✅ Generated $OUT (version=$version, sha256=$sha)"
}

case "$MODE" in
  build)
    RELEASE_DOC_DIR="release_doc" bash scripts/bundle.sh
    if [ ! -f "$DMG" ] || [ ! -f "$ZIP" ]; then
      echo "error: build did not produce $DMG and $ZIP" >&2
      exit 1
    fi
    VERSION="$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)"
    generate_cask "$VERSION" "$DMG"
    echo "Copy $OUT to your Homebrew tap's Casks/ directory when ready."
    echo "ℹ️  R2 upload skipped; use --notarize, then --check, then --push for production."
    ;;

  notarize)
    if [ -z "${CODESIGN_IDENTITY:-}" ]; then
      echo "error: set CODESIGN_IDENTITY to a Developer ID Application identity" >&2
      exit 1
    fi
    if [ -e "$PENDING_DIR" ]; then
      echo "error: a notarization is already pending at $PENDING_DIR" >&2
      echo "       Check it with scripts/release.sh --check before starting another release." >&2
      exit 1
    fi

    RELEASE_SIGNING=1 RELEASE_DOC_DIR="release_doc" bash scripts/bundle.sh
    if [ ! -d "$APP" ]; then
      echo "error: build did not produce $APP" >&2
      exit 1
    fi

    mkdir -p "$NOTARY_ROOT"
    mkdir "$PENDING_DIR"
    ditto "$APP" "$STAGED_APP"
    ditto -c -k --keepParent "$STAGED_APP" "$NOTARY_ZIP"

    echo "Submitting $NOTARY_ZIP for notarization (this command does not wait for Apple)."
    if ! xcrun notarytool submit "$NOTARY_ZIP" \
      --keychain-profile "$NOTARY_PROFILE" \
      --output-format plist > "$PENDING_DIR/submission.plist"; then
      echo "error: notarization submission failed; staged files were kept in $PENDING_DIR" >&2
      exit 1
    fi
    SUBMISSION_ID="$(/usr/libexec/PlistBuddy -c 'Print :id' "$PENDING_DIR/submission.plist")"
    validate_submission_id "$SUBMISSION_ID"
    printf '%s\n' "$SUBMISSION_ID" > "$SUBMISSION_FILE"
    echo "✅ Submitted to Apple: $SUBMISSION_ID"
    echo "Check later with: scripts/release.sh --check"
    ;;

  check)
    if [ ! -d "$PENDING_DIR" ] || [ ! -d "$STAGED_APP" ]; then
      echo "error: no staged production release found at $PENDING_DIR" >&2
      echo "       Start one with scripts/release.sh --notarize." >&2
      exit 1
    fi
    SUBMISSION_ID="$(load_submission_id)"
    STATUS="$(query_notary_status "$SUBMISSION_ID")"
    echo "Notarization status: $STATUS (submission $SUBMISSION_ID)"
    case "$STATUS" in
      Accepted)
        echo "✅ Apple accepted the app. Nothing was pushed. You can now run: scripts/release.sh --push"
        ;;
      "In Progress"|Submitted|"Waiting for Export Compliance")
        echo "Not finished yet; check again later with: scripts/release.sh --check"
        ;;
      *)
        show_notary_log "$SUBMISSION_ID"
        echo "error: notarization status is '$STATUS'" >&2
        exit 1
        ;;
    esac
    ;;

  push)
    if [ ! -d "$PENDING_DIR" ] || [ ! -d "$STAGED_APP" ]; then
      echo "error: no staged notarized release found at $PENDING_DIR" >&2
      echo "       Start and submit one with scripts/release.sh --notarize." >&2
      exit 1
    fi
    SUBMISSION_ID="$(load_submission_id)"
    require_accepted_submission "$SUBMISSION_ID"

    # Staple the accepted ticket before creating the ZIP and DMG that will be published.
    if codesign --verify --deep --strict "$STAGED_APP"; then
      if xcrun stapler validate "$STAGED_APP" >/dev/null 2>&1; then
        echo "The notarization ticket is already stapled."
      else
        xcrun stapler staple "$STAGED_APP"
        xcrun stapler validate "$STAGED_APP"
      fi
    else
      echo "error: staged app signature verification failed" >&2
      exit 1
    fi

    VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$STAGED_APP/Contents/Info.plist")"
    BUILD_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$STAGED_APP/Contents/Info.plist")"
    RELEASE_NOTES="release_doc/${VERSION}.md"
    if [ ! -s "$RELEASE_NOTES" ]; then
      echo "error: release notes not found for version $VERSION: $RELEASE_NOTES" >&2
      exit 1
    fi

    rm -rf "$APP"
    ditto "$STAGED_APP" "$APP"
    rm -f "$ZIP" "$DMG"
    ditto -c -k --keepParent "$APP" "$ZIP"
    STAGING="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-tab-release.XXXXXX")"
    cleanup() {
      local code=$?
      if [ -n "${STAGING:-}" ] && [ -d "$STAGING" ]; then
        rm -rf "$STAGING"
      fi
      if [ "$code" -ne 0 ]; then
        echo "error: release packaging or publishing failed; pending notarization state remains at $PENDING_DIR" >&2
      fi
      exit "$code"
    }
    trap cleanup EXIT
    ditto "$APP" "$STAGING/Oh-My-Tab.app"
    ln -s /Applications "$STAGING/Applications"
    hdiutil create -volname "Oh-My-Tab" -srcfolder "$STAGING" -ov -format UDZO "$DMG" >/dev/null
    rm -rf "$STAGING"
    STAGING=""

    R2_RELEASE_PREFIX="${R2_RELEASE_PREFIX:-releases}" \
    R2_ARTIFACT_BASENAME="${R2_ARTIFACT_BASENAME:-Oh-My-Tab}" \
    R2_PUBLIC_BASE_URL="${R2_PUBLIC_BASE_URL:-https://download.oh-my-tab.app}" \
    SPARKLE_FEED_URL="${SPARKLE_FEED_URL:-https://download.oh-my-tab.app/appcast.xml}" \
      bash scripts/generate-appcast.sh "$APPCAST_PATH" "$ZIP" "$VERSION" "$BUILD_VERSION" "$RELEASE_NOTES"
    generate_cask "$VERSION" "$DMG"

    PUBLISH_ARGS=(--appcast "$APPCAST_PATH" --zip "$ZIP" --dmg "$DMG" \
      --version "$VERSION" --build-version "$BUILD_VERSION")
    if [ "$DRY_RUN" -eq 1 ]; then
      PUBLISH_ARGS+=(--dry-run)
    fi
    R2_LATEST_DMG_KEY="${R2_LATEST_DMG_KEY:-Oh-My-Tab.dmg}" \
      cargo run --manifest-path tools/r2-publisher/Cargo.toml --release -- "${PUBLISH_ARGS[@]}"

    if [ "$DRY_RUN" -eq 1 ]; then
      echo "ℹ️  Dry run complete; pending state is kept. Run --push to publish for real."
    else
      COMPLETED_DIR="$NOTARY_ROOT/completed/$SUBMISSION_ID"
      if [ -e "$COMPLETED_DIR" ]; then
        COMPLETED_DIR="${COMPLETED_DIR}-$(date -u +%Y%m%d%H%M%S)"
      fi
      mkdir -p "$(dirname "$COMPLETED_DIR")"
      mv "$PENDING_DIR" "$COMPLETED_DIR"
      echo "✅ Published notarized release. Submission record saved at $COMPLETED_DIR"
    fi
    ;;
esac
