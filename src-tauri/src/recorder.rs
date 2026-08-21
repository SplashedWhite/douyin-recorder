use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{oneshot, watch};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const STDERR_TAIL_LINES: usize = 50;
const STOP_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub struct RecordingExit {
    pub manually_stopped: bool,
    pub status_success: bool,
    pub wait_error: Option<String>,
    pub stderr_tail: Vec<String>,
}

struct ActiveRecording {
    stop_tx: Option<oneshot::Sender<()>>,
    completion_rx: watch::Receiver<Option<Result<(), String>>>,
}

pub struct Recorder {
    active_records: Arc<Mutex<HashMap<i64, ActiveRecording>>>,
    ffmpeg_path: String,
}

impl Recorder {
    pub fn new(ffmpeg_path: String) -> Self {
        Recorder {
            active_records: Arc::new(Mutex::new(HashMap::new())),
            ffmpeg_path,
        }
    }

    pub fn start_record<F, Fut>(
        &self,
        task_id: i64,
        stream_url: &str,
        output_path: &str,
        proxy: &str,
        on_exit: F,
    ) -> Result<(), String>
    where
        F: FnOnce(RecordingExit) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        if stream_url.is_empty() {
            return Err("直播流地址为空，主播可能未开播".to_string());
        }

        if self.is_active(task_id) {
            return Err("该录制任务已经在运行".to_string());
        }

        if let Some(parent) = std::path::Path::new(output_path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败: {}", e))?;
        }

        let mut cmd = Command::new(&self.ffmpeg_path);
        cmd.args([
            "-y",
            "-nostdin",
            "-loglevel",
            "warning",
            "-nostats",
            "-rw_timeout",
            "60000000",
            "-i",
            stream_url,
            "-c",
            "copy",
            "-f",
            "flv",
            output_path,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

        #[cfg(windows)]
        cmd.as_std_mut().creation_flags(0x08000000); // CREATE_NO_WINDOW

        if !proxy.is_empty() {
            cmd.env("http_proxy", proxy);
            cmd.env("https_proxy", proxy);
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "ffmpeg 未找到".to_string()
            } else {
                format!("启动 ffmpeg 录制失败: {}", e)
            }
        })?;

        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        let stderr_task = child.stderr.take().map(|stderr| {
            let stderr_tail = Arc::clone(&stderr_tail);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(mut tail) = stderr_tail.lock() {
                        if tail.len() == STDERR_TAIL_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                }
            })
        });

        let (stop_tx, mut stop_rx) = oneshot::channel();
        let (completion_tx, completion_rx) = watch::channel(None);

        {
            let mut records = self.active_records.lock().map_err(|e| e.to_string())?;
            records.insert(
                task_id,
                ActiveRecording {
                    stop_tx: Some(stop_tx),
                    completion_rx,
                },
            );
        }

        let active_records = Arc::clone(&self.active_records);
        tokio::spawn(async move {
            let (manually_stopped, status_success, wait_error) = tokio::select! {
                result = child.wait() => match result {
                    Ok(status) => (false, status.success(), None),
                    Err(error) => (false, false, Some(error.to_string())),
                },
                _ = &mut stop_rx => {
                    let kill_error = child.start_kill().err().map(|error| error.to_string());
                    match child.wait().await {
                        Ok(status) => (true, status.success(), kill_error),
                        Err(error) => (true, false, Some(error.to_string())),
                    }
                }
            };

            if let Some(stderr_task) = stderr_task {
                let _ = stderr_task.await;
            }

            let stderr_tail = stderr_tail
                .lock()
                .map(|tail| tail.iter().cloned().collect())
                .unwrap_or_default();

            let result = on_exit(RecordingExit {
                manually_stopped,
                status_success,
                wait_error,
                stderr_tail,
            })
            .await;

            let _ = completion_tx.send(Some(result.clone()));
            if let Ok(mut records) = active_records.lock() {
                records.remove(&task_id);
            }
        });

        Ok(())
    }

    pub async fn stop_record(&self, task_id: i64) -> Result<bool, String> {
        let (stop_tx, mut completion_rx) = {
            let mut records = self.active_records.lock().map_err(|e| e.to_string())?;
            let Some(recording) = records.get_mut(&task_id) else {
                return Ok(false);
            };
            (recording.stop_tx.take(), recording.completion_rx.clone())
        };

        if let Some(stop_tx) = stop_tx {
            let _ = stop_tx.send(());
        }

        tokio::time::timeout(STOP_WAIT_TIMEOUT, async {
            loop {
                if let Some(result) = completion_rx.borrow().clone() {
                    return result;
                }
                completion_rx
                    .changed()
                    .await
                    .map_err(|_| "录制进程状态通道已关闭".to_string())?;
            }
        })
        .await
        .map_err(|_| "等待录制进程停止超时".to_string())??;

        Ok(true)
    }

    pub fn is_active(&self, task_id: i64) -> bool {
        self.active_records
            .lock()
            .map(|records| records.contains_key(&task_id))
            .unwrap_or(false)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Ok(mut records) = self.active_records.lock() {
            for (_, mut recording) in records.drain() {
                if let Some(stop_tx) = recording.stop_tx.take() {
                    let _ = stop_tx.send(());
                }
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::Recorder;
    use std::os::windows::process::CommandExt;
    use std::process::Command as StdCommand;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn observes_natural_ffmpeg_exit_and_removes_active_record() {
        let ffmpeg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join("ffmpeg-x86_64-pc-windows-msvc.exe");
        if !ffmpeg.exists() {
            return;
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!(
            "douyin-recorder-lifecycle-{}-{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&temp_dir).expect("create lifecycle test directory");
        let input_path = temp_dir.join("finite-input.flv");
        let output_path = temp_dir.join("finite-output.flv");

        let mut generator = StdCommand::new(&ffmpeg);
        generator
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=32x32:r=1",
                "-t",
                "1",
                "-c:v",
                "flv",
                "-f",
                "flv",
            ])
            .arg(&input_path)
            .creation_flags(0x08000000);
        assert!(generator
            .status()
            .expect("run ffmpeg fixture generator")
            .success());

        let recorder = Recorder::new(ffmpeg.to_string_lossy().to_string());
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        recorder
            .start_record(
                42,
                input_path.to_str().expect("input path is utf-8"),
                output_path.to_str().expect("output path is utf-8"),
                "",
                move |exit| async move {
                    let _ = exit_tx.send(exit);
                    Ok(())
                },
            )
            .expect("start finite recording");

        let exit = tokio::time::timeout(Duration::from_secs(10), exit_rx)
            .await
            .expect("ffmpeg did not exit in time")
            .expect("recording exit callback dropped");
        assert!(!exit.manually_stopped);
        assert!(exit.status_success);
        assert!(
            std::fs::metadata(&output_path)
                .expect("output file exists")
                .len()
                > 0
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!recorder.is_active(42));

        let _ = std::fs::remove_file(input_path);
        let _ = std::fs::remove_file(output_path);
        let _ = std::fs::remove_dir(temp_dir);
    }

    #[tokio::test]
    #[ignore = "takes about 60 seconds to verify the configured read timeout"]
    async fn ends_stalled_http_input_after_read_timeout() {
        let ffmpeg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join("ffmpeg-x86_64-pc-windows-msvc.exe");
        if !ffmpeg.exists() {
            return;
        }

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stalled server");
        let address = listener.local_addr().expect("read stalled server address");
        let _server = std::thread::spawn(move || {
            if let Ok((_socket, _)) = listener.accept() {
                std::thread::sleep(Duration::from_secs(70));
            }
        });

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let output_path = std::env::temp_dir().join(format!(
            "douyin-recorder-stalled-{}-{}.flv",
            std::process::id(),
            nonce
        ));
        let input_url = format!("http://{}/stalled.flv", address);
        let recorder = Recorder::new(ffmpeg.to_string_lossy().to_string());
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        let started_at = std::time::Instant::now();
        recorder
            .start_record(
                43,
                &input_url,
                output_path.to_str().expect("output path is utf-8"),
                "",
                move |exit| async move {
                    let _ = exit_tx.send(exit);
                    Ok(())
                },
            )
            .expect("start stalled recording");

        let exit = tokio::time::timeout(Duration::from_secs(75), exit_rx)
            .await
            .expect("stalled ffmpeg did not honor read timeout")
            .expect("recording exit callback dropped");
        let elapsed = started_at.elapsed();
        assert!(!exit.manually_stopped);
        assert!(elapsed >= Duration::from_secs(55));
        assert!(elapsed < Duration::from_secs(75));

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!recorder.is_active(43));
        let _ = std::fs::remove_file(output_path);
    }
}
