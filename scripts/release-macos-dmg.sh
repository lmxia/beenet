#!/bin/sh
set -eu

# Build, sign, notarize, and staple the public macOS artifact.
# Required: DEVELOPER_ID_APPLICATION and NOTARYTOOL_PROFILE.
# Create the profile once with:
#   xcrun notarytool store-credentials beenet-notary --apple-id ... --team-id ... --password ...

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SRC="$ROOT_DIR/deploy/macos-contributor"
APP="${BEENET_APP_DIST:-$SRC/dist/Beenet.app}"
IDENTITY=${DEVELOPER_ID_APPLICATION:-}
PROFILE=${NOTARYTOOL_PROFILE:-beenet-notary}

[ "$(uname -s)" = Darwin ] || { echo "error: run this on macOS" >&2; exit 1; }
[ -n "$IDENTITY" ] || { echo "error: set DEVELOPER_ID_APPLICATION to the full Developer ID Application identity" >&2; exit 1; }
case "$IDENTITY" in *"Developer ID Application"*) ;; *) echo "error: identity must be Developer ID Application" >&2; exit 1 ;; esac

make -C "$ROOT_DIR" app-macos

ENTITLEMENTS="$SRC/Beenet.entitlements"
codesign --sign "$IDENTITY" --force --timestamp --options runtime --entitlements "$ENTITLEMENTS" "$APP/Contents/MacOS/vfkit"
codesign --sign "$IDENTITY" --force --timestamp --options runtime --entitlements "$ENTITLEMENTS" "$APP/Contents/MacOS/beenet-worker"
codesign --sign "$IDENTITY" --force --timestamp --options runtime --entitlements "$ENTITLEMENTS" "$APP/Contents/MacOS/Beenet"
codesign --sign "$IDENTITY" --force --timestamp --options runtime --entitlements "$ENTITLEMENTS" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

# Notarize the signed DMG once. The ticket covers the nested Developer ID app;
# a second --wait on a zipped .app doubles wall-clock time in Apple's queue.
DMG_DIR=$(dirname "$APP")
BEENET_APP_DIST="$APP" BEENET_DMG_PATH="$DMG_DIR/Beenet-notarized.dmg" \
  "$ROOT_DIR/scripts/package-macos-dmg.sh"
DMG="$DMG_DIR/Beenet-notarized.dmg"
codesign --sign "$IDENTITY" --force --timestamp "$DMG"
codesign --verify --verbose=2 "$DMG"
xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
xcrun notarytool history --keychain-profile "$PROFILE" | head -20
echo "release: $DMG"
