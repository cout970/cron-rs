#![allow(unused)]

mod config;
mod logging;
mod scheduler;

use clap::{Parser, Subcommand};
use log::{debug, error, info, warn};

use config::file::read_config_file;
use config::parse_config_file;
use config::validation::{validate_config, ValidationResult};

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
    // Look up the current user's crontab file and genera an equivalent config file
    // GenerateFromCrontab,
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
            std::fs::write(path, include_bytes!("config/default_config.yml"))
                .expect("Unable to write file");
            info!("Generated default config file at {}", path);
            Ok(())
        }
        // ArgCmd::GenerateFromCrontab => Ok(()),
    }
}
