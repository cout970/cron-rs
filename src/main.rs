#![allow(unused)]

mod config;
mod scheduler;

use clap::Parser;

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

    //  Validate config file
    if args.validate {
        let info = validate_config(&config_file);

        for msg in &info {
            match msg {
                ValidationResult::Error(m) => {
                    println!("[Error] {}", m);
                }
                ValidationResult::Warning(m) => {
                    println!("[Warning] {}", m);
                }
            }
        }

        if info.is_empty() {
            println!("Config file is OK");
        }
    }

    // Parse config into tasks to run
    let config = parse_config_file(config_file)?;
    // dbg!(&config);

    println!("Starting event loop");
    start_scheduler(config.tasks)?;

    println!("Exiting");
    Ok(())
}



