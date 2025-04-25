use anyhow::Result;
use crate::config::logging::{LogOutput, LoggingConfig};
use log::{LevelFilter, SetLoggerError};
use std::fs::OpenOptions;
use std::path::PathBuf;

pub fn setup_logging(config: &LoggingConfig) -> Result<()> {
    let level = config.level.parse::<LevelFilter>()?;

    match &config.output {
        LogOutput::Stdout => {
            env_logger::Builder::new()
                .filter_level(level)
                .format_timestamp_secs()
                .init();
        }
        LogOutput::File => {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(config.file.clone().unwrap_or_else(|| PathBuf::from("/var/log/cron-rs.log")))?;

            env_logger::Builder::new()
                .filter_level(level)
                .format_timestamp_secs()
                .target(env_logger::Target::Pipe(Box::new(file)))
                .init();
        }
        LogOutput::Syslog => {
            let formatter = syslog::Formatter3164 {
                facility: syslog::Facility::LOG_USER,
                hostname: None,
                process: "cron-rs".into(),
                pid: std::process::id(),
            };

            let logger = syslog::unix(formatter).expect("Failed to create syslog logger");
            log::set_boxed_logger(Box::new(syslog::BasicLogger::new(logger)))
                .map(|()| log::set_max_level(level))?;
        }
    }

    Ok(())
} 