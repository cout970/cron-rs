#![allow(unused)]

mod config;
mod logging;
mod scheduler;

use clap::Parser;
use log::{debug, error, info, warn};

use config::file::read_config_file;
use config::validation::{validate_config, ValidationResult};
use config::parse_config_file;

use scheduler::start_scheduler;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the config file
    #[arg(short, long)]
    config: String,

    /// Print config validation result
    #[arg(long)]
    validate: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let config_file = read_config_file(&args.config)?;

    // Parse config into tasks to run
    let config = parse_config_file(&config_file)?;

    // Setup logging
    logging::setup_logging(&config.logging)?;

    info!("Starting cron-rs with config file: {}", args.config);

    // Validate config file
    if args.validate {
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
    }

    debug!("Parsed config: {:?}", config);

    info!("Starting event loop");
    start_scheduler(config.tasks)?;

    info!("Exiting");
    Ok(())
}



