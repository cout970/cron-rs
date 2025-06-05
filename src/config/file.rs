use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::ops::Not;
use super::logging::LoggingConfig;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ConfigFile {
    pub tasks: Vec<TaskDefinition>,
    pub logging: Option<LoggingConfig>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
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
    #[serde(skip_serializing_if = "skip_if_false")]
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

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum TimePatternConfig {
    Short(String),
    Long(ExplodedTimePatternConfig),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExplodedTimePatternConfig {
    pub second: Option<ExplodedTimePatternFieldConfig>,
    pub minute: Option<ExplodedTimePatternFieldConfig>,
    pub hour: Option<ExplodedTimePatternFieldConfig>,
    pub day: Option<ExplodedTimePatternFieldConfig>,
    pub month: Option<ExplodedTimePatternFieldConfig>,
    pub year: Option<ExplodedTimePatternFieldConfig>,
    pub day_of_week: Option<ExplodedTimePatternFieldConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ExplodedTimePatternFieldConfig {
    Number(u32),
    Text(String),
    List(Vec<String>),
}

pub fn read_config_file<P: AsRef<Path>>(path: P) -> anyhow::Result<ConfigFile> {
    let content = std::fs::read_to_string(path).context("Failed to read config file")?;
    let config = serde_yml::from_str(&content).context("Failed to parse config file")?;

    Ok(config)
}

fn skip_if_false(arg: &bool) -> bool {
    !*arg
}