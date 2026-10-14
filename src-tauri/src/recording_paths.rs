use crate::{
    database::{Database, LiveRoom},
    segments::safe_filename,
    settings::AppSettings,
};
use std::path::{Path, PathBuf};

/// Resolve and create the destination before reserving a task or its first output file.
/// The caller holds both the recording start lock and the database lock.
pub fn prepare_directory(
    settings: &AppSettings,
    db: &Database,
    room: &mut LiveRoom,
) -> Result<PathBuf, String> {
    let root = if settings.recordings_dir.is_empty() {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map_err(|_| "无法获取用户目录".to_string())?;
        Path::new(&home).join("DouyinRecordings")
    } else {
        PathBuf::from(&settings.recordings_dir)
    };
    if !settings.organize_recordings_by_anchor {
        std::fs::create_dir_all(&root).map_err(|e| format!("创建录制目录失败: {e}"))?;
        return Ok(root);
    }

    let existing = room
        .recording_folder
        .as_deref()
        .filter(|name| !name.is_empty());
    let folder = existing
        .map(str::to_owned)
        .unwrap_or_else(|| folder_name(&room.anchor_name, &room.room_id));
    // Never reinterpret an invalid saved binding as another path or silently rename it.
    if !valid_folder_name(&folder) {
        return Err("主播录制文件夹名无效，必须是合法的单层目录名".into());
    }
    let directory = root.join(&folder);
    std::fs::create_dir_all(&directory)
        .map_err(|e| format!("创建主播录制目录失败（{}）: {e}", directory.display()))?;
    if existing.is_none() {
        db.bind_recording_folder(room.id, &folder)
            .map_err(|e| format!("保存主播录制文件夹失败: {e}"))?;
        room.recording_folder = Some(folder);
    }
    Ok(directory)
}

fn reserved_windows_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

fn folder_name(anchor: &str, room_id: &str) -> String {
    let name = safe_filename(anchor);
    let id = safe_filename(room_id);
    // Keep the ID suffix intact even with non-BMP characters; leave room for a
    // reserved-name prefix within Windows' 255 UTF-16-unit component limit.
    let budget = 240 - id.encode_utf16().count() - 1;
    let mut units = 0;
    let name: String = name
        .chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= budget
        })
        .collect();
    let folder = format!("{name}_{id}");
    if reserved_windows_name(&folder) {
        format!("_{folder}")
    } else {
        folder
    }
}

fn valid_folder_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with([' ', '.'])
        && name.encode_utf16().count() <= 255
        && !name
            .chars()
            .any(|ch| ch.is_control() || "<>:\"/\\|?*".contains(ch))
        && !reserved_windows_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(root: &Path) -> AppSettings {
        AppSettings {
            recordings_dir: root.to_string_lossy().into_owned(),
            organize_recordings_by_anchor: true,
            ..Default::default()
        }
    }

    #[test]
    fn binding_survives_rename_restart_toggle_and_root_change() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rooms.db");
        let mut config = settings(&temp.path().join("recordings"));
        let db = Database::new(&path).unwrap();
        let id = db
            .add_room_full("douyin", "123", "小明（生日）", "", "", "", true)
            .unwrap();
        let mut room = db.get_room(id).unwrap();
        config.organize_recordings_by_anchor = false;
        assert_eq!(
            prepare_directory(&config, &db, &mut room).unwrap(),
            PathBuf::from(&config.recordings_dir)
        );
        assert!(db.get_room(id).unwrap().recording_folder.is_none());
        let legacy = Path::new(&config.recordings_dir).join("legacy.flv");
        std::fs::write(&legacy, b"old recording").unwrap();
        let task = db.add_task(id, "manual").unwrap();
        db.update_task_status_and_path(task, "completed", legacy.to_str())
            .unwrap();
        config.organize_recordings_by_anchor = true;
        let first = prepare_directory(&config, &db, &mut room).unwrap();
        assert!(first.is_dir());
        assert_eq!(first.file_name().unwrap(), "小明（生日）_123");
        db.update_room_live_status(id, "小明（活动）", "", "", "", true)
            .unwrap();
        drop(db);
        let db = Database::new(&path).unwrap();
        let mut room = db.get_all_rooms().unwrap().pop().unwrap();
        assert_eq!(room.anchor_name, "小明（活动）");
        assert_eq!(prepare_directory(&config, &db, &mut room).unwrap(), first);
        config.organize_recordings_by_anchor = false;
        assert_eq!(
            prepare_directory(&config, &db, &mut room).unwrap(),
            PathBuf::from(&config.recordings_dir)
        );
        config.organize_recordings_by_anchor = true;
        assert_eq!(prepare_directory(&config, &db, &mut room).unwrap(), first);
        config.recordings_dir = temp.path().join("new-root").to_string_lossy().into_owned();
        let moved = prepare_directory(&config, &db, &mut room).unwrap();
        assert_eq!(
            moved,
            Path::new(&config.recordings_dir).join(first.file_name().unwrap())
        );
        assert!(moved.is_dir());
        assert_eq!(std::fs::read(&legacy).unwrap(), b"old recording");
        assert_eq!(
            db.get_task(task).unwrap().file_path.as_deref(),
            legacy.to_str()
        );
    }

    #[test]
    fn same_name_rooms_are_separate_and_readded_rooms_get_a_new_binding() {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::new(&temp.path().join("rooms.db")).unwrap();
        let config = settings(&temp.path().join("recordings"));
        let a = db
            .add_room_full("douyin", "123", "小明", "", "", "", true)
            .unwrap();
        let b = db
            .add_room_full("douyin", "456", "小明", "", "", "", true)
            .unwrap();
        let first = prepare_directory(&config, &db, &mut db.get_room(a).unwrap()).unwrap();
        let second = prepare_directory(&config, &db, &mut db.get_room(b).unwrap()).unwrap();
        assert_ne!(first, second);
        db.delete_room(a).unwrap();
        let new_id = db
            .add_room_full("douyin", "123", "新名字", "", "", "", true)
            .unwrap();
        assert!(db.get_room(new_id).unwrap().recording_folder.is_none());
        let new = prepare_directory(&config, &db, &mut db.get_room(new_id).unwrap()).unwrap();
        assert_eq!(new.file_name().unwrap(), "新名字_123");
        assert!(first.exists());
    }

    #[test]
    fn folder_names_are_single_components_and_can_be_created_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        for (anchor, expected) in [
            ("", "主播_123"),
            (" ... ", "主播_123"),
            ("小明（生日）", "小明（生日）_123"),
            ("../a\\b:c\n", "_a_b_c__123"),
            ("CON.txt", "_CON.txt_123"),
            ("lpt¹.log", "_lpt¹.log_123"),
            ("CON", "CON_123"),
        ] {
            let folder = folder_name(anchor, "123");
            assert_eq!(folder, expected);
            assert!(valid_folder_name(&folder));
            std::fs::create_dir_all(temp.path().join(folder)).unwrap();
        }
        let long = folder_name(&"😀".repeat(100), &"😁".repeat(100));
        assert!(valid_folder_name(&long));
        assert!(long.ends_with(&safe_filename(&"😁".repeat(100))));
        for invalid in [
            "../escape",
            "a\\b",
            "C:escape",
            ".",
            "..",
            "NUL",
            "AUX.txt",
            "x.",
            "x ",
        ] {
            assert!(!valid_folder_name(invalid), "{invalid}");
        }
    }

    #[test]
    fn directory_and_database_failures_do_not_set_a_binding_or_fall_back() {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::new(&temp.path().join("rooms.db")).unwrap();
        let config = settings(&temp.path().join("recordings"));
        let id = db
            .add_room_full("douyin", "123", "小明", "", "", "", true)
            .unwrap();
        let mut room = db.get_room(id).unwrap();
        std::fs::create_dir_all(&config.recordings_dir).unwrap();
        let blocked = Path::new(&config.recordings_dir).join("小明_123");
        std::fs::write(&blocked, b"existing file").unwrap();
        assert!(prepare_directory(&config, &db, &mut room)
            .unwrap_err()
            .contains("创建主播录制目录失败"));
        assert!(db.get_room(id).unwrap().recording_folder.is_none());
        assert_eq!(std::fs::read(&blocked).unwrap(), b"existing file");
        std::fs::remove_file(&blocked).unwrap();
        db.conn.execute_batch("CREATE TRIGGER reject_folder BEFORE UPDATE OF recording_folder ON live_rooms BEGIN SELECT RAISE(ABORT, 'test write failure'); END;").unwrap();
        assert!(prepare_directory(&config, &db, &mut room)
            .unwrap_err()
            .contains("保存主播录制文件夹失败"));
        assert!(room.recording_folder.is_none());
        assert!(db.get_room(id).unwrap().recording_folder.is_none());
        assert!(db.get_all_tasks().unwrap().is_empty());
        room.recording_folder = Some("../escape".into());
        assert!(prepare_directory(&config, &db, &mut room).is_err());
        assert!(!temp.path().join("escape").exists());
    }
}
