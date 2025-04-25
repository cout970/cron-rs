#![allow(unused)]

mod config;
mod logging;
mod scheduler;

use std::path::PathBuf;
use clap::{Parser, Subcommand};
use log::{debug, error, info, warn, LevelFilter};

use config::file::read_config_file;
use config::parse_config_file;
use config::validation::{validate_config, ValidationResult};
use crate::config::file::ConfigFile;
use crate::config::file::TaskDefinition;
use crate::config::file::TimePatternConfig;
use crate::config::file::ExplodedTimePatternConfig;
use crate::config::file::ExplodedTimePatternFieldConfig;

use scheduler::start_scheduler;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the config file
    #[arg(short, long, default_value = "./config.yml")]
    config: String,

    #[command(subcommand)]
    cmd: ArgCmd,
}

#[derive(Debug, Clone, Subcommand)]
enum ArgCmd {
    /// Run the tasks defined in the config file
    Run,
    /// Validate the config file
    Validate,
    /// Write the default config file in ./default_config.yml
    GenerateConfig,
    /// Look up the current user's crontab file and genera an equivalent config file
    GenerateFromCrontab {
        /// Path to the crontab file to read
        path: Option<PathBuf>
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.cmd {
        ArgCmd::Run => {
            let config_file = read_config_file(&args.config)?;
            let config = parse_config_file(&config_file)?;
            logging::setup_logging(&config.logging)?;

            info!("Starting cron-rs with config file: {}", args.config);
            info!("Starting event loop");
            start_scheduler(config.tasks)?;

            info!("Exiting");
            Ok(())
        }
        ArgCmd::Validate => {
            let config_file = read_config_file(&args.config)?;
            let info = validate_config(&config_file);

            for msg in &info {
                match msg {
                    ValidationResult::Error(m) => {
                        error!("{}", m);
                    }
                    ValidationResult::Warning(m) => {
                        warn!("{}", m);
                    }
                }
            }

            if info.is_empty() {
                info!("Config file is valid");
            }
            Ok(())
        }
        ArgCmd::GenerateConfig => {
            // Generate a default config file
            let path = "./default_config.yml";
            std::fs::write(path, include_bytes!("config/default_config.yml")).expect("Unable to write file");
            info!("Generated default config file at {}", path);
            Ok(())
        }
        ArgCmd::GenerateFromCrontab { path } => {
            let crontab = if let Some(path) = path {
                std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Failed to read crontab: {}", e))?
            } else {
                let output = std::process::Command::new("crontab")
                    .arg("-l")
                    .output()
                    .map_err(|e| anyhow::anyhow!("Failed to read crontab: {}", e))?;

                if !output.status.success() {
                    return Err(anyhow::anyhow!("Failed to read crontab: {}", String::from_utf8_lossy(&output.stderr)));
                }
                String::from_utf8(output.stdout)?
            };
            
            let tasks = parser_crontab_file(&crontab)?;
            let config = ConfigFile {
                tasks,
                ..Default::default()
            };
            
            let path = "./crontab_config.yml";
            std::fs::write(path, serde_yml::to_string(&config)?).expect("Unable to write file");
            info!("Generated config file from crontab at {}", path);
            Ok(())
        },
    }
}

fn parser_crontab_file(crontab: &str) -> anyhow::Result<Vec<TaskDefinition>> {
    let mut tasks = vec![];

    for line in crontab.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 6 {
            continue;
        }

        let (minute, hour, day, month, day_of_week) = (parts[0], parts[1], parts[2], parts[3], parts[4]);
        let cmd = parts[5..].join(" ");

        let task = TaskDefinition {
            name: format!("Crontab: {}", line),
            cmd,
            when: Some(TimePatternConfig::Long(ExplodedTimePatternConfig{
                second: Some(ExplodedTimePatternFieldConfig::Number(0)),
                minute: Some(ExplodedTimePatternFieldConfig::Text(minute.to_string())),
                hour: Some(ExplodedTimePatternFieldConfig::Text(hour.to_string())),
                day: Some(ExplodedTimePatternFieldConfig::Text(day.to_string())),
                month: Some(ExplodedTimePatternFieldConfig::Text(month.to_string())),
                year: Some(ExplodedTimePatternFieldConfig::Text("*".to_string())),
                day_of_week: Some(ExplodedTimePatternFieldConfig::Text(day_of_week.to_string())),
            })),
            ..Default::default()
        };

        tasks.push(task);
    }

    Ok(tasks)
}
