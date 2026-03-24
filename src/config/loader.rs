use std::path::PathBuf;

use anyhow::Context;

use super::{file::{read_config_file, validate_config_path, ConfigFile}, parse_config_file, Config};
use super::cron::load_crontab_tasks;

/// Holds the config file path and runtime flags needed to (re)load the configuration.
/// Used by the CLI commands and the scheduler's hot-reload (SIGHUP) path.
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    pub path: PathBuf,
    pub cron_compat: bool,
}

impl ConfigLoader {
    pub fn new(path: PathBuf, cron_compat: bool) -> Self {
        Self { path, cron_compat }
    }

    /// Validates the path, reads the YAML file, and optionally merges system
    /// crontab tasks (when `cron_compat` is true). Returns the raw `ConfigFile`
    /// so callers that need it (e.g. `validate`) can inspect it before parsing.
    pub fn load_file(&self) -> anyhow::Result<ConfigFile> {
        validate_config_path(&self.path)?;
        let mut config_file = read_config_file(&self.path)
            .with_context(|| format!("Failed to read config file: {}", self.path.display()))?;

        if self.cron_compat {
            let extra = load_crontab_tasks();
            log::info!("cron-compat: merged {} task(s) from system crontab files", extra.len());
            config_file.tasks.extend(extra);
        }

        Ok(config_file)
    }

    /// Convenience wrapper: `load_file` + `parse_config_file`.
    pub fn load(&self) -> anyhow::Result<Config> {
        let config_file = self.load_file()?;
        parse_config_file(&config_file)
    }
}
