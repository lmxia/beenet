#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use beenet_common::config::WorkerQuotaSettings;
use tracing::info;

/// Shown when CPU/memory/pids quota is configured but this process cannot write cgroup v2.
#[cfg(target_os = "linux")]
pub const LINUX_CGROUP_QUOTA_HINT: &str = "warning: [worker.quota] writes cgroup v2 (cpu.max / memory.max / pids.max) and needs sudo, or a systemd unit with Delegate=yes";

pub fn apply_os_quota(q: &WorkerQuotaSettings) -> Result<()> {
    if !quota_configured(q) {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        apply_linux_cgroup_v2(q).map_err(|err| {
            eprintln!("{LINUX_CGROUP_QUOTA_HINT}");
            err.context("Linux cgroup v2 quota is required; systemd must only start the process (Delegate=yes), bworker applies cpu/memory/pids")
        })?;
        apply_unix_nice(q)?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        apply_macos_quota(q)?;
        apply_unix_nice(q)?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        apply_windows_job_object(q)?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = q;
        anyhow::bail!("OS quota is currently supported on Linux, macOS, and Windows");
    }
}

/// Job Object `CpuRate` is tenths of a percent of the **whole machine**.
/// `cpu_percent` is a percent of **one** logical CPU (same as Linux `cpu.max`).
pub(crate) fn windows_cpu_rate(cpu_percent: u32, logical_cpus: u32) -> u32 {
    let n = u64::from(logical_cpus.max(1));
    let tenths = (u64::from(cpu_percent) * 10 + n / 2) / n;
    tenths.clamp(1, 10_000) as u32
}

fn quota_configured(q: &WorkerQuotaSettings) -> bool {
    q.cpu_percent.is_some() || q.memory_mb.is_some() || q.pids_max.is_some() || q.nice.is_some()
}

/// True when CPU/memory/pids quota is set and this process cannot write its cgroup subtree.
#[cfg(target_os = "linux")]
pub fn linux_cgroup_quota_needs_sudo(q: &WorkerQuotaSettings) -> bool {
    if q.cpu_percent.is_none() && q.memory_mb.is_none() && q.pids_max.is_none() {
        return false;
    }
    !cgroup_v2_subtree_writable()
}

#[cfg(target_os = "linux")]
fn cgroup_v2_subtree_writable() -> bool {
    let root = Path::new("/sys/fs/cgroup");
    if !root.join("cgroup.controllers").exists() {
        return false;
    }
    let Ok(current) = current_cgroup_v2_path(root) else {
        return false;
    };
    if fs::OpenOptions::new()
        .write(true)
        .open(current.join("cgroup.subtree_control"))
        .is_ok()
    {
        return true;
    }
    let probe = current.join(format!(".beenet-quota-probe-{}", std::process::id()));
    match fs::create_dir(&probe) {
        Ok(()) => {
            let _ = fs::remove_dir(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
fn apply_linux_cgroup_v2(q: &WorkerQuotaSettings) -> Result<()> {
    if q.cpu_percent.is_none() && q.memory_mb.is_none() && q.pids_max.is_none() {
        return Ok(());
    }

    let root = Path::new("/sys/fs/cgroup");
    if !root.join("cgroup.controllers").exists() {
        anyhow::bail!("Linux cgroup v2 is not mounted at /sys/fs/cgroup");
    }

    let current = current_cgroup_v2_path(root)?;
    let pid = std::process::id();
    let worker_cgroup = current.join(format!("beenet-worker-{pid}"));
    fs::create_dir_all(&worker_cgroup)
        .with_context(|| format!("create cgroup `{}`", worker_cgroup.display()))?;
    // cgroup v2 forbids enabling controllers while this cgroup still has
    // processes (EBUSY). Move out first, then enable on the parent, then
    // write limits on the child. systemd Delegate=yes hits this path.
    write_cgroup_file(&worker_cgroup, "cgroup.procs", &pid.to_string())?;
    enable_requested_controllers(&current, q)?;

    if let Some(memory_mb) = q.memory_mb {
        let bytes = (memory_mb as u64)
            .checked_mul(1024 * 1024)
            .context("worker quota memory_mb is too large")?;
        write_cgroup_file(&worker_cgroup, "memory.max", &bytes.to_string())?;
    }

    if let Some(cpu_percent) = q.cpu_percent {
        let period_us = 100_000u64;
        let quota_us = ((cpu_percent as u64) * period_us / 100).max(1);
        write_cgroup_file(
            &worker_cgroup,
            "cpu.max",
            &format!("{quota_us} {period_us}"),
        )?;
    }

    if let Some(pids_max) = q.pids_max {
        write_cgroup_file(&worker_cgroup, "pids.max", &pids_max.to_string())?;
    }

    info!(
        cgroup = %worker_cgroup.display(),
        cpu_percent = ?q.cpu_percent,
        memory_mb = ?q.memory_mb,
        pids_max = ?q.pids_max,
        "applied Linux cgroup v2 quota"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn enable_requested_controllers(parent: &Path, q: &WorkerQuotaSettings) -> Result<()> {
    let mut controllers = Vec::new();
    if q.cpu_percent.is_some() {
        controllers.push("+cpu");
    }
    if q.memory_mb.is_some() {
        controllers.push("+memory");
    }
    if q.pids_max.is_some() {
        controllers.push("+pids");
    }
    if controllers.is_empty() {
        return Ok(());
    }

    let available = fs::read_to_string(parent.join("cgroup.controllers"))
        .with_context(|| format!("read `{}` controllers", parent.display()))?;
    for controller in &controllers {
        let name = controller.trim_start_matches('+');
        if !available.split_whitespace().any(|item| item == name) {
            anyhow::bail!(
                "cgroup v2 controller `{name}` is not delegated to `{}`",
                parent.display()
            );
        }
    }

    fs::write(parent.join("cgroup.subtree_control"), controllers.join(" ")).with_context(|| {
        format!(
            "enable cgroup v2 controllers in `{}`; run the guest worker as root or delegate a writable cgroup subtree",
            parent.display()
        )
    })
}

#[cfg(target_os = "linux")]
fn current_cgroup_v2_path(root: &Path) -> Result<PathBuf> {
    let raw = fs::read_to_string("/proc/self/cgroup").context("read /proc/self/cgroup")?;
    for line in raw.lines() {
        if let Some(path) = line.strip_prefix("0::") {
            return Ok(root.join(path.trim_start_matches('/')));
        }
    }
    anyhow::bail!("current process is not in a cgroup v2 hierarchy")
}

#[cfg(target_os = "linux")]
fn write_cgroup_file(dir: &Path, name: &str, value: &str) -> Result<()> {
    let path = dir.join(name);
    fs::write(&path, value).with_context(|| format!("write `{}` = `{value}`", path.display()))
}

#[cfg(target_os = "macos")]
fn apply_macos_quota(q: &WorkerQuotaSettings) -> Result<()> {
    if q.cpu_percent.is_some() || q.memory_mb.is_some() || q.pids_max.is_some() {
        anyhow::bail!(
            "macOS native quota currently supports only nice; CPU, memory, and pids need Linux cgroup v2 or a future VM backend"
        );
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn apply_windows_job_object(q: &WorkerQuotaSettings) -> Result<()> {
    if q.cpu_percent.is_none() && q.memory_mb.is_none() && q.pids_max.is_none() {
        return Ok(());
    }
    windows_job::apply(q)
}

#[cfg(unix)]
fn apply_unix_nice(q: &WorkerQuotaSettings) -> Result<()> {
    let Some(nice) = q.nice else {
        return Ok(());
    };
    let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("apply worker nice priority");
    }
    info!(nice, "applied worker nice priority");
    Ok(())
}

#[cfg(target_os = "windows")]
mod windows_job {
    use super::*;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation,
        JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    static JOB: OnceLock<HANDLE> = OnceLock::new();

    pub(super) fn apply(q: &WorkerQuotaSettings) -> Result<()> {
        let job = *JOB.get_or_try_init(|| create_job())?;
        apply_limits(job, q)?;
        Ok(())
    }

    fn create_job() -> Result<HANDLE> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() || job == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32))
                .context("CreateJobObjectW");
        }
        let current = unsafe { GetCurrentProcess() };
        if unsafe { AssignProcessToJobObject(job, current) } == 0 {
            let err = std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32);
            unsafe { CloseHandle(job) };
            return Err(err).context(
                "AssignProcessToJobObject; this process may already be in a job that cannot nest",
            );
        }
        Ok(job)
    }

    fn apply_limits(job: HANDLE, q: &WorkerQuotaSettings) -> Result<()> {
        let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        ext.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        if let Some(pids_max) = q.pids_max {
            ext.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            ext.BasicLimitInformation.ActiveProcessLimit = pids_max;
        }
        if let Some(memory_mb) = q.memory_mb {
            let bytes = (memory_mb as usize)
                .checked_mul(1024 * 1024)
                .context("worker quota memory_mb is too large")?;
            ext.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            ext.JobMemoryLimit = bytes;
        }
        let ok = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &ext as *const _ as *const _,
                std::mem::size_of_val(&ext) as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32))
                .context("SetInformationJobObject extended limits");
        }

        if let Some(cpu_percent) = q.cpu_percent {
            let n = std::thread::available_parallelism()
                .map(|value| value.get() as u32)
                .unwrap_or(1);
            let mut cpu: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { std::mem::zeroed() };
            cpu.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            unsafe {
                cpu.Anonymous.CpuRate = super::windows_cpu_rate(cpu_percent, n);
            }
            let ok = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectCpuRateControlInformation,
                    &cpu as *const _ as *const _,
                    std::mem::size_of_val(&cpu) as u32,
                )
            };
            if ok == 0 {
                return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32))
                    .context("SetInformationJobObject CPU rate");
            }
        }

        info!(
            cpu_percent = ?q.cpu_percent,
            memory_mb = ?q.memory_mb,
            pids_max = ?q.pids_max,
            "applied Windows Job Object quota"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::windows_cpu_rate;

    #[test]
    fn windows_cpu_rate_is_one_cpu_share_of_the_machine() {
        assert_eq!(windows_cpu_rate(25, 1), 250);
        assert_eq!(windows_cpu_rate(25, 8), 31);
        assert_eq!(windows_cpu_rate(150, 8), 188);
        assert_eq!(windows_cpu_rate(100, 16), 63);
        assert_eq!(windows_cpu_rate(1, 64), 1);
    }
}
