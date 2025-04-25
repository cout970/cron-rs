use chrono::TimeZone;
use chrono_tz::Tz;

use crate::config::file::ConfigFile;

#[derive(Debug, Clone)]
pub enum ValidationResult {
    Error(String),
    Warning(String),
}

pub fn validate_config(conf: &ConfigFile) -> Vec<ValidationResult> {
    let mut result = vec![];
    let mut task_names = vec![];

    for task in &conf.tasks {
        // Non empty and unique name
        if task.name.is_empty() {
            result.push(ValidationResult::Error(format!("Task name must not be empty")));
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
    }

    result
}