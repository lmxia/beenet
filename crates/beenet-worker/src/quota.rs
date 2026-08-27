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

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = q;
        anyhow::bail!("OS quota is currently supported on Linux and macOS only");
    }
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
