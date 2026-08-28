//! Size-cap worker logs that launchd / shell redirects keep appending forever.
//!
//! Copy the tail aside, then truncate the live file in place so existing fds
//! (LaunchAgent `StandardOutPath`, Alpine `>> worker.log`) keep writing.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use tracing::info;

pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
pub const KEEP: usize = 3;

pub fn host_log_path(wasm_cache_dir: &Path) -> PathBuf {
    wasm_cache_dir
        .parent()
        .unwrap_or(wasm_cache_dir)
        .join("logs")
        .join("beenet-worker.log")
}

pub fn guest_log_path(wasm_cache_dir: &Path) -> PathBuf {
    wasm_cache_dir.join("logs").join("worker.log")
}

pub fn tick(wasm_cache_dir: &Path) {
    for path in [
        host_log_path(wasm_cache_dir),
        guest_log_path(wasm_cache_dir),
    ] {
        match rotate_if_needed(&path, MAX_BYTES, KEEP) {
            Ok(true) => info!(log = %path.display(), max_bytes = MAX_BYTES, keep = KEEP, "rotated worker log"),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(log = %path.display(), %error, "worker log rotation failed");
            }
        }
    }
}

pub fn read_tail(path: &Path, max_bytes: u64) -> io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes))?;
    }
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    Ok(buf)
}

fn rotate_if_needed(path: &Path, max_bytes: u64, keep: usize) -> io::Result<bool> {
    let Ok(meta) = fs::metadata(path) else {
        return Ok(false);
    };
    if !meta.is_file() || meta.len() < max_bytes {
        return Ok(false);
    }
    if keep > 0 {
        let last = backup_path(path, keep);
        let _ = fs::remove_file(&last);
        for i in (1..keep).rev() {
            let from = backup_path(path, i);
            let to = backup_path(path, i + 1);
            if from.exists() {
                fs::rename(&from, &to)?;
            }
        }
        copy_tail(path, &backup_path(path, 1), max_bytes)?;
    }
    OpenOptions::new().write(true).open(path)?.set_len(0)?;
    Ok(true)
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

fn copy_tail(src: &Path, dst: &Path, max_bytes: u64) -> io::Result<()> {
    let mut input = File::open(src)?;
    let len = input.metadata()?.len();
    if len > max_bytes {
        input.seek(SeekFrom::Start(len - max_bytes))?;
    }
    let mut output = File::create(dst)?;
    io::copy(&mut input, &mut output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{backup_path, read_tail, rotate_if_needed};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_log(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("beenet-log-rotate-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir.join("worker.log")
    }

    #[test]
    fn leaves_small_log_alone() {
        let path = temp_log("small");
        fs::write(&path, "hello\n").unwrap();
        assert!(!rotate_if_needed(&path, 32, 2).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello\n");
        assert!(!backup_path(&path, 1).exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn truncates_and_keeps_numbered_tails() {
        let path = temp_log("roll");
        fs::write(&path, "aaaaaaaaaa").unwrap();
        assert!(rotate_if_needed(&path, 4, 2).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        assert_eq!(fs::read_to_string(backup_path(&path, 1)).unwrap(), "aaaa");

        fs::write(&path, "bbbbbbbbbb").unwrap();
        assert!(rotate_if_needed(&path, 4, 2).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        assert_eq!(fs::read_to_string(backup_path(&path, 1)).unwrap(), "bbbb");
        assert_eq!(fs::read_to_string(backup_path(&path, 2)).unwrap(), "aaaa");
        assert!(!backup_path(&path, 3).exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn read_tail_skips_the_head() {
        let path = temp_log("tail");
        fs::write(&path, "0123456789").unwrap();
        assert_eq!(read_tail(&path, 4).unwrap(), "6789");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
