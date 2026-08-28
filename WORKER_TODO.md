# Beenet Worker — decisions and remaining work

This is a decision record for the contributor worker, not a backlog from scratch.
The original plan (native daemon per OS, no Docker at runtime, two-layer quotas)
still holds. The macOS quota path and the desktop stack diverged on purpose.

Windows Job Object mapping lives in [`deploy/windows/job-objects.md`](deploy/windows/job-objects.md).

## Decisions

### Runtime is the daemon; UI is not

`beenet-worker` / `bworker` executes Wasm, heartbeats, and dials the gateway.
Desktop UI only installs, configures, starts/stops, and displays status.
Headless CLI/systemd remains a first-class path.

### No Docker as a worker runtime

Docker is for building images (guest initrd, registry/gateway) and for local
control-plane compose. Ordinary contributor nodes do not run dockerd.

### One product quota, OS-specific enforcement

User-facing `[worker.quota]` is the same everywhere:

| Field | Meaning |
| --- | --- |
| `cpu_percent` | Budget as a **percentage of one logical CPU** (25 = quarter core, 150 = 1.5 cores) |
| `memory_mb` | Whole-worker memory cap |
| `pids_max` | Whole-worker process/thread cap where the OS can express it |
| `nice` | Optional UNIX niceness; not part of the Windows v1 mapping |

Wasmtime still applies per-instance memory and wall-clock deadline. OS quota is
the second boundary around the **whole worker process tree**, not per invoke.

If CPU/memory/pids are set and the OS backend cannot apply them, **start fails**.
Do not warn-and-continue.

### How each OS enforces that quota

| OS | Process shape | OS quota backend |
| --- | --- | --- |
| Linux | Native `beenet-worker` | cgroup v2 written by the daemon (`cpu.max` / `memory.max` / `pids.max`). systemd only starts the process with `Delegate=yes`. |
| macOS contributors | Host supervisor + vfkit Alpine guest | Guest uses the **same Linux cgroup code**. Host `backend=vm` is required for CPU/memory/pids. Host `backend=native` is nice-only and is not the product path. |
| Windows | Native `beenet-worker.exe` | Job Objects. **Not** a Hyper-V/WSL copy of the Mac VM. See [`deploy/windows/job-objects.md`](deploy/windows/job-objects.md). |

Linux and Windows stay native because the OS already has a real second boundary.
macOS does not (nice/rlimit is not cgroup), so contributors pay for a microVM to
get the same CPU/memory/pids semantics as Linux.

Do not add a Windows VM backend unless Job Objects prove insufficient or we
later need an identical Linux syscall surface.

### Desktop stacks stay per-OS

- macOS: Swift App + LaunchAgent (`com.beenet.worker`). Not Tauri.
- Linux: no desktop app. `get-bworker.sh` / `bworker` / systemd.
- Windows: tray + optional user service later, talking to the same exe. Do not
  rewrite the Mac app into a cross-platform shell to get Windows. v1 ships an
  egui desktop app + Inno Setup wizard (`BeenetSetup-x64.exe`). Cache dir is
  chosen at install time; name and region are edited in the running app.

### Control plane vs process liveness

Cloud “online” is a fresh registry heartbeat (60s lease), not “vfkit/systemd
says running”. The Mac supervisor rebuilds vfkit NAT when the host can reach
the registry but the guest heartbeat file is stale (sleep/lock).

### Identity and install

- Peer identity lives in `wasm_cache_dir` (`identity.key`). Join tokens are not
  persisted in `config.toml`.
- Linux config: `~/.config/beenet/config.toml` (XDG).
- macOS config: `~/Library/Application Support/Beenet/config.toml`.
- Windows config: `%APPDATA%\beenet\config.toml`. Cache dir is chosen by the
  installer (`%LOCALAPPDATA%\beenet\wasm_cache` by default).

### One worker process, many Wasm instances

A single worker process runs concurrent instances under one OS quota. That
matches the product control (“this machine contributes 25% CPU / 512 MB”).
Per-invoke child processes are a future isolation upgrade, not the current
model.

## Current implementation

### Linux

- Native daemon; default CLI with no subcommand enrolls then starts.
- `scripts/get-bworker.sh`, alias `bworker`, sample unit
  `deploy/linux/beenet-worker.service`.
- Default quota when installed that way: 25% CPU / 512 MB / 128 pids.
- CI: `.github/workflows/linux-worker.yml` (check on PR; tarball on tag).
- Packaging today: standalone binary + tarball + unit. No `.deb` / `.rpm` yet.

### macOS (arm64)

- App source and DMG: `deploy/macos-contributor/`.
- LaunchAgent runs `beenet-worker run-internal`, which supervises vfkit (does
  not `exec` it). Guest initrd is bundled in the app; users do not run the
  image-build script for the contributor path.
- Guest applies Linux cgroup v2. Host envelope is
  `derive_vm_envelope`: vCPUs = `ceil(cpu_percent / 100)`, RAM =
  `memory_mb + 320`.
- Heartbeat marker + log rotation; CI DMG on `macos-15` (Apple Silicon only).
- Intel Mac is not shipped.

### Windows (x64)

- Native `beenet-worker.exe`; Job Objects apply `[worker.quota]` (see
  [`deploy/windows/job-objects.md`](deploy/windows/job-objects.md)).
- Desktop app: `deploy/windows/app` (egui). Login, start/stop, name/region,
  quota presets. Cache dir is chosen in the Inno Setup wizard, not in the app.
- Installer: `deploy/windows/Beenet.iss` → `BeenetSetup-x64.exe`.
- CI: `.github/workflows/windows-contributor.yml` (check on PR; installer on tag).

### Still missing (all platforms)

- Local control socket / structured status API (App currently execs CLI).
- Pause / resume / run-only-when-idle.
- Quota hot-reload without restart.
- Windows Service / tray autostart; installer is Inno Setup already.
- macOS: vsock guest agent, atomic guest-image update/rollback, Intel.

## Rejected or deferred

| Idea | Status | Why |
| --- | --- | --- |
| macOS native nice as the contributor quota | Rejected | Not a real CPU/memory/pids cap. |
| systemd `CPUQuota` / `MemoryMax` as the Linux quota | Rejected | Dockerd model: unit starts the daemon; the daemon applies cgroup. |
| Docker / WSL2 as the Windows worker | Rejected | Friction and extra runtime; contradicts “no Docker for nodes”. |
| Hyper-V Alpine guest as Windows v1 | Deferred | Job Objects already cap CPU and job memory. Revisit only if needed. |
| One Tauri app for Mac+Windows+Linux | Deferred | Mac Swift app exists; Linux stays CLI. |
| Per-task OS process | Deferred | Product quota is whole-worker. |

## Next work (order)

1. Optional local control API if pause/hot quota needs it.
2. Windows Service / signed installer later; the Service must not set Job
   limits itself (same split as systemd `Delegate=yes`).
3. Linux `.deb` / macOS guest-image updates only when packaging pain shows up.
