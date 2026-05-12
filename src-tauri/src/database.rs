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
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn new(db_path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| rusqlite::Error::InvalidParameterName(format!("创建数据库目录失败: {}", e)))?;
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
                created_at TEXT DEFAULT (datetime('now'))
            )",
            [],
        )?;

        // Migration: add avatar_url column for existing databases
        let _ = self.conn.execute(
            "ALTER TABLE live_rooms ADD COLUMN avatar_url TEXT DEFAULT ''",
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
                FOREIGN KEY (room_id) REFERENCES live_rooms(id)
            )",
            [],
        )?;

        Ok(())
    }

    pub fn get_all_rooms(&self) -> Result<Vec<LiveRoom>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, platform, room_id, anchor_name, room_title, cover_url, avatar_url, is_live, created_at FROM live_rooms"
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
                })
            })?
            .collect::<Result<Vec<_>>>()?;

        Ok(rooms)
    }

    pub fn add_room(&self, platform: &str, room_id: &str, anchor_name: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO live_rooms (platform, room_id, anchor_name) VALUES (?1, ?2, ?3)",
            [platform, room_id, anchor_name],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

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

    pub fn delete_room(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM live_rooms WHERE id = ?1", [id])?;
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

    pub fn delete_room_cascade(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM record_tasks WHERE room_id = ?1", [id])?;
        self.conn.execute("DELETE FROM live_rooms WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn delete_task(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM record_tasks WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn get_all_tasks(&self) -> Result<Vec<RecordTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, room_id, status, start_time, end_time, file_path, file_size FROM record_tasks ORDER BY id DESC"
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
                })
            })?
            .collect::<Result<Vec<_>>>()?;

        Ok(tasks)
    }

    pub fn add_task(&self, room_id: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO record_tasks (room_id) VALUES (?1)",
            [room_id],
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

    pub fn update_task_status_and_path(&self, id: i64, status: &str, file_path: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE record_tasks SET status = ?1, file_path = COALESCE(?2, file_path) WHERE id = ?3",
            rusqlite::params![status, file_path, id],
        )?;
        Ok(())
    }

    pub fn complete_task(&self, id: i64, file_path: &str, file_size: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE record_tasks SET status = 'completed', end_time = datetime('now'), file_path = ?1, file_size = ?2 WHERE id = ?3",
            [file_path, &file_size.to_string(), &id.to_string()],
        )?;
        Ok(())
    }

    pub fn update_task_file_size(&self, id: i64, file_size: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE record_tasks SET file_size = ?1 WHERE id = ?2",
            rusqlite::params![file_size, id],
        )?;
        Ok(())
    }
}