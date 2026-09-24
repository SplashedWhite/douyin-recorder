use std::path::Path;
use tauri::{AppHandle, Manager};

// All automatic and manual remuxes share one FIFO lock. Only metadata waits in RAM.
static QUEUE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn remux(file_path: String) -> Result<(String, i64), String> {
    remux_with(crate::resolve_ffmpeg_path(), file_path).await
}

async fn remux_with(ffmpeg: String, file_path: String) -> Result<(String, i64), String> {
    let _queue = QUEUE.lock().await;
    tokio::task::spawn_blocking(move || remux_file(&ffmpeg, &file_path))
        .await
        .map_err(|e| format!("转换任务异常: {e}"))?
}

pub fn remux_file(ffmpeg: &str, file_path: &str) -> Result<(String, i64), String> {
    let source = Path::new(file_path);
    if source.extension().is_none_or(|ext| ext != "flv") {
        return Err("文件不是 FLV 格式，无需转换".into());
    }
    let destination = source.with_extension("mp4");
    if destination.exists() {
        return Err("同名 MP4 已存在，已保留 FLV，请先处理同名文件再重试".into());
    }
    let temporary = tempfile::Builder::new()
        .prefix(".douyin-remux-")
        .suffix(".mp4")
        .tempfile_in(source.parent().ok_or("文件目录无效")?)
        .map_err(|e| e.to_string())?;
    let mut command = std::process::Command::new(ffmpeg);
    command
        .args([
            "-y",
            "-nostdin",
            "-loglevel",
            "error",
            "-i",
            file_path,
            "-c",
            "copy",
            "-f",
            "mp4",
        ])
        .arg(temporary.path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let result = command.output().map_err(|e| format!("启动转换失败: {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "FFmpeg 转换失败: {}",
            String::from_utf8_lossy(&result.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        ));
    }
    let size = temporary
        .as_file()
        .metadata()
        .map_err(|e| e.to_string())?
        .len() as i64;
    if size == 0 {
        return Err("转换完成但输出文件为空".into());
    }
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary
        .persist_noclobber(&destination)
        .map_err(|e| format!("保存 MP4 失败: {}", e.error))?;
    // The caller first commits the new path to the database, then removes the FLV.
    Ok((destination.to_string_lossy().into_owned(), size))
}

pub async fn convert_claimed_segment(app: AppHandle, id: i64) -> Result<String, String> {
    let _queue = QUEUE.lock().await;
    let state = app.state::<crate::AppState>();
    let segment = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.set_conversion_running(id).map_err(|e| e.to_string())?;
        db.get_segment(id).map_err(|e| e.to_string())?
    };
    crate::segment_runtime::emit_segments(&app, segment.task_id)?;
    let source = segment.file_path.clone();
    let ffmpeg = crate::resolve_ffmpeg_path();
    let result = tokio::task::spawn_blocking(move || remux_file(&ffmpeg, &source))
        .await
        .map_err(|e| format!("转换任务异常: {e}"))
        .and_then(|result| result);
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.finish_segment_conversion(id, &result)
            .map_err(|e| e.to_string())?;
    }
    if result.is_ok() {
        if let Err(error) = std::fs::remove_file(&segment.file_path) {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            db.conn
                .execute(
                    "UPDATE record_segments SET conversion_error = ?2 WHERE id = ?1",
                    rusqlite::params![id, format!("MP4 已保存，原 FLV 删除失败: {error}")],
                )
                .map_err(|e| e.to_string())?;
        }
    }
    crate::segment_runtime::emit_segments(&app, segment.task_id)?;
    result.map(|(path, _)| path)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};

    #[tokio::test]
    async fn queued_conversions_hold_exit_until_both_files_are_saved() {
        let dir = tempfile::tempdir().unwrap();
        let source = crate::segment_tests::fixture(dir.path(), "0.5");
        let second = dir.path().join("second.flv");
        std::fs::copy(&source, &second).unwrap();
        let lifecycle = Arc::new(crate::lifecycle::Lifecycle::default());
        let gate = QUEUE.lock().await;
        let mut jobs = vec![];
        for path in [source, second] {
            let operation = lifecycle.operation().unwrap();
            jobs.push(tokio::spawn(async move {
                let _operation = operation;
                remux_with(
                    crate::segment_tests::ffmpeg()
                        .to_string_lossy()
                        .into_owned(),
                    path.to_string_lossy().into_owned(),
                )
                .await
            }));
        }
        assert!(lifecycle.begin_exit());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), lifecycle.wait_for_operations())
                .await
                .is_err()
        );
        assert!(jobs.iter().all(|job| !job.is_finished()));
        assert!(!dir.path().join("input.mp4").exists());
        drop(gate);
        for job in jobs {
            let (path, size) = job.await.unwrap().unwrap();
            assert!(size > 0 && Path::new(&path).exists());
        }
        tokio::time::timeout(Duration::from_secs(2), lifecycle.wait_for_operations())
            .await
            .unwrap()
            .unwrap();
    }
}
