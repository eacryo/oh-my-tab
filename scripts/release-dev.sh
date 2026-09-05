#!/bin/sh
# Build the development update channel. By default this only creates local artifacts;
# pass --push to publish them to the dev R2 prefix and appcast.
set -e

PUSH_R2=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --push) PUSH_R2=1 ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help)
      echo "Usage: sh scripts/release-dev.sh [--push] [--dry-run]"
      echo "  (no flag)  build the dev package locally; never contacts R2"
      echo "  --push     upload the dev ZIP, DMG, then dev appcast to R2"
      echo "  --dry-run  print the R2 upload plan without uploading"
      exit 0
      ;;
    *)
      echo "❌ Unknown argument: $arg" >&2
      echo "Usage: sh scripts/release-dev.sh [--push] [--dry-run]" >&2
      exit 2
      ;;
  esac
done

cd "$(dirname "$0")/.."

# Keep the development channel isolated from production at every level: bundle identity,
# Sparkle feed, R2 prefix, appcast key, and archive basename are all distinct.
APP_BASENAME="${APP_BASENAME:-Oh-My-Tab-Dev}"
BUNDLE_ID="${BUNDLE_ID:-com.eacryo.oh-my-tab.dev}"
BUNDLE_NAME="${BUNDLE_NAME:-Oh-My-Tab Dev}"
CARGO_BUILD_FEATURES="dev-long-text"
SPARKLE_FEED_URL="${SPARKLE_FEED_URL:-https://download.oh-my-tab.app/dev_release/appcast.xml}"
R2_RELEASE_PREFIX="${R2_RELEASE_PREFIX:-dev_release}"
R2_APPCAST_KEY="${R2_APPCAST_KEY:-dev_release/appcast.xml}"
R2_ARTIFACT_BASENAME="${R2_ARTIFACT_BASENAME:-$APP_BASENAME}"
R2_APPCAST_PATH="${R2_APPCAST_PATH:-dist/appcast-dev.xml}"

APP="dist/${APP_BASENAME}.app"
ZIP="dist/${APP_BASENAME}.zip"
DMG="dist/${APP_BASENAME}.dmg"

APP_BASENAME="$APP_BASENAME" \
BUNDLE_ID="$BUNDLE_ID" \
BUNDLE_NAME="$BUNDLE_NAME" \
CARGO_BUILD_FEATURES="$CARGO_BUILD_FEATURES" \
SPARKLE_FEED_URL="$SPARKLE_FEED_URL" \
sh scripts/bundle.sh

if [ ! -f "$APP/Contents/Info.plist" ] || [ ! -f "$ZIP" ] || [ ! -f "$DMG" ]; then
  echo "❌ Build failed: expected $APP, $ZIP, and $DMG" >&2
  exit 1
fi

VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")
BUILD_VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist")
echo "✅ Dev build ready: $APP (version=$VERSION, build=$BUILD_VERSION)"

if [ "$PUSH_R2" -eq 1 ]; then
  if [ "$DRY_RUN" -eq 0 ]; then
    # Generate/update the feed before publishing. The helper reuses a local feed or fetches the
    # public feed on a clean checkout, then generates URLs matching the R2 object names.
    R2_RELEASE_PREFIX="$R2_RELEASE_PREFIX" \
    R2_ARTIFACT_BASENAME="$R2_ARTIFACT_BASENAME" \
    R2_PUBLIC_BASE_URL="${R2_PUBLIC_BASE_URL:-https://download.oh-my-tab.app}" \
    SPARKLE_FEED_URL="$SPARKLE_FEED_URL" \
    bash scripts/generate-appcast.sh "$R2_APPCAST_PATH" "$ZIP" "$VERSION" "$BUILD_VERSION"
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
