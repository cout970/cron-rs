#![allow(unused)]

mod config;
mod logging;
mod scheduler;

mod alerts;

use crate::config::file::ConfigFile;
use crate::config::file::ExplodedTimePatternConfig;
use crate::config::file::ExplodedTimePatternFieldConfig;
use crate::config::file::TaskDefinition;
use crate::config::file::TimePatternConfig;
use anyhow::anyhow;
use clap::{Parser, Subcommand};
use config::file::read_config_file;
use config::parse_config_file;
use config::validation::{validate_config, ValidationResult};
use log::{debug, error, info, warn, LevelFilter};
use std::io::{stdout, Write};
use std::path::PathBuf;

use scheduler::start_scheduler;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the config file
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: ArgCmd,
}

#[derive(Debug, Clone, Subcommand)]
enum ArgCmd {
    /// Run the tasks defined in the config file
    Run,
    /// Validate the config file
    Validate {
        /// Path to the config file to validate
        path: Option<PathBuf>,
    },
    /// Write the default config file in ./default_config.yml
    GenerateConfig {
        /// Path to the file to write
        path: Option<PathBuf>,
    },
    /// Look up the current user's crontab file and genera an equivalent config file
    GenerateFromCrontab {
        /// Path to the file to write
        path: Option<PathBuf>,
        /// Path to the crontab file to read
        crontab_file: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.cmd {
        ArgCmd::Run => {
            cmd_run(get_config_path(args.config)?)?;
            Ok(())
        }
        ArgCmd::Validate { path } => {
            let path = if let Some(path) = path { path } else { get_config_path(args.config)? };
            cmd_validate_config_file(path)?;
            Ok(())
        }
        ArgCmd::GenerateConfig { path } => {
            cmd_generate_default_config(path)?;
            Ok(())
        }
        ArgCmd::GenerateFromCrontab { path, crontab_file } => {
            cmd_generate_config_from_crontab(path, crontab_file)?;
            Ok(())
        }
    }
}

fn cmd_run(config_path: PathBuf) -> anyhow::Result<()> {
    let config_file = read_config_file(&config_path)?;
    let config = parse_config_file(&config_file)?;
    logging::setup_logging(&config.logging)?;

    info!(
        "Starting cron-rs with config file: {}",
        config_path.to_string_lossy()
    );

    start_scheduler(&config)?;

    info!("Exiting");
    Ok(())
}

fn cmd_validate_config_file(path: PathBuf) -> anyhow::Result<()> {
    env_logger::Builder::new()
        .filter_level(LevelFilter::Info)
        .format_timestamp(None)
        .format_level(true)
        .format_target(false)
        .format_indent(None)
        .format_module_path(false)
        .format_file(false)
        .format_line_number(false)
        .init();

    let config_file = read_config_file(path)?;
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

fn cmd_generate_config_from_crontab(
    path: Option<PathBuf>,
    crontab_file: Option<PathBuf>,
) -> anyhow::Result<()> {
    // Crontab file contents
    let crontab = if let Some(crontab_file) = crontab_file {
        // If a file path is provided, read the crontab from that file
        std::fs::read_to_string(crontab_file)
            .map_err(|e| anyhow::anyhow!("Failed to read crontab: {}", e))?
    } else {
        // If no path is provided, use the crontab command to get the current user's crontab
        let output = std::process::Command::new("crontab")
            .arg("-l")
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to read crontab: {}", e))?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to read crontab: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        String::from_utf8(output.stdout)?
    };

    let tasks = parse_crontab_file(&crontab)?;
    let config = ConfigFile {
        tasks,
        ..Default::default()
    };

    let config_file_contents = serde_yml::to_string(&config)?;
    print_config_file(config_file_contents.as_bytes(), &path)?;
    Ok(())
}

fn cmd_generate_default_config(path: Option<PathBuf>) -> anyhow::Result<()> {
    print_config_file(include_bytes!("config/default_config.yml"), &path)?;
    Ok(())
}

fn print_config_file(contents: &[u8], path: &Option<PathBuf>) -> anyhow::Result<()> {
    match path {
        Some(path) => {
            // Validate the file is writable or does not exist and the directory is writable
            if path.exists() {
                if !path.is_file() {
                    return Err(anyhow::anyhow!(
                        "Path {} is not a file",
                        path.to_string_lossy()
                    ));
                }
                if !path.metadata()?.permissions().readonly() {
                    return Err(anyhow::anyhow!(
                        "File {} is not writable",
                        path.to_string_lossy()
                    ));
                }
            } else {
                if let Some(parent) = path.parent() {
                    if !parent.is_dir() || !parent.metadata()?.permissions().readonly() {
                        return Err(anyhow::anyhow!(
                            "Directory {} is not writable",
                            parent.to_string_lossy()
                        ));
                    }
                }
            }

            std::fs::write(&path, contents).expect("Unable to write file");

            println!("Generated config file at {}", path.to_string_lossy());
        }
        None => {
            stdout()
                .lock()
                .write_all(contents)
                .expect("Unable to write file");
        }
    }
    Ok(())
}

fn parse_crontab_file(crontab: &str) -> anyhow::Result<Vec<TaskDefinition>> {
    let mut tasks = vec![];
    let mut last_comment = String::new();

    for line in crontab.lines() {
        let line = line.trim();
        if line.is_empty() {
            last_comment.clear();
            continue;
        }

        if line.starts_with('#') {
            last_comment.push_str(line[1..].trim());
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 6 {
            last_comment.clear();
            continue;
        }

        let (minute, hour, day, month, day_of_week) =
            (parts[0], parts[1], parts[2], parts[3], parts[4]);
        let cmd = parts[5..].join(" ");

        let name = if last_comment.trim().is_empty() {
            format!("Crontab: {}", line)
        } else {
            last_comment.trim().to_string()
        };

        let task = TaskDefinition {
            name,
            cmd,
            when: Some(TimePatternConfig::Long(ExplodedTimePatternConfig {
                second: Some(ExplodedTimePatternFieldConfig::Number(0)),
                minute: Some(ExplodedTimePatternFieldConfig::Text(minute.to_string())),
                hour: Some(ExplodedTimePatternFieldConfig::Text(hour.to_string())),
                day: Some(ExplodedTimePatternFieldConfig::Text(day.to_string())),
                month: Some(ExplodedTimePatternFieldConfig::Text(month.to_string())),
                year: Some(ExplodedTimePatternFieldConfig::Text("*".to_string())),
                day_of_week: Some(ExplodedTimePatternFieldConfig::Text(
                    day_of_week.replace("-", "..").to_string(),
                )),
            })),
            ..Default::default()
        };

        tasks.push(task);
    }

    Ok(tasks)
}

fn get_config_path(mut config_path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    // If not provided, check in the current directory for `config.yml`
    if config_path.is_none() {
        if std::fs::exists("./config.yml")? {
            config_path = Some(PathBuf::from("./config.yml"));
        }
    }

    // or check in the default config directory `$XDG_CONFIG_HOME/cron-rs` or `$HOME/.config/cron-rs`
    if config_path.is_none() {
        let config_location = if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
            format!("{}/cron-rs/config.yml", dir)
        } else if let Ok(dir) = std::env::var("HOME") {
            format!("{}/.config/cron-rs/config.yml", dir)
        } else {
            "./config.yml".to_string()
        };

        if std::fs::exists(&config_location)? {
            config_path = Some(PathBuf::from(&config_location));
        }
    }

    // or check the system-wide config directory `/etc/cron-rs.yml`
    if config_path.is_none() {
        if std::fs::exists("/etc/cron-rs.yml")? {
            config_path = Some(PathBuf::from("/etc/cron-rs.yml"));
        }
    }

    // Not specified and not found in any of the default locations
    if config_path.is_none() {
        return Err(anyhow!(
            "No config file found. Please specify a config file with --config"
        ));
    }

    Ok(config_path.unwrap())
}
