#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
SRC="$ROOT_DIR/apps/macos-contributor"
DIST="${BEENET_APP_DIST:-$SRC/dist/Beenet.app}"
MACOS="$DIST/Contents/MacOS"
RESOURCES="$DIST/Contents/Resources"

if ! command -v swiftc >/dev/null 2>&1; then
    echo "swiftc not found; install Xcode Command Line Tools" >&2
    exit 1
fi

SDK=$(xcrun --sdk macosx --show-sdk-path)
ARCH=${BEENET_APP_ARCH:-$(uname -m)}
TARGET="${ARCH}-apple-macos13"

rm -rf "$DIST"
mkdir -p "$MACOS" "$RESOURCES"

swiftc \
    -O \
    -parse-as-library \
    -target "$TARGET" \
    -sdk "$SDK" \
    -framework SwiftUI \
    -framework AppKit \
    -o "$MACOS/Beenet" \
    "$SRC/Sources/Quota.swift" \
    "$SRC/Sources/Theme.swift" \
    "$SRC/Sources/WorkerConfigFile.swift" \
    "$SRC/Sources/WorkerProcess.swift" \
    "$SRC/Sources/ProcessMeter.swift" \
    "$SRC/Sources/ContributorModel.swift" \
    "$SRC/Sources/Views.swift" \
    "$SRC/Sources/BeenetApp.swift"

cp "$SRC/Info.plist" "$DIST/Contents/Info.plist"
printf 'APPL????' > "$DIST/Contents/PkgInfo"

WORKER_BIN=${BEENET_WORKER_BIN:-"$ROOT_DIR/target/release/beenet-worker"}
if [ -x "$WORKER_BIN" ]; then
    cp "$WORKER_BIN" "$MACOS/beenet-worker"
    chmod +x "$MACOS/beenet-worker"
    echo "bundled worker: $MACOS/beenet-worker"
else
    echo "error: $WORKER_BIN not found; build with cargo build --release -p beenet-worker" >&2
    exit 1
fi

VFKIT_BIN=${BEENET_VFKIT_BIN:-$(command -v vfkit || true)}
if [ -n "$VFKIT_BIN" ]; then
    VFKIT_BIN=$(python3 -c "import os,sys; print(os.path.realpath(sys.argv[1]))" "$VFKIT_BIN")
fi
if [ ! -x "${VFKIT_BIN:-}" ]; then
    echo "error: vfkit not found; install with brew install vfkit or set BEENET_VFKIT_BIN" >&2
    exit 1
fi
cp "$VFKIT_BIN" "$MACOS/vfkit"
chmod +x "$MACOS/vfkit"
echo "bundled vfkit: $MACOS/vfkit ($("$MACOS/vfkit" --version 2>/dev/null || echo unknown))"

VM_CACHE=${BEENET_VM_CACHE_DIR:-"$HOME/Library/Caches/beenet/vm/alpine-3.24.1"}
KERNEL=${BEENET_KERNEL_PATH:-"$VM_CACHE/extracted/boot/Image"}
INITRD=${BEENET_INITRD_PATH:-"$VM_CACHE/beenet-alpine-3.24.1-aarch64-initramfs.img"}
if [ ! -f "$KERNEL" ] || [ ! -f "$INITRD" ]; then
    echo "error: missing Linux guest image. Run scripts/build-macos-vm-image.sh first." >&2
    echo "  kernel: $KERNEL" >&2
    echo "  initrd: $INITRD" >&2
    exit 1
fi
mkdir -p "$RESOURCES/vm"
cp "$KERNEL" "$RESOURCES/vm/Image"
cp "$INITRD" "$RESOURCES/vm/initrd.img"
{
    echo "vfkit $($MACOS/vfkit --version 2>/dev/null || echo unknown)"
    echo "kernel alpine-3.24.1 Image"
    echo "initrd beenet-alpine-3.24.1-aarch64-initramfs.img"
} > "$RESOURCES/vm/runtime.txt"
echo "bundled kernel: $RESOURCES/vm/Image"
echo "bundled initrd: $RESOURCES/vm/initrd.img"

if command -v codesign >/dev/null 2>&1; then
    ENTITLEMENTS="$SRC/Beenet.entitlements"
    codesign --sign - --force --timestamp=none --entitlements "$ENTITLEMENTS" "$MACOS/vfkit" >/dev/null
    codesign --sign - --force --timestamp=none --entitlements "$ENTITLEMENTS" "$MACOS/beenet-worker" >/dev/null
    codesign --sign - --force --timestamp=none --entitlements "$ENTITLEMENTS" "$MACOS/Beenet" >/dev/null
    echo "signed with virtualization entitlement"
fi

echo "app: $DIST"
echo "open with: open \"$DIST\""
if [ "${BEENET_MAKE_DMG:-}" = "1" ]; then
    "$ROOT_DIR/scripts/package-macos-dmg.sh"
fi
