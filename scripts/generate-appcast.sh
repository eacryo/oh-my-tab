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

# R2 发布器会把归档放到 RELEASE_PREFIX 下，因此 appcast 的 enclosure URL 也必须使用同一前缀。
# R2 stores archives below RELEASE_PREFIX, so enclosure URLs must use the same prefix.
if [ -n "$RELEASE_PREFIX" ]; then
  DOWNLOAD_PREFIX="${PUBLIC_BASE_URL%/}/${RELEASE_PREFIX#/}/"
else
  DOWNLOAD_PREFIX="${PUBLIC_BASE_URL%/}/"
fi

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

# 旧 feed 可能在接入频道前缀以前生成。这里只迁移当前归档族，生产和开发 feed 可以独立修复，
# 不会改动无关的 enclosure URL。
# Older feeds may predate channel prefixes. Migrate only this artifact family so production and
# development feeds can be repaired independently without touching unrelated enclosure URLs.
OLD_DOWNLOAD_PREFIX="${PUBLIC_BASE_URL%/}/"
if [ -f "$WORK_DIR/appcast.xml" ] && [ "$OLD_DOWNLOAD_PREFIX" != "$DOWNLOAD_PREFIX" ]; then
  sed -i '' "s|${OLD_DOWNLOAD_PREFIX}${ARTIFACT_BASENAME}-|${DOWNLOAD_PREFIX}${ARTIFACT_BASENAME}-|g" \
    "$WORK_DIR/appcast.xml"
fi

mkdir -p "$(dirname "$APPCAST_PATH")"

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
EXPECTED_URL="${DOWNLOAD_PREFIX}${ARCHIVE_NAME}"
if ! grep -Fq "url=\"$EXPECTED_URL\"" "$APPCAST_PATH"; then
  echo "error: generated appcast does not contain the expected enclosure URL: $EXPECTED_URL" >&2
  exit 1
fi
echo "✅ Generated $APPCAST_PATH"
