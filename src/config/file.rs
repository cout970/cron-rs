use anyhow::Context;
use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct ConfigFile {
    pub tasks: Vec<TaskConfig>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct TaskConfig {
    pub name: String,
    pub cmd: String,
    pub every: Option<String>,
    pub when: Option<TimePatternConfig>,
    pub timezone: Option<String>,
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
