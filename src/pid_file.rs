use anyhow::{anyhow, Context};
use log::{debug, warn};
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessesToUpdate, System};

pub const DEFAULT_PID_FILE: &str = "/run/cron-rs.pid";

/// PID file owned by the running scheduler. The file is removed when this value is dropped,
/// as long as it still contains our own PID (another instance may have replaced it).
pub struct PidFile {
    path: PathBuf,
    pid: u32,
}

impl PidFile {
    /// Writes the current process PID to `path`.
    /// Warns (but does not fail) if the file points to another live cron-rs process.
    pub fn create(path: &Path) -> anyhow::Result<Self> {
        let pid = std::process::id();

        if let Ok(existing) = read_pid(path) {
            if existing != pid && is_cron_rs_process(existing) {
                warn!(
                    "PID file {} points to another running cron-rs process (PID {}), overwriting it",
                    path.display(),
                    existing
                );
            }
        }

        std::fs::write(path, format!("{}\n", pid))
            .with_context(|| format!("Failed to write PID file {}", path.display()))?;
        debug!("Wrote PID {} to {}", pid, path.display());

        Ok(Self { path: path.to_path_buf(), pid })
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if matches!(read_pid(&self.path), Ok(pid) if pid == self.pid) {
            if let Err(e) = std::fs::remove_file(&self.path) {
                warn!("Failed to remove PID file {}: {}", self.path.display(), e);
            }
        }
    }
}

pub fn read_pid(path: &Path) -> anyhow::Result<u32> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read PID file {}", path.display()))?;
    contents
        .trim()
        .parse::<u32>()
        .map_err(|_| anyhow!("PID file {} does not contain a valid PID: '{}'", path.display(), contents.trim()))
}

/// Returns true if `pid` is alive and its process name looks like cron-rs.
pub fn is_cron_rs_process(pid: u32) -> bool {
    process_name(pid).is_some_and(|name| name.contains("cron-rs"))
}

pub fn process_name(pid: u32) -> Option<String> {
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).map(|p| p.name().to_string_lossy().to_string())
}

/// Sends SIGHUP to `pid`.
pub fn send_sighup(pid: u32) -> anyhow::Result<()> {
    let sys_pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[sys_pid]), true);
    let process = sys.process(sys_pid).ok_or_else(|| anyhow!("Process {} is not running", pid))?;

    match process.kill_with(sysinfo::Signal::Hangup) {
        Some(true) => Ok(()),
        Some(false) => Err(anyhow!(
            "Failed to send SIGHUP to process {} (insufficient permissions? try with sudo)",
            pid
        )),
        None => Err(anyhow!("SIGHUP is not supported on this platform")),
    }
}
