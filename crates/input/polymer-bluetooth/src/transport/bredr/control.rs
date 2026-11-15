use super::l2cap::Channel;
use crate::{
    Error, Result,
    transport::{HidControl, HidReportType},
};
use async_trait::async_trait;
use bytes::Bytes;
use std::time::Duration;
use tokio::{sync::Mutex, time::Instant};

pub(super) struct Control {
    channel: Mutex<(Channel, Option<Instant>)>,
}

impl Control {
    pub(super) fn new(channel: Channel) -> Self {
        Self {
            channel: Mutex::new((channel, None)),
        }
    }

    async fn exchange(&self, request: Vec<u8>) -> Result<Bytes> {
        let mut guard = self.channel.lock().await;
        let (channel, owed_until) = &mut *guard;
        if let Some(deadline) = *owed_until
            && let Ok(owed) = tokio::time::timeout_at(deadline, channel.receive()).await
        {
            owed?;
        }
        // Leave poisoned on cancellation, timeout or I/O failure. A late ACK has
        // no transaction ID and must never complete the next command.
        *owed_until = Some(Instant::now() + Duration::from_secs(4));
        let reply = tokio::time::timeout(Duration::from_secs(2), async {
            channel.sender().send(Bytes::from(request), false).await?;
            channel.receive().await
        })
        .await
        .map_err(|_| Error::Timeout("HID control transaction"))??;
        *owed_until = None;
        Ok(reply)
    }
}

#[async_trait]
impl HidControl for Control {
    async fn set_report(&self, kind: HidReportType, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Err(Error::InvalidPacket("empty HID SET_REPORT"));
        }
        let mut request = vec![0x50 | kind as u8];
        request.extend_from_slice(data);
        let reply = self.exchange(request).await?;
        if reply.as_ref() != [0] {
            return Err(Error::L2cap(format!(
                "HID SET_REPORT rejected: {reply:02x?}"
            )));
        }
        Ok(())
    }

    async fn get_report(&self, kind: HidReportType, id: u8, size: u16) -> Result<Bytes> {
        if size == 0 || size > 512 {
            return Err(Error::InvalidPacket("invalid HID GET_REPORT size"));
        }
        let [lo, hi] = size.to_le_bytes();
        let reply = self.exchange(vec![0x48 | kind as u8, id, lo, hi]).await?;
        if reply.first() != Some(&(0xa0 | kind as u8))
            || reply.len() < 2
            || reply.len() > usize::from(size) + 1
        {
            return Err(Error::L2cap(format!(
                "HID GET_REPORT rejected: {reply:02x?}"
            )));
        }
        Ok(reply.slice(1..))
    }
}
