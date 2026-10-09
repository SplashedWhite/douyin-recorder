//! Production start/exit/recovery code with Tauri's headless runtime, real local
//! HTTP and the bundled FFmpeg. No global settings or live Douyin rooms are used.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    app: tauri::App<tauri::test::MockRuntime>,
    dir: tempfile::TempDir,
    room: i64,
    input: String,
    server: tokio::task::JoinHandle<()>,
    requests: Arc<AtomicUsize>,
}

impl Fixture {
    async fn new(segmented: bool, bodies: Vec<(u16, String)>, delay: Duration) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let input = segment_tests::fixture(dir.path(), if segmented { "65" } else { "1" })
            .to_string_lossy()
            .into_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let media = input.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let Ok(byte) = socket.read_u8().await else {
                        break;
                    };
                    request.push(byte);
                }
                let room = request.starts_with(b"GET /room?");
                let (status, body) = if room {
                    let index = counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    bodies.get(index).cloned().unwrap_or((200, offline()))
                } else {
                    (200, "{}".into())
                };
                let body = body.replace("TEST_MEDIA_JSON", &serde_json::to_string(&media).unwrap());
                let response = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\nSet-Cookie: ttwid=test\r\n\r\n{body}", body.len());
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        let mut parser = DouyinParser::new();
        parser.test_endpoints = Some((base.clone(), format!("{base}/room")));
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let room = db
            .add_room_full("douyin", "123", "anchor", "title", "", "", true)
            .unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState {
            db: Mutex::new(db),
            recorder: Recorder::new(segment_tests::ffmpeg().to_string_lossy().into_owned()),
            parser,
            auto_recorder: AutoRecorder::new(),
            recoveries: Default::default(),
            start_lock: AsyncMutex::new(()),
            lifecycle: Default::default(),
            test_settings: Some(AppSettings {
                recordings_dir: dir.path().join("recordings").to_string_lossy().into_owned(),
                segment_recording_enabled: segmented,
                segment_duration_minutes: 1,
                auto_convert_mp4: segmented,
                ffmpeg_reconnect_enabled: false,
                recording_recovery_enabled: true,
                ..Default::default()
            }),
        });
        Self {
            app,
            dir,
            room,
            input,
            server,
            requests,
        }
    }

    async fn start(&self, automatic: bool, bad_input: bool) -> RecordTask {
        let state = self.app.state::<AppState>();
        let revision = if automatic {
            let db = state.db.lock().unwrap();
            let mut room = db.get_room(self.room).unwrap();
            auto_policy::set_enabled(&mut room, true, 6, Utc::now());
            Some(db.save_room_automation(&room).unwrap().auto_record_revision)
        } else {
            None
        };
        let info = LiveInfo {
            platform: "douyin".into(),
            room_id: "123".into(),
            anchor_name: "anchor".into(),
            room_title: "title".into(),
            cover_url: String::new(),
            avatar_url: String::new(),
            is_live: true,
            stream_url: if bad_input {
                self.dir
                    .path()
                    .join("missing.flv")
                    .to_string_lossy()
                    .into_owned()
            } else {
                self.input.clone()
            },
        };
        start_record_from_info(
            self.app.handle(),
            &state,
            self.room,
            &info,
            if automatic { "auto" } else { "manual" },
            revision,
        )
        .await
        .unwrap()
    }

    async fn until(&self, ready: impl Fn(&AppState) -> bool) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if ready(&self.app.state::<AppState>()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "{error}: recovery={:?}, tasks={:?}, requests={}",
                self.app.state::<AppState>().recoveries.snapshot(),
                self.app
                    .state::<AppState>()
                    .db
                    .lock()
                    .unwrap()
                    .get_all_tasks()
                    .unwrap(),
                self.requests.load(Ordering::SeqCst)
            )
        });
    }

    async fn idle(&self) {
        self.until(|state| {
            !state.recorder.has_active_records().unwrap() && !state.recoveries.any_busy()
        })
        .await;
        self.app
            .state::<AppState>()
            .lifecycle
            .wait_for_operations()
            .await
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn live() -> String {
    r#"{"data":{"data":[{"status":2,"stream_url":{"flv_pull_url":{"ORIGIN":TEST_MEDIA_JSON}}}]}}"#
        .into()
}
fn offline() -> String {
    r#"{"data":{"message":"room has finished","prompts":"直播已结束"}}"#.into()
}

#[tokio::test]
async fn eof_and_failed_process_restart_into_distinct_files_without_continuous_monitoring() {
    for (automatic, bad_input) in [(false, false), (true, false), (false, true)] {
        let f = Fixture::new(false, vec![(200, live()), (200, offline())], Duration::ZERO).await;
        let first = f.start(automatic, bad_input).await;
        f.idle().await;
        let state = f.app.state::<AppState>();
        let db = state.db.lock().unwrap();
        let tasks = db.get_all_tasks().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].recovery_from_task_id, Some(first.id));
        assert_eq!(tasks[0].trigger, if automatic { "auto" } else { "manual" });
        assert_eq!(tasks[0].status, "completed");
        assert_eq!(
            tasks[1].status,
            if bad_input { "failed" } else { "interrupted" }
        );
        assert_ne!(tasks[0].file_path, tasks[1].file_path);
        assert!(std::path::Path::new(tasks[0].file_path.as_ref().unwrap()).exists());
        assert!(std::path::Path::new(tasks[1].file_path.as_ref().unwrap()).exists());
        assert_eq!(f.requests.load(Ordering::SeqCst), 2); // No duplicate initial check.
        assert!(!db.get_room(f.room).unwrap().auto_record_enabled);
    }
}

#[tokio::test]
async fn retry_after_query_failure_succeeds_and_cancelled_response_cannot_start_a_process() {
    let f = Fixture::new(
        false,
        vec![(500, "temporary".into()), (200, live()), (200, offline())],
        Duration::ZERO,
    )
    .await;
    f.start(false, false).await;
    f.until(|s| {
        s.recoveries
            .status(f.room)
            .is_some_and(|r| r.phase == "waiting")
    })
    .await;
    assert_eq!(
        f.app
            .state::<AppState>()
            .db
            .lock()
            .unwrap()
            .get_all_tasks()
            .unwrap()[0]
            .status,
        "finalizing"
    );
    f.idle().await;
    assert_eq!(
        f.app
            .state::<AppState>()
            .db
            .lock()
            .unwrap()
            .get_all_tasks()
            .unwrap()
            .len(),
        2
    );

    let f = Fixture::new(false, vec![(200, live())], Duration::from_millis(300)).await;
    let first = f.start(true, false).await;
    f.until(|_| f.requests.load(Ordering::SeqCst) == 1).await;
    let id = f
        .app
        .state::<AppState>()
        .recoveries
        .status(f.room)
        .unwrap()
        .recovery_id;
    cancel_recording_recovery(f.app.handle().clone(), f.app.state(), f.room, id)
        .await
        .unwrap();
    f.idle().await;
    tokio::time::sleep(Duration::from_millis(350)).await;
    let state = f.app.state::<AppState>();
    let db = state.db.lock().unwrap();
    assert_eq!(db.get_all_tasks().unwrap().len(), 1);
    assert_eq!(db.get_task(first.id).unwrap().status, "interrupted");
    assert!(db.get_room(f.room).unwrap().auto_record_enabled);
    assert!(!state.auto_recorder.is_due(f.room));
}

#[tokio::test]
async fn old_segment_conversion_queue_does_not_block_new_capture_and_exit_waits_for_it() {
    let gate = conversion::hold_queue().await;
    let f = Fixture::new(true, vec![(200, live()), (200, offline())], Duration::ZERO).await;
    let first = f.start(false, false).await;
    f.until(|s| s.db.lock().unwrap().get_all_tasks().unwrap().len() == 2)
        .await;
    let state = f.app.state::<AppState>();
    let tasks = state.db.lock().unwrap().get_all_tasks().unwrap();
    assert_eq!(tasks[0].recovery_from_task_id, Some(first.id));
    assert_ne!(
        tasks[0].segment_output.as_ref().unwrap().file_prefix,
        tasks[1].segment_output.as_ref().unwrap().file_prefix
    );
    assert!(tasks[1]
        .segments
        .iter()
        .any(|s| s.conversion_state == "queued"));
    state.lifecycle.begin_exit();
    let app = f.app.handle().clone();
    let mut shutdown = tokio::spawn(async move { desktop::finish_pending_work(&app).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
            .await
            .is_err()
    );
    drop(gate);
    tokio::time::timeout(Duration::from_secs(15), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!state.db.lock().unwrap().has_running_tasks().unwrap());
}

#[tokio::test]
async fn monitor_switch_cancels_only_automatic_pending_recovery_and_ticks_respect_reservation() {
    for automatic in [false, true] {
        let f = Fixture::new(
            false,
            vec![(200, live()), (200, offline())],
            Duration::from_millis(250),
        )
        .await;
        f.start(automatic, false).await;
        f.until(|_| f.requests.load(Ordering::SeqCst) == 1).await;
        {
            let state = f.app.state::<AppState>();
            let db = state.db.lock().unwrap();
            let mut room = db.get_room(f.room).unwrap();
            room.auto_record_enabled = true;
            room.auto_record_until = Some((Utc::now() - ChronoDuration::seconds(1)).to_rfc3339());
            db.save_room_automation(&room).unwrap();
        }
        process_auto_deadlines_and_schedules(f.app.handle()).unwrap();
        assert!(
            f.app
                .state::<AppState>()
                .db
                .lock()
                .unwrap()
                .get_room(f.room)
                .unwrap()
                .auto_record_enabled
        );
        assert!(next_due_auto_room(f.app.handle()).unwrap().is_none());
        set_room_auto_record(f.app.handle().clone(), f.app.state(), f.room, false)
            .await
            .unwrap();
        f.idle().await;
        assert_eq!(
            f.app
                .state::<AppState>()
                .db
                .lock()
                .unwrap()
                .get_all_tasks()
                .unwrap()
                .len(),
            if automatic { 1 } else { 2 }
        );
    }
}

#[tokio::test]
async fn local_start_error_ends_recovery_and_keeps_the_original_file() {
    let f = Fixture::new(false, vec![(200, live())], Duration::ZERO).await;
    std::fs::write(
        f.dir.path().join("recordings"),
        "directory blocked by a file",
    )
    .unwrap();
    let state = f.app.state::<AppState>();
    let task = {
        let db = state.db.lock().unwrap();
        let task = db.add_task(f.room, "manual").unwrap();
        db.update_task_status_and_path(task, "recording", Some(&f.input))
            .unwrap();
        task
    };
    recovery_runtime::recording_exit(
        f.app.handle().clone(),
        task,
        f.room,
        RecordingExit {
            manually_stopped: false,
            status_success: true,
            exit_code: Some(0),
            forced_stop_reason: None,
            wait_error: None,
            stderr_tail: vec![],
        },
        tokio::sync::watch::channel(false).0,
        None,
        Arc::new(state.lifecycle.operation().unwrap()),
    )
    .await
    .unwrap();
    assert!(!state.recoveries.busy(f.room));
    assert_eq!(state.recoveries.status(f.room).unwrap().phase, "failed");
    assert_eq!(
        state.db.lock().unwrap().get_task(task).unwrap().status,
        "interrupted"
    );
    assert!(std::path::Path::new(&f.input).exists());
    assert_eq!(state.db.lock().unwrap().get_all_tasks().unwrap().len(), 1);
}

#[tokio::test]
async fn stop_intent_before_exit_callback_prevents_recovery_registration() {
    let f = Fixture::new(false, vec![(200, live())], Duration::ZERO).await;
    let task = f.start(false, false).await;
    let state = f.app.state::<AppState>();
    let lock = state.start_lock.lock().await;
    f.until(|s| s.recorder.progress(task.id).is_some_and(|p| !p.running()))
        .await;
    assert!(!state.recoveries.busy(f.room));
    state.recorder.request_stop(task.id); // The synchronous portion of normal stop.
    drop(lock);
    f.idle().await;
    assert_eq!(f.requests.load(Ordering::SeqCst), 0);
    assert_eq!(state.db.lock().unwrap().get_all_tasks().unwrap().len(), 1);
    assert_eq!(
        state.db.lock().unwrap().get_task(task.id).unwrap().status,
        "completed"
    );
}

#[tokio::test]
async fn deadline_ends_pending_recovery_without_an_extra_request_and_applies_current_policy() {
    for continuous in [false, true] {
        let f = Fixture::new(false, vec![(500, "temporary".into())], Duration::ZERO).await;
        f.start(true, false).await;
        f.until(|s| {
            s.recoveries
                .status(f.room)
                .is_some_and(|r| r.phase == "waiting")
        })
        .await;
        let state = f.app.state::<AppState>();
        if continuous {
            let db = state.db.lock().unwrap();
            let mut room = db.get_room(f.room).unwrap();
            room.auto_monitor_mode = AutoMonitorMode::Continuous;
            db.save_room_automation(&room).unwrap();
        }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(121)).await;
        f.idle().await;
        tokio::time::resume();
        let db = state.db.lock().unwrap();
        let room = db.get_room(f.room).unwrap();
        assert_eq!(room.auto_record_enabled, continuous);
        assert_eq!(room.auto_record_retry_at.is_some(), continuous);
        assert_eq!(state.recoveries.status(f.room).unwrap().phase, "exhausted");
        assert_eq!(f.requests.load(Ordering::SeqCst), 1);
        assert_eq!(db.get_all_tasks().unwrap()[0].status, "interrupted");
    }
}

#[tokio::test]
async fn deleting_a_recovering_room_waits_for_output_close_and_late_response_cannot_revive_it() {
    let f = Fixture::new(false, vec![(200, live())], Duration::from_millis(250)).await;
    f.start(false, false).await;
    f.until(|_| f.requests.load(Ordering::SeqCst) == 1).await;
    assert!(
        delete_room(f.app.handle().clone(), f.app.state(), f.room, Some(true))
            .await
            .is_err()
    );
    f.idle().await;
    delete_room(f.app.handle().clone(), f.app.state(), f.room, Some(true))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let state = f.app.state::<AppState>();
    assert!(state.db.lock().unwrap().get_all_rooms().unwrap().is_empty());
    assert!(state.db.lock().unwrap().get_all_tasks().unwrap().is_empty());
    assert!(!state.recorder.has_active_records().unwrap());
}

#[tokio::test]
async fn manual_start_supersedes_pending_recovery_without_duplicate_capture() {
    let f = Fixture::new(
        false,
        vec![(200, live()), (200, live()), (200, offline())],
        Duration::from_millis(200),
    )
    .await;
    f.start(false, false).await;
    f.until(|_| f.requests.load(Ordering::SeqCst) == 1).await;
    let task = start_record(f.app.handle().clone(), f.app.state(), f.room)
        .await
        .unwrap();
    assert_eq!(task.recovery_from_task_id, None);
    f.idle().await;
    let tasks = f
        .app
        .state::<AppState>()
        .db
        .lock()
        .unwrap()
        .get_all_tasks()
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].status, "completed");
    assert_eq!(tasks[1].status, "interrupted");
    assert_eq!(f.requests.load(Ordering::SeqCst), 3);
}
