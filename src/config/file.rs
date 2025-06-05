use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::collections::HashMap;

use super::logging::LoggingConfig;

#[derive(Deserialize, Clone, Debug)]
pub struct ConfigFile {
    pub tasks: Vec<TaskDefinition>,
    pub logging: Option<LoggingConfig>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct TaskDefinition {
    pub name: String,
    pub cmd: String,
    #[serde(default)]
    pub when: Option<TimePatternConfig>,
    #[serde(default)]
    pub every: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub avoid_overlapping: bool,
    #[serde(default)]
    pub run_as: Option<String>,
    #[serde(default)]
    pub time_limit: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum TimePatternConfig {
    Short(String),
    Long(ExplodedTimePatternConfig),
}

#[derive(Deserialize, Debug, Clone)]
pub struct ExplodedTimePatternConfig {
    pub second: Option<ExplodedTimePatternFieldConfig>,
    pub minute: Option<ExplodedTimePatternFieldConfig>,
    pub hour: Option<ExplodedTimePatternFieldConfig>,
    pub day: Option<ExplodedTimePatternFieldConfig>,
    pub month: Option<ExplodedTimePatternFieldConfig>,
    pub year: Option<ExplodedTimePatternFieldConfig>,
    pub day_of_week: Option<ExplodedTimePatternFieldConfig>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ExplodedTimePatternFieldConfig {
    Number(u32),
    Text(String),
    List(Vec<String>),
}

pub fn read_config_file(path: &str) -> anyhow::Result<ConfigFile> {
    let content = std::fs::read_to_string(path).context("Failed to read config file")?;
    let config = serde_yml::from_str(&content).context("Failed to parse config file")?;

    Ok(config)
}
