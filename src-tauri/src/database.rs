use crate::auto_policy::AutoMonitorMode;
use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct LiveRoom {
    pub id: i64,
    pub platform: String,
    pub room_id: String,
    pub anchor_name: String,
    pub room_title: String,
    pub cover_url: String,
    pub avatar_url: String,
    pub is_live: bool,
    pub created_at: String,
    pub auto_record_enabled: bool,
    pub auto_monitor_mode: AutoMonitorMode,
    pub auto_record_retry_at: Option<String>,
    pub auto_record_error: Option<String>,
    pub auto_record_revision: i64,
    pub auto_record_daily_time: Option<String>,
    pub auto_record_until: Option<String>,
    pub last_schedule_trigger_date: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecordTask {
    pub id: i64,
    pub room_id: i64,
    pub status: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub file_path: Option<String>,
    pub file_size: Option<i64>,
    pub trigger: String,
    pub segment_output: Option<crate::segments::SegmentOutput>,
    pub segments: Vec<crate::segments::RecordSegment>,
}

impl rusqlite::types::FromSql for AutoMonitorMode {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        match value.as_str()? {
            "window" => Ok(Self::Window),
            "continuous" => Ok(Self::Continuous),
            _ => Err(rusqlite::types::FromSqlError::Other(
                "无效的监控模式".into(),
            )),
        }
    }
}

pub struct Database {
    pub(crate) conn: Connection,
    path: PathBuf,
}

impl Database {
    pub fn new(db_path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::InvalidParameterName(format!("创建数据库目录失败: {}", e))
            })?;
        }
        let conn = Connection::open(db_path)?;
        let path = std::fs::canonicalize(db_path)
            .map_err(|_| rusqlite::Error::InvalidPath(db_path.to_path_buf()))?;
        let db = Database { conn, path };
        db.init_tables()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn has_running_tasks(&self) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM record_tasks WHERE status IN ('recording', 'finalizing')) OR EXISTS(SELECT 1 FROM record_segments WHERE conversion_state IN ('queued', 'converting'))",
            [],
            |row| row.get(0),
        )
    }

    /// The caller holds the database lock until the new connection replaces this one.
    pub fn migrate_to<F>(&mut self, new_path: &str, persist_path: F) -> Result<String, String>
    where
        F: FnOnce(&Path) -> Result<(), String>,
    {
        if self.has_running_tasks().map_err(|e| e.to_string())? {
            return Err("有任务正在录制或结束处理中，请等待结束后再迁移数据库".to_string());
        }
        if new_path.trim().is_empty() {
            return Err("迁移目标路径不能为空".to_string());
        }
        let target = PathBuf::from(new_path);
        if std::fs::canonicalize(&target).ok().as_ref() == Some(&self.path) {
            return Err("迁移目标就是当前数据库".to_string());
        }
        match std::fs::symlink_metadata(&target) {
            Ok(_) => return Err("迁移目标已存在，请选择一个尚不存在的数据库文件".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("检查迁移目标失败: {}", e)),
        }
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目标目录失败: {}", e))?;
        }
        // Reserve exclusively so an existing file can never be overwritten or cleaned up.
        let reservation = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| format!("创建目标数据库失败: {}", e))?;
        drop(reservation);
        let prepared = (|| {
            let mut conn =
                Connection::open(&target).map_err(|e| format!("打开目标数据库失败: {}", e))?;
            {
                let backup = rusqlite::backup::Backup::new(&self.conn, &mut conn)
                    .map_err(|e| format!("创建数据库备份失败: {}", e))?;
                match backup
                    .step(-1)
                    .map_err(|e| format!("备份数据库失败: {}", e))?
                {
                    rusqlite::backup::StepResult::Done => {}
                    _ => return Err("数据库正被占用，备份未完成，请稍后重试".to_string()),
                }
            }
            let check: String = conn
                .query_row("PRAGMA quick_check", [], |row| row.get(0))
                .map_err(|e| format!("验证数据库备份失败: {}", e))?;
            if check != "ok" {
                return Err(format!("数据库备份完整性检查失败: {}", check));
            }
            let path =
                std::fs::canonicalize(&target).map_err(|e| format!("解析目标路径失败: {}", e))?;
            let replacement = Database { conn, path };
            replacement
                .get_all_rooms()
                .map_err(|e| format!("验证房间数据失败: {}", e))?;
            replacement
                .get_all_tasks()
                .map_err(|e| format!("验证任务数据失败: {}", e))?;
            persist_path(&replacement.path)?;
            Ok(replacement)
        })();
        match prepared {
            Ok(replacement) => {
                // No fallible operations after the settings have committed.
                *self = replacement;
                Ok(self.path.to_string_lossy().to_string())
            }
            Err(error) => {
                // The failed replacement connection is closed before removing our new file.
                if let Err(cleanup) = std::fs::remove_file(&target) {
                    return Err(format!(
                        "{}；清理未完成的目标数据库失败: {}",
                        error, cleanup
                    ));
                }
                Err(error)
            }
        }
    }

    fn init_tables(&self) -> Result<()> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS live_rooms (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                platform TEXT NOT NULL,
                room_id TEXT NOT NULL,
                anchor_name TEXT DEFAULT '',
                room_title TEXT DEFAULT '',
                cover_url TEXT DEFAULT '',
                avatar_url TEXT DEFAULT '',
                is_live BOOLEAN DEFAULT 0,
                auto_record_enabled BOOLEAN DEFAULT 0,
                auto_record_daily_time TEXT,
                auto_record_until TEXT,
                last_schedule_trigger_date TEXT,
                created_at TEXT DEFAULT (datetime('now'))
            )",
            [],
        )?;

        // Migration: add avatar_url column for existing databases
        let _ = self.conn.execute(
            "ALTER TABLE live_rooms ADD COLUMN avatar_url TEXT DEFAULT ''",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE live_rooms ADD COLUMN auto_record_enabled BOOLEAN DEFAULT 0",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE live_rooms ADD COLUMN auto_record_daily_time TEXT",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE live_rooms ADD COLUMN auto_record_until TEXT",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE live_rooms ADD COLUMN last_schedule_trigger_date TEXT",
            [],
        );

        // Inspect columns instead of swallowing migration errors. This also upgrades
        // databases created before automation existed, after the legacy migrations above.
        let columns = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(live_rooms)")?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>>>()?;
            names
        };
        for (name, definition) in [
            ("auto_monitor_mode", "TEXT NOT NULL DEFAULT 'window'"),
            ("auto_record_retry_at", "TEXT"),
            ("auto_record_error", "TEXT"),
            ("auto_record_revision", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !columns.iter().any(|column| column == name) {
                self.conn.execute(
                    &format!("ALTER TABLE live_rooms ADD COLUMN {name} {definition}"),
                    [],
                )?;
            }
        }

        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS record_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                room_id INTEGER NOT NULL,
                status TEXT DEFAULT 'waiting',
                start_time TEXT DEFAULT (datetime('now')),
                end_time TEXT,
                file_path TEXT,
                file_size INTEGER,
                trigger TEXT DEFAULT 'manual',
                FOREIGN KEY (room_id) REFERENCES live_rooms(id)
            )",
            [],
        )?;
        let _ = self.conn.execute(
            "ALTER TABLE record_tasks ADD COLUMN trigger TEXT DEFAULT 'manual'",
            [],
        );

        self.init_segments()?;

        Ok(())
    }

    pub fn get_all_rooms(&self) -> Result<Vec<LiveRoom>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, platform, room_id, anchor_name, room_title, cover_url, avatar_url, is_live, created_at, auto_record_enabled, auto_record_daily_time, auto_record_until, last_schedule_trigger_date, auto_monitor_mode, auto_record_retry_at, auto_record_error, auto_record_revision FROM live_rooms"
        )?;

        let rooms = stmt
            .query_map([], |row| {
                Ok(LiveRoom {
                    id: row.get(0)?,
                    platform: row.get(1)?,
                    room_id: row.get(2)?,
                    anchor_name: row.get(3)?,
                    room_title: row.get(4)?,
                    cover_url: row.get(5)?,
                    avatar_url: row.get(6)?,
                    is_live: row.get(7)?,
                    created_at: row.get(8)?,
                    auto_record_enabled: row.get(9)?,
                    auto_record_daily_time: row.get(10)?,
                    auto_record_until: row.get(11)?,
                    last_schedule_trigger_date: row.get(12)?,
                    auto_monitor_mode: row.get(13)?,
                    auto_record_retry_at: row.get(14)?,
                    auto_record_error: row.get(15)?,
                    auto_record_revision: row.get(16)?,
                })
            })?
            .collect::<Result<Vec<_>>>()?;

        Ok(rooms)
    }

    pub fn get_room(&self, id: i64) -> Result<LiveRoom> {
        self.conn.query_row(
            "SELECT id, platform, room_id, anchor_name, room_title, cover_url, avatar_url, is_live, created_at, auto_record_enabled, auto_record_daily_time, auto_record_until, last_schedule_trigger_date, auto_monitor_mode, auto_record_retry_at, auto_record_error, auto_record_revision FROM live_rooms WHERE id = ?1",
            [id],
            |row| {
                Ok(LiveRoom {
                    id: row.get(0)?,
                    platform: row.get(1)?,
                    room_id: row.get(2)?,
                    anchor_name: row.get(3)?,
                    room_title: row.get(4)?,
                    cover_url: row.get(5)?,
                    avatar_url: row.get(6)?,
                    is_live: row.get(7)?,
                    created_at: row.get(8)?,
                    auto_record_enabled: row.get(9)?,
                    auto_record_daily_time: row.get(10)?,
                    auto_record_until: row.get(11)?,
                    last_schedule_trigger_date: row.get(12)?,
                    auto_monitor_mode: row.get(13)?,
                    auto_record_retry_at: row.get(14)?,
                    auto_record_error: row.get(15)?,
                    auto_record_revision: row.get(16)?,
                })
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_room_full(
        &self,
        platform: &str,
        room_id: &str,
        anchor_name: &str,
        room_title: &str,
        cover_url: &str,
        avatar_url: &str,
        is_live: bool,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO live_rooms (platform, room_id, anchor_name, room_title, cover_url, avatar_url, is_live) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![platform, room_id, anchor_name, room_title, cover_url, avatar_url, is_live],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_room_live_status(
        &self,
        id: i64,
        anchor_name: &str,
        room_title: &str,
        cover_url: &str,
        avatar_url: &str,
        is_live: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE live_rooms SET anchor_name = ?1, room_title = ?2, cover_url = ?3, avatar_url = ?4, is_live = ?5 WHERE id = ?6",
            rusqlite::params![anchor_name, room_title, cover_url, avatar_url, is_live, id],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn set_room_auto_record(&self, id: i64, enabled: bool, until: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE live_rooms SET auto_record_enabled = ?1, auto_record_until = ?2, auto_record_error = NULL, auto_record_retry_at = NULL, auto_record_revision = auto_record_revision + 1 WHERE id = ?3",
            rusqlite::params![enabled, until, id],
        )?;
        Ok(())
    }

    /// Callers serialize read/decide/write with AppState.db. A revision lets
    /// in-flight network checks discard results after a newer user decision.
    pub fn save_room_automation(&self, room: &LiveRoom) -> Result<LiveRoom> {
        self.conn.execute(
            "UPDATE live_rooms SET auto_record_enabled = ?1, auto_monitor_mode = ?2,
             auto_record_until = ?3, auto_record_daily_time = ?4, last_schedule_trigger_date = ?5,
             auto_record_retry_at = ?6, auto_record_error = ?7,
             auto_record_revision = auto_record_revision + 1 WHERE id = ?8",
            rusqlite::params![
                room.auto_record_enabled,
                room.auto_monitor_mode.as_str(),
                room.auto_record_until,
                room.auto_record_daily_time,
                room.last_schedule_trigger_date,
                room.auto_record_retry_at,
                room.auto_record_error,
                room.id
            ],
        )?;
        self.get_room(room.id)
    }

    pub fn finish_task_and_automation(
        &self,
        task_id: i64,
        status: &str,
        path: Option<&str>,
        size: i64,
        room: &LiveRoom,
    ) -> Result<(RecordTask, LiveRoom)> {
        let transaction = self.conn.unchecked_transaction()?;
        self.finish_task(task_id, status, path, size)?;
        let room = self.save_room_automation(room)?;
        let task = self.get_task(task_id)?;
        transaction.commit()?;
        Ok((task, room))
    }

    pub fn delete_room(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM live_rooms WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn count_tasks_for_room(&self, room_id: i64) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM record_tasks WHERE room_id = ?1",
            [room_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    pub fn has_running_tasks_for_room(&self, room_id: i64) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM record_tasks WHERE room_id = ?1 AND (status IN ('recording', 'finalizing') OR id IN (SELECT task_id FROM record_segments WHERE conversion_state IN ('queued', 'converting')))",
            [room_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn delete_room_cascade(&self, id: i64) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        self.conn.execute("DELETE FROM record_segments WHERE task_id IN (SELECT id FROM record_tasks WHERE room_id = ?1)", [id])?;
        self.conn.execute("DELETE FROM segment_outputs WHERE task_id IN (SELECT id FROM record_tasks WHERE room_id = ?1)", [id])?;
        self.conn
            .execute("DELETE FROM record_tasks WHERE room_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM live_rooms WHERE id = ?1", [id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_task(&self, id: i64) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        self.conn
            .execute("DELETE FROM record_segments WHERE task_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM segment_outputs WHERE task_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM record_tasks WHERE id = ?1", [id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn get_all_tasks(&self) -> Result<Vec<RecordTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, room_id, status, start_time, end_time, file_path, file_size, trigger FROM record_tasks ORDER BY id DESC"
        )?;

        let tasks = stmt
            .query_map([], |row| {
                Ok(RecordTask {
                    id: row.get(0)?,
                    room_id: row.get(1)?,
                    status: row.get(2)?,
                    start_time: row.get(3)?,
                    end_time: row.get(4)?,
                    file_path: row.get(5)?,
                    file_size: row.get(6)?,
                    trigger: row.get(7)?,
                    segment_output: self.get_segment_output(row.get(0)?)?,
                    segments: self.get_segments(row.get(0)?)?,
                })
            })?
            .collect::<Result<Vec<_>>>()?;

        Ok(tasks)
    }

    pub fn get_task(&self, id: i64) -> Result<RecordTask> {
        self.conn.query_row(
            "SELECT id, room_id, status, start_time, end_time, file_path, file_size, trigger FROM record_tasks WHERE id = ?1",
            [id],
            |row| {
                Ok(RecordTask {
                    id: row.get(0)?,
                    room_id: row.get(1)?,
                    status: row.get(2)?,
                    start_time: row.get(3)?,
                    end_time: row.get(4)?,
                    file_path: row.get(5)?,
                    file_size: row.get(6)?,
                    trigger: row.get(7)?,
                    segment_output: self.get_segment_output(row.get(0)?)?,
                    segments: self.get_segments(row.get(0)?)?,
                })
            },
        )
    }

    pub fn add_task(&self, room_id: i64, trigger: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO record_tasks (room_id, trigger) VALUES (?1, ?2)",
            rusqlite::params![room_id, trigger],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_task_status(&self, id: i64, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE record_tasks SET status = ?1 WHERE id = ?2",
            [status, &id.to_string()],
        )?;
        Ok(())
    }

    pub fn update_task_status_and_path(
        &self,
        id: i64,
        status: &str,
        file_path: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE record_tasks SET status = ?1, file_path = COALESCE(?2, file_path) WHERE id = ?3",
            rusqlite::params![status, file_path, id],
        )?;
        Ok(())
    }

    pub fn mark_task_finalizing(&self, id: i64, file_size: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE record_tasks SET status = 'finalizing', end_time = datetime('now'), file_size = ?1 WHERE id = ?2 AND status IN ('recording', 'finalizing')",
            rusqlite::params![file_size, id],
        )?;
        Ok(())
    }

    pub fn finish_task(
        &self,
        id: i64,
        status: &str,
        file_path: Option<&str>,
        file_size: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE record_tasks SET status = ?1, end_time = COALESCE(end_time, datetime('now')), file_path = COALESCE(?2, file_path), file_size = ?3 WHERE id = ?4",
            rusqlite::params![status, file_path, file_size, id],
        )?;
        Ok(())
    }

    pub fn reconcile_incomplete_tasks(&self) -> Result<()> {
        self.recover_segments()?;
        let stale_tasks = {
            let mut stmt = self.conn.prepare(
                "SELECT id, file_path FROM record_tasks WHERE status IN ('recording', 'finalizing')",
            )?;
            let tasks = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<Result<Vec<_>>>()?;
            tasks
        };

        for (id, file_path) in stale_tasks {
            if self.get_segment_output(id)?.is_some() {
                let size = self.segment_total_size(id)?;
                self.finish_task(
                    id,
                    if size > 0 { "interrupted" } else { "failed" },
                    None,
                    size,
                )?;
                continue;
            }
            let file_size = file_path
                .as_deref()
                .and_then(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len() as i64)
                .unwrap_or(0);
            let status = if file_size > 0 {
                "interrupted"
            } else {
                "failed"
            };
            self.finish_task(id, status, file_path.as_deref(), file_size)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Database;
    use rusqlite::Connection;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn migration_includes_committed_data_still_in_wal() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let target = dir.path().join("target.db");
        let mut db = Database::new(&source).unwrap();
        db.conn
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        let room = db
            .add_room_full("douyin", "wal-room", "anchor", "title", "", "", false)
            .unwrap();
        assert!(
            std::fs::metadata(dir.path().join("source.db-wal"))
                .unwrap()
                .len()
                > 0
        );
        db.migrate_to(target.to_str().unwrap(), |_| Ok(())).unwrap();
        assert_eq!(db.get_room(room).unwrap().room_id, "wal-room");
        drop(db);
        let reopened = Database::new(&target).unwrap();
        assert_eq!(reopened.get_room(room).unwrap().room_id, "wal-room");
    }

    #[test]
    fn backup_lock_failure_does_not_persist_path_or_leave_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let target = dir.path().join("target.db");
        let mut db = Database::new(&source).unwrap();
        let original_path = db.path().to_path_buf();
        // A write transaction on the source makes sqlite3_backup_step return LOCKED.
        db.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut saved = false;
        let result = db.migrate_to(target.to_str().unwrap(), |_| {
            saved = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!saved);
        assert!(!target.exists());
        assert_eq!(db.path(), original_path);
        db.conn.execute_batch("ROLLBACK").unwrap();
        db.add_room_full("douyin", "still-writable", "anchor", "title", "", "", false)
            .unwrap();
        db.migrate_to(target.to_str().unwrap(), |_| Ok(())).unwrap();
    }

    #[test]
    fn persistence_failure_rolls_back_migration_and_allows_retry() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let target = dir.path().join("target.db");
        let mut db = Database::new(&source).unwrap();
        let original_path = db.path().to_path_buf();
        assert_eq!(
            db.migrate_to(target.to_str().unwrap(), |_| Err("save failed".to_string()))
                .unwrap_err(),
            "save failed"
        );
        assert_eq!(db.path(), original_path);
        assert!(!target.exists());
        let room = db
            .add_room_full("douyin", "after-failure", "anchor", "title", "", "", false)
            .unwrap();
        db.migrate_to(target.to_str().unwrap(), |_| Ok(())).unwrap();
        assert_eq!(db.get_room(room).unwrap().room_id, "after-failure");
    }

    #[test]
    fn reconciles_stale_recordings_from_file_state() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!(
            "douyin-recorder-database-{}-{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&temp_dir).expect("create database test directory");
        let db_path = temp_dir.join("test.db");
        let recording_path = temp_dir.join("recording.flv");
        std::fs::write(&recording_path, b"non-empty recording").expect("write recording fixture");

        let db = Database::new(&db_path).expect("create test database");
        let room_id = db
            .add_room_full("douyin", "123", "anchor", "title", "", "", true)
            .expect("add test room");
        let interrupted_id = db
            .add_task(room_id, "manual")
            .expect("add interrupted task");
        db.update_task_status_and_path(interrupted_id, "recording", recording_path.to_str())
            .expect("mark interrupted fixture recording");
        let failed_id = db.add_task(room_id, "auto").expect("add failed task");
        db.update_task_status(failed_id, "finalizing")
            .expect("mark failed fixture finalizing");

        db.reconcile_incomplete_tasks()
            .expect("reconcile incomplete tasks");

        let interrupted = db.get_task(interrupted_id).expect("read interrupted task");
        assert_eq!(interrupted.status, "interrupted");
        assert_eq!(interrupted.file_size, Some(19));
        assert_eq!(interrupted.trigger, "manual");
        assert!(interrupted.end_time.is_some());
        assert!(!db
            .has_running_tasks_for_room(room_id)
            .expect("interrupted and failed history must not block another recording"));

        let failed = db.get_task(failed_id).expect("read failed task");
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.file_size, Some(0));
        assert_eq!(failed.trigger, "auto");
        assert!(failed.end_time.is_some());

        drop(db);
        let _ = std::fs::remove_file(recording_path);
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_dir(temp_dir);
    }

    #[test]
    fn migrates_legacy_database_with_safe_defaults() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!(
            "douyin-recorder-migration-{}-{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&temp_dir).expect("create migration test directory");
        let db_path = temp_dir.join("legacy.db");

        {
            let connection = Connection::open(&db_path).expect("create legacy database");
            connection
                .execute_batch(
                    "CREATE TABLE live_rooms (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        platform TEXT NOT NULL,
                        room_id TEXT NOT NULL,
                        anchor_name TEXT DEFAULT '',
                        room_title TEXT DEFAULT '',
                        cover_url TEXT DEFAULT '',
                        is_live BOOLEAN DEFAULT 0,
                        created_at TEXT DEFAULT (datetime('now'))
                    );
                    CREATE TABLE record_tasks (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        room_id INTEGER NOT NULL,
                        status TEXT DEFAULT 'waiting',
                        start_time TEXT DEFAULT (datetime('now')),
                        end_time TEXT,
                        file_path TEXT,
                        file_size INTEGER
                    );
                    INSERT INTO live_rooms (platform, room_id, anchor_name)
                    VALUES ('douyin', 'legacy-room', 'legacy-anchor');
                    INSERT INTO record_tasks (room_id, status) VALUES (1, 'completed');",
                )
                .expect("write legacy schema");
        }

        let db = Database::new(&db_path).expect("migrate legacy database");
        let room = db.get_room(1).expect("read migrated room");
        assert!(!room.auto_record_enabled);
        assert_eq!(room.auto_record_daily_time, None);
        assert_eq!(room.auto_record_until, None);
        assert_eq!(room.last_schedule_trigger_date, None);
        assert_eq!(
            room.auto_monitor_mode,
            crate::auto_policy::AutoMonitorMode::Window
        );
        assert_eq!(room.auto_record_retry_at, None);
        assert_eq!(room.auto_record_error, None);
        assert_eq!(room.auto_record_revision, 0);
        let task = db.get_task(1).expect("read migrated task");
        assert_eq!(task.trigger, "manual");

        drop(db);
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_dir(temp_dir);
    }

    #[test]
    fn monitoring_config_round_trips_and_migration_is_idempotent() {
        use crate::auto_policy::{self, AutoMonitorMode};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automation.db");
        let db = Database::new(&path).unwrap();
        let id = db
            .add_room_full("douyin", "room", "anchor", "", "", "", false)
            .unwrap();
        let mut room = db.get_room(id).unwrap();
        assert_eq!(room.auto_monitor_mode, AutoMonitorMode::Window);
        let now = chrono::Local::now();
        auto_policy::configure(
            &mut room,
            AutoMonitorMode::Continuous,
            Some("00:00".into()),
            6,
            now,
        )
        .unwrap();
        auto_policy::set_enabled(&mut room, true, 6, now.with_timezone(&chrono::Utc));
        room.auto_record_retry_at = Some((now + chrono::Duration::minutes(30)).to_rfc3339());
        room.auto_record_error = Some("限流，等待重试".into());
        let saved = db.save_room_automation(&room).unwrap();
        drop(db);
        for _ in 0..2 {
            let db = Database::new(&path).unwrap();
            let loaded = db.get_all_rooms().unwrap().pop().unwrap();
            assert_eq!(loaded.auto_monitor_mode, AutoMonitorMode::Continuous);
            assert!(loaded.auto_record_enabled);
            assert_eq!(loaded.auto_record_daily_time.as_deref(), Some("00:00"));
            assert_eq!(loaded.auto_record_until, None);
            assert_eq!(loaded.auto_record_retry_at, saved.auto_record_retry_at);
            assert_eq!(loaded.auto_record_error, saved.auto_record_error);
            assert_eq!(loaded.auto_record_revision, saved.auto_record_revision);
        }
    }

    #[test]
    fn old_network_result_cannot_override_switch_or_mode_changes() {
        use crate::auto_policy::{self, AutoMonitorMode};
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("automation.db")).unwrap();
        let id = db
            .add_room_full("douyin", "room", "anchor", "", "", "", false)
            .unwrap();
        let now = chrono::Utc::now();
        let mut room = db.get_room(id).unwrap();
        auto_policy::set_enabled(&mut room, true, 6, now);
        let snapshot = db.save_room_automation(&room).unwrap();
        assert!(auto_policy::accepts_check(
            &snapshot,
            snapshot.auto_record_revision,
            false,
            now
        ));
        auto_policy::set_enabled(&mut room, false, 6, now);
        db.save_room_automation(&room).unwrap();
        auto_policy::set_enabled(&mut room, true, 6, now);
        let current = db.save_room_automation(&room).unwrap();
        assert!(!auto_policy::accepts_check(
            &current,
            snapshot.auto_record_revision,
            false,
            now
        ));
        assert!(auto_policy::accepts_check(
            &current,
            current.auto_record_revision,
            false,
            now
        ));
        assert!(!auto_policy::accepts_check(
            &current,
            current.auto_record_revision,
            true,
            now
        ));
        auto_policy::configure(
            &mut room,
            AutoMonitorMode::Continuous,
            None,
            6,
            chrono::Local::now(),
        )
        .unwrap();
        let changed = db.save_room_automation(&room).unwrap();
        assert!(!auto_policy::accepts_check(
            &changed,
            current.auto_record_revision,
            false,
            now
        ));
    }

    #[test]
    fn task_completion_and_monitoring_state_commit_or_rollback_together() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("automation.db")).unwrap();
        let id = db
            .add_room_full("douyin", "room", "anchor", "", "", "", false)
            .unwrap();
        let task = db.add_task(id, "auto").unwrap();
        db.update_task_status(task, "finalizing").unwrap();
        let mut room = db.get_room(id).unwrap();
        room.id = id + 1; // Force the room update to fail after updating the task.
        assert!(db
            .finish_task_and_automation(task, "interrupted", None, 123, &room)
            .is_err());
        assert_eq!(db.get_task(task).unwrap().status, "finalizing");
        assert!(db.has_running_tasks_for_room(id).unwrap());
        room.id = id;
        room.auto_record_enabled = true;
        room.auto_record_retry_at = Some("2099-01-01T00:00:00Z".into());
        room.auto_record_error = Some("录制异常，等待重试".into());
        let (saved_task, saved_room) = db
            .finish_task_and_automation(task, "interrupted", None, 123, &room)
            .unwrap();
        assert_eq!(saved_task.status, "interrupted");
        assert_eq!(saved_task.file_size, Some(123));
        assert_eq!(saved_room.auto_record_retry_at, room.auto_record_retry_at);
        assert!(!db.has_running_tasks_for_room(id).unwrap());
    }
}
