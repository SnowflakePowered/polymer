use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use tokio::{
    sync::broadcast,
    task::JoinHandle,
    time::{Instant, sleep_until},
};

use crate::{ConnectionState, DeviceAddr, Error, ReportReceiver, Result, Transport};

use super::{
    LinkKey, LinkKeyStore,
    hci::{Command, Event as HciEvent, HciHost, InquiryResult, L2capFrame},
    l2cap::{Channel, ChannelSender, HID_CONTROL_PSM, HID_INTERRUPT_PSM, Link},
};

const DISCONNECTED: u8 = 0;
const CONNECTING: u8 = 1;
const CONNECTED: u8 = 2;

/// HID-over-L2CAP transport for a single BR/EDR controller.
///
/// Input values yielded by [`Transport::reports`] omit the HIDP `0xA1`
/// transaction byte and begin with the controller's report ID. Output values
/// passed to [`Transport::send`] follow the same convention; `0xA2` is added.
pub struct BrEdrHid {
    host: Arc<HciHost>,
    addr: DeviceAddr,
    keys: Arc<dyn LinkKeyStore>,
    reports: broadcast::Sender<Bytes>,
    disconnects: broadcast::Sender<()>,
    state: Arc<AtomicU8>,
    handle: Option<u16>,
    link: Option<Link>,
    control: Option<Arc<super::control::Control>>,
    control_sender: Option<ChannelSender>,
    interrupt_sender: Option<ChannelSender>,
    report_task: Option<JoinHandle<()>>,
    sdp_task: Option<JoinHandle<()>>,
    automatic_flush_slots: u16,
}

impl BrEdrHid {
    pub fn new(host: Arc<HciHost>, addr: DeviceAddr, keys: Arc<dyn LinkKeyStore>) -> Self {
        let (reports, _) = broadcast::channel(256);
        let (disconnects, _) = broadcast::channel(8);
        Self {
            host,
            addr,
            keys,
            reports,
            disconnects,
            state: Arc::new(AtomicU8::new(DISCONNECTED)),
            handle: None,
            link: None,
            control: None,
            control_sender: None,
            interrupt_sender: None,
            report_task: None,
            sdp_task: None,
            // 96 baseband slots = 60ms. Exposed for empirical tuning.
            automatic_flush_slots: 96,
        }
    }

    pub fn set_automatic_flush_slots(&mut self, slots: u16) {
        self.automatic_flush_slots = slots;
    }
    pub fn handle(&self) -> Option<u16> {
        self.handle
    }

    /// Waits until encryption maintenance on the current ACL link has been
    /// quiet for `duration`.
    ///
    /// Switch controllers can briefly pause and resume encryption several
    /// times after a bonded reconnect. Sending their sleep command during that
    /// sequence can leave the next button-wake reconnect unable to recover
    /// from an LMP transaction collision.
    pub async fn wait_for_security_quiet(&self, duration: Duration) -> Result<()> {
        let handle = self
            .handle
            .ok_or_else(|| Error::L2cap("HID link is not connected".into()))?;
        let mut events = self.host.events();
        let mut quiet_deadline = Instant::now() + duration;

        loop {
            let timer = sleep_until(quiet_deadline);
            tokio::pin!(timer);
            tokio::select! {
                _ = &mut timer => return Ok(()),
                event = events.recv() => match event {
                    Ok(HciEvent::EncryptionChange { handle: event_handle, .. }
                        | HciEvent::EncryptionKeyRefreshComplete {
                            handle: event_handle,
                            ..
                        }) if event_handle == handle => {
                            quiet_deadline = Instant::now() + duration;
                        }
                    Ok(HciEvent::DisconnectionComplete {
                        handle: event_handle,
                        reason,
                        ..
                    }) if event_handle == handle => {
                        return Err(disconnected_before_hid_ready(reason));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // We cannot prove the interval was quiet if security
                        // events may have been dropped.
                        quiet_deadline = Instant::now() + duration;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(Error::ChannelClosed);
                    }
                }
            }
        }
    }

    /// Binds a transport to a controller that initiated the ACL connection.
    /// Wii Remote and DualShock 3 reconnection use this direction.
    pub async fn attach_inbound(&mut self, handle: u16) -> Result<()> {
        let frames = self.host.l2cap_frames();
        let hci_events = self.host.events();
        self.attach_inbound_with_frames(handle, frames, hci_events)
            .await
    }

    pub async fn attach_inbound_with_frames(
        &mut self,
        handle: u16,
        frames: broadcast::Receiver<L2capFrame>,
        hci_events: broadcast::Receiver<HciEvent>,
    ) -> Result<()> {
        self.adopt(handle);
        let link = Link::new_with_frame_receiver(self.host.clone(), handle, frames);
        let (control, interrupt) = tokio::join!(
            link.accept_channel(HID_CONTROL_PSM),
            link.accept_channel(HID_INTERRUPT_PSM)
        );
        match (control, interrupt) {
            (Ok(control), Ok(interrupt)) => {
                self.finish_connection(handle, link, control, interrupt, hci_events)
                    .await
            }
            (Err(error), _) | (_, Err(error)) => self.abandon(error).await,
        }
    }

    pub async fn attach_inbound_encrypted_with_frames(
        &mut self,
        handle: u16,
        frames: broadcast::Receiver<L2capFrame>,
        mut hci_events: broadcast::Receiver<HciEvent>,
    ) -> Result<()> {
        self.adopt(handle);
        let link = Link::new_with_frame_receiver(self.host.clone(), handle, frames);
        let key = match wait_for_encryption(&mut hci_events, handle, self.addr).await {
            Ok(key) => key,
            Err(error) => return self.abandon(error).await,
        };
        if let Err(error) = self.persist(key) {
            return self.abandon(error).await;
        }
        let report_events = hci_events.resubscribe();
        let channels = async {
            tokio::join!(
                accept_channel_with_retry(&link, HID_CONTROL_PSM),
                accept_channel_with_retry(&link, HID_INTERRUPT_PSM)
            )
        };
        let channel_result = tokio::select! {
            channels = channels => Ok(channels),
            error = wait_for_disconnection(&mut hci_events, handle) => Err(error),
        };
        let (control, interrupt) = match channel_result {
            Ok(channels) => channels,
            Err(error) => return self.abandon(error).await,
        };
        match (control, interrupt) {
            (Ok(control), Ok(interrupt)) => {
                self.finish_connection(handle, link, control, interrupt, report_events)
                    .await
            }
            (Err(error), _) | (_, Err(error)) => self.abandon(error).await,
        }
    }

    /// The peer pages us, but expects host security and HID establishment.
    /// Keep one L2CAP actor alive from the first SDP probe through HID setup.
    pub async fn attach_inbound_host_hid_with_frames(
        &mut self,
        handle: u16,
        frames: broadcast::Receiver<L2capFrame>,
        hci_events: broadcast::Receiver<HciEvent>,
    ) -> Result<()> {
        self.adopt(handle);
        let link = Link::new_with_frame_receiver(self.host.clone(), handle, frames);
        self.sdp_task = Some(super::sdp::server::Server::spawn(link.clone()));
        let result = async {
            self.authenticate(handle).await?;
            // open_channel also accepts queued/crossed requests from the peer.
            let control = open_channel_with_retry(&link, HID_CONTROL_PSM).await?;
            let interrupt = open_channel_with_retry(&link, HID_INTERRUPT_PSM).await?;
            self.finish_connection(handle, link, control, interrupt, hci_events)
                .await
        }
        .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.abandon(error).await,
        }
    }

    async fn open_outbound_channels(&mut self, handle: u16) -> Result<()> {
        let hci_events = self.host.events();
        let link = Link::new(self.host.clone(), handle);
        let control = open_channel_with_retry(&link, HID_CONTROL_PSM).await?;
        let interrupt = open_channel_with_retry(&link, HID_INTERRUPT_PSM).await?;
        self.finish_connection(handle, link, control, interrupt, hci_events)
            .await
    }

    /// Opens an ACL link owned by this transport, so dropping the transport
    /// before HID setup completes still disconnects it.
    pub async fn connect_acl(&mut self, device: &InquiryResult) -> Result<u16> {
        if self.handle.is_some() {
            self.disconnect().await?;
        }
        self.state.store(CONNECTING, Ordering::Release);
        match self.host.connect(device).await {
            Ok(handle) => {
                self.handle = Some(handle);
                Ok(handle)
            }
            Err(error) => {
                self.state.store(DISCONNECTED, Ordering::Release);
                Err(error)
            }
        }
    }

    pub async fn connect_discovered(&mut self, device: &InquiryResult) -> Result<()> {
        let handle = self.connect_acl(device).await?;
        self.finish_outbound_handle(handle).await
    }

    /// Creates and authenticates the ACL link, then accepts HID channels opened
    /// by the controller. Nintendo Switch 1 controllers advertise
    /// HIDReconnectInitiate and use this direction, including during initial
    /// pairing. The L2CAP actor is installed before authentication so requests
    /// sent immediately after pairing cannot be lost.
    pub async fn connect_discovered_device_initiated_hid(
        &mut self,
        device: &InquiryResult,
    ) -> Result<()> {
        let handle = self.connect_acl(device).await?;
        self.establish_handle(handle, true).await
    }

    /// Establishes HID channels on an ACL handle already opened for SDP or
    /// another connection-time identification step.
    pub async fn establish_handle(
        &mut self,
        handle: u16,
        controller_opens_hid: bool,
    ) -> Result<()> {
        self.handle = Some(handle);
        self.state.store(CONNECTING, Ordering::Release);
        if !controller_opens_hid {
            return self.finish_outbound_handle(handle).await;
        }
        let hci_events = self.host.events();
        let link = Link::new(self.host.clone(), handle);
        if let Err(error) = self.authenticate(handle).await {
            return self.abandon(error).await;
        }
        let (control, interrupt) = tokio::join!(
            link.accept_channel(HID_CONTROL_PSM),
            link.accept_channel(HID_INTERRUPT_PSM)
        );
        match (control, interrupt) {
            (Ok(control), Ok(interrupt)) => {
                self.finish_connection(handle, link, control, interrupt, hci_events)
                    .await
            }
            (Err(error), _) | (_, Err(error)) => self.abandon(error).await,
        }
    }

    async fn finish_outbound_handle(&mut self, handle: u16) -> Result<()> {
        if let Err(error) = self.authenticate(handle).await {
            return self.abandon(error).await;
        }
        if let Err(error) = self.open_outbound_channels(handle).await {
            return self.abandon(error).await;
        }
        Ok(())
    }

    fn adopt(&mut self, handle: u16) {
        self.close_channels();
        self.handle = Some(handle);
        self.state.store(CONNECTING, Ordering::Release);
    }

    async fn abandon<T>(&mut self, error: Error) -> Result<T> {
        let _ = self.disconnect().await;
        Err(error)
    }

    async fn authenticate(&self, handle: u16) -> Result<()> {
        let key = self.host.authenticate(handle, self.addr, true).await?;
        self.persist(key)
    }

    fn persist(&self, key: Option<LinkKey>) -> Result<()> {
        match key {
            Some(key) => self.keys.put(self.addr, key),
            None => Ok(()),
        }
    }

    async fn finish_connection(
        &mut self,
        handle: u16,
        link: Link,
        control: Channel,
        mut interrupt: Channel,
        mut hci_events: broadcast::Receiver<HciEvent>,
    ) -> Result<()> {
        let response_sender = self.reports.clone();
        let disconnect_sender = self.disconnects.clone();
        let state = self.state.clone();
        let control_sender = control.sender();
        let interrupt_sender = interrupt.sender();
        self.state.store(CONNECTED, Ordering::Release);
        let report_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = interrupt.receive() => {
                        let report = match result {
                            Ok(report) => report,
                            Err(_) => break,
                        };
                        if report.is_empty() {
                            continue;
                        }
                        // HIDP DATA/Input. Handshakes and control transactions are not input reports.
                        if report[0] == 0xa1 {
                            let _ = response_sender.send(report.slice(1..));
                        } else {
                            tracing::trace!(
                                transaction = report[0],
                                "ignored non-input HIDP interrupt transaction"
                            );
                        }
                    }
                    event = hci_events.recv() => match event {
                        Ok(HciEvent::DisconnectionComplete { handle: event_handle, .. })
                            if event_handle == handle => break,
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
            state.store(DISCONNECTED, Ordering::Release);
            let _ = disconnect_sender.send(());
        });
        self.handle = Some(handle);
        self.link = Some(link);
        self.control = Some(Arc::new(super::control::Control::new(control)));
        self.control_sender = Some(control_sender);
        self.interrupt_sender = Some(interrupt_sender);
        self.report_task = Some(report_task);
        // Failure to set the latency-oriented flush timeout should not tear down
        // an otherwise working HID link; controllers may reject the command.
        let flush_result = if self.automatic_flush_slots == 0 {
            None
        } else {
            Some(
                self.host
                    .execute_checked(
                        Command::write_automatic_flush_timeout(handle, self.automatic_flush_slots),
                        "Write Automatic Flush Timeout",
                    )
                    .await,
            )
        };
        if let Some(Err(error)) = flush_result {
            tracing::warn!(handle, %error, "could not set automatic flush timeout");
        }
        Ok(())
    }

    /// Sends HID SET_REPORT(Output) on the control channel. Wii Remotes accept
    /// interrupt DATA(Output), but this transaction is the more interoperable
    /// form and is required by some compatible devices.
    pub async fn send_set_report(&self, data: &[u8]) -> Result<()> {
        let sender = self
            .control_sender
            .as_ref()
            .ok_or(Error::L2cap("HID control channel is not connected".into()))?;
        let mut transaction = BytesMut::with_capacity(data.len() + 1);
        if data.first() == Some(&0x52) {
            transaction.extend_from_slice(data);
        } else {
            transaction.extend_from_slice(&[0x52]);
            transaction.extend_from_slice(data);
        }
        sender.send(transaction.freeze(), false).await
    }

    fn close_channels(&mut self) {
        if let Some(task) = self.sdp_task.take() {
            task.abort();
        }
        if let Some(task) = self.report_task.take() {
            task.abort();
        }
        self.interrupt_sender = None;
        self.control_sender = None;
        self.control = None;
        self.link = None;
        self.handle = None;
        self.state.store(DISCONNECTED, Ordering::Release);
    }
}

async fn accept_channel_with_retry(link: &Link, psm: u16) -> Result<Channel> {
    tokio::time::timeout(Duration::from_secs(20), link.accept_channel(psm))
        .await
        .map_err(|_| Error::Timeout("inbound L2CAP channel request"))?
}

async fn open_channel_with_retry(link: &Link, psm: u16) -> Result<Channel> {
    let mut last_error = None;
    for attempt in 0..3 {
        match link.open_channel(psm).await {
            Ok(channel) => return Ok(channel),
            Err(error) => last_error = Some(error),
        }
        if attempt != 2 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Err(last_error.unwrap_or(Error::ChannelClosed))
}

async fn wait_for_encryption(
    events: &mut broadcast::Receiver<HciEvent>,
    handle: u16,
    addr: DeviceAddr,
) -> Result<Option<LinkKey>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut key = None;
        loop {
            match events.recv().await {
                Ok(HciEvent::LinkKeyNotification {
                    addr: event_addr,
                    key: value,
                    key_type,
                }) if event_addr == addr => key = Some(LinkKey { value, key_type }),
                Ok(HciEvent::EncryptionChange {
                    status: 0,
                    handle: event_handle,
                    enabled: true,
                }) if event_handle == handle => return Ok(key),
                Ok(HciEvent::EncryptionChange {
                    status,
                    handle: event_handle,
                    ..
                }) if event_handle == handle && status != 0 => {
                    return Err(Error::HciStatus {
                        operation: "Inbound Connection Encryption",
                        status,
                    });
                }
                Ok(HciEvent::DisconnectionComplete {
                    handle: event_handle,
                    reason,
                    ..
                }) if event_handle == handle => {
                    return Err(disconnected_before_hid_ready(reason));
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Err(Error::ChannelClosed),
            }
        }
    })
    .await
    .map_err(|_| Error::Timeout("inbound BR/EDR encryption"))?
}

async fn wait_for_disconnection(events: &mut broadcast::Receiver<HciEvent>, handle: u16) -> Error {
    loop {
        match events.recv().await {
            Ok(HciEvent::DisconnectionComplete {
                handle: event_handle,
                reason,
                ..
            }) if event_handle == handle => return disconnected_before_hid_ready(reason),
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return Error::ChannelClosed,
        }
    }
}

fn disconnected_before_hid_ready(reason: u8) -> Error {
    Error::L2cap(format!(
        "controller disconnected before HID channels were ready (reason 0x{reason:02x})"
    ))
}

#[async_trait]
impl Transport for BrEdrHid {
    async fn connect(&mut self, addr: DeviceAddr) -> Result<()> {
        if self.handle.is_some() {
            self.disconnect().await?;
        }
        self.addr = addr;
        self.state.store(CONNECTING, Ordering::Release);
        let handle = match self.host.connect_addr(addr).await {
            Ok(handle) => handle,
            Err(error) => {
                self.state.store(DISCONNECTED, Ordering::Release);
                return Err(error);
            }
        };
        self.handle = Some(handle);
        self.finish_outbound_handle(handle).await
    }

    fn reports(&self) -> ReportReceiver {
        self.reports.subscribe()
    }

    fn disconnections(&self) -> Option<crate::DisconnectReceiver> {
        Some(self.disconnects.subscribe())
    }

    async fn send(&mut self, data: &[u8]) -> Result<()> {
        let sender = self.interrupt_sender.as_ref().ok_or(Error::L2cap(
            "HID interrupt channel is not connected".into(),
        ))?;
        let mut transaction = BytesMut::with_capacity(data.len() + 1);
        if data.first() == Some(&0xa2) {
            transaction.extend_from_slice(data);
        } else {
            transaction.extend_from_slice(&[0xa2]);
            transaction.extend_from_slice(data);
        }
        sender.send(transaction.freeze(), true).await
    }

    async fn set_report(
        &mut self,
        kind: crate::transport::HidReportType,
        data: &[u8],
    ) -> Result<()> {
        self.hid_control()
            .ok_or(Error::ChannelClosed)?
            .set_report(kind, data)
            .await
    }

    fn hid_control(&self) -> Option<Arc<dyn crate::transport::HidControl>> {
        self.control
            .as_ref()
            .map(|control| control.clone() as Arc<dyn crate::transport::HidControl>)
    }

    async fn set_automatic_flush_slots(&mut self, slots: u16) -> Result<()> {
        self.automatic_flush_slots = slots;
        let Some(handle) = self.handle else {
            return Ok(());
        };
        self.host
            .execute_checked(
                Command::write_automatic_flush_timeout(handle, slots),
                "Write Automatic Flush Timeout",
            )
            .await
            .map(|_| ())
    }

    async fn disconnect(&mut self) -> Result<()> {
        let handle = self.handle;
        self.close_channels();
        if let Some(handle) = handle {
            self.host.disconnect(handle).await?;
        }
        Ok(())
    }

    fn state(&self) -> ConnectionState {
        match self.state.load(Ordering::Acquire) {
            CONNECTING => ConnectionState::Connecting,
            CONNECTED => ConnectionState::Connected,
            _ => ConnectionState::Disconnected,
        }
    }
}

impl Drop for BrEdrHid {
    fn drop(&mut self) {
        let handle = self
            .handle
            .filter(|_| self.state.load(Ordering::Acquire) != DISCONNECTED);
        self.close_channels();
        if let (Some(handle), Ok(runtime)) = (handle, tokio::runtime::Handle::try_current()) {
            let host = self.host.clone();
            runtime.spawn(async move {
                let _ = host.disconnect(handle).await;
            });
        }
    }
}
