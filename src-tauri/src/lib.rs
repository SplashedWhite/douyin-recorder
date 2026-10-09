// Desktop entry points are intentionally not launched by the headless tests.
#![cfg_attr(test, allow(dead_code))]

#[cfg(all(test, windows))]
#[link(name = "resource", kind = "static")]
unsafe extern "C" {}

mod auto_policy;
mod auto_recorder;
mod conversion;
mod database;
mod desktop;
mod lifecycle;
mod migration;
mod parser;
mod recorder;
mod recording_log;
mod recovery;
mod recovery_runtime;
mod segment_runtime;
mod segment_store;
#[cfg(all(test, windows))]
mod segment_tests;
mod segments;
mod settings;
mod updater;

#[cfg(test)]
use auto_policy::{daily_schedule_is_due, initial_schedule_marker};
use auto_policy::{monitor_is_expired, monitor_until};
use auto_policy::{AutoMonitorMode, PostRecordAction, TickAction};
use auto_recorder::AutoRecorder;
use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use database::{Database, LiveRoom, RecordTask};
use parser::{DouyinParser, LiveInfo, RequestSource};
use recorder::{Recorder, RecordingExit};
use serde::Serialize;
use serde_json::json;
use settings::AppSettings;
use std::sync::{atomic::Ordering, Arc, Mutex};
use std::time::Duration;
#[cfg(not(test))]
use tauri::AppHandle;
use tauri::{Emitter, Manager, State};
#[cfg(test)]
type AppHandle = tauri::AppHandle<tauri::test::MockRuntime>;
#[cfg(all(test, windows))]
mod recovery_tests;
use tokio::sync::Mutex as AsyncMutex;

fn resolve_ffmpeg_path() -> String {
    #[cfg(all(test, windows))]
    {
        return segment_tests::ffmpeg().to_string_lossy().into_owned();
    }
    #[cfg(not(all(test, windows)))]
    {
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let target = if cfg!(target_os = "windows") {
            format!("{}-pc-windows-msvc", std::env::consts::ARCH)
        } else if cfg!(target_os = "macos") {
            format!("{}-apple-darwin", std::env::consts::ARCH)
        } else {
            format!("{}-unknown-linux-gnu", std::env::consts::ARCH)
        };
        let sidecar_name = format!("ffmpeg-{}{}", target, ext);

        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let sidecar_path = exe_dir.join(&sidecar_name);
                if sidecar_path.exists() {
                    return sidecar_path.to_string_lossy().to_string();
                }
                let resource_sidecar = exe_dir.join("resources").join(&sidecar_name);
                if resource_sidecar.exists() {
                    return resource_sidecar.to_string_lossy().to_string();
                }
            }
        }
        "ffmpeg".to_string()
    }
}

struct AppState {
    db: Mutex<Database>,
    recorder: Recorder,
    parser: DouyinParser,
    auto_recorder: AutoRecorder,
    recoveries: recovery::Recoveries,
    start_lock: AsyncMutex<()>,
    lifecycle: Arc<lifecycle::Lifecycle>,
    #[cfg(test)]
    test_settings: Option<AppSettings>,
}

impl AppState {
    fn settings(&self) -> AppSettings {
        #[cfg(test)]
        if let Some(settings) = &self.test_settings {
            return settings.clone();
        }
        settings::load_settings()
    }
}

#[derive(Clone, Serialize)]
struct RecordingStatusChanged {
    task: RecordTask,
    room: Option<LiveRoom>,
    reason: String,
    message: Option<String>,
}

#[derive(Clone, Serialize)]
struct RoomAutoRecordingChanged {
    room: LiveRoom,
    reason: String,
    message: Option<String>,
}

#[derive(Clone, Copy)]
enum LiveVerification {
    Offline,
    Live,
    Failed,
}

fn classify_recording(
    file_size: i64,
    exit: &RecordingExit,
    verification: Option<LiveVerification>,
) -> &'static str {
    if file_size <= 0 {
        return "failed";
    }
    if exit.manually_stopped {
        return if exit.stopped_cleanly() {
            "completed"
        } else {
            "interrupted"
        };
    }
    match verification {
        Some(LiveVerification::Offline) => "completed",
        Some(LiveVerification::Live | LiveVerification::Failed) | None => "interrupted",
    }
}

fn should_poll_auto_room(enabled: bool, expired: bool, running: bool, due: bool) -> bool {
    enabled && !expired && !running && due
}

fn retry_is_due(room: &LiveRoom, now: DateTime<Utc>) -> bool {
    room.auto_record_retry_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .is_none_or(|until| until <= now)
}

fn set_retry(room: &mut LiveRoom, delay: u64, message: String) {
    room.auto_record_retry_at =
        Some((Utc::now() + ChronoDuration::seconds(delay as i64)).to_rfc3339());
    room.auto_record_error = Some(message);
}

fn get_recordings_dir(settings: &AppSettings) -> Result<String, String> {
    let dir = if settings.recordings_dir.is_empty() {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map_err(|_| "无法获取用户目录".to_string())?;
        std::path::Path::new(&home).join("DouyinRecordings")
    } else {
        std::path::PathBuf::from(&settings.recordings_dir)
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建录制目录失败: {}", e))?;
    Ok(dir.to_string_lossy().to_string())
}

fn file_size(path: Option<&str>) -> i64 {
    path.and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len() as i64)
        .unwrap_or(0)
}

fn emit_recording_event(
    app: &AppHandle,
    task: RecordTask,
    room: Option<LiveRoom>,
    reason: &str,
    message: Option<String>,
) {
    recording_log::event(
        if matches!(reason, "failed" | "interrupted") {
            "WARN"
        } else {
            "INFO"
        },
        "recording_status",
        Some(task.id),
        Some(task.room_id),
        json!({"reason": reason, "status": task.status, "trigger": task.trigger,
            "recovery_from_task_id": task.recovery_from_task_id,
            "file_path": task.file_path, "file_size": task.file_size, "message": message}),
    );
    let _ = app.emit(
        "recording-status-changed",
        RecordingStatusChanged {
            task,
            room,
            reason: reason.to_string(),
            message,
        },
    );
}

fn emit_auto_recording_event(
    app: &AppHandle,
    room: LiveRoom,
    reason: &str,
    message: Option<String>,
) {
    // Quiet offline polls emit enabled/None too; only meaningful changes are logged here.
    if message.is_some() {
        recording_log::event(
            if matches!(reason, "paused" | "backoff") {
                "WARN"
            } else {
                "INFO"
            },
            "auto_monitor_changed",
            None,
            Some(room.id),
            recording_log::automation_details(&room, reason, message.as_deref()),
        );
    }
    let _ = app.emit(
        "room-auto-recording-changed",
        RoomAutoRecordingChanged {
            room,
            reason: reason.to_string(),
            message,
        },
    );
}

fn apply_live_info(state: &AppState, room_id: i64, info: &LiveInfo) -> Result<LiveRoom, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    apply_live_info_in_db(&db, room_id, info)
}

fn apply_live_info_in_db(db: &Database, room_id: i64, info: &LiveInfo) -> Result<LiveRoom, String> {
    let room = db.get_room(room_id).map_err(|e| e.to_string())?;
    let anchor_name = if info.anchor_name.is_empty() {
        &room.anchor_name
    } else {
        &info.anchor_name
    };
    let room_title = if info.room_title.is_empty() {
        &room.room_title
    } else {
        &info.room_title
    };
    let cover_url = if info.cover_url.is_empty() {
        &room.cover_url
    } else {
        &info.cover_url
    };
    let avatar_url = if info.avatar_url.is_empty() {
        &room.avatar_url
    } else {
        &info.avatar_url
    };
    db.update_room_live_status(
        room_id,
        anchor_name,
        room_title,
        cover_url,
        avatar_url,
        info.is_live,
    )
    .map_err(|e| e.to_string())?;
    db.get_room(room_id).map_err(|e| e.to_string())
}

async fn refresh_room_internal(state: &AppState, room_id: i64) -> Result<LiveRoom, String> {
    let douyin_url = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let room = db.get_room(room_id).map_err(|e| e.to_string())?;
        format!("https://live.douyin.com/{}", room.room_id)
    };
    let app_settings = state.settings();
    let info = state
        .parser
        .parse_douyin_url(&douyin_url, &app_settings, RequestSource::ManualRefresh)
        .await
        .map_err(|e| e.to_string())?;
    apply_live_info(state, room_id, &info)
}

// Called with the database lock held, before publishing a finished task.
fn apply_post_recording_auto_policy(
    state: &AppState,
    room: &mut LiveRoom,
    task_trigger: &str,
    status: &str,
    exit: &RecordingExit,
    verification: Option<LiveVerification>,
    rate_limited: bool,
) -> (&'static str, Option<String>) {
    let settings = state.settings();
    let offline = matches!(verification, Some(LiveVerification::Offline));
    match auto_policy::post_record_action(
        room,
        task_trigger,
        status,
        exit.manually_stopped,
        offline,
        settings.auto_disable_after_record,
    ) {
        PostRecordAction::Retry => {
            let delay = if rate_limited {
                state
                    .auto_recorder
                    .mark_failure(room.id, settings.auto_check_interval_secs, true)
            } else {
                state
                    .auto_recorder
                    .mark_recording_failure(room.id, settings.auto_check_interval_secs)
            };
            let message = if rate_limited {
                "直播状态检测受到限制，等待 30 分钟后重试".to_string()
            } else {
                "录制异常结束，等待重试；将重新获取直播地址".to_string()
            };
            set_retry(room, delay, message.clone());
            ("backoff", Some(message))
        }
        PostRecordAction::Continue | PostRecordAction::RenewWindow => {
            room.auto_record_until = monitor_until(
                room.auto_monitor_mode,
                settings.auto_monitor_window_hours,
                Utc::now(),
            );
            room.auto_record_error = None;
            room.auto_record_retry_at = None;
            state.auto_recorder.mark_immediate(room.id);
            ("enabled", None)
        }
        PostRecordAction::Disable => {
            auto_policy::set_enabled(room, false, settings.auto_monitor_window_hours, Utc::now());
            state.auto_recorder.clear(room.id);
            if status == "completed" {
                (
                    "disabled",
                    Some("自动录制已完成，本次监控已关闭".to_string()),
                )
            } else {
                let message = "自动录制异常结束，已暂停该房间的自动监控".to_string();
                room.auto_record_error = Some(message.clone());
                ("paused", Some(message))
            }
        }
        PostRecordAction::Preserve => {
            if room.auto_record_enabled && monitor_is_expired(room, Utc::now()) {
                auto_policy::set_enabled(
                    room,
                    false,
                    settings.auto_monitor_window_hours,
                    Utc::now(),
                );
                state.auto_recorder.clear(room.id);
                ("window_expired", Some("自动录制监控时间已结束".to_string()))
            } else {
                if room.auto_record_enabled {
                    state.auto_recorder.mark_immediate(room.id);
                }
                ("state_changed", None)
            }
        }
    }
}

async fn handle_recording_exit(
    app: AppHandle,
    task_id: i64,
    room_id: i64,
    exit: RecordingExit,
) -> Result<(), String> {
    finalize_recording_exit(app, task_id, room_id, exit, None, true, None).await
}

#[allow(clippy::too_many_arguments)]
async fn finalize_recording_exit(
    app: AppHandle,
    task_id: i64,
    room_id: i64,
    exit: RecordingExit,
    verified: Option<(LiveVerification, bool, Option<String>)>,
    apply_policy: bool,
    work: Option<Arc<lifecycle::Operation>>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (original_path, original_size, task_trigger, finalizing_task) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task = db.get_task(task_id).map_err(|e| e.to_string())?;
        if task.status != "recording" && task.status != "finalizing" {
            return Ok(());
        }
        state.auto_recorder.mark_recording_ended(room_id);
        let size = if task.segment_output.is_some() {
            db.segment_total_size(task_id).map_err(|e| e.to_string())?
        } else {
            file_size(task.file_path.as_deref())
        };
        db.mark_task_finalizing(task_id, size)
            .map_err(|e| e.to_string())?;
        let updated = db.get_task(task_id).map_err(|e| e.to_string())?;
        (task.file_path, size, task.trigger, updated)
    };
    let segmented = finalizing_task.segment_output.is_some();
    emit_recording_event(&app, finalizing_task, None, "finalizing", None);
    let mut rate_limited = false;
    let (verification, verification_message) =
        if let Some((verification, limited, message)) = verified {
            rate_limited = limited;
            (Some(verification), message)
        } else if exit.manually_stopped {
            (None, None)
        } else {
            let room = {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                db.get_room(room_id).map_err(|e| e.to_string())?
            };
            let url = format!("https://live.douyin.com/{}", room.room_id);
            let result = state
                .parser
                .parse_douyin_url(
                    &url,
                    &state.settings(),
                    RequestSource::RecordingVerification,
                )
                .await;
            match result {
                Ok(info) => {
                    if let Some(logger) = recording_log::logger() {
                        logger.live_verification(
                            task_id,
                            room_id,
                            Some(if info.is_live {
                                LiveVerification::Live
                            } else {
                                LiveVerification::Offline
                            }),
                            false,
                            None,
                        );
                    }
                    apply_live_info(&state, room_id, &info)?;
                    if info.is_live {
                        (
                            Some(LiveVerification::Live),
                            Some("录制进程已结束，但主播仍在直播".to_string()),
                        )
                    } else {
                        (Some(LiveVerification::Offline), None)
                    }
                }
                Err(error) => {
                    rate_limited = error.is_rate_limited();
                    if let Some(logger) = recording_log::logger() {
                        logger.live_verification(
                            task_id,
                            room_id,
                            Some(LiveVerification::Failed),
                            rate_limited,
                            Some(error.to_string()),
                        );
                    }
                    (
                        Some(LiveVerification::Failed),
                        Some(format!("录制进程已结束，但无法确认直播状态: {}", error)),
                    )
                }
            }
        };
    if exit.manually_stopped {
        if let Some(logger) = recording_log::logger() {
            logger.live_verification(task_id, room_id, None, false, None);
        }
    }
    let status = classify_recording(original_size, &exit, verification);
    recording_log::event(
        "INFO",
        "recording_classified",
        Some(task_id),
        Some(room_id),
        json!({"status": status, "file_size": original_size, "message": verification_message}),
    );
    let mut final_path = original_path.clone();
    let mut final_size = original_size;
    let mut message = match status {
        "completed" if exit.manually_stopped => Some("录制已停止".to_string()),
        "completed" => Some("直播已结束，录制已完成".to_string()),
        "failed" => Some("录制已结束，但没有生成有效文件".to_string()),
        "interrupted" if exit.forced_stop_reason.is_some() => {
            Some("录制未能正常收尾，已强制停止并保留现有文件，末尾可能不完整".to_string())
        }
        "interrupted" if exit.manually_stopped => {
            Some("录制结束时发生异常，已保留现有文件，请检查末尾是否完整".to_string())
        }
        _ => verification_message,
    };
    if segmented {
        segment_runtime::finish_segments(&app, task_id, status, work).await?;
        final_size = state
            .db
            .lock()
            .map_err(|e| e.to_string())?
            .segment_total_size(task_id)
            .map_err(|e| e.to_string())?;
    } else if status == "completed"
        && state.settings().auto_convert_mp4
        && final_path
            .as_deref()
            .is_some_and(|path| path.ends_with(".flv"))
    {
        let path = final_path.clone().unwrap_or_default();
        match conversion::remux(path).await {
            Ok((mp4_path, mp4_size)) => {
                final_path = Some(mp4_path);
                final_size = mp4_size;
            }
            Err(error) => {
                message = Some(format!(
                    "{}；自动转换 MP4 失败，已保留 FLV: {}",
                    message.unwrap_or_else(|| "录制已完成".to_string()),
                    error
                ));
            }
        }
    }
    // Completion and the next monitoring decision are committed together. Commands
    // and ticks use this same lock, so they cannot publish an obsolete room snapshot.
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut room = db.get_room(room_id).map_err(|e| e.to_string())?;
    let (auto_reason, auto_message) = if !apply_policy
        || state.lifecycle.is_exiting()
        || state.recorder.stopped_for_exit(task_id)
    {
        // Application shutdown must not consume a monitoring window or change
        // the user's auto-record intent, even if a timed-out shutdown was cancelled.
        ("state_changed", None)
    } else {
        apply_post_recording_auto_policy(
            &state,
            &mut room,
            &task_trigger,
            status,
            &exit,
            verification,
            rate_limited,
        )
    };
    let (final_task, room) = if apply_policy {
        db.finish_task_and_automation(task_id, status, final_path.as_deref(), final_size, &room)
            .map_err(|e| e.to_string())?
    } else {
        db.finish_task(task_id, status, final_path.as_deref(), final_size)
            .map_err(|e| e.to_string())?;
        (db.get_task(task_id).map_err(|e| e.to_string())?, room)
    };
    if final_path != original_path {
        if let Some(source) = original_path {
            if let Err(error) = std::fs::remove_file(source) {
                message = Some(format!("MP4 已保存，原 FLV 删除失败: {error}"));
            }
        }
    }
    if apply_policy {
        recording_log::event(
            "INFO",
            "recording_auto_policy",
            Some(task_id),
            Some(room_id),
            recording_log::automation_details(&room, auto_reason, auto_message.as_deref()),
        );
        emit_auto_recording_event(&app, room.clone(), auto_reason, auto_message);
    }
    let reason = match status {
        "completed" if exit.manually_stopped => "manual_stop",
        "completed" => "stream_ended",
        "interrupted" => "interrupted",
        _ => "failed",
    };
    emit_recording_event(
        &app,
        final_task,
        apply_policy.then_some(room),
        reason,
        message,
    );
    Ok(())
}

#[derive(Debug)]
enum StartRecordError {
    Superseded,
    AlreadyRunning,
    Transient(String),
    Fatal(String),
}

impl std::fmt::Display for StartRecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Superseded => f.write_str("自动录制设置已变化或监控已结束"),
            Self::AlreadyRunning => f.write_str("该直播间已经在录制或结束处理中"),
            Self::Transient(message) | Self::Fatal(message) => f.write_str(message),
        }
    }
}

async fn start_record_from_info(
    app: &AppHandle,
    state: &AppState,
    room_id: i64,
    info: &LiveInfo,
    trigger: &str,
    expected_revision: Option<i64>,
) -> Result<RecordTask, StartRecordError> {
    let result =
        start_record_from_info_inner(app, state, room_id, info, trigger, expected_revision, None)
            .await;
    if let Err(error) = &result {
        recording_log::event(
            if matches!(
                error,
                StartRecordError::Superseded | StartRecordError::AlreadyRunning
            ) {
                "INFO"
            } else {
                "ERROR"
            },
            "recording_start_rejected",
            None,
            Some(room_id),
            json!({"trigger": trigger, "error": error.to_string()}),
        );
    }
    result
}

async fn start_record_from_info_inner(
    app: &AppHandle,
    state: &AppState,
    room_id: i64,
    info: &LiveInfo,
    trigger: &str,
    expected_revision: Option<i64>,
    recovery: Option<(&recovery::Ticket, i64)>,
) -> Result<RecordTask, StartRecordError> {
    let _start_guard = state.start_lock.lock().await;
    if state.lifecycle.is_exiting() {
        return Err(StartRecordError::Superseded);
    }
    // Register the process before a stop command can observe the new task.
    let db = state
        .db
        .lock()
        .map_err(|e| StartRecordError::Fatal(e.to_string()))?;
    let fatal = |e: rusqlite::Error| StartRecordError::Fatal(e.to_string());
    if let Some((ticket, _)) = recovery {
        if !ticket.usable()
            || !state.recoveries.owns(ticket)
            || !state.settings().recording_recovery_enabled
        {
            return Err(StartRecordError::Superseded);
        }
    } else if state.recoveries.busy(room_id) {
        return Err(StartRecordError::AlreadyRunning);
    }
    if (if recovery.is_some() {
        db.has_capturing_tasks_for_room(room_id)
    } else {
        db.has_running_tasks_for_room(room_id)
    })
    .map_err(fatal)?
    {
        return Err(StartRecordError::AlreadyRunning);
    }
    let mut room = db.get_room(room_id).map_err(fatal)?;
    if recovery.is_none()
        && trigger == "auto"
        && !expected_revision
            .is_some_and(|revision| auto_policy::accepts_check(&room, revision, false, Utc::now()))
    {
        return Err(StartRecordError::Superseded);
    }
    if info.stream_url.is_empty() {
        return Err(StartRecordError::Transient(
            "直播流地址为空，稍后重新检查".to_string(),
        ));
    }
    let recordings_dir = get_recordings_dir(&state.settings()).map_err(StartRecordError::Fatal)?;
    let task_id = db.add_task(room_id, trigger).map_err(fatal)?;
    if let Some((_, previous)) = recovery {
        db.link_recovery(task_id, previous).map_err(fatal)?;
    }
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    // Task id also prevents a quick retry from overwriting an earlier partial file.
    let app_settings = state.settings();
    let basename = format!(
        "{}_{}_{}_{}",
        segments::safe_filename(&room.anchor_name),
        segments::safe_filename(&room.room_id),
        timestamp,
        task_id
    );
    let segment_output = app_settings.segment_recording_enabled.then(|| {
        segments::SegmentOutput::new(
            std::path::Path::new(&recordings_dir),
            &basename,
            u64::from(app_settings.segment_duration_minutes.max(1)) * 60,
        )
    });
    let output_str = segment_output
        .as_ref()
        .map(|output| output.path(1))
        .unwrap_or_else(|| {
            std::path::Path::new(&recordings_dir)
                .join(format!("{basename}.flv"))
                .to_string_lossy()
                .into_owned()
        });
    recording_log::event(
        "INFO",
        "recording_start_requested",
        Some(task_id),
        Some(room_id),
        json!({"platform_room_id": room.room_id, "trigger": trigger, "mode": room.auto_monitor_mode,
            "recovery_id": recovery.map(|(ticket, _)| ticket.id),
            "recovery_from_task_id": recovery.map(|(_, previous)| previous),
            "quality": app_settings.quality, "segmented": segment_output.is_some(),
            "segment_duration_minutes": app_settings.segment_duration_minutes, "output_path": output_str,
            "ffmpeg_reconnect_enabled": app_settings.ffmpeg_reconnect_enabled,
            "ffmpeg_rw_timeout_secs": app_settings.ffmpeg_rw_timeout_secs,
            "ffmpeg_reconnect_max_retries": app_settings.ffmpeg_reconnect_max_retries,
            "ffmpeg_reconnect_delay_max_secs": app_settings.ffmpeg_reconnect_delay_max_secs,
            "ffmpeg_reconnect_delay_total_max_secs": app_settings.ffmpeg_reconnect_delay_total_max_secs}),
    );
    db.update_task_status_and_path(
        task_id,
        "recording",
        if segment_output.is_none() {
            Some(&output_str)
        } else {
            None
        },
    )
    .map_err(fatal)?;
    if let Some(output) = &segment_output {
        db.set_segment_output(task_id, output).map_err(fatal)?;
    }
    let exit_app = app.clone();
    let start_result = (|| -> Result<(RecordTask, LiveRoom), String> {
        if let Some(output) = &segment_output {
            let manifest = std::path::Path::new(&output.manifest_path);
            std::fs::create_dir_all(manifest.parent().ok_or("分段清单目录无效")?)
                .map_err(|e| e.to_string())?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(manifest)
                .map_err(|e| format!("无法创建分段清单: {e}"))?;
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_str)
            .map_err(|error| format!("无法创建录制文件: {}", error))?;
        drop(file);
        if segment_output.is_some() {
            let task = db.get_task(task_id).map_err(|e| e.to_string())?;
            db.add_segment(task_id, 1, &output_str, &task.start_time)
                .map_err(|e| e.to_string())?;
        }
        // Finish fallible database work before spawning. Once a process exists,
        // recovery must never mistake a metadata error for a failed launch.
        room.auto_record_error = None;
        room.auto_record_retry_at = None;
        let published_room = db.save_room_automation(&room).map_err(|e| e.to_string())?;
        let published_task = db.get_task(task_id).map_err(|e| e.to_string())?;
        let (monitor_stop, monitor_rx) = tokio::sync::watch::channel(false);
        let work = Arc::new(state.lifecycle.operation()?);
        let monitor = segment_output.clone().map(|output| {
            tokio::spawn(segment_runtime::monitor(
                app.clone(),
                task_id,
                output,
                monitor_rx,
                work.clone(),
            ))
        });
        let abort = monitor.as_ref().map(|handle| handle.abort_handle());
        let stop_on_failure = monitor_stop.clone();
        if recovery.is_some_and(|(ticket, _)| !ticket.usable()) {
            let _ = stop_on_failure.send(true);
            if let Some(abort) = abort {
                abort.abort();
            }
            return Err("已到最长恢复时间，未启动新进程".into());
        }
        let result = state.recorder.start_record_before(
            task_id,
            &info.stream_url,
            &output_str,
            &app_settings,
            segment_output.as_ref(),
            recovery.map(|(ticket, _)| ticket.deadline),
            move |exit| {
                recovery_runtime::recording_exit(
                    exit_app,
                    task_id,
                    room_id,
                    exit,
                    monitor_stop,
                    monitor,
                    work,
                )
            },
        );
        if result.is_err() {
            let _ = stop_on_failure.send(true);
            if let Some(abort) = abort {
                abort.abort();
            }
        }
        result.map(|()| (published_task, published_room))
    })();
    if let Err(error) = start_result {
        recording_log::event(
            "ERROR",
            "recording_start_failed",
            Some(task_id),
            Some(room_id),
            json!({"error": error}),
        );
        for segment in db.get_segments(task_id).map_err(fatal)? {
            db.close_segment(
                segment.id,
                "failed",
                &Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            )
            .map_err(fatal)?;
        }
        db.finish_task(
            task_id,
            "failed",
            if segment_output.is_none() {
                Some(&output_str)
            } else {
                None
            },
            0,
        )
        .map_err(fatal)?;
        emit_recording_event(
            app,
            db.get_task(task_id).map_err(fatal)?,
            Some(room),
            "failed",
            Some(format!("启动录制失败: {}", error)),
        );
        return Err(StartRecordError::Fatal(error));
    }
    let (task, room) = start_result.map_err(StartRecordError::Fatal)?;
    recording_log::event(
        "INFO",
        "recording_started",
        Some(task_id),
        Some(room_id),
        json!({"trigger": trigger}),
    );
    state.auto_recorder.mark_recording_started(room_id);
    if let Some((ticket, _)) = recovery {
        if let Some(status) = state.recoveries.started(ticket, task_id) {
            recovery_runtime::publish(app, status, "process_started");
        }
    }
    if recovery.is_some() {
        emit_recording_event(app, task.clone(), Some(room), "recovery_started", None);
        recovery_runtime::watch_media(app.clone(), room_id, task_id);
    } else if trigger == "auto" {
        emit_recording_event(
            app,
            task.clone(),
            Some(room),
            "auto_started",
            Some("检测到主播开播，已开始自动录制".to_string()),
        );
    } else {
        emit_auto_recording_event(app, room, "state_changed", None);
    }
    Ok(task)
}

fn handle_check_error(
    app: &AppHandle,
    snapshot: &LiveRoom,
    message: String,
    rate_limited: bool,
    fatal: bool,
) -> Result<(), String> {
    recording_log::event(
        "WARN",
        "auto_check_error",
        None,
        Some(snapshot.id),
        json!({"error": message, "fatal": fatal, "rate_limited": rate_limited}),
    );
    let state = app.state::<AppState>();
    if state.lifecycle.is_exiting() {
        return Ok(());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut room = db.get_room(snapshot.id).map_err(|e| e.to_string())?;
    let running = db
        .has_running_tasks_for_room(room.id)
        .map_err(|e| e.to_string())?
        || state.recoveries.busy(room.id);
    if !auto_policy::accepts_check(&room, snapshot.auto_record_revision, running, Utc::now()) {
        return Ok(());
    }
    let settings = state.settings();
    let (reason, message) = if fatal {
        auto_policy::set_enabled(
            &mut room,
            false,
            settings.auto_monitor_window_hours,
            Utc::now(),
        );
        state.auto_recorder.clear(room.id);
        let message = format!("自动录制无法启动，请处理后重新开启监控: {}", message);
        room.auto_record_error = Some(message.clone());
        ("paused", message)
    } else {
        let delay = state.auto_recorder.mark_failure(
            room.id,
            settings.auto_check_interval_secs,
            rate_limited,
        );
        let message = if rate_limited {
            format!("开播检测受到限制，等待 30 分钟后重试: {}", message)
        } else {
            format!("开播检测失败，等待重试: {}", message)
        };
        set_retry(&mut room, delay, message.clone());
        ("backoff", message)
    };
    let room = db.save_room_automation(&room).map_err(|e| e.to_string())?;
    emit_auto_recording_event(app, room, reason, Some(message));
    Ok(())
}

async fn check_auto_room(app: &AppHandle, snapshot: LiveRoom) -> Result<(), String> {
    let state = app.state::<AppState>();
    let Ok(_operation) = state.lifecycle.operation() else {
        return Ok(());
    };
    let settings = state.settings();
    let url = format!("https://live.douyin.com/{}", snapshot.room_id);
    let info = match state
        .parser
        .parse_douyin_url(&url, &settings, RequestSource::AutoCheck)
        .await
    {
        Ok(info) => info,
        Err(error) => {
            return handle_check_error(
                app,
                &snapshot,
                error.to_string(),
                error.is_rate_limited(),
                false,
            )
        }
    };
    if state.lifecycle.is_exiting() {
        return Ok(());
    }
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let room = db.get_room(snapshot.id).map_err(|e| e.to_string())?;
        let running = db
            .has_running_tasks_for_room(room.id)
            .map_err(|e| e.to_string())?
            || state.recoveries.busy(room.id);
        if !auto_policy::accepts_check(&room, snapshot.auto_record_revision, running, Utc::now()) {
            return Ok(());
        }
        let mut room = apply_live_info_in_db(&db, room.id, &info)?;
        if !info.is_live {
            if room.auto_record_error.is_some() {
                recording_log::event(
                    "INFO",
                    "auto_check_recovered",
                    None,
                    Some(room.id),
                    json!({"result": "offline"}),
                );
            }
            state
                .auto_recorder
                .mark_success(room.id, settings.auto_check_interval_secs);
            room.auto_record_error = None;
            room.auto_record_retry_at = None;
            let room = db.save_room_automation(&room).map_err(|e| e.to_string())?;
            emit_auto_recording_event(app, room, "enabled", None);
            return Ok(());
        }
    }
    match start_record_from_info(
        app,
        &state,
        snapshot.id,
        &info,
        "auto",
        Some(snapshot.auto_record_revision),
    )
    .await
    {
        Ok(_) | Err(StartRecordError::Superseded | StartRecordError::AlreadyRunning) => Ok(()),
        Err(StartRecordError::Transient(message)) => {
            handle_check_error(app, &snapshot, message, false, false)
        }
        Err(StartRecordError::Fatal(message)) => {
            handle_check_error(app, &snapshot, message, false, true)
        }
    }
}

fn process_auto_deadlines_and_schedules(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let Ok(_operation) = state.lifecycle.operation() else {
        return Ok(());
    };
    let settings = state.settings();
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let now = Local::now();
    for mut room in db.get_all_rooms().map_err(|e| e.to_string())? {
        let running = db
            .has_running_tasks_for_room(room.id)
            .map_err(|e| e.to_string())?
            || state.recoveries.busy(room.id);
        let action = auto_policy::tick(&mut room, running, settings.auto_monitor_window_hours, now);
        let (reason, message) = match action {
            TickAction::None => continue,
            TickAction::Scheduled => {
                if room.auto_record_retry_at.is_none() && !running {
                    state.auto_recorder.mark_immediate(room.id);
                }
                ("schedule_triggered", "每日定时已触发，开始监控直播状态")
            }
            TickAction::Expired => {
                state.auto_recorder.clear(room.id);
                ("window_expired", "自动录制监控时间已结束")
            }
        };
        let room = db.save_room_automation(&room).map_err(|e| e.to_string())?;
        emit_auto_recording_event(app, room, reason, Some(message.to_string()));
    }
    Ok(())
}

fn next_due_auto_room(app: &AppHandle) -> Result<Option<LiveRoom>, String> {
    let state = app.state::<AppState>();
    if state.lifecycle.is_exiting() {
        return Ok(None);
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let now = Utc::now();
    for room in db.get_all_rooms().map_err(|e| e.to_string())? {
        let running = db
            .has_running_tasks_for_room(room.id)
            .map_err(|e| e.to_string())?
            || state.recoveries.busy(room.id);
        let due = retry_is_due(&room, now) && state.auto_recorder.is_due(room.id);
        if should_poll_auto_room(
            room.auto_record_enabled,
            monitor_is_expired(&room, now),
            running,
            due,
        ) {
            return Ok(Some(room));
        }
    }
    Ok(None)
}

async fn auto_record_loop(app: AppHandle) {
    loop {
        if let Err(error) = process_auto_deadlines_and_schedules(&app) {
            recording_log::event(
                "ERROR",
                "auto_schedule_failed",
                None,
                None,
                json!({"error": error}),
            );
        }
        match next_due_auto_room(&app) {
            Ok(Some(room)) => {
                let room_id = room.id;
                if let Err(error) = check_auto_room(&app, room).await {
                    let state = app.state::<AppState>();
                    let delay = state.auto_recorder.mark_failure(
                        room_id,
                        state.settings().auto_check_interval_secs,
                        false,
                    );
                    recording_log::event(
                        "ERROR",
                        "auto_check_failed",
                        None,
                        Some(room_id),
                        json!({"error": error, "retry_at": (Utc::now() + ChronoDuration::seconds(delay as i64)).to_rfc3339()}),
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                recording_log::event(
                    "ERROR",
                    "auto_selection_failed",
                    None,
                    None,
                    json!({"error": error}),
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn reconcile_auto_state_on_startup(db: &Database, settings: &AppSettings) -> Result<(), String> {
    let now = Local::now();
    for mut room in db.get_all_rooms().map_err(|e| e.to_string())? {
        auto_policy::reconcile(&mut room, settings.auto_monitor_window_hours, now);
        db.save_room_automation(&room).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_settings_cmd(state: State<AppState>) -> Result<AppSettings, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut current_settings = state.settings();
    current_settings.db_path = db.path().to_string_lossy().to_string();
    Ok(current_settings)
}

#[tauri::command]
async fn save_settings_cmd(
    app: AppHandle,
    state: State<'_, AppState>,
    new_settings: AppSettings,
) -> Result<AppSettings, String> {
    let _operation = state.lifecycle.operation()?;
    let _start = state.start_lock.lock().await;
    let saved = migration::save_settings(&state, new_settings, &settings::settings_path())?;
    if !saved.recording_recovery_enabled {
        recovery_runtime::cancel_all(&app, recovery::CancelReason::Settings, true);
    }
    state.lifecycle.close_to_tray.store(
        saved.close_behavior == settings::CloseBehavior::Tray,
        Ordering::Release,
    );
    Ok(saved)
}

#[tauri::command]
async fn migrate_db_cmd(app: AppHandle, new_path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let _operation = app.state::<AppState>().lifecycle.operation()?;
        migration::migrate_database(
            &app.state::<AppState>(),
            &new_path,
            &settings::settings_path(),
        )
    })
    .await
    .map_err(|e| format!("数据库迁移任务异常: {}", e))?
}

#[tauri::command]
async fn check_for_update(app: AppHandle) -> Result<Option<updater::UpdateInfo>, String> {
    let current_version = app.package_info().version.clone();
    let app_settings = app.state::<AppState>().settings();
    updater::check_for_update(&current_version, &app_settings).await
}

#[tauri::command]
fn get_rooms(state: State<AppState>) -> Result<Vec<LiveRoom>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.get_all_rooms().map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_room(state: State<'_, AppState>, url: String) -> Result<LiveRoom, String> {
    let _operation = state.lifecycle.operation()?;
    let app_settings = state.settings();
    let info = state
        .parser
        .parse_douyin_url(&url, &app_settings, RequestSource::AddRoom)
        .await
        .map_err(|e| e.to_string())?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let room_id = db
        .add_room_full(
            &info.platform,
            &info.room_id,
            &info.anchor_name,
            &info.room_title,
            &info.cover_url,
            &info.avatar_url,
            info.is_live,
        )
        .map_err(|e| e.to_string())?;
    db.get_room(room_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn refresh_room(state: State<'_, AppState>, room_id: i64) -> Result<LiveRoom, String> {
    let _operation = state.lifecycle.operation()?;
    refresh_room_internal(&state, room_id).await
}

#[tauri::command]
async fn set_room_auto_record(
    app: AppHandle,
    state: State<'_, AppState>,
    room_id: i64,
    enabled: bool,
) -> Result<LiveRoom, String> {
    let _operation = state.lifecycle.operation()?;
    let _start = state.start_lock.lock().await;
    if !enabled
        && state
            .recoveries
            .status(room_id)
            .is_some_and(|s| s.trigger == "auto")
    {
        recovery_runtime::cancel(&app, room_id, None, recovery::CancelReason::Monitor, true);
    }
    let settings = state.settings();
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut room = db.get_room(room_id).map_err(|e| e.to_string())?;
    auto_policy::set_enabled(
        &mut room,
        enabled,
        settings.auto_monitor_window_hours,
        Utc::now(),
    );
    let room = db.save_room_automation(&room).map_err(|e| e.to_string())?;
    if enabled {
        state.auto_recorder.mark_immediate(room_id);
    } else {
        state.auto_recorder.clear(room_id);
    }
    emit_auto_recording_event(
        &app,
        room.clone(),
        if enabled { "enabled" } else { "disabled" },
        Some(
            if enabled {
                "自动录制已开启，正在检查直播状态"
            } else {
                "自动录制已关闭；正在进行的录制不会停止"
            }
            .to_string(),
        ),
    );
    Ok(room)
}

fn save_room_auto_config(
    app: &AppHandle,
    state: &AppState,
    room_id: i64,
    mode: Option<AutoMonitorMode>,
    daily_time: Option<String>,
) -> Result<LiveRoom, String> {
    let _operation = state.lifecycle.operation()?;
    let settings = state.settings();
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut room = db.get_room(room_id).map_err(|e| e.to_string())?;
    let mode = mode.unwrap_or(room.auto_monitor_mode);
    auto_policy::configure(
        &mut room,
        mode,
        daily_time,
        settings.auto_monitor_window_hours,
        Local::now(),
    )?;
    let room = db.save_room_automation(&room).map_err(|e| e.to_string())?;
    emit_auto_recording_event(
        app,
        room.clone(),
        "configured",
        Some("录制设置已保存".to_string()),
    );
    Ok(room)
}

#[tauri::command]
fn set_room_auto_config(
    app: AppHandle,
    state: State<AppState>,
    room_id: i64,
    monitor_mode: AutoMonitorMode,
    daily_time: Option<String>,
) -> Result<LiveRoom, String> {
    save_room_auto_config(&app, &state, room_id, Some(monitor_mode), daily_time)
}

#[tauri::command]
fn set_room_auto_schedule(
    app: AppHandle,
    state: State<AppState>,
    room_id: i64,
    daily_time: Option<String>,
) -> Result<LiveRoom, String> {
    save_room_auto_config(&app, &state, room_id, None, daily_time)
}

#[tauri::command]
fn get_room_task_count(state: State<AppState>, room_id: i64) -> Result<i64, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.count_tasks_for_room(room_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_room(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    cascade: Option<bool>,
) -> Result<(), String> {
    let _operation = state.lifecycle.operation()?;
    {
        let _start = state.start_lock.lock().await;
        recovery_runtime::cancel(&app, id, None, recovery::CancelReason::Superseded, true);
    }
    // A cancelled query still owns its old output until the exit callback saves it.
    // The existing running-work guard below keeps deletion reviewable and safe.
    let _start = state.start_lock.lock().await;
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        if state.recoveries.busy(id)
            || db
                .has_running_tasks_for_room(id)
                .map_err(|e| e.to_string())?
        {
            return Err("该房间正在录制或结束处理中，请先停止录制".to_string());
        }
        if cascade == Some(true) {
            db.delete_room_cascade(id).map_err(|e| e.to_string())?;
        } else {
            let task_count = db.count_tasks_for_room(id).map_err(|e| e.to_string())?;
            if task_count > 0 {
                return Err(format!("该房间还有 {} 个录制任务，请确认删除", task_count));
            }
            db.delete_room(id).map_err(|e| e.to_string())?;
        }
    }
    state.auto_recorder.clear(id);
    Ok(())
}

#[tauri::command]
fn get_tasks(state: State<AppState>) -> Result<Vec<RecordTask>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.get_all_tasks().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_recording_recoveries(state: State<AppState>) -> Vec<recovery::RecoveryStatus> {
    state.recoveries.snapshot()
}

#[tauri::command]
async fn cancel_recording_recovery(
    app: AppHandle,
    state: State<'_, AppState>,
    room_id: i64,
    recovery_id: u64,
) -> Result<bool, String> {
    let _operation = state.lifecycle.operation()?;
    let _start = state.start_lock.lock().await;
    Ok(recovery_runtime::cancel(
        &app,
        room_id,
        Some(recovery_id),
        recovery::CancelReason::User,
        false,
    )
    .is_some())
}

#[tauri::command]
async fn start_record(
    app: AppHandle,
    state: State<'_, AppState>,
    room_id: i64,
) -> Result<RecordTask, String> {
    let _operation = state.lifecycle.operation()?;
    let previous = {
        let _start = state.start_lock.lock().await;
        if state.recoveries.busy(room_id) {
            if state
                .recoveries
                .status(room_id)
                .is_some_and(|s| s.phase == "recording")
            {
                return Err("该直播间已经在录制".into());
            }
            recovery_runtime::cancel(
                &app,
                room_id,
                None,
                recovery::CancelReason::Superseded,
                true,
            )
            .map(|s| s.task_id)
        } else {
            None
        }
    };
    if let Some(task) = previous {
        state.recorder.stop_record(task).await?;
    }
    let douyin_url = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let room = db.get_room(room_id).map_err(|e| e.to_string())?;
        format!("https://live.douyin.com/{}", room.room_id)
    };
    let app_settings = state.settings();
    let info = state
        .parser
        .parse_douyin_url(&douyin_url, &app_settings, RequestSource::ManualStart)
        .await
        .map_err(|error| {
            recording_log::event("ERROR", "recording_start_failed", None, Some(room_id),
                json!({"trigger": "manual", "stage": "live_check", "error": error.to_string(), "rate_limited": error.is_rate_limited()}));
            error.to_string()
        })?;
    apply_live_info(&state, room_id, &info)?;
    state.lifecycle.ensure_running()?;
    if !info.is_live {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task_id = db.add_task(room_id, "manual").map_err(|e| e.to_string())?;
        db.update_task_status(task_id, "waiting")
            .map_err(|e| e.to_string())?;
        recording_log::event(
            "INFO",
            "recording_not_live",
            Some(task_id),
            Some(room_id),
            json!({"trigger": "manual"}),
        );
        return Err("主播未开播，任务已创建但未开始录制".to_string());
    }
    start_record_from_info(&app, &state, room_id, &info, "manual", None)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_task(state: State<AppState>, id: i64) -> Result<(), String> {
    let _operation = state.lifecycle.operation()?;
    if state.recorder.is_active(id) {
        return Err("该任务正在录制或结束处理中，请先停止录制".to_string());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let task = db.get_task(id).map_err(|e| e.to_string())?;
    if task.status == "recording" || task.status == "finalizing" {
        return Err("该任务正在录制或结束处理中，请先停止录制".to_string());
    }
    if task
        .segments
        .iter()
        .any(|segment| matches!(segment.conversion_state.as_str(), "queued" | "converting"))
    {
        return Err("分段正在转换，请等待完成后再删除记录".into());
    }
    db.delete_task(id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn stop_record(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: i64,
) -> Result<RecordTask, String> {
    let _operation = state.lifecycle.operation()?;
    {
        let _start_guard = state.start_lock.lock().await;
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task = db.get_task(task_id).map_err(|e| e.to_string())?;
        let stopping_chain = state.recoveries.includes_task(task.room_id, task_id);
        // Also records intent when FFmpeg has exited but its callback is waiting
        // for this lock and has not yet registered a recovery episode.
        state.recorder.request_stop(task_id);
        if stopping_chain {
            recovery_runtime::cancel(
                &app,
                task.room_id,
                None,
                recovery::CancelReason::Stop,
                false,
            );
        }
        recording_log::event(
            "INFO",
            "recording_stop_requested",
            Some(task_id),
            Some(task.room_id),
            json!({"status": task.status}),
        );
        let mut room = db.get_room(task.room_id).map_err(|e| e.to_string())?;
        if auto_policy::disable_for_manual_stop(
            &mut room,
            if stopping_chain {
                "recording"
            } else {
                &task.status
            },
            Utc::now(),
        ) {
            let room = db.save_room_automation(&room).map_err(|e| e.to_string())?;
            state.auto_recorder.clear(room.id);
            emit_auto_recording_event(
                &app,
                room,
                "disabled",
                Some("已关闭持续监控，正在停止录制".to_string()),
            );
        }
    }
    let was_active = state.recorder.stop_record(task_id).await?;
    if !was_active {
        let task = {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            db.get_task(task_id).map_err(|e| e.to_string())?
        };
        if task.status == "recording" {
            recording_log::event(
                "ERROR",
                "recording_process_missing",
                Some(task_id),
                Some(task.room_id),
                json!({"error": "未找到对应的 FFmpeg 进程"}),
            );
            handle_recording_exit(
                app,
                task_id,
                task.room_id,
                RecordingExit {
                    manually_stopped: true,
                    status_success: false,
                    exit_code: None,
                    forced_stop_reason: None,
                    wait_error: Some("未找到对应的 FFmpeg 进程".to_string()),
                    stderr_tail: Vec::new(),
                },
            )
            .await?;
        }
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.get_task(task_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn convert_to_mp4(app: AppHandle, task_id: i64) -> Result<String, String> {
    let state = app.state::<AppState>();
    let _operation = state.lifecycle.operation()?;
    let task = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task = db.get_task(task_id).map_err(|e| e.to_string())?;
        if task.status == "recording" || task.status == "finalizing" {
            return Err("录制或转换尚未结束，请稍后再试".into());
        }
        if task.segment_output.is_some() {
            return Err("请选择具体分段进行转换".into());
        }
        if task
            .file_path
            .as_deref()
            .is_none_or(|path| !path.ends_with(".flv"))
        {
            return Err("找不到可转换的 FLV 文件".into());
        }
        db.update_task_status(task_id, "finalizing")
            .map_err(|e| e.to_string())?;
        emit_recording_event(
            &app,
            db.get_task(task_id).map_err(|e| e.to_string())?,
            None,
            "conversion_started",
            None,
        );
        task
    };
    let source = task.file_path.clone().unwrap();
    let result = conversion::remux(source.clone()).await;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    match &result {
        Ok((path, size)) => {
            db.finish_task(task_id, &task.status, Some(path), *size)
                .map_err(|e| e.to_string())?;
            if let Err(error) = std::fs::remove_file(&source) {
                emit_recording_event(
                    &app,
                    db.get_task(task_id).map_err(|e| e.to_string())?,
                    None,
                    "conversion_finished",
                    Some(format!("MP4 已保存，原 FLV 删除失败: {error}")),
                );
            }
        }
        Err(error) => {
            db.update_task_status(task_id, &task.status)
                .map_err(|e| e.to_string())?;
            state.lifecycle.record_failure(error.clone());
        }
    }
    emit_recording_event(
        &app,
        db.get_task(task_id).map_err(|e| e.to_string())?,
        None,
        "conversion_finished",
        None,
    );
    result.map(|(path, _)| path)
}

#[tauri::command]
async fn convert_segment_to_mp4(app: AppHandle, segment_id: i64) -> Result<String, String> {
    let state = app.state::<AppState>();
    let _operation = state.lifecycle.operation()?;
    let task_id = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        if !db
            .claim_segment_conversion(segment_id)
            .map_err(|e| e.to_string())?
        {
            return Err("该分段正在录制、转换中，或没有可转换的 FLV 文件".into());
        }
        db.get_segment(segment_id)
            .map_err(|e| e.to_string())?
            .task_id
    };
    segment_runtime::emit_segments(&app, task_id)?;
    let result = conversion::convert_claimed_segment(app.clone(), segment_id).await;
    if let Err(error) = &result {
        state.lifecycle.record_failure(error.clone());
    }
    result
}

#[tauri::command]
fn delete_segment(app: AppHandle, segment_id: i64) -> Result<(), String> {
    let state = app.state::<AppState>();
    let _operation = state.lifecycle.operation()?;
    let task_id = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        if !db.hide_segment(segment_id).map_err(|e| e.to_string())? {
            return Err("正在录制或转换，暂时不能删除记录".into());
        }
        db.get_segment(segment_id)
            .map_err(|e| e.to_string())?
            .task_id
    };
    segment_runtime::emit_segments(&app, task_id)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[cfg(not(test))]
pub fn run() {
    tauri::Builder::default()
        // This must run before database reconciliation or any background task.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            desktop::request_restore(app)
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            recording_log::initialize();
            let db = Database::new(&settings::get_db_path())?;
            db.reconcile_incomplete_tasks()?;
            let app_settings = settings::load_settings();
            reconcile_auto_state_on_startup(&db, &app_settings).map_err(std::io::Error::other)?;
            let lifecycle = Arc::new(lifecycle::Lifecycle::default());
            lifecycle.close_to_tray.store(
                app_settings.close_behavior == settings::CloseBehavior::Tray,
                Ordering::Release,
            );
            app.manage(AppState {
                db: Mutex::new(db),
                recorder: Recorder::new(resolve_ffmpeg_path()),
                parser: DouyinParser::new(),
                auto_recorder: AutoRecorder::new(),
                recoveries: crate::recovery::Recoveries::default(),
                #[cfg(test)]
                test_settings: None,
                start_lock: AsyncMutex::new(()),
                lifecycle,
            });
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(auto_record_loop(app_handle));
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    desktop::request_close(window.app_handle());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_rooms,
            add_room,
            refresh_room,
            set_room_auto_record,
            set_room_auto_schedule,
            set_room_auto_config,
            delete_room,
            get_room_task_count,
            get_tasks,
            delete_task,
            start_record,
            stop_record,
            convert_to_mp4,
            convert_segment_to_mp4,
            delete_segment,
            get_settings_cmd,
            save_settings_cmd,
            get_recording_recoveries,
            cancel_recording_recovery,
            migrate_db_cmd,
            check_for_update,
            desktop::get_lifecycle_status,
            recording_log::get_recording_log_info,
        ])
        .build(tauri::generate_context!())
        .expect("应用启动失败")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if let Some(state) = app.try_state::<AppState>() {
                    if !state.lifecycle.allow_exit.load(Ordering::Acquire) {
                        api.prevent_exit();
                        desktop::request_exit(app);
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{
        classify_recording, daily_schedule_is_due, initial_schedule_marker, should_poll_auto_room,
        LiveVerification,
    };
    use crate::database::LiveRoom;
    use crate::recorder::RecordingExit;
    use chrono::{Local, TimeZone};

    fn scheduled_room(time: &str, last_date: Option<&str>) -> LiveRoom {
        LiveRoom {
            id: 1,
            platform: "douyin".to_string(),
            room_id: "123".to_string(),
            anchor_name: String::new(),
            room_title: String::new(),
            cover_url: String::new(),
            avatar_url: String::new(),
            is_live: false,
            created_at: String::new(),
            auto_record_enabled: false,
            auto_record_daily_time: Some(time.to_string()),
            auto_record_until: None,
            last_schedule_trigger_date: last_date.map(str::to_string),
            ..Default::default()
        }
    }

    fn recording_exit(manually_stopped: bool, status_success: bool) -> RecordingExit {
        RecordingExit {
            manually_stopped,
            status_success,
            exit_code: Some(if status_success { 0 } else { 1 }),
            forced_stop_reason: None,
            wait_error: None,
            stderr_tail: Vec::new(),
        }
    }

    #[test]
    fn only_clean_manual_stops_complete_nonempty_recordings() {
        let mut exit = recording_exit(true, true);
        assert_eq!(classify_recording(1024, &exit, None), "completed");
        exit.forced_stop_reason = Some("停止超时".to_string());
        assert_eq!(classify_recording(1024, &exit, None), "interrupted");
        assert_eq!(classify_recording(0, &exit, None), "failed");
        exit.forced_stop_reason = None;
        exit.wait_error = Some("进程等待失败".to_string());
        assert_eq!(classify_recording(1024, &exit, None), "interrupted");
        assert_eq!(
            classify_recording(1024, &recording_exit(true, false), None),
            "interrupted"
        );
    }

    #[test]
    fn classifies_natural_offline_exit_as_completed() {
        assert_eq!(
            classify_recording(
                1024,
                &recording_exit(false, true),
                Some(LiveVerification::Offline)
            ),
            "completed"
        );
    }

    #[test]
    fn classifies_live_or_unverified_exit_as_interrupted() {
        assert_eq!(
            classify_recording(
                1024,
                &recording_exit(false, true),
                Some(LiveVerification::Live)
            ),
            "interrupted"
        );
        assert_eq!(
            classify_recording(
                1024,
                &recording_exit(false, true),
                Some(LiveVerification::Failed)
            ),
            "interrupted"
        );
    }

    #[test]
    fn classifies_empty_output_as_failed() {
        assert_eq!(
            classify_recording(
                0,
                &recording_exit(true, true),
                Some(LiveVerification::Offline)
            ),
            "failed"
        );
    }

    #[test]
    fn daily_schedule_triggers_once_after_local_time() {
        let now = Local.with_ymd_and_hms(2026, 8, 21, 9, 30, 0).unwrap();
        assert!(daily_schedule_is_due(&scheduled_room("09:00", None), now));
        assert!(!daily_schedule_is_due(
            &scheduled_room("09:00", Some("2026-08-21")),
            now
        ));
        assert!(!daily_schedule_is_due(&scheduled_room("10:00", None), now));
    }

    #[test]
    fn saving_past_schedule_marks_today_as_already_handled() {
        let now = Local.with_ymd_and_hms(2026, 8, 21, 9, 30, 0).unwrap();
        let past = chrono::NaiveTime::parse_from_str("09:00", "%H:%M").unwrap();
        let future = chrono::NaiveTime::parse_from_str("10:00", "%H:%M").unwrap();
        assert_eq!(
            initial_schedule_marker(past, now),
            Some("2026-08-21".to_string())
        );
        assert_eq!(initial_schedule_marker(future, now), None);
    }

    #[test]
    fn polling_is_disabled_when_auto_is_off_or_recording_is_active() {
        assert!(!should_poll_auto_room(false, false, false, true));
        assert!(!should_poll_auto_room(true, false, true, true));
        assert!(!should_poll_auto_room(true, true, false, true));
        assert!(should_poll_auto_room(true, false, false, true));
    }
}
