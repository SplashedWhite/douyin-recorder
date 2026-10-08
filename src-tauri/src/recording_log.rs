//! Best-effort, local recording diagnostics. Never participates in recording decisions.
use chrono::Local;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

const MIB: u64 = 1024 * 1024;
const MAX_TEXT_BYTES: usize = 16 * 1024;
static LOGGER: OnceLock<Arc<RecordingLogger>> = OnceLock::new();

#[derive(Clone, Serialize)]
pub struct RecordingLogInfo {
    pub directory: String,
    pub last_error: Option<String>,
    pub api_last_error: Option<String>,
}

pub struct RecordingLogger {
    recording: RotatingLog,
    api: RotatingLog,
}

// Both logs share file management, but never share limits, locks or archive names.
struct RotatingLog {
    directory: PathBuf,
    stem: &'static str,
    // Serializes records, configuration changes and rotation independently of the DB.
    state: Mutex<LogState>,
}

struct LogState {
    enabled: bool,
    max_file_bytes: u64,
    backup_count: usize,
    prune_pending: bool,
    last_error: Option<String>,
}

impl RotatingLog {
    fn new(directory: PathBuf, stem: &'static str, enabled: bool) -> Self {
        Self {
            directory,
            stem,
            state: Mutex::new(LogState {
                enabled,
                max_file_bytes: crate::settings::DEFAULT_LOG_MAX_SIZE_MIB * MIB,
                backup_count: crate::settings::DEFAULT_LOG_BACKUP_COUNT,
                prune_pending: true,
                last_error: None,
            }),
        }
    }

    fn last_error(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_error
            .clone()
    }

    fn enabled(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).enabled
    }

    fn configure(&self, enabled: bool, max_file_bytes: u64, backup_count: usize) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let changed = state.enabled != enabled
            || state.max_file_bytes != max_file_bytes
            || state.backup_count != backup_count;
        if changed {
            state.enabled = enabled;
            state.max_file_bytes = max_file_bytes;
            state.backup_count = backup_count;
            state.prune_pending = true;
        }
        changed
    }

    fn write(&self, record: &Value) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // Recheck under the write lock so disabling also stops requests already in flight.
        if !state.enabled {
            return;
        }
        let result = (|| -> io::Result<()> {
            let mut bytes = serde_json::to_vec(record)?;
            bytes.push(b'\n');
            fs::create_dir_all(&self.directory)?;
            if state.prune_pending {
                self.prune_history(state.backup_count)?;
                state.prune_pending = false;
            }
            let current = self.path(0);
            let size = match fs::metadata(&current) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
                Err(error) => return Err(error),
            };
            if size > 0 && size.saturating_add(bytes.len() as u64) > state.max_file_bytes {
                self.rotate(state.backup_count)?;
            }
            let mut file = OpenOptions::new().create(true).append(true).open(current)?;
            file.write_all(&bytes)?;
            file.flush()
        })();
        if let Err(error) = result {
            // Keep the last failure even after recovery, so missed records are visible.
            state.last_error = Some(format!(
                "{} 日志写入失败: {}",
                Local::now().to_rfc3339(),
                sanitize_text(&error.to_string())
            ));
        }
    }

    fn path(&self, index: usize) -> PathBuf {
        self.directory.join(if index == 0 {
            format!("{}.log", self.stem)
        } else {
            format!("{}.{index}.log", self.stem)
        })
    }

    fn prune_history(&self, backup_count: usize) -> io::Result<()> {
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(index) = name
                .strip_prefix(&format!("{}.", self.stem))
                .and_then(|s| s.strip_suffix(".log"))
                .and_then(|s| s.parse::<usize>().ok())
            else {
                continue;
            };
            // Only our canonical archive names; never remove subdirectories or user files.
            if index > backup_count
                && name == format!("{}.{index}.log", self.stem)
                && entry.file_type()?.is_file()
            {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    fn rotate(&self, backup_count: usize) -> io::Result<()> {
        // No open handles are retained, allowing rename on Windows too.
        match fs::remove_file(self.path(backup_count)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        for index in (0..backup_count).rev() {
            match fs::rename(self.path(index), self.path(index + 1)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

fn normalized_limits(size: u64, count: usize) -> (u64, usize) {
    let size = if (1..=crate::settings::MAX_LOG_SIZE_MIB).contains(&size) {
        size
    } else {
        crate::settings::DEFAULT_LOG_MAX_SIZE_MIB
    };
    let count = if (1..=crate::settings::MAX_LOG_BACKUP_COUNT).contains(&count) {
        count
    } else {
        crate::settings::DEFAULT_LOG_BACKUP_COUNT
    };
    (size, count)
}

impl RecordingLogger {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            recording: RotatingLog::new(directory.clone(), "recorder", true),
            api: RotatingLog::new(directory, "douyin-api", false),
        }
    }

    pub fn info(&self) -> RecordingLogInfo {
        RecordingLogInfo {
            directory: self.recording.directory.to_string_lossy().into_owned(),
            last_error: self.recording.last_error(),
            api_last_error: self.api.last_error(),
        }
    }

    pub(crate) fn apply_settings(&self, settings: &crate::settings::AppSettings) {
        let (size, count) =
            normalized_limits(settings.api_log_max_size_mib, settings.api_log_backup_count);
        self.api
            .configure(settings.api_log_enabled, size * MIB, count);
        let (size, count) = normalized_limits(settings.log_max_size_mib, settings.log_backup_count);
        if self.recording.configure(true, size * MIB, count) {
            self.write(
                "INFO",
                "logging_configured",
                None,
                None,
                json!({"max_size_mib": size, "backup_count": count}),
            );
        }
    }

    pub fn write(
        &self,
        level: &str,
        event: &str,
        task_id: Option<i64>,
        room_id: Option<i64>,
        mut details: Value,
    ) {
        sanitize_value(&mut details);
        self.recording.write(&json!({
            "timestamp": Local::now().to_rfc3339(), "level": level, "event": event,
            "task_id": task_id, "room_id": room_id, "details": details,
        }));
    }

    pub fn api_enabled(&self) -> bool {
        self.api.enabled()
    }

    pub fn write_api(&self, mut record: Value) {
        if !self.api_enabled() {
            return;
        }
        // Parser error previews may be incomplete JSON. The complete body is saved
        // separately, so apply the existing compact error policy only to metadata.
        for key in ["error", "response_read_error"] {
            if let Some(Value::String(text)) = record.get_mut(key) {
                *text = sanitize_text(text);
            }
        }
        sanitize_api_value(&mut record);
        self.api.write(&record);
    }

    pub fn process_exit(&self, task_id: i64, exit: &crate::recorder::RecordingExit) {
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
        self.write(if exit.stopped_cleanly() { "INFO" } else { "WARN" }, "recording_process_exit", Some(task_id), None, json!({
            "reason": reason, "success": exit.status_success, "exit_code": exit.exit_code,
            "manually_stopped": exit.manually_stopped, "forced_stop_reason": exit.forced_stop_reason,
            "wait_error": exit.wait_error, "stderr_tail": exit.stderr_tail,
        }));
    }

    pub fn live_verification(
        &self,
        task_id: i64,
        room_id: i64,
        verification: Option<crate::LiveVerification>,
        rate_limited: bool,
        error: Option<String>,
    ) {
        let result = match verification {
            Some(crate::LiveVerification::Offline) => "offline",
            Some(crate::LiveVerification::Live) => "live",
            Some(crate::LiveVerification::Failed) => "failed",
            None => "skipped_manual_stop",
        };
        self.write(
            if error.is_some() { "WARN" } else { "INFO" },
            "recording_live_verification",
            Some(task_id),
            Some(room_id),
            json!({"result": result, "rate_limited": rate_limited, "error": error}),
        );
    }
}

pub fn initialize() {
    LOGGER.get_or_init(|| {
        Arc::new(RecordingLogger::new(
            crate::settings::default_db_dir().join("logs"),
        ))
    });
    configure(&crate::settings::load_settings());
    event(
        "INFO",
        "application_started",
        None,
        None,
        json!({"version": env!("CARGO_PKG_VERSION")}),
    );
}

pub fn configure(settings: &crate::settings::AppSettings) {
    if let Some(logger) = LOGGER.get() {
        logger.apply_settings(settings);
    }
}

pub fn logger() -> Option<Arc<RecordingLogger>> {
    LOGGER.get().cloned()
}

pub fn event(level: &str, name: &str, task_id: Option<i64>, room_id: Option<i64>, details: Value) {
    if let Some(logger) = LOGGER.get() {
        logger.write(level, name, task_id, room_id, details);
    }
}

#[tauri::command]
pub fn get_recording_log_info() -> Result<RecordingLogInfo, String> {
    LOGGER
        .get()
        .map(|logger| logger.info())
        .ok_or_else(|| "诊断日志尚未初始化".into())
}

pub fn automation_details(
    room: &crate::database::LiveRoom,
    reason: &str,
    message: Option<&str>,
) -> Value {
    json!({"reason": reason, "message": message, "platform_room_id": room.room_id,
        "mode": room.auto_monitor_mode, "enabled": room.auto_record_enabled,
        "retry_at": room.auto_record_retry_at, "monitor_until": room.auto_record_until,
        "daily_time": room.auto_record_daily_time})
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "cookie",
        "authorization",
        "password",
        "passwd",
        "token",
        "signature",
        "secret",
        "credential",
    ]
    .iter()
    .any(|part| key.contains(part))
}

fn sanitize_value(value: &mut Value) {
    match value {
        Value::String(text) => *text = sanitize_text(text),
        Value::Array(items) => items.iter_mut().for_each(sanitize_value),
        Value::Object(fields) => {
            for (key, value) in fields {
                if sensitive_key(key) {
                    *value = Value::String("[已隐藏]".into());
                } else {
                    sanitize_value(value);
                }
            }
        }
        _ => {}
    }
}

// API diagnostics preserve full strings and nested JSON, unlike compact recorder errors.
fn sanitize_api_value(value: &mut Value) {
    sanitize_api_value_at(value, 0);
}

fn sanitize_api_value_at(value: &mut Value, depth: usize) {
    // Bound recursion across JSON encoded inside JSON strings as well as normal objects.
    if depth >= 128 {
        *value = Value::String("[过深的嵌套内容已隐藏]".into());
        return;
    }
    match value {
        Value::String(text) => {
            if let Ok(mut embedded @ (Value::Object(_) | Value::Array(_) | Value::String(_))) =
                serde_json::from_str::<Value>(text)
            {
                sanitize_api_value_at(&mut embedded, depth + 1);
                *text = embedded.to_string();
            } else {
                *text = text.split_inclusive('\n').map(sanitize_api_line).collect();
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_api_value_at(item, depth + 1);
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                if sensitive_key(key)
                    || matches!(
                        key.to_ascii_lowercase().as_str(),
                        "ttwid"
                            | "sessionid"
                            | "sessionid_ss"
                            | "sid_tt"
                            | "sid_guard"
                            | "sign"
                            | "sign_key"
                            | "a_bogus"
                            | "x-bogus"
                            | "x_bogus"
                            | "auth"
                            | "auth_key"
                            | "access_key"
                            | "api_key"
                    )
                {
                    *value = Value::String("[已隐藏]".into());
                } else {
                    sanitize_api_value_at(value, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn sanitize_text(text: &str) -> String {
    // Parser errors contain an API response preview. Omit it, including multiline bodies.
    let text = text.split(" (响应:").next().unwrap_or(text);
    let mut result = text
        .lines()
        .map(sanitize_line)
        .collect::<Vec<_>>()
        .join("\n");
    if result.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        result.truncate(end);
        result.push_str("…[过长内容已截断]");
    }
    result
}

fn sanitize_api_line(line: &str) -> String {
    // Non-JSON responses can contain session credentials as plain text too.
    let lower = line.to_ascii_lowercase();
    let cut = [
        "ttwid",
        "sessionid",
        "sessionid_ss",
        "sid_tt",
        "sid_guard",
        "sign",
        "sign_key",
        "a_bogus",
        "x-bogus",
        "x_bogus",
        "auth",
        "auth_key",
        "access_key",
        "api_key",
    ]
    .iter()
    .flat_map(|needle| lower.match_indices(needle))
    .filter_map(|(index, needle)| {
        // These short names must be whole keys, not the end of words like "design".
        if index > 0
            && lower[..index]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return None;
        }
        let suffix = lower[index + needle.len()..]
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '\'' | '"'));
        (suffix.starts_with(':') || suffix.starts_with('=')).then_some(index)
    })
    .min();
    match cut {
        Some(index) => format!("{}[认证信息已隐藏]", sanitize_line(&line[..index])),
        None => sanitize_line(line),
    }
}

fn sanitize_line(line: &str) -> String {
    // Hide the rest of a header/credential line rather than attempting to parse secrets.
    let lower = line.to_ascii_lowercase();
    let cut = [
        "cookie",
        "authorization",
        "password",
        "passwd",
        "token",
        "signature",
        "secret",
        "credential",
    ]
    .iter()
    .flat_map(|needle| lower.match_indices(needle))
    .filter_map(|(index, needle)| {
        let suffix = lower[index + needle.len()..]
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '\'' | '"'));
        (suffix.starts_with(':') || suffix.starts_with('=')).then_some(index)
    })
    .chain(lower.find("bearer "))
    .min();
    let line = match cut {
        Some(index) => &line[..index],
        None => line,
    };
    let mut result = String::new();
    let mut rest = line;
    while let Some(marker) = rest.find("://") {
        let start = rest[..marker]
            .rfind(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '+' | '-' | '.'))
            .map_or(0, |index| {
                index + rest[index..].chars().next().unwrap().len_utf8()
            });
        let end = rest[marker + 3..]
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>'))
            .map_or(rest.len(), |index| marker + 3 + index);
        result.push_str(&rest[..start]);
        // Retain only scheme and host. Even URL paths can contain signed credentials.
        match reqwest::Url::parse(&rest[start..end]) {
            Ok(url) if url.host_str().is_some() => {
                result.push_str(url.scheme());
                result.push_str("://");
                result.push_str(url.host_str().unwrap());
                result.push_str("/[链接已隐藏]");
            }
            _ => result.push_str("[链接已隐藏]"),
        }
        rest = &rest[end..];
    }
    result.push_str(rest);
    if cut.is_some() {
        result.push_str("[认证信息已隐藏]");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn read_records(logger: &RecordingLogger) -> Vec<Value> {
        fs::read_to_string(logger.recording.path(0))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn appends_utf8_records_across_restarts_with_timezone() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            RecordingLogger::new(dir.path().into()).write(
                "INFO",
                "recording_started",
                Some(42),
                Some(7),
                json!({"message": "录制开始"}),
            );
        }
        let records = read_records(&RecordingLogger::new(dir.path().into()));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["details"]["message"], "录制开始");
        assert_eq!(records[0]["task_id"], 42);
        chrono::DateTime::parse_from_rfc3339(records[0]["timestamp"].as_str().unwrap()).unwrap();
    }

    #[test]
    fn rotates_only_owned_files_and_keeps_four_backups() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        logger.recording.configure(true, 250, 4);
        fs::write(dir.path().join("user.txt"), "keep").unwrap();
        for index in 0..10 {
            logger.write(
                "INFO",
                "sample",
                None,
                None,
                json!({"index":index,"message":"x".repeat(100)}),
            );
        }
        assert_eq!(read_records(&logger)[0]["details"]["index"], 9);
        for index in 1..=4 {
            let record: Value =
                serde_json::from_str(&fs::read_to_string(logger.recording.path(index)).unwrap())
                    .unwrap();
            assert_eq!(record["details"]["index"], 9 - index);
        }
        assert!(!logger.recording.path(5).exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("user.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn changed_limits_apply_to_next_write_and_prune_only_old_archives() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        logger.recording.configure(true, 250, 6);
        for index in 0..8 {
            logger.write(
                "INFO",
                "sample",
                None,
                None,
                json!({"index": index, "message": "x".repeat(100)}),
            );
        }
        assert!(logger.recording.path(6).exists());
        for name in ["user.log", "recorder.notes.log", "recorder.099.log"] {
            fs::write(dir.path().join(name), "keep").unwrap();
        }
        let settings = crate::settings::AppSettings {
            log_max_size_mib: 10,
            log_backup_count: 2,
            ..Default::default()
        };
        logger.apply_settings(&settings);
        // Increasing size does not rotate the current file. Decreasing retention prunes immediately.
        assert_eq!(read_records(&logger)[0]["details"]["index"], 7);
        assert!(logger.recording.path(2).exists());
        for index in 3..=6 {
            assert!(!logger.recording.path(index).exists());
        }
        for name in ["user.log", "recorder.notes.log", "recorder.099.log"] {
            assert_eq!(fs::read_to_string(dir.path().join(name)).unwrap(), "keep");
        }
        logger.recording.configure(true, 250, 2);
        logger.write("INFO", "smaller", None, None, json!({}));
        assert_eq!(read_records(&logger)[0]["event"], "smaller");
        assert!(logger.recording.path(1).exists());
        assert!(!logger.recording.path(3).exists());
        assert!(logger.info().last_error.is_none());
    }

    #[test]
    fn restart_loads_custom_limits_before_pruning_and_invalid_limits_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        logger.recording.configure(true, 250, 6);
        for index in 0..8 {
            logger.write(
                "INFO",
                "sample",
                None,
                None,
                json!({"index": index, "message": "x".repeat(100)}),
            );
        }
        let settings_path = dir.path().join("settings.json");
        let settings = crate::settings::AppSettings {
            log_max_size_mib: 50,
            log_backup_count: 6,
            ..Default::default()
        };
        crate::settings::save_settings_at(&settings, &settings_path).unwrap();
        let restarted = RecordingLogger::new(dir.path().into());
        restarted.apply_settings(&crate::settings::load_settings_from(&settings_path).unwrap());
        assert!(
            restarted.recording.path(6).exists(),
            "startup must not prune using defaults first"
        );
        assert_eq!(read_records(&restarted)[0]["details"]["index"], 7);
        assert_eq!(
            restarted.recording.state.lock().unwrap().max_file_bytes,
            50 * MIB
        );
        restarted.apply_settings(&crate::settings::AppSettings {
            log_max_size_mib: 0,
            log_backup_count: usize::MAX,
            ..Default::default()
        });
        let state = restarted.recording.state.lock().unwrap();
        assert_eq!(state.max_file_bytes, 5 * MIB);
        assert_eq!(state.backup_count, 4);
    }

    #[test]
    fn concurrent_records_do_not_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Arc::new(RecordingLogger::new(dir.path().into()));
        logger.api.configure(true, 5 * MIB, 4);
        let workers: Vec<_> = (0..8)
            .map(|id| {
                let logger = logger.clone();
                std::thread::spawn(move || {
                    for index in 0..25 {
                        logger.write_api(json!({"worker": id, "index": index, "response": {"prompts": "直播已结束"}}));
                        logger.write(
                            "INFO",
                            "concurrent",
                            Some(id),
                            None,
                            json!({"index": index, "stderr_tail": ["第一行", "第二行"]}),
                        );
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(read_records(&logger).len(), 200);
        let api_records: Vec<Value> = fs::read_to_string(logger.api.path(0))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(api_records.len(), 200);
        for record in api_records {
            assert_eq!(record["response"]["prompts"], "直播已结束");
        }
        assert!(logger.info().last_error.is_none());
        assert!(logger.info().api_last_error.is_none());
    }

    #[test]
    fn api_log_is_opt_in_and_disabled_files_survive_restart_and_limit_changes() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        let logger = RecordingLogger::new(log_dir.clone());
        logger.write_api(json!({"response": {"data": "not captured"}}));
        assert!(!log_dir.exists());
        let mut settings = crate::settings::AppSettings {
            api_log_enabled: true,
            api_log_max_size_mib: 20,
            api_log_backup_count: 6,
            ..Default::default()
        };
        logger.apply_settings(&settings);
        logger.write_api(json!({"response": {"data": "captured"}}));
        fs::write(logger.api.path(6), "history").unwrap();
        let before = fs::read(logger.api.path(0)).unwrap();
        let settings_path = dir.path().join("settings.json");
        crate::settings::save_settings_at(&settings, &settings_path).unwrap();
        let restarted = RecordingLogger::new(log_dir);
        restarted.apply_settings(&crate::settings::load_settings_from(&settings_path).unwrap());
        assert!(restarted.api_enabled());
        assert_eq!(restarted.api.state.lock().unwrap().max_file_bytes, 20 * MIB);
        settings.api_log_enabled = false;
        settings.api_log_backup_count = 1;
        restarted.apply_settings(&settings);
        restarted.write_api(json!({"response": "must not write or prune"}));
        assert_eq!(fs::read(restarted.api.path(0)).unwrap(), before);
        assert!(restarted.api.path(6).exists());
        settings.api_log_enabled = true;
        restarted.apply_settings(&settings);
        restarted.write_api(json!({"response": "new request"}));
        assert!(!restarted.api.path(6).exists());
        assert!(fs::read(restarted.api.path(0))
            .unwrap()
            .starts_with(&before));
        settings.api_log_max_size_mib = 0;
        settings.api_log_backup_count = 101;
        restarted.apply_settings(&settings);
        let state = restarted.api.state.lock().unwrap();
        assert_eq!(state.max_file_bytes, 5 * MIB);
        assert_eq!(state.backup_count, 4);
    }

    #[test]
    fn api_rotation_preserves_full_oversized_json_and_other_log_files() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        logger.api.configure(true, MIB, 2);
        logger.recording.configure(true, 250, 4);
        for index in 0..6 {
            logger.write(
                "INFO",
                "sample",
                None,
                None,
                json!({"index": index, "text": "x".repeat(200)}),
            );
        }
        let recorder_files: Vec<_> = (0..=4)
            .map(|i| fs::read(logger.recording.path(i)).unwrap())
            .collect();
        let large = "中".repeat(MIB as usize / 3 + 200);
        let record = json!({"response": {"large": large, "tail": [1, true, null, "完整末尾"]}});
        for name in ["user.log", "douyin-api.notes.log", "douyin-api.099.log"] {
            fs::write(dir.path().join(name), "keep").unwrap();
        }
        for _ in 0..5 {
            logger.write_api(record.clone());
        }
        for index in 0..=2 {
            let bytes = fs::read(logger.api.path(index)).unwrap();
            assert!(bytes.len() > MIB as usize);
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), record);
        }
        assert!(!logger.api.path(3).exists());
        logger.api.configure(true, MIB, 1);
        logger.write_api(json!({"response": "next"}));
        assert!(!logger.api.path(2).exists());
        for (index, before) in recorder_files.iter().enumerate() {
            assert_eq!(&fs::read(logger.recording.path(index)).unwrap(), before);
        }
        let api_before = fs::read(logger.api.path(1)).unwrap();
        logger.recording.configure(true, 250, 1);
        logger.write("INFO", "prune", None, None, json!({}));
        assert_eq!(fs::read(logger.api.path(1)).unwrap(), api_before);
        for name in ["user.log", "douyin-api.notes.log", "douyin-api.099.log"] {
            assert_eq!(fs::read_to_string(dir.path().join(name)).unwrap(), "keep");
        }
    }

    #[test]
    fn api_redaction_preserves_nested_json_strings_and_long_ordinary_fields() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        logger.api.configure(true, 5 * MIB, 4);
        let embedded = json!({"items": [{"access_token": "nested-credential", "status": 2}],
            "stream_data": json!({"url": "https://user:password@cdn.test/signed-path?sig=private-query", "codec": "h264"}).to_string()});
        let long = format!("{} (响应: 普通文字)\n", "文".repeat(20_000));
        logger.write_api(json!({"response": {
            "status_code": 0, "message": "room has finished", "prompts": "直播已结束",
            "cookie": "cookie-credential", "sessionid": "session-credential", "ttwid": "ttwid-credential",
            "nested": embedded.to_string(), "ordinary": long, "null": null
        }}));
        let text = fs::read_to_string(logger.api.path(0)).unwrap();
        for secret in [
            "nested-credential",
            "cookie-credential",
            "session-credential",
            "ttwid-credential",
            "signed-path",
            "private-query",
            "user:password",
        ] {
            assert!(!text.contains(secret), "leaked {secret}");
        }
        let record: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(record["response"]["ordinary"], long);
        assert_eq!(record["response"]["message"], "room has finished");
        assert_eq!(record["response"]["status_code"], 0);
        let nested: Value =
            serde_json::from_str(record["response"]["nested"].as_str().unwrap()).unwrap();
        assert_eq!(nested["items"][0]["status"], 2);
        assert_eq!(nested["items"][0]["access_token"], "[已隐藏]");
        let streams: Value = serde_json::from_str(nested["stream_data"].as_str().unwrap()).unwrap();
        assert_eq!(streams["codec"], "h264");
        assert_eq!(streams["url"], "https://cdn.test/[链接已隐藏]");
    }

    #[test]
    fn api_write_failure_is_independent_and_remains_visible_after_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        logger.api.configure(true, MIB, 4);
        fs::create_dir(logger.api.path(0)).unwrap();
        logger.write_api(json!({"response": {}}));
        let error = logger.info().api_last_error.unwrap();
        logger.write("INFO", "still works", None, None, json!({}));
        assert!(logger.info().last_error.is_none());
        fs::remove_dir(logger.api.path(0)).unwrap();
        logger.write_api(json!({"response": "recovered"}));
        assert!(logger.api.path(0).is_file());
        assert_eq!(
            logger.info().api_last_error.as_deref(),
            Some(error.as_str())
        );
    }

    #[test]
    fn api_text_and_error_previews_cannot_expose_session_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        logger.api.configure(true, MIB, 4);
        logger.write_api(json!({
            "error": "解析失败 (响应: {\"sessionid\":\"preview-secret\"}",
            "response_format": "text",
            "response": "broken JSON: {\"sessionid\":\"body-secret\"\nTTWID = another-secret\nsign: signed-secret\n普通文本 design:保留\n最后一行"
        }));
        let text = fs::read_to_string(logger.api.path(0)).unwrap();
        for secret in [
            "preview-secret",
            "body-secret",
            "another-secret",
            "signed-secret",
        ] {
            assert!(!text.contains(secret));
        }
        let record: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(record["error"], "解析失败");
        assert!(record["response"].as_str().unwrap().contains("design:保留"));
        assert!(record["response"].as_str().unwrap().ends_with("最后一行"));
    }

    #[test]
    fn failed_writes_do_not_panic_and_remain_visible_after_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("logs");
        fs::write(&blocked, "not a directory").unwrap();
        let logger = RecordingLogger::new(blocked.clone());
        logger.write("ERROR", "sample", None, None, json!({}));
        let error = logger.info().last_error.unwrap();
        fs::remove_file(&blocked).unwrap();
        logger.write("INFO", "recovered", None, None, json!({}));
        assert_eq!(read_records(&logger)[0]["event"], "recovered");
        assert_eq!(logger.info().last_error.as_deref(), Some(error.as_str()));
    }

    #[test]
    fn redacts_errors_urls_headers_and_nested_fields() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        logger.write("WARN", "error", None, None, json!({
            "error": "解析 API 响应失败 (响应: private-body\nprivate-body-2)",
            "stderr_tail": ["HTTP error 403 https://user:proxy-pass@cdn.test/signed-secret?sig=query-secret#fragment-secret",
                "Cookie: cookie-secret; sessionid=other-secret", "Authorization: Bearer bearer-secret",
                "\"Cookie\" : \"quoted-cookie-secret\"", "password = spaced-password-secret",
                "request https://cdn.test/video?token=token-secret", "代理 socks5://proxy-user:proxy-secret@localhost:1080"],
            "nested": {"cookie": "nested-secret", "password": "password-secret"},
            "ordinary": "录制进程已结束，但主播仍在直播"
        }));
        let text = fs::read_to_string(logger.recording.path(0)).unwrap();
        for secret in [
            "private-body",
            "private-body-2",
            "proxy-pass",
            "signed-secret",
            "query-secret",
            "fragment-secret",
            "cookie-secret",
            "other-secret",
            "bearer-secret",
            "quoted-cookie-secret",
            "spaced-password-secret",
            "token-secret",
            "proxy-user",
            "proxy-secret",
            "nested-secret",
            "password-secret",
        ] {
            assert!(!text.contains(secret), "leaked {secret}: {text}");
        }
        assert!(text.contains("HTTP error 403"));
        assert!(text.contains("cdn.test"));
        assert!(text.contains("主播仍在直播"));
        assert_eq!(read_records(&logger).len(), 1);
    }

    #[test]
    fn verification_preserves_query_failure_separately_from_process_success() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().into());
        let exit = crate::recorder::RecordingExit {
            manually_stopped: false,
            status_success: true,
            exit_code: Some(0),
            forced_stop_reason: None,
            wait_error: None,
            stderr_tail: vec![],
        };
        logger.process_exit(42, &exit);
        logger.live_verification(
            42,
            7,
            Some(crate::LiveVerification::Failed),
            true,
            Some("抖音 API 返回错误: HTTP 403".into()),
        );
        logger.write("INFO", "recording_classified", Some(42), Some(7), json!({
            "status": crate::classify_recording(1024, &exit, Some(crate::LiveVerification::Failed))
        }));
        let records = read_records(&logger);
        assert_eq!(records[0]["details"]["reason"], "stream_exit");
        assert_eq!(records[1]["details"]["result"], "failed");
        assert_eq!(records[1]["details"]["rate_limited"], true);
        assert_eq!(
            records[1]["details"]["error"],
            "抖音 API 返回错误: HTTP 403"
        );
        assert_eq!(records[2]["details"]["status"], "interrupted");
        logger.live_verification(43, 7, Some(crate::LiveVerification::Live), false, None);
        logger.live_verification(44, 7, Some(crate::LiveVerification::Offline), false, None);
        let records = read_records(&logger);
        assert_eq!(records[3]["details"]["result"], "live");
        assert_eq!(records[4]["details"]["result"], "offline");
    }

    #[test]
    fn window_pause_and_continuous_retry_are_diagnosable() {
        use crate::auto_policy::AutoMonitorMode;
        let dir = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::new(dir.path().join("logs"));
        let state = crate::AppState {
            db: Mutex::new(crate::Database::new(&dir.path().join("test.db")).unwrap()),
            recorder: crate::Recorder::new("unused".into()),
            parser: crate::DouyinParser::new(),
            auto_recorder: crate::AutoRecorder::new(),
            start_lock: tokio::sync::Mutex::new(()),
            lifecycle: Default::default(),
        };
        let exit = crate::recorder::RecordingExit {
            manually_stopped: false,
            status_success: false,
            exit_code: Some(1),
            forced_stop_reason: None,
            wait_error: None,
            stderr_tail: vec![],
        };
        for mode in [AutoMonitorMode::Window, AutoMonitorMode::Continuous] {
            let mut room = crate::LiveRoom {
                id: 7,
                room_id: "123456".into(),
                auto_record_enabled: true,
                auto_monitor_mode: mode,
                ..Default::default()
            };
            let (reason, message) = crate::apply_post_recording_auto_policy(
                &state,
                &mut room,
                "auto",
                "interrupted",
                &exit,
                Some(crate::LiveVerification::Live),
                false,
            );
            logger.write(
                "INFO",
                "recording_auto_policy",
                Some(42),
                Some(room.id),
                automation_details(&room, reason, message.as_deref()),
            );
        }
        let records = read_records(&logger);
        assert_eq!(records[0]["details"]["reason"], "paused");
        assert_eq!(records[0]["details"]["enabled"], false);
        assert!(records[0]["details"]["message"]
            .as_str()
            .unwrap()
            .contains("异常结束"));
        assert_eq!(records[1]["details"]["reason"], "backoff");
        assert_eq!(records[1]["details"]["enabled"], true);
        chrono::DateTime::parse_from_rfc3339(records[1]["details"]["retry_at"].as_str().unwrap())
            .unwrap();
    }
}
