use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct CheckState {
    next_check: Instant,
    failures: u32,
    recording_failures: u32,
    recording_started: Option<Instant>,
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
                    recording_failures: 0,
                    recording_started: None,
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
                    recording_failures: 0,
                    recording_started: None,
                },
            );
        }
    }

    pub fn mark_failure(&self, room_id: i64, interval_secs: u64, rate_limited: bool) -> u64 {
        if let Ok(mut checks) = self.checks.lock() {
            let failures = checks
                .get(&room_id)
                .map(|state| state.failures.saturating_add(1))
                .unwrap_or(1);
            let delay = failure_delay_secs(interval_secs, failures, rate_limited);
            let recording_failures = checks.get(&room_id).map_or(0, |s| s.recording_failures);
            checks.insert(
                room_id,
                CheckState {
                    next_check: Instant::now() + Duration::from_secs(delay),
                    failures,
                    recording_failures,
                    recording_started: None,
                },
            );
            return delay;
        }
        failure_delay_secs(interval_secs, 1, rate_limited)
    }

    pub fn mark_recording_started(&self, room_id: i64) {
        if let Ok(mut checks) = self.checks.lock() {
            let check = checks.entry(room_id).or_insert(CheckState {
                next_check: Instant::now(),
                failures: 0,
                recording_failures: 0,
                recording_started: None,
            });
            check.failures = 0;
            check.recording_started = Some(Instant::now());
        }
    }

    pub fn mark_recording_ended(&self, room_id: i64) {
        self.recording_ended_at(room_id, Instant::now());
    }

    fn recording_ended_at(&self, room_id: i64, now: Instant) {
        if let Ok(mut checks) = self.checks.lock() {
            if let Some(check) = checks.get_mut(&room_id) {
                if check
                    .recording_started
                    .take()
                    .is_some_and(|start| now.duration_since(start) >= Duration::from_secs(60))
                {
                    check.recording_failures = 0;
                }
            }
        }
    }

    pub fn mark_recording_failure(&self, room_id: i64, interval_secs: u64) -> u64 {
        if let Ok(mut checks) = self.checks.lock() {
            let check = checks.entry(room_id).or_insert(CheckState {
                next_check: Instant::now(),
                failures: 0,
                recording_failures: 0,
                recording_started: None,
            });
            check.recording_failures = check.recording_failures.saturating_add(1);
            let delay = failure_delay_secs(interval_secs, check.recording_failures, false);
            check.next_check = Instant::now() + Duration::from_secs(delay);
            return delay;
        }
        failure_delay_secs(interval_secs, 1, false)
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
    use super::{failure_delay_secs, success_delay_secs, AutoRecorder};
    use std::time::Duration;

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

    #[test]
    fn consecutive_short_recordings_keep_backoff_across_successful_starts() {
        let recorder = AutoRecorder::new();
        for delay in [120, 240, 480, 900, 900] {
            recorder.mark_recording_started(1);
            let started = recorder.checks.lock().unwrap()[&1]
                .recording_started
                .unwrap();
            recorder.recording_ended_at(1, started + Duration::from_secs(5));
            assert_eq!(recorder.mark_recording_failure(1, 60), delay);
            assert!(!recorder.is_due(1));
        }
        recorder.mark_recording_started(1);
        let started = recorder.checks.lock().unwrap()[&1]
            .recording_started
            .unwrap();
        recorder.recording_ended_at(1, started + Duration::from_secs(60));
        assert_eq!(recorder.mark_recording_failure(1, 60), 120);
        recorder.mark_success(1, 60); // Confirmed offline resets the session's failures.
        assert_eq!(recorder.mark_recording_failure(1, 60), 120);
    }

    #[test]
    fn parser_errors_and_recording_errors_do_not_reset_each_other() {
        let recorder = AutoRecorder::new();
        assert_eq!(recorder.mark_recording_failure(1, 60), 120);
        assert_eq!(recorder.mark_failure(1, 60, true), 1800);
        recorder.mark_recording_started(1);
        recorder.mark_recording_ended(1);
        assert_eq!(recorder.mark_recording_failure(1, 60), 240);
        recorder.clear(1);
        recorder.mark_immediate(1);
        assert!(recorder.is_due(1));
        assert_eq!(recorder.mark_recording_failure(1, 60), 120);
    }
}
