# Windows worker: Job Objects

Status: implemented in `beenet-worker` (`quota.rs` Job Objects + process start/stop).
The shipping package is Inno Setup `BeenetSetup-x64.exe`, not a zip.

Product decision: Windows is a **native** `beenet-worker.exe`, same role as
Linux, not a Hyper-V copy of the macOS vfkit guest. The OS second boundary is a
[Job Object](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
that the daemon assigns **itself** into at start (and any children it creates).
The Windows Service / installer must not set CPU or memory limits; it only
starts the process, the same way systemd uses `Delegate=yes` and lets bworker
write cgroup v2.

Wasmtime per-instance memory and deadline stay the first boundary. This page
only maps `[worker.quota]` onto Job Object fields.

## Product fields (unchanged)

```toml
[worker.quota]
cpu_percent = 25   # percent of one logical CPU
memory_mb = 512    # whole-worker cap
pids_max = 128     # whole-worker cap; see process vs thread below
```

Same numbers as Linux `get-bworker.sh` and the Mac App presets. `nice` is UNIX
priority and is **not** mapped on Windows v1.

If any of `cpu_percent` / `memory_mb` / `pids_max` is set and the Job Object
cannot be created or assigned, **start fails**.

## Mapping

### `cpu_percent` → `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION`

Linux: `cpu.max = (cpu_percent * 100000 / 100) 100000` with a 100 ms period.
That is a cap in **one-CPU units** (25 → quarter core, 150 → 1.5 cores).

Windows `CpuRate` is **tenths of a percent of the whole machine**, range 1–10000
(10000 = 100% of every logical processor combined). Flags:

- `JOB_OBJECT_CPU_RATE_CONTROL_ENABLE`
- `JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP` (a cap, not a scheduler weight)

```text
N        = GetActiveProcessorCount(ALL_PROCESSOR_GROUPS)   # at least 1
CpuRate  = clamp(round(cpu_percent * 10 / N), 1, 10000)
```

Examples:

| `cpu_percent` | Logical CPUs | `CpuRate` | Effective |
| --- | --- | --- | --- |
| 25 | 1 | 250 | 25% of the only CPU (matches Linux) |
| 25 | 8 | 31 | ≈ 3.1% of the machine ≈ 0.25 CPU |
| 150 | 8 | 188 | ≈ 18.8% of the machine ≈ 1.50 CPU |
| 100 | 16 | 63 | 6.3% of the machine ≈ 1.01 CPU |

Round to nearest integer; never emit 0 (API rejects it). If `cpu_percent * 10`
is smaller than `N`, the cap becomes the minimum 0.1% of the machine — document
that very small budgets on many-core boxes are coarse.

Do not use `JOB_OBJECT_CPU_RATE_CONTROL_WEIGHT_BASED`. Weight-sharing is not a
hard budget.

### `memory_mb` → `JobMemoryLimit`

Linux: `memory.max = memory_mb * 1024 * 1024` on the worker cgroup (RSS/cgroup
memory, not a vfkit envelope).

Windows: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION.JobMemoryLimit` with
`JOB_OBJECT_LIMIT_JOB_MEMORY`.

```text
JobMemoryLimit = memory_mb * 1024 * 1024
```

This is commit charge for the **entire job** (the worker plus children), which
is the right analogue of whole-worker `memory.max`.

Do **not** set `JOB_OBJECT_LIMIT_PROCESS_MEMORY` as the primary cap: one
process is the usual shape, but children must share the same budget.

Do **not** add the macOS VM 320 MB guest headroom. That RAM is for the Alpine
kernel; a native Windows process does not need it. `memory_mb=512` is 512 MiB
on Linux native and Windows native, and 512 MiB **cgroup** + 832 MiB vfkit RAM
on Mac.

When the job is over the limit, further allocations fail with
`ERROR_NOT_ENOUGH_QUOTA` rather than a Linux-style OOM kill. The worker should
surface that as invoke failures, not hang.

### `pids_max` → `ActiveProcessLimit` (processes only)

Linux `pids.max` counts **PIDs**, including threads.

A Job Object has `JOBOBJECT_BASIC_LIMIT_INFORMATION.ActiveProcessLimit` with
`JOB_OBJECT_LIMIT_ACTIVE_PROCESS`. That is **processes**, not threads. There is
no Job Object field for a thread cap.

Beenet’s worker is one process with many Tokio/Wasmtime threads. Mapping:

```text
ActiveProcessLimit = pids_max
```

Use the same integer the user set (25% preset → 128). Meaning on Windows:

- Caps how many OS processes may exist in the job (fork/spawn bomb).
- Does **not** cap threads. Thread growth is bounded by `cpu_percent` and
  `memory_mb`.

Do not special-case `ActiveProcessLimit = 1`. That would silently ignore a user
value of 128 and break any future helper child. The daemon today does not need
128 processes; leaving headroom is fine.

If a true thread cap is needed later, it is an in-process runtime limit, not a
Job Object.

### `nice`

Skip on Windows v1. Optional later: `BELOW_NORMAL_PRIORITY_CLASS` when
`nice > 0`. Do not require `SeIncreaseBasePriorityPrivilege` for v1.

## Job flags besides quota

Set these whenever a quota job is created, even if only one of the three
fields is present:

| Flag | Why |
| --- | --- |
| `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` | Closing the handle kills the tree (crash of a wrapper must not leak workers). |
| `JOB_OBJECT_LIMIT_BREAKAWAY_OK` **off** | Children cannot leave the job. |
| `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION` | Avoid hung crashed children. |
| `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` already implies the job owns the lifetime | Assign **this** process with `AssignProcessToJobObject`. |

Create the job, then `AssignProcessToJobObject(job, GetCurrentProcess())`, then
apply CPU/memory/pids info. Order matches Linux: enter the cgroup, then write
limits.

A process may already be in a job (installer, debugger, some AV). On Windows 8+
try a **nested** job (`JOB_OBJECT_LIMIT_BREAKAWAY_OK` still off on the nested
job). If nested jobs are unavailable or assign fails, fail start with a message
that the process is already in a non-nestable job.

## Who creates the job

```text
Windows Service / Task Scheduler / tray
        starts beenet-worker.exe
                |
                v
        beenet-worker creates Job Object, assigns self, applies quota
```

Do not put CPU rate or `JobMemoryLimit` on the service definition. If SCM or a
wrapper must use a job, it should be empty of quota so the worker can nest.

## Failure and observability

- Log the resolved `CpuRate`, `N`, `JobMemoryLimit`, and `ActiveProcessLimit`
  at info, analogous to `applied Linux cgroup v2 quota`.
- `beenet-worker status` should print `backend: native` and the same
  `cpu_percent` / `memory_mb` / `pids_max` the config asked for, plus a line if
  the job is active (query `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`).
- Cloud online remains registry heartbeat, unchanged.

## Packaging (follow-on, not this mapping)

- `%APPDATA%\beenet\config.toml`, `%LOCALAPPDATA%\beenet\wasm_cache`
- Signed zip first; MSI + Windows Service later
- CI: `windows-2022` `cargo check -p beenet-worker`, then Job Object tests

## Not in v1

- Hyper-V / WHP Alpine guest (`backend=vm`)
- WSL2
- Mapping `cpu_percent` onto vCPU count (that is only the Mac vfkit envelope)
- Per-invoke Job Objects
