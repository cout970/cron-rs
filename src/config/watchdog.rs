use serde::{Deserialize, Serialize};

/// Top-level watchdog configuration block (optional in the YAML file).
///
/// ```yaml
/// watchdog:
///   clock_shift:
///     enabled: true
///     check_interval_secs: 30
///     threshold_secs: 60
///   scheduler:
///     enabled: true
///     check_interval_secs: 30
///     max_silence_secs: 120
///     on_stuck: warn   # warn | restart
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WatchdogConfig {
    #[serde(default)]
    pub clock_shift: ClockShiftConfig,
    #[serde(default)]
    pub scheduler: SchedulerWatchdogConfig,
}

/// Detects sudden wall-clock jumps (e.g. VM resume, large NTP step) by comparing
/// the `SystemTime` delta against the monotonic `Instant` delta between checks.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClockShiftConfig {
    /// Whether clock-shift detection is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How often to sample the clocks, in seconds.
    #[serde(default = "default_clock_check_interval")]
    pub check_interval_secs: u64,
    /// Minimum discrepancy between wall-clock and monotonic elapsed time (in seconds)
    /// that triggers a warning.
    #[serde(default = "default_clock_threshold")]
    pub threshold_secs: u64,
}

impl Default for ClockShiftConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_secs: default_clock_check_interval(),
            threshold_secs: default_clock_threshold(),
        }
    }
}

/// Monitors the scheduler's async runtime via a heartbeat counter.
/// If the heartbeat stops updating, the runtime is considered stuck.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SchedulerWatchdogConfig {
    /// Whether scheduler-liveness monitoring is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How often to check the heartbeat, in seconds.
    #[serde(default = "default_scheduler_check_interval")]
    pub check_interval_secs: u64,
    /// Maximum allowed silence (in seconds) before the scheduler is considered stuck.
    #[serde(default = "default_max_silence")]
    pub max_silence_secs: u64,
    /// What to do when the scheduler appears stuck.
    #[serde(default)]
    pub on_stuck: OnStuckAction,
}

impl Default for SchedulerWatchdogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_secs: default_scheduler_check_interval(),
            max_silence_secs: default_max_silence(),
            on_stuck: OnStuckAction::default(),
        }
    }
}

/// Action taken when the scheduler heartbeat goes silent.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OnStuckAction {
    /// Log a warning but keep running.
    #[default]
    Warn,
    /// Log an error and call `std::process::exit(1)` so the service manager can restart.
    Restart,
}

fn default_true() -> bool { true }
fn default_clock_check_interval() -> u64 { 60 }
fn default_clock_threshold() -> u64 { 120 }
fn default_scheduler_check_interval() -> u64 { 60 }
fn default_max_silence() -> u64 { 180 }
