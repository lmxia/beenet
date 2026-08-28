use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, Default)]
pub struct WorkerStatus {
    pub running: bool,
    pub heartbeat: bool,
    pub peer_id: Option<String>,
    pub name: Option<String>,
}

pub struct WorkerProcess;

impl WorkerProcess {
    pub fn locate_binary() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("BEENET_WORKER") {
            let path = PathBuf::from(path);
            if path.exists() {
                return Some(path);
            }
        }
        let exe = std::env::current_exe().ok()?;
        let sibling = exe.with_file_name(worker_file_name());
        if sibling.exists() {
            return Some(sibling);
        }
        None
    }

    pub fn run(
        config: &Path,
        command: &str,
        extra: &[&str],
        stdin: Option<&str>,
    ) -> Result<String, String> {
        let binary = Self::locate_binary().ok_or_else(|| {
            "找不到 beenet-worker.exe。请通过安装程序安装，或设置 BEENET_WORKER。".to_string()
        })?;
        let mut cmd = Command::new(&binary);
        cmd.args(extra)
            .arg("--config")
            .arg(config)
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", binary.display()))?;
        if let Some(stdin) = stdin {
            if let Some(mut handle) = child.stdin.take() {
                handle
                    .write_all(stdin.as_bytes())
                    .map_err(|error| error.to_string())?;
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("wait {}: {error}", binary.display()))?;
        let combined = [
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ]
        .join("")
        .trim()
        .to_string();
        if !output.status.success() {
            if combined.is_empty() {
                return Err(format!("beenet-worker {command} 失败"));
            }
            return Err(combined);
        }
        Ok(combined)
    }

    pub fn parse_enroll(text: &str) -> Option<String> {
        for line in text.lines() {
            let mut parts = line.splitn(2, ':');
            let key = parts.next()?.trim();
            let value = parts.next()?.trim();
            if key == "peer_id" && !value.is_empty() {
                return Some(value.to_string());
            }
        }
        None
    }
}

impl WorkerStatus {
    pub fn parse(text: &str) -> Self {
        let mut status = Self::default();
        for line in text.lines() {
            let mut parts = line.splitn(2, ':');
            let Some(key) = parts.next() else { continue };
            let Some(value) = parts.next() else { continue };
            let key = key.trim();
            let value = value.trim();
            match key {
                "running" => status.running = value == "true",
                "heartbeat" => status.heartbeat = value == "true",
                "peer_id" => {
                    if !value.is_empty() {
                        status.peer_id = Some(value.to_string());
                    }
                }
                "name" => {
                    if !value.is_empty() {
                        status.name = Some(value.to_string());
                    }
                }
                _ => {}
            }
        }
        status
    }
}

fn worker_file_name() -> &'static str {
    if cfg!(windows) {
        "beenet-worker.exe"
    } else {
        "beenet-worker"
    }
}
