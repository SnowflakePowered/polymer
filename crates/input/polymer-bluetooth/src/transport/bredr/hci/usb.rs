use std::{ffi::OsString, sync::Arc, time::Duration};

use bytes::Bytes;

use nusb::{
    Device, Interface,
    descriptors::TransferType,
    transfer::{Buffer, Bulk, ControlOut, ControlType, Direction, In, Interrupt, Out, Recipient},
};

use crate::{Error, Result};

use super::{
    AclPacket, Command, Event,
    trace::{self, Direction as TraceDirection, Recorder},
};

const BLUETOOTH_CLASS: u8 = 0xe0;
const BLUETOOTH_SUBCLASS: u8 = 0x01;
const BLUETOOTH_PROTOCOL: u8 = 0x01;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const RECEIVE_QUEUE_DEPTH: usize = 4;
// An HCI event has a two-byte header and an eight-bit parameter length. USB
// interrupt endpoints are often only 16 or 64 bytes wide, but one transfer can
// span multiple packets. Extended Inquiry Result uses the full 255-byte body.
const MAX_HCI_EVENT_SIZE: usize = 2 + u8::MAX as usize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterSelector {
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
    /// On Windows, only consider devices explicitly assigned to WinUSB.
    pub require_winusb: bool,
}

impl Default for AdapterSelector {
    fn default() -> Self {
        Self::first_winusb()
    }
}

impl AdapterSelector {
    pub const fn from_vid_pid(vendor_id: u16, product_id: u16) -> Self {
        Self {
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
            serial_number: None,
            require_winusb: true,
        }
    }

    pub const fn first_winusb() -> Self {
        Self {
            vendor_id: None,
            product_id: None,
            serial_number: None,
            require_winusb: true,
        }
    }

    fn matches(&self, info: &nusb::DeviceInfo) -> bool {
        self.vendor_id.is_none_or(|id| info.vendor_id() == id)
            && self.product_id.is_none_or(|id| info.product_id() == id)
            && self
                .serial_number
                .as_deref()
                .is_none_or(|serial| info.serial_number() == Some(serial))
            && self.driver_matches(info)
    }

    #[cfg(target_os = "windows")]
    fn driver_matches(&self, info: &nusb::DeviceInfo) -> bool {
        !self.require_winusb
            || info
                .driver()
                .is_some_and(|driver| driver.eq_ignore_ascii_case("winusb"))
    }

    #[cfg(not(target_os = "windows"))]
    fn driver_matches(&self, _info: &nusb::DeviceInfo) -> bool {
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub product: Option<String>,
    pub serial_number: Option<String>,
    #[cfg(target_os = "windows")]
    pub instance_id: OsString,
    #[cfg(target_os = "windows")]
    pub driver: Option<String>,
}

impl AdapterInfo {
    fn from_device(info: &nusb::DeviceInfo) -> Self {
        Self {
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            product: info.product_string().map(str::to_owned),
            serial_number: info.serial_number().map(str::to_owned),
            #[cfg(target_os = "windows")]
            instance_id: info.instance_id().to_owned(),
            #[cfg(target_os = "windows")]
            driver: info.driver().map(str::to_owned),
        }
    }
}

/// Exclusive USB H2 connection to a Bluetooth controller.
///
/// On Windows the selected device must be bound to WinUSB. A device bound to
/// `libusbK.sys` is intentionally not opened by `nusb`.
pub struct UsbHci {
    info: AdapterInfo,
    interface: Interface,
    claimed_sco_interfaces: Vec<Interface>,
    event_endpoint: u8,
    acl_in_endpoint: u8,
    acl_out_endpoint: u8,
}

impl UsbHci {
    /// Lists USB Bluetooth controllers visible to `nusb`. On Windows this may
    /// include an OS-owned composite controller whose HCI interface cannot be
    /// claimed; explicit selection prevents accidentally taking that radio.
    pub async fn adapters() -> Result<Vec<AdapterInfo>> {
        Ok(nusb::list_devices()
            .await?
            .filter(is_bluetooth_device)
            .map(|info| AdapterInfo::from_device(&info))
            .collect())
    }

    pub async fn open(selector: &AdapterSelector) -> Result<Self> {
        let info = nusb::list_devices()
            .await?
            .find(|info| selector.matches(info) && is_bluetooth_device(info))
            .ok_or(Error::AdapterNotFound)?;
        let adapter_info = AdapterInfo::from_device(&info);
        let device = info.open().await?;
        Self::open_device(device, adapter_info).await
    }

    async fn open_device(device: Device, info: AdapterInfo) -> Result<Self> {
        let configuration = device.active_configuration()?;
        let mut hci_endpoints = None;
        'interfaces: for descriptors in configuration.interfaces() {
            for descriptor in descriptors.alt_settings() {
                if is_hci_interface(
                    descriptor.class(),
                    descriptor.subclass(),
                    descriptor.protocol(),
                ) {
                    let mut event = None;
                    let mut acl_in = None;
                    let mut acl_out = None;
                    for endpoint in descriptor.endpoints() {
                        match (endpoint.transfer_type(), endpoint.direction()) {
                            (TransferType::Interrupt, Direction::In) => {
                                event = Some(endpoint.address())
                            }
                            (TransferType::Bulk, Direction::In) => {
                                acl_in = Some(endpoint.address())
                            }
                            (TransferType::Bulk, Direction::Out) => {
                                acl_out = Some(endpoint.address())
                            }
                            _ => {}
                        }
                    }
                    hci_endpoints = Some((descriptor.interface_number(), event, acl_in, acl_out));
                    break 'interfaces;
                }
            }
        }
        let (interface_number, event_endpoint, acl_in_endpoint, acl_out_endpoint) = hci_endpoints
            .ok_or(
            Error::Unsupported("USB device has no Bluetooth HCI interface"),
        )?;
        let event_endpoint = event_endpoint.ok_or(Error::Unsupported(
            "Bluetooth interface has no event endpoint",
        ))?;
        let acl_in_endpoint = acl_in_endpoint.ok_or(Error::Unsupported(
            "Bluetooth interface has no ACL IN endpoint",
        ))?;
        let acl_out_endpoint = acl_out_endpoint.ok_or(Error::Unsupported(
            "Bluetooth interface has no ACL OUT endpoint",
        ))?;

        let interface = device.claim_interface(interface_number).await?;
        if interface.get_alt_setting() != 0 {
            interface.set_alt_setting(0).await?;
        }

        // Bluetooth USB controllers conventionally expose SCO on a second
        // interface with isochronous alternate settings. Retaining alt 0 keeps
        // it disabled and prevents reserving bandwidth for audio we never use.
        let sco_numbers: Vec<u8> = configuration
            .interfaces()
            .filter(|descriptors| descriptors.interface_number() != interface_number)
            .filter(|descriptors| {
                descriptors.alt_settings().any(|descriptor| {
                    descriptor
                        .endpoints()
                        .any(|ep| ep.transfer_type() == TransferType::Isochronous)
                })
            })
            .map(|descriptors| descriptors.interface_number())
            .collect();
        let mut claimed_sco_interfaces = Vec::new();
        for number in sco_numbers {
            match device.claim_interface(number).await {
                Ok(sco) => {
                    if sco.get_alt_setting() != 0 {
                        sco.set_alt_setting(0).await?;
                    }
                    claimed_sco_interfaces.push(sco);
                }
                Err(error) => {
                    tracing::debug!(interface = number, %error, "could not claim optional Bluetooth SCO interface")
                }
            }
        }

        Ok(Self {
            info,
            interface,
            claimed_sco_interfaces,
            event_endpoint,
            acl_in_endpoint,
            acl_out_endpoint,
        })
    }

    pub fn info(&self) -> &AdapterInfo {
        &self.info
    }
    pub fn sco_interface_count(&self) -> usize {
        self.claimed_sco_interfaces.len()
    }

    pub fn split(self) -> Result<(CommandSender, EventReceiver, AclSender, AclReceiver)> {
        let event = self
            .interface
            .endpoint::<Interrupt, In>(self.event_endpoint)?;
        let acl_in = self.interface.endpoint::<Bulk, In>(self.acl_in_endpoint)?;
        let acl_out = self
            .interface
            .endpoint::<Bulk, Out>(self.acl_out_endpoint)?;
        let command = CommandSender {
            interface: self.interface.clone(),
            _sco_interfaces: Arc::new(self.claimed_sco_interfaces),
        };
        Ok((
            command,
            EventReceiver::new(event),
            AclSender { endpoint: acl_out },
            AclReceiver::new(acl_in),
        ))
    }
}

fn is_bluetooth_device(info: &nusb::DeviceInfo) -> bool {
    is_hci_interface(info.class(), info.subclass(), info.protocol())
        || info
            .interfaces()
            .any(|i| is_hci_interface(i.class(), i.subclass(), i.protocol()))
}

fn is_hci_interface(class: u8, subclass: u8, protocol: u8) -> bool {
    class == BLUETOOTH_CLASS && subclass == BLUETOOTH_SUBCLASS && protocol == BLUETOOTH_PROTOCOL
}

#[derive(Clone)]
pub struct CommandSender {
    interface: Interface,
    _sco_interfaces: Arc<Vec<Interface>>,
}

impl CommandSender {
    pub async fn send(&self, command: &Command) -> Result<()> {
        let packet = command.encode();
        if let Some(recorder) = Recorder::global() {
            recorder.record(TraceDirection::HostToController, trace::COMMAND, &packet);
        }
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Device,
                    request: 0,
                    value: 0,
                    index: 0,
                    data: &packet,
                },
                COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }
}

pub struct EventReceiver {
    endpoint: nusb::Endpoint<Interrupt, In>,
}

impl EventReceiver {
    fn new(mut endpoint: nusb::Endpoint<Interrupt, In>) -> Self {
        let packet_size = endpoint.max_packet_size();
        let transfer_len = MAX_HCI_EVENT_SIZE.next_multiple_of(packet_size);
        for _ in 0..RECEIVE_QUEUE_DEPTH {
            endpoint.submit(Buffer::new(transfer_len));
        }
        Self { endpoint }
    }

    pub async fn receive(&mut self) -> Result<Event> {
        let completion = self.endpoint.next_complete().await;
        completion.status?;
        let bytes = Bytes::copy_from_slice(&completion.buffer);
        self.endpoint.submit(completion.buffer);
        if let Some(recorder) = Recorder::global() {
            recorder.record(TraceDirection::ControllerToHost, trace::EVENT, &bytes);
        }
        Event::parse(&bytes)
    }
}

pub struct AclReceiver {
    endpoint: nusb::Endpoint<Bulk, In>,
}

impl AclReceiver {
    fn new(mut endpoint: nusb::Endpoint<Bulk, In>) -> Self {
        let transfer_len = endpoint
            .max_packet_size()
            .max(1024)
            .next_multiple_of(endpoint.max_packet_size());
        for _ in 0..RECEIVE_QUEUE_DEPTH {
            endpoint.submit(Buffer::new(transfer_len));
        }
        Self { endpoint }
    }

    pub async fn receive(&mut self) -> Result<AclPacket> {
        let completion = self.endpoint.next_complete().await;
        completion.status?;
        let bytes = Bytes::copy_from_slice(&completion.buffer);
        self.endpoint.submit(completion.buffer);
        if let Some(recorder) = Recorder::global() {
            recorder.record(TraceDirection::ControllerToHost, trace::ACL, &bytes);
        }
        AclPacket::parse(&bytes)
    }
}

pub struct AclSender {
    endpoint: nusb::Endpoint<Bulk, Out>,
}

impl AclSender {
    pub async fn send(&mut self, packet: &AclPacket) -> Result<()> {
        let encoded = packet.encode();
        if let Some(recorder) = Recorder::global() {
            recorder.record(TraceDirection::HostToController, trace::ACL, &encoded);
        }
        self.endpoint.submit(Buffer::from(&encoded[..]));
        let completion = self.endpoint.next_complete().await;
        completion.status?;
        Ok(())
    }
}
