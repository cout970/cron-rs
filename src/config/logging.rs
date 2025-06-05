use serde::{Deserialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub enum LogOutput {
    Stdout,
    File(PathBuf),
    Syslog,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    pub output: LogOutput,
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            output: LogOutput::Stdout,
            level: "info".to_string(),
        }
    }
} 