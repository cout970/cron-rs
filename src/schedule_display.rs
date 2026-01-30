use crate::config::{Config, Schedule, TaskConfig, TimePatternField};
use crate::scheduler::{PendingTask, Scheduler};
use chrono::{DateTime, Datelike, Duration, TimeDelta, TimeZone, Timelike};
use chrono_tz::Tz;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

pub struct ScheduleDisplay;

impl ScheduleDisplay {
    /// Display all task schedules in a human-readable format
    pub fn display_schedules(config: &Config) -> String {
        let mut output = String::new();
        output.push_str("Task Schedules:\n");
        output.push_str("==============\n\n");

        for task in &config.tasks {
            output.push_str(&Self::display_task_schedule(task));
            output.push_str("\n");
        }

        output
    }

    /// Display a single task's schedule
    pub fn display_task_schedule(task: &TaskConfig) -> String {
        let mut output = String::new();
        output.push_str(&format!("Task: {}\n", task.name));
        output.push_str(&format!("Command: {}\n", task.cmd));
        output.push_str(&format!("Timezone: {}\n", task.timezone));

        match &task.schedule {
            Schedule::Every { interval, aligned } => {
                let aligned_str = if *aligned { " (aligned)" } else { "" };
                output.push_str(&format!(
                    "Schedule: Every {}{}\n",
                    crate::utils::format_duration(*interval),
                    aligned_str
                ));
            }
            Schedule::When { time } => {
                output.push_str(&format!("Schedule: {}\n", time));
            }
        }

        // Show next execution times
        let now = Scheduler::get_current_datetime_at(task.timezone);

        let next_runs = Self::get_next_execution_times(task, now, 5);

        output.push_str(&format!("Now: {}\n", now.format("%Y-%m-%d %H:%M:%S %Z")));
        output.push_str("Next 5 executions:\n");
        for (i, next_time) in next_runs.iter().enumerate() {
            output.push_str(&format!("  {}: {}\n", i + 1, next_time.format("%Y-%m-%d %H:%M:%S %Z")));
        }

        output
    }

    /// Get the next N execution times for a task
    pub fn get_next_execution_times(task: &TaskConfig, from: DateTime<Tz>, count: usize) -> Vec<DateTime<Tz>> {
        let mut times = Vec::new();
        let mut current = from;
        let mut pending_task = PendingTask::new(Arc::new(task.clone()));
        let mut current_instant = Instant::now();
        let mut first = true;

        for _ in 0..count {
            let next = Scheduler::get_next_execution_time(&pending_task, current, false);
            times.push(next);

            let mut diff = next.timestamp() - current.timestamp();

            if (diff < 0) {
                panic!(
                    "Next execution time is before current time, next: {}, current: {}",
                    next, current
                );
            }

            // Avoid repeating the same date
            if diff < 1 {
                diff = 1i64;
            }

            let duration = std::time::Duration::from_secs(diff as u64);

            if first {
                pending_task.last_execution_time = Some((current + duration).to_utc());
            } else {
                pending_task.last_execution_time = Some(current.to_utc());
            }

            current_instant = current_instant + duration;
            current = current + duration;
        }

        times
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Schedule, TimePattern, TimePatternField};
    use chrono_tz::UTC;
    use std::time::Duration;

    fn create_test_task(name: &str, schedule: Schedule) -> TaskConfig {
        TaskConfig {
            name: name.to_string(),
            cmd: "echo test".to_string(),
            schedule,
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

    #[test]
    fn test_display_every_schedule() {
        let schedule = Schedule::Every {
            interval: Duration::from_secs(300),
            aligned: true,
        }; // 5 minutes
        let task = create_test_task("test_task", schedule);

        let display = ScheduleDisplay::display_task_schedule(&task);
        assert!(display.contains("Every 5 m (aligned)"));
        assert!(display.contains("Task: test_task"));
    }

    #[test]
    fn test_get_next_execution_times() {
        let schedule = Schedule::Every {
            interval: Duration::from_secs(60),
            aligned: false,
        }; // 1 minute
        let task = create_test_task("test_task", schedule);

        let now = UTC.with_ymd_and_hms(2023, 1, 1, 12, 0, 0).unwrap();
        let times = ScheduleDisplay::get_next_execution_times(&task, now, 3);

        assert_eq!(times.len(), 3);
        // Times should be approximately 1 minute apart
        assert!(times[1] > times[0]);
        assert!(times[2] > times[1]);
    }

    #[test]
    fn test_format_time_pattern() {
        let pattern = TimePattern {
            second: TimePatternField::Value(0),
            minute: TimePatternField::Value(30),
            hour: TimePatternField::Range(9, 17),
            day_of_week: TimePatternField::List(vec![1, 2, 3, 4, 5]), // Mon-Fri
            day: TimePatternField::Any,
            month: TimePatternField::Any,
            year: TimePatternField::Any,
        };

        let formatted = format!("{}", pattern);
        assert!(formatted.contains("30"));
        assert!(formatted.contains("9..17"));
        assert!(formatted.contains("[1,2,3,4,5]"));
    }
}
