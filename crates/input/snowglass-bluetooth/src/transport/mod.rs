use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{DeviceAddr, Result};

pub mod ble;
pub mod bredr;

pub type ReportReceiver = broadcast::Receiver<Bytes>;
pub type DisconnectReceiver = broadcast::Receiver<()>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

#[async_trait]
pub trait Transport: Send {
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
    async fn set_automatic_flush_slots(&mut self, _slots: u16) -> Result<()> {
        Err(crate::Error::Unsupported(
            "transport does not expose automatic flush control",
        ))
    }
    async fn disconnect(&mut self) -> Result<()>;
    fn state(&self) -> ConnectionState;
}
