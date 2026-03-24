use crate::alerts::{Alert, EscapeStrategy};
use log::{info, warn};

use super::file::{
    ExplodedTimePatternConfig, ExplodedTimePatternFieldConfig, TaskDefinition, TimePatternConfig,
};

/// Loads tasks from all standard system crontab locations and returns them as
/// a flat `Vec<TaskDefinition>` ready to be merged into a [`ConfigFile`].
///
/// Sources (in order):
/// - `/etc/crontab`
/// - `/etc/cron.d/*`  (files without dots or trailing `~` in their name)
/// - Scripts in `/etc/cron.hourly/`, `/etc/cron.daily/`, `/etc/cron.weekly/`,
///   `/etc/cron.monthly/`  (executable files only, following `run-parts` conventions)
///
/// Missing paths or unreadable files are logged as warnings and skipped.
pub fn load_crontab_tasks() -> Vec<TaskDefinition> {
    let mut tasks: Vec<TaskDefinition> = vec![];

    // /etc/crontab
    match std::fs::read_to_string("/etc/crontab") {
        Ok(content) => {
            let extra = parse_system_crontab_file(&content, "crontab");
            info!("cron-compat: loaded {} task(s) from /etc/crontab", extra.len());
            tasks.extend(extra);
        }
        Err(e) => warn!("cron-compat: could not read /etc/crontab: {}", e),
    }

    // /etc/cron.d/*
    match std::fs::read_dir("/etc/cron.d") {
        Ok(entries) => {
            let mut sorted: Vec<_> = entries.flatten().collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                // Skip dpkg/rpm artifacts and backup files (run-parts convention)
                if file_name.ends_with('~') || file_name.contains('.') {
                    continue;
                }
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        let source = format!("cron.d/{}", file_name);
                        let extra = parse_system_crontab_file(&content, &source);
                        info!("cron-compat: loaded {} task(s) from {}", extra.len(), path.display());
                        tasks.extend(extra);
                    }
                    Err(e) => warn!("cron-compat: could not read {}: {}", path.display(), e),
                }
            }
        }
        Err(e) => warn!("cron-compat: could not read /etc/cron.d: {}", e),
    }

    // Script directories — times mirror typical Debian/Ubuntu defaults
    let cron_dirs: &[(&str, &str)] = &[
        ("/etc/cron.hourly",  "* *-*-* *:17:00"),
        ("/etc/cron.daily",   "* *-*-* 06:25:00"),
        ("/etc/cron.weekly",  "[Sun] *-*-* 06:47:00"),
        ("/etc/cron.monthly", "* *-*-1 06:52:00"),
    ];

    for (dir, schedule_str) in cron_dirs {
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                let mut sorted: Vec<_> = entries.flatten().collect();
                sorted.sort_by_key(|e| e.file_name());
                for entry in sorted {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    let file_name =
                        path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    // run-parts skips dotfiles, files with extensions and backups
                    if file_name.starts_with('.') || file_name.ends_with('~') || file_name.contains('.') {
                        continue;
                    }
                    // run-parts only runs executable files
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(meta) = path.metadata() {
                            if meta.permissions().mode() & 0o111 == 0 {
                                continue;
                            }
                        }
                    }
                    let task_name = format!("{}/{}", dir, file_name);
                    let cmd = path.to_string_lossy().to_string();
                    info!("cron-compat: adding task '{}' from {}", task_name, path.display());
                    tasks.push(TaskDefinition {
                        name: task_name,
                        cmd,
                        when: Some(TimePatternConfig::Short(schedule_str.to_string())),
                        ..Default::default()
                    });
                }
            }
            Err(e) => warn!("cron-compat: could not read {}: {}", dir, e),
        }
    }

    // User scripts
    match std::fs::read_dir("/var/spool/cron") {
        Ok(entries) => {
            let mut sorted: Vec<_> = entries.flatten().collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        let mut extra = parse_crontab_file(&content).unwrap_or_else(|e| {
                            warn!("cron-compat: could not parse {}: {}", path.display(), e);
                            vec![]
                        });
                        for task in &mut extra {
                            task.run_as = Some(file_name.clone());
                        }
                        info!("cron-compat: loaded {} task(s) from {}", extra.len(), path.display());
                        tasks.extend(extra);
                    }
                    Err(e) => warn!("cron-compat: could not read {}: {}", path.display(), e),
                }
            }
        }
        Err(e) => warn!("cron-compat: could not read /var/spool/cron/: {}", e),
    }

    tasks
}

/// Parses a user-style crontab (e.g. from `crontab -l` or a personal crontab file).
/// Format: `minute hour day month dow  command`
/// Used by the `generate-from-crontab` subcommand.
pub fn parse_crontab_file(crontab: &str) -> anyhow::Result<Vec<TaskDefinition>> {
    let mut tasks = vec![];
    let mut last_comment = String::new();

    for line in crontab.lines() {
        let line = line.trim();
        if line.is_empty() {
            last_comment.clear();
            continue;
        }

        if line.starts_with('#') {
            last_comment.push(' ');
            last_comment.push_str(line[1..].trim());
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 6 {
            last_comment.clear();
            continue;
        }

        let (minute, hour, day, month, dow) = (parts[0], parts[1], parts[2], parts[3], parts[4]);
        let cmd = parts[5..].join(" ");

        let name = if last_comment.trim().is_empty() {
            format!("Crontab: {}", line)
        } else {
            last_comment.trim().to_string()
        };

        tasks.push(TaskDefinition {
            name,
            cmd,
            when: Some(TimePatternConfig::Long(make_when_pattern(minute, hour, day, month, dow))),
            ..Default::default()
        });
        last_comment.clear();
    }

    Ok(tasks)
}

/// Parses a system-style crontab file (e.g. `/etc/crontab` or `/etc/cron.d/*`).
/// Format: `minute hour day month dow  username  command`
///
/// The `username` field is mapped to `run_as`.
/// Variable assignments are handled:
///   `SHELL=<path>`  → sets the `shell` field on all subsequent tasks.
///   `MAILTO=<addr>` → adds an `on_failure` email alert (via localhost MTA) on all subsequent
///                     tasks. An empty value (`MAILTO=` or `MAILTO=""`) clears it.
///   Any other key   → a warning is emitted and the line is ignored.
///
/// `run-parts` entries that reference the cron.hourly/daily/weekly/monthly directories are
/// skipped because those scripts are loaded directly by [`load_crontab_tasks`].
pub fn parse_system_crontab_file(content: &str, source: &str) -> Vec<TaskDefinition> {
    const CRON_SCRIPT_DIRS: &[&str] = &[
        "/etc/cron.hourly",
        "/etc/cron.daily",
        "/etc/cron.weekly",
        "/etc/cron.monthly",
    ];

    let mut tasks = vec![];
    let mut last_comment = String::new();
    let mut current_shell: Option<String> = None;
    // None = not set, Some("") = explicitly cleared (no mail)
    let mut current_mailto: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty() {
            last_comment.clear();
            continue;
        }

        if line.starts_with('#') {
            last_comment.push(' ');
            last_comment.push_str(line[1..].trim());
            continue;
        }

        // Variable assignments: KEY=value  (alphanumeric + _, not starting with @)
        if !line.starts_with('@') {
            if let Some(eq_pos) = line.find('=') {
                let key = line[..eq_pos].trim();
                if !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    let raw = line[eq_pos + 1..].trim();
                    let value = strip_quotes(raw);

                    match key {
                        "SHELL" => {
                            info!("cron-compat: {}: SHELL={}", source, value);
                            current_shell = if value.is_empty() { None } else { Some(value.to_string()) };
                        }
                        "MAILTO" => {
                            if value.is_empty() {
                                info!("cron-compat: {}: MAILTO cleared", source);
                            } else {
                                info!("cron-compat: {}: MAILTO={}", source, value);
                            }
                            current_mailto = Some(value.to_string());
                        }
                        other => {
                            warn!(
                                "cron-compat: {}: unsupported variable '{}={}', ignoring",
                                source, other, value
                            );
                        }
                    }
                    last_comment.clear();
                    continue;
                }
            }
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            last_comment.clear();
            continue;
        }

        // Determine schedule fields, username and command parts
        let (schedule_fields, username, cmd_parts) = if parts[0].starts_with('@') {
            if parts.len() < 3 {
                last_comment.clear();
                continue;
            }
            (std::slice::from_ref(&parts[0]), parts[1], &parts[2..])
        } else {
            if parts.len() < 7 {
                last_comment.clear();
                continue;
            }
            (&parts[..5], parts[5], &parts[6..])
        };

        let cmd = cmd_parts.join(" ");

        // Skip run-parts entries — those directories are handled directly
        if cmd.contains("run-parts") && CRON_SCRIPT_DIRS.iter().any(|d| cmd.contains(d)) {
            last_comment.clear();
            continue;
        }

        // Resolve `when` schedule — return None to skip the entry (e.g. @reboot)
        let when_opt: Option<TimePatternConfig> = if schedule_fields[0].starts_with('@') {
            match schedule_fields[0] {
                "@hourly"               => Some(TimePatternConfig::Short("* *-*-* *:00:00".into())),
                "@daily" | "@midnight"  => Some(TimePatternConfig::Short("* *-*-* 00:00:00".into())),
                "@weekly"               => Some(TimePatternConfig::Short("[Sun] *-*-* 00:00:00".into())),
                "@monthly"              => Some(TimePatternConfig::Short("* *-*-1 00:00:00".into())),
                "@yearly" | "@annually" => Some(TimePatternConfig::Short("* *-1-1 00:00:00".into())),
                "@reboot" => {
                    warn!("cron-compat: {}: skipping @reboot entry: {}", source, cmd);
                    None
                }
                other => {
                    warn!("cron-compat: {}: unknown shorthand '{}', skipping: {}", source, other, cmd);
                    None
                }
            }
        } else {
            let (minute, hour, day, month, dow) = (
                schedule_fields[0], schedule_fields[1], schedule_fields[2],
                schedule_fields[3], schedule_fields[4],
            );
            Some(TimePatternConfig::Long(make_when_pattern(minute, hour, day, month, dow)))
        };

        let Some(when) = when_opt else {
            last_comment.clear();
            continue;
        };

        let name = if last_comment.trim().is_empty() {
            format!("{}: {}", source, cmd)
        } else {
            format!("{}: {}", source, last_comment.trim())
        };

        tasks.push(TaskDefinition {
            name,
            cmd,
            when: Some(when),
            run_as: Some(username.to_string()),
            shell: current_shell.clone(),
            on_failure: build_mailto_alerts(&current_mailto),
            ..Default::default()
        });

        last_comment.clear();
    }

    tasks
}


/// Helper to build an `ExplodedTimePatternConfig` from the 5 standard cron fields.
fn make_when_pattern(
    minute: &str,
    hour: &str,
    day: &str,
    month: &str,
    dow: &str,
) -> ExplodedTimePatternConfig {
    ExplodedTimePatternConfig {
        second: None,
        minute: Some(map_cron_field(minute, false)),
        hour: Some(map_cron_field(hour, false)),
        day: Some(map_cron_field(day, false)),
        month: Some(map_cron_field(month, false)),
        year: None,
        day_of_week: Some(map_cron_field(dow, true)),
    }
}

/// Maps a single standard-cron field string to an `ExplodedTimePatternFieldConfig`.
/// Handles `*`, `*/n`, bare numbers, `n-m` ranges, and comma-separated combinations.
/// When `is_dow` is true, value `7` is normalized to `0` (both mean Sunday in standard cron).
pub fn map_cron_field(s: &str, is_dow: bool) -> ExplodedTimePatternFieldConfig {
    // Standard cron uses `-` for ranges; cron-rs uses `..`
    let text = s.replace('-', "..");

    if text.contains(',') {
        let mut result: Vec<String> = vec![];

        for opt in text.split(',').map(str::trim) {
            if opt.contains("..") && !opt.starts_with("*/") {
                // Expanded range: 1..5
                let parts: Vec<&str> = opt.splitn(2, "..").collect();
                match (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                    (Ok(start), Ok(end)) if start <= end => {
                        for i in start..=end {
                            result.push(if is_dow && i == 7 { "0".to_string() } else { i.to_string() });
                        }
                    }
                    _ => warn!("cron-compat: invalid range '{}', skipping", opt),
                }
            } else {
                if is_dow {
                    if let Ok(n) = opt.parse::<u32>() {
                        result.push(if n == 7 { "0".to_string() } else { n.to_string() });
                        continue;
                    }
                }
                result.push(opt.to_string());
            }
        }

        if result.len() == 1 {
            ExplodedTimePatternFieldConfig::Text(result.into_iter().next().unwrap())
        } else {
            ExplodedTimePatternFieldConfig::Text(format!("[{}]", result.join(", ")))
        }
    } else {
        // Single value — normalise DOW 7 → 0
        let text = if is_dow {
            if let Ok(n) = text.parse::<u32>() {
                if n == 7 { "0".to_string() } else { text }
            } else {
                text
            }
        } else {
            text
        };
        ExplodedTimePatternFieldConfig::Text(text)
    }
}

/// Strips matching single or double quotes from the start and end of a string, if present.
fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Builds a vector of `Alert::Email` based on the provided `mailto` value.
fn build_mailto_alerts(mailto: &Option<String>) -> Vec<Alert> {
    match mailto {
        Some(addr) if !addr.is_empty() => vec![Alert::Email {
            to: addr.clone(),
            subject: None,
            body: None,
            from: None,
            smtp_server: None, // defaults to localhost:25 — same as classic cron
            smtp_port: None,
            smtp_username: None,
            smtp_password: None,
            escape: EscapeStrategy::Html,
        }],
        _ => vec![],
    }
}
