mod native;
mod vm;

#[cfg(target_os = "macos")]
use std::process::Command;

use anyhow::Result;
use beenet_common::config::{WorkerBackend, WorkerSettings};

pub fn validate(settings: &WorkerSettings) -> Result<()> {
    match settings.backend {
        WorkerBackend::Native => native::validate(settings),
        WorkerBackend::Vm => vm::validate(settings),
    }
}

#[cfg(target_os = "macos")]
pub fn vm_command(settings: &WorkerSettings, config_path: &std::path::Path) -> Result<Command> {
    vm::command(settings, config_path)
}

#[cfg(target_os = "macos")]
pub const VM_LAUNCH_AGENT_LABEL: &str = vm::LAUNCH_AGENT_LABEL;

#[cfg(target_os = "macos")]
pub struct VmLaunchAgentStatus {
    pub running: bool,
    pub pid: Option<u32>,
}

#[cfg(target_os = "macos")]
pub fn start_vm_launch_agent(
    settings: &WorkerSettings,
    config_path: &std::path::Path,
    worker_exe: &std::path::Path,
    working_directory: &std::path::Path,
    log_path: &std::path::Path,
) -> Result<()> {
    vm::start_launch_agent(
        settings,
        config_path,
        worker_exe,
        working_directory,
        log_path,
    )
}

#[cfg(target_os = "macos")]
pub fn stop_vm_launch_agent() -> Result<()> {
    vm::stop_launch_agent()
}

#[cfg(target_os = "macos")]
pub fn vm_launch_agent_status() -> Result<Option<VmLaunchAgentStatus>> {
    Ok(vm::inspect_launch_agent()?.map(|status| VmLaunchAgentStatus {
        running: status.running,
        pid: status.pid,
    }))
}
