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
MNTROOT=$(mktemp -d)
RW_DMG="${DMG}.rw.dmg"
MOUNT=""
trap 'if [ -n "$MOUNT" ]; then hdiutil detach "$MOUNT" >/dev/null 2>&1 || true; fi; rm -rf "$STAGE" "$MNTROOT"; rm -f "$RW_DMG"' EXIT

mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/Beenet.app"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG" "$RW_DMG"
hdiutil create \
    -volname "Beenet" \
    -srcfolder "$STAGE" \
    -ov \
    -format UDRW \
    "$RW_DMG" >/dev/null

hdiutil attach -readwrite -noverify -nobrowse -mountroot "$MNTROOT" "$RW_DMG" >/dev/null
MOUNT="$MNTROOT/Beenet"
if [ ! -d "$MOUNT" ]; then
    echo "error: failed to mount $RW_DMG at $MOUNT" >&2
    exit 1
fi
ICON_ICNS="$APP/Contents/Resources/AppIcon.icns"
if [ -f "$ICON_ICNS" ]; then
    cp "$ICON_ICNS" "$MOUNT/.VolumeIcon.icns"
    if command -v SetFile >/dev/null 2>&1; then
        SetFile -c icnC "$MOUNT/.VolumeIcon.icns"
        SetFile -a C "$MOUNT"
    fi
fi
hdiutil detach "$MOUNT" >/dev/null
MOUNT=""
hdiutil convert "$RW_DMG" -format UDZO -imagekey zlib-level=9 -o "$DMG" >/dev/null
rm -f "$RW_DMG"

echo "dmg: $DMG"
ls -lh "$DMG"
