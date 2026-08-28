#!/bin/sh
set -eu

# Build AppIcon.png (full-bleed) and AppIcon.icns from Assets/AppIcon-source.png.

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SRC="${1:-$ROOT/Assets/AppIcon-source.png}"
PNG="${2:-$ROOT/Assets/AppIcon.png}"
ICNS="${3:-$ROOT/Assets/AppIcon.icns}"

if [ ! -f "$SRC" ]; then
    echo "error: missing icon source $SRC" >&2
    exit 1
fi

if ! command -v sips >/dev/null 2>&1 || ! command -v iconutil >/dev/null 2>&1; then
    echo "error: sips and iconutil are required to build AppIcon.icns" >&2
    exit 1
fi

swift "$ROOT/scripts/fill-appicon.swift" "$SRC" "$PNG"

ICONSET=$(mktemp -d)/AppIcon.iconset
mkdir -p "$ICONSET"
trap 'rm -rf "$(dirname "$ICONSET")"' EXIT

# iconutil names: 16,32,128,256,512 and their @2x counterparts.
sips -z 16 16     "$PNG" --out "$ICONSET/icon_16x16.png" >/dev/null
sips -z 32 32     "$PNG" --out "$ICONSET/icon_16x16@2x.png" >/dev/null
sips -z 32 32     "$PNG" --out "$ICONSET/icon_32x32.png" >/dev/null
sips -z 64 64     "$PNG" --out "$ICONSET/icon_32x32@2x.png" >/dev/null
sips -z 128 128   "$PNG" --out "$ICONSET/icon_128x128.png" >/dev/null
sips -z 256 256   "$PNG" --out "$ICONSET/icon_128x128@2x.png" >/dev/null
sips -z 256 256   "$PNG" --out "$ICONSET/icon_256x256.png" >/dev/null
sips -z 512 512   "$PNG" --out "$ICONSET/icon_256x256@2x.png" >/dev/null
sips -z 512 512   "$PNG" --out "$ICONSET/icon_512x512.png" >/dev/null
sips -z 1024 1024 "$PNG" --out "$ICONSET/icon_512x512@2x.png" >/dev/null

iconutil -c icns "$ICONSET" -o "$ICNS"
echo "png: $PNG"
echo "icns: $ICNS"
ls -lh "$PNG" "$ICNS"
