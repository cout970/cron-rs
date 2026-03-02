use crate::alerts::{send_alert, Alert, AlertConfig, TaskExecutionDetails};
use crate::config::TaskConfig;
use crate::sqlite_logger::{ExecutionAttempt, ExecutionFailure, ExecutionSuccess, SqliteLogger};
use crate::utils::format_duration;
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use log::{debug, error, info, warn};
use std::fs::File;
use std::os::unix::prelude::CommandExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use users::{get_user_by_name, get_group_by_name};

static TASK_ID_COUNTER: AtomicU32 = AtomicU32::new(1);

#[derive(Debug)]
pub struct TaskExecutor {
    pub alerts: AlertConfig,
    pub sqlite_logger: Option<SqliteLogger>,
}

#[derive(Debug)]
pub struct ExecutionResult {
    pub task_id: u32,
    pub pid: u32,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub duration: Duration,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

impl TaskExecutor {
    pub fn new(alerts: AlertConfig, sqlite_logger: Option<SqliteLogger>) -> Self {
        Self {
            alerts,
            sqlite_logger,
        }
    }

    /// Execute a task immediately, returning the execution result
    pub async fn execute_task(&self, task: &TaskConfig) -> anyhow::Result<ExecutionResult> {
        let stdout_path = self.get_stdout_path(task);
        let stderr_path = self.get_stderr_path(task);

        // Create output directories if needed
        self.create_output_directories(&stdout_path, &stderr_path, &task.name).await?;

        // Create output files
        let stdout_file = File::create(&stdout_path).map_err(|e| {
            anyhow!(
                "Failed to create stdout file {} for task '{}': {}",
                stdout_path.display(),
                task.name,
                e
            )
        })?;

        let stderr_file = File::create(&stderr_path).map_err(|e| {
            anyhow!(
                "Failed to create stderr file {} for task '{}': {}",
                stderr_path.display(),
                task.name,
                e
            )
        })?;

        // Build command
        let shell = task.shell.as_deref().unwrap_or("/bin/sh");
        let mut cmd = Command::new(shell);
        cmd.arg("-c");
        cmd.arg(&task.cmd);

        // Set environment variables
        if let Some(env) = &task.env {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }

        // Set working directory
        if let Some(dir) = &task.working_directory {
            cmd.current_dir(dir);
        }

        // Set output redirection
        cmd.stdout(Stdio::from(stdout_file));
        cmd.stderr(Stdio::from(stderr_file));

        // Set user/group if specified
        if let Some(run_as) = &task.run_as {
            if cfg!(unix) {
                let (uid, gid) = self.get_uid_and_gid(run_as)?;
                unsafe {
                    cmd.uid(uid);
                    cmd.gid(gid);
                }
            } else {
                warn!("Task '{}' cannot run as '{}', unsupported on this platform", task.name, run_as);
            }
        }

        let start_time = Utc::now();
        let start_instant = Instant::now();
        let task_id = TASK_ID_COUNTER.fetch_add(1, Ordering::Relaxed);

        // Spawn process
        let mut child = cmd.spawn().map_err(|e| {
            anyhow!("Task '{}' failed to start: {}", task.name, e)
        })?;

        let pid = child.id().unwrap_or(0);
        info!("Task '{}' started with PID: {}", task.name, pid);

        // Log execution attempt
        if let Some(sqlite_logger) = &self.sqlite_logger {
            let attempt = ExecutionAttempt {
                task_name: task.name.clone(),
                task_id,
                pid,
                cmd: task.cmd.clone(),
                start_time,
                timezone: task.timezone.to_string(),
                working_directory: task.working_directory.clone(),
                shell: task.shell.clone(),
                run_as: task.run_as.clone(),
                time_limit: task.time_limit,
            };
            
            if let Err(e) = sqlite_logger.log_execution_attempt(&attempt).await {
                error!("Failed to log execution attempt for task '{}': {}", task.name, e);
            }
        }

        // Wait for completion with optional timeout
        let exit_status = if let Some(time_limit) = task.time_limit {
            tokio::select! {
                status = child.wait() => {
                    status.map_err(|e| anyhow!("Failed to wait for task '{}': {}", task.name, e))?
                }
                _ = tokio::time::sleep(Duration::from_secs(time_limit)) => {
                    warn!("Task '{}' exceeded time limit of {} seconds, sending SIGKILL", task.name, time_limit);
                    child.kill().await.map_err(|e| anyhow!("Failed to kill task '{}': {}", task.name, e))?;
                    child.wait().await.map_err(|e| anyhow!("Failed to wait for task '{}': {}", task.name, e))?
                }
            }
        } else {
            child.wait().await.map_err(|e| anyhow!("Failed to wait for task '{}': {}", task.name, e))?
        };

        let end_time = Utc::now();
        let duration = start_instant.elapsed();
        let exit_code = exit_status.code().unwrap_or(-1);
        let success = exit_status.success();

        // Read output files
        let stdout = tokio::fs::read_to_string(&stdout_path).await.unwrap_or_default();
        let stderr = tokio::fs::read_to_string(&stderr_path).await.unwrap_or_default();

        // Create execution details for alerts
        let details = TaskExecutionDetails {
            task_name: task.name.clone(),
            task_id,
            pid,
            exit_code,
            start_time,
            duration,
            error_message: if success {
                String::new()
            } else {
                format!("Task '{}' failed with exit code {}", task.name, exit_code)
            },
            debug_info: format!("Shell: {}, Command: {}", shell, task.cmd),
            stdout: stdout.clone(),
            stderr: stderr.clone(),
        };

        // Handle success/failure
        if success {
            info!("Task '{}' completed successfully in {}", task.name, format_duration(duration));
            
            // Send success alerts
            for alert in &self.alerts.on_success {
                if let Err(e) = send_alert(alert, &details) {
                    error!("Failed to send success alert for task '{}': {}", task.name, e);
                }
            }
            for alert in &task.on_success {
                if let Err(e) = send_alert(alert, &details) {
                    error!("Failed to send task-specific success alert for task '{}': {}", task.name, e);
                }
            }

            // Log success to SQLite
            if let Some(sqlite_logger) = &self.sqlite_logger {
                let success_log = ExecutionSuccess {
                    task_name: task.name.clone(),
                    task_id,
                    pid,
                    start_time,
                    end_time,
                    duration_seconds: duration.as_secs_f64(),
                    exit_code,
                };
                
                if let Err(e) = sqlite_logger.log_execution_success(&success_log).await {
                    error!("Failed to log execution success for task '{}': {}", task.name, e);
                }
            }
        } else {
            error!("Task '{}' failed with exit code {}", task.name, exit_code);
            
            // Send failure alerts
            for alert in &self.alerts.on_failure {
                if let Err(e) = send_alert(alert, &details) {
                    error!("Failed to send failure alert for task '{}': {}", task.name, e);
                }
            }
            for alert in &task.on_failure {
                if let Err(e) = send_alert(alert, &details) {
                    error!("Failed to send task-specific failure alert for task '{}': {}", task.name, e);
                }
            }

            // Log failure to SQLite
            if let Some(sqlite_logger) = &self.sqlite_logger {
                let failure_log = ExecutionFailure {
                    task_name: task.name.clone(),
                    task_id,
                    pid,
                    start_time,
                    end_time,
                    duration_seconds: duration.as_secs_f64(),
                    exit_code: Some(exit_code),
                    error_message: details.error_message.clone(),
                    failure_reason: "Task execution failed".to_string(),
                };
                
                if let Err(e) = sqlite_logger.log_execution_failure(&failure_log).await {
                    error!("Failed to log execution failure for task '{}': {}", task.name, e);
                }
            }
        }

        Ok(ExecutionResult {
            task_id,
            pid,
            start_time,
            end_time,
            duration,
            exit_code,
            stdout,
            stderr,
            success,
        })
    }

    fn get_stdout_path(&self, task: &TaskConfig) -> PathBuf {
        if let Some(path) = task.stdout.as_deref() {
            PathBuf::from(path)
        } else {
            PathBuf::from(format!(
                ".tmp/{}_stdout.log",
                sanitise_file_name::sanitise(&task.name)
            ))
        }
    }

    fn get_stderr_path(&self, task: &TaskConfig) -> PathBuf {
        if let Some(path) = task.stderr.as_deref() {
            PathBuf::from(path)
        } else {
            PathBuf::from(format!(
                ".tmp/{}_stderr.log",
                sanitise_file_name::sanitise(&task.name)
            ))
        }
    }

    async fn create_output_directories(&self, stdout_path: &PathBuf, stderr_path: &PathBuf, task_name: &str) -> anyhow::Result<()> {
        if let Some(path) = stdout_path.parent() {
            if !path.exists() {
                tokio::fs::create_dir_all(path).await.map_err(|e| {
                    anyhow!("Failed to create stdout parent directory for task '{}': {}", task_name, e)
                })?;
            }
        }
        if let Some(path) = stderr_path.parent() {
            if !path.exists() {
                tokio::fs::create_dir_all(path).await.map_err(|e| {
                    anyhow!("Failed to create stderr parent directory for task '{}': {}", task_name, e)
                })?;
            }
        }
        Ok(())
    }

    fn get_uid_and_gid(&self, run_as: &str) -> anyhow::Result<(u32, u32)> {
        let parts: Vec<&str> = run_as.split(':').collect();
        let username = parts[0];
        let groupname = parts.get(1).unwrap_or(&username);

        let user = get_user_by_name(username)
            .ok_or_else(|| anyhow!("User '{}' not found", username))?;

        let group = get_group_by_name(groupname)
            .ok_or_else(|| anyhow!("Group '{}' not found", groupname))?;

        Ok((user.uid(), group.gid()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Schedule;
    use chrono_tz::UTC;
    use std::time::Duration as StdDuration;

    fn create_test_task(name: &str, cmd: &str) -> TaskConfig {
        TaskConfig {
            name: name.to_string(),
            cmd: cmd.to_string(),
            schedule: Schedule::Every { interval: StdDuration::from_secs(60), aligned: false },
            timezone: UTC,
            avoid_overlapping: false,
            run_as: None,
            time_limit: None,
            working_directory: None,
            env: None,
            shell: None,
            stdout: None,
            stderr: None,
            on_failure: vec![],
            on_success: vec![],
        }
    }

    #[tokio::test]
    async fn test_execute_simple_task() {
        let alerts = AlertConfig::default();
        let executor = TaskExecutor::new(alerts, None);
        let task = create_test_task("test_echo", "echo 'Hello, World!'");
        
        let result = executor.execute_task(&task).await.unwrap();
        
        assert!(result.success);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("Hello, World!"));
    }

    #[tokio::test]
    async fn test_execute_failing_task() {
        let alerts = AlertConfig::default();
        let executor = TaskExecutor::new(alerts, None);
        let task = create_test_task("test_fail", "exit 1");
        
        let result = executor.execute_task(&task).await.unwrap();
        
        assert!(!result.success);
        assert_eq!(result.exit_code, 1);
    }

    #[tokio::test]
    async fn test_execute_task_with_timeout() {
        let alerts = AlertConfig::default();
        let executor = TaskExecutor::new(alerts, None);
        let mut task = create_test_task("test_timeout", "sleep 5");
        task.time_limit = Some(1); // 1 second timeout
        
        let result = executor.execute_task(&task).await.unwrap();
        
        assert!(!result.success);
        assert!(result.duration.as_secs() <= 2); // Should timeout quickly
    }
}