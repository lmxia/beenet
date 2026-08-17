# Beenet Worker TODO

This document tracks the product and engineering path for making
`beenet-worker` suitable for globally distributed, user-operated nodes.

## Positioning

`beenet-worker` should be a lightweight native worker runtime for discrete
nodes across different operating systems and networks.

Docker is useful for local development and controlled deployments, but it
should not be required for ordinary worker nodes. A global worker network needs
a lower-friction installation path and native resource controls.

The preferred shape is:

```text
beenet-worker daemon
        ^
        | local API / socket
        v
Beenet Desktop App
```

Advanced users and servers can run the daemon directly. Ordinary users can use
a desktop app to install, configure, pause, resume, and observe the worker.

## Architecture Goals

- Keep the core worker as a native daemon / service.
- Keep desktop UI separate from the execution runtime.
- Do not require Docker for end-user worker participation.
- Use Wasm sandbox limits as the first resource boundary.
- Add OS-level resource quotas as a second boundary.
- Make worker identity, cache, logs, and configuration explicit and persistent.
- Support unattended restart through system service integration.

## Resource Quota Model

The worker should enforce limits at two layers.

### Wasm Runtime Layer

- Memory limit per instance.
- Wall-clock deadline per invocation.
- Maximum concurrency per worker.
- Capability policy for outbound HTTP and other factors.
- Log/output truncation for wire safety.
- Future: fuel or epoch-based interruption where appropriate.

### OS Layer

Different operating systems need different quota backends behind a shared
worker abstraction.

| OS | Preferred Backend | Notes |
| --- | --- | --- |
| Linux | cgroup v2 / systemd slice | Best fit for CPU, memory, pids, and IO quotas. |
| Windows | Job Objects | Good fit for process tree CPU and memory limits. |
| macOS | rlimit, priority, process policy | Weaker than cgroup; may need conservative defaults. |

The initial macOS backend split is now `native` (nice only) and `vm` (vfkit /
Apple Virtualization.framework supervising a dedicated Linux guest). The guest runs
`beenet-worker run-internal` and uses the same Linux cgroup v2 implementation. This is
deliberately not a Docker Desktop runtime dependency. The Linux/arm64 worker is built natively
inside a multi-stage Alpine Docker build, so macOS does not need a Linux Rust target, Zig, LLVM,
or another cross toolchain. The initial Alpine initramfs and real vfkit boot have been validated
with `cpu.max`, `memory.max`, and `pids.max` applied.
The guest PID 1 also shuts down Linux when the worker exits so launchd can restart vfkit cleanly.

The worker should expose one product-level quota model, then translate it to
the best local backend.

Example user-facing controls:

- CPU budget: 10%, 25%, 50%, unlimited.
- Memory budget: 256 MB, 512 MB, 1 GB, custom.
- Max concurrent tasks.
- Run always / run only when idle.
- Network permission profile.
- Pause and resume.

## Native Daemon Work

- Define daemon mode for `beenet-worker`.
- Add a local control API, preferably over a local-only socket.
- Expose status:
  - peer id
  - display name
  - registry enrollment state
  - connected gateway state
  - supported CIDs
  - loaded CIDs
  - recent invocations
  - quota state
  - last errors
- Add commands for:
  - start
  - stop
  - pause
  - resume
  - status
  - rotate / reset identity with explicit confirmation
  - refresh join token
- Decide service integration:
  - Linux: systemd unit.
  - macOS: launchd plist.
  - Windows: Windows Service.

## Desktop App Work

The desktop app should be a control surface, not the runtime itself.

Recommended stack: Tauri, unless a later reason strongly favors another shell.

Desktop app responsibilities:

- Install or locate the worker daemon.
- Create and edit worker config.
- Guide first-time enrollment with a join token.
- Start, stop, pause, and resume the daemon.
- Show health, logs, quota usage, and recent tasks.
- Let users choose CPU, memory, and network limits.
- Surface upgrade availability.
- Preserve a path for headless CLI/server operation.

## Cross-OS Developer Work

- Add `make build-worker`.
- Add `make lint-worker`.
- Add CI matrix for:
  - Linux
  - macOS
  - Windows
- At minimum, run `cargo check -p beenet-worker` on all three OSes.
- Document platform config paths:
  - Linux: `$XDG_CONFIG_HOME/beenet/config.toml` or `~/.config/beenet/config.toml`.
  - macOS: `~/Library/Application Support/beenet/config.toml`.
  - Windows: `%APPDATA%\beenet\config.toml`.
- Add Windows PowerShell startup instructions.
- Keep Bash scripts for macOS/Linux local development.
- Consider adding `scripts/dev-up.ps1` only if native Windows local development
  becomes a priority.

## Packaging

Potential distribution formats:

- Linux:
  - standalone binary
  - `.deb`
  - `.rpm`
  - systemd unit
- macOS:
  - signed app bundle for desktop UI
  - signed daemon/helper
  - launchd integration
- Windows:
  - signed installer
  - Windows Service
  - tray app

The CLI/daemon should remain independently downloadable even after a desktop
app exists.

## Open Questions

- Should a single worker process run multiple Wasm tasks under one OS quota, or
  should each task run in a supervised child process for stronger OS isolation?
- How strict should macOS quotas be in the first version?
- Should quota enforcement be required before public node onboarding?
- Should the desktop app require a Beenet Cloud account, or support anonymous /
  token-only node enrollment?
- What telemetry is necessary for trust and abuse prevention, and what should
  remain local only?

## Suggested Milestones

1. Make worker build/check visible in Makefile and CI.
2. Introduce a small internal quota abstraction.
3. Implement Linux cgroup v2 backend first.
4. Add local daemon control API and status command.
5. Add Windows Job Object backend.
6. Add macOS conservative quota backend.
7. Build Tauri desktop control app.
8. Package native installers and service integration.

## macOS VM Follow-ups

- Build, sign, publish, and verify the minimal kernel/initrd/root-disk bundle.
- Add atomic image updates, rollback, and garbage collection.
- Package the existing guest init, Docker-built worker, and verified Alpine kernel as a signed
  release artifact instead of requiring users to run the developer image-build script.
- Add a guest agent or vsock health protocol so `status` reports worker readiness rather than
  only the vfkit process state.
- Add a one-time enrollment flow that passes bootstrap credentials over a temporary protected
  channel and never uses process arguments, kernel parameters, config logs, or image layers.
- Validate vfkit device syntax and boot behavior in macOS CI on both Apple Silicon and Intel,
  where supported.
- Define host-to-guest networking/DNS for control-plane URLs that currently use localhost.
