use anyhow::Result;
use beenet_common::config::WorkerSettings;

pub(super) fn validate(_settings: &WorkerSettings) -> Result<()> {
    #[cfg(target_os = "macos")]
    if _settings.quota.cpu_percent.is_some()
        || _settings.quota.memory_mb.is_some()
        || _settings.quota.pids_max.is_some()
    {
        anyhow::bail!(
            "macOS native backend supports only quota.nice; set [worker] backend = \"vm\" \
             to enforce cpu_percent, memory_mb, or pids_max with Linux cgroup v2"
        );
    }
    Ok(())
}
