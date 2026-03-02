use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use libsql::{Builder, Connection, Database};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

const DB_SCHEMA_VERSION: i32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SqliteLoggerConfig {
    pub enabled: bool,
    pub database_path: PathBuf,
}

impl Default for SqliteLoggerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            database_path: PathBuf::from("cron_execution_logs.db"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SqliteLogger {
    db: Arc<Mutex<Connection>>,
    config: SqliteLoggerConfig,
}

#[derive(Debug, Clone)]
pub struct ExecutionAttempt {
    pub task_name: String,
    pub task_id: u32,
    pub pid: u32,
    pub cmd: String,
    pub start_time: DateTime<Utc>,
    pub timezone: String,
    pub working_directory: Option<String>,
    pub shell: Option<String>,
    pub run_as: Option<String>,
    pub time_limit: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ExecutionSuccess {
    pub task_name: String,
    pub task_id: u32,
    pub pid: u32,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub duration_seconds: f64,
    pub exit_code: i32,
}

#[derive(Debug, Clone)]
pub struct ExecutionFailure {
    pub task_name: String,
    pub task_id: u32,
    pub pid: u32,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub duration_seconds: f64,
    pub exit_code: Option<i32>,
    pub error_message: String,
    pub failure_reason: String,
}

impl SqliteLogger {
    pub async fn new(config: SqliteLoggerConfig) -> Result<Self> {
        if !config.enabled {
            return Err(anyhow::anyhow!("SQLite logger is not enabled"));
        }


        let db = Builder::new_local(&config.database_path).build().await
            .context("Failed to open SQLite database")?;

        let conn = db.connect()?;
        
        let logger = Self {
            db: Arc::new(Mutex::new(conn)),
            config: config.clone(),
        };

        logger.initialize_schema().await?;
        info!("SQLite logger initialized with database: {:?}", config.database_path);
        
        Ok(logger)
    }

    async fn initialize_schema(&self) -> Result<()> {
        let db = self.db.lock().await;
        
        // Create database_version table first
        db.execute(
            r#"
            CREATE TABLE IF NOT EXISTS database_version (
                version INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
            (),
        ).await?;

        // Check current database version
        let current_version = self.get_database_version(&db).await?;
        if current_version == 0 {
            db.execute(
                "INSERT INTO database_version (version) VALUES (?)",
                [DB_SCHEMA_VERSION],
            ).await?;
            debug!("Initialized database with schema version {}", DB_SCHEMA_VERSION);
        } else if current_version != DB_SCHEMA_VERSION {
            warn!("Database schema version {} (current {}), but no pending migrations found", current_version, DB_SCHEMA_VERSION);
        }
        
        db.execute(
            r#"
            CREATE TABLE IF NOT EXISTS execution_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_name TEXT NOT NULL,
                task_id INTEGER NOT NULL,
                pid INTEGER NOT NULL,
                cmd TEXT NOT NULL,
                start_time TEXT NOT NULL,
                timezone TEXT NOT NULL,
                working_directory TEXT,
                shell TEXT,
                run_as TEXT,
                time_limit INTEGER,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
            (),
        ).await?;

        db.execute(
            r#"
            CREATE TABLE IF NOT EXISTS execution_successes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_name TEXT NOT NULL,
                task_id INTEGER NOT NULL,
                pid INTEGER NOT NULL,
                start_time TEXT NOT NULL,
                end_time TEXT NOT NULL,
                duration_seconds REAL NOT NULL,
                exit_code INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
            (),
        ).await?;

        db.execute(
            r#"
            CREATE TABLE IF NOT EXISTS execution_failures (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_name TEXT NOT NULL,
                task_id INTEGER NOT NULL,
                pid INTEGER NOT NULL,
                start_time TEXT NOT NULL,
                end_time TEXT NOT NULL,
                duration_seconds REAL NOT NULL,
                exit_code INTEGER,
                error_message TEXT NOT NULL,
                failure_reason TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
            (),
        ).await?;

        // Create indexes for better query performance
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_attempts_task_name ON execution_logs(task_name)",
            (),
        ).await?;
        
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_attempts_start_time ON execution_logs(start_time)",
            (),
        ).await?;
        
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_successes_task_name ON execution_successes(task_name)",
            (),
        ).await?;
        
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_successes_start_time ON execution_successes(start_time)",
            (),
        ).await?;
        
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_failures_task_name ON execution_failures(task_name)",
            (),
        ).await?;
        
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_failures_start_time ON execution_failures(start_time)",
            (),
        ).await?;

        debug!("SQLite schema initialized successfully");
        Ok(())
    }

    async fn get_database_version(&self, db: &Connection) -> Result<i32> {
        let mut rows = db.query("SELECT version FROM database_version ORDER BY created_at DESC LIMIT 1", ()).await?;
        if let Some(row) = rows.next().await? {
            Ok(row.get(0)?)
        } else {
            Ok(0) // No version found, assume new database
        }
    }

    pub async fn log_execution_attempt(&self, attempt: &ExecutionAttempt) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let db = self.db.lock().await;
        
        db.execute(
            r#"
            INSERT INTO execution_logs (
                task_name, task_id, pid, cmd, start_time, timezone,
                working_directory, shell, run_as, time_limit
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            (
                attempt.task_name.as_str(),
                attempt.task_id as i64,
                attempt.pid as i64,
                attempt.cmd.as_str(),
                attempt.start_time.to_rfc3339().as_str(),
                attempt.timezone.as_str(),
                attempt.working_directory.as_deref(),
                attempt.shell.as_deref(),
                attempt.run_as.as_deref(),
                attempt.time_limit.map(|t| t as i64),
            ),
        ).await
        .context("Failed to log execution attempt")?;

        debug!("Logged execution attempt for task: {}", attempt.task_name);
        Ok(())
    }

    pub async fn log_execution_success(&self, success: &ExecutionSuccess) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let db = self.db.lock().await;
        
        db.execute(
            r#"
            INSERT INTO execution_successes (
                task_name, task_id, pid, start_time, end_time, duration_seconds,
                exit_code
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
            (
                success.task_name.as_str(),
                success.task_id as i64,
                success.pid as i64,
                success.start_time.to_rfc3339().as_str(),
                success.end_time.to_rfc3339().as_str(),
                success.duration_seconds,
                success.exit_code as i64,
            ),
        ).await
        .context("Failed to log execution success")?;

        debug!("Logged execution success for task: {}", success.task_name);
        Ok(())
    }

    pub async fn log_execution_failure(&self, failure: &ExecutionFailure) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let db = self.db.lock().await;
        
        db.execute(
            r#"
            INSERT INTO execution_failures (
                task_name, task_id, pid, start_time, end_time, duration_seconds,
                exit_code, error_message, failure_reason
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            (
                failure.task_name.as_str(),
                failure.task_id as i64,
                failure.pid as i64,
                failure.start_time.to_rfc3339().as_str(),
                failure.end_time.to_rfc3339().as_str(),
                failure.duration_seconds,
                failure.exit_code.map(|c| c as i64),
                failure.error_message.as_str(),
                failure.failure_reason.as_str(),
            ),
        ).await
        .context("Failed to log execution failure")?;

        debug!("Logged execution failure for task: {}", failure.task_name);
        Ok(())
    }

    pub async fn get_database_version_info(&self) -> Result<i32> {
        if !self.config.enabled {
            return Ok(0);
        }

        let db = self.db.lock().await;
        self.get_database_version(&db).await
    }
}