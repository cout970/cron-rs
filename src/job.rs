use anyhow::Result;
use serde::{Deserialize, Serialize};
use crate::task::TaskConfig;
use crate::alert::AlertConfig;

#[derive(Debug, Serialize, Deserialize)]
pub struct JobConfig {
    pub name: String,
    pub schedule: String,
    pub task: TaskConfig,
    pub alert: Option<AlertConfig>,
}

impl JobConfig {
    pub async fn execute(&self) -> Result<()> {
        if let Err(e) = self.task.execute() {
            if let Some(alert) = &self.alert {
                alert.send(&self.name, &e.to_string()).await?;
            }
            return Err(e);
        }
        Ok(())
    }
} 