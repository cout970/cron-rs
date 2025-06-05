use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub enum LogOutput {
    #[serde(rename = "stdout")]
    #[default]
    Stdout,
    #[serde(rename = "file")]
    File,
    #[serde(rename = "syslog")]
    Syslog,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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