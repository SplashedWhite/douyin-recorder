use crate::{lifecycle::LifecycleStatus, recorder::STOP_WAIT_TIMEOUT, AppState};
use std::sync::atomic::Ordering;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};

const TRAY_ID: &str = "main-tray";
pub const STATUS_EVENT: &str = "lifecycle-status-changed";

trait Presentation {
    fn show_tray(&self) -> Result<(), String>;
    fn hide_tray(&self) -> Result<(), String>;
    fn show_window(&self) -> Result<(), String>;
    fn hide_window(&self) -> Result<(), String>;
}

fn enter_tray(presentation: &impl Presentation) -> Result<(), String> {
    presentation.show_tray()?;
    if let Err(error) = presentation.hide_window() {
        let _ = presentation.hide_tray();
        return Err(error);
    }
    Ok(())
}

fn leave_tray(presentation: &impl Presentation) -> Result<(), String> {
    presentation.show_window()?;
    presentation.hide_tray()
}

struct Desktop<'a>(&'a AppHandle);

impl Presentation for Desktop<'_> {
    fn show_tray(&self) -> Result<(), String> {
        ensure_tray_visible(self.0)
    }
    fn hide_tray(&self) -> Result<(), String> {
        if let Some(tray) = self.0.tray_by_id(TRAY_ID) {
            tray.set_visible(false).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn show_window(&self) -> Result<(), String> {
        let window = self
            .0
            .get_webview_window("main")
            .ok_or("主窗口尚未准备好")?;
        window.show().map_err(|e| e.to_string())?;
        window.unminimize().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())
    }
    fn hide_window(&self) -> Result<(), String> {
        self.0
            .get_webview_window("main")
            .ok_or("主窗口尚未准备好")?
            .hide()
            .map_err(|e| e.to_string())
    }
}

pub fn emit_status(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        let _ = app.emit(STATUS_EVENT, state.lifecycle.status());
    }
}

pub fn report_error(app: &AppHandle, message: String) {
    eprintln!("{message}");
    if let Some(state) = app.try_state::<AppState>() {
        state.lifecycle.report_error(message, false);
        emit_status(app);
    }
}

// All callers dispatch window/tray transitions onto the event-loop thread.
pub fn restore_window(app: &AppHandle) -> Result<(), String> {
    leave_tray(&Desktop(app))
}

pub fn request_restore(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        if let Err(error) = restore_window(&handle) {
            report_error(&handle, format!("恢复窗口失败: {error}"));
        }
    }) {
        report_error(app, format!("恢复窗口失败: {error}"));
    }
}

fn ensure_tray_visible(app: &AppHandle) -> Result<(), String> {
    let tray = match app.tray_by_id(TRAY_ID) {
        Some(tray) => tray,
        None => {
            let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)
                .map_err(|e| e.to_string())?;
            let quit = MenuItem::with_id(app, "quit", "退出程序", true, None::<&str>)
                .map_err(|e| e.to_string())?;
            let menu = Menu::with_items(app, &[&show, &quit]).map_err(|e| e.to_string())?;
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().ok_or("无法读取应用图标")?.clone())
                .tooltip("抖音直播录制")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => request_restore(app),
                    "quit" => request_exit(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        request_restore(tray.app_handle());
                    }
                })
                .build(app)
                .map_err(|e| e.to_string())?
        }
    };
    tray.set_visible(true).map_err(|e| e.to_string())?;
    // On Windows the tray backend can return Ok while Explorer is unavailable.
    // Confirm registration before taking away the taskbar entry.
    #[cfg(windows)]
    if tray.rect().map_err(|e| e.to_string())?.is_none() {
        return Err("系统托盘暂不可用，窗口将保持打开，请稍后重试".into());
    }
    Ok(())
}

pub fn request_close(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if state.lifecycle.is_exiting() {
        return;
    }
    if state.lifecycle.close_to_tray.load(Ordering::Acquire) {
        if let Err(error) = enter_tray(&Desktop(app)) {
            report_error(app, format!("隐藏到托盘失败: {error}"));
        }
    } else {
        request_exit(app);
    }
}

async fn finish_pending_work(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let completions = {
        // Synchronize with a recording that was being registered when exit began.
        let _start = state.start_lock.lock().await;
        state.recorder.request_stop_all_for_exit()?
    };
    let mut failure = None;
    for completion in completions {
        if let Err(error) = completion.wait().await {
            failure = Some(error);
        }
    }
    if let Err(error) = state.lifecycle.wait_for_operations().await {
        failure = Some(error);
    }
    if let Some(error) = failure {
        return Err(error);
    }
    if state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .has_running_tasks()
        .map_err(|e| e.to_string())?
    {
        return Err("仍有录制结果未保存完成，请检查任务状态后重试".into());
    }
    Ok(())
}

pub fn request_exit(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if !state.lifecycle.begin_exit() {
        return;
    }
    emit_status(app);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        // Make an ongoing shutdown visible without flashing the window on an idle exit.
        let work = finish_pending_work(&handle);
        tokio::pin!(work);
        let result = tokio::time::timeout(STOP_WAIT_TIMEOUT, async {
            tokio::select! {
                result = &mut work => result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(150)) => {
                    request_restore(&handle);
                    work.await
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err("等待录制或转换收尾超时，程序已保留，请检查任务状态后重试".into()));
        let state = handle.state::<AppState>();
        match result {
            Ok(()) => {
                state.lifecycle.permit_exit();
                handle.exit(0);
            }
            Err(error) => {
                state
                    .lifecycle
                    .report_error(format!("退出未完成: {error}"), true);
                request_restore(&handle);
                emit_status(&handle);
            }
        }
    });
}

#[tauri::command]
pub fn get_lifecycle_status(state: tauri::State<AppState>) -> LifecycleStatus {
    state.lifecycle.status()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeDesktop {
        window: Cell<bool>,
        tray: Cell<bool>,
        fail: &'static str,
    }
    impl Presentation for FakeDesktop {
        fn show_tray(&self) -> Result<(), String> {
            if self.fail == "show_tray" {
                return Err("托盘不可用".into());
            }
            self.tray.set(true);
            Ok(())
        }
        fn hide_tray(&self) -> Result<(), String> {
            if self.fail == "hide_tray" {
                return Err("托盘隐藏失败".into());
            }
            self.tray.set(false);
            Ok(())
        }
        fn show_window(&self) -> Result<(), String> {
            if self.fail == "show_window" {
                return Err("窗口恢复失败".into());
            }
            self.window.set(true);
            Ok(())
        }
        fn hide_window(&self) -> Result<(), String> {
            if self.fail == "hide_window" {
                return Err("窗口隐藏失败".into());
            }
            self.window.set(false);
            Ok(())
        }
    }

    #[test]
    fn tray_transitions_always_leave_an_accessible_entry_on_failure() {
        for fail in ["show_tray", "hide_window"] {
            let desktop = FakeDesktop {
                window: Cell::new(true),
                tray: Cell::new(false),
                fail,
            };
            assert!(enter_tray(&desktop).is_err());
            assert!(desktop.window.get());
            assert!(!desktop.tray.get());
        }
        for fail in ["show_window", "hide_tray"] {
            let desktop = FakeDesktop {
                window: Cell::new(false),
                tray: Cell::new(true),
                fail,
            };
            assert!(leave_tray(&desktop).is_err());
            assert!(desktop.tray.get());
        }
    }

    #[test]
    fn repeated_close_and_restore_never_leave_both_entries_hidden() {
        let desktop = FakeDesktop {
            window: Cell::new(true),
            tray: Cell::new(false),
            fail: "",
        };
        for _ in 0..5 {
            enter_tray(&desktop).unwrap();
            enter_tray(&desktop).unwrap();
            assert!(!desktop.window.get() && desktop.tray.get());
            leave_tray(&desktop).unwrap();
            leave_tray(&desktop).unwrap();
            assert!(desktop.window.get() && !desktop.tray.get());
        }
    }
}
