use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{error, info, warn};

use crate::config::watchdog::{OnStuckAction, WatchdogConfig};

/// Inner polling interval of the watchdog thread.
/// Each subsystem fires at its own configured interval; this is just the granularity.
const POLL_SECS: u64 = 60;

/// Spawns the watchdog background thread.
///
/// # Parameters
/// - `config`: shared, hot-reload-aware watchdog configuration.
/// - `heartbeat`: monotonically increasing epoch-seconds counter updated by the
///   scheduler's async runtime. The watchdog checks it is not stale.
pub fn spawn(config: Arc<RwLock<WatchdogConfig>>, heartbeat: Arc<AtomicU64>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("watchdog".to_string())
        .spawn(move || run(config, heartbeat))
        .expect("failed to spawn watchdog thread")
}

fn run(config: Arc<RwLock<WatchdogConfig>>, heartbeat: Arc<AtomicU64>) {
    let poll = Duration::from_secs(POLL_SECS);

    // Clock-shift detection state — kept across iterations.
    // We compare the SystemTime delta against the Instant delta:
    //   • Instant is monotonic and does NOT advance during suspend.
    //   • SystemTime advances with the wall clock (including after resume/NTP step).
    // A large discrepancy between the two signals a clock jump.
    let mut ref_instant = Instant::now();
    let mut ref_system = SystemTime::now();
    let mut last_clock_check = Instant::now();

    // Scheduler-heartbeat state.
    let mut last_heartbeat_check = Instant::now();

    {
        let cfg = config.read().unwrap();
        info!(
            "Watchdog started (clock_shift={}, scheduler={})",
            cfg.clock_shift.enabled,
            cfg.scheduler.enabled,
        );
    }

    loop {
        thread::sleep(poll);

        let now_instant = Instant::now();
        let now_system  = SystemTime::now();

        // Take a snapshot of the config so we don't hold the lock across log calls.
        let cfg = config.read().unwrap().clone();

        // ── Clock-shift check ─────────────────────────────────────────────────
        if cfg.clock_shift.enabled
            && now_instant.duration_since(last_clock_check).as_secs()
                >= cfg.clock_shift.check_interval_secs
        {
            let monotonic_elapsed = now_instant.duration_since(ref_instant).as_secs_f64();
            let wall_elapsed = now_system
                .duration_since(ref_system)
                .unwrap_or(Duration::ZERO)
                .as_secs_f64();

            // Positive shift → clock jumped forward (VM resume / NTP step-forward).
            // Negative shift → clock jumped backward (NTP correction, DST, etc.).
            let shift = wall_elapsed - monotonic_elapsed;

            if shift.abs() >= cfg.clock_shift.threshold_secs as f64 {
                if shift > 0.0 {
                    warn!(
                        "Watchdog: wall clock jumped forward by ~{:.0}s \
                         (VM resume or NTP step-forward).",
                        shift
                    );
                } else {
                    warn!(
                        "Watchdog: wall clock jumped backward by ~{:.0}s \
                         (NTP correction or manual change).",
                        -shift
                    );
                }
            }

            ref_instant = now_instant;
            ref_system  = now_system;
            last_clock_check = now_instant;
        }

        // ── Scheduler heartbeat check ─────────────────────────────────────────
        if cfg.scheduler.enabled
            && now_instant.duration_since(last_heartbeat_check).as_secs()
                >= cfg.scheduler.check_interval_secs
        {
            let heartbeat_ts = heartbeat.load(Ordering::Relaxed);
            let now_secs = now_system
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs();

            let silence_secs = now_secs.saturating_sub(heartbeat_ts);

            if silence_secs >= cfg.scheduler.max_silence_secs {
                match cfg.scheduler.on_stuck {
                    OnStuckAction::Warn => {
                        warn!(
                            "Watchdog: scheduler heartbeat silent for {}s \
                             (threshold {}s) — async runtime may be stuck.",
                            silence_secs, cfg.scheduler.max_silence_secs
                        );
                    }
                    OnStuckAction::Restart => {
                        error!(
                            "Watchdog: scheduler heartbeat silent for {}s \
                             (threshold {}s) — restarting process.",
                            silence_secs, cfg.scheduler.max_silence_secs
                        );
                        std::process::exit(1);
                    }
                }
            }

            last_heartbeat_check = now_instant;
        }
    }
}
