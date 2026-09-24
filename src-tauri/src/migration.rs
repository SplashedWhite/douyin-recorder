use crate::{settings, AppSettings, AppState};
use std::path::Path;

/// Run on a blocking thread; lock ordering matches recording startup.
pub fn migrate_database(
    state: &AppState,
    new_path: &str,
    settings_path: &Path,
) -> Result<String, String> {
    migrate_database_with(state, new_path, |path| {
        let mut current_settings = settings::load_settings_from(settings_path)?;
        current_settings.db_path = path.to_string_lossy().to_string();
        settings::save_settings_at(&current_settings, settings_path)
    })
}

fn migrate_database_with<F>(
    state: &AppState,
    new_path: &str,
    persist_path: F,
) -> Result<String, String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
    let _start_guard = state.start_lock.blocking_lock();
    let mut db = state.db.lock().map_err(|e| e.to_string())?;
    if state.recorder.has_active_records()? {
        return Err("有任务正在录制或结束处理中，请等待结束后再迁移数据库".to_string());
    }
    db.migrate_to(new_path, persist_path)
}

pub fn save_settings(
    state: &AppState,
    mut new_settings: AppSettings,
    settings_path: &Path,
) -> Result<AppSettings, String> {
    // Ordinary saves cannot redirect the next startup to a stale or unmigrated database.
    let db = state.db.lock().map_err(|e| e.to_string())?;
    new_settings.db_path = db.path().to_string_lossy().to_string();
    settings::save_settings_at(&new_settings, settings_path)?;
    Ok(new_settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AutoRecorder, Database, DouyinParser, Recorder};
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::Mutex as AsyncMutex;

    struct Fixture {
        state: Arc<AppState>,
        config: PathBuf,
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = Database::new(&dir.path().join("source.db")).unwrap();
            let config = dir.path().join("settings.json");
            let app_settings = AppSettings {
                db_path: db.path().to_string_lossy().to_string(),
                quality: "SD1".to_string(),
                ..AppSettings::default()
            };
            settings::save_settings_at(&app_settings, &config).unwrap();
            let state = Arc::new(AppState {
                db: Mutex::new(db),
                recorder: Recorder::new("unused-ffmpeg".to_string()),
                parser: DouyinParser::new(),
                auto_recorder: AutoRecorder::new(),
                start_lock: AsyncMutex::new(()),
                lifecycle: Default::default(),
            });
            Self { state, config, dir }
        }

        fn add_room(&self, room: &str) -> i64 {
            self.state
                .db
                .lock()
                .unwrap()
                .add_room_full("douyin", room, room, "title", "", "", false)
                .unwrap()
        }

        fn migrate(&self, target: &Path) -> Result<String, String> {
            migrate_database(&self.state, target.to_str().unwrap(), &self.config)
        }
    }

    #[test]
    fn consecutive_migrations_and_restart_keep_all_new_writes() {
        let fixture = Fixture::new();
        let source = fixture.state.db.lock().unwrap().path().to_path_buf();
        let first_room = fixture.add_room("first");
        let target_b = fixture.dir.path().join("b.db");
        let target_c = fixture.dir.path().join("c.db");
        fixture.migrate(&target_b).unwrap();
        let second_room = fixture.add_room("second");
        let task_id = {
            let db = fixture.state.db.lock().unwrap();
            db.set_room_auto_record(first_room, true, Some("2099-01-01T00:00:00Z"))
                .unwrap();
            let task = db.add_task(second_room, "manual").unwrap();
            db.update_task_status(task, "recording").unwrap();
            db.mark_task_finalizing(task, 123).unwrap();
            db.finish_task(task, "completed", Some("recording.flv"), 123)
                .unwrap();
            task
        };
        fixture.migrate(&target_c).unwrap();
        fixture.add_room("third");
        let saved = settings::load_settings_from(&fixture.config).unwrap();
        assert_eq!(saved.quality, "SD1");
        assert_eq!(
            Path::new(&saved.db_path),
            std::fs::canonicalize(&target_c).unwrap()
        );
        assert_eq!(
            Database::new(&source)
                .unwrap()
                .get_all_rooms()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            Database::new(&target_b)
                .unwrap()
                .get_all_rooms()
                .unwrap()
                .len(),
            2
        );
        drop(fixture.state);
        let restarted = Database::new(Path::new(&saved.db_path)).unwrap();
        assert_eq!(restarted.get_all_rooms().unwrap().len(), 3);
        assert!(restarted.get_room(first_room).unwrap().auto_record_enabled);
        let task = restarted.get_task(task_id).unwrap();
        assert_eq!(task.status, "completed");
        assert_eq!(task.file_size, Some(123));
        assert!(task.end_time.is_some());
        assert_eq!(task.file_path.as_deref(), Some("recording.flv"));
    }

    #[test]
    fn recording_and_finalizing_tasks_block_migration_without_changes() {
        for status in ["recording", "finalizing"] {
            let fixture = Fixture::new();
            let room = fixture.add_room("room");
            {
                let db = fixture.state.db.lock().unwrap();
                let task = db.add_task(room, "auto").unwrap();
                db.update_task_status(task, status).unwrap();
            }
            let before = std::fs::read(&fixture.config).unwrap();
            let target = fixture.dir.path().join("new-parent/target.db");
            assert!(fixture
                .migrate(&target)
                .unwrap_err()
                .contains("录制或结束处理中"));
            assert!(!target.parent().unwrap().exists());
            assert_eq!(std::fs::read(&fixture.config).unwrap(), before);
        }
    }

    #[test]
    fn invalid_or_existing_targets_preserve_existing_files_and_settings() {
        let fixture = Fixture::new();
        let source = fixture.state.db.lock().unwrap().path().to_path_buf();
        let before = std::fs::read(&fixture.config).unwrap();
        let existing = fixture.dir.path().join("existing.db");
        std::fs::write(&existing, b"do not overwrite").unwrap();
        for target in [
            Path::new(""),
            Path::new("   "),
            &source,
            fixture.dir.path(),
            &existing,
        ] {
            assert!(fixture.migrate(target).is_err());
        }
        // A regular file cannot be used as the destination's parent directory.
        assert!(fixture.migrate(&existing.join("target.db")).is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"do not overwrite");
        assert_eq!(std::fs::read(&fixture.config).unwrap(), before);
        assert_eq!(fixture.state.db.lock().unwrap().path(), source);
        fixture.add_room("still-writable");
    }

    #[test]
    fn failed_config_read_keeps_source_and_removes_new_database() {
        let fixture = Fixture::new();
        let source = fixture.state.db.lock().unwrap().path().to_path_buf();
        let target = fixture.dir.path().join("target.db");
        std::fs::write(&fixture.config, b"invalid settings").unwrap();
        assert!(fixture
            .migrate(&target)
            .unwrap_err()
            .contains("读取设置失败"));
        assert!(!target.exists());
        assert_eq!(fixture.state.db.lock().unwrap().path(), source);
        assert_eq!(std::fs::read(&fixture.config).unwrap(), b"invalid settings");
        fixture.add_room("still-writable");
    }

    #[cfg(windows)]
    #[test]
    fn failed_atomic_config_replace_keeps_source_and_removes_new_database() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let source = fixture.state.db.lock().unwrap().path().to_path_buf();
        let before = std::fs::read(&fixture.config).unwrap();
        let target = fixture.dir.path().join("target.db");
        // Allow reads, but deny replacing the settings file while this handle is open.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&fixture.config)
            .unwrap();
        assert!(fixture
            .migrate(&target)
            .unwrap_err()
            .contains("替换设置文件失败"));
        assert!(!target.exists());
        assert_eq!(fixture.state.db.lock().unwrap().path(), source);
        assert_eq!(std::fs::read(&fixture.config).unwrap(), before);
        drop(lock);
        fixture.add_room("still-writable");
        fixture.migrate(&target).unwrap();
    }

    #[test]
    fn saving_stale_settings_cannot_redirect_the_database() {
        let fixture = Fixture::new();
        let mut stale = settings::load_settings_from(&fixture.config).unwrap();
        let target = fixture.dir.path().join("target.db");
        let migrated = fixture.migrate(&target).unwrap();
        stale.quality = "SD2".to_string();
        let saved = save_settings(&fixture.state, stale, &fixture.config).unwrap();
        assert_eq!(saved.db_path, migrated);
        let reloaded = settings::load_settings_from(&fixture.config).unwrap();
        assert_eq!(reloaded.db_path, migrated);
        assert_eq!(reloaded.quality, "SD2");
    }

    #[test]
    fn migration_waits_for_recording_start_before_checking_task_state() {
        let fixture = Fixture::new();
        let room = fixture.add_room("room");
        let target = fixture.dir.path().join("target.db");
        let start_guard = fixture.state.start_lock.blocking_lock();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                entered_tx.send(()).unwrap();
                result_tx.send(fixture.migrate(&target)).unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                result_rx.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            {
                let db = fixture.state.db.lock().unwrap();
                let task = db.add_task(room, "auto").unwrap();
                db.update_task_status(task, "recording").unwrap();
            }
            drop(start_guard);
            assert!(result_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap_err()
                .contains("录制或结束处理中"));
        });
        assert!(!target.exists());
    }

    #[test]
    fn recording_start_after_migration_writes_only_to_the_new_connection() {
        let fixture = Fixture::new();
        let room = fixture.add_room("room");
        let source = fixture.state.db.lock().unwrap().path().to_path_buf();
        let target = fixture.dir.path().join("target.db");
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let fixture_ref = &fixture;
            let target_ref = &target;
            let migration = scope.spawn(move || {
                migrate_database_with(&fixture_ref.state, target_ref.to_str().unwrap(), |path| {
                    ready_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let mut current = settings::load_settings_from(&fixture_ref.config)?;
                    current.db_path = path.to_string_lossy().to_string();
                    settings::save_settings_at(&current, &fixture_ref.config)
                })
            });
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            scope.spawn(|| {
                started_tx.send(()).unwrap();
                let _start = fixture.state.start_lock.blocking_lock();
                let db = fixture.state.db.lock().unwrap();
                let task = db.add_task(room, "auto").unwrap();
                db.update_task_status(task, "recording").unwrap();
                done_tx.send(task).unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                done_rx.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            release_tx.send(()).unwrap();
            migration.join().unwrap().unwrap();
            let task = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                fixture
                    .state
                    .db
                    .lock()
                    .unwrap()
                    .get_task(task)
                    .unwrap()
                    .status,
                "recording"
            );
        });
        assert!(Database::new(&source)
            .unwrap()
            .get_all_tasks()
            .unwrap()
            .is_empty());
        assert_eq!(
            Database::new(&target)
                .unwrap()
                .get_all_tasks()
                .unwrap()
                .len(),
            1
        );
    }
}
