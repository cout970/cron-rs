use crate::alerts::{send_alerts, Alert, AlertConfig, TaskExecutionDetails};
use crate::config::loader::ConfigLoader;
use crate::config::watchdog::WatchdogConfig;
use crate::config::{Config, Schedule, TaskConfig, TimePattern, TimePatternField};
use crate::supervisor;
use crate::watchdog;
use crate::sqlite_logger::{ExecutionAttempt, ExecutionFailure, ExecutionSuccess, SqliteLogger};
use crate::utils::format_duration;
use anyhow::anyhow;
use chrono::{DateTime, Datelike, Local, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeDelta, Timelike};
use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use log::{debug, error, info, warn};
use serde_json::json;
use signal_hook::consts::SIGINT;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::ops::{Add, Deref};
use std::os::unix::prelude::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use sysinfo::{Gid, Groups, ProcessStatus, User, Users};
use tokio::process::{Child, Command};
use tokio::signal;
use tokio::signal::unix::SignalKind;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::sleep;

/// Log messages emitted after a SIGHUP reload; `cron-rs reload` looks for them in the daemon logs.
pub const RELOAD_OK_MSG: &str = "Configuration reloaded successfully";
pub const RELOAD_FAILED_MSG: &str = "Failed to reload configuration";

#[derive(Debug, Clone)]
pub struct PendingTask {
    pub config: Arc<TaskConfig>,
    pub last_execution_time: Option<DateTime<Utc>>,
    pub last_pid: Option<u32>,
    pub retries: u32,
}

static ACTIVE_TASK_ID_COUNTER: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug, Clone)]
struct ActiveTask {
    id: u32,
    config: Arc<TaskConfig>,
    pid: u32,
    start_instant: Instant,
    start_time: DateTime<Utc>,
    child: Arc<Mutex<Child>>,
    debug_info: String,
    time_limit: Option<u64>,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

pub struct Scheduler {
    tasks: Vec<Arc<TaskConfig>>,
    active_tasks: Vec<ActiveTask>,
    pending_tasks: Vec<Arc<Mutex<PendingTask>>>,
    task_loop_handles: Vec<JoinHandle<()>>,
    wait_handles: Vec<JoinHandle<()>>,
    config: Config,
    loader: ConfigLoader,
    sqlite_logger: Option<SqliteLogger>,
    /// Epoch-seconds timestamp updated every few seconds by a dedicated tokio task.
    /// The watchdog thread reads this to detect a frozen async runtime.
    heartbeat: Arc<AtomicU64>,
    /// Shared watchdog config so the background thread picks up hot-reload changes.
    watchdog_config: Arc<RwLock<WatchdogConfig>>,
}

impl Scheduler {
    pub fn new(config: Config, loader: ConfigLoader) -> Self {
        let watchdog_config = Arc::new(RwLock::new(config.watchdog.clone()));
        // Initialize heartbeat to now so the watchdog doesn't fire immediately on startup.
        let heartbeat = Arc::new(AtomicU64::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        ));
        Scheduler {
            tasks: config.tasks.clone(),
            active_tasks: Vec::new(),
            pending_tasks: Vec::new(),
            task_loop_handles: Vec::new(),
            wait_handles: Vec::new(),
            config,
            loader,
            sqlite_logger: None,
            heartbeat,
            watchdog_config,
        }
    }

    pub fn run(mut self) -> anyhow::Result<()> {
        let runtime = tokio::runtime::Runtime::new()?;
        let mutex: Arc<Mutex<Scheduler>> = Arc::new(Mutex::new(self));

        runtime.block_on(Self::run_async(mutex))?;
        Ok(())
    }

    pub async fn save_state(&self) {
        let mut pending_tasks = vec![];

        for t in &self.pending_tasks {
            let pt = t.lock().await;
            let now: DateTime<Tz> = Self::get_current_datetime_at(pt.config.timezone);

            // Use the current datetime plus 1 second to avoid returning the exact same value
            let next_run = Self::get_next_execution_time(&pt, now, false);

            pending_tasks.push(json!({
                "config_name": pt.config.name,
                "last_execution_time": pt.last_execution_time.map(|dt| dt.to_rfc3339()),
                "last_pid": pt.last_pid,
                "retries": pt.retries,
                "next_run": next_run.to_rfc3339(),
            }));
        }

        let active_tasks = self
            .active_tasks
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "config_name": t.config.name,
                    "pid": t.pid,
                    "start_time": t.start_time.to_rfc3339(),
                    "time_limit": t.time_limit,
                    "stdout_path": t.stdout_path.to_string_lossy(),
                    "stderr_path": t.stderr_path.to_string_lossy(),
                })
            })
            .collect::<Vec<_>>();

        let mut state = serde_json::to_string(&json!({
            "now": Utc::now().to_rfc3339(),
            "pending_tasks": pending_tasks,
            "active_tasks": active_tasks,
        }))
        .unwrap();

        state.push_str("\n");

        // TODO make the path configurable
        let res = tokio::fs::write("./cron-rs_scheduler_state.json", state.as_bytes()).await;

        if let Err(e) = res {
            error!("Failed to save scheduler state: {}", e);
        }
    }

    async fn reload_config(&mut self) -> anyhow::Result<usize> {
        let new_config = self.loader.load()?;

        // Propagate new watchdog settings to the background thread.
        if let Ok(mut guard) = self.watchdog_config.write() {
            *guard = new_config.watchdog.clone();
        }

        // Save current state before tearing down
        self.save_state().await;

        // Build a map of task_name -> (last_execution_time, last_pid, retries) from existing pending tasks
        let mut state_map: HashMap<String, PendingTask> = HashMap::new();
        for pt_mutex in &self.pending_tasks {
            let pt = pt_mutex.lock().await;
            state_map.insert(pt.config.name.clone(), pt.clone());
        }

        // Abort all task loop handles (they stop at their next await point)
        for handle in self.task_loop_handles.drain(..) {
            handle.abort();
        }

        // Clean up finished wait handles (running subprocesses keep their handles)
        self.wait_handles.retain(|h| !h.is_finished());

        // Detect added/removed/retained tasks
        let old_names: std::collections::HashSet<&str> = state_map.keys().map(|s| s.as_str()).collect();
        let new_names: std::collections::HashSet<&str> = new_config.tasks.iter().map(|t| t.name.as_str()).collect();

        let added: Vec<&str> = new_names.difference(&old_names).copied().collect();
        let removed: Vec<&str> = old_names.difference(&new_names).copied().collect();
        let retained: Vec<&str> = new_names.intersection(&old_names).copied().collect();

        if !added.is_empty() {
            info!("New tasks added: {}", added.join(", "));
        }
        if !removed.is_empty() {
            info!("Tasks removed: {}", removed.join(", "));
        }
        if !retained.is_empty() {
            info!("Tasks retained: {}", retained.join(", "));
        }

        // Warn if logging config changed (env_logger can only be initialized once)
        if new_config.logging != self.config.logging {
            warn!("Logging configuration changed, but logging cannot be reconfigured at runtime. Restart to apply logging changes.");
        }

        // Update config and tasks
        self.config = new_config;
        self.tasks = self.config.tasks.clone();

        // Reinitialize SQLite logger if configured
        self.sqlite_logger = None;
        if let Some(sqlite_config) = &self.config.logging.sqlite {
            if sqlite_config.enabled {
                match SqliteLogger::new(sqlite_config.clone()).await {
                    Ok(logger) => {
                        self.sqlite_logger = Some(logger);
                    }
                    Err(e) => {
                        error!("Failed to reinitialize SQLite logger: {}", e);
                    }
                }
            }
        }

        // Create new PendingTasks, restoring state for tasks that still exist by name
        let mut new_pending_tasks = Vec::new();
        for task_config in &self.tasks {
            let mut new_task = PendingTask::new(task_config.clone());
            if let Some(prev_task) = state_map.get(&task_config.name) {
                new_task.last_execution_time = prev_task.last_execution_time;
                new_task.last_pid = prev_task.last_pid;
                new_task.retries = prev_task.retries;
            }
            new_pending_tasks.push(Arc::new(Mutex::new(new_task)));
        }
        self.pending_tasks = new_pending_tasks;

        Ok(self.tasks.len())
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

        // Spawn the heartbeat tokio task — keeps the watchdog's liveness check happy.
        // The task updates an atomic counter every few seconds. If the tokio runtime
        // freezes, the counter stops updating and the watchdog detects it.
        let (heartbeat, watchdog_config) = {
            let s = mutex.lock().await;
            (Arc::clone(&s.heartbeat), Arc::clone(&s.watchdog_config))
        };
        {
            let hb = Arc::clone(&heartbeat);
            tokio::spawn(async move {
                loop {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    hb.store(ts, Ordering::Relaxed);
                    sleep(Duration::from_secs(5)).await;
                }
            });
        }

        // Start the watchdog background thread (fire-and-forget; dies with the process).
        watchdog::spawn(watchdog_config, Arc::clone(&heartbeat));

        let pending_tasks: Vec<Arc<Mutex<PendingTask>>> = {
            let mut scheduler = mutex.lock().await;
            let mut pending_tasks = vec![];

            for t in &scheduler.tasks {
                let pt = PendingTask::new(t.clone());
                pending_tasks.push(Arc::new(Mutex::new(pt)));
            }

            scheduler.pending_tasks = pending_tasks.clone();
            pending_tasks
        };
        info!("Initializing scheduler with {} tasks", pending_tasks.len());

        // Spawn task execution tasks
        Self::spawn_tasks(mutex.clone(), pending_tasks).await;

        // Wait for Ctrl+C (SIGINT) or SIGTERM (e.g. `systemctl stop`) to stop the infinite loop
        let ctrl_c = signal::ctrl_c();
        let mut sigterm = signal::unix::signal(SignalKind::terminate()).expect("Failed to register SIGTERM");
        let mut sigusr1 = signal::unix::signal(SignalKind::user_defined1()).expect("Failed to register SIGUSR1");
        let mut sighup = signal::unix::signal(SignalKind::hangup()).expect("Failed to register SIGHUP");

        tokio::pin!(ctrl_c);
        tokio::pin!(sigusr1);
        tokio::pin!(sighup);
        loop {
            tokio::select! {
                _ = &mut ctrl_c => {
                    info!("Received SIGINT, scheduler shutdown initiated");
                    Self::shutdown(&mutex).await;
                    break;
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, scheduler shutdown initiated");
                    Self::shutdown(&mutex).await;
                    break;
                }
                _ = sigusr1.recv() => {
                    info!("Received SIGUSR1, saving scheduler state");
                    {
                        let mut scheduler = mutex.lock().await;
                        scheduler.save_state().await;
                    }
                }
                _ = sighup.recv() => {
                    info!("Received SIGHUP, reloading configuration");
                    {
                        let mut scheduler = mutex.lock().await;
                        match scheduler.reload_config().await {
                            Ok(task_count) => {
                                info!("{} with {} tasks", RELOAD_OK_MSG, task_count);

                                // Re-spawn task loops for the new tasks
                                let pending_tasks = scheduler.pending_tasks.clone();
                                drop(scheduler);

                                Self::spawn_tasks(mutex.clone(), pending_tasks).await;
                            }
                            Err(e) => {
                                error!("{}: {}. Keeping existing config.", RELOAD_FAILED_MSG, e);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Saves state and aborts all task loops and wait handles before exiting.
    async fn shutdown(mutex: &Arc<Mutex<Scheduler>>) {
        let mut scheduler = mutex.lock().await;
        scheduler.save_state().await;

        for handle in &scheduler.task_loop_handles {
            handle.abort();
        }
        for handle in &scheduler.wait_handles {
            handle.abort();
        }
    }

    async fn spawn_tasks(mutex: Arc<Mutex<Scheduler>>, pending_tasks: Vec<Arc<Mutex<PendingTask>>>) {
        for pending_task_mutex in pending_tasks {
            let scheduler_mutex = mutex.clone();

            let name = format!("Task loop of '{}'", pending_task_mutex.lock().await.config.name);

            // Supervised: if the loop panics it is restarted instead of silently stopping the task
            let handle = tokio::spawn(supervisor::supervise_default(name, move || {
                Self::execute_task_loop(pending_task_mutex.clone(), scheduler_mutex.clone())
            }));

            {
                let mut scheduler = mutex.lock().await;
                scheduler.task_loop_handles.push(handle);
            }
        }
    }

    async fn execute_task_loop(pending_task_mutex: Arc<Mutex<PendingTask>>, scheduler_mutex: Arc<Mutex<Scheduler>>) {
        // Wait loop for the right time to execute the task
        loop {
            let pending_task_copy: PendingTask = { pending_task_mutex.lock().await.clone() };
            let start = Instant::now();

            // Check if the task must be executed now
            if !Self::is_task_ready_for_execution(&pending_task_copy) {
                Self::sleep_until_task_is_ready(&pending_task_copy).await;
                continue;
            }

            // Verify that the previous execution is finished, if the config requires it
            if pending_task_copy.config.avoid_overlapping {
                let running_tasks = {
                    let scheduler = scheduler_mutex.lock().await;
                    scheduler
                        .active_tasks
                        .iter()
                        .map(|t| t.config.name.to_string())
                        .collect::<Vec<_>>()
                };

                if Self::is_task_running(&pending_task_copy, &running_tasks) {
                    debug!(
                        "Task '{}' is already running, skipping execution",
                        pending_task_copy.config.name
                    );
                    Self::sleep_until_task_is_ready(&pending_task_copy).await;
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
                match Self::execute_task(&pending_task_copy.config, &alert_config, &config, &sqlite_logger).await {
                    Ok(active_task) => active_task,
                    Err(e) => {
                        error!("{}", e);
                        // Count it as an execution, otherwise the task is still "ready" and the
                        // loop retries immediately, in a busy loop
                        pending_task_mutex.lock().await.last_execution_time = Some(Utc::now());
                        continue;
                    }
                };

            {
                let mut pending_task = pending_task_mutex.lock().await;
                pending_task.last_execution_time = Some(active_task.start_time);
                pending_task.last_pid = Some(active_task.pid);
            }

            let task_id = active_task.id;
            {
                let mut scheduler = scheduler_mutex.lock().await;
                scheduler.active_tasks.push(active_task);
                scheduler.save_state().await;
            }

            // Wait for the task to finish
            Self::wait_for_task(scheduler_mutex.clone(), task_id).await;

            // Sleep at least to the next second to avoid running the task multiple times the same datetime
            if start.elapsed().as_secs() < 1 {
                // Sleeping a whole second makes the time drifts slowly, by sleeping only the
                // millis needed to jump to the next second, we mostly eliminate the drift
                sleep(Self::duration_to_next_second(Utc::now())).await;
            }
        }
    }

    // Wait for the task to end and handle the result
    async fn wait_for_task(mutex: Arc<Mutex<Scheduler>>, task_id: u32) {
        let (child_mutex, time_limit, task_name) = {
            let scheduler = mutex.lock().await;
            let Some(active_task) = scheduler.active_tasks.iter().find(|t| t.id == task_id) else {
                error!("Active task {} not found", task_id);
                return;
            };
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

            let wait_result = if let Some(time_limit) = time_limit {
                tokio::select! {
                    status = child.wait() => status,
                    _ = sleep(Duration::from_secs(time_limit)) => {
                        // Warn the user that the task will be killed
                        warn!("Task '{}' exceeded time limit of {} seconds, sending SIGKILL", task_name, time_limit);

                        if let Err(e) = child.kill().await {
                            error!("Unable to kill task '{}': {}", task_name, e);
                        }
                        // We still need to wait for the process to fully terminate
                        child.wait().await
                    }
                }
            } else {
                child.wait().await
            };
            drop(child);

            // Never panic before removing the task from active_tasks, otherwise a task with
            // avoid_overlapping would be considered running forever
            let exit_status = wait_result.unwrap_or_else(|e| {
                error!("Failed to wait for task '{}': {}", task_name, e);
                ExitStatus::from_raw(255 << 8)
            });

            // Remove the active task, keep the lock only for that: the completion handlers
            // (alerts, sqlite, reading output files) can be slow
            let (active_task, config, sqlite_logger) = {
                let mut scheduler = scheduler_mutex.lock().await;
                let Some(index) = scheduler.active_tasks.iter().position(|t| t.id == task_id) else {
                    error!("Active task {} ('{}') not found", task_id, task_name);
                    return;
                };
                let active_task = scheduler.active_tasks.remove(index);
                (active_task, scheduler.config.clone(), scheduler.sqlite_logger.clone())
            };

            Self::on_task_completed(&active_task, exit_status, &config, &sqlite_logger).await;
        });

        {
            let mut scheduler = mutex.lock().await;
            // Finished handles keep their task allocation alive until dropped, clean them up
            scheduler.wait_handles.retain(|h| !h.is_finished());
            scheduler.wait_handles.push(handle);
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
        let next_run = Self::get_next_execution_time(&task, now, true);
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

        let next_scheduled_run = Self::get_next_execution_time(task, now, true);
        // If the next scheduled run is now, return true
        next_scheduled_run.timestamp() <= now.timestamp()
    }

    /// Time until the next whole second, always between 1 ms and 1 s.
    /// Computed from a single clock reading: two readings could be apart by more than a second
    /// (clock step, VM pause) and give a negative value, which used to become a sleep of ~584 million years
    fn duration_to_next_second(now: DateTime<Utc>) -> Duration {
        let millis = now.timestamp_subsec_millis() as u64; // can be >= 1000 during a leap second
        Duration::from_millis(1000u64.saturating_sub(millis).clamp(1, 1000))
    }

    /// Checks if the task is running.
    /// `active_tasks` is the source of truth: tasks are removed from it when their process ends.
    /// (Checking `last_pid` against the running processes is wrong once the kernel reuses the PID,
    /// the task would be considered running forever)
    fn is_task_running<T: AsRef<str>>(task: &PendingTask, active_tasks: &[T]) -> bool {
        active_tasks.iter().any(|name| name.as_ref() == task.config.name)
    }

    /// Spawns a subprocess to execute the task
    async fn execute_task(
        task_config: &Arc<TaskConfig>,
        alerts: &AlertConfig,
        config: &Config,
        sqlite_logger: &Option<SqliteLogger>,
    ) -> anyhow::Result<ActiveTask> {
        let stdout_path = if let Some(path) = task_config.stdout.as_deref() {
            PathBuf::from(path)
        } else {
            PathBuf::from(format!(
                ".tmp/{}_stdout.log",
                sanitise_file_name::sanitise(&task_config.name)
            ))
        };

        let stderr_path = if let Some(path) = task_config.stderr.as_deref() {
            PathBuf::from(path)
        } else {
            PathBuf::from(format!(
                ".tmp/{}_stderr.log",
                sanitise_file_name::sanitise(&task_config.name)
            ))
        };

        if let Some(path) = stdout_path.parent() {
            if !path.exists() {
                tokio::fs::create_dir_all(path).await.map_err(|e| {
                    anyhow!(
                        "Failed to create stdout parent directory for task '{}': {}",
                        task_config.name,
                        e
                    )
                })?;
            }
        }
        if let Some(path) = stderr_path.parent() {
            if !path.exists() {
                tokio::fs::create_dir_all(path).await.map_err(|e| {
                    anyhow!(
                        "Failed to create stderr parent directory for task '{}': {}",
                        task_config.name,
                        e
                    )
                })?;
            }
        }

        let stdout = match File::create(&stdout_path) {
            Ok(file) => file,
            Err(e) => {
                return Err(anyhow!(
                    "Failed to create {} for task '{}': {}",
                    stdout_path.to_string_lossy(),
                    task_config.name,
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
                    task_config.name,
                    e
                ));
            }
        };

        // Record debug information, to show in case of failure
        let mut debug_info = String::new();

        // Shell to run the command
        let shell = task_config.shell.as_deref().unwrap_or_else(|| "/bin/sh");

        debug_info.push_str(&format!("Cmd: {} -c '{}'\n", shell, task_config.cmd));
        let mut cmd = Command::new(shell);
        cmd.arg("-c");
        cmd.arg(&task_config.cmd);

        // Set environment variables if specified
        if let Some(env) = &task_config.env {
            for (key, value) in env {
                debug_info.push_str(&format!("Env '{}' => '{}'\n", key, value));
                cmd.env(key, value);
            }
            debug!(
                "Set {} environment variables for task '{}'",
                env.len(),
                task_config.name
            );
        }

        // Set working directory if specified
        if let Some(dir) = &task_config.working_directory {
            debug_info.push_str(&format!("Working dir '{}'\n", dir));
            cmd.current_dir(dir);
            debug!("Set runtime directory to '{}' for task '{}'", dir, task_config.name);
        }

        // Set output redirection
        debug_info.push_str(&format!("Stdio '{}'\n", stdout_path.to_string_lossy()));
        debug_info.push_str(&format!("Stderr '{}'\n", stderr_path.to_string_lossy()));
        cmd.stdout(Stdio::from(stdout));
        cmd.stderr(Stdio::from(stderr));

        // Run as another user if specified
        if let Some(run_as) = &task_config.run_as {
            // Only available on Unix-like systems
            if cfg!(unix) {
                let (uid, user_str, gid, group_str) = match Self::get_uid_and_gid(run_as) {
                    Ok((uid, user_str, gid, group_str)) => (uid, user_str, gid, group_str),
                    Err(e) => {
                        return Err(anyhow!(
                            "Failed to get uid and gid for task '{}': {}",
                            task_config.name,
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
                    task_config.name, user_str, group_str
                );
            } else {
                warn!(
                    "Task '{}' cannot run as '{}', unsupported on this platform",
                    task_config.name, run_as
                );
            }
        }

        let clock_time: DateTime<Utc> = Utc::now();
        let now = Instant::now();

        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id().unwrap_or(0);
                let task_id = ACTIVE_TASK_ID_COUNTER.fetch_add(1, Ordering::Relaxed) as u32;
                info!("Task '{}' started with PID: {}", task_config.name, pid);

                // Log execution attempt to SQLite
                if let Some(sqlite_logger) = sqlite_logger {
                    let attempt = ExecutionAttempt {
                        task_name: task_config.name.clone(),
                        task_id,
                        pid,
                        cmd: task_config.cmd.clone(),
                        start_time: clock_time,
                        timezone: task_config.timezone.to_string(),
                        working_directory: task_config.working_directory.clone(),
                        shell: task_config.shell.clone(),
                        run_as: task_config.run_as.clone(),
                        time_limit: task_config.time_limit,
                    };

                    if let Err(e) = sqlite_logger.log_execution_attempt(&attempt).await {
                        error!("Failed to log execution attempt for task '{}': {}", task_config.name, e);
                    }
                }

                Ok(ActiveTask {
                    id: task_id,
                    config: task_config.clone(),
                    pid,
                    start_instant: now,
                    start_time: clock_time,
                    child: Arc::new(Mutex::new(child)),
                    debug_info: debug_info.trim().to_string(),
                    time_limit: task_config.time_limit,
                    stdout_path: stdout_path.clone(),
                    stderr_path: stderr_path.clone(),
                })
            }
            Err(e) => {
                if e.to_string().contains("Operation not permitted") && task_config.run_as.is_some() {
                    debug_info.push_str(&format!(
                        "Note: The task was executed with run_as '{}', make sure the current user '{}' has permission to run as that user",
                        task_config.run_as.as_deref().unwrap(),
                        users::get_current_username().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "<unknown>".to_string())
                    ));
                }

                let details = TaskExecutionDetails {
                    task_name: task_config.name.to_string(),
                    task_id: 0,
                    pid: 0,
                    exit_code: -1,
                    start_time: clock_time,
                    duration: Duration::default(),
                    error_message: format!("Task '{}' failed to start", task_config.name),
                    debug_info: debug_info.trim().to_string(),
                    stdout: String::new(),
                    stderr: e.to_string(),
                };

                Self::on_task_failure(&details, alerts, &task_config.on_failure, sqlite_logger).await;

                Err(anyhow!(
                    "Task '{}' failed to start: {}, Debug info:\n{}",
                    task_config.name,
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
            stdout: tokio::fs::read_to_string(&task.stdout_path).await.unwrap_or_default(),
            stderr: tokio::fs::read_to_string(&task.stderr_path).await.unwrap_or_default(),
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
        send_alerts(alerts.on_failure.iter().chain(task_on_failure), details).await;

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
        send_alerts(alerts.on_success.iter().chain(task_on_success), details).await;

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
    ///
    /// This function must never panic: it runs inside the task loops and `save_state`, a panic there
    /// would silently stop the task (or the whole scheduler).
    pub fn get_next_execution_time(task: &PendingTask, current_date: DateTime<Tz>, allow_now: bool) -> DateTime<Tz> {
        match &task.config.schedule {
            Schedule::Every { interval, aligned } => {
                Self::get_next_every_execution_time(task, *interval, *aligned, current_date, allow_now)
            }
            Schedule::When { time } => Self::get_next_when_execution_time(task, time, current_date, allow_now),
        }
    }

    fn get_next_every_execution_time(
        task: &PendingTask,
        interval: Duration,
        aligned: bool,
        current_date: DateTime<Tz>,
        allow_now: bool,
    ) -> DateTime<Tz> {
        let tz = current_date.timezone();
        let tick = (interval.as_secs() as i64).max(1);
        let at = |ts: i64| tz.timestamp_opt(ts, 0).single().unwrap_or(current_date);
        // First point of the interval grid strictly after `ts`
        let grid_after = |ts: i64| at((ts.div_euclid(tick) + 1) * tick);
        let now_ts = current_date.timestamp();

        let Some(last_execution_time) = task.last_execution_time else {
            // First run
            return if allow_now { current_date } else if aligned { grid_after(now_ts) } else { at(now_ts + tick) };
        };
        let last_ts = last_execution_time.timestamp();

        // Clock went backwards (or bad input), assume no previous run
        if now_ts < last_ts {
            return if allow_now { current_date } else { at(now_ts + 1) };
        }

        let next_date = if aligned { grid_after(last_ts) } else { at(last_ts + tick) };

        // The execution is overdue (the process was suspended, the clock jumped, the loop was delayed...),
        // run it as soon as possible instead of trying to catch up with every missed run
        let next_date = next_date.max(current_date);

        if !allow_now && next_date == current_date {
            if aligned { grid_after(now_ts) } else { at(now_ts + tick) }
        } else {
            next_date
        }
    }

    fn get_next_when_execution_time(
        task: &PendingTask,
        time: &TimePattern,
        current_date: DateTime<Tz>,
        allow_now: bool,
    ) -> DateTime<Tz> {
        let tz = current_date.timezone();
        // Returned when no valid date is found, re-evaluate later instead of running right now
        let fallback = current_date + TimeDelta::days(1);
        let hourly = matches!(time.hour, TimePatternField::Any);
        let mut from = if allow_now { current_date } else { current_date + TimeDelta::seconds(1) };

        // Only DST transitions need more than one iteration
        for _ in 0..8 {
            let Some(naive) = Self::get_next_matching_local_time(time, from.naive_local()) else {
                error!("Task '{}' has no valid next execution time", task.config.name);
                return fallback;
            };

            let next = match tz.from_local_datetime(&naive) {
                LocalResult::Single(dt) => dt,
                // Fall back DST transition, this local time happens twice
                LocalResult::Ambiguous(first, second) => {
                    if first >= from {
                        first
                    } else if hourly {
                        // Tasks that run every hour also run during the repeated hour
                        second
                    } else {
                        // Tasks with a fixed hour already ran in the first occurrence, skip the repeated one
                        from = second.max(from) + TimeDelta::seconds(1);
                        continue;
                    }
                }
                // Spring forward DST transition, this local time doesn't exist
                LocalResult::None => Self::resolve_local_time(tz, naive),
            };

            // Searching by local time from the first occurrence of the repeated hour jumps straight past
            // the second occurrence, tasks that run every hour must also run during it
            if hourly {
                if let Some(repeated_start) = Self::get_repeated_hour_start(tz, from) {
                    if next >= repeated_start && repeated_start > from {
                        from = repeated_start;
                        continue;
                    }
                }
            }

            if next < from {
                error!(
                    "Task '{}' has no valid next execution time (curr = {}, next = {})",
                    task.config.name, from, next
                );
                return fallback;
            }
            return next;
        }

        error!("Task '{}' has no valid next execution time", task.config.name);
        fallback
    }

    /// Finds the first local time >= `start` (truncated to the second) that matches the pattern.
    /// Advances one field at a time, resetting the smaller fields, e.g. for '05:*:00' at 10:30
    /// the result is 05:00 the next day, not 05:30.
    fn get_next_matching_local_time(time: &TimePattern, start: NaiveDateTime) -> Option<NaiveDateTime> {
        let midnight = |date: NaiveDate| date.and_hms_opt(0, 0, 0);
        let mut t = start.with_nanosecond(0)?;

        // Enough to cover centuries of day by day steps, impossible patterns (Feb 30th) end here
        for _ in 0..500_000 {
            let date = t.date();
            if date.year() > 3000 {
                return None;
            }

            if !time.year.matches_value(date.year().max(0) as u32) {
                t = midnight(NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)?)?;
                continue;
            }
            if !time.month.matches_one_based(date.month()) {
                let (y, m) = if date.month() == 12 { (date.year() + 1, 1) } else { (date.year(), date.month() + 1) };
                t = midnight(NaiveDate::from_ymd_opt(y, m, 1)?)?;
                continue;
            }
            if !time.matches_date(date) {
                t = midnight(date.succ_opt()?)?;
                continue;
            }
            if !time.hour.matches_value(t.hour()) {
                t = date.and_hms_opt(t.hour(), 0, 0)? + TimeDelta::hours(1);
                continue;
            }
            if !time.minute.matches_value(t.minute()) {
                t = date.and_hms_opt(t.hour(), t.minute(), 0)? + TimeDelta::minutes(1);
                continue;
            }
            if !time.second.matches_value(t.second()) {
                t += TimeDelta::seconds(1);
                continue;
            }
            return Some(t);
        }
        None
    }

    /// If `dt` is in the first occurrence of a local time that happens twice (fall back DST transition),
    /// returns the instant where the repeated local time range starts again (the transition instant)
    fn get_repeated_hour_start(tz: Tz, dt: DateTime<Tz>) -> Option<DateTime<Tz>> {
        let local = dt.naive_local();
        let LocalResult::Ambiguous(first, _) = tz.from_local_datetime(&local) else {
            return None;
        };
        if first != dt {
            return None;
        }

        // Find the start of the ambiguous range, transitions happen at minute boundaries
        let mut start = local.with_second(0)?.with_nanosecond(0)?;
        for _ in 0..(24 * 60) {
            let prev = start - TimeDelta::minutes(1);
            if !matches!(tz.from_local_datetime(&prev), LocalResult::Ambiguous(..)) {
                break;
            }
            start = prev;
        }
        tz.from_local_datetime(&start).latest()
    }

    /// Converts a local time to an instant, never fails:
    /// - Ambiguous times (fall back DST transition) resolve to the first occurrence
    /// - Non-existent times (spring forward DST transition) are shifted forward by the length of the gap,
    ///   e.g. 02:30 becomes 03:30 when the clock jumps from 02:00 to 03:00
    fn resolve_local_time(tz: Tz, naive: NaiveDateTime) -> DateTime<Tz> {
        match tz.from_local_datetime(&naive) {
            LocalResult::Single(dt) => dt,
            LocalResult::Ambiguous(first, _) => first,
            LocalResult::None => {
                // Use the offset in effect before the gap (transitions are months apart)
                let offset = tz.offset_from_utc_datetime(&(naive - TimeDelta::days(1))).fix();
                let utc = naive - TimeDelta::seconds(offset.local_minus_utc() as i64);
                tz.from_utc_datetime(&utc)
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
}

impl PendingTask {
    pub fn new(config: Arc<TaskConfig>) -> Self {
        PendingTask {
            config,
            last_execution_time: None,
            last_pid: None,
            retries: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::shorthand::parse_shorthand;

    fn task(schedule: Schedule, tz: Tz) -> PendingTask {
        PendingTask::new(Arc::new(TaskConfig {
            name: "t".into(),
            cmd: "true".into(),
            schedule,
            timezone: tz,
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
        }))
    }

    fn when(pattern: &str, tz: &str) -> PendingTask {
        task(
            Schedule::When { time: parse_shorthand(pattern).unwrap() },
            tz.parse().unwrap(),
        )
    }

    fn every(secs: u64, aligned: bool) -> PendingTask {
        task(
            Schedule::Every { interval: Duration::from_secs(secs), aligned },
            Tz::UTC,
        )
    }

    fn madrid() -> Tz {
        "Europe/Madrid".parse().unwrap()
    }

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, s).unwrap()
    }

    fn next(t: &PendingTask, now: DateTime<Tz>, allow_now: bool) -> DateTime<Utc> {
        Scheduler::get_next_execution_time(t, now, allow_now).with_timezone(&Utc)
    }

    // ── DST: spring forward (Europe/Madrid 2026-03-29, 02:00 CET -> 03:00 CEST) ──

    #[test]
    fn when_every_minute_across_spring_forward_gap() {
        let t = when("* *-*-* *:*:0", "Europe/Madrid");
        // 01:59:30 CET = 00:59:30 UTC. 02:00 local does not exist, next valid instant is 03:00 CEST
        let now = madrid().with_ymd_and_hms(2026, 3, 29, 1, 59, 30).unwrap();
        assert_eq!(next(&t, now, false), utc(2026, 3, 29, 1, 0, 0));
    }

    #[test]
    fn when_daily_inside_spring_forward_gap_runs_after_the_gap() {
        let t = when("* *-*-* 02:30:00", "Europe/Madrid");
        let now = madrid().with_ymd_and_hms(2026, 3, 29, 1, 0, 0).unwrap();
        // 02:30 does not exist that day; shifted by the gap length -> 03:30 CEST = 01:30 UTC
        assert_eq!(next(&t, now, false), utc(2026, 3, 29, 1, 30, 0));
    }

    // ── DST: fall back (Europe/Madrid 2026-10-25, 03:00 CEST -> 02:00 CET) ──

    #[test]
    fn when_every_minute_enters_ambiguous_hour() {
        let t = when("* *-*-* *:*:0", "Europe/Madrid");
        let now = madrid().with_ymd_and_hms(2026, 10, 25, 1, 59, 30).unwrap();
        // First occurrence of 02:00 (CEST) = 00:00 UTC
        assert_eq!(next(&t, now, false), utc(2026, 10, 25, 0, 0, 0));
    }

    #[test]
    fn when_every_minute_keeps_running_during_repeated_hour() {
        let t = when("* *-*-* *:*:0", "Europe/Madrid");
        // 02:59:30 CEST = 00:59:30 UTC, the next minute is 02:00 CET = 01:00 UTC
        let now = utc(2026, 10, 25, 0, 59, 30).with_timezone(&madrid());
        assert_eq!(next(&t, now, false), utc(2026, 10, 25, 1, 0, 0));
        // Inside the repeated hour: 02:10:30 CET = 01:10:30 UTC
        let now = utc(2026, 10, 25, 1, 10, 30).with_timezone(&madrid());
        assert_eq!(next(&t, now, false), utc(2026, 10, 25, 1, 11, 0));
    }

    #[test]
    fn when_daily_in_ambiguous_hour_runs_only_once() {
        let t = when("* *-*-* 02:30:00", "Europe/Madrid");
        let now = madrid().with_ymd_and_hms(2026, 10, 25, 1, 0, 0).unwrap();
        // First occurrence: 02:30 CEST = 00:30 UTC
        assert_eq!(next(&t, now, false), utc(2026, 10, 25, 0, 30, 0));
        // Right after running, the repeated 02:30 CET must be skipped -> next day 02:30 CET
        let now = utc(2026, 10, 25, 0, 30, 1).with_timezone(&madrid());
        assert_eq!(next(&t, now, false), utc(2026, 10, 26, 1, 30, 0));
    }

    #[test]
    fn when_hourly_runs_in_both_repeated_hours() {
        let t = when("* *-*-* *:30:00", "Europe/Madrid");
        let now = utc(2026, 10, 25, 0, 30, 1).with_timezone(&madrid());
        // 02:30 CEST already ran, 02:30 CET is a different hour and must run
        assert_eq!(next(&t, now, false), utc(2026, 10, 25, 1, 30, 0));
    }

    // ── Year / month boundaries ──

    #[test]
    fn when_every_minute_across_new_year() {
        let t = when("* *-*-* *:*:0", "UTC");
        let now = Tz::UTC.with_ymd_and_hms(2026, 12, 31, 23, 59, 30).unwrap();
        assert_eq!(next(&t, now, false), utc(2027, 1, 1, 0, 0, 0));
    }

    #[test]
    fn when_day_that_does_not_exist_in_next_month() {
        // From Jan 31st, February has no 31st, the next one is in March
        let t = when("* *-*-31 10:00:00", "UTC");
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 31, 12, 0, 0).unwrap();
        assert_eq!(next(&t, now, false), utc(2026, 3, 31, 10, 0, 0));
    }

    #[test]
    fn when_impossible_date_does_not_run_now() {
        // February 30th never exists
        let t = when("* *-02-30 10:00:00", "UTC");
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        assert!(next(&t, now, true) > now.with_timezone(&Utc));
    }

    // ── every ──

    #[test]
    fn every_aligned_next_grid_point() {
        let mut t = every(60, true);
        t.last_execution_time = Some(utc(2026, 1, 1, 10, 0, 0));
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 10, 0, 1).unwrap();
        assert_eq!(next(&t, now, true), utc(2026, 1, 1, 10, 1, 0));
    }

    #[test]
    fn every_aligned_overdue_does_not_panic() {
        let mut t = every(60, true);
        t.last_execution_time = Some(utc(2026, 1, 1, 10, 0, 0));
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 10, 1, 30).unwrap();
        // Overdue: run now
        assert_eq!(next(&t, now, true), utc(2026, 1, 1, 10, 1, 30));
        // Not allowed to run now: next grid point
        assert_eq!(next(&t, now, false), utc(2026, 1, 1, 10, 2, 0));
    }

    #[test]
    fn every_not_aligned_overdue_does_not_panic() {
        let mut t = every(60, false);
        t.last_execution_time = Some(utc(2026, 1, 1, 10, 0, 0));
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 10, 5, 0).unwrap();
        assert_eq!(next(&t, now, true), utc(2026, 1, 1, 10, 5, 0));
        assert_eq!(next(&t, now, false), utc(2026, 1, 1, 10, 6, 0));
    }

    #[test]
    fn every_not_aligned_regular() {
        let mut t = every(60, false);
        t.last_execution_time = Some(utc(2026, 1, 1, 10, 0, 0));
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 10, 0, 20).unwrap();
        assert_eq!(next(&t, now, true), utc(2026, 1, 1, 10, 1, 0));
    }


    /// Simulates consecutive executions between two instants, checking that time always moves forward
    fn simulate(t: &mut PendingTask, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
        let tz = t.config.timezone;
        let mut runs = vec![];
        let mut now = from.with_timezone(&tz);
        loop {
            let run = Scheduler::get_next_execution_time(t, now, runs.is_empty()).with_timezone(&Utc);
            if run >= to {
                return runs;
            }
            if let Some(prev) = runs.last() {
                assert!(run > *prev, "time went backwards: {} -> {}", prev, run);
            }
            runs.push(run);
            t.last_execution_time = Some(run);
            now = run.with_timezone(&tz);
        }
    }

    #[test]
    fn simulate_dst_days_every_minute() {
        // Local days in Europe/Madrid: spring forward has 23h, fall back has 25h
        let mut t = when("* *-*-* *:*:0", "Europe/Madrid");
        let runs = simulate(&mut t, utc(2026, 3, 28, 23, 0, 0), utc(2026, 3, 29, 22, 0, 0));
        assert_eq!(runs.len(), 23 * 60);
        let runs = simulate(&mut t, utc(2026, 10, 24, 22, 0, 0), utc(2026, 10, 25, 23, 0, 0));
        assert_eq!(runs.len(), 25 * 60);
    }

    #[test]
    fn simulate_dst_days_daily_at_ambiguous_or_missing_time() {
        let mut t = when("* *-*-* 02:30:00", "Europe/Madrid");
        // One run per local day, including the DST days
        let runs = simulate(&mut t, utc(2026, 3, 27, 23, 0, 0), utc(2026, 3, 30, 22, 0, 0));
        assert_eq!(runs.len(), 3, "{:?}", runs);
        let runs = simulate(&mut t, utc(2026, 10, 23, 22, 0, 0), utc(2026, 10, 26, 23, 0, 0));
        assert_eq!(runs.len(), 3, "{:?}", runs);
    }

    #[test]
    fn simulate_new_year_and_leap_day() {
        let mut t = when("* *-*-* *:0:0", "UTC");
        let runs = simulate(&mut t, utc(2026, 12, 31, 20, 0, 0), utc(2027, 1, 1, 4, 0, 0));
        assert_eq!(runs.len(), 8);
        let runs = simulate(&mut t, utc(2028, 2, 28, 0, 0, 0), utc(2028, 3, 2, 0, 0, 0));
        assert_eq!(runs.len(), 3 * 24);
    }

    #[test]
    fn simulate_every_aligned() {
        let mut t = every(300, true);
        let runs = simulate(&mut t, utc(2026, 1, 1, 10, 2, 0), utc(2026, 1, 1, 11, 0, 0));
        // First run immediately, then aligned to 5 minute boundaries
        assert_eq!(runs[0], utc(2026, 1, 1, 10, 2, 0));
        assert_eq!(runs[1], utc(2026, 1, 1, 10, 5, 0));
        assert!(runs[1..].iter().all(|r| r.timestamp() % 300 == 0));
        assert_eq!(runs.len(), 12);
    }

    // ── Field semantics: day and month are 1-based, day of week follows cron (Sun = 0) ──

    #[test]
    fn when_day_and_month_are_one_based() {
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        // /etc/cron.monthly pattern used by --cron-compat
        assert_eq!(next(&when("* *-*-1 06:52:00", "UTC"), now, false), utc(2026, 1, 1, 6, 52, 0));
        assert_eq!(next(&when("* *-02-* 10:00:00", "UTC"), now, false), utc(2026, 2, 1, 10, 0, 0));
        assert_eq!(next(&when("* *-12-31 10:00:00", "UTC"), now, false), utc(2026, 12, 31, 10, 0, 0));
        assert_eq!(next(&when("* *-*-01..04 12:00:00", "UTC"), now, false), utc(2026, 1, 1, 12, 0, 0));
    }

    #[test]
    fn when_day_of_week() {
        // 2026-01-01 is a Thursday
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        assert_eq!(next(&when("Mon *-*-* 00:00:00", "UTC"), now, false), utc(2026, 1, 5, 0, 0, 0));
        assert_eq!(next(&when("Sun *-*-* 00:00:00", "UTC"), now, false), utc(2026, 1, 4, 0, 0, 0));
        assert_eq!(next(&when("[Mon,Fri] *-*-* 00:00:00", "UTC"), now, false), utc(2026, 1, 2, 0, 0, 0));
        // Numeric, cron style: 0 and 7 are Sunday
        assert_eq!(next(&when("7 *-*-* 00:00:00", "UTC"), now, false), utc(2026, 1, 4, 0, 0, 0));
        // Friday 13th
        assert_eq!(next(&when("Fri *-*-13 09:00:00", "UTC"), now, false), utc(2026, 2, 13, 9, 0, 0));
    }

    #[test]
    fn when_ratios() {
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 10, 2, 0).unwrap();
        // Documented: '*/5+1' -> 1, 6, 11, ...
        assert_eq!(next(&when("* *-*-* *:*/5+1:00", "UTC"), now, false), utc(2026, 1, 1, 10, 6, 0));
        assert_eq!(next(&when("* *-*-* *:*/5:00", "UTC"), now, false), utc(2026, 1, 1, 10, 5, 0));
        // Documented: months '*/2' -> January, March, May, ...
        let feb = Tz::UTC.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();
        assert_eq!(next(&when("* *-*/2-1 00:00:00", "UTC"), feb, false), utc(2026, 3, 1, 0, 0, 0));
        // Days '*/2' -> 1, 3, 5, ...
        let day2 = Tz::UTC.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        assert_eq!(next(&when("* *-*-*/2 00:00:00", "UTC"), day2, false), utc(2026, 1, 3, 0, 0, 0));
    }

    #[test]
    fn when_smaller_fields_reset_when_a_bigger_one_advances() {
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 10, 30, 15).unwrap();
        // Every minute of the 05:xx hour: next is 05:00:00 tomorrow, not 05:30
        assert_eq!(next(&when("* *-*-* 05:*:00", "UTC"), now, false), utc(2026, 1, 2, 5, 0, 0));
        // Every second during minute 45 of each hour
        assert_eq!(next(&when("* *-*-* *:45:*", "UTC"), now, false), utc(2026, 1, 1, 10, 45, 0));
        // Specific year in the future
        assert_eq!(next(&when("* 2028-*-* *:*:*", "UTC"), now, false), utc(2028, 1, 1, 0, 0, 0));
    }

    #[test]
    fn when_year_in_the_past_never_runs() {
        let now = Tz::UTC.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert!(next(&when("* 2025-*-* *:*:*", "UTC"), now, true) > now.with_timezone(&Utc));
    }

    // ── Misc ──

    #[test]
    fn duration_to_next_second_is_never_huge() {
        let base = utc(2026, 1, 1, 0, 0, 0);
        assert_eq!(Scheduler::duration_to_next_second(base), Duration::from_millis(1000));
        assert_eq!(
            Scheduler::duration_to_next_second(base + TimeDelta::milliseconds(250)),
            Duration::from_millis(750)
        );
        assert_eq!(
            Scheduler::duration_to_next_second(base + TimeDelta::milliseconds(999)),
            Duration::from_millis(1)
        );
        // Leap second representation (nanos >= 1e9)
        let leap = base.with_nanosecond(1_500_000_000).unwrap();
        assert_eq!(Scheduler::duration_to_next_second(leap), Duration::from_millis(1));
    }

    #[test]
    fn task_with_reused_pid_is_not_considered_running() {
        let mut t = every(60, false);
        // PID 1 always exists
        t.last_pid = Some(1);
        assert!(!Scheduler::is_task_running(&t, &Vec::<String>::new()));
        assert!(Scheduler::is_task_running(&t, &["t".to_string()]));
    }
}
