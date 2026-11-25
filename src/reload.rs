use crate::config::loader::ConfigLoader;
use crate::config::logging::{LogOutput, LoggingConfig};
use crate::config::parse_config_file;
use crate::config::validation::{validate_config, ValidationResult};
use crate::pid_file::{process_name, read_pid, send_sighup};
use crate::scheduler::{RELOAD_FAILED_MSG, RELOAD_OK_MSG};
use anyhow::anyhow;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_LOG_FILE: &str = "/var/log/cron-rs.log";
const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub struct ReloadOptions {
    /// Max seconds to wait for the daemon's log output. 0 disables log output.
    pub wait: u64,
    /// Send the signal even if validation fails.
    pub force: bool,
}

/// Where the running daemon writes its logs.
enum LogSource {
    File {
        path: PathBuf,
        offset: u64,
    },
    /// `seen` = number of lines already returned (journalctl is re-queried from `since` each poll)
    Journal {
        output: LogOutput,
        since: u64,
        seen: usize,
    },
}

/// Validates the config, sends SIGHUP to the daemon and prints the log lines it produces.
/// Returns Ok(false) if the daemon reported a reload failure.
pub fn cmd_reload(pid: u32, loader: &ConfigLoader, opts: &ReloadOptions) -> anyhow::Result<bool> {
    println!("Validating config file: {}", loader.path.display());
    match validate(loader) {
        Ok(()) => println!("Config file is valid"),
        Err(e) if opts.force => {
            eprintln!("{}", e);
            eprintln!("--force given, sending reload signal anyway");
        }
        Err(e) => return Err(anyhow!("{}. Reload aborted (use --force to reload anyway)", e)),
    }

    // Even if the config has errors, its logging section tells where the daemon logs to
    let logging = loader.load_file().ok().and_then(|f| f.logging).unwrap_or_default();
    let mut source = log_source(&logging)?;

    send_sighup(pid)?;
    println!("Sent SIGHUP to cron-rs (PID {})", pid);

    if opts.wait == 0 {
        return Ok(true);
    }

    println!("Waiting up to {}s for log output...", opts.wait);
    let deadline = Instant::now() + Duration::from_secs(opts.wait);
    let mut lines = vec![];
    loop {
        std::thread::sleep(POLL_INTERVAL);
        match read_new_lines(&mut source, pid) {
            Ok(new) => lines.extend(new),
            Err(e) => {
                eprintln!("Unable to read daemon logs: {}", e);
                return Ok(true);
            }
        }
        let finished = lines
            .iter()
            .any(|l| l.contains(RELOAD_OK_MSG) || l.contains(RELOAD_FAILED_MSG));
        if finished || Instant::now() >= deadline {
            break;
        }
    }

    if lines.is_empty() {
        println!("No log lines found. {}", no_lines_hint(&source));
        return Ok(true);
    }

    println!("---");
    for line in &lines {
        println!("{}", line);
    }
    println!("---");

    Ok(!lines.iter().any(|l| l.contains(RELOAD_FAILED_MSG)))
}

/// Runs the same checks as `validate` plus a full parse (what the daemon does on reload).
fn validate(loader: &ConfigLoader) -> anyhow::Result<()> {
    let config_file = loader.load_file()?;
    let results = validate_config(&config_file);

    let mut errors = 0;
    for msg in &results {
        match msg {
            ValidationResult::Error(m) => {
                errors += 1;
                eprintln!("error: {}", m);
            }
            ValidationResult::Warning(m) => eprintln!("warning: {}", m),
        }
    }
    if errors > 0 {
        return Err(anyhow!("Config file has {} error(s)", errors));
    }

    parse_config_file(&config_file).map_err(|e| anyhow!("Config file is invalid: {}", e))?;
    Ok(())
}

fn log_source(logging: &LoggingConfig) -> anyhow::Result<LogSource> {
    Ok(match logging.output {
        LogOutput::File => {
            let path = logging.file.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_LOG_FILE));
            let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            LogSource::File { path, offset }
        }
        LogOutput::Stdout | LogOutput::Syslog => LogSource::Journal {
            output: logging.output.clone(),
            since: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            seen: 0,
        },
    })
}

fn read_new_lines(source: &mut LogSource, pid: u32) -> anyhow::Result<Vec<String>> {
    match source {
        LogSource::File { path, offset } => {
            let mut file = std::fs::File::open(&path)?;
            let len = file.metadata()?.len();
            if len < *offset {
                // File was truncated/rotated
                *offset = 0;
            }
            file.seek(SeekFrom::Start(*offset))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;

            // Only consume complete lines, keep partial ones for the next poll
            let complete = buf.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
            *offset += complete as u64;
            Ok(String::from_utf8_lossy(&buf[..complete])
                .lines()
                .map(str::to_string)
                .collect())
        }
        LogSource::Journal { output, since, seen } => {
            let format = if *output == LogOutput::Stdout {
                "cat"
            } else {
                "short-iso"
            };
            let out = Command::new("journalctl")
                .arg(format!("_PID={}", pid))
                .arg(format!("--since=@{}", since))
                .args(["--no-pager", "--quiet", "--output", format])
                .output()
                .map_err(|e| anyhow!("failed to run journalctl: {}", e))?;
            if !out.status.success() {
                return Err(anyhow!(
                    "journalctl failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .skip(*seen)
                .map(str::to_string)
                .collect();
            *seen += lines.len();
            Ok(lines)
        }
    }
}

fn no_lines_hint(source: &LogSource) -> String {
    match source {
        LogSource::File { path, .. } => format!("Check the log file {} manually.", path.display()),
        LogSource::Journal { .. } => {
            "The daemon logs to stdout/syslog; if it is not running under systemd, check its output directly."
                .to_string()
        }
    }
}
