#!/bin/bash
# Generate an appcast for the exact archive that will be uploaded by a release script.
# 为即将上传的精确归档生成 appcast，避免 appcast 中的 URL 和上传对象不一致。
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "Usage: $0 APPCAST_PATH ZIP_PATH VERSION BUILD_VERSION" >&2
  exit 2
fi

APPCAST_PATH="$1"
ZIP_PATH="$2"
VERSION="$3"
BUILD_VERSION="$4"

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
GENERATE_APPCAST="$REPO_DIR/vendor/Sparkle/bin/generate_appcast"
RELEASE_PREFIX="${R2_RELEASE_PREFIX:-releases}"
ARTIFACT_BASENAME="${R2_ARTIFACT_BASENAME:-Oh-My-Tab}"
PUBLIC_BASE_URL="${R2_PUBLIC_BASE_URL:-https://download.oh-my-tab.app}"
FEED_URL="${SPARKLE_FEED_URL:-https://download.oh-my-tab.app/appcast.xml}"

if [ ! -x "$GENERATE_APPCAST" ]; then
  echo "error: Sparkle appcast tool not found or not executable: $GENERATE_APPCAST" >&2
  exit 1
fi
if [ ! -f "$ZIP_PATH" ]; then
  echo "error: update archive not found: $ZIP_PATH" >&2
  exit 1
fi

WORK_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

# R2 publisher renames the local ZIP to this immutable object key. Generate from that same name
# so the enclosure URL in appcast points at the object that will actually be uploaded.
ARCHIVE_NAME="${ARTIFACT_BASENAME}-${VERSION}-${BUILD_VERSION}.zip"
cp "$ZIP_PATH" "$WORK_DIR/$ARCHIVE_NAME"

# Prefer a local appcast so an intentional offline release can preserve its feed. On a clean
# checkout, fetch the public feed before generating so older update entries are retained.
if [ -f "$APPCAST_PATH" ]; then
  cp "$APPCAST_PATH" "$WORK_DIR/appcast.xml"
else
  HTTP_STATUS="$(curl -sS -L --retry 2 --connect-timeout 10 \
    -o "$WORK_DIR/remote-appcast.xml" -w '%{http_code}' "$FEED_URL" || true)"
  case "$HTTP_STATUS" in
    2??)
      cp "$WORK_DIR/remote-appcast.xml" "$WORK_DIR/appcast.xml"
      echo "ℹ️  Reusing existing appcast from $FEED_URL"
      ;;
    404)
      echo "ℹ️  No existing appcast found at $FEED_URL; generating a new feed"
      ;;
    *)
      echo "error: could not fetch existing appcast from $FEED_URL (HTTP status: ${HTTP_STATUS:-unknown})" >&2
      echo "       Place an existing appcast at $APPCAST_PATH or fix the feed/network before publishing." >&2
      exit 1
      ;;
  esac
fi

mkdir -p "$(dirname "$APPCAST_PATH")"
DOWNLOAD_PREFIX="${PUBLIC_BASE_URL%/}/${RELEASE_PREFIX#/}"

APPCAST_ARGS=(
  --download-url-prefix "$DOWNLOAD_PREFIX"
  --link "https://github.com/eacryo/oh-my-tab"
  -o "$APPCAST_PATH"
)
if [ -n "${SPARKLE_ED_KEY_FILE:-}" ]; then
  APPCAST_ARGS+=(--ed-key-file "$SPARKLE_ED_KEY_FILE")
fi

echo "ℹ️  Generating appcast for $ARCHIVE_NAME"
"$GENERATE_APPCAST" "${APPCAST_ARGS[@]}" "$WORK_DIR"
echo "✅ Generated $APPCAST_PATH"
