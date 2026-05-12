mod database;
mod parser;
mod recorder;
mod settings;

use database::{Database, LiveRoom, RecordTask};
use recorder::Recorder;
use settings::AppSettings;
use std::sync::Mutex;
use tauri::State;

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
    recorder: Mutex<Recorder>,
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
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建录制目录失败: {}", e))?;
    Ok(dir.to_string_lossy().to_string())
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
    let info = parser::parse_douyin_url(&url, &app_settings).await?;

    let db = state.db.lock().map_err(|e| e.to_string())?;
    let room_id = db.add_room_full(
        &info.platform,
        &info.room_id,
        &info.anchor_name,
        &info.room_title,
        &info.cover_url,
        &info.avatar_url,
        info.is_live,
    ).map_err(|e| e.to_string())?;

    let rooms = db.get_all_rooms().map_err(|e| e.to_string())?;
    rooms.into_iter()
        .find(|r| r.id == room_id)
        .ok_or_else(|| "添加失败".to_string())
}

#[tauri::command]
async fn refresh_room(state: State<'_, AppState>, room_id: i64) -> Result<LiveRoom, String> {
    let (douyin_url, old_anchor_name, old_room_title, old_cover_url, old_avatar_url) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let rooms = db.get_all_rooms().map_err(|e| e.to_string())?;
        let room = rooms.into_iter().find(|r| r.id == room_id)
            .ok_or_else(|| "房间不存在".to_string())?;
        (
            format!("https://live.douyin.com/{}", room.room_id),
            room.anchor_name,
            room.room_title,
            room.cover_url,
            room.avatar_url,
        )
    };

    let app_settings = settings::load_settings();
    let info = parser::parse_douyin_url(&douyin_url, &app_settings).await?;

    // Preserve existing values when API returns empty (e.g. stream is offline)
    let anchor_name = if info.anchor_name.is_empty() { old_anchor_name } else { info.anchor_name };
    let room_title = if info.room_title.is_empty() { old_room_title } else { info.room_title };
    let cover_url = if info.cover_url.is_empty() { old_cover_url } else { info.cover_url };
    let avatar_url = if info.avatar_url.is_empty() { old_avatar_url } else { info.avatar_url };

    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.update_room_live_status(
        room_id,
        &anchor_name,
        &room_title,
        &cover_url,
        &avatar_url,
        info.is_live,
    ).map_err(|e| e.to_string())?;

    let rooms = db.get_all_rooms().map_err(|e| e.to_string())?;
    rooms.into_iter()
        .find(|r| r.id == room_id)
        .ok_or_else(|| "刷新失败".to_string())
}

#[tauri::command]
fn get_room_task_count(state: State<AppState>, room_id: i64) -> Result<i64, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.count_tasks_for_room(room_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_room(state: State<AppState>, id: i64, cascade: Option<bool>) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    if cascade == Some(true) {
        db.delete_room_cascade(id).map_err(|e| e.to_string())
    } else {
        let task_count = db.count_tasks_for_room(id).map_err(|e| e.to_string())?;
        if task_count > 0 {
            return Err(format!("该房间还有 {} 个录制任务，请确认删除", task_count));
        }
        db.delete_room(id).map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn get_tasks(state: State<AppState>) -> Result<Vec<RecordTask>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.get_all_tasks().map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_record(state: State<'_, AppState>, room_id: i64) -> Result<RecordTask, String> {
    let (douyin_url, task_id, output_str) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let rooms = db.get_all_rooms().map_err(|e| e.to_string())?;
        let room = rooms.iter().find(|r| r.id == room_id)
            .ok_or_else(|| "房间不存在".to_string())?
            .clone();

        let douyin_url = format!("https://live.douyin.com/{}", room.room_id);
        let task_id = db.add_task(room_id).map_err(|e| e.to_string())?;

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let filename = format!("{}_{}_{}.flv", room.anchor_name, room.room_id, timestamp);
        let recordings_dir = get_recordings_dir()?;
        let output_path = std::path::Path::new(&recordings_dir).join(&filename);
        let output_str = output_path.to_string_lossy().to_string();

        db.update_task_status_and_path(task_id, "recording", Some(&output_str))
            .map_err(|e| e.to_string())?;

        (douyin_url, task_id, output_str)
    };

    let app_settings = settings::load_settings();
    let info = parser::parse_douyin_url(&douyin_url, &app_settings).await?;

    if !info.is_live {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.update_task_status(task_id, "waiting").map_err(|e| e.to_string())?;
        return Err("主播未开播，任务已创建但未开始录制".to_string());
    }

    {
        let recorder = state.recorder.lock().map_err(|e| e.to_string())?;
        recorder.start_record(task_id, &info.stream_url, &output_str, &app_settings.proxy)?;
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    let tasks = db.get_all_tasks().map_err(|e| e.to_string())?;
    tasks.into_iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| "创建任务失败".to_string())
}

#[tauri::command]
fn delete_task(state: State<AppState>, id: i64) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.delete_task(id).map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_record(state: State<AppState>, task_id: i64) -> Result<RecordTask, String> {
    {
        let recorder = state.recorder.lock().map_err(|e| e.to_string())?;
        recorder.stop_record(task_id)?;
    }

    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.update_task_status(task_id, "completed").map_err(|e| e.to_string())?;
    }

    let app_settings = settings::load_settings();
    if app_settings.auto_convert_mp4 {
        let file_path = {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            db.get_all_tasks().map_err(|e| e.to_string())?
                .into_iter()
                .find(|t| t.id == task_id)
                .and_then(|t| t.file_path)
        };
        if let Some(ref fp) = file_path {
            if fp.ends_with(".flv") {
                let mp4_path = fp.trim_end_matches(".flv").to_string() + ".mp4";
                let mut cmd = std::process::Command::new(resolve_ffmpeg_path());
                cmd.args(["-y", "-i", fp, "-c", "copy", &mp4_path]);
                #[cfg(windows)]
                cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
                if let Ok(output) = cmd.output()
                {
                    if output.status.success() && std::path::Path::new(&mp4_path).exists() {
                        let file_size = std::fs::metadata(&mp4_path).map(|m| m.len() as i64).unwrap_or(0);
                        let db = state.db.lock().map_err(|e| e.to_string())?;
                        let _ = db.update_task_status_and_path(task_id, "completed", Some(&mp4_path));
                        let _ = db.update_task_file_size(task_id, file_size);
                        drop(db);
                        let _ = std::fs::remove_file(fp);
                    }
                }
            }
        }
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.get_all_tasks().map_err(|e| e.to_string())?
        .into_iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| "获取任务失败".to_string())
}

#[tauri::command]
fn convert_to_mp4(state: State<AppState>, task_id: i64) -> Result<String, String> {
    let file_path = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let tasks = db.get_all_tasks().map_err(|e| e.to_string())?;
        let task = tasks.iter().find(|t| t.id == task_id)
            .ok_or_else(|| "任务不存在".to_string())?;
        let fp = task.file_path.clone()
            .ok_or_else(|| "文件路径为空".to_string())?;
        if !fp.ends_with(".flv") {
            return Err("文件不是 FLV 格式，无需转换".to_string());
        }
        fp
    };

    let mp4_path = file_path.trim_end_matches(".flv").to_string() + ".mp4";

    let mut cmd = std::process::Command::new(resolve_ffmpeg_path());
    cmd.args(["-y", "-i", &file_path, "-c", "copy", &mp4_path]);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let output = cmd
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "ffmpeg 未找到".to_string()
            } else {
                format!("转换失败: {}", e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg 转换失败: {}", stderr.chars().take(200).collect::<String>()));
    }

    if !std::path::Path::new(&mp4_path).exists() {
        return Err("转换完成但未找到输出文件".to_string());
    }

    let file_size = std::fs::metadata(&mp4_path).map(|m| m.len() as i64).unwrap_or(0);

    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.update_task_status_and_path(task_id, "completed", Some(&mp4_path))
            .map_err(|e| e.to_string())?;
        db.update_task_file_size(task_id, file_size)
            .map_err(|e| e.to_string())?;
    }

    let _ = std::fs::remove_file(&file_path);
    Ok(mp4_path)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db_path = settings::get_db_path();
    let db = Database::new(&db_path).expect("数据库初始化失败");
    let ffmpeg_path = resolve_ffmpeg_path();
    let recorder = Recorder::new(ffmpeg_path.clone());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            db: Mutex::new(db),
            recorder: Mutex::new(recorder),
        })
        .invoke_handler(tauri::generate_handler![
            get_rooms,
            add_room,
            refresh_room,
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
