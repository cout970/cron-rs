#![allow(unused)]

mod config;
mod logging;
mod schedule_display;
mod scheduler;
mod sqlite_logger;
mod task_executor;

mod alerts;

mod utils;

use crate::alerts::AlertConfig;
use crate::config::cron::parse_crontab_file;
use crate::config::file::ConfigFile;
use crate::config::loader::ConfigLoader;
use crate::config::logging::LoggingConfig;
use crate::schedule_display::ScheduleDisplay;
use crate::scheduler::Scheduler;
use crate::sqlite_logger::SqliteLogger;
use crate::task_executor::TaskExecutor;
use crate::utils::format_duration;
use anyhow::anyhow;
use clap::{Parser, Subcommand};
use config::validation::{validate_config, ValidationResult};
use log::{debug, error, info, warn, LevelFilter};
use std::io::{stdout, Write};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the config file
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Load extra tasks from system crontab files: /etc/crontab, /etc/cron.d/*, and
    /// scripts in /etc/cron.hourly/, /etc/cron.daily/, /etc/cron.weekly/, /etc/cron.monthly/.
    /// Allows using cron-rs as a drop-in cron replacement.
    /// Applies to: run, validate, execute-task, show-schedule.
    #[arg(long, global = true)]
    cron_compat: bool,

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
    /// Execute a specific task immediately
    ExecuteTask {
        /// Name of the task to execute
        task_name: String,
        /// Path to the config file (optional)
        #[arg(long, short)]
        config: Option<PathBuf>,
    },
    /// Show the schedule for all tasks
    ShowSchedule {
        /// Path to the config file (optional)
        #[arg(long, short)]
        config: Option<PathBuf>,
    },
    /// Write the default config file in ./default_config.yml
    GenerateConfig {
        /// Path to the file to write
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Look up the current user's crontab file and generate an equivalent config file
    GenerateFromCrontab {
        /// Path to the crontab file to read
        #[arg(long, short = 'f')]
        crontab_file: Option<PathBuf>,

        /// Path to the file to write
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cron_compat = args.cron_compat;

    match args.cmd {
        ArgCmd::Run => {
            let loader = ConfigLoader::new(get_config_path(args.config)?, cron_compat);
            cmd_run(loader)?;
        }
        ArgCmd::Validate { path } => {
            let path = path.map(Ok).unwrap_or_else(|| get_config_path(args.config))?;
            let loader = ConfigLoader::new(path, cron_compat);
            cmd_validate_config_file(loader)?;
        }
        ArgCmd::ExecuteTask { task_name, config } => {
            let config_path = config.map(Ok).unwrap_or_else(|| get_config_path(args.config))?;
            let loader = ConfigLoader::new(config_path, cron_compat);
            cmd_execute_task(loader, task_name)?;
        }
        ArgCmd::ShowSchedule { config } => {
            let config_path = config.map(Ok).unwrap_or_else(|| get_config_path(args.config))?;
            let loader = ConfigLoader::new(config_path, cron_compat);
            cmd_show_schedule(loader)?;
        }
        ArgCmd::GenerateConfig { output } => {
            if cron_compat {
                eprintln!("warning: --cron-compat has no effect on generate-config");
            }
            cmd_generate_default_config(output)?;
        }
        ArgCmd::GenerateFromCrontab { output, crontab_file } => {
            if cron_compat {
                eprintln!("warning: --cron-compat has no effect on generate-from-crontab");
            }
            cmd_generate_config_from_crontab(output, crontab_file)?;
        }
    }

    Ok(())
}

fn cmd_run(loader: ConfigLoader) -> anyhow::Result<()> {
    let config = loader.load()?;
    logging::setup_logging(&config.logging)?;

    info!("Starting cron-rs with config file: {}", loader.path.display());

    Scheduler::new(config, loader).run();

    info!("Exiting");
    Ok(())
}

fn cmd_execute_task(loader: ConfigLoader, task_name: String) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let config = loader.load()?;

        // Find the task
        let task = config
            .tasks
            .iter()
            .find(|t| t.name == task_name)
            .ok_or_else(|| anyhow!("Task '{}' not found", task_name))?;

        // Initialize SQLite logger if configured
        let sqlite_logger = if let Some(sqlite_config) = &config.logging.sqlite {
            if sqlite_config.enabled {
                match SqliteLogger::new(sqlite_config.clone()).await {
                    Ok(logger) => Some(logger),
                    Err(e) => {
                        eprintln!("Warning: Failed to initialize SQLite logger: {}", e);
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        // Create task executor
        let executor = TaskExecutor::new(config.alerts, sqlite_logger);

        // Execute the task
        println!("Executing task '{}'...", task_name);
        match executor.execute_task(task).await {
            Ok(result) => {
                println!("Task '{}' completed:", task_name);
                println!("  Status: {}", if result.success { "Success" } else { "Failed" });
                println!("  Exit code: {}", result.exit_code);
                println!("  Duration: {}", format_duration(result.duration));
                println!("  PID: {}", result.pid);

                if !result.stdout.is_empty() {
                    println!("  Stdout: {}", result.stdout.trim());
                }
                if !result.stderr.is_empty() {
                    println!("  Stderr: {}", result.stderr.trim());
                }
            }
            Err(e) => {
                eprintln!("Failed to execute task '{}': {}", task_name, e);
                std::process::exit(1);
            }
        }

        Ok(())
    })
}

fn cmd_show_schedule(loader: ConfigLoader) -> anyhow::Result<()> {
    let config = loader.load()?;
    println!("{}", ScheduleDisplay::display_schedules(&config));
    Ok(())
}

fn cmd_validate_config_file(loader: ConfigLoader) -> anyhow::Result<()> {
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

    let config_file = loader.load_file()?;
    let results = validate_config(&config_file);

    for msg in &results {
        match msg {
            ValidationResult::Error(m) => error!("{}", m),
            ValidationResult::Warning(m) => warn!("{}", m),
        }
    }

    if results.is_empty() {
        info!("Config file is valid");
    }
    Ok(())
}

fn cmd_generate_config_from_crontab(path: Option<PathBuf>, crontab_file: Option<PathBuf>) -> anyhow::Result<()> {
    let crontab = if let Some(crontab_file) = crontab_file {
        std::fs::read_to_string(crontab_file).map_err(|e| anyhow::anyhow!("Failed to read crontab: {}", e))?
    } else {
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
        logging: Some(LoggingConfig { ..Default::default() }),
        alerts: Some(AlertConfig { ..Default::default() }),
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
            if path.exists() {
                if !path.is_file() {
                    return Err(anyhow::anyhow!("Path {} is not a file", path.to_string_lossy()));
                }
                if path.metadata()?.permissions().readonly() {
                    return Err(anyhow::anyhow!("File {} is not writable", path.to_string_lossy()));
                }
            } else if let Some(parent) = path.parent() {
                if !parent.is_dir() || parent.metadata()?.permissions().readonly() {
                    return Err(anyhow::anyhow!(
                        "Directory {} is not writable",
                        parent.to_string_lossy(),
                    ));
                }
            }

            std::fs::write(&path, contents).expect("Unable to write file");
            println!("Generated config file at {}", path.to_string_lossy());
        }
        None => {
            stdout().lock().write_all(contents).expect("Unable to write file");
        }
    }
    Ok(())
}

fn get_config_path(mut config_path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if config_path.is_none() {
        if std::fs::exists("./config.yml")? {
            config_path = Some(PathBuf::from("./config.yml"));
        }
    }

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

    if config_path.is_none() && std::fs::exists("/etc/cron-rs.yml")? {
        config_path = Some(PathBuf::from("/etc/cron-rs.yml"));
    }

    config_path.ok_or_else(|| anyhow!("No config file found. Please specify a config file with --config"))
}
