use crate::segments::{ManifestReader, SegmentOutput};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::watch;

pub fn emit_segments(app: &AppHandle, task_id: i64) -> Result<(), String> {
    let state = app.state::<crate::AppState>();
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let task = db.get_task(task_id).map_err(|e| e.to_string())?;
    // Publishing under the DB lock keeps successive snapshots ordered.
    let _ = app.emit("recording-segments-changed", task);
    Ok(())
}

pub async fn monitor(
    app: AppHandle,
    task_id: i64,
    output: SegmentOutput,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    let mut reader = ManifestReader::default();
    let mut entries = vec![];
    let mut jobs = tokio::task::JoinSet::new();
    let mut last_snapshot = String::new();
    let failure = loop {
        let stopped = *stop.borrow() || stop.has_changed().is_err();
        let observation = (|| -> Result<Vec<i64>, String> {
            entries.extend(reader.read(&output)?);
            let state = app.state::<crate::AppState>();
            let db = state.db.lock().map_err(|e| e.to_string())?;
            db.observe_segments(task_id, &output, &entries)
                .map_err(|e| e.to_string())?;
            let mut ids = vec![];
            if crate::settings::load_settings().auto_convert_mp4 {
                for segment in db.get_segments(task_id).map_err(|e| e.to_string())? {
                    if segment.status == "completed"
                        && segment.conversion_state == "idle"
                        && db
                            .claim_segment_conversion(segment.id)
                            .map_err(|e| e.to_string())?
                    {
                        ids.push(segment.id);
                    }
                }
            }
            let task = db.get_task(task_id).map_err(|e| e.to_string())?;
            let snapshot = serde_json::to_string(&task).map_err(|e| e.to_string())?;
            if snapshot != last_snapshot {
                let _ = app.emit("recording-segments-changed", task);
                last_snapshot = snapshot;
            }
            Ok(ids)
        })();
        let failure = match observation {
            Ok(ids) => {
                for id in ids {
                    let app = app.clone();
                    jobs.spawn(
                        async move { crate::conversion::convert_claimed_segment(app, id).await },
                    );
                }
                None
            }
            Err(error) => Some(error),
        };
        while jobs.try_join_next().is_some() {}
        if stopped {
            break failure;
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {},
            _ = stop.changed() => {},
        }
    };
    // Recording completion (and application exit) includes all queued remuxes.
    while jobs.join_next().await.is_some() {}
    failure.map_or(Ok(()), Err)
}

pub async fn finish_segments(app: &AppHandle, task_id: i64, status: &str) -> Result<(), String> {
    let state = app.state::<crate::AppState>();
    let ids = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let task = db.get_task(task_id).map_err(|e| e.to_string())?;
        let output = task.segment_output.as_ref().ok_or("找不到分段设置")?;
        let entries = ManifestReader::read_all(output)?;
        db.observe_segments(task_id, output, &entries)
            .map_err(|e| e.to_string())?;
        let mut ids = vec![];
        for segment in db.get_segments(task_id).map_err(|e| e.to_string())? {
            if segment.status == "recording" {
                let end = entries
                    .iter()
                    .find(|entry| entry.index == segment.segment_index)
                    .map(|entry| crate::segments::segment_time(&task.start_time, entry.end))
                    .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
                db.close_segment(segment.id, status, &end)
                    .map_err(|e| e.to_string())?;
            }
            let updated = db.get_segment(segment.id).map_err(|e| e.to_string())?;
            if crate::settings::load_settings().auto_convert_mp4
                && updated.status == "completed"
                && updated.conversion_state == "idle"
                && db
                    .claim_segment_conversion(segment.id)
                    .map_err(|e| e.to_string())?
            {
                ids.push(segment.id);
            }
        }
        ids
    };
    emit_segments(app, task_id)?;
    for id in ids {
        // Individual conversion failures remain visible on the segment and do not stop recording.
        let _ = crate::conversion::convert_claimed_segment(app.clone(), id).await;
    }
    Ok(())
}
