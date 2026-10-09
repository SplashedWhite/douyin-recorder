//! Per-room recovery ownership. Call admission/cancellation under AppState::start_lock.
//! Monotonic deadlines govern work; wall-clock timestamps are only for the UI.
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    User,
    Stop,
    Settings,
    Monitor,
    Shutdown,
    Superseded,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProbeDecision {
    Offline,
    Start,
    Retry,
    Stop,
}

pub fn probe_decision(
    result: &Result<crate::parser::LiveInfo, crate::parser::ParseError>,
) -> ProbeDecision {
    match result {
        Ok(info) if !info.is_live => ProbeDecision::Offline,
        Ok(info) if !info.stream_url.is_empty() => ProbeDecision::Start,
        Err(error) if error.is_authentication_failed() || error.is_rate_limited() => {
            ProbeDecision::Stop
        }
        _ => ProbeDecision::Retry,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RecoveryStatus {
    pub recovery_id: u64,
    pub room_id: i64,
    pub from_task_id: i64,
    pub task_id: i64,
    pub trigger: String,
    pub phase: String,
    pub attempts: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub deadline: DateTime<Utc>,
    pub last_error: Option<String>,
    pub revision: u64,
    pub media_received: bool,
}

#[derive(Clone)]
pub struct Ticket {
    pub id: u64,
    pub room_id: i64,
    pub task_id: i64,
    pub deadline: Instant,
    pub cancel: watch::Receiver<Option<CancelReason>>,
}

impl Ticket {
    pub fn usable(&self) -> bool {
        self.cancel.borrow().is_none() && Instant::now() < self.deadline
    }
    pub async fn interrupted(&mut self) {
        if self.cancel.borrow().is_some() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(self.deadline) => {},
            _ = self.cancel.changed() => {},
        }
    }
}

struct Entry {
    status: RecoveryStatus,
    deadline: Instant,
    cancel: watch::Sender<Option<CancelReason>>,
    active: bool,
    tasks: HashSet<i64>,
    waits: u32,
    media_received: bool,
}

#[derive(Default)]
struct Inner {
    sequence: u64,
    rooms: HashMap<i64, Entry>,
    // A stop racing a recovered process must not run its normal post-record policy.
    suppressed: HashMap<i64, CancelReason>,
}

#[derive(Default)]
pub struct Recoveries(Mutex<Inner>);

impl Recoveries {
    pub fn snapshot(&self) -> Vec<RecoveryStatus> {
        self.0
            .lock()
            .unwrap()
            .rooms
            .values()
            .map(|e| e.status.clone())
            .collect()
    }
    pub fn busy(&self, room: i64) -> bool {
        self.0
            .lock()
            .unwrap()
            .rooms
            .get(&room)
            .is_some_and(|e| e.active)
    }
    pub fn any_busy(&self) -> bool {
        self.0.lock().unwrap().rooms.values().any(|e| e.active)
    }
    pub fn suppressed(&self, task: i64) -> Option<CancelReason> {
        self.0.lock().unwrap().suppressed.remove(&task)
    }

    #[cfg(test)]
    pub fn begin(&self, room: i64, task: i64, trigger: &str, timeout: u64) -> Option<Ticket> {
        self.begin_at(room, task, trigger, timeout, Instant::now())
    }

    pub fn begin_at(
        &self,
        room: i64,
        task: i64,
        trigger: &str,
        timeout: u64,
        exited_at: Instant,
    ) -> Option<Ticket> {
        let mut inner = self.0.lock().unwrap();
        if inner.suppressed.contains_key(&task) {
            return None;
        }
        if let Some(entry) = inner.rooms.get_mut(&room).filter(|e| e.active) {
            if entry.status.task_id != task || entry.status.phase != "recording" {
                return None;
            }
            entry.status.phase = "confirming".into();
            entry.status.next_attempt_at = None;
            entry.status.revision += 1;
            return Some(Ticket {
                id: entry.status.recovery_id,
                room_id: room,
                task_id: task,
                deadline: entry.deadline,
                cancel: entry.cancel.subscribe(),
            });
        }
        inner.sequence += 1;
        let id = inner.sequence;
        let (cancel, receiver) = watch::channel(None);
        let deadline = exited_at + Duration::from_secs(timeout);
        let status = RecoveryStatus {
            recovery_id: id,
            room_id: room,
            from_task_id: task,
            task_id: task,
            trigger: trigger.into(),
            phase: "confirming".into(),
            attempts: 0,
            next_attempt_at: None,
            deadline: Utc::now() + chrono::Duration::seconds(timeout as i64)
                - chrono::Duration::from_std(Instant::now().saturating_duration_since(exited_at))
                    .unwrap_or_default(),
            last_error: None,
            revision: 1,
            media_received: false,
        };
        inner.rooms.insert(
            room,
            Entry {
                status,
                deadline,
                cancel,
                active: true,
                tasks: HashSet::from([task]),
                waits: 0,
                media_received: false,
            },
        );
        Some(Ticket {
            id,
            room_id: room,
            task_id: task,
            deadline,
            cancel: receiver,
        })
    }

    pub fn owns(&self, ticket: &Ticket) -> bool {
        self.0
            .lock()
            .unwrap()
            .rooms
            .get(&ticket.room_id)
            .is_some_and(|e| {
                e.active && e.status.recovery_id == ticket.id && e.cancel.borrow().is_none()
            })
    }
    pub fn status(&self, room: i64) -> Option<RecoveryStatus> {
        self.0
            .lock()
            .unwrap()
            .rooms
            .get(&room)
            .map(|e| e.status.clone())
    }
    fn update(&self, ticket: &Ticket, action: impl FnOnce(&mut Entry)) -> Option<RecoveryStatus> {
        let mut inner = self.0.lock().unwrap();
        let entry = inner.rooms.get_mut(&ticket.room_id)?;
        if !entry.active || entry.status.recovery_id != ticket.id || entry.cancel.borrow().is_some()
        {
            return None;
        }
        action(entry);
        entry.status.revision += 1;
        Some(entry.status.clone())
    }
    pub fn attempt(&self, ticket: &Ticket) -> Option<RecoveryStatus> {
        self.update(ticket, |e| {
            e.status.attempts += 1;
            e.status.phase = "confirming".into();
            e.status.next_attempt_at = None;
        })
    }
    pub fn waiting(&self, ticket: &Ticket, error: String) -> Option<(RecoveryStatus, Duration)> {
        let mut delay = Duration::ZERO;
        let status = self.update(ticket, |e| {
            delay = retry_delay(e.waits);
            e.waits = e.waits.saturating_add(1);
            e.status.phase = "waiting".into();
            e.status.last_error = Some(error);
            e.status.next_attempt_at = Some(
                (Utc::now() + chrono::Duration::seconds(delay.as_secs() as i64))
                    .min(e.status.deadline),
            );
        })?;
        Some((status, delay))
    }
    pub fn starting(&self, ticket: &Ticket) -> Option<RecoveryStatus> {
        self.update(ticket, |e| {
            e.status.phase = "starting".into();
            e.status.next_attempt_at = None;
        })
    }
    pub fn started(&self, ticket: &Ticket, task: i64) -> Option<RecoveryStatus> {
        self.update(ticket, |e| {
            e.status.task_id = task;
            e.tasks.insert(task);
            e.status.phase = "recording".into();
            e.media_received = false;
            e.status.media_received = false;
        })
    }
    pub fn media(&self, room: i64, task: i64, stable: bool) -> Option<RecoveryStatus> {
        let mut inner = self.0.lock().unwrap();
        let e = inner.rooms.get_mut(&room)?;
        if !e.active
            || e.status.task_id != task
            || e.status.phase != "recording"
            || *e.cancel.borrow() != None
        {
            return None;
        }
        if e.media_received && !stable {
            return None;
        }
        e.media_received = true;
        e.status.media_received = true;
        e.status.last_error = None;
        if stable {
            e.active = false;
            e.status.phase = "stable".into();
        }
        e.status.revision += 1;
        Some(e.status.clone())
    }
    pub fn end(
        &self,
        ticket: &Ticket,
        phase: &str,
        error: Option<String>,
    ) -> Option<RecoveryStatus> {
        let mut inner = self.0.lock().unwrap();
        let e = inner.rooms.get_mut(&ticket.room_id)?;
        if !e.active || e.status.recovery_id != ticket.id {
            return None;
        }
        e.active = false;
        e.status.phase = phase.into();
        e.status.last_error = error;
        e.status.next_attempt_at = None;
        e.status.revision += 1;
        Some(e.status.clone())
    }
    pub fn cancel(
        &self,
        room: i64,
        id: Option<u64>,
        reason: CancelReason,
        pending_only: bool,
    ) -> Option<RecoveryStatus> {
        let mut inner = self.0.lock().unwrap();
        let e = inner.rooms.get_mut(&room)?;
        if !e.active
            || id.is_some_and(|id| id != e.status.recovery_id)
            || (pending_only && e.status.phase == "recording")
            || (e.cancel.borrow().is_some()
                && !matches!(reason, CancelReason::Stop | CancelReason::Shutdown))
        {
            return None;
        }
        let recording = e.status.phase == "recording";
        e.cancel.send_replace(Some(reason));
        e.status.phase = "cancelled".into();
        e.status.next_attempt_at = None;
        e.status.last_error = Some("恢复已取消；保留的监控会按常规检测继续运行".into());
        e.status.revision += 1;
        // A pending worker acknowledges cancellation after saving its old file.
        // A running replacement instead acknowledges through its exit callback.
        if recording {
            e.active = false;
        }
        let status = e.status.clone();
        if recording {
            inner.suppressed.insert(status.task_id, reason);
        }
        Some(status)
    }
    pub fn includes_task(&self, room: i64, task: i64) -> bool {
        self.0
            .lock()
            .unwrap()
            .rooms
            .get(&room)
            .is_some_and(|e| e.active && e.tasks.contains(&task))
    }

    pub fn detach_recording(&self, room: i64, task: i64) -> Option<RecoveryStatus> {
        let mut inner = self.0.lock().unwrap();
        let entry = inner.rooms.get_mut(&room)?;
        if !entry.active || entry.status.phase != "recording" || entry.status.task_id != task {
            return None;
        }
        entry.active = false;
        entry.status.phase = "cancelled".into();
        entry.status.last_error = Some("软件断流恢复已关闭，按原录制结束规则收尾".into());
        entry.status.revision += 1;
        Some(entry.status.clone())
    }
}

pub fn retry_delay(wait: u32) -> Duration {
    Duration::from_secs(match wait {
        0 => 3,
        1 => 5,
        2 => 10,
        _ => 20,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn budget_includes_waits_and_restarted_processes_but_does_not_stop_them() {
        let r = Recoveries::default();
        let mut ticket = r.begin(1, 1, "manual", 120).unwrap();
        let start = Instant::now();
        for delay in [3, 5, 10, 20, 20, 20, 20, 20] {
            let (status, wait) = r.waiting(&ticket, "temporary".into()).unwrap();
            assert_eq!(wait.as_secs(), delay);
            assert!(status.next_attempt_at.unwrap() <= status.deadline);
            tokio::select! { biased;
                _ = ticket.interrupted() => {},
                _ = tokio::time::sleep(wait) => {},
            }
        }
        assert_eq!(Instant::now() - start, Duration::from_secs(118));
        r.started(&ticket, 2);
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(!ticket.usable());
        assert!(ticket.cancel.borrow().is_none()); // Deadline alone never sends a stop.
        assert_eq!(r.status(1).unwrap().phase, "recording");
        let again = r.begin(1, 2, "manual", 120).unwrap();
        assert_eq!(again.deadline, ticket.deadline);
        assert!(!again.usable());
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_wakes_wait_and_late_work_cannot_revive_or_overwrite_a_new_episode() {
        let r = Recoveries::default();
        let mut ticket = r.begin(1, 1, "auto", 120).unwrap();
        r.waiting(&ticket, "network".into());
        r.cancel(1, Some(ticket.id), CancelReason::User, false)
            .unwrap();
        let before = Instant::now();
        ticket.interrupted().await;
        assert_eq!(Instant::now(), before);
        assert!(r.starting(&ticket).is_none());
        assert!(r.started(&ticket, 2).is_none());
        assert!(r.busy(1)); // Saving the original output still reserves the room.
        r.end(&ticket, "cancelled", None).unwrap();
        assert!(!r.busy(1));
        let next = r.begin(1, 3, "manual", 120).unwrap();
        assert!(r.end(&ticket, "failed", None).is_none());
        assert!(r.owns(&next));
        assert!(Recoveries::default().snapshot().is_empty());
    }
    #[test]
    fn flapping_preserves_deadline_and_attempts_until_stable_media() {
        let r = Recoveries::default();
        let first = r.begin(1, 10, "manual", 120).unwrap();
        r.attempt(&first);
        r.started(&first, 11);
        assert!(r.begin(1, 10, "manual", 120).is_none());
        let second = r.begin(1, 11, "manual", 900).unwrap();
        assert_eq!(first.deadline, second.deadline);
        assert_eq!(first.id, second.id);
        assert_eq!(r.attempt(&second).unwrap().attempts, 2);
        r.started(&second, 12);
        r.media(1, 12, false);
        assert!(r.busy(1));
        r.media(1, 12, true);
        assert!(!r.busy(1));
        let third = r.begin(1, 12, "manual", 120).unwrap();
        assert_ne!(third.id, first.id);
        assert!(!r.owns(&first));
        assert!(r.end(&first, "ended", None).is_none());
    }
    #[test]
    fn cancel_is_generation_scoped_and_preserves_other_rooms() {
        let r = Recoveries::default();
        let a = r.begin(1, 10, "auto", 120).unwrap();
        let b = r.begin(2, 20, "manual", 120).unwrap();
        assert!(r
            .cancel(1, Some(a.id + 1), CancelReason::User, false)
            .is_none());
        r.started(&a, 11);
        assert!(r
            .cancel(1, Some(a.id), CancelReason::Settings, true)
            .is_none());
        r.cancel(1, Some(a.id), CancelReason::User, false).unwrap();
        assert!(!a.usable());
        assert!(!r.owns(&a));
        assert!(r.owns(&b));
        assert_eq!(r.suppressed(11), Some(CancelReason::User));
    }
    #[test]
    fn retry_sequence_caps_without_resetting_on_start() {
        let r = Recoveries::default();
        let a = r.begin(1, 1, "manual", 120).unwrap();
        for (index, expected) in [3, 5, 10, 20, 20, 20].into_iter().enumerate() {
            assert_eq!(
                r.waiting(&a, "network".into()).unwrap().1.as_secs(),
                expected
            );
            r.started(&a, index as i64 + 2);
            r.begin(1, index as i64 + 2, "manual", 120).unwrap();
        }
    }

    #[test]
    fn disabling_while_recording_releases_chain_without_suppressing_normal_verification() {
        let r = Recoveries::default();
        let ticket = r.begin(1, 10, "manual", 120).unwrap();
        r.started(&ticket, 11).unwrap();
        assert!(r
            .cancel(1, Some(ticket.id), CancelReason::Settings, true)
            .is_none());
        assert!(r.detach_recording(1, 10).is_none());
        r.detach_recording(1, 11).unwrap();
        assert!(!r.busy(1));
        assert_eq!(r.suppressed(11), None);
    }
}
