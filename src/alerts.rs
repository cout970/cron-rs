use crate::utils::format_duration;
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
        #[serde(default = "default_escape_email")]
        escape: EscapeStrategy,
    },
    #[serde(rename = "cmd")]
    Cmd {
        cmd: String,
        #[serde(default = "default_escape_cmd")]
        escape: EscapeStrategy,
    },
    #[serde(rename = "webhook")]
    Webhook {
        url: String,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        headers: HashMap<String, String>,
        #[serde(default = "default_escape_webhook")]
        escape: EscapeStrategy,
    },
}

pub struct TaskExecutionDetails {
    pub task_name: String,
    pub task_id: u32,
    pub pid: u32,
    pub exit_code: i32,
    pub start_time: DateTime<Utc>,
    pub duration: Duration,
    pub error_message: String,
    pub debug_info: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EscapeStrategy {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "json")]
    Json,
    #[serde(rename = "html")]
    Html,
    #[serde(rename = "shell")]
    Shell,
}

fn default_escape_email() -> EscapeStrategy {
    EscapeStrategy::Html
}

fn default_escape_cmd() -> EscapeStrategy {
    EscapeStrategy::Shell
}

fn default_escape_webhook() -> EscapeStrategy {
    EscapeStrategy::Json
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
            escape,
        } => {
            let from = from.clone().unwrap_or_else(|| "cron-rs@localhost".to_string());
            let body = body
                .clone()
                .unwrap_or_else(|| "Task {{ task_name }} failed with exit code {{ exit_code }}".to_string());
            let subject = subject.clone().unwrap_or_else(|| "Task Failure Alert".to_string());

            let body = template_replace(&body, details, escape);
            let subject = template_replace(&subject, details, escape);

            let email = Message::builder()
                .from(from.parse()?)
                .to(to.parse()?)
                .subject(subject)
                .body(body)?;

            let server = smtp_server.clone().unwrap_or_else(|| "localhost".to_string());
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
        Alert::Cmd { cmd, escape } => {
            let cmd = template_replace(cmd, details, escape);
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
            escape,
        } => {
            let body = body
                .clone()
                .unwrap_or_else(|| "Task {{ task_name }} failed with exit code {{ exit_code }}".to_string());
            let body = template_replace(&body, details, escape);

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
                        error!(
                            "Webhook request failed with status: {}, '{}'",
                            response.status(),
                            response.text().unwrap_or_default()
                        );
                    }
                }
                Err(e) => error!("Failed to send webhook: {}", e),
            }
        }
    }
    Ok(())
}

fn template_replace(template: &str, details: &TaskExecutionDetails, escape: &EscapeStrategy) -> String {
    let mut result = template.to_string();

    fn replace_and_escape(result: &mut String, placeholder: &str, value: &str, escape: &EscapeStrategy) {
        let start = "{{";
        let end = "{{";
        let with_spaces = format!("{} {} {}", start, placeholder, end);
        if result.contains(&with_spaces) {
            let escaped_value = template_escape(value, escape);
            *result = result.replace(&with_spaces, &escaped_value);
        }

        let without_spaces = format!("{}{}{}", start, placeholder, end);
        if result.contains(&without_spaces) {
            let escaped_value = template_escape(value, escape);
            *result = result.replace(&without_spaces, &escaped_value);
        }
    }

    replace_and_escape(&mut result, "task_id", &details.task_id.to_string(), escape);
    replace_and_escape(&mut result, "pid", &details.pid.to_string(), escape);
    replace_and_escape(&mut result, "task_name", &details.task_name, escape);
    replace_and_escape(&mut result, "exit_code", &details.exit_code.to_string(), escape);
    replace_and_escape(&mut result, "start_time", &details.start_time.to_rfc3339(), escape);
    replace_and_escape(&mut result, "duration", &format_duration(details.duration), escape);
    replace_and_escape(
        &mut result,
        "end_time",
        &details
            .start_time
            .add(TimeDelta::from_std(details.duration).unwrap())
            .to_rfc3339(),
        escape,
    );
    replace_and_escape(&mut result, "error_message", &details.error_message, escape);
    replace_and_escape(&mut result, "debug_info", &details.debug_info, escape);
    replace_and_escape(&mut result, "stdout", details.stdout.trim(), escape);
    replace_and_escape(&mut result, "stderr", details.stderr.trim(), escape);

    result
}

fn template_escape(value: &str, strategy: &EscapeStrategy) -> String {
    match strategy {
        EscapeStrategy::None => value.trim().to_string(),
        EscapeStrategy::Json => escape_json_string(value.trim()),
        EscapeStrategy::Html => escape_html_string(value.trim()),
        EscapeStrategy::Shell => escape_shell_arg_string(value.into()),
    }
}

pub fn escape_json_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());

    for c in s.chars() {
        match c {
            '"' => result.push_str(r#"\""#),
            '\\' => result.push_str(r"\\"),
            '\n' => result.push_str(r"\n"),
            '\r' => result.push_str(r"\r"),
            '\t' => result.push_str(r"\t"),
            '\x08' => result.push_str(r"\b"), // backspace
            '\x0C' => result.push_str(r"\f"), // form feed
            c if c.is_control() => {
                result.push_str(&format!(r"\u{:04x}", c as u32));
            }
            c => result.push(c),
        }
    }

    result
}

pub fn escape_html_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());

    for c in s.chars() {
        match c {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&#39;"),
            '/' => result.push_str("&#x2F;"),
            '`' => result.push_str("&#x60;"),
            '=' => result.push_str("&#x3D;"),
            c => result.push(c),
        }
    }

    result
}

pub fn escape_shell_arg_string(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }

    let needs_quoting = s.chars().any(|c| {
        matches!(
            c,
            ' ' | '\t'
                | '\n'
                | '\r'
                | '\''
                | '"'
                | '\\'
                | '$'
                | '`'
                | '!'
                | '*'
                | '?'
                | '['
                | ']'
                | '{'
                | '}'
                | '('
                | ')'
                | '<'
                | '>'
                | '|'
                | '&'
                | ';'
                | '#'
                | '~'
                | '\x00'..='\x1F' | '\x7F'
        )
    }) || s.starts_with('-');

    if !needs_quoting {
        return s.to_string();
    }

    let mut result = String::from("'");

    for c in s.chars() {
        match c {
            '\'' => result.push_str(r"'\''"),
            '\0' => {
                // ignore null bytes
            }
            c => result.push(c),
        }
    }

    result.push('\'');
    result
}
