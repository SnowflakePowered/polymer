use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::broadcast;

use crate::{DeviceAddr, Result};

pub type ReportReceiver = broadcast::Receiver<Bytes>;

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
    async fn send(&mut self, data: &[u8]) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    fn state(&self) -> ConnectionState;
}
