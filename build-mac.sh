#!/usr/bin/env bash
# Sentroy Monitor — universal macOS build: sign (Developer ID), notarize, staple.
# Env: SIGN_ID, APPLE_ID, TEAM_ID, APP_PASSWORD (same as sentroy-desktop/build-mac.sh).
# Updater: signs the .app.tar.gz with ~/.tauri/sentroy-monitor.key (or
# TAURI_SIGNING_PRIVATE_KEY) and writes latest.json (darwin platforms) next to the DMG.
set -euo pipefail
cd "$(dirname "$0")"

for v in SIGN_ID APPLE_ID TEAM_ID APP_PASSWORD; do
  if [ -z "${!v:-}" ]; then echo "missing env: $v" >&2; exit 1; fi
done
rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null

export APPLE_SIGNING_IDENTITY="$SIGN_ID"
export APPLE_ID APPLE_TEAM_ID="$TEAM_ID" APPLE_PASSWORD="$APP_PASSWORD"

if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  KEY_FILE="${TAURI_SIGNING_KEY_FILE:-$HOME/.tauri/sentroy-monitor.key}"
  if [ ! -f "$KEY_FILE" ]; then echo "missing updater key: $KEY_FILE" >&2; exit 1; fi
  TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_FILE")"
fi
export TAURI_SIGNING_PRIVATE_KEY
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

npm install --no-audit --no-fund
npm test
# Tauri signs, notarizes and staples the .app. The DMG is made with plain
# hdiutil: Tauri's DMG step drives Finder over AppleScript, which times out
# in headless/background sessions.
if [ "${SKIP_APP_BUILD:-}" != "1" ]; then
  npx tauri build --target universal-apple-darwin --bundles app
fi

VERSION=$(node -p "require('./package.json').version")
OUT="src-tauri/target/universal-apple-darwin/release/bundle"
APP="$OUT/macos/Sentroy Monitor.app"
DMG="$OUT/Sentroy-Monitor-${VERSION}-mac-universal.dmg"
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "Sentroy Monitor" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

codesign --force --sign "$SIGN_ID" --timestamp "$DMG"
xcrun notarytool submit "$DMG" --apple-id "$APPLE_ID" --team-id "$TEAM_ID" --password "$APP_PASSWORD" --wait
xcrun stapler staple "$DMG"

spctl -a -vvv -t install "$DMG"
spctl -a -vvv "$APP"

# Updater artifacts: Tauri wrote "<App>.app.tar.gz" + ".sig" next to the .app.
TARBALL_SRC="$OUT/macos/Sentroy Monitor.app.tar.gz"
if [ ! -f "$TARBALL_SRC" ] || [ ! -f "$TARBALL_SRC.sig" ]; then
  echo "missing updater artifacts ($TARBALL_SRC[.sig])" >&2; exit 1
fi
TARBALL_NAME="Sentroy-Monitor-${VERSION}-mac-universal.app.tar.gz"
cp "$TARBALL_SRC" "$OUT/$TARBALL_NAME"
cp "$TARBALL_SRC.sig" "$OUT/$TARBALL_NAME.sig"
URL="https://github.com/Sentroy-Co/sentroy-monitor-desktop/releases/download/v${VERSION}/${TARBALL_NAME}"
SIG="$(cat "$OUT/$TARBALL_NAME.sig")" URL="$URL" VERSION="$VERSION" node -e '
  const { URL: url, SIG: signature, VERSION: version } = process.env
  const entry = { signature, url }
  const json = { version, notes: `Sentroy Monitor ${version}`, pub_date: new Date().toISOString(),
    platforms: { "darwin-aarch64": entry, "darwin-x86_64": entry } }
  process.stdout.write(JSON.stringify(json, null, 2) + "\n")
' > "$OUT/latest-mac.json"

echo "✓ $DMG"
echo "✓ $OUT/$TARBALL_NAME (+ .sig)"
echo "✓ $OUT/latest-mac.json — merge into the release's latest.json (README → Release)"
