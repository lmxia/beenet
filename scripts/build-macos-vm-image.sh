#!/bin/sh
set -eu

# Build a small, immutable Alpine initramfs for Apple Silicon. The output is
# kept outside the repository; identity, cache, config, and logs are mounted
# from the host at runtime.

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CACHE_DIR=${BEENET_VM_CACHE_DIR:-"$HOME/Library/Caches/beenet/vm/alpine-3.24.1"}
ALPINE_VERSION=3.24.1
ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/aarch64/alpine-virt-${ALPINE_VERSION}-aarch64.iso"
ALPINE_SHA256=c81699152db11d2a6dbb7d75348d632fcf5811eff414d7e71876a8bb6d48bc02
ISO="$CACHE_DIR/alpine-virt-${ALPINE_VERSION}-aarch64.iso"
WORK="$CACHE_DIR/build"
SPIN_DIR=${BEENET_SPIN_DIR:-"$ROOT_DIR/../spin"}

iso_extract() {
    # macOS tar is libarchive and can read ISO 9660; GNU tar on Linux cannot.
    if command -v bsdtar >/dev/null 2>&1; then
        bsdtar -xOf "$ISO" "$@"
    else
        tar -xOf "$ISO" "$@"
    fi
}

sha256_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        sha256sum "$1" | awk '{print $1}'
    fi
}

mkdir -p "$CACHE_DIR" "$WORK/root"
if [ ! -f "$ISO" ]; then
    curl -fL --retry 5 -C - -o "$ISO" "$ALPINE_URL"
fi
actual_sha=$(sha256_of "$ISO")
[ "$actual_sha" = "$ALPINE_SHA256" ] || {
    echo "Alpine SHA-256 mismatch: $actual_sha" >&2
    exit 1
}

mkdir -p "$CACHE_DIR/extracted/boot"
iso_extract boot/vmlinuz-virt > "$CACHE_DIR/extracted/boot/vmlinuz-virt"
python3 - "$CACHE_DIR/extracted/boot/vmlinuz-virt" "$CACHE_DIR/extracted/boot/Image" <<'PY'
import sys, zlib
src, dst = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
idx = data.find(b"\x1f\x8b")
if idx < 0:
    raise SystemExit("gzip payload not found in vmlinuz-virt")
open(dst, "wb").write(zlib.decompress(data[idx:], 16 + zlib.MAX_WBITS))
PY

rm -rf "$WORK/root"
mkdir -p "$WORK/root"
iso_extract boot/initramfs-virt | gzip -dc | (cd "$WORK/root" && cpio -idm 2>/dev/null)
mkdir -p "$WORK/root/etc/ssl/certs"
iso_extract apks/aarch64/ca-certificates-bundle-20260611-r0.apk \
    | tar -xzOf - etc/ssl/certs/ca-certificates.crt \
    > "$WORK/root/etc/ssl/certs/ca-certificates.crt"

rm -rf "$WORK/worker-artifact"
mkdir -p "$WORK/worker-artifact"
if [ ! -d "$SPIN_DIR" ]; then
    echo "error: Spin checkout not found at $SPIN_DIR" >&2
    echo "clone https://github.com/spinframework/spin at the revision in scripts/spin.rev" >&2
    exit 1
fi

BUILDX_BUILDER_ARGS=""
if [ -n "${BEENET_BUILDX_BUILDER:-}" ]; then
    BUILDX_BUILDER_ARGS="--builder $BEENET_BUILDX_BUILDER"
fi

# shellcheck disable=SC2086
docker buildx build \
    $BUILDX_BUILDER_ARGS \
    --platform linux/arm64 \
    --build-context "spin=$SPIN_DIR" \
    --build-arg "HTTP_PROXY=${BEENET_DOCKER_HTTP_PROXY:-${HTTP_PROXY:-}}" \
    --build-arg "HTTPS_PROXY=${BEENET_DOCKER_HTTPS_PROXY:-${HTTPS_PROXY:-}}" \
    --file "$ROOT_DIR/docker/Dockerfile.worker-vm" \
    --target artifact \
    --output "type=local,dest=$WORK/worker-artifact" \
    "$ROOT_DIR"
mkdir -p "$WORK/root/usr/local/bin"
install -m 0755 "$WORK/worker-artifact/beenet-worker" \
    "$WORK/root/usr/local/bin/beenet-worker"
install -m 0644 "$WORK/worker-artifact/libgcc_s.so.1" \
    "$WORK/root/usr/lib/libgcc_s.so.1"
install -m 0755 "$ROOT_DIR/vm/alpine-init" "$WORK/root/init"

(cd "$WORK/root" && find . -print | LC_ALL=C sort | cpio -o -H newc 2>/dev/null) \
    | gzip -9 > "$CACHE_DIR/beenet-alpine-${ALPINE_VERSION}-aarch64-initramfs.img"
cp "$WORK/root/usr/local/bin/beenet-worker" "$CACHE_DIR/beenet-worker-aarch64-linux-musl"

echo "kernel:    $CACHE_DIR/extracted/boot/Image"
echo "initramfs: $CACHE_DIR/beenet-alpine-${ALPINE_VERSION}-aarch64-initramfs.img"
echo "worker:    $CACHE_DIR/beenet-worker-aarch64-linux-musl"
ls -lh "$CACHE_DIR/extracted/boot/Image" \
    "$CACHE_DIR/beenet-alpine-${ALPINE_VERSION}-aarch64-initramfs.img" \
    "$CACHE_DIR/beenet-worker-aarch64-linux-musl"
