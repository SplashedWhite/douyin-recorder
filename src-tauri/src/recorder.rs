use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub struct RecordingProcess {
    pub child: Child,
    pub output_path: String,
}

pub struct Recorder {
    active_records: Mutex<HashMap<i64, RecordingProcess>>,
    ffmpeg_path: String,
}

impl Recorder {
    pub fn new(ffmpeg_path: String) -> Self {
        Recorder {
            active_records: Mutex::new(HashMap::new()),
            ffmpeg_path,
        }
    }

    pub fn start_record(
        &self,
        task_id: i64,
        stream_url: &str,
        output_path: &str,
        proxy: &str,
    ) -> Result<(), String> {
        if stream_url.is_empty() {
            return Err("直播流地址为空，主播可能未开播".to_string());
        }

        if let Some(parent) = std::path::Path::new(output_path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建输出目录失败: {}", e))?;
        }

        let mut cmd = Command::new(&self.ffmpeg_path);
        cmd.args([
                "-y",
                "-i", stream_url,
                "-c", "copy",
                "-f", "flv",
                output_path,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

        if !proxy.is_empty() {
            cmd.env("http_proxy", proxy);
            cmd.env("https_proxy", proxy);
        }

        let child = cmd
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "ffmpeg 未找到".to_string()
                } else {
                    format!("启动 ffmpeg 录制失败: {}", e)
                }
            })?;

        let process = RecordingProcess {
            child,
            output_path: output_path.to_string(),
        };

        let mut records = self.active_records.lock().map_err(|e| e.to_string())?;
        records.insert(task_id, process);

        Ok(())
    }

    pub fn stop_record(&self, task_id: i64) -> Result<(), String> {
        let mut records = self.active_records.lock().map_err(|e| e.to_string())?;
        if let Some(mut process) = records.remove(&task_id) {
            process.child.kill().map_err(|e| format!("停止录制失败: {}", e))?;
            let _ = process.child.wait();
        }
        Ok(())
    }

    pub fn is_recording(&self, task_id: i64) -> bool {
        let records = self.active_records.lock().unwrap();
        records.contains_key(&task_id)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Ok(mut records) = self.active_records.lock() {
            for (_, mut process) in records.drain() {
                let _ = process.child.kill();
                let _ = process.child.wait();
            }
        }
    }
}
