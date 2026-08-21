mod auto_recorder;
mod database;
mod parser;
mod recorder;
mod settings;

use auto_recorder::AutoRecorder;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveTime, Utc};
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

#[derive(Debug, PartialEq, Eq)]
enum AutoPostRecordAction {
    Disable,
    Continue,
    Preserve,
}

fn classify_recording(
    file_size: i64,
    manually_stopped: bool,
    verification: Option<LiveVerification>,
) -> &'static str {
    if file_size <= 0 {
        return "failed";
    }
    if manually_stopped {
        return "completed";
    }
    match verification {
        Some(LiveVerification::Offline) => "completed",
        Some(LiveVerification::Live | LiveVerification::Failed) | None => "interrupted",
    }
}

fn auto_post_record_action(
    task_trigger: &str,
    status: &str,
    disable_after_record: bool,
) -> AutoPostRecordAction {
    if task_trigger != "auto" {
        AutoPostRecordAction::Preserve
    } else if status == "completed" && !disable_after_record {
        AutoPostRecordAction::Continue
    } else {
        AutoPostRecordAction::Disable
    }
}

fn should_poll_auto_room(enabled: bool, expired: bool, running: bool, due: bool) -> bool {
    enabled && !expired && !running && due
}

fn monitor_until(window_hours: u64) -> String {
    (Utc::now() + ChronoDuration::hours(window_hours as i64)).to_rfc3339()
}

fn monitor_is_expired(until: Option<&str>) -> bool {
    until
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .is_none_or(|value| value.with_timezone(&Utc) <= Utc::now())
}

fn daily_schedule_is_due(room: &LiveRoom, now: DateTime<Local>) -> bool {
    let Some(daily_time) = room.auto_record_daily_time.as_deref() else {
        return false;
    };
    let Ok(daily_time) = NaiveTime::parse_from_str(daily_time, "%H:%M") else {
        return false;
    };
    let today = now.format("%Y-%m-%d").to_string();
    room.last_schedule_trigger_date.as_deref() != Some(&today) && now.time() >= daily_time
}

fn initial_schedule_marker(daily_time: NaiveTime, now: DateTime<Local>) -> Option<String> {
    (daily_time <= now.time()).then(|| now.format("%Y-%m-%d").to_string())
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

fn apply_post_recording_auto_policy(
    app: &AppHandle,
    state: &AppState,
    room_id: i64,
    task_trigger: &str,
    status: &str,
) -> Result<Option<LiveRoom>, String> {
    let app_settings = settings::load_settings();
    let current_room = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_room(room_id).map_err(|e| e.to_string())?
    };
    match auto_post_record_action(task_trigger, status, app_settings.auto_disable_after_record) {
        AutoPostRecordAction::Continue => {
            let until = monitor_until(app_settings.auto_monitor_window_hours);
            let room = {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                db.set_room_auto_record(room_id, true, Some(&until))
                    .map_err(|e| e.to_string())?;
                db.get_room(room_id).map_err(|e| e.to_string())?
            };
            state.auto_recorder.mark_immediate(room_id);
            emit_auto_recording_event(
                app,
                room.clone(),
                "enabled",
                Some("自动录制已完成，已开始新的监控窗口".to_string()),
            );
            return Ok(Some(room));
        }
        AutoPostRecordAction::Disable => {
            let (reason, message) = if status == "completed" {
                ("disabled", "自动录制已完成，本次监控已关闭")
            } else {
                ("paused", "自动录制异常结束，已暂停该房间的自动监控")
            };
            let room = {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                db.set_room_auto_record(room_id, false, None)
                    .map_err(|e| e.to_string())?;
                db.get_room(room_id).map_err(|e| e.to_string())?
            };
            state.auto_recorder.clear(room_id);
            emit_auto_recording_event(app, room.clone(), reason, Some(message.to_string()));
            return Ok(Some(room));
        }
        AutoPostRecordAction::Preserve => {}
    }
    if current_room.auto_record_enabled {
        if monitor_is_expired(current_room.auto_record_until.as_deref()) {
            let room = {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                db.set_room_auto_record(room_id, false, None)
                    .map_err(|e| e.to_string())?;
                db.get_room(room_id).map_err(|e| e.to_string())?
            };
            state.auto_recorder.clear(room_id);
            emit_auto_recording_event(
                app,
                room.clone(),
                "window_expired",
                Some("自动录制监控时间已结束".to_string()),
            );
            return Ok(Some(room));
        }
        state.auto_recorder.mark_immediate(room_id);
    }
    Ok(Some(current_room))
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
        let size = file_size(task.file_path.as_deref());
        db.mark_task_finalizing(task_id, size)
            .map_err(|e| e.to_string())?;
        let updated = db.get_task(task_id).map_err(|e| e.to_string())?;
        (task.file_path, size, task.trigger, updated)
    };
    emit_recording_event(&app, finalizing_task, None, "finalizing", None);
    #[cfg(debug_assertions)]
    if !exit.status_success || exit.wait_error.is_some() {
        eprintln!(
            "ffmpeg task {} exited (success: {}, wait_error: {:?}, stderr_lines: {})",
            task_id,
            exit.status_success,
            exit.wait_error,
            exit.stderr_tail.len()
        );
    }
    let (verification, verified_room, verification_message) = if exit.manually_stopped {
        (None, None, None)
    } else {
        match refresh_room_internal(&state, room_id).await {
            Ok(room) if room.is_live => (
                Some(LiveVerification::Live),
                Some(room),
                Some("录制进程已结束，但主播仍在直播，请检查网络后重新录制".to_string()),
            ),
            Ok(room) => (Some(LiveVerification::Offline), Some(room), None),
            Err(error) => (
                Some(LiveVerification::Failed),
                None,
                Some(format!("录制进程已结束，但无法确认直播状态: {}", error)),
            ),
        }
    };
    let status = classify_recording(original_size, exit.manually_stopped, verification);
    let mut final_path = original_path;
    let mut final_size = original_size;
    let mut message = match status {
        "completed" if exit.manually_stopped => Some("录制已停止".to_string()),
        "completed" => Some("直播已结束，录制已完成".to_string()),
        "failed" => Some("录制已结束，但没有生成有效文件".to_string()),
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
    let final_task = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.finish_task(task_id, status, final_path.as_deref(), final_size)
            .map_err(|e| e.to_string())?;
        db.get_task(task_id).map_err(|e| e.to_string())?
    };
    let policy_room =
        apply_post_recording_auto_policy(&app, &state, room_id, &task_trigger, status)?;
    let reason = match status {
        "completed" if exit.manually_stopped => "manual_stop",
        "completed" => "stream_ended",
        "interrupted" => "interrupted",
        _ => "failed",
    };
    emit_recording_event(
        &app,
        final_task,
        policy_room.or(verified_room),
        reason,
        message,
    );
    Ok(())
}

async fn start_record_from_info(
    app: &AppHandle,
    state: &AppState,
    room_id: i64,
    info: &LiveInfo,
    trigger: &str,
) -> Result<RecordTask, String> {
    let _start_guard = state.start_lock.lock().await;
    let (task_id, output_str, app_settings) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        if db
            .has_running_tasks_for_room(room_id)
            .map_err(|e| e.to_string())?
        {
            return Err("该直播间已经在录制或结束处理中".to_string());
        }
        let room = db.get_room(room_id).map_err(|e| e.to_string())?;
        if trigger == "auto"
            && (!room.auto_record_enabled || monitor_is_expired(room.auto_record_until.as_deref()))
        {
            return Err("自动录制监控已关闭".to_string());
        }
        let task_id = db.add_task(room_id, trigger).map_err(|e| e.to_string())?;
        let timestamp = Local::now().format("%Y%m%d_%H%M%S");
        let filename = format!("{}_{}_{}.flv", room.anchor_name, room.room_id, timestamp);
        let recordings_dir = get_recordings_dir()?;
        let output_path = std::path::Path::new(&recordings_dir).join(filename);
        let output_str = output_path.to_string_lossy().to_string();
        db.update_task_status_and_path(task_id, "recording", Some(&output_str))
            .map_err(|e| e.to_string())?;
        (task_id, output_str, settings::load_settings())
    };
    let exit_app = app.clone();
    let start_result = state.recorder.start_record(
        task_id,
        &info.stream_url,
        &output_str,
        &app_settings.proxy,
        move |exit| handle_recording_exit(exit_app, task_id, room_id, exit),
    );
    if let Err(error) = start_result {
        let (task, room) = {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            db.finish_task(task_id, "failed", Some(&output_str), 0)
                .map_err(|e| e.to_string())?;
            (
                db.get_task(task_id).map_err(|e| e.to_string())?,
                db.get_room(room_id).ok(),
            )
        };
        emit_recording_event(
            app,
            task,
            room,
            "failed",
            Some(format!("启动录制失败: {}", error)),
        );
        return Err(error);
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.get_task(task_id).map_err(|e| e.to_string())
}

fn pause_auto_recording(
    app: &AppHandle,
    state: &AppState,
    room_id: i64,
    message: String,
) -> Result<LiveRoom, String> {
    let room = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.set_room_auto_record(room_id, false, None)
            .map_err(|e| e.to_string())?;
        db.get_room(room_id).map_err(|e| e.to_string())?
    };
    state.auto_recorder.clear(room_id);
    emit_auto_recording_event(app, room.clone(), "paused", Some(message));
    Ok(room)
}

async fn check_auto_room(app: &AppHandle, room: LiveRoom) -> Result<(), String> {
    let state = app.state::<AppState>();
    let app_settings = settings::load_settings();
    let url = format!("https://live.douyin.com/{}", room.room_id);
    let info = match state.parser.parse_douyin_url(&url, &app_settings).await {
        Ok(info) => info,
        Err(error) => {
            state.auto_recorder.mark_failure(
                room.id,
                app_settings.auto_check_interval_secs,
                error.is_rate_limited(),
            );
            return Ok(());
        }
    };
    let current_room = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_room(room.id).map_err(|e| e.to_string())?
    };
    if !current_room.auto_record_enabled {
        state.auto_recorder.clear(room.id);
        return Ok(());
    }
    if monitor_is_expired(current_room.auto_record_until.as_deref()) {
        let expired_room = {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            db.set_room_auto_record(room.id, false, None)
                .map_err(|e| e.to_string())?;
            db.get_room(room.id).map_err(|e| e.to_string())?
        };
        state.auto_recorder.clear(room.id);
        emit_auto_recording_event(
            app,
            expired_room,
            "window_expired",
            Some("自动录制监控时间已结束".to_string()),
        );
        return Ok(());
    }
    let updated_room = apply_live_info(&state, room.id, &info)?;
    if !info.is_live {
        state
            .auto_recorder
            .mark_success(room.id, app_settings.auto_check_interval_secs);
        emit_auto_recording_event(app, updated_room, "enabled", None);
        return Ok(());
    }
    match start_record_from_info(app, &state, room.id, &info, "auto").await {
        Ok(task) => {
            state.auto_recorder.clear(room.id);
            emit_recording_event(
                app,
                task,
                Some(updated_room),
                "auto_started",
                Some("检测到主播开播，已开始自动录制".to_string()),
            );
        }
        Err(error) => {
            let (already_running, current_room) = {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                (
                    db.has_running_tasks_for_room(room.id)
                        .map_err(|e| e.to_string())?,
                    db.get_room(room.id).map_err(|e| e.to_string())?,
                )
            };
            if already_running || !current_room.auto_record_enabled {
                state.auto_recorder.clear(room.id);
            } else if monitor_is_expired(current_room.auto_record_until.as_deref()) {
                let expired_room = {
                    let db = state.db.lock().map_err(|e| e.to_string())?;
                    db.set_room_auto_record(room.id, false, None)
                        .map_err(|e| e.to_string())?;
                    db.get_room(room.id).map_err(|e| e.to_string())?
                };
                state.auto_recorder.clear(room.id);
                emit_auto_recording_event(
                    app,
                    expired_room,
                    "window_expired",
                    Some("自动录制监控时间已结束".to_string()),
                );
            } else {
                pause_auto_recording(
                    app,
                    &state,
                    room.id,
                    format!("自动录制启动失败，已暂停监控: {}", error),
                )?;
            }
        }
    }
    Ok(())
}

fn process_auto_deadlines_and_schedules(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let now = Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let rooms = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_all_rooms().map_err(|e| e.to_string())?
    };
    for room in rooms {
        let running = {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            db.has_running_tasks_for_room(room.id)
                .map_err(|e| e.to_string())?
        };
        if room.auto_record_enabled
            && monitor_is_expired(room.auto_record_until.as_deref())
            && !running
        {
            let expired_room = {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                db.set_room_auto_record(room.id, false, None)
                    .map_err(|e| e.to_string())?;
                db.get_room(room.id).map_err(|e| e.to_string())?
            };
            state.auto_recorder.clear(room.id);
            emit_auto_recording_event(
                app,
                expired_room,
                "window_expired",
                Some("自动录制监控时间已结束".to_string()),
            );
        }
        if daily_schedule_is_due(&room, now) {
            let app_settings = settings::load_settings();
            let until = monitor_until(app_settings.auto_monitor_window_hours);
            let scheduled_room = {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                db.trigger_room_auto_schedule(room.id, &until, &today)
                    .map_err(|e| e.to_string())?;
                db.get_room(room.id).map_err(|e| e.to_string())?
            };
            state.auto_recorder.mark_immediate(room.id);
            emit_auto_recording_event(
                app,
                scheduled_room,
                "schedule_triggered",
                Some("每日定时已触发，开始监控直播状态".to_string()),
            );
        }
    }
    Ok(())
}

fn next_due_auto_room(app: &AppHandle) -> Result<Option<LiveRoom>, String> {
    let state = app.state::<AppState>();
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let rooms = db.get_all_rooms().map_err(|e| e.to_string())?;
    for room in rooms {
        let expired = monitor_is_expired(room.auto_record_until.as_deref());
        let running = db
            .has_running_tasks_for_room(room.id)
            .map_err(|e| e.to_string())?;
        let due = state.auto_recorder.is_due(room.id);
        if should_poll_auto_room(room.auto_record_enabled, expired, running, due) {
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

fn reconcile_auto_state_on_startup(
    db: &Database,
    app_settings: &AppSettings,
) -> Result<(), String> {
    let now = Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let rooms = db.get_all_rooms().map_err(|e| e.to_string())?;
    for room in rooms {
        if room.auto_record_enabled {
            if room.auto_record_until.is_none() {
                let until = monitor_until(app_settings.auto_monitor_window_hours);
                db.set_room_auto_record(room.id, true, Some(&until))
                    .map_err(|e| e.to_string())?;
            } else if monitor_is_expired(room.auto_record_until.as_deref()) {
                db.set_room_auto_record(room.id, false, None)
                    .map_err(|e| e.to_string())?;
            }
        }
        if daily_schedule_is_due(&room, now) {
            let until = monitor_until(app_settings.auto_monitor_window_hours);
            db.trigger_room_auto_schedule(room.id, &until, &today)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
fn get_settings_cmd() -> Result<AppSettings, String> {
    Ok(settings::load_settings())
}

#[tauri::command]
fn save_settings_cmd(new_settings: AppSettings) -> Result<(), String> {
    settings::save_settings(&new_settings)
}

#[tauri::command]
fn migrate_db_cmd(new_path: String) -> Result<String, String> {
    settings::migrate_db(&new_path)
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
    let until = enabled.then(|| monitor_until(settings::load_settings().auto_monitor_window_hours));
    let room = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_room(room_id).map_err(|e| e.to_string())?;
        db.set_room_auto_record(room_id, enabled, until.as_deref())
            .map_err(|e| e.to_string())?;
        db.get_room(room_id).map_err(|e| e.to_string())?
    };
    if enabled {
        state.auto_recorder.mark_immediate(room_id);
    } else {
        state.auto_recorder.clear(room_id);
    }
    emit_auto_recording_event(
        &app,
        room.clone(),
        if enabled { "enabled" } else { "disabled" },
        Some(if enabled {
            "自动录制已开启，正在检查直播状态".to_string()
        } else {
            "自动录制已关闭；正在进行的录制不会停止".to_string()
        }),
    );
    Ok(room)
}

#[tauri::command]
fn set_room_auto_schedule(
    app: AppHandle,
    state: State<AppState>,
    room_id: i64,
    daily_time: Option<String>,
) -> Result<LiveRoom, String> {
    let room = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let existing = db.get_room(room_id).map_err(|e| e.to_string())?;
        match daily_time.as_deref() {
            Some(value) => {
                let parsed = NaiveTime::parse_from_str(value, "%H:%M")
                    .map_err(|_| "定时时间格式无效，请使用 HH:mm".to_string())?;
                let marker = if existing.auto_record_daily_time.as_deref() == Some(value) {
                    existing.last_schedule_trigger_date
                } else {
                    initial_schedule_marker(parsed, Local::now())
                };
                db.set_room_auto_schedule(room_id, Some(value), marker.as_deref())
                    .map_err(|e| e.to_string())?;
            }
            None => db
                .set_room_auto_schedule(room_id, None, None)
                .map_err(|e| e.to_string())?,
        }
        db.get_room(room_id).map_err(|e| e.to_string())?
    };
    let scheduled = daily_time.is_some();
    emit_auto_recording_event(
        &app,
        room.clone(),
        if scheduled {
            "scheduled"
        } else {
            "schedule_cancelled"
        },
        Some(if scheduled {
            format!("已设置每天 {} 开启自动录制", daily_time.unwrap_or_default())
        } else {
            "已取消每日定时".to_string()
        }),
    );
    Ok(room)
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
    start_record_from_info(&app, &state, room_id, &info, "manual").await
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
        ])
        .run(tauri::generate_context!())
        .expect("应用启动失败");
}

#[cfg(test)]
mod tests {
    use super::{
        auto_post_record_action, classify_recording, daily_schedule_is_due,
        initial_schedule_marker, should_poll_auto_room, AutoPostRecordAction, LiveVerification,
    };
    use crate::database::LiveRoom;
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
        }
    }

    #[test]
    fn classifies_natural_offline_exit_as_completed() {
        assert_eq!(
            classify_recording(1024, false, Some(LiveVerification::Offline)),
            "completed"
        );
    }

    #[test]
    fn classifies_live_or_unverified_exit_as_interrupted() {
        assert_eq!(
            classify_recording(1024, false, Some(LiveVerification::Live)),
            "interrupted"
        );
        assert_eq!(
            classify_recording(1024, false, Some(LiveVerification::Failed)),
            "interrupted"
        );
    }

    #[test]
    fn classifies_empty_output_as_failed() {
        assert_eq!(
            classify_recording(0, true, Some(LiveVerification::Offline)),
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
    fn automatic_recording_post_action_respects_trigger_and_setting() {
        assert_eq!(
            auto_post_record_action("auto", "completed", true),
            AutoPostRecordAction::Disable
        );
        assert_eq!(
            auto_post_record_action("auto", "completed", false),
            AutoPostRecordAction::Continue
        );
        assert_eq!(
            auto_post_record_action("auto", "interrupted", false),
            AutoPostRecordAction::Disable
        );
        assert_eq!(
            auto_post_record_action("manual", "completed", true),
            AutoPostRecordAction::Preserve
        );
    }

    #[test]
    fn polling_is_disabled_when_auto_is_off_or_recording_is_active() {
        assert!(!should_poll_auto_room(false, false, false, true));
        assert!(!should_poll_auto_room(true, false, true, true));
        assert!(!should_poll_auto_room(true, true, false, true));
        assert!(should_poll_auto_room(true, false, false, true));
    }
}
