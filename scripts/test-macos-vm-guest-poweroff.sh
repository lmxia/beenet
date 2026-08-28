#!/bin/sh
set -eu

# Boot the macOS microVM with a stub guest worker that exits immediately.
# PID 1 must power off Linux so vfkit exits (the launchd KeepAlive contract).

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CACHE_DIR=${BEENET_VM_CACHE_DIR:-"$HOME/Library/Caches/beenet/vm/alpine-3.24.1"}
KERNEL=${BEENET_VM_KERNEL:-"$CACHE_DIR/extracted/boot/Image"}
BASE_ROOT="$CACHE_DIR/build/root"
VFKIT=${BEENET_VFKIT:-$(command -v vfkit)}
TIMEOUT_SECS=${BEENET_VFKIT_TIMEOUT_SECS:-45}

if [ "$(uname -s)" != "Darwin" ]; then
    echo "this test requires macOS and vfkit" >&2
    exit 1
fi
[ -x "$VFKIT" ] || {
    echo "vfkit not found; install with brew install vfkit" >&2
    exit 1
}
[ -f "$KERNEL" ] || {
    echo "missing kernel: $KERNEL (run scripts/build-macos-vm-image.sh)" >&2
    exit 1
}
[ -d "$BASE_ROOT" ] || {
    echo "missing initramfs root: $BASE_ROOT (run scripts/build-macos-vm-image.sh)" >&2
    exit 1
}

WORK=$(mktemp -d "${TMPDIR:-/tmp}/beenet-vm-poweroff.XXXXXX")
cleanup() {
    if [ -n "${VFKIT_PID:-}" ] && kill -0 "$VFKIT_PID" 2>/dev/null; then
        kill -TERM "$VFKIT_PID" 2>/dev/null || true
        wait "$VFKIT_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

echo "copying initramfs root to $WORK"
mkdir -p "$WORK/config" "$WORK/state/logs"
cp -a "$BASE_ROOT" "$WORK/root"
install -m 0755 "$ROOT_DIR/deploy/macos-contributor/guest/alpine-init" "$WORK/root/init"
cat >"$WORK/root/usr/local/bin/beenet-worker" <<'EOF'
#!/bin/sh
mkdir -p /var/lib/beenet/logs
echo "stub guest worker exiting 0" >>/var/lib/beenet/logs/worker.log
exit 0
EOF
chmod 0755 "$WORK/root/usr/local/bin/beenet-worker"
cat >"$WORK/config/config.toml" <<'EOF'
[worker]
registry_url = "http://127.0.0.1:9"
name = "poweroff-stub"
EOF

echo "packing test initramfs"
(cd "$WORK/root" && find . -print | LC_ALL=C sort | cpio -o -H newc 2>/dev/null) \
    | gzip -9 >"$WORK/initrd.img"

CONSOLE="$WORK/console.log"
"$VFKIT" \
    --cpus 1 \
    --memory 512 \
    --kernel "$KERNEL" \
    --initrd "$WORK/initrd.img" \
    --kernel-cmdline "console=hvc0 beenet.config=config.toml" \
    --device virtio-net,nat \
    --device "virtio-serial,logFilePath=$CONSOLE" \
    --device "virtio-fs,sharedDir=$WORK/config,mountTag=beenet-config" \
    --device "virtio-fs,sharedDir=$WORK/state,mountTag=beenet-state" \
    --log-level info &
VFKIT_PID=$!

elapsed=0
while kill -0 "$VFKIT_PID" 2>/dev/null; do
    if [ "$elapsed" -ge "$TIMEOUT_SECS" ]; then
        echo "vfkit still running after ${TIMEOUT_SECS}s; guest did not power off" >&2
        echo "---- console ----" >&2
        cat "$CONSOLE" 2>/dev/null >&2 || true
        echo "---- guest worker.log ----" >&2
        cat "$WORK/state/logs/worker.log" 2>/dev/null >&2 || true
        exit 1
    fi
    sleep 1
    elapsed=$((elapsed + 1))
done

wait "$VFKIT_PID"
vfkit_status=$?
VFKIT_PID=""

echo "vfkit exited with status $vfkit_status after ${elapsed}s"
if [ ! -f "$WORK/state/logs/worker.log" ]; then
    echo "missing guest worker.log" >&2
    cat "$CONSOLE" 2>/dev/null || true
    exit 1
fi
grep -q "stub guest worker exiting 0" "$WORK/state/logs/worker.log"
grep -q "beenet guest worker exited with status 0; powering off" "$WORK/state/logs/worker.log"
echo "guest worker exited and PID 1 requested poweroff"
if [ -f "$CONSOLE" ]; then
    echo "---- console (tail) ----"
    tail -n 40 "$CONSOLE" || true
fi
echo "ok: vfkit exited after guest poweroff"
