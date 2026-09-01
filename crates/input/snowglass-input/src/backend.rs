use crate::{
    BackendId, DeviceDescriptor, DeviceId, DeviceState, Feedback, HdRumbleFormat, HdRumbleSink,
    PointerSample, hub::HubInner,
};
use std::{
    borrow::Cow,
    fmt,
    sync::{Arc, Weak},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendError {
    Unsupported,
    Unavailable(String),
    Failed(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => formatter.write_str("operation is unsupported"),
            Self::Unavailable(message) | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for BackendError {}

pub trait RawHidFilter: Send + Sync {
    fn filter_input(&self, report: &mut [u8], capture: bool);
    fn filter_output<'a>(&self, report: &'a [u8], capture: bool) -> Cow<'a, [u8]>;
}

pub trait InputBackend: Send + Sync + 'static {
    fn id(&self) -> BackendId;
    fn start(&self, handle: BackendHandle) -> Result<(), BackendError>;
    fn shutdown(&self);
    fn apply_feedback(&self, device: &DeviceId, feedback: &Feedback) -> Result<(), BackendError>;

    fn open_hd_rumble(
        &self,
        _device: &DeviceId,
        _format: HdRumbleFormat,
    ) -> Result<Box<dyn HdRumbleSink>, BackendError> {
        Err(BackendError::Unsupported)
    }

    fn send_raw_hid(&self, _device: &DeviceId, _report: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    fn set_raw_hid_capture(&self, _device: &DeviceId, _enabled: bool) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    fn raw_hid_filter(&self, _device: &DeviceId) -> Option<Arc<dyn RawHidFilter>> {
        None
    }
}

#[derive(Clone)]
pub struct BackendHandle {
    pub(crate) hub: Weak<HubInner>,
    pub(crate) backend: BackendId,
}

impl BackendHandle {
    pub(crate) fn new(hub: &Arc<HubInner>, backend: BackendId) -> Self {
        Self {
            hub: Arc::downgrade(hub),
            backend,
        }
    }

    pub const fn backend(&self) -> BackendId {
        self.backend
    }

    pub fn connected(&self, descriptor: DeviceDescriptor) {
        if let Some(hub) = self.hub.upgrade() {
            hub.connected(self.backend, descriptor);
        }
    }

    pub fn disconnected(&self, device: &DeviceId) {
        if let Some(hub) = self.hub.upgrade() {
            hub.disconnected(self.backend, device);
        }
    }

    pub fn publish(&self, device: &DeviceId, state: &DeviceState) {
        if let Some(hub) = self.hub.upgrade() {
            hub.publish(self.backend, device, state);
        }
    }

    pub fn publish_pointer(&self, device: &DeviceId, sample: &PointerSample) {
        if let Some(hub) = self.hub.upgrade() {
            hub.publish_pointer(self.backend, device, sample);
        }
    }

    pub fn publish_raw_hid(&self, device: &DeviceId, report: &[u8]) {
        if let Some(hub) = self.hub.upgrade() {
            hub.publish_raw_hid(self.backend, device, report);
        }
    }

    pub fn unsupported(&self, device: &DeviceId, reason: &str) {
        tracing::warn!(backend = %self.backend, device = %device, %reason, "input device unsupported");
    }
}
