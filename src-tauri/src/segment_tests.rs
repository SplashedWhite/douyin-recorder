#![cfg(all(test, windows))]

use crate::{
    conversion,
    recorder::Recorder,
    segments::{ManifestReader, SegmentOutput},
    Database,
};
use std::os::windows::process::CommandExt;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) fn ffmpeg() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries/ffmpeg-x86_64-pc-windows-msvc.exe")
}

pub(crate) fn fixture(directory: &Path, seconds: &str) -> PathBuf {
    let path = directory.join("input.flv");
    let result = Command::new(ffmpeg())
        .creation_flags(0x08000000)
        .args([
            "-y",
            "-nostdin",
            "-v",
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
            seconds,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "30",
            "-sc_threshold",
            "0",
            "-c:a",
            "aac",
            "-f",
            "flv",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    path
}

fn video_hashes(path: &Path) -> Vec<String> {
    let result = Command::new(ffmpeg())
        .creation_flags(0x08000000)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "framemd5", "-"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.rsplit(',').next().unwrap().trim().to_string())
        .collect()
}

fn assert_audio_decodes(path: &Path) {
    let result = Command::new(ffmpeg())
        .creation_flags(0x08000000)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn real_segments_keep_all_video_frames_audio_and_atomic_mp4_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(dir.path(), "7.4");
    let out_dir = dir.path().join("含 空格, 和100%");
    std::fs::create_dir(&out_dir).unwrap();
    let output = SegmentOutput::new(&out_dir, "主播, 100%_123_20260925_1", 2);
    std::fs::create_dir_all(Path::new(&output.manifest_path).parent().unwrap()).unwrap();
    let recorder = Recorder::new(ffmpeg().to_string_lossy().into_owned());
    let (sender, receiver) = tokio::sync::oneshot::channel();
    recorder
        .start_record_with_segments(
            1,
            input.to_str().unwrap(),
            &output.path(1),
            "",
            Some(&output),
            move |exit| async move {
                let _ = sender.send(exit);
                Ok(())
            },
        )
        .unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(20), receiver)
        .await
        .unwrap()
        .unwrap();
    assert!(exit.status_success, "{exit:?}");
    let entries = ManifestReader::default().read(&output).unwrap();
    assert_eq!(entries.len(), 4);
    assert!(entries[3].end - entries[3].start < 2.0);
    let mut hashes = vec![];
    for (offset, entry) in entries.iter().enumerate() {
        assert_eq!(entry.index as usize, offset + 1);
        let path = PathBuf::from(output.path(entry.index));
        assert_audio_decodes(&path);
        hashes.extend(video_hashes(&path));
    }
    assert_eq!(
        hashes,
        video_hashes(&input),
        "segmentation must not lose, duplicate or change video frames"
    );
    let original = std::fs::read(output.path(1)).unwrap();
    let (mp4, size) = conversion::remux_file(ffmpeg().to_str().unwrap(), &output.path(1)).unwrap();
    assert!(size > 0);
    assert_eq!(
        original,
        std::fs::read(output.path(1)).unwrap(),
        "FLV remains until the DB update commits"
    );
    assert_eq!(
        video_hashes(Path::new(&mp4)),
        video_hashes(Path::new(&output.path(1)))
    );
    assert_audio_decodes(Path::new(&mp4));
    let completed = std::fs::read(&mp4).unwrap();
    assert!(conversion::remux_file(ffmpeg().to_str().unwrap(), &output.path(1)).is_err());
    assert_eq!(
        completed,
        std::fs::read(&mp4).unwrap(),
        "existing MP4 must never be overwritten"
    );
    let invalid = dir.path().join("invalid.flv");
    std::fs::write(&invalid, b"invalid input").unwrap();
    assert!(conversion::remux_file(ffmpeg().to_str().unwrap(), invalid.to_str().unwrap()).is_err());
    assert!(invalid.exists() && !invalid.with_extension("mp4").exists());
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".douyin-remux-")));
}

#[tokio::test]
async fn concurrent_rooms_keep_separate_sequences_and_durations() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(dir.path(), "6.4");
    let recorder = Recorder::new(ffmpeg().to_string_lossy().into_owned());
    let mut recordings = vec![];
    for (id, seconds) in [(101, 2), (202, 3)] {
        let output = SegmentOutput::new(dir.path(), &format!("room_{id}"), seconds);
        std::fs::create_dir_all(Path::new(&output.manifest_path).parent().unwrap()).unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        recorder
            .start_record_with_segments(
                id,
                input.to_str().unwrap(),
                &output.path(1),
                "",
                Some(&output),
                move |exit| async move {
                    let _ = sender.send(exit);
                    Ok(())
                },
            )
            .unwrap();
        recordings.push((id, output, receiver));
    }
    for (id, output, receiver) in recordings {
        let exit = tokio::time::timeout(Duration::from_secs(20), receiver)
            .await
            .unwrap()
            .unwrap();
        assert!(exit.status_success, "{exit:?}");
        let entries = ManifestReader::default().read(&output).unwrap();
        assert_eq!(entries.len(), if id == 101 { 4 } else { 3 });
        assert_eq!(entries[0].index, 1);
        assert_eq!(output.files().unwrap().len(), entries.len());
    }
}

#[tokio::test]
async fn live_segmentation_keeps_recording_during_conversion_and_flushes_stop() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(dir.path(), "18");
    let media = std::fs::read(&input).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/live.flv", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![];
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/x-flv\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        for chunk in media.chunks(4096) {
            if socket.write_all(chunk).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        std::future::pending::<()>().await;
    });
    let output = SegmentOutput::new(dir.path(), "live_1", 2);
    std::fs::create_dir_all(Path::new(&output.manifest_path).parent().unwrap()).unwrap();
    let recorder = Recorder::new(ffmpeg().to_string_lossy().into_owned());
    let (sender, receiver) = tokio::sync::oneshot::channel();
    recorder
        .start_record_with_segments(
            1,
            &url,
            &output.path(1),
            "",
            Some(&output),
            move |exit| async move {
                let _ = sender.send(exit);
                Ok(())
            },
        )
        .unwrap();
    let db = Database::new(&dir.path().join("state.db")).unwrap();
    let room = db
        .add_room_full("douyin", "1", "主播", "", "", "", true)
        .unwrap();
    let task = db.add_task(room, "auto").unwrap();
    db.update_task_status(task, "recording").unwrap();
    db.set_segment_output(task, &output).unwrap();
    let mut reader = ManifestReader::default();
    let mut entries = vec![];
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            entries.extend(reader.read(&output).unwrap());
            db.observe_segments(task, &output, &entries).unwrap();
            if db
                .get_segments(task)
                .unwrap()
                .first()
                .is_some_and(|segment| segment.status == "completed")
            {
                break;
            }
            assert!(recorder.is_active(1));
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let first = db.get_segments(task).unwrap()[0].clone();
    assert!(db.claim_segment_conversion(first.id).unwrap());
    assert!(!db.claim_segment_conversion(first.id).unwrap());
    let source = first.file_path.clone();
    let converted = tokio::task::spawn_blocking(move || {
        conversion::remux_file(ffmpeg().to_str().unwrap(), &source)
    })
    .await
    .unwrap();
    db.finish_segment_conversion(first.id, &converted).unwrap();
    converted.unwrap();
    std::fs::remove_file(&first.file_path).unwrap();
    assert!(recorder.is_active(1));
    tokio::time::timeout(Duration::from_secs(25), async {
        while crate::file_size(Some(&output.path(3))) < 8192 {
            assert!(recorder.is_active(1));
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(recorder.stop_record(1).await.unwrap());
    let exit = receiver.await.unwrap();
    assert!(exit.manually_stopped && exit.stopped_cleanly(), "{exit:?}");
    server.abort();
    entries.extend(reader.read(&output).unwrap());
    db.observe_segments(task, &output, &entries).unwrap();
    for segment in db.get_segments(task).unwrap() {
        if segment.status == "recording" {
            db.close_segment(segment.id, "completed", "2026-09-25 00:00:00")
                .unwrap();
        }
        assert_audio_decodes(Path::new(&segment.file_path));
        assert!(!video_hashes(Path::new(&segment.file_path)).is_empty());
    }
    assert!(entries.len() >= 3);
    assert!(entries.last().unwrap().end - entries.last().unwrap().start < 2.0);
    assert_eq!(
        db.get_task(task).unwrap().status,
        "recording",
        "only session exit handling changes automation/session state"
    );
}
