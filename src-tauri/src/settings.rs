use serde::{Deserialize, Deserializer, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_QUALITY: &str = "ORIGIN";
pub const DEFAULT_LOG_MAX_SIZE_MIB: u64 = 5;
pub const DEFAULT_LOG_BACKUP_COUNT: usize = 4;
pub const MAX_LOG_SIZE_MIB: u64 = 1024;
pub const MAX_LOG_BACKUP_COUNT: usize = 100;

pub fn normalize_quality(quality: &str) -> &str {
    match quality.trim() {
        "ORIGIN" | "FULL_HD1" | "HD1" | "SD2" | "SD1" => quality.trim(),
        _ => DEFAULT_QUALITY,
    }
}

fn deserialize_quality<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let quality = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    Ok(normalize_quality(&quality).to_string())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    #[default]
    Exit,
    Tray,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub proxy: String,
    pub cookie: String,
    #[serde(deserialize_with = "deserialize_quality")]
    pub quality: String,
    pub recordings_dir: String,
    pub db_path: String,
    pub auto_convert_mp4: bool,
    pub segment_recording_enabled: bool,
    pub segment_duration_minutes: u32,
    pub ffmpeg_reconnect_enabled: bool,
    pub recording_recovery_enabled: bool,
    pub recording_recovery_timeout_secs: u64,
    pub ffmpeg_rw_timeout_secs: u64,
    pub ffmpeg_reconnect_max_retries: u32,
    pub ffmpeg_reconnect_delay_max_secs: u64,
    pub ffmpeg_reconnect_delay_total_max_secs: u64,
    pub time_format_24h: bool,
    pub time_display_mode: String,
    pub auto_check_interval_secs: u64,
    pub auto_monitor_window_hours: u64,
    pub auto_disable_after_record: bool,
    pub notify_updates: bool,
    pub close_behavior: CloseBehavior,
    pub log_max_size_mib: u64,
    pub log_backup_count: usize,
    pub api_log_enabled: bool,
    pub api_log_max_size_mib: u64,
    pub api_log_backup_count: usize,
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
            quality: DEFAULT_QUALITY.to_string(),
            recordings_dir: default_recordings,
            db_path: String::new(),
            auto_convert_mp4: false,
            segment_recording_enabled: false,
            segment_duration_minutes: 60,
            ffmpeg_reconnect_enabled: false,
            recording_recovery_enabled: false,
            recording_recovery_timeout_secs: 120,
            ffmpeg_rw_timeout_secs: 20,
            ffmpeg_reconnect_max_retries: 5,
            ffmpeg_reconnect_delay_max_secs: 15,
            ffmpeg_reconnect_delay_total_max_secs: 30,
            time_format_24h: true,
            time_display_mode: "absolute".to_string(),
            auto_check_interval_secs: 60,
            auto_monitor_window_hours: 6,
            auto_disable_after_record: true,
            notify_updates: true,
            close_behavior: CloseBehavior::Exit,
            log_max_size_mib: DEFAULT_LOG_MAX_SIZE_MIB,
            log_backup_count: DEFAULT_LOG_BACKUP_COUNT,
            api_log_enabled: false,
            api_log_max_size_mib: DEFAULT_LOG_MAX_SIZE_MIB,
            api_log_backup_count: DEFAULT_LOG_BACKUP_COUNT,
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

pub fn settings_path() -> PathBuf {
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
    load_settings_from(&settings_path()).unwrap_or_default()
}

pub fn load_settings_from(path: &Path) -> Result<AppSettings, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).map_err(|e| format!("读取设置失败: {}", e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppSettings::default()),
        Err(e) => Err(format!("读取设置失败: {}", e)),
    }
}

pub fn save_settings_at(settings: &AppSettings, path: &Path) -> Result<(), String> {
    if !(10..=3600).contains(&settings.recording_recovery_timeout_secs) {
        return Err("最长恢复时间必须为 10 到 3600 秒的整数".into());
    }
    settings.validate_ffmpeg()?;
    if !(1..=MAX_LOG_SIZE_MIB).contains(&settings.api_log_max_size_mib) {
        return Err(format!(
            "单个接口日志大小必须在 1 到 {MAX_LOG_SIZE_MIB} MiB 之间"
        ));
    }
    if !(1..=MAX_LOG_BACKUP_COUNT).contains(&settings.api_log_backup_count) {
        return Err(format!(
            "接口日志历史保留份数必须在 1 到 {MAX_LOG_BACKUP_COUNT} 之间"
        ));
    }
    if !(1..=MAX_LOG_SIZE_MIB).contains(&settings.log_max_size_mib) {
        return Err(format!(
            "单个日志大小必须在 1 到 {MAX_LOG_SIZE_MIB} MiB 之间"
        ));
    }
    if !(1..=MAX_LOG_BACKUP_COUNT).contains(&settings.log_backup_count) {
        return Err(format!(
            "历史日志保留份数必须在 1 到 {MAX_LOG_BACKUP_COUNT} 之间"
        ));
    }
    if settings.segment_duration_minutes == 0 {
        return Err("分段时长必须为正整数分钟".to_string());
    }
    if !(10..=3600).contains(&settings.auto_check_interval_secs) {
        return Err("自动录制检测间隔必须在 10 到 3600 秒之间".to_string());
    }
    if !(1..=24).contains(&settings.auto_monitor_window_hours) {
        return Err("自动录制检测窗口必须在 1 到 24 小时之间".to_string());
    }
    let json =
        serde_json::to_string_pretty(settings).map_err(|e| format!("序列化设置失败: {}", e))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败: {}", e))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("创建临时设置文件失败: {}", e))?;
    temporary
        .write_all(json.as_bytes())
        .map_err(|e| format!("保存设置失败: {}", e))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| format!("同步设置失败: {}", e))?;
    temporary
        .persist(path)
        .map_err(|e| format!("替换设置文件失败: {}", e.error))?;
    Ok(())
}

impl AppSettings {
    pub fn validate_ffmpeg(&self) -> Result<(), String> {
        for (label, value) in [
            ("网络读写超时", self.ffmpeg_rw_timeout_secs),
            ("单次重连等待阈值", self.ffmpeg_reconnect_delay_max_secs),
            (
                "累计重连等待阈值",
                self.ffmpeg_reconnect_delay_total_max_secs,
            ),
        ] {
            if !(1..=3600).contains(&value) {
                return Err(format!("{label}必须在 1 到 3600 秒之间"));
            }
        }
        if !(1..=100).contains(&self.ffmpeg_reconnect_max_retries) {
            return Err("重连次数限制必须在 1 到 100 次之间".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{load_settings_from, save_settings_at, AppSettings};

    #[test]
    fn recovery_defaults_save_reload_and_invalid_save_are_compatible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"quality":"HD1"}"#).unwrap();
        let mut settings = load_settings_from(&path).unwrap();
        assert!(!settings.recording_recovery_enabled);
        assert_eq!(settings.recording_recovery_timeout_secs, 120);
        settings.recording_recovery_enabled = true;
        settings.recording_recovery_timeout_secs = 15;
        save_settings_at(&settings, &path).unwrap();
        let loaded = load_settings_from(&path).unwrap();
        assert!(loaded.recording_recovery_enabled);
        assert_eq!(loaded.recording_recovery_timeout_secs, 15);
        let original = std::fs::read(&path).unwrap();
        for invalid in [0, 9, 3601, u64::MAX] {
            settings.recording_recovery_timeout_secs = invalid;
            assert!(save_settings_at(&settings, &path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        assert!(
            serde_json::from_str::<AppSettings>(r#"{"recording_recovery_timeout_secs":10.5}"#)
                .is_err()
        );
    }

    #[test]
    fn ffmpeg_settings_upgrade_persist_and_reject_invalid_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"quality":"HD1"}"#).unwrap();
        let mut settings = load_settings_from(&path).unwrap();
        assert!(!settings.ffmpeg_reconnect_enabled);
        assert_eq!(settings.ffmpeg_rw_timeout_secs, 20);
        assert_eq!(settings.ffmpeg_reconnect_max_retries, 5);
        assert_eq!(settings.ffmpeg_reconnect_delay_max_secs, 15);
        assert_eq!(settings.ffmpeg_reconnect_delay_total_max_secs, 30);
        settings.ffmpeg_rw_timeout_secs = 15;
        settings.ffmpeg_reconnect_max_retries = 3;
        settings.ffmpeg_reconnect_delay_max_secs = 10;
        settings.ffmpeg_reconnect_delay_total_max_secs = 25;
        settings.ffmpeg_reconnect_enabled = true;
        save_settings_at(&settings, &path).unwrap();
        let loaded = load_settings_from(&path).unwrap();
        assert!(loaded.ffmpeg_reconnect_enabled);
        assert_eq!(loaded.ffmpeg_rw_timeout_secs, 15);
        assert_eq!(loaded.ffmpeg_reconnect_max_retries, 3);
        assert_eq!(loaded.ffmpeg_reconnect_delay_max_secs, 10);
        assert_eq!(loaded.ffmpeg_reconnect_delay_total_max_secs, 25);
        assert_eq!(loaded.quality, "HD1");
        let original = std::fs::read(&path).unwrap();
        for (field, invalid) in [
            ("ffmpeg_rw_timeout_secs", vec![0, 3601]),
            ("ffmpeg_reconnect_max_retries", vec![0, 101]),
            ("ffmpeg_reconnect_delay_max_secs", vec![0, 3601]),
            ("ffmpeg_reconnect_delay_total_max_secs", vec![0, 3601]),
        ] {
            for value in invalid {
                let mut json = serde_json::to_value(&settings).unwrap();
                json[field] = value.into();
                let invalid: AppSettings = serde_json::from_value(json).unwrap();
                assert!(save_settings_at(&invalid, &path).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
        }
    }

    #[test]
    fn log_settings_default_roundtrip_and_invalid_saves_preserve_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"quality":"HD1"}"#).unwrap();
        let mut settings = load_settings_from(&path).unwrap();
        assert_eq!(settings.log_max_size_mib, 5);
        assert_eq!(settings.log_backup_count, 4);
        settings.log_max_size_mib = 50;
        settings.log_backup_count = 10;
        save_settings_at(&settings, &path).unwrap();
        let loaded = load_settings_from(&path).unwrap();
        assert_eq!(loaded.log_max_size_mib, 50);
        assert_eq!(loaded.log_backup_count, 10);
        assert_eq!(loaded.quality, "HD1");
        let original = std::fs::read(&path).unwrap();
        for (size, count) in [(0, 4), (1025, 4), (5, 0), (5, 101)] {
            settings.log_max_size_mib = size;
            settings.log_backup_count = count;
            assert!(save_settings_at(&settings, &path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn api_log_settings_upgrade_roundtrip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"quality":"HD1","log_max_size_mib":10}"#).unwrap();
        let mut settings = load_settings_from(&path).unwrap();
        assert!(!settings.api_log_enabled);
        assert_eq!(settings.api_log_max_size_mib, 5);
        assert_eq!(settings.api_log_backup_count, 4);
        settings.api_log_enabled = true;
        settings.api_log_max_size_mib = 20;
        settings.api_log_backup_count = 6;
        save_settings_at(&settings, &path).unwrap();
        let loaded = load_settings_from(&path).unwrap();
        assert!(loaded.api_log_enabled);
        assert_eq!(loaded.api_log_max_size_mib, 20);
        assert_eq!(loaded.api_log_backup_count, 6);
        assert_eq!(loaded.log_max_size_mib, 10);
        assert_eq!(loaded.quality, "HD1");
        let original = std::fs::read(&path).unwrap();
        for enabled in [false, true] {
            for (size, count) in [(0, 4), (1025, 4), (5, 0), (5, 101)] {
                settings.api_log_enabled = enabled;
                settings.api_log_max_size_mib = size;
                settings.api_log_backup_count = count;
                assert!(save_settings_at(&settings, &path).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
        }
    }

    #[test]
    fn segment_settings_upgrade_roundtrip_and_reject_zero_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{"quality":"HD1","auto_convert_mp4":true}"#).unwrap();
        let mut settings = load_settings_from(&path).unwrap();
        assert!(!settings.segment_recording_enabled);
        assert_eq!(settings.segment_duration_minutes, 60);
        settings.segment_recording_enabled = true;
        settings.segment_duration_minutes = 30;
        save_settings_at(&settings, &path).unwrap();
        let loaded = load_settings_from(&path).unwrap();
        assert!(loaded.segment_recording_enabled && loaded.auto_convert_mp4);
        assert_eq!(loaded.segment_duration_minutes, 30);
        assert_eq!(loaded.quality, "HD1");
        let original = std::fs::read(&path).unwrap();
        settings.segment_duration_minutes = 0;
        assert!(save_settings_at(&settings, &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn close_behavior_defaults_and_round_trips_without_losing_settings() {
        use super::CloseBehavior;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"quality":"HD1","notify_updates":false}"#).unwrap();
        let mut settings = load_settings_from(&path).unwrap();
        assert_eq!(settings.close_behavior, CloseBehavior::Exit);
        for behavior in [CloseBehavior::Tray, CloseBehavior::Exit] {
            settings.close_behavior = behavior;
            save_settings_at(&settings, &path).unwrap();
            let loaded = load_settings_from(&path).unwrap();
            assert_eq!(loaded.close_behavior, behavior);
            assert_eq!(loaded.quality, "HD1");
            assert!(!loaded.notify_updates);
        }
    }

    #[test]
    fn new_missing_and_invalid_quality_settings_use_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(AppSettings::default().quality, "ORIGIN");
        assert_eq!(load_settings_from(&path).unwrap().quality, "ORIGIN");
        for content in [
            "{}",
            r#"{"quality":""}"#,
            r#"{"quality":"  \t"}"#,
            r#"{"quality":"unknown"}"#,
            r#"{"quality":null}"#,
        ] {
            std::fs::write(&path, content).unwrap();
            let settings = load_settings_from(&path).unwrap();
            assert_eq!(settings.quality, "ORIGIN", "{content}");
            save_settings_at(&settings, &path).unwrap();
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["quality"], "ORIGIN");
        }
    }

    #[test]
    fn saved_quality_values_survive_loading_and_saving() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for quality in ["ORIGIN", "FULL_HD1", "HD1", "SD2", "SD1"] {
            std::fs::write(&path, serde_json::json!({"quality": quality}).to_string()).unwrap();
            let settings = load_settings_from(&path).unwrap();
            assert_eq!(settings.quality, quality);
            save_settings_at(&settings, &path).unwrap();
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["quality"], quality);
        }
    }

    #[test]
    fn atomic_save_replaces_existing_settings_and_invalid_save_preserves_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/settings.json");
        let mut settings = AppSettings::default();
        save_settings_at(&settings, &path).unwrap();
        settings.db_path = "new.db".to_string();
        save_settings_at(&settings, &path).unwrap();
        assert_eq!(load_settings_from(&path).unwrap().db_path, "new.db");
        let before = std::fs::read(&path).unwrap();
        settings.auto_check_interval_secs = 0;
        assert!(save_settings_at(&settings, &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

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
