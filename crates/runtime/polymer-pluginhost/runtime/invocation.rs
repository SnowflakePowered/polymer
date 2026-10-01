use crate::controller::ControllerSession;
use deno_core::v8;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use thiserror::Error;
use tokio::sync::Notify;

/// Scheduling policy for plugin workers.
#[derive(Clone)]
pub struct PluginRuntimeOptions {
    /// Maximum number of fresh isolates executing at the same time. Tokio's
    /// semaphore is fair, so excess calls are admitted in FIFO order.
    pub max_concurrent_calls: usize,
    /// Where jobs started by plugins are reported.
    pub jobs: Option<Arc<polymer_library::jobs::JobStore>>,
}

impl Default for PluginRuntimeOptions {
    fn default() -> Self {
        Self {
            max_concurrent_calls: 2,
            jobs: None,
        }
    }
}

/// Structured error returned to either DOM-facing adapter.
#[derive(Clone, Debug, Deserialize, Error, Serialize)]
#[error("{name}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct PluginInvocationError {
    pub name: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Result of a plugin call before application-owned resources are converted
/// into frontend-visible opaque handles.
pub struct PluginInvocationResult {
    pub value: Value,
    pub prepared_filesystem: Option<polymer_vfs::PreparedFilesystemLaunch>,
    pub controller_session: Option<ControllerSession>,
}

impl PluginInvocationError {
    pub fn new(name: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            code: code.to_owned(),
            message: message.into(),
            stack: None,
            details: None,
        }
    }

    pub(crate) fn cancelled() -> Self {
        Self::new(
            "AbortError",
            "PLUGIN_CANCELLED",
            "plugin invocation was cancelled",
        )
    }

    pub(crate) fn worker(error: impl std::fmt::Display) -> Self {
        let message = error.to_string();
        Self {
            name: if message.contains("TypeError") {
                "TypeError".to_owned()
            } else {
                "Error".to_owned()
            },
            code: "PLUGIN_WORKER_FAILED".to_owned(),
            stack: Some(message.clone()),
            details: None,
            message,
        }
    }
}

pub(crate) struct InvocationControl {
    cancelled: AtomicBool,
    isolate: Mutex<Option<v8::IsolateHandle>>,
    cancelled_signal: Notify,
}

/// Single-use cancellation capability for one queued or executing invocation.
///
/// Adapters create this before scheduling work so a protocol cancellation can
/// reach the invocation even while it is waiting for a worker permit.
#[derive(Clone)]
pub struct PluginCancellationToken {
    pub(crate) control: Arc<InvocationControl>,
}

impl PluginCancellationToken {
    pub fn cancel(&self) {
        self.control.cancel();
    }
}

impl InvocationControl {
    pub(crate) fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            isolate: Mutex::new(None),
            cancelled_signal: Notify::new(),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancelled_signal.notify_one();
        if let Some(isolate) = self.isolate.lock().as_ref() {
            isolate.terminate_execution();
        }
    }

    pub(crate) fn install_isolate(&self, isolate: v8::IsolateHandle) -> bool {
        *self.isolate.lock() = Some(isolate);
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn cancelled_notification(&self) -> impl std::future::Future<Output = ()> + '_ {
        self.cancelled_signal.notified()
    }
}
