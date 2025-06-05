use crate::config::{Schedule, TaskConfig, TimePatternField};
use chrono::TimeZone;
use chrono::{DateTime, Datelike, Local, Timelike};
use chrono_tz::Tz;
use log::{debug, error, info, warn};
use signal_hook::consts::SIGINT;
use std::collections::HashMap;
use std::fs::File;
use std::ops::{Add, Deref};
use std::os::unix::prelude::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::{Gid, Groups, ProcessStatus, User, Users};
use sysinfo::{Pid, System};

#[derive(Debug, Clone)]
struct PendingTask {
    config: TaskConfig,
    last_execution: Option<Instant>,
    last_pid: Option<u32>,
    retries: u32,
}

#[derive(Debug)]
struct ActiveTask {
    config: TaskConfig,
    pid: u32,
    start_time: Instant,
    child: Child,
    time_limit: Option<u64>,
}

pub fn start_scheduler(tasks: Vec<TaskConfig>) -> anyhow::Result<()> {
    info!("Initializing scheduler with {} tasks", tasks.len());

    // Detect CTRL+C to stop the infinite loop
    let term = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&term))?;
    info!("Signal handler registered for graceful shutdown");

    let mut min_interval = Duration::from_secs(60);
    debug!("Calculating minimum interval for task checks");

    for task in &tasks {
        match &task.schedule {
            Schedule::Every { interval } => {
                if *interval < min_interval {
                    min_interval = *interval;
                    debug!(
                        "Updated min_interval to {:?} for task '{}'",
                        interval, task.name
                    );
                }
            }
            Schedule::When { time } => {
                match time.second {
                    TimePatternField::Any => {
                        // nothing to do
                    }
                    _ => {
                        min_interval = Duration::from_secs(1);
                        debug!(
                            "Task '{}' requires second-level precision, setting min_interval to 1s",
                            task.name
                        );
                    }
                }
            }
        }
    }

    info!("Minimum check interval set to {:?}", min_interval);

    let mut pending_tasks = tasks
        .into_iter()
        .map(|task| {
            debug!(
                "Initializing task '{}' with timezone {:?}",
                task.name, task.timezone
            );
            PendingTask {
                config: task,
                last_execution: None,
                last_pid: None,
                retries: 0,
            }
        })
        .collect::<Vec<_>>();

    let mut active_tasks = Vec::new();

    info!("Starting main scheduler loop");
    while !term.load(Ordering::Relaxed) {
        remove_inactive_tasks(&mut active_tasks);
        check_time_limits(&mut active_tasks);
        run_pending_tasks(&mut pending_tasks, &mut active_tasks);
        thread::sleep(min_interval);
    }
    info!("Scheduler shutdown initiated");
    Ok(())
}

fn remove_inactive_tasks(active_tasks: &mut Vec<ActiveTask>) {
    let mut dead_pids = Vec::new();

    for task in &mut *active_tasks {
        match task.child.try_wait() {
            Ok(Some(status)) => {
                info!(
                    "Task {} finished with status: {}, elapsed {} seconds",
                    task.config.name,
                    status,
                    task.start_time.elapsed().as_secs()
                );
                dead_pids.push(task.pid);
            }
            Ok(None) => {
                // Task is still running, do nothing
            }
            Err(e) => {
                error!("error attempting to wait: {}", e);
                dead_pids.push(task.pid);
            }
        }
    }

    active_tasks.retain(|task| !dead_pids.contains(&task.pid));
}

fn run_pending_tasks(tasks: &mut [PendingTask], active_tasks: &mut Vec<ActiveTask>) {
    let now = Instant::now();
    debug!("Checking {} tasks for execution", tasks.len());

    for task in tasks {
        let date: DateTime<Tz> = task
            .config
            .timezone
            .from_utc_datetime(&chrono::Utc::now().naive_utc());
        debug!(
            "Current time in task '{}' timezone: {}",
            task.config.name, date
        );

        if !is_task_scheduled(task, now, date) {
            continue;
        }

        if task.config.avoid_overlapping {
            let sys = System::new_all();
            if let Some(pid) = task.last_pid {
                if sys.process(Pid::from_u32(pid)).is_some() {
                    warn!("Task '{}' is already running (PID: {}). Skipping execution due to avoid_overlapping=true.", 
                        task.config.name, pid);
                    continue;
                }
            }

            // Check if task is in active_tasks
            if active_tasks
                .iter()
                .any(|active| active.config.name == task.config.name)
            {
                warn!("Task '{}' is already running. Skipping execution due to avoid_overlapping=true.", 
                    task.config.name);
                continue;
            }
        }

        execute_task(task, now, active_tasks);
    }
}

fn execute_task(task: &mut PendingTask, now: Instant, active_tasks: &mut Vec<ActiveTask>) {
    let stdout_path = if let Some(path) = task.config.stdout.as_deref() {
        PathBuf::from(path)
    } else {
        PathBuf::from(format!(
            ".tmp/{}_stdout.log",
            task_name_to_filename(&task.config.name)
        ))
    };

    let stderr_path = if let Some(path) = task.config.stderr.as_deref() {
        PathBuf::from(path)
    } else {
        PathBuf::from(format!(
            ".tmp/{}_stderr.log",
            task_name_to_filename(&task.config.name)
        ))
    };

    if let Some(path) = stdout_path.parent() {
        if !path.exists() {
            std::fs::create_dir_all(path).expect(
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
            std::fs::create_dir_all(path).expect(
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
            error!(
                "Failed to create {} for task '{}': {}",
                stdout_path.to_string_lossy(),
                task.config.name,
                e
            );
            return;
        }
    };
    let stderr = match File::create(&stderr_path) {
        Ok(file) => file,
        Err(e) => {
            error!(
                "Failed to create {} for task '{}': {}",
                stderr_path.to_string_lossy(),
                task.config.name,
                e
            );
            return;
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
        debug!(
            "Set runtime directory to '{}' for task '{}'",
            dir, task.config.name
        );
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
            let (uid, user_str, gid, group_str) = match get_uid_and_gid(run_as) {
                Ok((uid, user_str, gid, group_str)) => (uid, user_str, gid, group_str),
                Err(e) => {
                    error!("Failed to get uid and gid for task '{}': {}", task.config.name, e);
                    return;
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

    match cmd.spawn() {
        Ok(child) => {
            info!(
                "Task '{}' started with PID: {}",
                task.config.name,
                child.id()
            );
            task.last_execution = Some(now);
            task.last_pid = Some(child.id());
            active_tasks.push(ActiveTask {
                config: task.config.clone(),
                pid: child.id(),
                start_time: now,
                child,
                time_limit: task.config.time_limit,
            });
        }
        Err(e) => {
            if e.to_string().contains("Operation not permitted") && task.config.run_as.is_some() {
                debug_info.push_str(&format!(
                    "Note: The task was executed with run_as '{}', make sure the current user '{}' has permission to run as that user",
                    task.config.run_as.as_deref().unwrap(),
                    users::get_current_username().map(|s|s.to_string_lossy().to_string()).unwrap_or_else(|| "<unknown>".to_string())
                ));
            }
            error!(
                "Task '{}' failed to start: {}, Debug info:\n{}",
                task.config.name, e, debug_info
            );
        }
    }
}

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
    
    Ok((uid.add(0u32), user_str.to_string(), gid.add(0u32), group_str.to_string()))
}

fn is_task_scheduled(task: &PendingTask, now: Instant, date: DateTime<Tz>) -> bool {
    match &task.config.schedule {
        Schedule::Every { interval } => {
            if let Some(last_execution) = task.last_execution {
                let elapsed = now.duration_since(last_execution);
                let should_run = elapsed >= *interval;
                debug!(
                    "Task '{}' interval check: elapsed={:?}, interval={:?}, should_run={}",
                    task.config.name, elapsed, interval, should_run
                );
                should_run
            } else {
                debug!("Task '{}' first execution", task.config.name);
                true
            }
        }
        Schedule::When { time } => {
            let second = date.second();
            let minute = date.minute();
            let hour = date.hour();
            let day_of_week = date.weekday().num_days_from_sunday();
            let day = date.day();
            let month = date.month();
            let year = date.year();

            let matches = matches_time_pattern(&time.second, second)
                && matches_time_pattern(&time.minute, minute)
                && matches_time_pattern(&time.hour, hour)
                && matches_time_pattern(&time.day_of_week, day_of_week)
                && matches_time_pattern(&time.day, day)
                && matches_time_pattern(&time.month, month)
                && matches_time_pattern(&time.year, year as u32);

            if matches {
                debug!("Task '{}' matches schedule at {}", task.config.name, date);
            } else {
                debug!(
                    "Task '{}' does not match schedule at {}",
                    task.config.name, date
                );
            }

            matches
        }
    }
}

fn matches_time_pattern(pattern: &TimePatternField, value: u32) -> bool {
    match pattern {
        TimePatternField::Any => true,
        TimePatternField::Value(v) => value == *v,
        TimePatternField::Range(start, end) => value >= *start && value <= *end,
        TimePatternField::List(values) => values.contains(&value),
        TimePatternField::Ratio(divisor, offset) => value % divisor + *offset == 0,
    }
}

fn check_time_limits(active_tasks: &mut Vec<ActiveTask>) {
    let now = Instant::now();
    let mut to_remove = Vec::new();

    for (i, task) in active_tasks.iter_mut().enumerate() {
        if let Some(time_limit) = task.time_limit {
            let elapsed = now.duration_since(task.start_time).as_secs();
            if elapsed >= time_limit {
                warn!(
                    "Task '{}' (PID: {}) exceeded time limit of {} seconds",
                    task.config.name, task.pid, time_limit
                );

                task.child.kill().expect("Failed to kill task");

                to_remove.push(i);
            }
        }
    }

    // Remove terminated tasks in reverse order to maintain indices
    for i in to_remove.into_iter().rev() {
        active_tasks.remove(i);
    }
}

fn task_name_to_filename(name: &str) -> String {
    sanitise_file_name::sanitise(name)
}
