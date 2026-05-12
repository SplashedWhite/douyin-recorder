use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub proxy: String,
    pub cookie: String,
    pub quality: String,
    pub recordings_dir: String,
    pub db_path: String,
    pub auto_convert_mp4: bool,
    pub time_format_24h: bool,
    pub time_display_mode: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        let default_recordings = if home.is_empty() {
            String::new()
        } else {
            std::path::Path::new(&home).join("DouyinRecordings").to_string_lossy().to_string()
        };

        AppSettings {
            proxy: String::new(),
            cookie: String::new(),
            quality: "HD1".to_string(),
            recordings_dir: default_recordings,
            db_path: String::new(),
            auto_convert_mp4: false,
            time_format_24h: true,
            time_display_mode: "absolute".to_string(),
        }
    }
}

pub fn default_db_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("无法获取用户目录");
    PathBuf::from(&home).join(".douyin-recorder")
}

pub fn default_db_path() -> PathBuf {
    default_db_dir().join("douyin_recorder.db")
}

fn settings_path() -> PathBuf {
    default_db_dir().join("settings.json")
}

pub fn get_db_path() -> PathBuf {
    let settings = load_settings();
    if settings.db_path.is_empty() {
        default_db_path()
    } else {
        PathBuf::from(&settings.db_path)
    }
}

pub fn load_settings() -> AppSettings {
    let path = settings_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => AppSettings::default(),
    }
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path();
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("序列化设置失败: {}", e))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("保存设置失败: {}", e))?;
    Ok(())
}

pub fn migrate_db(new_path: &str) -> Result<String, String> {
    let old_path = default_db_path();
    let new_path = PathBuf::from(new_path);

    if !old_path.exists() {
        return Err("当前数据库文件不存在".to_string());
    }

    // Create parent directory
    if let Some(parent) = new_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目标目录失败: {}", e))?;
    }

    // Copy database file
    std::fs::copy(&old_path, &new_path)
        .map_err(|e| format!("复制数据库失败: {}", e))?;

    // Update settings
    let mut settings = load_settings();
    settings.db_path = new_path.to_string_lossy().to_string();
    save_settings(&settings)?;

    Ok(new_path.to_string_lossy().to_string())
}
