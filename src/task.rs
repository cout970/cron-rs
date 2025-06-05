use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::{Duration, SystemTime};

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub timeout: Option<u64>,
}

impl TaskConfig {
    pub async fn execute(&self) -> Result<bool> {
        let start_time = SystemTime::now();
        
        let mut cmd = Command::new(&self.command);
        
        cmd.args(&self.args);
        
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }
        
        let output = if let Some(timeout) = self.timeout {
            tokio::time::timeout(
                Duration::from_secs(timeout),
                tokio::task::spawn_blocking(move || cmd.output()),
            )
            .await??
        } else {
            tokio::task::spawn_blocking(move || cmd.output()).await?
        };
        
        let success = output.status.success();
        let duration = start_time.elapsed()?;
        
        log::info!(
            "Task '{}' completed in {:?} with status: {}",
            self.name,
            duration,
            if success { "success" } else { "failure" }
        );
        
        if !success {
            log::error!(
                "Task '{}' failed with error: {}",
                self.name,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        
        Ok(success)
    }
} 