use crate::{
    recovery::{CancelReason, RecoveryStatus, Ticket},
    *,
};
use std::{future::Future, pin::Pin};

pub fn publish(app: &AppHandle, status: RecoveryStatus, event: &str) {
    recording_log::event(
        "INFO",
        &format!("recovery_{event}"),
        Some(status.task_id),
        Some(status.room_id),
        json!({"recovery_id": status.recovery_id, "from_task_id": status.from_task_id,
            "phase": status.phase, "attempts": status.attempts, "deadline": status.deadline,
            "next_attempt_at": status.next_attempt_at, "error": status.last_error}),
    );
    let _ = app.emit("recording-recovery-changed", status);
}

// Boxing breaks the start -> exit -> recover -> start future type cycle.
pub fn recording_exit(
    app: AppHandle,
    task_id: i64,
    room_id: i64,
    mut exit: RecordingExit,
    monitor_stop: tokio::sync::watch::Sender<bool>,
    monitor: Option<tokio::task::JoinHandle<Result<(), String>>>,
    work: Arc<lifecycle::Operation>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
    Box::pin(async move {
        let state = app.state::<AppState>();
        let (ticket, suppressed) = {
            let _start = state.start_lock.lock().await;
            if state.recorder.stop_requested(task_id) {
                exit.manually_stopped = true;
            }
            let progress = state.recorder.progress(task_id);
            if let Some(progress) = progress.as_ref().filter(|p| p.has_media()) {
                if let Some(status) = state.recoveries.media(room_id, task_id, progress.stable()) {
                    publish(
                        &app,
                        status,
                        if progress.stable() {
                            "stable"
                        } else {
                            "media_received"
                        },
                    );
                }
            }
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let task = db.get_task(task_id).map_err(|e| e.to_string())?;
            let settings = state.settings();
            if !settings.recording_recovery_enabled && state.recoveries.busy(room_id) {
                if let Some(status) = state.recoveries.detach_recording(room_id, task_id) {
                    publish(&app, status, "ended");
                }
            }
            let ticket = if !exit.manually_stopped
                && !state.lifecycle.is_exiting()
                && settings.recording_recovery_enabled
            {
                state.recoveries.begin_at(
                    room_id,
                    task_id,
                    &task.trigger,
                    settings.recording_recovery_timeout_secs,
                    progress
                        .and_then(|p| p.exited_at())
                        .unwrap_or_else(tokio::time::Instant::now),
                )
            } else {
                None
            };
            let suppressed = state.recoveries.suppressed(task_id);
            db.mark_task_finalizing(task_id, file_size(task.file_path.as_deref()))
                .map_err(|e| e.to_string())?;
            emit_recording_event(
                &app,
                db.get_task(task_id).map_err(|e| e.to_string())?,
                None,
                "finalizing",
                None,
            );
            if ticket.is_some() {
                publish(&app, state.recoveries.status(room_id).unwrap(), "started");
            }
            (ticket, suppressed)
        };
        let _ = monitor_stop.send(true);
        let monitor_result = if let Some(monitor) = monitor {
            monitor
                .await
                .map_err(|e| e.to_string())
                .and_then(|result| result)
        } else {
            Ok(())
        };
        let result = if let Some(ticket) = ticket {
            let cleanup_ticket = ticket.clone();
            let result = recover(&app, task_id, exit, ticket, work).await;
            if let Err(error) = &result {
                let _start = state.start_lock.lock().await;
                if let Some(status) =
                    state
                        .recoveries
                        .end(&cleanup_ticket, "failed", Some(error.clone()))
                {
                    publish(&app, status, "failed");
                }
                state.lifecycle.record_failure(error.clone());
            }
            result
        } else {
            let skip = suppressed.is_some_and(|reason| reason != CancelReason::Stop);
            // Cancellation of a replacement must not issue another status request.
            let verified = suppressed.map(|_| {
                (
                    LiveVerification::Failed,
                    false,
                    Some("断流恢复已停止".into()),
                )
            });
            finalize_recording_exit(
                app.clone(),
                task_id,
                room_id,
                exit,
                verified,
                !skip,
                Some(work),
            )
            .await
        };
        result?;
        monitor_result
    })
}

async fn recover(
    app: &AppHandle,
    task_id: i64,
    exit: RecordingExit,
    mut ticket: Ticket,
    work: Arc<lifecycle::Operation>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (url, trigger) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let room = db.get_room(ticket.room_id).map_err(|e| e.to_string())?;
        (
            format!("https://live.douyin.com/{}", room.room_id),
            db.get_task(task_id).map_err(|e| e.to_string())?.trigger,
        )
    };
    let mut finalized = false;
    let mut verification = LiveVerification::Failed;
    let mut limited = false;
    let mut error = Some("已到最长恢复时间".to_string());
    let mut first = true;
    let mut phase = "exhausted";
    while ticket.usable() && state.recoveries.owns(&ticket) {
        if let Some(status) = state.recoveries.attempt(&ticket) {
            publish(app, status, "attempt");
        }
        let source = if first {
            RequestSource::RecordingVerification
        } else {
            RequestSource::RecordingRecovery
        };
        first = false;
        let result = state
            .parser
            .parse_for_recovery(&url, &state.settings(), source, &mut ticket)
            .await;
        let decision = recovery::probe_decision(&result);
        limited |= result.as_ref().err().is_some_and(|e| e.is_rate_limited());
        // Late responses cannot update the room, finalize policy or start a process.
        {
            let _start = state.start_lock.lock().await;
            if !ticket.usable() || !state.recoveries.owns(&ticket) {
                break;
            }
            if let Ok(info) = &result {
                apply_live_info(&state, ticket.room_id, info)?;
            }
        }
        match result {
            Ok(info) => {
                verification = if info.is_live {
                    LiveVerification::Live
                } else {
                    LiveVerification::Offline
                };
                if let Some(logger) = recording_log::logger() {
                    logger.live_verification(
                        task_id,
                        ticket.room_id,
                        Some(verification),
                        false,
                        None,
                    );
                }
                if decision == recovery::ProbeDecision::Offline {
                    error = None;
                    phase = "offline";
                    break;
                }
                if !finalized {
                    finalize_recording_exit(
                        app.clone(),
                        task_id,
                        ticket.room_id,
                        exit.clone(),
                        Some((
                            verification,
                            false,
                            Some("直播仍在进行，正在断流恢复".into()),
                        )),
                        false,
                        Some(work.clone()),
                    )
                    .await?;
                    finalized = true;
                }
                if decision == recovery::ProbeDecision::Start {
                    if let Some(status) = state.recoveries.starting(&ticket) {
                        publish(app, status, "starting");
                    }
                    match start_record_from_info_inner(
                        app,
                        &state,
                        ticket.room_id,
                        &info,
                        &trigger,
                        None,
                        Some((&ticket, task_id)),
                    )
                    .await
                    {
                        Ok(_) => return Ok(()),
                        Err(StartRecordError::Superseded | StartRecordError::AlreadyRunning) => {
                            break;
                        }
                        Err(StartRecordError::Fatal(message)) => {
                            error = Some(message);
                            phase = "failed";
                            break;
                        }
                        Err(StartRecordError::Transient(message)) => {
                            error = Some(message);
                        }
                    }
                } else {
                    error = Some("直播中，但接口没有返回可用地址".into());
                }
            }
            Err(failure) => {
                verification = LiveVerification::Failed;
                limited = failure.is_rate_limited();
                error = Some(failure.to_string());
                if let Some(logger) = recording_log::logger() {
                    logger.live_verification(
                        task_id,
                        ticket.room_id,
                        Some(verification),
                        limited,
                        error.clone(),
                    );
                }
                if decision == recovery::ProbeDecision::Stop {
                    phase = "failed";
                    break;
                }
            }
        }
        if !ticket.usable() || !state.recoveries.owns(&ticket) {
            break;
        }
        if let Some((status, delay)) = state
            .recoveries
            .waiting(&ticket, error.clone().unwrap_or_default())
        {
            publish(app, status, "waiting");
            tokio::select! {
                biased;
                _ = ticket.interrupted() => {},
                _ = tokio::time::sleep(delay) => {},
            }
        }
    }
    if ticket.cancel.borrow().is_some() {
        phase = "cancelled";
        error = Some("断流恢复已取消".into());
    } else if tokio::time::Instant::now() >= ticket.deadline {
        phase = "exhausted";
        error = Some("已到最长恢复时间".into());
    }
    if !finalized {
        finalize_recording_exit(
            app.clone(),
            task_id,
            ticket.room_id,
            exit.clone(),
            Some((verification, limited, error.clone())),
            false,
            Some(work),
        )
        .await?;
    }
    let _start = state.start_lock.lock().await;
    let cancel = *ticket.cancel.borrow();
    if cancel.is_some() {
        phase = "cancelled";
        error = Some("断流恢复已取消".into());
    }
    // End policy is committed once, while the room is still reserved by this episode.
    if state
        .recoveries
        .status(ticket.room_id)
        .is_some_and(|s| s.recovery_id == ticket.id)
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task = db.get_task(task_id).map_err(|e| e.to_string())?;
        let mut room = db.get_room(ticket.room_id).map_err(|e| e.to_string())?;
        if !state.lifecycle.is_exiting() && (cancel.is_none() || cancel == Some(CancelReason::Stop))
        {
            let mut policy_exit = exit;
            if cancel == Some(CancelReason::Stop) {
                policy_exit.manually_stopped = true;
            }
            let (reason, message) = apply_post_recording_auto_policy(
                &state,
                &mut room,
                &trigger,
                &task.status,
                &policy_exit,
                Some(verification),
                limited,
            );
            let room = db.save_room_automation(&room).map_err(|e| e.to_string())?;
            emit_auto_recording_event(app, room, reason, message);
        }
        if let Some(status) = state.recoveries.end(&ticket, phase, error) {
            publish(app, status, "ended");
        }
    }
    Ok(())
}

pub fn watch_media(app: AppHandle, room: i64, task: i64) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let state = app.state::<AppState>();
            let _start = state.start_lock.lock().await;
            if state.lifecycle.is_exiting() || !state.recoveries.busy(room) {
                break;
            }
            let Some(progress) = state.recorder.progress(task) else {
                break;
            };
            if !progress.running() {
                break;
            }
            if progress.has_media() {
                if let Some(status) = state.recoveries.media(room, task, progress.stable()) {
                    let event = if status.phase == "stable" {
                        "stable"
                    } else {
                        "media_received"
                    };
                    publish(&app, status, event);
                }
            }
        }
    });
}

// Caller holds start_lock. Sending a stop is synchronous; waiting for output close
// happens outside the lock through the recorder's existing completion mechanism.
pub fn cancel(
    app: &AppHandle,
    room: i64,
    id: Option<u64>,
    reason: CancelReason,
    pending_only: bool,
) -> Option<RecoveryStatus> {
    let state = app.state::<AppState>();
    let previous = state.recoveries.status(room)?;
    let status = state.recoveries.cancel(room, id, reason, pending_only)?;
    if matches!(reason, CancelReason::User | CancelReason::Stop) && previous.phase == "recording" {
        state.recorder.request_stop(previous.task_id);
    }
    if reason != CancelReason::Shutdown {
        // Keep persisted monitor settings and any existing rate-limit cooldown.
        state
            .auto_recorder
            .mark_success(room, state.settings().auto_check_interval_secs);
    }
    publish(app, status.clone(), "cancelled");
    Some(status)
}

pub fn cancel_all(app: &AppHandle, reason: CancelReason, pending_only: bool) {
    for status in app.state::<AppState>().recoveries.snapshot() {
        cancel(
            app,
            status.room_id,
            Some(status.recovery_id),
            reason,
            pending_only,
        );
    }
}
