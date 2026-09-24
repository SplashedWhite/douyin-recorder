use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::sync::watch;

#[derive(Clone, Default, Serialize)]
pub struct LifecycleStatus {
    pub exiting: bool,
    pub revision: u64,
    pub message: Option<String>,
}

#[derive(Default)]
struct Inner {
    status: LifecycleStatus,
    operations: usize,
    failure: Option<String>,
}

pub struct Lifecycle {
    inner: Mutex<Inner>,
    changed: watch::Sender<usize>,
    pub allow_exit: AtomicBool,
    pub close_to_tray: AtomicBool,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            changed: watch::channel(0).0,
            allow_exit: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(false),
        }
    }
}

impl Lifecycle {
    pub fn status(&self) -> LifecycleStatus {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }

    pub fn is_exiting(&self) -> bool {
        self.status().exiting
    }

    pub fn ensure_running(&self) -> Result<(), String> {
        if self.is_exiting() {
            Err("程序正在退出，请等待录制收尾".into())
        } else {
            Ok(())
        }
    }

    // Admission and the exit transition share one lock: no operation can slip
    // between the last pending-work check and process exit.
    pub fn operation(self: &Arc<Self>) -> Result<Operation, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.status.exiting {
            return Err("程序正在退出，请等待录制收尾".into());
        }
        inner.operations += 1;
        self.changed.send_replace(inner.operations);
        Ok(Operation {
            lifecycle: Arc::clone(self),
        })
    }

    pub fn begin_exit(&self) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.status.exiting {
            return false;
        }
        inner.failure = None;
        inner.status.exiting = true;
        inner.status.message = None;
        inner.status.revision += 1;
        true
    }

    pub fn report_error(&self, message: String, cancel_exit: bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if cancel_exit {
            inner.status.exiting = false;
        }
        inner.status.message = Some(message);
        inner.status.revision += 1;
    }

    pub fn record_failure(&self, message: String) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.status.exiting {
            inner.failure = Some(message);
        }
    }

    pub async fn wait_for_operations(&self) -> Result<(), String> {
        let mut changed = self.changed.subscribe();
        loop {
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.operations == 0 {
                    return inner.failure.clone().map_or(Ok(()), Err);
                }
            }
            changed.changed().await.map_err(|e| e.to_string())?;
        }
    }

    pub fn permit_exit(&self) {
        self.allow_exit.store(true, Ordering::Release);
    }
}

pub struct Operation {
    lifecycle: Arc<Lifecycle>,
}

impl Drop for Operation {
    fn drop(&mut self) {
        let mut inner = self
            .lifecycle
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        inner.operations -= 1;
        self.lifecycle.changed.send_replace(inner.operations);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn exit_blocks_new_work_waits_for_admitted_work_and_can_recover() {
        let lifecycle = Arc::new(Lifecycle::default());
        let work = lifecycle.operation().unwrap();
        assert!(lifecycle.begin_exit());
        assert!(!lifecycle.begin_exit());
        assert!(lifecycle.operation().is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), lifecycle.wait_for_operations())
                .await
                .is_err()
        );
        lifecycle.record_failure("保存失败".into());
        drop(work);
        assert_eq!(
            lifecycle.wait_for_operations().await.unwrap_err(),
            "保存失败"
        );
        lifecycle.report_error("退出未完成".into(), true);
        assert!(lifecycle.operation().is_ok());
        assert!(lifecycle.begin_exit());
        assert!(lifecycle.wait_for_operations().await.is_ok());
        assert!(!lifecycle.allow_exit.load(Ordering::Acquire));
    }
}
