use anyhow::Result;
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AlertConfig {
    pub on_failure: Vec<Alert>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Alert {
    #[serde(rename = "email")]
    Email {
        to: String,
        subject: String,
        body: String,
        #[serde(default)]
        smtp_server: Option<String>,
        #[serde(default)]
        smtp_port: Option<u16>,
        #[serde(default)]
        smtp_username: Option<String>,
        #[serde(default)]
        smtp_password: Option<String>,
    },
    #[serde(rename = "cmd")]
    Cmd {
        cmd: String,
    },
    #[serde(rename = "webhook")]
    Webhook {
        url: String,
        #[serde(default = "default_webhook_method")]
        method: String,
        body: String,
        #[serde(default)]
        headers: Vec<String>,
    },
}

fn default_webhook_method() -> String {
    "POST".to_string()
}

pub struct TaskExecutionDetails {
    pub task_name: String,
    pub exit_code: i32,
    pub start_time: DateTime<Utc>,
    pub duration: Duration,
    pub error_message: String,
    pub debug_info: String,
    pub stdout: String,
    pub stderr: String,
}

pub fn send_alert(alert: &Alert, details: &TaskExecutionDetails) -> Result<()> {
    match alert {
        Alert::Email {
            to,
            subject,
            body,
            smtp_server,
            smtp_port,
            smtp_username,
            smtp_password,
        } => {
            let body = template_replace(body, details);
            let subject = template_replace(subject, details);

            // TODO use SMTP client library instead of shell command
            let mut cmd = Command::new("mail");
            cmd.arg("-s").arg(&subject);

            // TODO: for each values that is None, use the default value, e.g. smtp_port = 25
            if let (Some(server), Some(port), Some(username), Some(password)) = 
                (smtp_server, smtp_port, smtp_username, smtp_password) {
                cmd.arg("-S")
                    .arg(format!("smtp={}:{}", server, port))
                    .arg("-S")
                    .arg(format!("smtp-auth-user={}", username))
                    .arg("-S")
                    .arg(format!("smtp-auth-password={}", password));
            }

            cmd.arg(&to);

            let output = cmd.output()?;
            if !output.status.success() {
                error!(
                    "Failed to send email alert: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        Alert::Cmd { cmd } => {
            let cmd = template_replace(cmd, details);
            let output = Command::new("/bin/sh").arg("-c").arg(&cmd).output()?;
            if !output.status.success() {
                error!(
                    "Failed to execute alert command: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        Alert::Webhook {
            url,
            method,
            body,
            headers,
        } => {
            let body = template_replace(body, details);

            // TODO use a request library instead of shell command
            let mut cmd = Command::new("curl");
            cmd.arg("-X").arg(method).arg(url);

            for header in headers {
                cmd.arg("-H").arg(header);
            }

            cmd.arg("-d").arg(&body);

            let output = cmd.output()?;
            if !output.status.success() {
                error!(
                    "Failed to send webhook alert: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
    Ok(())
}

fn template_replace(
    template: &str,
    details: &TaskExecutionDetails,
) -> String {
    let mut result = template.to_string();
    result = result.replace("{{ task_name }}", &details.task_name);
    result = result.replace("{{ exit_code }}", &details.exit_code.to_string());
    result = result.replace("{{ start_time }}", &details.start_time.to_rfc3339());
    result = result.replace("{{ duration }}", &details.duration.as_secs().to_string());
    result = result.replace("{{ error_message }}", &details.error_message);
    result = result.replace("{{ debug_info }}", &details.debug_info);
    result = result.replace("{{ stdout }}", &details.stdout);
    result = result.replace("{{ stderr }}", &details.stderr);
    result
}