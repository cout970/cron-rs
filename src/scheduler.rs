use std::fs::File;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use chrono::{DateTime, Datelike, Local, Timelike};
use signal_hook::consts::SIGINT;
use sysinfo::{Pid, System};
use crate::config::{Schedule, Task, TimePatternField};
use chrono::TimeZone;
use chrono_tz::Tz;
use std::process::Child;
use sysinfo::ProcessStatus;
use log::{debug, error, info, warn};

#[derive(Debug, Clone)]
struct PendingTask {
    config: Task,
    last_execution: Option<Instant>,
    last_pid: Option<u32>,
    retries: u32,
}

#[derive(Debug)]
struct ActiveTask {
    config: Task,
    pid: u32,
    start_time: Instant,
    child: Child, 
}

pub fn start_scheduler(tasks: Vec<Task>) -> anyhow::Result<()> {
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
                    debug!("Updated min_interval to {:?} for task '{}'", interval, task.name);
                }
            }
            Schedule::When { time } => {
                match time.second {
                    TimePatternField::Any => {
                        // nothing to do
                    }
                    _ => {
                        min_interval = Duration::from_secs(1);
                        debug!("Task '{}' requires second-level precision, setting min_interval to 1s", task.name);
                    }
                }
            }
        }
    }

    info!("Minimum check interval set to {:?}", min_interval);

    let mut pending_tasks = tasks.into_iter().map(|task| {
        debug!("Initializing task '{}' with timezone {:?}", task.name, task.timezone);
        PendingTask {
            config: task,
            last_execution: None,
            last_pid: None,
            retries: 0,
        }
    }).collect::<Vec<_>>();

    let mut active_tasks = Vec::new();

    info!("Starting main scheduler loop");
    while !term.load(Ordering::Relaxed) {
        remove_inactive_tasks(&mut active_tasks);
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
                info!("Task {} finished with status: {}, elapsed {} seconds", task.config.name, status, task.start_time.elapsed().as_secs());
                dead_pids.push(task.pid);
            },
            Ok(None) => {
                // Task is still running, do nothing
            },
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
        let date: DateTime<Tz> = task.config.timezone.from_utc_datetime(&chrono::Utc::now().naive_utc());
        debug!("Current time in task '{}' timezone: {}", task.config.name, date);

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
            if active_tasks.iter().any(|active| active.config.name == task.config.name) {
                warn!("Task '{}' is already running. Skipping execution due to avoid_overlapping=true.", 
                    task.config.name);
                continue;
            }
        }

        execute_task(task, now, active_tasks);
    }
}

fn execute_task(task: &mut PendingTask, now: Instant, active_tasks: &mut Vec<ActiveTask>) {
    let stdout = match File::create("stdout.log") {
        Ok(file) => file,
        Err(e) => {
            error!("Failed to create stdout.log for task '{}': {}", task.config.name, e);
            return;
        }
    };
    let stderr = match File::create("stderr.log") {
        Ok(file) => file,
        Err(e) => {
            error!("Failed to create stderr.log for task '{}': {}", task.config.name, e);
            return;
        }
    };

    let mut cmd = Command::new("sh");
    cmd.arg("-c");
    cmd.arg(&task.config.cmd);

    // TODO: Add runtime_dir to Task
    // cmd.current_dir(&task.config.runtime_dir);

    // TODO: Allow to change the stdout and stderr redirection
    cmd.stdout(Stdio::from(stdout));
    cmd.stderr(Stdio::from(stderr));

    match cmd.spawn() {
        Ok(child) => {
            info!("Task '{}' started with PID: {}", task.config.name, child.id());
            task.last_execution = Some(now);
            task.last_pid = Some(child.id());
            active_tasks.push(ActiveTask {
                config: task.config.clone(),
                pid: child.id(),
                start_time: now,
                child,
            });
        }
        Err(e) => {
            error!("Task '{}' failed to start: {}", task.config.name, e);
        }
    }
}

fn is_task_scheduled(task: &PendingTask, now: Instant, date: DateTime<Tz>) -> bool {
    match &task.config.schedule {
        Schedule::Every { interval } => {
            if let Some(last_execution) = task.last_execution {
                let elapsed = now.duration_since(last_execution);
                let should_run = elapsed >= *interval;
                debug!("Task '{}' interval check: elapsed={:?}, interval={:?}, should_run={}", 
                    task.config.name, elapsed, interval, should_run);
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

            let matches = matches_time_pattern(&time.second, second) &&
                matches_time_pattern(&time.minute, minute) &&
                matches_time_pattern(&time.hour, hour) &&
                matches_time_pattern(&time.day_of_week, day_of_week) &&
                matches_time_pattern(&time.day, day) &&
                matches_time_pattern(&time.month, month) &&
                matches_time_pattern(&time.year, year as u32);

            if matches {
                debug!("Task '{}' matches schedule at {}", task.config.name, date);
            } else {
                debug!("Task '{}' does not match schedule at {}", task.config.name, date);
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
