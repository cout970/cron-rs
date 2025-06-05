use std::time::Duration;

/// Converts a Duration to a human-readable string with at most 2 units
/// e.g., "1 h, 30 m", "5 m, 20 s", "1 s, 133 ms", "10 ms"
pub fn format_duration(duration: Duration) -> String {
    let total_ms = duration.as_millis();

    // Unit definitions in milliseconds
    const MS_PER_SEC: u128 = 1_000;
    const MS_PER_MIN: u128 = MS_PER_SEC * 60;
    const MS_PER_HOUR: u128 = MS_PER_MIN * 60;
    const MS_PER_DAY: u128 = MS_PER_HOUR * 24;

    // Early return for zero duration
    if total_ms == 0 {
        return "0 ms".to_string();
    }

    // Calculate components
    let days = total_ms / MS_PER_DAY;
    let hours = (total_ms % MS_PER_DAY) / MS_PER_HOUR;
    let minutes = (total_ms % MS_PER_HOUR) / MS_PER_MIN;
    let seconds = (total_ms % MS_PER_MIN) / MS_PER_SEC;
    let milliseconds = total_ms % MS_PER_SEC;

    // Find the first two non-zero components
    let mut result = String::new();
    let mut units_added = 0;

    if days > 0 && units_added < 2 {
        result.push_str(&format!("{} d", days));
        units_added += 1;
    }

    if hours > 0 && units_added < 2 {
        if !result.is_empty() {
            result.push_str(", ");
        }
        result.push_str(&format!("{} h", hours));
        units_added += 1;
    }

    if minutes > 0 && units_added < 2 {
        if !result.is_empty() {
            result.push_str(", ");
        }
        result.push_str(&format!("{} m", minutes));
        units_added += 1;
    }

    if seconds > 0 && units_added < 2 {
        if !result.is_empty() {
            result.push_str(", ");
        }
        result.push_str(&format!("{} s", seconds));
        units_added += 1;
    }

    if milliseconds > 0 && units_added < 2 {
        if !result.is_empty() {
            result.push_str(", ");
        }
        result.push_str(&format!("{} ms", milliseconds));
        units_added += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        // Test various durations
        assert_eq!(format_duration(Duration::from_secs(0)), "0 ms");
        assert_eq!(format_duration(Duration::from_millis(10)), "10 ms");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1 s, 500 ms");
        assert_eq!(format_duration(Duration::from_secs(65)), "1 m, 5 s");
        assert_eq!(format_duration(Duration::from_secs(3600 + 120)), "1 h, 2 m");
        assert_eq!(format_duration(Duration::from_secs(86400 + 3600)), "1 d, 1 h");

        // Check more complex durations
        let duration = Duration::from_secs(90061); // 1 d, 1 h, 1 m, 1 s
        assert_eq!(format_duration(duration), "1 d, 1 h"); // Should only show first 2 units

        let duration = Duration::from_millis(59999); // 59 s, 999 ms
        assert_eq!(format_duration(duration), "59 s, 999 ms");
    }
}