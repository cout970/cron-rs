use std::fs::File;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use chrono::{DateTime, Datelike, Local, Timelike};
use signal_hook::consts::SIGTERM;
use sysinfo::{Pid, System};
use crate::config::{Schedule, Task, TimePatternField};
use chrono::TimeZone;
use chrono_tz::Tz;

struct PendingTask {
    config: Task,
    last_execution: Option<Instant>,
    last_pid: Option<u32>,
    retries: u32,
}

pub fn start_scheduler(tasks: Vec<Task>) -> anyhow::Result<()> {
    // Detect CTRL+C to stop the infinite loop
    let term = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGTERM, Arc::clone(&term))?;

    let mut min_interval = Duration::from_secs(60);

    for task in &tasks {
        match &task.schedule {
            Schedule::Every { interval } => {
                if *interval < min_interval {
                    min_interval = *interval;
                }
            }
            Schedule::When { time } => {
                match time.second {
                    TimePatternField::Any => {
                        // nothing to do
                    }
                    _ => {
                        min_interval = Duration::from_secs(1);
                    }
                }
            }
        }
    }

    let mut pending_tasks = tasks.into_iter().map(|task| PendingTask {
        config: task,
        last_execution: None,
        last_pid: None,
        retries: 0,
    }).collect::<Vec<_>>();

    while !term.load(Ordering::Relaxed) {
        run_pending_tasks(&mut pending_tasks);
        thread::sleep(min_interval);
    }
    Ok(())
}

fn run_pending_tasks(tasks: &mut [PendingTask]) {
    let now = Instant::now();

    for task in tasks {
        let date: DateTime<Tz> = task.config.timezone.from_utc_datetime(&chrono::Utc::now().naive_utc());

        if !is_task_scheduled(task, now, date) {
            continue;
        }

        // TODO: Implement retries
        // TODO: Add avoid_overlapping option to Task
        // if task.config.avoid_overlapping {
        if let Some(pid) = task.last_pid {
            let sys = System::new_all();
            if sys.process(Pid::from_u32(pid)).is_some() {
                println!("Task '{}' is already running. 'avoid_overlapping' option forbids concurrent executions.", task.config.name);
                continue;
            }
        }
        // }

        println!("Starting task: {}", task.config.name);
        let stdout = File::create("stdout.log").unwrap();
        let stderr = File::create("stderr.log").unwrap();

        let mut cmd = Command::new("sh");
        cmd.arg("-c");
        cmd.arg(&task.config.cmd);

        // TODO: Add runtime_dir to Task
        // cmd.current_dir(&task.config.runtime_dir);

        // TODO: Allow to change the stdout and stderr redirection
        cmd.stdout(Stdio::from(stdout));
        cmd.stderr(Stdio::from(stderr));

        // Run without blocking the scheduler
        let child = cmd.spawn().expect("Unable to execute command");

        task.last_execution = Some(now);
        task.last_pid = Some(child.id());
    }
}

fn is_task_scheduled(task: &PendingTask, now: Instant, date: DateTime<Tz>) -> bool {
    match &task.config.schedule {
        Schedule::Every { interval } => {
            if let Some(last_execution) = task.last_execution {
                now.duration_since(last_execution) >= *interval
            } else {
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

            if !matches_time_pattern(&time.second, second) {
                return false;
            }
            if !matches_time_pattern(&time.minute, minute) {
                return false;
            }
            if !matches_time_pattern(&time.hour, hour) {
                return false;
            }
            if !matches_time_pattern(&time.day_of_week, day_of_week) {
                return false;
            }
            if !matches_time_pattern(&time.day, day) {
                return false;
            }
            if !matches_time_pattern(&time.month, month) {
                return false;
            }
            if !matches_time_pattern(&time.year, year as u32) {
                return false;
            }

            true
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
