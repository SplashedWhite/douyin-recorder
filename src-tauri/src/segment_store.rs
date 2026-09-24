use crate::database::Database;
use crate::segments::{segment_time, ManifestEntry, ManifestReader, RecordSegment, SegmentOutput};
use rusqlite::{params, OptionalExtension, Result};

impl Database {
    pub(crate) fn init_segments(&self) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS segment_outputs (
            task_id INTEGER PRIMARY KEY, output_json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS record_segments (
                id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL,
                segment_index INTEGER NOT NULL, file_path TEXT NOT NULL, file_size INTEGER NOT NULL DEFAULT 0,
                start_time TEXT NOT NULL, end_time TEXT, status TEXT NOT NULL DEFAULT 'recording',
                conversion_state TEXT NOT NULL DEFAULT 'idle', conversion_error TEXT,
                deleted INTEGER NOT NULL DEFAULT 0, UNIQUE(task_id, segment_index));")
    }

    pub fn set_segment_output(&self, task_id: i64, output: &SegmentOutput) -> Result<()> {
        let json = serde_json::to_string(output)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.conn.execute(
            "INSERT INTO segment_outputs (task_id, output_json) VALUES (?1, ?2)",
            params![task_id, json],
        )?;
        Ok(())
    }

    pub fn get_segment_output(&self, task_id: i64) -> Result<Option<SegmentOutput>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT output_json FROM segment_outputs WHERE task_id = ?1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| {
            serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .transpose()
    }

    pub fn get_segments(&self, task_id: i64) -> Result<Vec<RecordSegment>> {
        let mut statement = self.conn.prepare("SELECT id, task_id, segment_index, file_path, file_size, start_time, end_time, status, conversion_state, conversion_error, deleted FROM record_segments WHERE task_id = ?1 ORDER BY segment_index")?;
        let rows = statement
            .query_map([task_id], read_segment)?
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_segment(&self, id: i64) -> Result<RecordSegment> {
        self.conn.query_row("SELECT id, task_id, segment_index, file_path, file_size, start_time, end_time, status, conversion_state, conversion_error, deleted FROM record_segments WHERE id = ?1", [id], read_segment)
    }

    pub fn add_segment(
        &self,
        task_id: i64,
        index: u32,
        path: &str,
        start_time: &str,
    ) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO record_segments (task_id, segment_index, file_path, start_time) VALUES (?1, ?2, ?3, ?4)", params![task_id, index, path, start_time])?;
        Ok(())
    }

    pub fn close_segment(&self, id: i64, status: &str, end_time: &str) -> Result<()> {
        let segment = self.get_segment(id)?;
        let size = crate::file_size(Some(&segment.file_path));
        self.conn.execute("UPDATE record_segments SET status = ?2, end_time = ?3, file_size = ?4 WHERE id = ?1 AND status = 'recording'", params![id, if size == 0 { "failed" } else { status }, end_time, size])?;
        Ok(())
    }

    pub fn segment_total_size(&self, task_id: i64) -> Result<i64> {
        Ok(self
            .get_segments(task_id)?
            .iter()
            .map(|segment| {
                std::fs::metadata(&segment.file_path)
                    .map(|meta| meta.len() as i64)
                    .unwrap_or(segment.file_size)
            })
            .sum())
    }

    pub fn observe_segments(
        &self,
        task_id: i64,
        output: &SegmentOutput,
        entries: &[ManifestEntry],
    ) -> Result<()> {
        let task = self.get_task(task_id)?;
        let files = output
            .files()
            .map_err(rusqlite::Error::InvalidParameterName)?;
        for (&index, path) in &files {
            let start = entries
                .iter()
                .find(|entry| entry.index == index)
                .map(|entry| entry.start)
                .unwrap_or((index as f64 - 1.0) * output.duration_secs as f64);
            self.add_segment(
                task_id,
                index,
                &path.to_string_lossy(),
                &segment_time(&task.start_time, start),
            )?;
        }
        for segment in self.get_segments(task_id)? {
            if segment.status != "recording" {
                continue;
            }
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.index == segment.segment_index)
            {
                self.conn.execute(
                    "UPDATE record_segments SET start_time = ?2 WHERE id = ?1",
                    params![segment.id, segment_time(&task.start_time, entry.start)],
                )?;
                // A manifest entry is emitted just before the output handle closes.
                // The next file can only exist after that close has completed.
                if output.next_exists(segment.segment_index) {
                    self.close_segment(
                        segment.id,
                        "completed",
                        &segment_time(&task.start_time, entry.end),
                    )?;
                }
            }
        }
        Ok(())
    }

    pub fn claim_segment_conversion(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE record_segments SET conversion_state = 'queued', conversion_error = NULL
            WHERE id = ?1 AND deleted = 0 AND status IN ('completed', 'interrupted', 'failed')
            AND conversion_state IN ('idle', 'failed') AND file_path LIKE '%.flv'",
            [id],
        )? == 1)
    }

    pub fn set_conversion_running(&self, id: i64) -> Result<()> {
        self.conn.execute("UPDATE record_segments SET conversion_state = 'converting' WHERE id = ?1 AND conversion_state = 'queued'", [id])?;
        Ok(())
    }

    pub fn finish_segment_conversion(
        &self,
        id: i64,
        result: &std::result::Result<(String, i64), String>,
    ) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        match result {
            Ok((path, size)) => {
                self.conn.execute("UPDATE record_segments SET file_path = ?2, file_size = ?3, conversion_state = 'idle', conversion_error = NULL WHERE id = ?1", params![id, path, size])?;
            }
            Err(error) => {
                self.conn.execute("UPDATE record_segments SET conversion_state = 'failed', conversion_error = ?2 WHERE id = ?1", params![id, error])?;
            }
        }
        let task = self.get_segment(id)?.task_id;
        self.conn.execute(
            "UPDATE record_tasks SET file_size = ?2 WHERE id = ?1",
            params![task, self.segment_total_size(task)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn hide_segment(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute("UPDATE record_segments SET deleted = 1 WHERE id = ?1 AND status != 'recording' AND conversion_state NOT IN ('queued', 'converting')", [id])? == 1)
    }

    pub(crate) fn recover_segments(&self) -> Result<()> {
        let mut statement = self.conn.prepare("SELECT task_id FROM segment_outputs")?;
        let tasks = statement
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>>>()?;
        for task_id in tasks {
            let task = self.get_task(task_id)?;
            if !matches!(task.status.as_str(), "recording" | "finalizing") {
                continue;
            }
            let output = task.segment_output.as_ref().unwrap();
            // Missing/moved media must not prevent the application from starting.
            let entries = ManifestReader::read_all(output).unwrap_or_default();
            if std::path::Path::new(&output.file_prefix)
                .parent()
                .is_some_and(|path| path.is_dir())
            {
                self.observe_segments(task_id, output, &entries)?;
            }
            for segment in self.get_segments(task_id)? {
                if segment.status == "recording" {
                    self.close_segment(
                        segment.id,
                        "interrupted",
                        &chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    )?;
                }
            }
        }
        self.conn.execute("UPDATE record_segments SET conversion_state = 'failed', conversion_error = '上次转换未正常完成，已保留文件，可重试转换' WHERE conversion_state IN ('queued', 'converting')", [])?;
        Ok(())
    }
}

fn read_segment(row: &rusqlite::Row<'_>) -> Result<RecordSegment> {
    Ok(RecordSegment {
        id: row.get(0)?,
        task_id: row.get(1)?,
        segment_index: row.get(2)?,
        file_path: row.get(3)?,
        file_size: row.get(4)?,
        start_time: row.get(5)?,
        end_time: row.get(6)?,
        status: row.get(7)?,
        conversion_state: row.get(8)?,
        conversion_error: row.get(9)?,
        deleted: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn segment_recovery_preserves_finished_files_tombstones_and_automation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recordings.db");
        let db = Database::new(&path).unwrap();
        let room = db
            .add_room_full("douyin", "123", "主播", "", "", "", true)
            .unwrap();
        let task = db.add_task(room, "auto").unwrap();
        let output = SegmentOutput::new(dir.path(), "主播, 100%_123_1", 2);
        db.set_segment_output(task, &output).unwrap();
        db.update_task_status(task, "recording").unwrap();
        std::fs::create_dir_all(Path::new(&output.manifest_path).parent().unwrap()).unwrap();
        for index in 1..=3 {
            std::fs::write(output.path(index), vec![index as u8; 40]).unwrap();
        }
        let csv = (1..=2)
            .map(|index| {
                format!(
                    "\"{}\",{},{}\n",
                    Path::new(&output.path(index))
                        .file_name()
                        .unwrap()
                        .to_string_lossy(),
                    (index - 1) * 2,
                    index * 2
                )
            })
            .collect::<String>();
        std::fs::write(&output.manifest_path, csv).unwrap();
        let entries = ManifestReader::default().read(&output).unwrap();
        db.observe_segments(task, &output, &entries).unwrap();
        let segments = db.get_segments(task).unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|s| s.status.as_str())
                .collect::<Vec<_>>(),
            vec!["completed", "completed", "recording"]
        );
        assert!(!db.hide_segment(segments[2].id).unwrap());
        assert!(!db.claim_segment_conversion(segments[2].id).unwrap());
        assert!(db.hide_segment(segments[0].id).unwrap());
        assert!(!db.claim_segment_conversion(segments[0].id).unwrap());
        assert!(db.claim_segment_conversion(segments[1].id).unwrap());
        assert!(!db.claim_segment_conversion(segments[1].id).unwrap());
        assert!(!db.hide_segment(segments[1].id).unwrap());
        drop(db);

        let db = Database::new(&path).unwrap();
        db.reconcile_incomplete_tasks().unwrap();
        db.reconcile_incomplete_tasks().unwrap();
        let restored = db.get_task(task).unwrap();
        assert_eq!(restored.status, "interrupted");
        assert_eq!(restored.file_size, Some(120));
        assert_eq!(restored.segments.len(), 3);
        assert!(restored.segments[0].deleted);
        assert_eq!(restored.segments[0].status, "completed");
        assert_eq!(restored.segments[1].conversion_state, "failed");
        assert_eq!(restored.segments[2].status, "interrupted");
        assert_eq!(db.get_room(room).unwrap().auto_record_revision, 0);
        assert!(Path::new(&output.path(1)).exists());
        assert!(db.claim_segment_conversion(segments[1].id).unwrap());
        assert!(db.has_running_tasks().unwrap());
        assert!(db.has_running_tasks_for_room(room).unwrap());
        db.finish_segment_conversion(segments[1].id, &Err("模拟失败".into()))
            .unwrap();
        assert!(!db.has_running_tasks().unwrap());
    }

    #[test]
    fn manifest_does_not_close_a_file_until_next_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("db")).unwrap();
        let room = db
            .add_room_full("douyin", "123", "主播", "", "", "", true)
            .unwrap();
        let task = db.add_task(room, "manual").unwrap();
        let output = SegmentOutput::new(dir.path(), "record", 60);
        db.set_segment_output(task, &output).unwrap();
        std::fs::write(output.path(1), b"data").unwrap();
        let entries = vec![ManifestEntry {
            index: 1,
            start: 0.0,
            end: 60.5,
        }];
        db.observe_segments(task, &output, &entries).unwrap();
        assert_eq!(db.get_segments(task).unwrap()[0].status, "recording");
        std::fs::write(output.path(2), b"next").unwrap();
        db.observe_segments(task, &output, &entries).unwrap();
        assert_eq!(db.get_segments(task).unwrap()[0].status, "completed");
        db.delete_task(task).unwrap();
        assert!(db.get_segments(task).unwrap().is_empty());
        assert!(db.get_segment_output(task).unwrap().is_none());
        assert!(Path::new(&output.path(1)).exists());
    }
}
