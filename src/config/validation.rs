use chrono::TimeZone;
use chrono_tz::Tz;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::config::file::{ConfigFile, TimePatternConfig};
use crate::config::logging::LogOutput;
use crate::config::{Schedule, TimePattern};

#[derive(Debug, Clone)]
pub enum ValidationResult {
    Error(String),
    Warning(String),
}

fn validate_user_group(user_group: &str) -> Option<String> {
    let parts: Vec<&str> = user_group.split(':').collect();
    let (user, group) = match parts.as_slice() {
        [user] => (user, user),  // Single value means same user and group
        [user, group] => (user, group),
        _ => return Some(format!("Invalid user:group format: '{}'", user_group))
    };

    // Check if user exists (try both as name and uid)
    let user_exists = if let Ok(uid) = user.parse::<u32>() {
        Command::new("id")
            .arg("-u")
            .arg(uid.to_string())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        Command::new("id")
            .arg(user)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    if !user_exists {
        return Some(format!("User '{}' does not exist", user));
    }

    // Check if group exists (try both as name and gid)
    let group_exists = if let Ok(gid) = group.parse::<u32>() {
        Command::new("getent")
            .args(["group", &gid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        Command::new("getent")
            .args(["group", group])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    if !group_exists {
        return Some(format!("Group '{}' does not exist", group));
    }

    None
}

fn validate_shell(shell: &str) -> Option<String> {
    // Check if shell exists and is executable
    if !Path::new(shell).exists() {
        return Some(format!("Shell '{}' does not exist", shell));
    }
    
    if !Command::new(shell)
        .arg("-c")
        .arg("exit 0")
        .status()
        .map(|s| s.success())
        .unwrap_or(false) {
        return Some(format!("Shell '{}' is not executable or invalid", shell));
    }
    
    None
}

fn validate_output_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    
    // If path exists, it must be a file
    if path.exists() && !path.is_file() {
        return Some(format!("Path '{}' exists but is not a file", path.display()));
    }
    
    // Check if parent directory exists and is writable
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            return Some(format!("Parent directory '{}' does not exist", parent.display()));
        }
        
        // Try to check if directory is writable
        if !Command::new("test")
            .args(["-w", &parent.to_string_lossy()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false) {
            return Some(format!("Parent directory '{}' is not writable", parent.display()));
        }
    }
    
    None
}

fn validate_logging_config(conf: &ConfigFile) -> Vec<ValidationResult> {
    let mut result = vec![];
    
    if let Some(logging) = &conf.logging {
        // Validate log level
        let valid_levels = ["error", "warn", "info", "debug", "trace"];
        if !valid_levels.contains(&logging.level.as_str()) {
            result.push(ValidationResult::Error(
                format!("Invalid log level '{}'. Must be one of: {}", 
                    logging.level, 
                    valid_levels.join(", ")
                )
            ));
        }
        
        // Validate file path if output is file
        if logging.output == LogOutput::File {
            if let Some(path) = &logging.file {
                if let Some(err) = validate_output_path(path.to_str().unwrap_or("")) {
                    result.push(ValidationResult::Error(format!("Invalid log file: {}", err)));
                }
            } else {
                result.push(ValidationResult::Warning(
                    "Log output is set to 'file' but no file path specified".to_string()
                ));
            }
        }
    }
    
    result
}

pub fn validate_config(conf: &ConfigFile) -> Vec<ValidationResult> {
    let mut result = vec![];
    let mut task_names = vec![];

    for task in &conf.tasks {
        // Non empty and unique name
        if task.name.is_empty() {
            result.push(ValidationResult::Error("Task name must not be empty".to_string()));
        }
        if task_names.contains(&task.name) {
            result.push(ValidationResult::Warning(format!("Non unique task name: '{}'", task.name)));
        }
        task_names.push(task.name.to_string());

        // Valid timezone
        if let Some(tz_name) = &task.timezone {
            let tz: Result<Tz, _> = tz_name.parse();
            if tz.is_err() {
                result.push(ValidationResult::Error(format!("Unable to parse timezone: '{}'", tz_name)));
            }
        }

        // Command must not be empty
        if task.cmd.is_empty() {
            result.push(ValidationResult::Error(format!("Task '{}': Command must not be empty", task.name)));
        }

        // Must have either when or every, but not both
        match (&task.when, &task.every) {
            (None, None) => {
                result.push(ValidationResult::Error(format!("Task '{}': Must specify either 'when' or 'every'", task.name)));
            }
            (Some(_), Some(_)) => {
                result.push(ValidationResult::Error(format!("Task '{}': Cannot specify both 'when' and 'every'", task.name)));
            }
            _ => {}
        }

        // Validate every format if present
        if let Some(every) = &task.every {
            if let Err(e) = Schedule::parse_time_duration(every) {
                result.push(ValidationResult::Error(format!("Task '{}': Invalid 'every' format: {}", task.name, e)));
            }
        }

        // Validate when format if present
        if let Some(when) = &task.when {
            match when {
                TimePatternConfig::Short(s) => {
                    if let Err(e) = TimePattern::parse_short(s) {
                        result.push(ValidationResult::Error(format!("Task '{}': Invalid short time pattern: {}", task.name, e)));
                    }
                }
                TimePatternConfig::Long(c) => {
                    if let Err(e) = TimePattern::parse_long(c) {
                        result.push(ValidationResult::Error(format!("Task '{}': Invalid long time pattern: {}", task.name, e)));
                    }
                }
            }
        }

        // Validate time_limit format if present
        if let Some(limit) = &task.time_limit {
            if let Err(e) = Schedule::parse_time_duration(limit) {
                result.push(ValidationResult::Error(format!("Task '{}': Invalid time limit format: {}", task.name, e)));
            }
            // Validate time_limit is not too short
            if let Ok(duration) = Schedule::parse_time_duration(limit) {
                if duration < Duration::from_secs(1) {
                    result.push(ValidationResult::Error(
                        format!("Task '{}': time_limit must be at least 1 second", task.name)
                    ));
                }
            }
        }

        // Validate run_as format and existence
        if let Some(run_as) = &task.run_as {
            if let Some(err) = validate_user_group(run_as) {
                result.push(ValidationResult::Error(
                    format!("Task '{}': {}", task.name, err)
                ));
            }
        }

        // Validate working_directory exists if specified
        if let Some(dir) = &task.working_directory {
            if !Path::new(dir).exists() {
                result.push(ValidationResult::Error(format!("Task '{}': Working directory '{}' does not exist", task.name, dir)));
            }
        }

        // Validate shell executable
        let shell = task.shell.as_deref().unwrap_or("/bin/sh");
        if let Some(err) = validate_shell(shell) {
            result.push(ValidationResult::Error(
                format!("Task '{}': {}", task.name, err)
            ));
        }

        // Validate stdout and stderr paths
        if let Some(path) = &task.stdout {
            if let Some(err) = validate_output_path(path) {
                result.push(ValidationResult::Error(
                    format!("Task '{}': Invalid stdout path: {}", task.name, err)
                ));
            }
        }
        
        if let Some(path) = &task.stderr {
            if let Some(err) = validate_output_path(path) {
                result.push(ValidationResult::Error(
                    format!("Task '{}': Invalid stderr path: {}", task.name, err)
                ));
            }
        }
    }

    // Validate logging config
    result.extend(validate_logging_config(conf));

    result
}