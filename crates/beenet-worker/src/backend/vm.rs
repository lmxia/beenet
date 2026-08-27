#[cfg(target_os = "macos")]
use std::fs;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::Result;
use beenet_common::config::WorkerSettings;

#[cfg(target_os = "macos")]
const CONFIG_MOUNT_TAG: &str = "beenet-config";
#[cfg(target_os = "macos")]
const STATE_MOUNT_TAG: &str = "beenet-state";

#[cfg(target_os = "macos")]
pub(crate) const LAUNCH_AGENT_LABEL: &str = "com.beenet.worker";
#[cfg(target_os = "macos")]
const LEGACY_LAUNCH_AGENT_LABEL: &str = "com.beenet.worker-hk";

pub(super) fn validate(_settings: &WorkerSettings) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("worker backend=vm is currently supported only on macOS hosts");

    #[cfg(target_os = "macos")]
    {
        required_file(_settings.vm.kernel_path.as_deref(), "worker.vm.kernel_path")?;
        if let Some(path) = _settings.vm.root_disk_path.as_deref() {
            ensure_file(path, "worker.vm.root_disk_path")?;
        }
        if let Some(path) = _settings.vm.initrd_path.as_deref() {
            ensure_file(path, "worker.vm.initrd_path")?;
        }
        find_executable(&_settings.vm.vfkit_path).with_context(|| {
            format!(
                "vfkit `{}` is not executable; install vfkit or set worker.vm.vfkit_path",
                _settings.vm.vfkit_path.display()
            )
        })?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub(super) fn command(settings: &WorkerSettings, config_path: &Path) -> Result<Command> {
    validate(settings)?;
    let config_path = fs::canonicalize(config_path)
        .with_context(|| format!("canonicalize config `{}`", config_path.display()))?;
    let config_dir = config_path
        .parent()
        .context("worker config path has no parent directory")?;
    let config_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("worker config filename is not valid UTF-8")?;
    let state_dir = canonicalize_or_create(&settings.wasm_cache_dir)?;
    reject_vfkit_delimiter(config_dir)?;
    reject_vfkit_delimiter(&state_dir)?;
    if let Some(root_disk) = settings.vm.root_disk_path.as_deref() {
        reject_vfkit_delimiter(root_disk)?;
    }

    let mut cmd = Command::new(&settings.vm.vfkit_path);
    cmd.arg("--cpus")
        .arg(settings.vm.cpus.to_string())
        .arg("--memory")
        .arg(settings.vm.memory_mb.to_string())
        .arg("--kernel")
        .arg(settings.vm.kernel_path.as_ref().expect("validated kernel"));
    if let Some(initrd) = settings.vm.initrd_path.as_ref() {
        cmd.arg("--initrd").arg(initrd);
    }
    cmd.arg("--kernel-cmdline")
        .arg(format!("console=hvc0 beenet.config={config_name}"));
    if let Some(root_disk) = settings.vm.root_disk_path.as_ref() {
        cmd.arg("--device")
            .arg(format!("virtio-blk,path={}", root_disk.display()));
    }
    cmd.arg("--device")
        .arg("virtio-net,nat")
        .arg("--device")
        .arg(format!(
            "virtio-fs,sharedDir={},mountTag={CONFIG_MOUNT_TAG}",
            config_dir.display()
        ))
        .arg("--device")
        .arg(format!(
            "virtio-fs,sharedDir={},mountTag={STATE_MOUNT_TAG}",
            state_dir.display()
        ));
    Ok(cmd)
}

#[cfg(target_os = "macos")]
fn required_file<'a>(path: Option<&'a Path>, setting: &str) -> Result<&'a Path> {
    let path = path.with_context(|| format!("{setting} is required for backend=vm"))?;
    ensure_file(path, setting)?;
    Ok(path)
}

#[cfg(target_os = "macos")]
fn ensure_file(path: &Path, setting: &str) -> Result<()> {
    if !path.is_file() {
        anyhow::bail!("{setting} `{}` is not a file", path.display());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn find_executable(program: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    if program.components().count() > 1 {
        let metadata = fs::metadata(program)?;
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Ok(program.to_path_buf());
        }
    } else if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(program);
            if candidate
                .metadata()
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
            {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!("executable not found")
}

#[cfg(target_os = "macos")]
fn canonicalize_or_create(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path).with_context(|| format!("create `{}`", path.display()))?;
    fs::canonicalize(path).with_context(|| format!("canonicalize `{}`", path.display()))
}

#[cfg(target_os = "macos")]
fn reject_vfkit_delimiter(path: &Path) -> Result<()> {
    if path.as_os_str().to_string_lossy().contains(',') {
        anyhow::bail!(
            "vfkit shared directory `{}` cannot contain a comma",
            path.display()
        );
    }
    Ok(())
}

/// Generate a launchd LaunchAgent plist for the host supervisor.
///
/// `KeepAlive` is unconditional: guest poweroff is a successful vfkit exit, and
/// launchd must still restart the VM. `ProgramArguments` must use `run-internal`
/// so launchd supervises the foreground process after `exec vfkit`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn launch_agent_plist(
    label: &str,
    worker_exe: &Path,
    config_path: &Path,
    working_directory: &Path,
    log_path: &Path,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--config</string>
    <string>{config}</string>
    <string>run-internal</string>
  </array>
  <key>WorkingDirectory</key>
  <string>{cwd}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>RUST_LOG</key>
    <string>info</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        exe = xml_escape(&worker_exe.display().to_string()),
        config = xml_escape(&config_path.display().to_string()),
        cwd = xml_escape(&working_directory.display().to_string()),
        log = xml_escape(&log_path.display().to_string()),
    )
}

#[allow(dead_code)]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) struct LaunchAgentRuntime {
    pub running: bool,
    pub pid: Option<u32>,
}

#[cfg(target_os = "macos")]
pub(crate) fn start_launch_agent(
    settings: &WorkerSettings,
    config_path: &Path,
    worker_exe: &Path,
    working_directory: &Path,
    log_path: &Path,
) -> Result<()> {
    validate(settings)?;
    retire_legacy_launch_agent();
    unload_launch_agent(LAUNCH_AGENT_LABEL)?;

    let plist_path = launch_agent_path(LAUNCH_AGENT_LABEL)?;
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create `{}`", parent.display()))?;
    }
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create `{}`", parent.display()))?;
    }

    let config_path = fs::canonicalize(config_path)
        .with_context(|| format!("canonicalize config `{}`", config_path.display()))?;
    let worker_exe = fs::canonicalize(worker_exe)
        .with_context(|| format!("canonicalize worker `{}`", worker_exe.display()))?;
    let working_directory = fs::canonicalize(working_directory).with_context(|| {
        format!(
            "canonicalize working directory `{}`",
            working_directory.display()
        )
    })?;
    fs::write(
        &plist_path,
        launch_agent_plist(
            LAUNCH_AGENT_LABEL,
            &worker_exe,
            &config_path,
            &working_directory,
            log_path,
        ),
    )
    .with_context(|| format!("write `{}`", plist_path.display()))?;
    enable_launch_agent(LAUNCH_AGENT_LABEL)?;
    bootstrap_launch_agent(&plist_path)?;
    kickstart_launch_agent(LAUNCH_AGENT_LABEL)?;
    println!("worker launch agent {LAUNCH_AGENT_LABEL} started");
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn stop_launch_agent() -> Result<()> {
    retire_legacy_launch_agent();
    if inspect_launch_agent()?.is_none() {
        println!("worker is not running");
        return Ok(());
    }
    unload_launch_agent(LAUNCH_AGENT_LABEL)?;
    println!("worker stopped");
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn inspect_launch_agent() -> Result<Option<LaunchAgentRuntime>> {
    inspect_label(LAUNCH_AGENT_LABEL)
}

#[cfg(target_os = "macos")]
fn retire_legacy_launch_agent() {
    let _ = unload_launch_agent(LEGACY_LAUNCH_AGENT_LABEL);
    if let Ok(path) = launch_agent_path(LEGACY_LAUNCH_AGENT_LABEL) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(target_os = "macos")]
fn launch_agent_path(label: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

#[cfg(target_os = "macos")]
fn service_target(label: &str) -> String {
    format!("gui/{}/{label}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
fn inspect_label(label: &str) -> Result<Option<LaunchAgentRuntime>> {
    let output = Command::new("launchctl")
        .args(["print", &service_target(label)])
        .stderr(Stdio::null())
        .output()
        .context("launchctl print")?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(parse_launchctl_print(&String::from_utf8_lossy(
        &output.stdout,
    ))))
}

fn parse_launchctl_print(text: &str) -> LaunchAgentRuntime {
    let running = text.lines().any(|line| line.trim() == "state = running");
    let pid = text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|pid| pid.parse().ok())
    });
    LaunchAgentRuntime { running, pid }
}

#[cfg(target_os = "macos")]
fn run_launchctl(args: &[&str]) -> Result<(bool, String)> {
    let output = Command::new("launchctl")
        .args(args)
        .output()
        .with_context(|| format!("launchctl {}", args.join(" ")))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let err = String::from_utf8_lossy(&output.stderr);
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    Ok((output.status.success(), text))
}

fn launchctl_already_gone(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("no such process") || lower.contains("could not find service")
}

#[cfg(target_os = "macos")]
fn disable_launch_agent(label: &str) -> Result<()> {
    let (ok, text) = run_launchctl(&["disable", &service_target(label)])?;
    if ok || launchctl_already_gone(&text) {
        return Ok(());
    }
    anyhow::bail!(
        "launchctl disable {} failed: {}",
        service_target(label),
        text.trim()
    );
}

#[cfg(target_os = "macos")]
fn enable_launch_agent(label: &str) -> Result<()> {
    let (ok, text) = run_launchctl(&["enable", &service_target(label)])?;
    if ok || launchctl_already_gone(&text) {
        return Ok(());
    }
    anyhow::bail!(
        "launchctl enable {} failed: {}",
        service_target(label),
        text.trim()
    );
}

#[cfg(target_os = "macos")]
fn bootout_launch_agent(label: &str) -> Result<()> {
    let (ok, text) = run_launchctl(&["bootout", &service_target(label)])?;
    if ok || launchctl_already_gone(&text) {
        return Ok(());
    }
    anyhow::bail!(
        "launchctl bootout {} failed: {}",
        service_target(label),
        text.trim()
    );
}

#[cfg(target_os = "macos")]
fn wait_until_unloaded(label: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if inspect_label(label)?.is_none() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "launch agent {label} is still loaded after bootout; wait for the VM to power off and try again"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(target_os = "macos")]
fn unload_launch_agent(label: &str) -> Result<()> {
    if inspect_label(label)?.is_none() {
        return Ok(());
    }
    let _ = disable_launch_agent(label);
    bootout_launch_agent(label)?;
    wait_until_unloaded(label)
}

#[cfg(target_os = "macos")]
fn bootstrap_launch_agent(plist_path: &Path) -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let (ok, text) = run_launchctl(&["bootstrap", &domain, &plist_path.display().to_string()])?;
    if ok {
        return Ok(());
    }
    anyhow::bail!(
        "launchctl bootstrap `{}` failed: {}",
        plist_path.display(),
        text.trim()
    );
}

#[cfg(target_os = "macos")]
fn kickstart_launch_agent(label: &str) -> Result<()> {
    let (ok, text) = run_launchctl(&["kickstart", "-k", &service_target(label)])?;
    if ok {
        return Ok(());
    }
    anyhow::bail!(
        "launchctl kickstart {} failed: {}",
        service_target(label),
        text.trim()
    );
}

#[cfg(test)]
mod tests {
    use super::launch_agent_plist;
    use std::path::Path;

    const ALPINE_INIT: &str = include_str!("../../../../vm/alpine-init");

    #[test]
    fn guest_init_powers_off_via_sysrq_and_never_reboots() {
        assert!(
            ALPINE_INIT.contains("trap power_off EXIT"),
            "PID 1 must power off on any exit"
        );
        assert!(
            ALPINE_INIT.contains("/proc/sysrq-trigger"),
            "Alpine virt busybox has no poweroff applet; sysrq is required"
        );
        assert!(
            ALPINE_INIT.contains("printf o"),
            "sysrq 'o' requests poweroff"
        );
        assert!(
            !ALPINE_INIT.contains("reboot -f") && !ALPINE_INIT.contains("busybox reboot"),
            "guest reboot would keep vfkit alive and skip launchd KeepAlive"
        );
        assert!(ALPINE_INIT.contains("wait \"$worker_pid\""));
        assert!(ALPINE_INIT.contains("BEENET_VM_GUEST=1"));
    }

    #[test]
    fn launch_agent_keeps_alive_after_successful_vfkit_exit() {
        let plist = launch_agent_plist(
            "com.beenet.worker",
            Path::new("/opt/beenet/beenet-worker"),
            Path::new("/opt/beenet/config.toml"),
            Path::new("/opt/beenet"),
            Path::new("/opt/beenet/logs/worker.log"),
        );
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains("<string>run-internal</string>"));
        assert!(plist.contains("/opt/homebrew/bin:"));
        assert!(!plist.contains("<string>start</string>"));
        assert!(!plist.contains("SuccessfulExit"));
        assert!(plist.contains("<string>/opt/beenet/beenet-worker</string>"));
        assert!(plist.contains("<string>/opt/beenet/config.toml</string>"));
    }

    #[test]
    fn launch_agent_escapes_xml_in_paths() {
        let plist = launch_agent_plist(
            "com.beenet.worker",
            Path::new("/tmp/beenet & worker"),
            Path::new("/tmp/a<b>.toml"),
            Path::new("/tmp"),
            Path::new("/tmp/out.log"),
        );
        assert!(plist.contains("/tmp/beenet &amp; worker"));
        assert!(plist.contains("/tmp/a&lt;b&gt;.toml"));
    }

    #[test]
    fn launchctl_sigtermed_is_loaded_but_not_running() {
        let status = super::parse_launchctl_print(
            "\tstate = SIGTERMed\n\tpid = 57906\n\tactive count = 1\n",
        );
        assert!(!status.running);
        assert_eq!(status.pid, Some(57906));
        let running = super::parse_launchctl_print("\tstate = running\n\tpid = 12\n");
        assert!(running.running);
        assert_eq!(running.pid, Some(12));
    }
}
