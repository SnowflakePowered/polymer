use std::{collections::VecDeque, sync::Arc, time::Duration};

use bytes::Bytes;
use tokio::{
    sync::{Mutex, broadcast, mpsc, oneshot},
    task::JoinHandle,
    time,
};

use crate::{
    DeviceAddr, Error, Result,
    firmware::{FirmwareIdentity, FirmwareLoader, FirmwareRegistry},
};

use super::{
    AclCredits, AclPacket, AclReassembler, AdapterSelector, Command, Event, InquiryResult, Opcode,
    UsbHci,
    usb::{AclReceiver, AclSender, CommandSender, EventReceiver},
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(6);
const EVENT_MASK: u64 = 0x3dbf_f807_fffb_ffff;

#[derive(Clone, Debug)]
pub struct ControllerInfo {
    pub usb_vendor_id: u16,
    pub usb_product_id: u16,
    pub address: DeviceAddr,
    pub hci_version: u8,
    pub hci_revision: u16,
    pub lmp_version: u8,
    pub manufacturer: u16,
    pub lmp_subversion: u16,
    pub firmware_loaded_by_host: bool,
    pub firmware_source: Option<String>,
    pub features: [u8; 8],
    pub acl_data_packet_length: u16,
    pub total_acl_packets: u16,
    pub le_acl_data_packet_length: Option<u16>,
    pub total_le_acl_packets: Option<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct L2capFrame {
    pub handle: u16,
    /// A complete L2CAP PDU, including the length and CID header.
    pub pdu: Bytes,
}

enum AclWorkerMessage {
    Send {
        packet: AclPacket,
        completion: oneshot::Sender<Result<()>>,
    },
    Completed(Vec<(u16, u16)>),
    Forget(u16),
    SetCapacity(u16),
}

struct QueuedAcl {
    packet: AclPacket,
    completion: oneshot::Sender<Result<()>>,
}

pub struct HciHost {
    command: CommandSender,
    command_lock: Arc<Mutex<()>>,
    acl_worker: mpsc::Sender<AclWorkerMessage>,
    events: broadcast::Sender<Event>,
    l2cap_frames: broadcast::Sender<L2capFrame>,
    info: ControllerInfo,
    tasks: Vec<JoinHandle<()>>,
}

impl HciHost {
    pub async fn open(selector: &AdapterSelector) -> Result<Self> {
        Self::open_with_firmware(selector, FirmwareRegistry::embedded()).await
    }

    pub async fn open_with_firmware(
        selector: &AdapterSelector,
        firmware: FirmwareRegistry,
    ) -> Result<Self> {
        let usb = UsbHci::open(selector).await?;
        let usb_vendor_id = usb.info().vendor_id;
        let usb_product_id = usb.info().product_id;
        let (command, event_rx, acl_tx, acl_rx) = usb.split()?;
        let (event_sender, _) = broadcast::channel(256);
        let (l2cap_sender, _) = broadcast::channel(1024);
        let (acl_worker_sender, acl_worker_rx) = mpsc::channel(256);

        // Temporary credit capacity lets initialization run before the actual
        // Read Buffer Size result is known. No ACL packet is sent during init.
        let acl_task = tokio::spawn(acl_worker(acl_tx, acl_worker_rx, 1));
        let dispatch_task = tokio::spawn(dispatch_events(
            event_rx,
            acl_rx,
            event_sender.clone(),
            l2cap_sender.clone(),
            acl_worker_sender.clone(),
        ));

        let mut host = Self {
            command,
            command_lock: Arc::new(Mutex::new(())),
            acl_worker: acl_worker_sender,
            events: event_sender,
            l2cap_frames: l2cap_sender,
            info: ControllerInfo {
                usb_vendor_id,
                usb_product_id,
                address: DeviceAddr::public([0; 6]),
                hci_version: 0,
                hci_revision: 0,
                lmp_version: 0,
                manufacturer: 0,
                lmp_subversion: 0,
                firmware_loaded_by_host: false,
                firmware_source: None,
                features: [0; 8],
                acl_data_packet_length: 0,
                total_acl_packets: 0,
                le_acl_data_packet_length: None,
                total_le_acl_packets: None,
            },
            tasks: vec![acl_task, dispatch_task],
        };
        host.initialize(&firmware).await?;
        host.acl_worker
            .send(AclWorkerMessage::SetCapacity(host.info.total_acl_packets))
            .await
            .map_err(|_| Error::ChannelClosed)?;
        Ok(host)
    }

    async fn initialize(&mut self, firmware: &FirmwareRegistry) -> Result<()> {
        self.execute_checked(Command::reset(), "Reset").await?;
        self.execute_checked(Command::set_event_mask(EVENT_MASK), "Set Event Mask")
            .await?;

        self.read_local_version().await?;
        let firmware_identity = self.firmware_identity();
        if let Some(loader) = firmware.matching(&firmware_identity) {
            self.load_firmware(loader.as_ref(), &firmware_identity)
                .await?;
            self.read_local_version().await?;
            if loader.recognizes(&self.firmware_identity()) {
                return Err(Error::Firmware(
                    "adapter accepted the firmware download but remained in ROM mode".into(),
                ));
            }
        }

        let features = self
            .execute(Command::new(
                Opcode::READ_LOCAL_SUPPORTED_FEATURES,
                Bytes::new(),
            ))
            .await?;
        ensure_status("Read Local Supported Features", &features)?;
        if features.len() < 9 {
            return Err(Error::InvalidPacket(
                "short Read Local Supported Features response",
            ));
        }
        self.info.features.copy_from_slice(&features[1..9]);

        let buffers = self
            .execute(Command::new(Opcode::READ_BUFFER_SIZE, Bytes::new()))
            .await?;
        ensure_status("Read Buffer Size", &buffers)?;
        if buffers.len() < 8 {
            return Err(Error::InvalidPacket("short Read Buffer Size response"));
        }
        self.info.acl_data_packet_length = u16::from_le_bytes([buffers[1], buffers[2]]);
        self.info.total_acl_packets = u16::from_le_bytes([buffers[4], buffers[5]]);
        if self.info.acl_data_packet_length == 0 || self.info.total_acl_packets == 0 {
            return Err(Error::Unsupported(
                "Bluetooth controller exposes no BR/EDR ACL buffers",
            ));
        }

        match self
            .execute(Command::new(Opcode::LE_READ_BUFFER_SIZE, Bytes::new()))
            .await
        {
            Ok(le) if le.len() >= 4 && le[0] == 0 => {
                self.info.le_acl_data_packet_length = Some(u16::from_le_bytes([le[1], le[2]]));
                self.info.total_le_acl_packets = Some(le[3]);
            }
            _ => {}
        }

        let address = self
            .execute(Command::new(Opcode::READ_BD_ADDR, Bytes::new()))
            .await?;
        ensure_status("Read BD_ADDR", &address)?;
        if address.len() < 7 {
            return Err(Error::InvalidPacket("short Read BD_ADDR response"));
        }
        self.info.address = DeviceAddr::from_hci_bytes(address[1..7].try_into().unwrap());

        self.execute_checked(Command::clear_event_filters(), "Clear Event Filters")
            .await?;
        self.execute_checked(
            Command::write_simple_pairing_mode(true),
            "Write Simple Pairing Mode",
        )
        .await?;
        // Authentication is selected per controller. In particular, DS3 must
        // accept an inbound HID link without standard authentication.
        self.execute_checked(
            Command::write_authentication_enable(false),
            "Write Authentication Enable (disabled)",
        )
        .await?;
        self.execute_checked(Command::write_local_name("Snowglass"), "Write Local Name")
            .await?;
        self.execute_checked(
            Command::write_class_of_device([0x0c, 0x01, 0x1c]),
            "Write Class Of Device (Laptop)",
        )
        .await?;
        self.execute_checked(
            Command::write_default_link_policy(0x000f),
            "Write Default Link Policy",
        )
        .await?;
        // Do not inherit an adapter/driver-selected RSSI or extended inquiry
        // format. Standard results are small, universally supported, and carry
        // everything needed to page a discovered controller.
        self.execute_checked(
            Command::write_inquiry_mode(0),
            "Write Inquiry Mode (standard)",
        )
        .await?;
        self.execute_checked(
            Command::write_connection_accept_timeout(0x7d00),
            "Write Connection Accept Timeout",
        )
        .await?;
        self.execute_checked(Command::write_page_timeout(0x2000), "Write Page Timeout")
            .await?;
        self.execute_checked(
            Command::write_page_scan_activity(0x0050, 0x0012),
            "Write Page Scan Activity",
        )
        .await?;
        self.execute_checked(
            Command::write_page_scan_type(true),
            "Write Page Scan Type (interlaced)",
        )
        .await?;
        self.execute_checked(Command::write_scan_enable(false, true), "Write Scan Enable")
            .await?;
        Ok(())
    }

    async fn read_local_version(&mut self) -> Result<()> {
        let version = self
            .execute(Command::new(
                Opcode::READ_LOCAL_VERSION_INFORMATION,
                Bytes::new(),
            ))
            .await?;
        ensure_status("Read Local Version Information", &version)?;
        if version.len() < 9 {
            return Err(Error::InvalidPacket(
                "short Read Local Version Information response",
            ));
        }
        self.info.hci_version = version[1];
        self.info.hci_revision = u16::from_le_bytes([version[2], version[3]]);
        self.info.lmp_version = version[4];
        self.info.manufacturer = u16::from_le_bytes([version[5], version[6]]);
        self.info.lmp_subversion = u16::from_le_bytes([version[7], version[8]]);
        Ok(())
    }

    fn firmware_identity(&self) -> FirmwareIdentity {
        FirmwareIdentity {
            vendor_id: self.info.usb_vendor_id,
            product_id: self.info.usb_product_id,
            hci_version: self.info.hci_version,
            hci_revision: self.info.hci_revision,
            manufacturer: self.info.manufacturer,
            lmp_subversion: self.info.lmp_subversion,
        }
    }

    async fn load_firmware(
        &mut self,
        loader: &dyn FirmwareLoader,
        identity: &FirmwareIdentity,
    ) -> Result<()> {
        const READ_ROM_VERSION: Opcode = Opcode(0xfc6d);
        const DOWNLOAD_FIRMWARE: Opcode = Opcode(0xfc20);

        let rom_version = if identity.lmp_subversion == 0x1200 {
            None
        } else {
            let rom = self
                .execute(Command::new(READ_ROM_VERSION, Bytes::new()))
                .await?;
            ensure_status("Read firmware ROM version", &rom)?;
            Some(
                *rom.get(1)
                    .ok_or(Error::InvalidPacket("short firmware ROM version response"))?,
            )
        };
        let security_key_id = if loader.requires_security_key(identity) {
            const READ_REG16: Opcode = Opcode(0xfc61);
            let response = self
                .execute(Command::new(
                    READ_REG16,
                    Bytes::from_static(&[0x10, 0xa4, 0xad, 0x00, 0xb0]),
                ))
                .await?;
            ensure_status("Read firmware security project", &response)?;
            Some(*response.get(1).ok_or(Error::InvalidPacket(
                "short firmware security project response",
            ))?)
        } else {
            None
        };
        let prepared = loader
            .prepare(identity, rom_version, security_key_id)?
            .ok_or_else(|| {
                Error::Firmware(format!("{} did not provide firmware", loader.name()))
            })?;
        let patch = prepared.bytes;

        const DOWNLOAD_FRAGMENT_LENGTH: usize = 252;
        let fragment_count = patch.len().div_ceil(DOWNLOAD_FRAGMENT_LENGTH);
        let mut packet_index = 0u8;
        for fragment in 0..fragment_count {
            let start = fragment * DOWNLOAD_FRAGMENT_LENGTH;
            let end = (start + DOWNLOAD_FRAGMENT_LENGTH).min(patch.len());
            let last = fragment + 1 == fragment_count;
            let mut parameters = Vec::with_capacity(1 + end - start);
            parameters.push(packet_index | if last { 0x80 } else { 0 });
            parameters.extend_from_slice(&patch[start..end]);
            let response = self
                .execute(Command::new(DOWNLOAD_FIRMWARE, Bytes::from(parameters)))
                .await?;
            ensure_status("Download adapter firmware", &response)?;
            if response.get(1).copied() != Some(packet_index | if last { 0x80 } else { 0 }) {
                return Err(Error::InvalidPacket(
                    "firmware fragment acknowledgement mismatch",
                ));
            }
            packet_index = packet_index.wrapping_add(1);
            if packet_index == 0x7f {
                packet_index = 1;
            }
        }

        self.info.firmware_loaded_by_host = true;
        self.info.firmware_source = Some(format!("{}:{}", prepared.loader, prepared.family));
        Ok(())
    }

    pub fn info(&self) -> &ControllerInfo {
        &self.info
    }

    /// Applies the short-lived host persona requested by a device profile.
    /// The manager serializes connection attempts and restores Snowglass's
    /// neutral identity after the HID link is established.
    pub async fn set_host_identity(&self, name: &str, class_of_device: [u8; 3]) -> Result<()> {
        self.execute_checked(Command::write_local_name(name), "Write Local Name")
            .await?;
        self.execute_checked(
            Command::write_class_of_device(class_of_device),
            "Write Class Of Device",
        )
        .await?;
        Ok(())
    }
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
    pub fn l2cap_frames(&self) -> broadcast::Receiver<L2capFrame> {
        self.l2cap_frames.subscribe()
    }

    pub async fn inquiry(
        &self,
        duration_units: u8,
        max_responses: u8,
    ) -> Result<Vec<InquiryResult>> {
        self.inquiry_mode(duration_units, max_responses, false)
            .await
    }

    /// Performs inquiry with the Wii's LIAC rather than the normal GIAC.
    pub async fn limited_inquiry(
        &self,
        duration_units: u8,
        max_responses: u8,
    ) -> Result<Vec<InquiryResult>> {
        self.inquiry_mode(duration_units, max_responses, true).await
    }

    async fn inquiry_mode(
        &self,
        duration_units: u8,
        max_responses: u8,
        limited: bool,
    ) -> Result<Vec<InquiryResult>> {
        let mut events = self.events.subscribe();
        let duration_units = duration_units.clamp(1, 0x30);
        let command = if limited {
            Command::limited_inquiry(duration_units, max_responses)
        } else {
            Command::inquiry(duration_units, max_responses)
        };
        self.execute(command).await?;
        time::timeout(
            Duration::from_secs(u64::from(duration_units) * 2 + 3),
            async {
                let mut results = Vec::new();
                loop {
                    match events.recv().await.map_err(|_| Error::ChannelClosed)? {
                        Event::InquiryResults(mut batch) => results.append(&mut batch),
                        Event::InquiryComplete { status: 0 } => return Ok(results),
                        Event::InquiryComplete { status } => {
                            return Err(Error::HciStatus {
                                operation: "Inquiry",
                                status,
                            });
                        }
                        _ => {}
                    }
                }
            },
        )
        .await
        .map_err(|_| Error::Timeout("Bluetooth inquiry"))?
    }

    /// Stops an active BR/EDR inquiry. A controller may report Command
    /// Disallowed if the inquiry completed between the UI toggle and this
    /// command, so callers stopping a background scanner may safely ignore
    /// that status.
    pub async fn cancel_inquiry(&self) -> Result<()> {
        self.execute_checked(Command::inquiry_cancel(), "Inquiry Cancel")
            .await?;
        Ok(())
    }

    pub async fn remote_name(&self, result: &InquiryResult) -> Result<String> {
        let mut events = self.events.subscribe();
        self.execute(Command::remote_name_request(
            result.addr,
            result.clock_offset,
        ))
        .await?;
        time::timeout(Duration::from_secs(8), async {
            loop {
                if let Event::RemoteNameComplete { status, addr, name } =
                    events.recv().await.map_err(|_| Error::ChannelClosed)?
                {
                    if addr != result.addr {
                        continue;
                    }
                    if status == 0 {
                        return Ok(name);
                    }
                    return Err(Error::HciStatus {
                        operation: "Remote Name Request",
                        status,
                    });
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("remote name"))?
    }

    pub async fn remote_name_addr(&self, addr: DeviceAddr) -> Result<String> {
        self.remote_name(&InquiryResult {
            addr,
            class_of_device: [0; 3],
            clock_offset: 0,
            rssi: None,
        })
        .await
    }

    pub async fn read_remote_features(&self, handle: u16) -> Result<[u8; 8]> {
        let mut events = self.events.subscribe();
        self.execute(Command::handle_only(
            Opcode::READ_REMOTE_SUPPORTED_FEATURES,
            handle,
        ))
        .await?;
        time::timeout(Duration::from_secs(5), async {
            loop {
                if let Event::RemoteSupportedFeaturesComplete {
                    status,
                    handle: event_handle,
                    features,
                } = events.recv().await.map_err(|_| Error::ChannelClosed)?
                {
                    if event_handle != handle {
                        continue;
                    }
                    if status == 0 {
                        return Ok(features);
                    }
                    return Err(Error::HciStatus {
                        operation: "Read Remote Supported Features",
                        status,
                    });
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("remote supported features"))?
    }

    pub async fn read_remote_extended_features(&self, handle: u16, page: u8) -> Result<[u8; 8]> {
        let mut events = self.events.subscribe();
        self.execute(Command::read_remote_extended_features(handle, page))
            .await?;
        time::timeout(Duration::from_secs(5), async {
            loop {
                if let Event::RemoteExtendedFeaturesComplete {
                    status,
                    handle: event_handle,
                    page: event_page,
                    features,
                    ..
                } = events.recv().await.map_err(|_| Error::ChannelClosed)?
                {
                    if event_handle != handle || event_page != page {
                        continue;
                    }
                    if status == 0 {
                        return Ok(features);
                    }
                    return Err(Error::HciStatus {
                        operation: "Read Remote Extended Features",
                        status,
                    });
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("remote extended features"))?
    }

    pub async fn connect(&self, result: &InquiryResult) -> Result<u16> {
        self.connect_with_clock(result.addr, result.clock_offset | 0x8000)
            .await
    }

    pub async fn connect_addr(&self, addr: DeviceAddr) -> Result<u16> {
        self.connect_with_clock(addr, 0).await
    }

    async fn connect_with_clock(&self, addr: DeviceAddr, clock_offset: u16) -> Result<u16> {
        let mut events = self.events.subscribe();
        match self.cancel_inquiry().await {
            Ok(())
            | Err(Error::HciStatus {
                operation: "Inquiry Cancel",
                status: 0x0c,
            }) => {}
            Err(error) => return Err(error),
        }
        // DM1/DH1/DM3/DH3/DM5/DH5, with the mandatory reserved bits used by HCI.
        for attempt in 1..=3 {
            match self
                .execute_with_operation(
                    Command::create_connection(addr, 0xcc18, clock_offset),
                    "Create Connection",
                )
                .await
            {
                Ok(_) => break,
                Err(Error::HciStatus { status: 0x0c, .. }) if attempt < 3 => {
                    time::sleep(Duration::from_millis(250 * attempt)).await;
                }
                Err(error) => return Err(error),
            }
        }
        wait_connection_complete(&mut events, addr).await
    }

    /// Accepts an inbound ACL connection without requesting authentication.
    /// This is required for DualShock 3 and is also the reconnection path for
    /// Wii Remotes previously bonded to this adapter.
    pub async fn accept_connection(&self, addr: DeviceAddr) -> Result<u16> {
        self.accept_connection_with_role(addr, true).await
    }

    pub async fn accept_connection_with_role(
        &self,
        addr: DeviceAddr,
        become_master: bool,
    ) -> Result<u16> {
        let mut events = self.events.subscribe();
        self.execute(Command::accept_connection(addr, become_master))
            .await?;
        wait_connection_complete(&mut events, addr).await
    }

    pub async fn reject_connection(&self, addr: DeviceAddr) -> Result<()> {
        self.execute_checked(
            Command::reject_connection(addr, 0x0f),
            "Reject Connection Request",
        )
        .await?;
        Ok(())
    }

    pub async fn disconnect(&self, handle: u16) -> Result<()> {
        self.execute_checked(Command::disconnect(handle, 0x13), "Disconnect")
            .await?; // remote user terminated
        self.forget_handle(handle).await
    }

    /// Starts BR/EDR authentication and optionally enables link encryption.
    /// Do not call this for DualShock 3; its inbound connection intentionally
    /// skips standard authentication.
    pub async fn authenticate(&self, handle: u16, encrypt: bool) -> Result<()> {
        let mut events = self.events.subscribe();
        self.execute(Command::handle_only(
            Opcode::AUTHENTICATION_REQUESTED,
            handle,
        ))
        .await?;
        time::timeout(Duration::from_secs(30), async {
            loop {
                if let Event::AuthenticationComplete {
                    status,
                    handle: event_handle,
                } = events.recv().await.map_err(|_| Error::ChannelClosed)?
                {
                    if event_handle != handle {
                        continue;
                    }
                    if status == 0 {
                        break;
                    }
                    return Err(Error::HciStatus {
                        operation: "Authentication Requested",
                        status,
                    });
                }
            }
            if !encrypt {
                return Ok(());
            }
            self.execute(Command::set_connection_encryption(handle, true))
                .await?;
            loop {
                match events.recv().await.map_err(|_| Error::ChannelClosed)? {
                    Event::EncryptionChange {
                        status,
                        handle: event_handle,
                        enabled,
                    } if event_handle == handle => {
                        if status == 0 && enabled {
                            return Ok(());
                        }
                        return Err(Error::HciStatus {
                            operation: "Set Connection Encryption",
                            status,
                        });
                    }
                    // Enabling encryption on an already encrypted bonded link
                    // completes as a key refresh on Realtek controllers.
                    Event::EncryptionKeyRefreshComplete {
                        status,
                        handle: event_handle,
                    } if event_handle == handle => {
                        if status == 0 {
                            return Ok(());
                        }
                        return Err(Error::HciStatus {
                            operation: "Refresh Connection Encryption",
                            status,
                        });
                    }
                    _ => {}
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("BR/EDR authentication"))?
    }

    pub async fn execute(&self, command: Command) -> Result<Bytes> {
        self.execute_with_operation(command, "HCI command").await
    }

    async fn execute_with_operation(
        &self,
        command: Command,
        operation: &'static str,
    ) -> Result<Bytes> {
        let _guard = self.command_lock.lock().await;
        let mut events = self.events.subscribe();
        self.command.send(&command).await?;
        let opcode = command.opcode();
        time::timeout(COMMAND_TIMEOUT, async {
            loop {
                match events.recv().await.map_err(|_| Error::ChannelClosed)? {
                    Event::CommandComplete {
                        opcode: received,
                        return_parameters,
                        ..
                    } if received == opcode => {
                        return checked_return_parameters(operation, return_parameters);
                    }
                    Event::CommandStatus {
                        opcode: received,
                        status,
                        ..
                    } if received == opcode => {
                        if status == 0 {
                            return Ok(Bytes::from_static(&[0]));
                        }
                        return Err(Error::HciStatus { operation, status });
                    }
                    _ => {}
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("HCI command completion"))?
    }

    pub async fn execute_checked(
        &self,
        command: Command,
        operation: &'static str,
    ) -> Result<Bytes> {
        self.execute_with_operation(command, operation).await
    }

    pub async fn send_l2cap(&self, handle: u16, pdu: &[u8], flushable: bool) -> Result<()> {
        for packet in AclPacket::fragment(
            handle,
            pdu,
            self.info.acl_data_packet_length as usize,
            flushable,
        )? {
            let (tx, rx) = oneshot::channel();
            self.acl_worker
                .send(AclWorkerMessage::Send {
                    packet,
                    completion: tx,
                })
                .await
                .map_err(|_| Error::ChannelClosed)?;
            rx.await.map_err(|_| Error::ChannelClosed)??;
        }
        Ok(())
    }

    pub async fn forget_handle(&self, handle: u16) -> Result<()> {
        self.acl_worker
            .send(AclWorkerMessage::Forget(handle))
            .await
            .map_err(|_| Error::ChannelClosed)
    }
}

async fn wait_connection_complete(
    events: &mut broadcast::Receiver<Event>,
    expected: DeviceAddr,
) -> Result<u16> {
    time::timeout(Duration::from_secs(12), async {
        loop {
            if let Event::ConnectionComplete {
                status,
                handle,
                addr,
                ..
            } = events.recv().await.map_err(|_| Error::ChannelClosed)?
            {
                if addr != expected {
                    continue;
                }
                if status == 0 {
                    return Ok(handle);
                }
                return Err(Error::HciStatus {
                    operation: "Create/Accept Connection",
                    status,
                });
            }
        }
    })
    .await
    .map_err(|_| Error::Timeout("ACL connection"))?
}

impl Drop for HciHost {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn dispatch_events(
    mut event_rx: EventReceiver,
    mut acl_rx: AclReceiver,
    events: broadcast::Sender<Event>,
    l2cap_frames: broadcast::Sender<L2capFrame>,
    acl_worker: mpsc::Sender<AclWorkerMessage>,
) {
    let mut reassembler = AclReassembler::default();
    loop {
        tokio::select! {
            result = event_rx.receive() => match result {
                Ok(event) => {
                    let credit_worker_stopped = match &event {
                        Event::NumberOfCompletedPackets(completed) => acl_worker
                            .send(AclWorkerMessage::Completed(completed.clone()))
                            .await
                            .is_err(),
                        _ => false,
                    };
                    if credit_worker_stopped { break; }
                    if let Event::DisconnectionComplete { handle, .. } = &event {
                        reassembler.discard_handle(*handle);
                        let _ = acl_worker.send(AclWorkerMessage::Forget(*handle)).await;
                    }
                    let _ = events.send(event);
                }
                Err(error) => { tracing::error!(%error, "Bluetooth HCI event receiver stopped"); break; }
            },
            result = acl_rx.receive() => match result {
                Ok(packet) => {
                    let handle = packet.handle;
                    match reassembler.push(packet) {
                        Ok(Some(pdu)) => { let _ = l2cap_frames.send(L2capFrame { handle, pdu }); }
                        Ok(None) => {}
                        Err(error) => tracing::warn!(handle, %error, "dropping malformed ACL packet"),
                    }
                }
                Err(error) => { tracing::error!(%error, "Bluetooth ACL receiver stopped"); break; }
            },
        }
    }
}

async fn acl_worker(
    mut usb: AclSender,
    mut messages: mpsc::Receiver<AclWorkerMessage>,
    initial_capacity: u16,
) {
    let mut credits = AclCredits::new(initial_capacity);
    let mut queue: VecDeque<QueuedAcl> = VecDeque::new();
    while let Some(message) = messages.recv().await {
        match message {
            AclWorkerMessage::Send { packet, completion } => {
                queue.push_back(QueuedAcl { packet, completion })
            }
            AclWorkerMessage::Completed(entries) => {
                for (handle, count) in entries {
                    credits.complete(handle, count);
                }
            }
            AclWorkerMessage::Forget(handle) => {
                credits.forget_handle(handle);
            }
            AclWorkerMessage::SetCapacity(capacity) => credits.set_capacity(capacity),
        }
        while credits.available() != 0 {
            let Some(queued) = queue.pop_front() else {
                break;
            };
            credits.try_acquire(queued.packet.handle);
            let result = usb.send(&queued.packet).await;
            if result.is_err() {
                credits.complete(queued.packet.handle, 1);
            }
            let failed = result.is_err();
            let _ = queued.completion.send(result);
            if failed {
                break;
            }
        }
    }
    for queued in queue {
        let _ = queued.completion.send(Err(Error::ChannelClosed));
    }
}

fn ensure_status(operation: &'static str, response: &[u8]) -> Result<()> {
    let status = response
        .first()
        .copied()
        .ok_or(Error::InvalidPacket("HCI command returned no status"))?;
    if status == 0 {
        Ok(())
    } else {
        Err(Error::HciStatus { operation, status })
    }
}

fn checked_return_parameters(operation: &'static str, response: Bytes) -> Result<Bytes> {
    ensure_status(operation, &response)?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_complete_accepts_zero_status_and_preserves_payload() {
        let response = Bytes::from_static(&[0x00, 0x34, 0x12]);
        let checked = checked_return_parameters("Test command", response.clone()).unwrap();
        assert_eq!(checked, response);
    }

    #[test]
    fn command_complete_rejects_nonzero_status() {
        let error =
            checked_return_parameters("Refresh Encryption Key", Bytes::from_static(&[0x01]))
                .unwrap_err();
        assert!(matches!(
            error,
            Error::HciStatus {
                operation: "Refresh Encryption Key",
                status: 0x01
            }
        ));
    }

    #[test]
    fn command_complete_requires_a_status_byte() {
        let error = checked_return_parameters("Test command", Bytes::new()).unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidPacket("HCI command returned no status")
        ));
    }
}
