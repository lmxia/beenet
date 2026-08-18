#!/bin/sh
set -eu

# Wrap Beenet.app into a drag-to-Applications DMG. Kernel, vfkit, and worker
# stay inside the app bundle; this script does not copy them into git.

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SRC="$ROOT_DIR/apps/macos-contributor"
APP="${BEENET_APP_DIST:-$SRC/dist/Beenet.app}"
DIST_DIR=$(dirname "$APP")
ARCH=${BEENET_APP_ARCH:-$(uname -m)}
VERSION=${BEENET_APP_VERSION:-}

if [ ! -d "$APP" ]; then
    echo "error: $APP not found; run apps/macos-contributor/build.sh first" >&2
    exit 1
fi

if [ -z "$VERSION" ]; then
    VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")
fi

DMG="${BEENET_DMG_PATH:-$DIST_DIR/Beenet-${VERSION}-darwin-${ARCH}.dmg}"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/Beenet.app"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG"
hdiutil create \
    -volname "Beenet" \
    -srcfolder "$STAGE" \
    -ov \
    -format UDZO \
    "$DMG" >/dev/null

echo "dmg: $DMG"
ls -lh "$DMG"
