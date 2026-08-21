use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
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
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn new(db_path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::InvalidParameterName(format!("创建数据库目录失败: {}", e))
            })?;
        }
        let conn = Connection::open(db_path)?;
        let db = Database { conn };
        db.init_tables()?;
        Ok(db)
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

        Ok(())
    }

    pub fn get_all_rooms(&self) -> Result<Vec<LiveRoom>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, platform, room_id, anchor_name, room_title, cover_url, avatar_url, is_live, created_at, auto_record_enabled, auto_record_daily_time, auto_record_until, last_schedule_trigger_date FROM live_rooms"
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
                })
            })?
            .collect::<Result<Vec<_>>>()?;

        Ok(rooms)
    }

    pub fn get_room(&self, id: i64) -> Result<LiveRoom> {
        self.conn.query_row(
            "SELECT id, platform, room_id, anchor_name, room_title, cover_url, avatar_url, is_live, created_at, auto_record_enabled, auto_record_daily_time, auto_record_until, last_schedule_trigger_date FROM live_rooms WHERE id = ?1",
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

    pub fn set_room_auto_record(&self, id: i64, enabled: bool, until: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE live_rooms SET auto_record_enabled = ?1, auto_record_until = ?2 WHERE id = ?3",
            rusqlite::params![enabled, until, id],
        )?;
        Ok(())
    }

    pub fn set_room_auto_schedule(
        &self,
        id: i64,
        daily_time: Option<&str>,
        last_trigger_date: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE live_rooms SET auto_record_daily_time = ?1, last_schedule_trigger_date = ?2 WHERE id = ?3",
            rusqlite::params![daily_time, last_trigger_date, id],
        )?;
        Ok(())
    }

    pub fn trigger_room_auto_schedule(&self, id: i64, until: &str, date: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE live_rooms SET auto_record_enabled = 1, auto_record_until = ?1, last_schedule_trigger_date = ?2 WHERE id = ?3",
            rusqlite::params![until, date, id],
        )?;
        Ok(())
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
            "SELECT COUNT(*) FROM record_tasks WHERE room_id = ?1 AND status IN ('recording', 'finalizing')",
            [room_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn delete_room_cascade(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM record_tasks WHERE room_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM live_rooms WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn delete_task(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM record_tasks WHERE id = ?1", [id])?;
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
        let task = db.get_task(1).expect("read migrated task");
        assert_eq!(task.trigger, "manual");

        drop(db);
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_dir(temp_dir);
    }
}
