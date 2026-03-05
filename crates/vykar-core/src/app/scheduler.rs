use std::time::Duration;

use rand::Rng;

use crate::config::ScheduleConfig;
use vykar_types::error::{Result, VykarError};

pub fn random_jitter(jitter_seconds: u64) -> Duration {
    if jitter_seconds == 0 {
        return Duration::ZERO;
    }
    let secs = rand::thread_rng().gen_range(0..=jitter_seconds);
    Duration::from_secs(secs)
}

/// Compute the delay until the next cron tick, plus jitter.
fn next_cron_delay(schedule: &ScheduleConfig) -> Result<Duration> {
    let expr = schedule.cron.as_deref().unwrap_or("");
    let cron: croner::Cron = expr
        .parse()
        .map_err(|e| VykarError::Config(format!("schedule.cron: invalid expression: {e}")))?;

    let now = chrono::Local::now();
    let next = cron
        .find_next_occurrence(&now, false)
        .map_err(|e| VykarError::Config(format!("schedule.cron: no next occurrence: {e}")))?;

    let delay = (next - now).to_std().unwrap_or(Duration::from_secs(60));

    Ok(delay + random_jitter(schedule.jitter_seconds))
}

/// Unified entry point: returns the delay until the next scheduled run.
/// Uses cron when `schedule.cron` is set, otherwise falls back to `every` interval.
pub fn next_run_delay(schedule: &ScheduleConfig) -> Result<Duration> {
    if schedule.is_cron() {
        next_cron_delay(schedule)
    } else {
        Ok(schedule.every_duration()? + random_jitter(schedule.jitter_seconds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_parses_valid_value() {
        let schedule = ScheduleConfig {
            enabled: true,
            every: Some("2h".into()),
            cron: None,
            on_startup: false,
            jitter_seconds: 0,
            passphrase_prompt_timeout_seconds: 300,
        };

        let delay = next_run_delay(&schedule).unwrap();
        assert_eq!(delay.as_secs(), 2 * 3600);
    }

    #[test]
    fn interval_defaults_to_24h_when_none() {
        let schedule = ScheduleConfig {
            enabled: true,
            every: None,
            cron: None,
            on_startup: false,
            jitter_seconds: 0,
            passphrase_prompt_timeout_seconds: 300,
        };

        let delay = next_run_delay(&schedule).unwrap();
        assert_eq!(delay.as_secs(), 24 * 3600);
    }

    #[test]
    fn jitter_bounds_are_respected() {
        for _ in 0..64 {
            let jitter = random_jitter(5).as_secs();
            assert!(jitter <= 5);
        }
    }

    #[test]
    fn cron_next_run_is_positive() {
        let schedule = ScheduleConfig {
            enabled: true,
            every: None,
            cron: Some("*/5 * * * *".into()),
            on_startup: false,
            jitter_seconds: 0,
            passphrase_prompt_timeout_seconds: 300,
        };

        let delay = next_run_delay(&schedule).unwrap();
        assert!(delay.as_secs() > 0);
        assert!(delay.as_secs() <= 5 * 60);
    }

    #[test]
    fn cron_with_jitter() {
        let schedule = ScheduleConfig {
            enabled: true,
            every: None,
            cron: Some("0 3 * * *".into()),
            on_startup: false,
            jitter_seconds: 60,
            passphrase_prompt_timeout_seconds: 300,
        };

        let delay = next_run_delay(&schedule).unwrap();
        // Should be positive (cron delay + up to 60s jitter)
        assert!(delay.as_secs() > 0);
    }

    #[test]
    fn next_run_is_in_future() {
        let schedule = ScheduleConfig {
            enabled: true,
            every: Some("30m".into()),
            cron: None,
            on_startup: false,
            jitter_seconds: 0,
            passphrase_prompt_timeout_seconds: 300,
        };

        let delay = next_run_delay(&schedule).unwrap();
        assert!(delay.as_secs() > 0);
    }
}
