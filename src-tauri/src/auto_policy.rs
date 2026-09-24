//! Decisions shared by commands, the scheduler and recording completion.
//! Time is supplied by callers so day boundaries can be tested without waiting.
use chrono::{DateTime, Duration, Local, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use crate::database::LiveRoom;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoMonitorMode {
    #[default]
    Window,
    Continuous,
}

impl AutoMonitorMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Continuous => "continuous",
        }
    }
}

pub fn monitor_until(mode: AutoMonitorMode, hours: u64, now: DateTime<Utc>) -> Option<String> {
    (mode == AutoMonitorMode::Window).then(|| (now + Duration::hours(hours as i64)).to_rfc3339())
}

pub fn monitor_is_expired(room: &LiveRoom, now: DateTime<Utc>) -> bool {
    room.auto_monitor_mode == AutoMonitorMode::Window
        && room
            .auto_record_until
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .is_none_or(|until| until <= now)
}

pub fn accepts_check(room: &LiveRoom, revision: i64, running: bool, now: DateTime<Utc>) -> bool {
    room.auto_record_enabled
        && room.auto_record_revision == revision
        && !running
        && !monitor_is_expired(room, now)
}

pub fn disable_for_manual_stop(room: &mut LiveRoom, task_status: &str, now: DateTime<Utc>) -> bool {
    if matches!(task_status, "recording" | "finalizing")
        && room.auto_monitor_mode == AutoMonitorMode::Continuous
        && room.auto_record_enabled
    {
        set_enabled(room, false, 0, now);
        true
    } else {
        false
    }
}

pub fn daily_schedule_is_due(room: &LiveRoom, now: DateTime<Local>) -> bool {
    if room.auto_monitor_mode == AutoMonitorMode::Continuous {
        return false;
    }
    let Some(time) = room
        .auto_record_daily_time
        .as_deref()
        .and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok())
    else {
        return false;
    };
    room.last_schedule_trigger_date.as_deref() != Some(&now.format("%Y-%m-%d").to_string())
        && now.time() >= time
}

pub fn initial_schedule_marker(time: NaiveTime, now: DateTime<Local>) -> Option<String> {
    (time <= now.time()).then(|| now.format("%Y-%m-%d").to_string())
}

pub fn set_enabled(room: &mut LiveRoom, enabled: bool, hours: u64, now: DateTime<Utc>) {
    room.auto_record_enabled = enabled;
    room.auto_record_until = if enabled {
        monitor_until(room.auto_monitor_mode, hours, now)
    } else {
        None
    };
    room.auto_record_error = None;
    room.auto_record_retry_at = None;
}

pub fn configure(
    room: &mut LiveRoom,
    mode: AutoMonitorMode,
    daily_time: Option<String>,
    hours: u64,
    now: DateTime<Local>,
) -> Result<(), String> {
    let parsed = daily_time
        .as_deref()
        .map(|s| {
            NaiveTime::parse_from_str(s, "%H:%M")
                .map_err(|_| "定时时间格式无效，请使用 HH:mm".to_string())
        })
        .transpose()?;
    let mode_changed = room.auto_monitor_mode != mode;
    if room.auto_record_daily_time != daily_time || mode_changed {
        room.last_schedule_trigger_date =
            parsed.and_then(|time| initial_schedule_marker(time, now));
    }
    room.auto_record_daily_time = daily_time;
    room.auto_monitor_mode = mode;
    if mode_changed {
        room.auto_record_until = if room.auto_record_enabled {
            monitor_until(mode, hours, now.with_timezone(&Utc))
        } else {
            None
        };
    }
    // Saving preferences must not enable a room or cancel an existing cooldown/error.
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum TickAction {
    None,
    Scheduled,
    Expired,
}

pub fn tick(room: &mut LiveRoom, running: bool, hours: u64, now: DateTime<Local>) -> TickAction {
    // Renew first when both deadlines coincide, without an intermediate disabled state.
    if daily_schedule_is_due(room, now) {
        if !room.auto_record_enabled {
            set_enabled(room, true, hours, now.with_timezone(&Utc));
        }
        room.auto_record_until =
            monitor_until(room.auto_monitor_mode, hours, now.with_timezone(&Utc));
        room.last_schedule_trigger_date = Some(now.format("%Y-%m-%d").to_string());
        return TickAction::Scheduled;
    }
    if room.auto_record_enabled && !running && monitor_is_expired(room, now.with_timezone(&Utc)) {
        set_enabled(room, false, hours, now.with_timezone(&Utc));
        return TickAction::Expired;
    }
    TickAction::None
}

pub fn reconcile(room: &mut LiveRoom, hours: u64, now: DateTime<Local>) {
    if room.auto_record_enabled {
        if room.auto_monitor_mode == AutoMonitorMode::Continuous {
            room.auto_record_until = None;
        } else if room.auto_record_until.is_none() {
            room.auto_record_until =
                monitor_until(room.auto_monitor_mode, hours, now.with_timezone(&Utc));
        }
    }
    tick(room, false, hours, now);
}

#[derive(Debug, PartialEq, Eq)]
pub enum PostRecordAction {
    Preserve,
    Disable,
    RenewWindow,
    Continue,
    Retry,
}

pub fn post_record_action(
    room: &LiveRoom,
    trigger: &str,
    status: &str,
    manually_stopped: bool,
    confirmed_offline: bool,
    disable_after_record: bool,
) -> PostRecordAction {
    if !room.auto_record_enabled {
        return PostRecordAction::Preserve;
    }
    if room.auto_monitor_mode == AutoMonitorMode::Continuous {
        // The stop command already disabled monitoring. A later explicit enable wins.
        if manually_stopped {
            return PostRecordAction::Continue;
        }
        return if status == "completed" || confirmed_offline {
            PostRecordAction::Continue
        } else {
            PostRecordAction::Retry
        };
    }
    if trigger != "auto" {
        PostRecordAction::Preserve
    } else if status == "completed" && !disable_after_record {
        PostRecordAction::RenewWindow
    } else {
        PostRecordAction::Disable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    fn enabled(mode: AutoMonitorMode) -> LiveRoom {
        let mut room = LiveRoom {
            auto_monitor_mode: mode,
            ..Default::default()
        };
        set_enabled(&mut room, true, 24, now().with_timezone(&Utc));
        room
    }

    #[test]
    fn continuous_waits_for_days_and_never_triggers_saved_schedule() {
        let mut room = enabled(AutoMonitorMode::Continuous);
        room.auto_record_daily_time = Some("00:00".into());
        for days in [0, 1, 2, 100] {
            let later = now() + Duration::days(days);
            assert!(!monitor_is_expired(&room, later.with_timezone(&Utc)));
            assert!(!daily_schedule_is_due(&room, later));
            assert_eq!(tick(&mut room, false, 24, later), TickAction::None);
            assert!(room.auto_record_enabled);
            assert!(room.auto_record_until.is_none());
        }
    }

    #[test]
    fn midnight_renews_expired_or_overlapping_window_once_even_while_recording() {
        let midnight = Local.with_ymd_and_hms(2026, 9, 25, 0, 0, 0).unwrap();
        for offset in [-1, 0, 10] {
            for running in [false, true] {
                let mut room = enabled(AutoMonitorMode::Window);
                room.auto_record_daily_time = Some("00:00".into());
                room.auto_record_until = Some((midnight + Duration::seconds(offset)).to_rfc3339());
                assert_eq!(
                    tick(&mut room, running, 24, midnight),
                    TickAction::Scheduled
                );
                assert!(room.auto_record_enabled);
                assert_eq!(
                    room.auto_record_until,
                    monitor_until(AutoMonitorMode::Window, 24, midnight.with_timezone(&Utc))
                );
                assert_eq!(tick(&mut room, running, 24, midnight), TickAction::None);
            }
        }
    }

    #[test]
    fn expired_window_stops_waiting_but_does_not_interrupt_recording() {
        let later = now() + Duration::hours(25);
        let mut room = enabled(AutoMonitorMode::Window);
        assert_eq!(tick(&mut room, true, 24, later), TickAction::None);
        assert!(room.auto_record_enabled);
        assert_eq!(tick(&mut room, false, 24, later), TickAction::Expired);
        assert!(!room.auto_record_enabled);
    }

    #[test]
    fn config_preserves_switch_and_backoff_and_restores_schedule_from_next_day() {
        let mut room = enabled(AutoMonitorMode::Window);
        room.auto_record_daily_time = Some("00:00".into());
        room.auto_record_retry_at = Some((now() + Duration::minutes(30)).to_rfc3339());
        room.auto_record_error = Some("限流".into());
        let retry = room.auto_record_retry_at.clone();
        configure(
            &mut room,
            AutoMonitorMode::Continuous,
            Some("00:00".into()),
            6,
            now(),
        )
        .unwrap();
        assert!(room.auto_record_enabled);
        assert!(room.auto_record_until.is_none());
        assert_eq!(room.auto_record_retry_at, retry);
        assert_eq!(room.auto_record_error.as_deref(), Some("限流"));
        let later = now() + Duration::days(2);
        configure(
            &mut room,
            AutoMonitorMode::Window,
            Some("00:00".into()),
            6,
            later,
        )
        .unwrap();
        assert_eq!(
            room.auto_record_until,
            monitor_until(AutoMonitorMode::Window, 6, later.with_timezone(&Utc))
        );
        assert!(!daily_schedule_is_due(&room, later));
        assert!(daily_schedule_is_due(&room, later + Duration::days(1)));
        assert_eq!(room.auto_record_retry_at, retry);
        set_enabled(&mut room, false, 6, later.with_timezone(&Utc));
        configure(
            &mut room,
            AutoMonitorMode::Continuous,
            Some("00:00".into()),
            6,
            later,
        )
        .unwrap();
        assert!(!room.auto_record_enabled);
    }

    #[test]
    fn unchanged_config_does_not_extend_window_and_invalid_config_does_not_mutate() {
        let mut room = enabled(AutoMonitorMode::Window);
        let deadline = room.auto_record_until.clone();
        configure(&mut room, AutoMonitorMode::Window, None, 6, now()).unwrap();
        assert_eq!(room.auto_record_until, deadline);
        assert!(configure(
            &mut room,
            AutoMonitorMode::Continuous,
            Some("25:61".into()),
            6,
            now()
        )
        .is_err());
        assert_eq!(room.auto_monitor_mode, AutoMonitorMode::Window);
        assert_eq!(room.auto_record_until, deadline);
    }

    #[test]
    fn restart_preserves_continuous_intent_and_retry_deadline() {
        for on in [false, true] {
            let mut room = enabled(AutoMonitorMode::Continuous);
            room.auto_record_enabled = on;
            room.auto_record_daily_time = Some("00:00".into());
            room.auto_record_retry_at = Some((now() + Duration::minutes(30)).to_rfc3339());
            let retry = room.auto_record_retry_at.clone();
            reconcile(&mut room, 6, now());
            assert_eq!(room.auto_record_enabled, on);
            assert!(room.auto_record_until.is_none());
            assert_eq!(room.auto_record_retry_at, retry);
        }
        let mut legacy = LiveRoom {
            auto_record_enabled: true,
            ..Default::default()
        };
        reconcile(&mut legacy, 6, now());
        assert_eq!(legacy.auto_monitor_mode, AutoMonitorMode::Window);
        assert_eq!(
            legacy.auto_record_until,
            monitor_until(AutoMonitorMode::Window, 6, now().with_timezone(&Utc))
        );
    }

    #[test]
    fn manual_stop_disables_continuous_before_completion_and_never_stops_historical_tasks() {
        for status in ["recording", "finalizing"] {
            let mut room = enabled(AutoMonitorMode::Continuous);
            assert!(disable_for_manual_stop(
                &mut room,
                status,
                now().with_timezone(&Utc)
            ));
            assert_eq!(
                post_record_action(&room, "auto", "completed", true, false, false),
                PostRecordAction::Preserve
            );
            assert!(!room.auto_record_enabled);
        }
        let mut room = enabled(AutoMonitorMode::Continuous);
        assert!(!disable_for_manual_stop(
            &mut room,
            "completed",
            now().with_timezone(&Utc)
        ));
        assert!(room.auto_record_enabled);
        let mut room = enabled(AutoMonitorMode::Window);
        assert!(!disable_for_manual_stop(
            &mut room,
            "recording",
            now().with_timezone(&Utc)
        ));
    }

    #[test]
    fn post_record_policy_respects_current_mode_off_switch_and_failed_history() {
        for mode in [AutoMonitorMode::Window, AutoMonitorMode::Continuous] {
            let mut room = enabled(mode);
            set_enabled(&mut room, false, 6, now().with_timezone(&Utc));
            for status in ["completed", "interrupted", "failed"] {
                assert_eq!(
                    post_record_action(&room, "auto", status, false, false, false),
                    PostRecordAction::Preserve
                );
            }
        }
        let room = enabled(AutoMonitorMode::Continuous);
        for disable in [false, true] {
            assert_eq!(
                post_record_action(&room, "auto", "completed", false, true, disable),
                PostRecordAction::Continue
            );
            assert_eq!(
                post_record_action(&room, "auto", "failed", false, true, disable),
                PostRecordAction::Continue
            );
            assert_eq!(
                post_record_action(&room, "auto", "interrupted", false, false, disable),
                PostRecordAction::Retry
            );
            assert_eq!(
                post_record_action(&room, "manual", "failed", false, false, disable),
                PostRecordAction::Retry
            );
        }
        let room = enabled(AutoMonitorMode::Window);
        assert_eq!(
            post_record_action(&room, "auto", "completed", false, true, false),
            PostRecordAction::RenewWindow
        );
        assert_eq!(
            post_record_action(&room, "auto", "completed", false, true, true),
            PostRecordAction::Disable
        );
        assert_eq!(
            post_record_action(&room, "auto", "interrupted", false, false, false),
            PostRecordAction::Disable
        );
        assert_eq!(
            post_record_action(&room, "manual", "completed", false, true, true),
            PostRecordAction::Preserve
        );
    }
}
