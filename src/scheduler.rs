use crate::alerts::{send_alert, Alert, AlertConfig, TaskExecutionDetails};
use crate::config::{Config, Schedule, TaskConfig, TimePatternField};
use crate::sqlite_logger::{ExecutionAttempt, ExecutionFailure, ExecutionSuccess, SqliteLogger};
use crate::utils::format_duration;
use anyhow::anyhow;
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeDelta, Timelike};
use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use log::{debug, error, info, warn};
use signal_hook::consts::SIGINT;
use std::collections::HashMap;
use std::fs::File;
use std::ops::{Add, Deref};
use std::os::unix::prelude::CommandExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use sysinfo::{Gid, Groups, ProcessStatus, User, Users};
use sysinfo::{Pid, System};
use tokio::process::{Child, Command};
use tokio::signal;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct PendingTask {
    pub config: TaskConfig,
    pub last_execution: Option<Instant>,
    pub last_execution_time: Option<DateTime<Utc>>,
    pub last_pid: Option<u32>,
    pub retries: u32,
}

static ACTIVE_TASK_ID_COUNTER: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug)]
struct ActiveTask {
    id: u32,
    config: TaskConfig,
    pid: u32,
    start_instant: Instant,
    start_time: DateTime<Utc>,
    child: Arc<Mutex<Child>>,
    debug_info: String,
    time_limit: Option<u64>,
    stdout: PathBuf,
    stderr: PathBuf,
}

pub struct Scheduler {
    tasks: Vec<TaskConfig>,
    active_tasks: Vec<ActiveTask>,
    running_tasks: Vec<PendingTask>,
    async_handles: Vec<JoinHandle<()>>,
    config: Config,
    sqlite_logger: Option<SqliteLogger>,
}

impl Scheduler {
    pub fn new(config: Config) -> Self {
        Scheduler {
            tasks: config.tasks.clone(),
            active_tasks: Vec::new(),
            running_tasks: Vec::new(),
            async_handles: Vec::new(),
            config,
            sqlite_logger: None,
        }
    }

    pub fn run(mut self) -> anyhow::Result<()> {
        let runtime = tokio::runtime::Runtime::new()?;
        let mutex: Arc<Mutex<Scheduler>> = Arc::new(Mutex::new(self));

        runtime.block_on(Self::run_async(mutex))?;
        Ok(())
    }

    async fn run_async(mutex: Arc<Mutex<Scheduler>>) -> anyhow::Result<()> {
        // Initialize SQLite logger if configured
        {
            let mut scheduler = mutex.lock().await;
            if let Some(sqlite_config) = &scheduler.config.logging.sqlite {
                if sqlite_config.enabled {
                    match SqliteLogger::new(sqlite_config.clone()).await {
                        Ok(logger) => {
                            scheduler.sqlite_logger = Some(logger);
                        }
                        Err(e) => {
                            error!("Failed to initialize SQLite logger: {}", e);
                        }
                    }
                }
            }
        }

        let tasks_config = {
            let scheduler = mutex.lock().await;
            scheduler.tasks.clone()
        };
        info!("Initializing scheduler with {} tasks", tasks_config.len());

        // Spawn task execution tasks
        for task in &tasks_config {
            let task_config = task.clone();
            let scheduler_mutex = mutex.clone();

            let handle = tokio::spawn(async move {
                let mut pending_task = PendingTask::new(task_config);

                // Wait loop for the right time to execute the task
                loop {
                    let start = Instant::now();
                    // Check if the task must be executed now
                    if !Self::is_task_ready_for_execution(&pending_task) {
                        Self::sleep_until_task_is_ready(&pending_task).await;
                        continue;
                    }

                    // Verify that the previous execution is finished, if the config requires it
                    if pending_task.config.avoid_overlapping {
                        let running_tasks = {
                            let scheduler = scheduler_mutex.lock().await;
                            scheduler.running_tasks.clone()
                        };

                        if Self::is_task_running(&pending_task, &running_tasks) {
                            debug!(
                                "Task '{}' is already running, skipping execution",
                                pending_task.config.name
                            );
                            Self::sleep_until_task_is_ready(&pending_task).await;
                            continue;
                        }
                    }

                    // Execute the task
                    let (alert_config, config, sqlite_logger) = {
                        let scheduler = scheduler_mutex.lock().await;
                        (
                            scheduler.config.alerts.clone(),
                            scheduler.config.clone(),
                            scheduler.sqlite_logger.clone(),
                        )
                    };
                    let active_task =
                        match Self::execute_task(&pending_task, &alert_config, &config, &sqlite_logger).await {
                            Ok(active_task) => active_task,
                            Err(e) => {
                                error!("{}", e);
                                continue;
                            }
                        };

                    pending_task.last_execution = Some(active_task.start_instant);
                    pending_task.last_execution_time = Some(active_task.start_time);
                    pending_task.last_pid = Some(active_task.pid);

                    let task_id = active_task.id;
                    {
                        let mut scheduler = scheduler_mutex.lock().await;
                        scheduler.running_tasks.push(pending_task.clone());
                        scheduler.active_tasks.push(active_task);
                    }

                    // Wait for the task to finish
                    Self::wait_for_task(scheduler_mutex.clone(), task_id).await;

                    // Sleep at least to the next second to avoid running the task multiple times the same datetime
                    if start.elapsed().as_secs() < 1 {
                        // Sleeping a hole second makes the time drifts slowly, by sleeping only the
                        // millis needed to jump to the next second, we mostly eliminate the drift
                        let next_second =
                            Self::get_current_datetime_at(pending_task.config.timezone) + TimeDelta::seconds(1);
                        let current_second = Self::get_precise_datetime_at(pending_task.config.timezone);

                        // Get the number of milliseconds to the next second
                        let diff = next_second.signed_duration_since(current_second).num_milliseconds() as u64;

                        sleep(Duration::from_millis(diff)).await;
                    }
                }
            });

            {
                let mut scheduler = mutex.lock().await;
                scheduler.async_handles.push(handle);
            }
        }

        // Wait for Ctrl+C signal to stop the infinite loop
        let ctrl_c = signal::ctrl_c();
        tokio::pin!(ctrl_c);
        tokio::select! {
            _ = &mut ctrl_c => {
                info!("Scheduler shutdown initiated");
                {
                    let mut scheduler = mutex.lock().await;
                    for handle in &scheduler.async_handles {
                        handle.abort();
                    }
                }
            }
        }

        Ok(())
    }

    // Wait for the task to end and handle the result
    async fn wait_for_task(mutex: Arc<Mutex<Scheduler>>, task_id: u32) {
        let (child_mutex, time_limit, task_name) = {
            let scheduler = mutex.lock().await;
            let active_task = scheduler
                .active_tasks
                .iter()
                .find(|t| t.id == task_id)
                .expect("Task not found");
            (
                active_task.child.clone(),
                active_task.time_limit.clone(),
                active_task.config.name.clone(),
            )
        };

        // Wait for the task to finish in a separate coroutine to not block this loop
        let scheduler_mutex = mutex.clone();
        let handle = tokio::spawn(async move {
            let mut child = child_mutex.lock().await;

            let exit_status = if let Some(time_limit) = time_limit {
                tokio::select! {
                    status = child.wait() => {
                        status.expect("Failed to wait for task")
                    }
                    _ = sleep(Duration::from_secs(time_limit)) => {
                        // Warn the user that the task will be killed
                        warn!("Task '{}' exceeded time limit of {} seconds, sending SIGKILL", task_name, time_limit);

                        child.kill().await.expect("Unable to kill process");
                        // We still need to wait for the process to fully terminate
                        child.wait().await.expect("Failed to wait for task")
                    }
                }
            } else {
                child.wait().await.expect("Failed to wait for task")
            };

            {
                let mut scheduler = scheduler_mutex.lock().await;
                // Remove running task
                scheduler.running_tasks.retain(|t| t.config.name != task_name);

                // Remove active task
                let active_task_index = scheduler
                    .active_tasks
                    .iter()
                    .position(|t| t.id == task_id)
                    .expect("Task not found");

                let mut active_task = scheduler.active_tasks.remove(active_task_index);

                let sqlite_logger = scheduler.sqlite_logger.clone();
                Self::on_task_completed(&active_task, exit_status, &scheduler.config, &sqlite_logger).await;
            }
        });

        {
            let mut scheduler = mutex.lock().await;
            scheduler.async_handles.push(handle);
        }
    }

    /// Returns the current time rounded in a way that has no fractional seconds
    pub fn get_current_datetime_at(timezone: Tz) -> DateTime<Tz> {
        // Rounds the time, by flooring it to the second, to avoid issues with comparisons of dates and rounding
        timezone
            .from_utc_datetime(&Utc::now().naive_utc())
            .with_nanosecond(0)
            .unwrap()
    }

    /// Gets the current time in the given timezone, with full precision (including fractional seconds)
    pub fn get_precise_datetime_at(timezone: Tz) -> DateTime<Tz> {
        timezone.from_utc_datetime(&Utc::now().naive_utc())
    }

    async fn sleep_until_task_is_ready(task: &PendingTask) {
        let precise_now = Self::get_precise_datetime_at(task.config.timezone);
        let now: DateTime<Tz> = Self::get_current_datetime_at(task.config.timezone);

        // Use the current datetime plus 1 second to avoid returning the exact same value
        let next_run = Self::get_next_execution_time(&task, now);
        let wait_time = next_run.signed_duration_since(precise_now);

        debug!(
            "Task '{}' planned next execution at {} (current time {}, around {} s later)",
            task.config.name,
            next_run,
            now,
            (wait_time.num_milliseconds() as f32 / 1000.0f32).max(0f32)
        );

        let duration = if wait_time.num_milliseconds() > 1000 {
            // Wait the remaining time, minus 1 second, to account for the imprecision of sleep()
            Duration::from_millis(wait_time.num_milliseconds() as u64 - 1000u64)
        } else if wait_time.num_milliseconds() > 100 {
            // Sleep for the remaining time
            Duration::from_millis(wait_time.num_milliseconds() as u64)
        } else {
            // For intervals of less than 100 ms, sleep for 100 ms
            Duration::from_millis(100)
        };
        sleep(duration).await;
    }

    /// Checks if the task is ready for execution right now
    fn is_task_ready_for_execution(task: &PendingTask) -> bool {
        let now: DateTime<Tz> = Self::get_current_datetime_at(task.config.timezone);

        // If the last execution was at this time, avoid running it again, wait until at least the next second
        if let Some(time) = task.last_execution_time {
            if time.timestamp() == now.timestamp() {
                return false;
            }
        }

        let next_scheduled_run = Self::get_next_execution_time(task, now);
        // If the next scheduled run is now, return true
        next_scheduled_run.timestamp() <= now.timestamp()
    }

    /// Checks if the task is running
    fn is_task_running(task: &PendingTask, active_tasks: &[PendingTask]) -> bool {
        if let Some(pid) = task.last_pid {
            let sys = System::new_all();
            if sys.process(Pid::from_u32(pid)).is_some() {
                return true;
            }
        }

        active_tasks.iter().any(|active| active.config.name == task.config.name)
    }

    /// Spawns a subprocess to execute the task
    async fn execute_task(
        task: &PendingTask,
        alerts: &AlertConfig,
        config: &Config,
        sqlite_logger: &Option<SqliteLogger>,
    ) -> anyhow::Result<ActiveTask> {
        let stdout_path = if let Some(path) = task.config.stdout.as_deref() {
            PathBuf::from(path)
        } else {
            PathBuf::from(format!(
                ".tmp/{}_stdout.log",
                sanitise_file_name::sanitise(&task.config.name)
            ))
        };

        let stderr_path = if let Some(path) = task.config.stderr.as_deref() {
            PathBuf::from(path)
        } else {
            PathBuf::from(format!(
                ".tmp/{}_stderr.log",
                sanitise_file_name::sanitise(&task.config.name)
            ))
        };

        if let Some(path) = stdout_path.parent() {
            if !path.exists() {
                tokio::fs::create_dir_all(path).await.expect(
                    format!(
                        "Failed to create stdout parent directory for task '{}'",
                        task.config.name
                    )
                    .as_str(),
                );
            }
        }
        if let Some(path) = stderr_path.parent() {
            if !path.exists() {
                tokio::fs::create_dir_all(path).await.expect(
                    format!(
                        "Failed to create stderr parent directory for task '{}'",
                        task.config.name
                    )
                    .as_str(),
                );
            }
        }

        let stdout = match File::create(&stdout_path) {
            Ok(file) => file,
            Err(e) => {
                return Err(anyhow!(
                    "Failed to create {} for task '{}': {}",
                    stdout_path.to_string_lossy(),
                    task.config.name,
                    e
                ));
            }
        };
        let stderr = match File::create(&stderr_path) {
            Ok(file) => file,
            Err(e) => {
                return Err(anyhow!(
                    "Failed to create {} for task '{}': {}",
                    stderr_path.to_string_lossy(),
                    task.config.name,
                    e
                ));
            }
        };

        // Record debug information, to show in case of failure
        let mut debug_info = String::new();

        // Shell to run the command
        let shell = task.config.shell.as_deref().unwrap_or_else(|| "/bin/sh");

        debug_info.push_str(&format!("Cmd: {} -c '{}'\n", shell, task.config.cmd));
        let mut cmd = Command::new(shell);
        cmd.arg("-c");
        cmd.arg(&task.config.cmd);

        // Set environment variables if specified
        if let Some(env) = &task.config.env {
            for (key, value) in env {
                debug_info.push_str(&format!("Env '{}' => '{}'\n", key, value));
                cmd.env(key, value);
            }
            debug!(
                "Set {} environment variables for task '{}'",
                env.len(),
                task.config.name
            );
        }

        // Set working directory if specified
        if let Some(dir) = &task.config.working_directory {
            debug_info.push_str(&format!("Working dir '{}'\n", dir));
            cmd.current_dir(dir);
            debug!("Set runtime directory to '{}' for task '{}'", dir, task.config.name);
        }

        // Set output redirection
        debug_info.push_str(&format!("Stdio '{}'\n", stdout_path.to_string_lossy()));
        debug_info.push_str(&format!("Stderr '{}'\n", stderr_path.to_string_lossy()));
        cmd.stdout(Stdio::from(stdout));
        cmd.stderr(Stdio::from(stderr));

        // Run as another user if specified
        if let Some(run_as) = &task.config.run_as {
            // Only available on Unix-like systems
            if cfg!(unix) {
                let (uid, user_str, gid, group_str) = match Self::get_uid_and_gid(run_as) {
                    Ok((uid, user_str, gid, group_str)) => (uid, user_str, gid, group_str),
                    Err(e) => {
                        return Err(anyhow!(
                            "Failed to get uid and gid for task '{}': {}",
                            task.config.name,
                            e
                        ));
                    }
                };

                // uid and gid are opaque types, there is no operation to convert them to u32, but they deref() as u32, so add(0) works
                debug_info.push_str(&format!("Uid {} '{}'\n", uid, user_str));
                debug_info.push_str(&format!("Gid {} '{}'\n", gid, group_str));
                unsafe {
                    cmd.uid(uid);
                    cmd.gid(gid);
                }
                debug!(
                    "Task '{}' will run as user '{}' and group '{}'",
                    task.config.name, user_str, group_str
                );
            } else {
                warn!(
                    "Task '{}' cannot run as '{}', unsupported on this platform",
                    task.config.name, run_as
                );
            }
        }

        let clock_time: DateTime<Utc> = Utc::now();
        let now = Instant::now();

        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id().unwrap_or(0);
                let task_id = ACTIVE_TASK_ID_COUNTER.fetch_add(1, Ordering::Relaxed) as u32;
                info!("Task '{}' started with PID: {}", task.config.name, pid);

                // Log execution attempt to SQLite
                if let Some(sqlite_logger) = sqlite_logger {
                    let attempt = ExecutionAttempt {
                        task_name: task.config.name.clone(),
                        task_id,
                        pid,
                        cmd: task.config.cmd.clone(),
                        start_time: clock_time,
                        timezone: task.config.timezone.to_string(),
                        working_directory: task.config.working_directory.clone(),
                        shell: task.config.shell.clone(),
                        run_as: task.config.run_as.clone(),
                        time_limit: task.config.time_limit,
                    };

                    if let Err(e) = sqlite_logger.log_execution_attempt(&attempt).await {
                        error!("Failed to log execution attempt for task '{}': {}", task.config.name, e);
                    }
                }

                Ok(ActiveTask {
                    id: task_id,
                    config: task.config.clone(),
                    pid,
                    start_instant: now,
                    start_time: clock_time,
                    child: Arc::new(Mutex::new(child)),
                    debug_info: debug_info.trim().to_string(),
                    time_limit: task.config.time_limit,
                    stdout: stdout_path.clone(),
                    stderr: stderr_path.clone(),
                })
            }
            Err(e) => {
                if e.to_string().contains("Operation not permitted") && task.config.run_as.is_some() {
                    debug_info.push_str(&format!(
                        "Note: The task was executed with run_as '{}', make sure the current user '{}' has permission to run as that user",
                        task.config.run_as.as_deref().unwrap(),
                        users::get_current_username().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "<unknown>".to_string())
                    ));
                }

                let details = TaskExecutionDetails {
                    task_name: task.config.name.to_string(),
                    task_id: 0,
                    pid: 0,
                    exit_code: -1,
                    start_time: clock_time,
                    duration: Duration::default(),
                    error_message: format!("Task '{}' failed to start", task.config.name),
                    debug_info: debug_info.trim().to_string(),
                    stdout: String::new(),
                    stderr: e.to_string(),
                };

                Self::on_task_failure(&details, alerts, &task.config.on_failure, sqlite_logger).await;

                Err(anyhow!(
                    "Task '{}' failed to start: {}, Debug info:\n{}",
                    task.config.name,
                    e,
                    debug_info
                ))
            }
        }
    }

    /// Handle the task completion
    async fn on_task_completed(
        task: &ActiveTask,
        status: ExitStatus,
        config: &Config,
        sqlite_logger: &Option<SqliteLogger>,
    ) {
        let exit_code = status.code().unwrap_or(-1);
        let execution_time = task.start_instant.elapsed();

        let details = TaskExecutionDetails {
            task_name: task.config.name.to_string(),
            task_id: task.id,
            pid: task.pid,
            exit_code,
            start_time: task.start_time,
            duration: execution_time,
            error_message: format!("Task '{}' failed, {}", task.config.name, status),
            debug_info: task.debug_info.clone(),
            stdout: tokio::fs::read_to_string(&task.stdout).await.unwrap_or_default(),
            stderr: tokio::fs::read_to_string(&task.stderr).await.unwrap_or_default(),
        };

        if !status.success() {
            error!(
                "Task '{}' failed with exit code {} ({})",
                task.config.name, exit_code, status
            );

            Self::on_task_failure(&details, &config.alerts, &task.config.on_failure, sqlite_logger).await;
        } else {
            info!(
                "Task '{}' finished with status: {}, elapsed {}",
                task.config.name,
                status,
                format_duration(execution_time)
            );

            Self::on_task_success(&details, &config.alerts, &task.config.on_success, sqlite_logger).await;
        }
    }

    /// Notify the user about task failure
    async fn on_task_failure(
        details: &TaskExecutionDetails,
        alerts: &AlertConfig,
        task_on_failure: &[Alert],
        sqlite_logger: &Option<SqliteLogger>,
    ) {
        for alert in &alerts.on_failure {
            if let Err(e) = send_alert(alert, details) {
                error!("Failed to send alert for task '{}': {}", details.task_name, e);
            }
        }
        for alert in task_on_failure {
            if let Err(e) = send_alert(alert, details) {
                error!(
                    "Failed to send task-specific alert for task '{}': {}",
                    details.task_name, e
                );
            }
        }

        if let Some(sqlite_logger) = sqlite_logger {
            let failure = ExecutionFailure {
                task_name: details.task_name.clone(),
                task_id: details.task_id,
                pid: details.pid,
                start_time: details.start_time,
                end_time: details.start_time + chrono::Duration::from_std(details.duration).unwrap_or_default(),
                duration_seconds: details.duration.as_secs_f64(),
                exit_code: if details.exit_code == -1 {
                    None
                } else {
                    Some(details.exit_code)
                },
                error_message: details.error_message.clone(),
                failure_reason: "Task execution failed".to_string(),
            };

            if let Err(e) = sqlite_logger.log_execution_failure(&failure).await {
                error!(
                    "Failed to log execution failure for task '{}': {}",
                    details.task_name, e
                );
            }
        }
    }

    /// Notify the user about task success
    async fn on_task_success(
        details: &TaskExecutionDetails,
        alerts: &AlertConfig,
        task_on_success: &[Alert],
        sqlite_logger: &Option<SqliteLogger>,
    ) {
        for alert in &alerts.on_success {
            if let Err(e) = send_alert(alert, details) {
                error!("Failed to send alert for task '{}': {}", details.task_name, e);
            }
        }
        for alert in task_on_success {
            if let Err(e) = send_alert(alert, details) {
                error!(
                    "Failed to send task-specific alert for task '{}': {}",
                    details.task_name, e
                );
            }
        }

        if let Some(sqlite_logger) = sqlite_logger {
            let success = ExecutionSuccess {
                task_name: details.task_name.clone(),
                task_id: details.task_id,
                pid: details.pid,
                start_time: details.start_time,
                end_time: details.start_time + chrono::Duration::from_std(details.duration).unwrap_or_default(),
                duration_seconds: details.duration.as_secs_f64(),
                exit_code: details.exit_code,
            };

            if let Err(e) = sqlite_logger.log_execution_success(&success).await {
                error!(
                    "Failed to log execution success for task '{}': {}",
                    details.task_name, e
                );
            }
        }
    }

    /// Calculate the next date and time for the task to run
    /// current_date: must be rounded to the second, use Self::get_current_datetime_at(timezone) to get it
    pub fn get_next_execution_time(task: &PendingTask, current_date: DateTime<Tz>) -> DateTime<Tz> {
        match &task.config.schedule {
            Schedule::Every { interval, aligned } => {
                let tmp = if let (Some(last_execution), Some(last_execution_time)) =
                    (task.last_execution, task.last_execution_time)
                {
                    // Bad input, assume no previous run
                    if current_date.timestamp() < last_execution_time.timestamp() {
                        return current_date;
                    }

                    let last_execution_in_tz = last_execution_time
                        .with_timezone(&task.config.timezone)
                        .with_nanosecond(0)
                        .unwrap();

                    if *aligned {
                        // Make the next run aligned to the interval length
                        let tick_len = interval.as_secs() as i64;
                        let current_date_after_interval = ((current_date.timestamp() + tick_len) / tick_len) * tick_len;
                        let diff = current_date_after_interval - current_date.timestamp();

                        last_execution_in_tz + chrono::Duration::seconds(diff)
                    } else {
                        last_execution_in_tz + chrono::Duration::from_std(*interval).unwrap()
                    }
                } else {
                    // First run
                    current_date
                };

                tmp
            }
            Schedule::When { time } => {
                // Add 1 second to avoid returning the same value
                let mut curr = current_date;
                let mut limit = 365;

                loop {
                    // Iteration limit to avoid infinite loops
                    if limit <= 0 {
                        error!("Task '{}' has no valid next execution time", task.config.name);
                        return current_date;
                    }
                    limit -= 1;

                    // Try next second, minute, hour, etc.
                    let (second, t) = time.second.get_next_valid_value(curr.second(), 60);
                    let (minute, t) = time.minute.get_next_valid_value(curr.minute() + t, 60);
                    let (hour, t) = time.hour.get_next_valid_value(curr.hour() + t, 24);
                    let mut days_in_month = Self::get_num_of_days_in_month(curr.month(), curr.year());
                    if curr.day() + t >= days_in_month {
                        days_in_month = Self::get_num_of_days_in_month(curr.month() + 1, curr.year());
                    }
                    let (day0, t) = time.day.get_next_valid_value(curr.day0() + t, days_in_month);
                    let (month0, t) = time.month.get_next_valid_value(curr.month0() + t, 12);
                    let (year, _) = time.year.get_next_valid_value(curr.year() as u32, 3000);

                    let mut next_date = task
                        .config
                        .timezone
                        .with_ymd_and_hms(year as i32, month0 + 1, day0 + 1, hour, minute, second)
                        .unwrap();

                    // If the day of the week doesn't match, move to the next day
                    if !time.day_of_week.matches_value(curr.weekday().num_days_from_monday()) {
                        curr = next_date.add(TimeDelta::days(1));
                        continue;
                    }

                    return next_date.with_nanosecond(0).unwrap_or(next_date);
                }
            }
        }
    }

    /// Parse the user and group from the run_as string and return their UID and GID
    fn get_uid_and_gid(run_as: &str) -> anyhow::Result<(u32, String, u32, String)> {
        let (user_str, group_str) = run_as.split_once(':').unwrap_or((run_as, run_as));
        let users = Users::new_with_refreshed_list();

        let uid = users
            .list()
            .iter()
            .find(|u| u.name() == user_str || u.id().to_string() == user_str)
            .map(|user| user.id());

        let Some(uid) = uid else {
            return Err(anyhow::anyhow!("User '{}' not found", user_str));
        };

        let groups = Groups::new_with_refreshed_list();
        let gid = groups
            .list()
            .iter()
            .find(|g| g.name() == group_str || g.id().to_string() == group_str)
            .map(|group| group.id());

        let Some(gid) = gid else {
            return Err(anyhow::anyhow!("Group '{}' not found", group_str));
        };

        Ok((
            uid.add(0u32),
            user_str.to_string(),
            gid.add(0u32),
            group_str.to_string(),
        ))
    }

    /// Get the number of days in a month, taking into account leap years, the month value is 1-based
    fn get_num_of_days_in_month(mut month: u32, mut year: i32) -> u32 {
        // Wrap value if needed
        if month > 12 {
            month -= 12;
            year += 1;
        }
        let start_of_this_month = NaiveDate::from_ymd_opt(year, month, 1).expect("Invalid date");
        let start_of_next_month = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1).expect("Invalid date")
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1).expect("Invalid date")
        };
        start_of_next_month
            .signed_duration_since(start_of_this_month)
            .num_days() as u32
    }
}

impl PendingTask {
    pub fn new(config: TaskConfig) -> Self {
        PendingTask {
            config,
            last_execution: None,
            last_execution_time: None,
            last_pid: None,
            retries: 0,
        }
    }
}
