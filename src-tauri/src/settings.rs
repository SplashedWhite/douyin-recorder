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
    pub auto_check_interval_secs: u64,
    pub auto_monitor_window_hours: u64,
    pub auto_disable_after_record: bool,
    pub notify_updates: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        let default_recordings = if home.is_empty() {
            String::new()
        } else {
            std::path::Path::new(&home)
                .join("DouyinRecordings")
                .to_string_lossy()
                .to_string()
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
            auto_check_interval_secs: 60,
            auto_monitor_window_hours: 6,
            auto_disable_after_record: true,
            notify_updates: true,
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
    if !(10..=3600).contains(&settings.auto_check_interval_secs) {
        return Err("自动录制检测间隔必须在 10 到 3600 秒之间".to_string());
    }
    if !(1..=24).contains(&settings.auto_monitor_window_hours) {
        return Err("自动录制检测窗口必须在 1 到 24 小时之间".to_string());
    }
    let path = settings_path();
    let json =
        serde_json::to_string_pretty(settings).map_err(|e| format!("序列化设置失败: {}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("保存设置失败: {}", e))?;
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
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目标目录失败: {}", e))?;
    }

    // Copy database file
    std::fs::copy(&old_path, &new_path).map_err(|e| format!("复制数据库失败: {}", e))?;

    // Update settings
    let mut settings = load_settings();
    settings.db_path = new_path.to_string_lossy().to_string();
    save_settings(&settings)?;

    Ok(new_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::AppSettings;

    #[test]
    fn legacy_settings_receive_auto_record_defaults() {
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "proxy": "",
                "cookie": "",
                "quality": "HD1",
                "recordings_dir": "",
                "db_path": "",
                "auto_convert_mp4": false,
                "time_format_24h": true,
                "time_display_mode": "absolute"
            }"#,
        )
        .expect("deserialize legacy settings");

        assert_eq!(settings.auto_check_interval_secs, 60);
        assert_eq!(settings.auto_monitor_window_hours, 6);
        assert!(settings.auto_disable_after_record);
        assert!(settings.notify_updates);
    }
}
