use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{DeviceAddr, Result};

pub mod ble;
pub mod bredr;

pub type ReportReceiver = broadcast::Receiver<Bytes>;
pub type DisconnectReceiver = broadcast::Receiver<()>;

/// HID report type for acknowledged SET_REPORT transactions on a control channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidReportType {
    Output = 2,
    Feature = 3,
}

/// Serialized HID control transactions, shareable with a protocol maintenance task.
#[async_trait]
pub trait HidControl: Send + Sync {
    async fn set_report(&self, kind: HidReportType, data: &[u8]) -> Result<()>;
    /// Returns the device payload unchanged, without the HIDP DATA header.
    async fn get_report(&self, kind: HidReportType, id: u8, size: u16) -> Result<Bytes>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

#[async_trait]
pub trait Transport: Send {
    fn hid_control(&self) -> Option<std::sync::Arc<dyn HidControl>> {
        None
    }
    async fn get_report(&mut self, kind: HidReportType, id: u8, size: u16) -> Result<Bytes> {
        let control = self
            .hid_control()
            .ok_or(crate::Error::Unsupported("HID GET_REPORT"))?;
        control.get_report(kind, id, size).await
    }
    async fn connect(&mut self, addr: DeviceAddr) -> Result<()>;
    fn reports(&self) -> ReportReceiver;
    /// The active BLE input characteristic, when the transport has one.
    /// Controller discovery can use this to refine a family-level match after
    /// GATT services become available.
    fn input_characteristic_uuid(&self) -> Option<Uuid> {
        None
    }
    /// Optional device-command response stream, distinct from raw HID input.
    fn responses(&self) -> Option<ReportReceiver> {
        None
    }
    /// Notifies the owner when the remote link disappears without a local
    /// disconnect request. Transports which cannot observe this may return
    /// `None`; report-stream closure remains the manager's fallback signal.
    fn disconnections(&self) -> Option<DisconnectReceiver> {
        None
    }
    async fn send(&mut self, data: &[u8]) -> Result<()>;
    async fn set_report(&mut self, _kind: HidReportType, _data: &[u8]) -> Result<()> {
        Err(crate::Error::Unsupported(
            "transport does not support HID control SET_REPORT",
        ))
    }
    async fn set_automatic_flush_slots(&mut self, _slots: u16) -> Result<()> {
        Err(crate::Error::Unsupported(
            "transport does not expose automatic flush control",
        ))
    }
    async fn disconnect(&mut self) -> Result<()>;
    fn state(&self) -> ConnectionState;
}
