mod auto_policy;
mod auto_recorder;
mod database;
mod migration;
mod parser;
mod recorder;
mod settings;
mod updater;

#[cfg(test)]
use auto_policy::{daily_schedule_is_due, initial_schedule_marker};
use auto_policy::{monitor_is_expired, monitor_until};
use auto_policy::{AutoMonitorMode, PostRecordAction, TickAction};
use auto_recorder::AutoRecorder;
use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use database::{Database, LiveRoom, RecordTask};
use parser::{DouyinParser, LiveInfo};
use recorder::{Recorder, RecordingExit};
use serde::Serialize;
use settings::AppSettings;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex as AsyncMutex;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

fn resolve_ffmpeg_path() -> String {
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

struct AppState {
    db: Mutex<Database>,
    recorder: Recorder,
    parser: DouyinParser,
    auto_recorder: AutoRecorder,
    start_lock: AsyncMutex<()>,
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

fn get_recordings_dir() -> Result<String, String> {
    let settings = settings::load_settings();
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
    let app_settings = settings::load_settings();
    let info = state
        .parser
        .parse_douyin_url(&douyin_url, &app_settings)
        .await
        .map_err(|e| e.to_string())?;
    apply_live_info(state, room_id, &info)
}

fn remux_flv_to_mp4(file_path: &str) -> Result<(String, i64), String> {
    if !file_path.ends_with(".flv") {
        return Err("文件不是 FLV 格式，无需转换".to_string());
    }
    let mp4_path = file_path.trim_end_matches(".flv").to_string() + ".mp4";
    let mut cmd = std::process::Command::new(resolve_ffmpeg_path());
    cmd.args(["-y", "-i", file_path, "-c", "copy", &mp4_path]);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "ffmpeg 未找到".to_string()
        } else {
            format!("转换失败: {}", e)
        }
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffmpeg 转换失败: {}",
            stderr.chars().take(200).collect::<String>()
        ));
    }
    if !std::path::Path::new(&mp4_path).exists() {
        return Err("转换完成但未找到输出文件".to_string());
    }
    let size = file_size(Some(&mp4_path));
    std::fs::remove_file(file_path).map_err(|e| format!("MP4 已生成，但删除 FLV 失败: {}", e))?;
    Ok((mp4_path, size))
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
    let settings = settings::load_settings();
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
    let state = app.state::<AppState>();
    let (original_path, original_size, task_trigger, finalizing_task) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task = db.get_task(task_id).map_err(|e| e.to_string())?;
        if task.status != "recording" {
            return Ok(());
        }
        state.auto_recorder.mark_recording_ended(room_id);
        let size = file_size(task.file_path.as_deref());
        db.mark_task_finalizing(task_id, size)
            .map_err(|e| e.to_string())?;
        let updated = db.get_task(task_id).map_err(|e| e.to_string())?;
        (task.file_path, size, task.trigger, updated)
    };
    emit_recording_event(&app, finalizing_task, None, "finalizing", None);
    #[cfg(debug_assertions)]
    {
        let reason = if exit.forced_stop_reason.is_some() {
            "manual_stop_forced"
        } else if exit.manually_stopped && exit.stopped_cleanly() {
            "manual_stop"
        } else if exit.manually_stopped {
            "manual_stop_failed"
        } else if !exit.stopped_cleanly() {
            "process_error"
        } else {
            "stream_exit"
        };
        eprintln!(
            "ffmpeg task {} exited (reason: {}, success: {}, exit_code: {:?}, forced_stop: {:?}, wait_error: {:?}, stderr_lines: {})",
            task_id,
            reason,
            exit.status_success,
            exit.exit_code,
            exit.forced_stop_reason,
            exit.wait_error,
            exit.stderr_tail.len()
        );
    }
    let mut rate_limited = false;
    let (verification, verification_message) = if exit.manually_stopped {
        (None, None)
    } else {
        let room = {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            db.get_room(room_id).map_err(|e| e.to_string())?
        };
        let url = format!("https://live.douyin.com/{}", room.room_id);
        let result = state
            .parser
            .parse_douyin_url(&url, &settings::load_settings())
            .await;
        match result {
            Ok(info) => {
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
                (
                    Some(LiveVerification::Failed),
                    Some(format!("录制进程已结束，但无法确认直播状态: {}", error)),
                )
            }
        }
    };
    let status = classify_recording(original_size, &exit, verification);
    let mut final_path = original_path;
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
    if status == "completed"
        && settings::load_settings().auto_convert_mp4
        && final_path
            .as_deref()
            .is_some_and(|path| path.ends_with(".flv"))
    {
        let path = final_path.clone().unwrap_or_default();
        match tokio::task::spawn_blocking(move || remux_flv_to_mp4(&path)).await {
            Ok(Ok((mp4_path, mp4_size))) => {
                final_path = Some(mp4_path);
                final_size = mp4_size;
            }
            Ok(Err(error)) => {
                message = Some(format!(
                    "{}；自动转换 MP4 失败，已保留 FLV: {}",
                    message.unwrap_or_else(|| "录制已完成".to_string()),
                    error
                ));
            }
            Err(error) => {
                message = Some(format!(
                    "{}；自动转换任务异常，已保留 FLV: {}",
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
    let (auto_reason, auto_message) = apply_post_recording_auto_policy(
        &state,
        &mut room,
        &task_trigger,
        status,
        &exit,
        verification,
        rate_limited,
    );
    let (final_task, room) = db
        .finish_task_and_automation(task_id, status, final_path.as_deref(), final_size, &room)
        .map_err(|e| e.to_string())?;
    emit_auto_recording_event(&app, room.clone(), auto_reason, auto_message);
    let reason = match status {
        "completed" if exit.manually_stopped => "manual_stop",
        "completed" => "stream_ended",
        "interrupted" => "interrupted",
        _ => "failed",
    };
    emit_recording_event(&app, final_task, Some(room), reason, message);
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
    let _start_guard = state.start_lock.lock().await;
    // Register the process before a stop command can observe the new task.
    let db = state
        .db
        .lock()
        .map_err(|e| StartRecordError::Fatal(e.to_string()))?;
    let fatal = |e: rusqlite::Error| StartRecordError::Fatal(e.to_string());
    if db.has_running_tasks_for_room(room_id).map_err(fatal)? {
        return Err(StartRecordError::AlreadyRunning);
    }
    let mut room = db.get_room(room_id).map_err(fatal)?;
    if trigger == "auto"
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
    let recordings_dir = get_recordings_dir().map_err(StartRecordError::Fatal)?;
    let task_id = db.add_task(room_id, trigger).map_err(fatal)?;
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    // Task id also prevents a quick retry from overwriting an earlier partial file.
    let filename = format!(
        "{}_{}_{}_{}.flv",
        room.anchor_name, room.room_id, timestamp, task_id
    );
    let output_str = std::path::Path::new(&recordings_dir)
        .join(filename)
        .to_string_lossy()
        .to_string();
    db.update_task_status_and_path(task_id, "recording", Some(&output_str))
        .map_err(fatal)?;
    let app_settings = settings::load_settings();
    let exit_app = app.clone();
    let start_result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_str)
        .map_err(|error| format!("无法创建录制文件: {}", error))
        .and_then(|file| {
            drop(file);
            state.recorder.start_record(
                task_id,
                &info.stream_url,
                &output_str,
                &app_settings.proxy,
                move |exit| handle_recording_exit(exit_app, task_id, room_id, exit),
            )
        });
    if let Err(error) = start_result {
        db.finish_task(task_id, "failed", Some(&output_str), 0)
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
    state.auto_recorder.mark_recording_started(room_id);
    room.auto_record_error = None;
    room.auto_record_retry_at = None;
    let room = db.save_room_automation(&room).map_err(fatal)?;
    let task = db.get_task(task_id).map_err(fatal)?;
    if trigger == "auto" {
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
    let state = app.state::<AppState>();
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut room = db.get_room(snapshot.id).map_err(|e| e.to_string())?;
    let running = db
        .has_running_tasks_for_room(room.id)
        .map_err(|e| e.to_string())?;
    if !auto_policy::accepts_check(&room, snapshot.auto_record_revision, running, Utc::now()) {
        return Ok(());
    }
    let settings = settings::load_settings();
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
    let settings = settings::load_settings();
    let url = format!("https://live.douyin.com/{}", snapshot.room_id);
    let info = match state.parser.parse_douyin_url(&url, &settings).await {
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
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let room = db.get_room(snapshot.id).map_err(|e| e.to_string())?;
        let running = db
            .has_running_tasks_for_room(room.id)
            .map_err(|e| e.to_string())?;
        if !auto_policy::accepts_check(&room, snapshot.auto_record_revision, running, Utc::now()) {
            return Ok(());
        }
        let mut room = apply_live_info_in_db(&db, room.id, &info)?;
        if !info.is_live {
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
    let settings = settings::load_settings();
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let now = Local::now();
    for mut room in db.get_all_rooms().map_err(|e| e.to_string())? {
        let running = db
            .has_running_tasks_for_room(room.id)
            .map_err(|e| e.to_string())?;
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
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let now = Utc::now();
    for room in db.get_all_rooms().map_err(|e| e.to_string())? {
        let running = db
            .has_running_tasks_for_room(room.id)
            .map_err(|e| e.to_string())?;
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
            #[cfg(debug_assertions)]
            eprintln!("auto recorder schedule tick failed: {}", error);
        }
        match next_due_auto_room(&app) {
            Ok(Some(room)) => {
                let room_id = room.id;
                if let Err(error) = check_auto_room(&app, room).await {
                    let state = app.state::<AppState>();
                    state.auto_recorder.mark_failure(
                        room_id,
                        settings::load_settings().auto_check_interval_secs,
                        false,
                    );
                    #[cfg(debug_assertions)]
                    eprintln!("auto recorder room check failed: {}", error);
                }
            }
            Ok(None) => {}
            Err(error) => {
                #[cfg(debug_assertions)]
                eprintln!("auto recorder selection failed: {}", error);
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
    let mut current_settings = settings::load_settings();
    current_settings.db_path = db.path().to_string_lossy().to_string();
    Ok(current_settings)
}

#[tauri::command]
fn save_settings_cmd(
    state: State<AppState>,
    new_settings: AppSettings,
) -> Result<AppSettings, String> {
    migration::save_settings(&state, new_settings, &settings::settings_path())
}

#[tauri::command]
async fn migrate_db_cmd(app: AppHandle, new_path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
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
    let app_settings = settings::load_settings();
    updater::check_for_update(&current_version, &app_settings).await
}

#[tauri::command]
fn get_rooms(state: State<AppState>) -> Result<Vec<LiveRoom>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.get_all_rooms().map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_room(state: State<'_, AppState>, url: String) -> Result<LiveRoom, String> {
    let app_settings = settings::load_settings();
    let info = state
        .parser
        .parse_douyin_url(&url, &app_settings)
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
    refresh_room_internal(&state, room_id).await
}

#[tauri::command]
fn set_room_auto_record(
    app: AppHandle,
    state: State<AppState>,
    room_id: i64,
    enabled: bool,
) -> Result<LiveRoom, String> {
    let settings = settings::load_settings();
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
    let settings = settings::load_settings();
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
fn delete_room(state: State<AppState>, id: i64, cascade: Option<bool>) -> Result<(), String> {
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        if db
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
async fn start_record(
    app: AppHandle,
    state: State<'_, AppState>,
    room_id: i64,
) -> Result<RecordTask, String> {
    let douyin_url = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let room = db.get_room(room_id).map_err(|e| e.to_string())?;
        format!("https://live.douyin.com/{}", room.room_id)
    };
    let app_settings = settings::load_settings();
    let info = state
        .parser
        .parse_douyin_url(&douyin_url, &app_settings)
        .await
        .map_err(|e| e.to_string())?;
    apply_live_info(&state, room_id, &info)?;
    if !info.is_live {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task_id = db.add_task(room_id, "manual").map_err(|e| e.to_string())?;
        db.update_task_status(task_id, "waiting")
            .map_err(|e| e.to_string())?;
        return Err("主播未开播，任务已创建但未开始录制".to_string());
    }
    start_record_from_info(&app, &state, room_id, &info, "manual", None)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_task(state: State<AppState>, id: i64) -> Result<(), String> {
    if state.recorder.is_active(id) {
        return Err("该任务正在录制或结束处理中，请先停止录制".to_string());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let task = db.get_task(id).map_err(|e| e.to_string())?;
    if task.status == "recording" || task.status == "finalizing" {
        return Err("该任务正在录制或结束处理中，请先停止录制".to_string());
    }
    db.delete_task(id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn stop_record(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: i64,
) -> Result<RecordTask, String> {
    {
        let _start_guard = state.start_lock.lock().await;
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task = db.get_task(task_id).map_err(|e| e.to_string())?;
        let mut room = db.get_room(task.room_id).map_err(|e| e.to_string())?;
        if auto_policy::disable_for_manual_stop(&mut room, &task.status, Utc::now()) {
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
fn convert_to_mp4(state: State<AppState>, task_id: i64) -> Result<String, String> {
    let task = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_task(task_id).map_err(|e| e.to_string())?
    };
    if task.status == "recording" || task.status == "finalizing" {
        return Err("录制尚未结束，暂时不能转换".to_string());
    }
    let file_path = task
        .file_path
        .as_deref()
        .ok_or_else(|| "文件路径为空".to_string())?;
    let (mp4_path, size) = remux_flv_to_mp4(file_path)?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.finish_task(task_id, &task.status, Some(&mp4_path), size)
        .map_err(|e| e.to_string())?;
    Ok(mp4_path)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db_path = settings::get_db_path();
    let db = Database::new(&db_path).expect("数据库初始化失败");
    db.reconcile_incomplete_tasks()
        .expect("修复未完成录制任务失败");
    let app_settings = settings::load_settings();
    reconcile_auto_state_on_startup(&db, &app_settings).expect("修复自动录制状态失败");
    let recorder = Recorder::new(resolve_ffmpeg_path());
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            db: Mutex::new(db),
            recorder,
            parser: DouyinParser::new(),
            auto_recorder: AutoRecorder::new(),
            start_lock: AsyncMutex::new(()),
        })
        .setup(|app| {
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(auto_record_loop(app_handle));
            Ok(())
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
            get_settings_cmd,
            save_settings_cmd,
            migrate_db_cmd,
            check_for_update,
        ])
        .run(tauri::generate_context!())
        .expect("应用启动失败");
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
