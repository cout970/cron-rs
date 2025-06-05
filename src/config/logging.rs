use serde::{Deserialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub enum LogOutput {
    #[serde(rename = "stdout")]
    Stdout,
    #[serde(rename = "file")]
    File,
    #[serde(rename = "syslog")]
    Syslog,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    pub output: LogOutput,
    pub file: Option<PathBuf>,
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            output: LogOutput::Stdout,
            file: None,
            level: "info".to_string(),
        }
    }
} 