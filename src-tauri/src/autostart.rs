/// Kept behind an interface so save failures can be tested without touching login items.
pub trait Autostart {
    fn can_manage(&self) -> bool {
        true
    }
    fn is_enabled(&self) -> Result<bool, String>;
    fn set_enabled(&self, enabled: bool) -> Result<(), String>;
}

/// Report the native build's policy; a release web frontend can also run in a debug binary.
#[tauri::command]
pub fn is_autostart_available() -> bool {
    !cfg!(debug_assertions)
}

fn ensure_available() -> Result<(), String> {
    if is_autostart_available() {
        Ok(())
    } else {
        Err("开发模式不修改系统自启动，请使用正式构建测试此功能".into())
    }
}

impl Autostart for tauri_plugin_autostart::AutoLaunchManager {
    fn can_manage(&self) -> bool {
        is_autostart_available()
    }

    fn is_enabled(&self) -> Result<bool, String> {
        ensure_available()?;
        self.is_enabled().map_err(|e| e.to_string())
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        ensure_available()?;
        if enabled {
            self.enable()
        } else {
            self.disable()
        }
        .map_err(|e| e.to_string())
    }
}

pub fn sync_on_startup(manager: &impl Autostart, enabled: bool) -> Result<(), String> {
    if manager.can_manage() {
        manager.set_enabled(enabled)?;
    }
    Ok(())
}

/// The caller validates settings first and holds the settings-save lock throughout.
/// When autostart is unavailable, it must also preserve the persisted preference.
pub fn save_with_autostart<T>(
    manager: &impl Autostart,
    enabled: bool,
    persist: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if !manager.can_manage() {
        return persist();
    }
    let previous = manager
        .is_enabled()
        .map_err(|error| format!("读取系统自启动状态失败: {error}"))?;
    let result = manager
        .set_enabled(enabled)
        .map_err(|error| format!("设置系统自启动失败: {error}"))
        .and_then(|()| persist());
    match result {
        Ok(saved) => Ok(saved),
        Err(error) => {
            // An OS operation can partially succeed before returning an error.
            match manager.set_enabled(previous) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "{error}；恢复原有自启动状态也失败: {rollback_error}，请检查系统启动项后重试"
                )),
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::Autostart;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    pub struct FakeAutostart {
        pub available: Cell<bool>,
        pub enabled: Cell<bool>,
        pub reads: Cell<usize>,
        pub fail_read: Cell<bool>,
        pub writes: RefCell<Vec<bool>>,
        pub write_errors: RefCell<VecDeque<Option<&'static str>>>,
    }

    impl FakeAutostart {
        pub fn new(enabled: bool) -> Self {
            Self {
                available: Cell::new(true),
                enabled: Cell::new(enabled),
                reads: Cell::new(0),
                fail_read: Cell::new(false),
                writes: RefCell::new(Vec::new()),
                write_errors: RefCell::new(VecDeque::new()),
            }
        }
    }

    impl Autostart for FakeAutostart {
        fn can_manage(&self) -> bool {
            self.available.get()
        }

        fn is_enabled(&self) -> Result<bool, String> {
            self.reads.set(self.reads.get() + 1);
            if self.fail_read.get() {
                return Err("无法读取启动项".into());
            }
            Ok(self.enabled.get())
        }

        fn set_enabled(&self, enabled: bool) -> Result<(), String> {
            self.writes.borrow_mut().push(enabled);
            self.enabled.set(enabled);
            // Simulate an OS write that changes state and then reports a failure.
            if let Some(error) = self.write_errors.borrow_mut().pop_front().flatten() {
                return Err(error.into());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{save_with_autostart, sync_on_startup, test_support::FakeAutostart};
    use crate::settings::{load_settings_from, save_settings_at, AppSettings};
    use std::cell::Cell;

    #[test]
    fn protected_startup_never_reads_writes_or_removes_system_autostart() {
        let manager = FakeAutostart::new(true);
        manager.available.set(false);
        manager.fail_read.set(true);
        manager
            .write_errors
            .borrow_mut()
            .push_back(Some("必须保持未调用"));
        for enabled in [true, false] {
            sync_on_startup(&manager, enabled).unwrap();
        }
        assert!(manager.enabled.get());
        assert_eq!(manager.reads.get(), 0);
        assert!(manager.writes.borrow().is_empty());
    }

    #[test]
    fn release_startup_still_syncs_and_reports_os_errors() {
        let manager = FakeAutostart::new(false);
        sync_on_startup(&manager, true).unwrap();
        assert!(manager.enabled.get());
        sync_on_startup(&manager, false).unwrap();
        assert!(!manager.enabled.get());
        manager
            .write_errors
            .borrow_mut()
            .push_back(Some("访问被拒绝"));
        assert!(sync_on_startup(&manager, true)
            .unwrap_err()
            .contains("访问被拒绝"));
    }

    #[test]
    fn saving_enables_disables_and_repeats_before_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let manager = FakeAutostart::new(false);
        for enabled in [true, true, false, false, true] {
            let settings = AppSettings {
                launch_on_startup: enabled,
                quality: "HD1".into(),
                ..AppSettings::default()
            };
            let saved = save_with_autostart(&manager, enabled, || {
                assert_eq!(manager.enabled.get(), enabled);
                save_settings_at(&settings, &path)?;
                Ok(settings.clone())
            })
            .unwrap();
            let loaded = load_settings_from(&path).unwrap();
            assert_eq!(loaded.launch_on_startup, saved.launch_on_startup);
            assert_eq!(loaded.quality, saved.quality);
        }
        assert_eq!(*manager.writes.borrow(), [true, true, false, false, true]);
    }

    #[test]
    fn query_failure_neither_writes_autostart_nor_persists() {
        let manager = FakeAutostart::new(true);
        manager.fail_read.set(true);
        let persisted = Cell::new(false);
        let error = save_with_autostart(&manager, false, || {
            persisted.set(true);
            Ok(())
        })
        .unwrap_err();
        assert!(error.contains("读取系统自启动状态失败"));
        assert!(manager.enabled.get());
        assert!(manager.writes.borrow().is_empty());
        assert!(!persisted.get());
    }

    #[test]
    fn failed_enable_or_disable_restores_actual_os_state_and_keeps_config() {
        for previous in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("settings.json");
            // The persisted preference may differ from the real OS state.
            let settings = AppSettings {
                launch_on_startup: !previous,
                ..AppSettings::default()
            };
            save_settings_at(&settings, &path).unwrap();
            let original = std::fs::read(&path).unwrap();
            let manager = FakeAutostart::new(previous);
            manager
                .write_errors
                .borrow_mut()
                .push_back(Some("访问被拒绝"));
            let error = save_with_autostart(&manager, !previous, || {
                std::fs::write(&path, "must not persist").map_err(|e| e.to_string())
            })
            .unwrap_err();
            assert!(error.contains("访问被拒绝"));
            assert_eq!(manager.enabled.get(), previous);
            assert_eq!(*manager.writes.borrow(), [!previous, previous]);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn persistence_failure_rolls_back_autostart() {
        for previous in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("settings.json");
            let old = AppSettings::default();
            save_settings_at(&old, &path).unwrap();
            let original = std::fs::read(&path).unwrap();
            let manager = FakeAutostart::new(previous);
            // A file cannot also be the parent directory of the new config.
            let invalid_path = path.join("settings.json");
            let error = save_with_autostart(&manager, !previous, || {
                save_settings_at(&old, &invalid_path)
            })
            .unwrap_err();
            assert!(error.contains("创建设置目录失败"));
            assert_eq!(manager.enabled.get(), previous);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn rollback_errors_include_the_original_failure() {
        for fail_apply in [true, false] {
            let manager = FakeAutostart::new(false);
            manager
                .write_errors
                .borrow_mut()
                .extend([fail_apply.then_some("系统写入失败"), Some("回滚被拒绝")]);
            let error =
                save_with_autostart(&manager, true, || Err::<(), _>("配置文件写入失败".into()))
                    .unwrap_err();
            assert!(error.contains(if fail_apply {
                "系统写入失败"
            } else {
                "配置文件写入失败"
            }));
            assert!(error.contains("恢复原有自启动状态也失败"));
            assert!(error.contains("回滚被拒绝"));
        }
    }
}
