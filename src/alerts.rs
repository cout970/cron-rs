use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use log::{error, info};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Add;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};
use crate::utils::format_duration;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AlertConfig {
    #[serde(default)]
    pub on_failure: Vec<Alert>,
    #[serde(default)]
    pub on_success: Vec<Alert>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Alert {
    #[serde(rename = "email")]
    Email {
        to: String,
        #[serde(default)]
        subject: Option<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        from: Option<String>,
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
    Cmd { cmd: String },
    #[serde(rename = "webhook")]
    Webhook {
        url: String,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
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
            from,
            to,
            subject,
            body,
            smtp_server,
            smtp_port,
            smtp_username,
            smtp_password,
        } => {
            let from = from
                .clone()
                .unwrap_or_else(|| "cron-rs@localhost".to_string());
            let body = body.clone().unwrap_or_else(|| {
                "Task {{ task_name }} failed with exit code {{ exit_code }}".to_string()
            });
            let subject = subject
                .clone()
                .unwrap_or_else(|| "Task Failure Alert".to_string());

            let body = template_replace(&body, details);
            let subject = template_replace(&subject, details);

            let email = Message::builder()
                .from(from.parse()?)
                .to(to.parse()?)
                .subject(subject)
                .body(body)?;

            let server = smtp_server
                .clone()
                .unwrap_or_else(|| "localhost".to_string());
            let port = smtp_port.unwrap_or(25);
            let username = smtp_username.clone().unwrap_or_default();
            let password = smtp_password.clone().unwrap_or_default();

            let mut mailer = if server == "localhost" || port == 25 {
                SmtpTransport::builder_dangerous(server).port(port)
            } else {
                SmtpTransport::relay(&server)?.port(port)
            };

            if let (Some(username), Some(password)) = (smtp_username, smtp_password) {
                mailer = mailer.credentials(Credentials::new(username.clone(), password.clone()));
            }

            match mailer.build().send(&email) {
                Ok(_) => info!("Email sent successfully"),
                Err(e) => error!("Failed to send email: {}", e),
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
            let body = body.clone().unwrap_or_else(|| {
                "Task {{ task_name }} failed with exit code {{ exit_code }}".to_string()
            });
            let body = template_replace(&body, details);

            let client = Client::new();
            let mut request = match method.as_deref() {
                Some("GET") => client.get(url),
                Some("POST") => client.post(url),
                Some("PUT") => client.put(url),
                Some("PATCH") => client.patch(url),
                Some("DELETE") => client.delete(url),
                _ => client.post(url),
            };

            let mut header_map = HeaderMap::new();
            for (key, value) in headers {
                header_map.insert(
                    HeaderName::from_bytes(key.trim().as_bytes())?,
                    HeaderValue::from_str(value.trim())?,
                );
            }
            request = request.headers(header_map).body(body);

            match request.send() {
                Ok(response) => {
                    if !response.status().is_success() {
                        error!("Webhook request failed with status: {}", response.status());
                    }
                }
                Err(e) => error!("Failed to send webhook: {}", e),
            }
        }
    }
    Ok(())
}

fn template_replace(template: &str, details: &TaskExecutionDetails) -> String {
    let mut result = template.to_string();
    result = result.replace("{{ task_name }}", &details.task_name);
    result = result.replace("{{ exit_code }}", &details.exit_code.to_string());
    result = result.replace("{{ start_time }}", &details.start_time.to_rfc3339());
    result = result.replace("{{ duration }}", &format_duration(details.duration));
    result = result.replace("{{ end_time }}", &details.start_time.add(TimeDelta::from_std(details.duration).unwrap()).to_rfc3339());
    result = result.replace("{{ error_message }}", &details.error_message);
    result = result.replace("{{ debug_info }}", &details.debug_info);
    result = result.replace("{{ stdout }}", details.stdout.trim());
    result = result.replace("{{ stderr }}", details.stderr.trim());
    result
}
