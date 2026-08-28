#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::{Instant, SystemTime};

#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::Result;
use beenet_common::config::WorkerSettings;
#[cfg(target_os = "macos")]
use tracing::{info, warn};

#[cfg(target_os = "macos")]
const CONFIG_MOUNT_TAG: &str = "beenet-config";
#[cfg(target_os = "macos")]
const STATE_MOUNT_TAG: &str = "beenet-state";

#[cfg(target_os = "macos")]
pub(crate) const LAUNCH_AGENT_LABEL: &str = "com.beenet.worker";
#[cfg(target_os = "macos")]
const LEGACY_LAUNCH_AGENT_LABEL: &str = "com.beenet.worker-hk";

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub(crate) const VFKIT_PID_FILE: &str = "vfkit.pid";

/// Host-side NAT recovery after sleep or network change.
///
/// Wall-clock durations: `Instant` pauses across macOS sleep, which would keep
/// a just-spawned VM inside `boot_grace` after the user wakes and reconnects.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub(crate) struct NatRebuildPolicy {
    pub stale_after: Duration,
    pub boot_grace: Duration,
    pub restart_cooldown: Duration,
    pub cooldown_cap: Duration,
}

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
impl NatRebuildPolicy {
    pub const DEFAULT: Self = Self {
        stale_after: Duration::from_secs(90),
        boot_grace: Duration::from_secs(90),
        restart_cooldown: Duration::from_secs(60),
        cooldown_cap: Duration::from_secs(900),
    };

    fn cooldown(&self, rebuild_failures: u32) -> Duration {
        let shift = rebuild_failures.min(4);
        Duration::from_secs(
            self.restart_cooldown
                .as_secs()
                .saturating_mul(1u64 << shift),
        )
        .min(self.cooldown_cap)
    }
}

/// Rebuild vfkit NAT when the Mac can reach the registry but the guest heartbeat
/// is stale. Do not rebuild while the host itself is offline, or inside boot grace.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub(crate) fn should_rebuild_nat(
    heartbeat_age: Option<Duration>,
    time_since_spawn: Duration,
    time_since_rebuild: Option<Duration>,
    rebuild_failures: u32,
    host_registry_reachable: bool,
    policy: &NatRebuildPolicy,
) -> bool {
    if !host_registry_reachable {
        return false;
    }
    if time_since_spawn < policy.boot_grace {
        return false;
    }
    let cooldown = policy.cooldown(rebuild_failures);
    if time_since_rebuild.is_some_and(|elapsed| elapsed < cooldown) {
        return false;
    }
    match heartbeat_age {
        None => true,
        Some(age) => age >= policy.stale_after,
    }
}

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub(crate) fn vfkit_pid_path(wasm_cache_dir: &Path) -> PathBuf {
    wasm_cache_dir.join(VFKIT_PID_FILE)
}

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub(crate) fn parse_registry_host_port(registry_url: &str) -> Option<(String, u16)> {
    let url = registry_url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split('/').next()?.split('@').next_back()?;
    let default_port = if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        80
    };
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, port_part) = rest.split_once(']')?;
        let port = port_part
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
        return Some((host.to_string(), port));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if !host.is_empty() && host.chars().all(|c| c.is_ascii_digit() || c == '.') {
            // IPv4:port
            let port = port.parse().ok()?;
            return Some((host.to_string(), port));
        }
        if host.contains(':') {
            return None;
        }
        if let Ok(port) = port.parse::<u16>() {
            return Some((host.to_string(), port));
        }
    }
    if authority.is_empty() {
        return None;
    }
    Some((authority.to_string(), default_port))
}

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

#[cfg(target_os = "macos")]
static STOP_SUPERVISOR: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
extern "C" fn handle_supervisor_stop(_: libc::c_int) {
    STOP_SUPERVISOR.store(true, Ordering::SeqCst);
}

/// Stay in the foreground and respawn vfkit. launchd KeepAlive watches this
/// supervisor; guest poweroff still exits vfkit, and this loop starts a new VM.
/// Sleep/lock kills virtio-net NAT without exiting vfkit — rebuild when the host
/// can reach the registry but the guest heartbeat file is stale.
#[cfg(target_os = "macos")]
pub(crate) fn supervise(settings: &WorkerSettings, config_path: &Path) -> Result<()> {
    STOP_SUPERVISOR.store(false, Ordering::SeqCst);
    install_stop_handler();

    let policy = NatRebuildPolicy::DEFAULT;
    let pid_path = vfkit_pid_path(&settings.wasm_cache_dir);
    let mut rebuild_failures: u32 = 0;
    let mut last_rebuild_at: Option<SystemTime> = None;

    loop {
        if STOP_SUPERVISOR.load(Ordering::SeqCst) {
            let _ = fs::remove_file(&pid_path);
            return Ok(());
        }

        let mut cmd = command(settings, config_path)?;
        cmd.stdin(Stdio::null());
        info!(
            vfkit = %settings.vm.vfkit_path.display(),
            cpus = settings.vm.cpus,
            memory_mb = settings.vm.memory_mb,
            "starting Beenet Linux microVM"
        );
        let mut child = cmd.spawn().context("spawn vfkit")?;
        let spawned_at = SystemTime::now();
        write_vfkit_pid(&pid_path, child.id())?;

        loop {
            if STOP_SUPERVISOR.load(Ordering::SeqCst) {
                terminate_child(&mut child);
                let _ = fs::remove_file(&pid_path);
                return Ok(());
            }

            match child.try_wait().context("wait vfkit")? {
                Some(status) => {
                    let _ = fs::remove_file(&pid_path);
                    if STOP_SUPERVISOR.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    warn!(?status, "vfkit exited; launching a new VM");
                    std::thread::sleep(Duration::from_secs(1));
                    break;
                }
                None => {}
            }

            let heartbeat_age = beenet_common::display_name::registry_heartbeat_age_secs(
                &settings.wasm_cache_dir,
            )
            .map(Duration::from_secs);
            if heartbeat_age.is_some_and(|age| age < policy.stale_after) {
                rebuild_failures = 0;
            }

            let time_since_spawn = wall_elapsed(spawned_at);
            let time_since_rebuild = last_rebuild_at.map(wall_elapsed);
            let host_reachable = host_can_reach_registry(&settings.registry_url, Duration::from_secs(3));
            if should_rebuild_nat(
                heartbeat_age,
                time_since_spawn,
                time_since_rebuild,
                rebuild_failures,
                host_reachable,
                &policy,
            ) {
                warn!(
                    heartbeat_age_secs = heartbeat_age.map(|age| age.as_secs()),
                    "host can reach registry but guest heartbeat is stale; rebuilding vfkit NAT"
                );
                terminate_child(&mut child);
                let _ = fs::remove_file(&pid_path);
                last_rebuild_at = Some(SystemTime::now());
                rebuild_failures = rebuild_failures.saturating_add(1);
                break;
            }

            interruptible_sleep(Duration::from_secs(5));
            crate::log_rotate::tick(&settings.wasm_cache_dir);
        }
    }
}

#[cfg(target_os = "macos")]
fn install_stop_handler() {
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_supervisor_stop as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            handle_supervisor_stop as *const () as libc::sighandler_t,
        );
    }
}

#[cfg(target_os = "macos")]
fn interruptible_sleep(total: Duration) {
    let mut waited = Duration::ZERO;
    while waited < total {
        if STOP_SUPERVISOR.load(Ordering::SeqCst) {
            return;
        }
        let slice = Duration::from_millis(200).min(total - waited);
        std::thread::sleep(slice);
        waited += slice;
    }
}

#[cfg(target_os = "macos")]
fn wall_elapsed(since: SystemTime) -> Duration {
    SystemTime::now()
        .duration_since(since)
        .unwrap_or(Duration::from_secs(0))
}

#[cfg(target_os = "macos")]
fn write_vfkit_pid(path: &Path, pid: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create `{}`", parent.display()))?;
    }
    fs::write(path, format!("{pid}\n")).with_context(|| format!("write `{}`", path.display()))
}

#[cfg(target_os = "macos")]
fn terminate_child(child: &mut Child) {
    let pid = child.id() as i32;
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_os = "macos")]
fn host_can_reach_registry(registry_url: &str, timeout: Duration) -> bool {
    let Some((host, port)) = parse_registry_host_port(registry_url) else {
        return false;
    };
    let Ok(mut addrs) = (host.as_str(), port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr: SocketAddr| TcpStream::connect_timeout(&addr, timeout).is_ok())
}

/// Generate a launchd LaunchAgent plist for the host supervisor.
///
/// `KeepAlive` is unconditional so launchd restarts this supervisor if it
/// exits. The supervisor itself respawns vfkit on guest poweroff and when
/// host networking is back but the guest heartbeat has gone stale.
/// `ProgramArguments` must use `run-internal` (not `start`).
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

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
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

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
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

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
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
    use std::time::Duration;

    const ALPINE_INIT: &str = include_str!("../../../../deploy/macos-contributor/guest/alpine-init");

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

    #[test]
    fn parse_registry_host_port_defaults_and_overrides() {
        assert_eq!(
            super::parse_registry_host_port("http://registry.hyperos.online"),
            Some(("registry.hyperos.online".into(), 80))
        );
        assert_eq!(
            super::parse_registry_host_port("https://example.com/api"),
            Some(("example.com".into(), 443))
        );
        assert_eq!(
            super::parse_registry_host_port("http://127.0.0.1:3030/v1"),
            Some(("127.0.0.1".into(), 3030))
        );
        assert_eq!(
            super::parse_registry_host_port("http://[::1]:8080/"),
            Some(("::1".into(), 8080))
        );
    }

    #[test]
    fn rebuild_nat_when_host_is_online_and_heartbeat_is_stale() {
        let policy = super::NatRebuildPolicy::DEFAULT;
        let stale = Some(policy.stale_after + Duration::from_secs(1));
        let spawned = policy.boot_grace + Duration::from_secs(1);
        assert!(super::should_rebuild_nat(
            stale, spawned, None, 0, true, &policy
        ));
        assert!(
            !super::should_rebuild_nat(stale, spawned, None, 0, false, &policy),
            "Mac still offline: do not recycle the VM"
        );
        assert!(
            !super::should_rebuild_nat(
                stale,
                Duration::from_secs(10),
                None,
                0,
                true,
                &policy
            ),
            "guest still booting"
        );
        assert!(!super::should_rebuild_nat(
            Some(Duration::from_secs(5)),
            spawned,
            None,
            0,
            true,
            &policy
        ));
        assert!(
            !super::should_rebuild_nat(
                stale,
                spawned,
                Some(Duration::from_secs(10)),
                0,
                true,
                &policy
            ),
            "cooldown after a rebuild"
        );
        assert!(
            super::should_rebuild_nat(None, spawned, None, 0, true, &policy),
            "no heartbeat file after grace means the guest never came online"
        );
    }

    #[test]
    fn rebuild_nat_backoff_lengthens_cooldown() {
        let policy = super::NatRebuildPolicy::DEFAULT;
        let stale = Some(policy.stale_after);
        let spawned = policy.boot_grace;
        assert!(!super::should_rebuild_nat(
            stale,
            spawned,
            Some(Duration::from_secs(90)),
            1,
            true,
            &policy
        ));
        assert!(super::should_rebuild_nat(
            stale,
            spawned,
            Some(Duration::from_secs(120)),
            1,
            true,
            &policy
        ));
    }
}
