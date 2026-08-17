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

mkdir -p "$CACHE_DIR" "$WORK/root"
if [ ! -f "$ISO" ]; then
    curl -fL --retry 5 -C - -o "$ISO" "$ALPINE_URL"
fi
actual_sha=$(shasum -a 256 "$ISO" | awk '{print $1}')
[ "$actual_sha" = "$ALPINE_SHA256" ] || {
    echo "Alpine SHA-256 mismatch: $actual_sha" >&2
    exit 1
}

rm -rf "$WORK/root"
mkdir -p "$WORK/root"
tar -xOf "$ISO" boot/initramfs-virt | gzip -dc | (cd "$WORK/root" && cpio -idm 2>/dev/null)
mkdir -p "$WORK/root/etc/ssl/certs"
tar -xOf "$ISO" apks/aarch64/ca-certificates-bundle-20260611-r0.apk \
    | tar -xzOf - etc/ssl/certs/ca-certificates.crt \
    > "$WORK/root/etc/ssl/certs/ca-certificates.crt"

rm -rf "$WORK/worker-artifact"
mkdir -p "$WORK/worker-artifact"
docker buildx build \
    --builder "${BEENET_BUILDX_BUILDER:-desktop-linux}" \
    --platform linux/arm64 \
    --build-context "spin=$ROOT_DIR/../spin" \
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

echo "initramfs: $CACHE_DIR/beenet-alpine-${ALPINE_VERSION}-aarch64-initramfs.img"
echo "worker:    $CACHE_DIR/beenet-worker-aarch64-linux-musl"
ls -lh "$CACHE_DIR/beenet-alpine-${ALPINE_VERSION}-aarch64-initramfs.img" \
    "$CACHE_DIR/beenet-worker-aarch64-linux-musl"
