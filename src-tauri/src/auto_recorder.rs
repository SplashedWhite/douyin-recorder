use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct CheckState {
    next_check: Instant,
    failures: u32,
}

pub struct AutoRecorder {
    checks: Mutex<HashMap<i64, CheckState>>,
}

impl AutoRecorder {
    pub fn new() -> Self {
        Self {
            checks: Mutex::new(HashMap::new()),
        }
    }

    pub fn mark_immediate(&self, room_id: i64) {
        if let Ok(mut checks) = self.checks.lock() {
            checks.insert(
                room_id,
                CheckState {
                    next_check: Instant::now(),
                    failures: 0,
                },
            );
        }
    }

    pub fn clear(&self, room_id: i64) {
        if let Ok(mut checks) = self.checks.lock() {
            checks.remove(&room_id);
        }
    }

    pub fn is_due(&self, room_id: i64) -> bool {
        self.checks
            .lock()
            .map(|checks| {
                checks
                    .get(&room_id)
                    .is_none_or(|state| state.next_check <= Instant::now())
            })
            .unwrap_or(false)
    }

    pub fn mark_success(&self, room_id: i64, interval_secs: u64) {
        let delay = success_delay_secs(room_id, interval_secs);
        if let Ok(mut checks) = self.checks.lock() {
            checks.insert(
                room_id,
                CheckState {
                    next_check: Instant::now() + Duration::from_secs(delay),
                    failures: 0,
                },
            );
        }
    }

    pub fn mark_failure(&self, room_id: i64, interval_secs: u64, rate_limited: bool) {
        if let Ok(mut checks) = self.checks.lock() {
            let failures = checks
                .get(&room_id)
                .map(|state| state.failures.saturating_add(1))
                .unwrap_or(1);
            let delay = failure_delay_secs(interval_secs, failures, rate_limited);
            checks.insert(
                room_id,
                CheckState {
                    next_check: Instant::now() + Duration::from_secs(delay),
                    failures,
                },
            );
        }
    }
}

fn success_delay_secs(room_id: i64, interval_secs: u64) -> u64 {
    let jitter_limit = interval_secs / 10;
    let jitter = if jitter_limit == 0 {
        0
    } else {
        room_id.unsigned_abs() % (jitter_limit + 1)
    };
    interval_secs.saturating_add(jitter)
}

fn failure_delay_secs(interval_secs: u64, failures: u32, rate_limited: bool) -> u64 {
    if rate_limited {
        return 30 * 60;
    }
    let multiplier = 1_u64.checked_shl(failures.min(10)).unwrap_or(u64::MAX);
    interval_secs.saturating_mul(multiplier).min(15 * 60)
}

#[cfg(test)]
mod tests {
    use super::{failure_delay_secs, success_delay_secs};

    #[test]
    fn success_delay_uses_deterministic_jitter_up_to_ten_percent() {
        assert_eq!(success_delay_secs(1, 60), 61);
        assert_eq!(success_delay_secs(8, 60), 61);
        assert!((60..=66).contains(&success_delay_secs(1234, 60)));
    }

    #[test]
    fn applies_exponential_failure_backoff_with_cap() {
        assert_eq!(failure_delay_secs(60, 1, false), 120);
        assert_eq!(failure_delay_secs(60, 3, false), 480);
        assert_eq!(failure_delay_secs(60, 8, false), 900);
    }

    #[test]
    fn rate_limit_uses_thirty_minute_backoff() {
        assert_eq!(failure_delay_secs(10, 1, true), 1800);
    }
}
