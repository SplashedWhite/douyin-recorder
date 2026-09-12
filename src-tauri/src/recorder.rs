use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, watch};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const STDERR_TAIL_LINES: usize = 50;
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub struct RecordingExit {
    pub manually_stopped: bool,
    pub status_success: bool,
    pub exit_code: Option<i32>,
    pub forced_stop_reason: Option<String>,
    pub wait_error: Option<String>,
    pub stderr_tail: Vec<String>,
}

impl RecordingExit {
    fn from_status(result: std::io::Result<ExitStatus>, manually_stopped: bool) -> Self {
        let (status_success, exit_code, wait_error) = match result {
            Ok(status) => (status.success(), status.code(), None),
            Err(error) => (false, None, Some(error.to_string())),
        };
        Self {
            manually_stopped,
            status_success,
            exit_code,
            forced_stop_reason: None,
            wait_error,
            stderr_tail: Vec::new(),
        }
    }

    pub fn stopped_cleanly(&self) -> bool {
        self.status_success && self.forced_stop_reason.is_none() && self.wait_error.is_none()
    }
}

async fn stop_ffmpeg(
    child: &mut Child,
    mut stdin: Option<ChildStdin>,
    grace_period: Duration,
) -> RecordingExit {
    let graceful = tokio::time::timeout(grace_period, async {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Ok(status);
        }
        let input = stdin
            .as_mut()
            .ok_or_else(|| "FFmpeg 停止指令管道不可用".to_string())?;
        input
            .write_all(b"q\n")
            .await
            .map_err(|error| format!("发送 FFmpeg 停止指令失败: {error}"))?;
        input
            .flush()
            .await
            .map_err(|error| format!("发送 FFmpeg 停止指令失败: {error}"))?;
        // FFmpeg consumes q before EOF and writes buffered packets and the FLV trailer.
        drop(stdin.take());
        child.wait().await.map_err(|error| error.to_string())
    })
    .await;

    let forced_stop_reason = match graceful {
        Ok(Ok(status)) => return RecordingExit::from_status(Ok(status), true),
        Ok(Err(error)) => error,
        Err(_) => format!("FFmpeg 正常收尾超时（{} 秒）", grace_period.as_secs()),
    };
    // The process may have exited just as stdin closed or the deadline elapsed.
    if let Ok(Some(status)) = child.try_wait() {
        return RecordingExit::from_status(Ok(status), true);
    }
    let kill_error = child.start_kill().err().map(|error| error.to_string());
    let mut exit = RecordingExit::from_status(child.wait().await, true);
    exit.forced_stop_reason = Some(forced_stop_reason);
    if exit.wait_error.is_none() {
        exit.wait_error = kill_error;
    }
    exit
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
            "-stdin",
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
        .stdin(Stdio::piped())
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
        // Child::wait closes child.stdin, even when used in select!. Keep it separately
        // so a later stop request can still ask FFmpeg to finalize its output.
        let stdin = child.stdin.take();

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
            let mut exit = tokio::select! {
                result = child.wait() => RecordingExit::from_status(result, false),
                _ = &mut stop_rx => stop_ffmpeg(&mut child, stdin, GRACEFUL_STOP_TIMEOUT).await,
            };

            if let Some(stderr_task) = stderr_task {
                let _ = stderr_task.await;
            }

            exit.stderr_tail = stderr_tail
                .lock()
                .map(|tail| tail.iter().cloned().collect())
                .unwrap_or_default();

            let result = on_exit(exit).await;

            if let Ok(mut records) = active_records.lock() {
                records.remove(&task_id);
            }
            let _ = completion_tx.send(Some(result));
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

    pub fn has_active_records(&self) -> Result<bool, String> {
        self.active_records
            .lock()
            .map(|records| !records.is_empty())
            .map_err(|e| e.to_string())
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
    use super::{stop_ffmpeg, Recorder};
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command as StdCommand;
    use std::process::Stdio;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;

    fn bundled_ffmpeg() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join("ffmpeg-x86_64-pc-windows-msvc.exe")
    }

    fn assert_complete_flv(path: &Path) -> f64 {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[..3], b"FLV");
        let data_offset = u32::from_be_bytes(bytes[5..9].try_into().unwrap()) as usize;
        assert_eq!(&bytes[data_offset..data_offset + 4], &[0, 0, 0, 0]);
        let mut offset = data_offset + 4;
        let mut tags = 0;
        while offset < bytes.len() {
            assert!(offset + 11 <= bytes.len(), "incomplete FLV tag header");
            let size = ((bytes[offset + 1] as usize) << 16)
                | ((bytes[offset + 2] as usize) << 8)
                | bytes[offset + 3] as usize;
            let end = offset + 11 + size;
            assert!(
                end + 4 <= bytes.len(),
                "incomplete FLV packet at end of recording"
            );
            let previous_size = u32::from_be_bytes(bytes[end..end + 4].try_into().unwrap());
            assert_eq!(previous_size as usize, 11 + size);
            offset = end + 4;
            tags += 1;
        }
        assert!(tags > 2, "recording must contain media packets");
        let duration_key = b"\x00\x08duration\x00";
        let value_start = bytes
            .windows(duration_key.len())
            .position(|window| window == duration_key)
            .expect("FLV duration metadata")
            + duration_key.len();
        let duration = f64::from_be_bytes(bytes[value_start..value_start + 8].try_into().unwrap());
        assert!(duration > 0.0, "graceful stop must update FLV duration");
        duration
    }

    #[tokio::test]
    async fn manual_stop_flushes_live_h264_aac_and_handles_duplicate_requests() {
        let ffmpeg = bundled_ffmpeg();
        assert!(
            ffmpeg.exists(),
            "bundled FFmpeg required for recording regression tests"
        );
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("live-input.flv");
        let output = dir.path().join("stopped.flv");
        let mut generator = Command::new(&ffmpeg);
        generator
            .args([
                "-y",
                "-nostdin",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=320x180:r=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "30",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "30",
                "-c:a",
                "aac",
                "-f",
                "flv",
            ])
            .arg(&input)
            .kill_on_drop(true);
        generator.as_std_mut().creation_flags(0x08000000);
        let generated = tokio::time::timeout(Duration::from_secs(20), generator.output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );

        // A paced local HTTP source exercises the same startup/stop path as a live room.
        let media = std::fs::read(&input).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/live.flv", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let byte = socket.read_u8().await.unwrap();
                request.push(byte);
            }
            let header = format!("HTTP/1.1 200 OK\r\nContent-Type: video/x-flv\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", media.len());
            if socket.write_all(header.as_bytes()).await.is_err() {
                return;
            }
            for chunk in media.chunks(16 * 1024) {
                if socket.write_all(chunk).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        });
        let recorder = Recorder::new(ffmpeg.to_string_lossy().to_string());
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        recorder
            .start_record(
                74,
                &url,
                output.to_str().unwrap(),
                "",
                move |exit| async move {
                    let _ = exit_tx.send(exit);
                    Ok(())
                },
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(12), async {
            while std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0) < 64 * 1024 {
                assert!(
                    recorder.is_active(74),
                    "recording exited before stop request"
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("wait for live recording data");
        let (first, second) = tokio::time::timeout(Duration::from_secs(15), async {
            tokio::join!(recorder.stop_record(74), recorder.stop_record(74))
        })
        .await
        .expect("manual stop must finish promptly");
        assert!(first.unwrap());
        assert!(second.unwrap());
        let exit = exit_rx.await.unwrap();
        assert!(exit.manually_stopped);
        assert!(exit.stopped_cleanly(), "{exit:?}");
        assert_eq!(exit.exit_code, Some(0));
        assert!(!recorder.is_active(74));
        server.abort();
        let _ = server.await;

        let duration = assert_complete_flv(&output);
        assert!(
            duration < 30.0,
            "stop must occur before the source reaches EOF"
        );
        let mut decoder = Command::new(&ffmpeg);
        decoder
            .args([
                "-nostdin",
                "-v",
                "error",
                "-xerror",
                "-err_detect",
                "explode",
                "-i",
            ])
            .arg(&output)
            .args(["-f", "null", "-"])
            .kill_on_drop(true);
        decoder.as_std_mut().creation_flags(0x08000000);
        let decoded = tokio::time::timeout(Duration::from_secs(15), decoder.output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert!(
            decoded.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
    }

    #[tokio::test]
    async fn forces_unresponsive_ffmpeg_to_exit_after_grace_period() {
        let mut command = Command::new(bundled_ffmpeg());
        command
            .args([
                "-nostdin",
                "-loglevel",
                "error",
                "-re",
                "-f",
                "lavfi",
                "-i",
                "color=s=32x32:r=1",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.as_std_mut().creation_flags(0x08000000);
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let started_at = std::time::Instant::now();
        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            stop_ffmpeg(&mut child, stdin, Duration::from_millis(200)),
        )
        .await
        .expect("force stop must not hang");
        assert!(started_at.elapsed() >= Duration::from_millis(200));
        assert!(exit.manually_stopped);
        assert!(!exit.stopped_cleanly());
        assert!(exit.forced_stop_reason.as_deref().unwrap().contains("超时"));
        assert!(
            child.try_wait().unwrap().is_some(),
            "forced process must be reaped"
        );
    }

    #[tokio::test]
    async fn stopping_an_already_exited_process_does_not_report_forced_termination() {
        let mut command = Command::new(bundled_ffmpeg());
        command
            .arg("-version")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.as_std_mut().creation_flags(0x08000000);
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        assert!(child.wait().await.unwrap().success());
        let exit = stop_ffmpeg(&mut child, stdin, Duration::from_millis(200)).await;
        assert!(exit.stopped_cleanly(), "{exit:?}");
        assert!(exit.forced_stop_reason.is_none());
    }

    #[test]
    fn migration_is_blocked_while_a_recording_handle_is_still_active() {
        use crate::{migration, AppState, AutoRecorder, Database, DouyinParser};
        use std::sync::Mutex;
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::new("unused-ffmpeg".to_string());
        let (_, completion_rx) = tokio::sync::watch::channel(None);
        // Models the brief interval after DB finalization but before handle cleanup.
        recorder.active_records.lock().unwrap().insert(
            1,
            super::ActiveRecording {
                stop_tx: None,
                completion_rx,
            },
        );
        let state = AppState {
            db: Mutex::new(Database::new(&dir.path().join("source.db")).unwrap()),
            recorder,
            parser: DouyinParser::new(),
            auto_recorder: AutoRecorder::new(),
            start_lock: tokio::sync::Mutex::new(()),
        };
        let target = dir.path().join("target.db");
        let config = dir.path().join("settings.json");
        assert!(
            migration::migrate_database(&state, target.to_str().unwrap(), &config)
                .unwrap_err()
                .contains("录制或结束处理中")
        );
        assert!(!target.exists());
        assert!(!config.exists());
    }

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
