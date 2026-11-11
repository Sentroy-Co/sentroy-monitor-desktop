#!/usr/bin/env bash
# Sentroy Monitor — universal macOS build: sign (Developer ID), notarize, staple.
# Env: SIGN_ID, APPLE_ID, TEAM_ID, APP_PASSWORD (same as sentroy-desktop/build-mac.sh).
set -euo pipefail
cd "$(dirname "$0")"

for v in SIGN_ID APPLE_ID TEAM_ID APP_PASSWORD; do
  if [ -z "${!v:-}" ]; then echo "missing env: $v" >&2; exit 1; fi
done
rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null

export APPLE_SIGNING_IDENTITY="$SIGN_ID"
export APPLE_ID APPLE_TEAM_ID="$TEAM_ID" APPLE_PASSWORD="$APP_PASSWORD"

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
echo "✓ $DMG"
