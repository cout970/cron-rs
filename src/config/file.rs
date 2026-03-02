use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::ops::Not;
use crate::alerts::{Alert, AlertConfig};
use super::logging::LoggingConfig;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ConfigFile {
    pub tasks: Vec<TaskDefinition>,
    pub logging: Option<LoggingConfig>,
    pub alerts: Option<AlertConfig>,
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
    #[serde(default)]
    pub on_failure: Vec<Alert>,
    #[serde(default)]
    pub on_success: Vec<Alert>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum TimePatternConfig {
    Short(String),
    Long(ExplodedTimePatternConfig),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExplodedTimePatternConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub second: Option<ExplodedTimePatternFieldConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minute: Option<ExplodedTimePatternFieldConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hour: Option<ExplodedTimePatternFieldConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<ExplodedTimePatternFieldConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<ExplodedTimePatternFieldConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<ExplodedTimePatternFieldConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day_of_week: Option<ExplodedTimePatternFieldConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ExplodedTimePatternFieldConfig {
    Number(u32),
    Text(String),
    List(Vec<String>),
}

/// Validates that the config file exists, is a regular file, and has secure permissions.
pub fn validate_config_path(config_path: &Path) -> anyhow::Result<()> {
    match std::fs::metadata(config_path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(anyhow::anyhow!(
                    "Config path {} is not a file",
                    config_path.to_string_lossy()
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;

                if metadata.mode() & 0o002 != 0 {
                    let error = anyhow::anyhow!(
                        concat!(
                            "Config file {} is globally writable by any user.\n",
                            "###\n",
                            "THIS IS MAYOR SECURITY RISK\n",
                            "###\n",
                            "Any user can alter the config file and add tasks that will be run with the current user's permissions.\n",
                            "Refusing to run with insecure file permissions (mod {:o})"
                        ),
                        config_path.to_string_lossy(),
                        (metadata.mode() & 0o777)
                    );
                    return Err(error);
                }
            }
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!(
            "Failed to read config file {}: {}",
            config_path.to_string_lossy(),
            e
        )),
    }
}

pub fn read_config_file<P: AsRef<Path>>(path: P) -> anyhow::Result<ConfigFile> {
    let content = std::fs::read_to_string(path).context("Failed to read config file")?;
    let config = serde_yml::from_str(&content).context("Failed to parse config file")?;

    Ok(config)
}

fn skip_if_false(arg: &bool) -> bool {
    !*arg
}