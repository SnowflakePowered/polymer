use bytes::Bytes;
use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    byteorder::little_endian::{U16, U32},
};

use crate::{DeviceAddr, Error, Result};

use super::Opcode;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct EventHeader {
    code: u8,
    length: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireAddress {
    address: [u8; 6],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct StatusAddress {
    status: u8,
    address: [u8; 6],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct StatusHandle {
    status: u8,
    handle: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ConnectionComplete {
    status: u8,
    handle: U16,
    address: [u8; 6],
    link_type: u8,
    encryption: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ConnectionRequest {
    address: [u8; 6],
    class_of_device: [u8; 3],
    link_type: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct DisconnectionComplete {
    status: u8,
    handle: U16,
    reason: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct EncryptionChange {
    status: u8,
    handle: U16,
    enabled: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SupportedFeatures {
    status: u8,
    handle: U16,
    features: [u8; 8],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RoleChange {
    status: u8,
    address: [u8; 6],
    role: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct CommandComplete {
    command_packets: u8,
    opcode: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct CommandStatus {
    status: u8,
    command_packets: u8,
    opcode: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct LinkKeyNotification {
    address: [u8; 6],
    key: [u8; 16],
    key_type: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ExtendedFeatures {
    status: u8,
    handle: U16,
    page: u8,
    max_page: u8,
    features: [u8; 8],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ExtendedInquiryResult {
    count: u8,
    address: [u8; 6],
    repetition_mode: u8,
    reserved: u8,
    class_of_device: [u8; 3],
    clock_offset: U16,
    rssi: i8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct UserConfirmationRequest {
    address: [u8; 6],
    numeric_value: U32,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct CompletedPackets {
    handle: U16,
    count: U16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionLinkType {
    Acl,
    Sco,
    Esco,
    Unknown(u8),
}

impl From<u8> for ConnectionLinkType {
    fn from(value: u8) -> Self {
        match value {
            0x01 => Self::Acl,
            0x00 => Self::Sco,
            0x02 => Self::Esco,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InquiryResult {
    pub addr: DeviceAddr,
    pub class_of_device: [u8; 3],
    pub clock_offset: u16,
    pub rssi: Option<i8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Event {
    InquiryComplete {
        status: u8,
    },
    InquiryResults(Vec<InquiryResult>),
    ConnectionComplete {
        status: u8,
        handle: u16,
        addr: DeviceAddr,
        link_type: ConnectionLinkType,
        encryption: bool,
    },
    ConnectionRequest {
        addr: DeviceAddr,
        class_of_device: [u8; 3],
        link_type: ConnectionLinkType,
    },
    DisconnectionComplete {
        status: u8,
        handle: u16,
        reason: u8,
    },
    AuthenticationComplete {
        status: u8,
        handle: u16,
    },
    RemoteSupportedFeaturesComplete {
        status: u8,
        handle: u16,
        features: [u8; 8],
    },
    RemoteExtendedFeaturesComplete {
        status: u8,
        handle: u16,
        page: u8,
        max_page: u8,
        features: [u8; 8],
    },
    EncryptionChange {
        status: u8,
        handle: u16,
        enabled: bool,
    },
    EncryptionKeyRefreshComplete {
        status: u8,
        handle: u16,
    },
    RoleChange {
        status: u8,
        addr: DeviceAddr,
        role: u8,
    },
    RemoteNameComplete {
        status: u8,
        addr: DeviceAddr,
        name: String,
    },
    CommandComplete {
        command_packets: u8,
        opcode: Opcode,
        return_parameters: Bytes,
    },
    CommandStatus {
        status: u8,
        command_packets: u8,
        opcode: Opcode,
    },
    NumberOfCompletedPackets(Vec<(u16, u16)>),
    PinCodeRequest {
        addr: DeviceAddr,
    },
    LinkKeyRequest {
        addr: DeviceAddr,
    },
    LinkKeyNotification {
        addr: DeviceAddr,
        key: [u8; 16],
        key_type: u8,
    },
    IoCapabilityRequest {
        addr: DeviceAddr,
    },
    UserConfirmationRequest {
        addr: DeviceAddr,
        numeric_value: u32,
    },
    SimplePairingComplete {
        status: u8,
        addr: DeviceAddr,
    },
    Unknown {
        code: u8,
        parameters: Bytes,
    },
}

impl Event {
    pub fn parse(packet: &[u8]) -> Result<Self> {
        let (header, remaining) = EventHeader::ref_from_prefix(packet)
            .map_err(|_| Error::InvalidPacket("short HCI event header"))?;
        let p = remaining
            .get(..usize::from(header.length))
            .ok_or(Error::InvalidPacket("truncated HCI event"))?;
        match header.code {
            0x01 => Ok(Self::InquiryComplete {
                status: *p
                    .first()
                    .ok_or(Error::InvalidPacket("short HCI event parameters"))?,
            }),
            0x02 => parse_inquiry_results(p, false),
            0x03 => {
                let value = wire::<ConnectionComplete>(p)?;
                Ok(Self::ConnectionComplete {
                    status: value.status,
                    handle: value.handle.get() & 0x0fff,
                    addr: DeviceAddr::from_hci_bytes(value.address),
                    link_type: value.link_type.into(),
                    encryption: value.encryption != 0,
                })
            }
            0x04 => {
                let value = wire::<ConnectionRequest>(p)?;
                Ok(Self::ConnectionRequest {
                    addr: DeviceAddr::from_hci_bytes(value.address),
                    class_of_device: value.class_of_device,
                    link_type: value.link_type.into(),
                })
            }
            0x05 => {
                let value = wire::<DisconnectionComplete>(p)?;
                Ok(Self::DisconnectionComplete {
                    status: value.status,
                    handle: value.handle.get() & 0x0fff,
                    reason: value.reason,
                })
            }
            0x06 => {
                let value = wire::<StatusHandle>(p)?;
                Ok(Self::AuthenticationComplete {
                    status: value.status,
                    handle: value.handle.get() & 0x0fff,
                })
            }
            0x07 => {
                let (value, name_bytes) = wire_prefix::<StatusAddress>(p)?;
                let end = name_bytes
                    .iter()
                    .position(|b| *b == 0)
                    .unwrap_or(name_bytes.len());
                Ok(Self::RemoteNameComplete {
                    status: value.status,
                    addr: DeviceAddr::from_hci_bytes(value.address),
                    name: String::from_utf8_lossy(&name_bytes[..end]).into_owned(),
                })
            }
            0x08 => {
                let value = wire::<EncryptionChange>(p)?;
                Ok(Self::EncryptionChange {
                    status: value.status,
                    handle: value.handle.get() & 0x0fff,
                    enabled: value.enabled != 0,
                })
            }
            0x0b => {
                let value = wire::<SupportedFeatures>(p)?;
                Ok(Self::RemoteSupportedFeaturesComplete {
                    status: value.status,
                    handle: value.handle.get() & 0x0fff,
                    features: value.features,
                })
            }
            0x12 => {
                let value = wire::<RoleChange>(p)?;
                Ok(Self::RoleChange {
                    status: value.status,
                    addr: DeviceAddr::from_hci_bytes(value.address),
                    role: value.role,
                })
            }
            0x0e => {
                let (value, return_parameters) = wire_prefix::<CommandComplete>(p)?;
                Ok(Self::CommandComplete {
                    command_packets: value.command_packets,
                    opcode: Opcode(value.opcode.get()),
                    return_parameters: Bytes::copy_from_slice(return_parameters),
                })
            }
            0x0f => {
                let value = wire::<CommandStatus>(p)?;
                Ok(Self::CommandStatus {
                    status: value.status,
                    command_packets: value.command_packets,
                    opcode: Opcode(value.opcode.get()),
                })
            }
            0x13 => parse_completed_packets(p),
            0x16 => Ok(Self::PinCodeRequest {
                addr: DeviceAddr::from_hci_bytes(wire::<WireAddress>(p)?.address),
            }),
            0x17 => Ok(Self::LinkKeyRequest {
                addr: DeviceAddr::from_hci_bytes(wire::<WireAddress>(p)?.address),
            }),
            0x18 => {
                let value = wire::<LinkKeyNotification>(p)?;
                Ok(Self::LinkKeyNotification {
                    addr: DeviceAddr::from_hci_bytes(value.address),
                    key: value.key,
                    key_type: value.key_type,
                })
            }
            0x22 => parse_inquiry_results(p, true),
            0x23 => {
                let value = wire::<ExtendedFeatures>(p)?;
                Ok(Self::RemoteExtendedFeaturesComplete {
                    status: value.status,
                    handle: value.handle.get() & 0x0fff,
                    page: value.page,
                    max_page: value.max_page,
                    features: value.features,
                })
            }
            0x2f => parse_extended_inquiry_result(p),
            0x30 => {
                let value = wire::<StatusHandle>(p)?;
                Ok(Self::EncryptionKeyRefreshComplete {
                    status: value.status,
                    handle: value.handle.get() & 0x0fff,
                })
            }
            0x31 => Ok(Self::IoCapabilityRequest {
                addr: DeviceAddr::from_hci_bytes(wire::<WireAddress>(p)?.address),
            }),
            0x33 => {
                let value = wire::<UserConfirmationRequest>(p)?;
                Ok(Self::UserConfirmationRequest {
                    addr: DeviceAddr::from_hci_bytes(value.address),
                    numeric_value: value.numeric_value.get(),
                })
            }
            0x36 => {
                let value = wire::<StatusAddress>(p)?;
                Ok(Self::SimplePairingComplete {
                    status: value.status,
                    addr: DeviceAddr::from_hci_bytes(value.address),
                })
            }
            _ => Ok(Self::Unknown {
                code: header.code,
                parameters: Bytes::copy_from_slice(p),
            }),
        }
    }
}

fn parse_inquiry_results(p: &[u8], with_rssi: bool) -> Result<Event> {
    let count = usize::from(
        *p.first()
            .ok_or(Error::InvalidPacket("short HCI event parameters"))?,
    );
    let record_len = if with_rssi { 14 } else { 13 };
    require(p, 1 + count * record_len)?;
    let mut results = Vec::with_capacity(count);
    let addr_at = 1;
    let repetition_at = addr_at + count * 6;
    let reserved_at = repetition_at + count;
    let class_at = reserved_at + count;
    let clock_at = class_at + count * 3;
    let rssi_at = clock_at + count * 2;
    for index in 0..count {
        results.push(InquiryResult {
            addr: addr(p, addr_at + index * 6)?,
            class_of_device: p[class_at + index * 3..class_at + index * 3 + 3]
                .try_into()
                .unwrap(),
            clock_offset: le16_at(p, clock_at + index * 2)?,
            rssi: with_rssi.then(|| p[rssi_at + index] as i8),
        });
    }
    Ok(Event::InquiryResults(results))
}

fn parse_extended_inquiry_result(p: &[u8]) -> Result<Event> {
    let value = wire::<ExtendedInquiryResult>(p)?;
    if value.count == 0 {
        return Ok(Event::InquiryResults(Vec::new()));
    }
    Ok(Event::InquiryResults(vec![InquiryResult {
        addr: DeviceAddr::from_hci_bytes(value.address),
        class_of_device: value.class_of_device,
        clock_offset: value.clock_offset.get(),
        rssi: Some(value.rssi),
    }]))
}

fn parse_completed_packets(p: &[u8]) -> Result<Event> {
    let count = usize::from(
        *p.first()
            .ok_or(Error::InvalidPacket("short HCI event parameters"))?,
    );
    require(p, 1 + count * 4)?;
    let completed = p[1..]
        .chunks_exact(4)
        .take(count)
        .map(|v| {
            let value = CompletedPackets::ref_from_bytes(v).expect("four-byte record");
            (value.handle.get() & 0x0fff, value.count.get())
        })
        .collect();
    Ok(Event::NumberOfCompletedPackets(completed))
}

fn require(p: &[u8], len: usize) -> Result<()> {
    if p.len() < len {
        Err(Error::InvalidPacket("short HCI event parameters"))
    } else {
        Ok(())
    }
}
fn wire<T>(p: &[u8]) -> Result<&T>
where
    T: FromBytes + KnownLayout + Immutable + Unaligned,
{
    wire_prefix::<T>(p).map(|(value, _)| value)
}

fn wire_prefix<T>(p: &[u8]) -> Result<(&T, &[u8])>
where
    T: FromBytes + KnownLayout + Immutable + Unaligned,
{
    T::ref_from_prefix(p).map_err(|_| Error::InvalidPacket("short HCI event parameters"))
}

fn le16_at(p: &[u8], at: usize) -> Result<u16> {
    let bytes = p
        .get(at..)
        .ok_or(Error::InvalidPacket("short HCI event parameters"))?;
    Ok(wire::<U16>(bytes)?.get())
}
fn addr(p: &[u8], at: usize) -> Result<DeviceAddr> {
    let bytes = p
        .get(at..)
        .ok_or(Error::InvalidPacket("short HCI event parameters"))?;
    Ok(DeviceAddr::from_hci_bytes(
        wire::<WireAddress>(bytes)?.address,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_completed_packet_credits() {
        let event = Event::parse(&[0x13, 9, 2, 0x01, 0x00, 3, 0, 0x02, 0x00, 4, 0]).unwrap();
        assert_eq!(event, Event::NumberOfCompletedPackets(vec![(1, 3), (2, 4)]));
    }

    #[test]
    fn rejects_truncated_event() {
        assert!(Event::parse(&[0x03, 11, 0]).is_err());
    }

    #[test]
    fn parses_encryption_key_refresh() {
        assert_eq!(
            Event::parse(&[0x30, 3, 0, 0x34, 0x02]).unwrap(),
            Event::EncryptionKeyRefreshComplete {
                status: 0,
                handle: 0x0234,
            }
        );
    }

    #[test]
    fn parses_columnar_inquiry_result() {
        let packet = [
            0x22, 29, 2, 6, 5, 4, 3, 2, 1, 12, 11, 10, 9, 8, 7, 1, 1, 0, 0, 0x04, 0x05, 0x00, 0x08,
            0x25, 0x00, 0x34, 0x12, 0x78, 0x56, 0xd8, 0xce,
        ];
        let Event::InquiryResults(results) = Event::parse(&packet).unwrap() else {
            panic!()
        };
        assert_eq!(results[0].addr, DeviceAddr::public([1, 2, 3, 4, 5, 6]));
        assert_eq!(results[1].class_of_device, [0x08, 0x25, 0]);
        assert_eq!(results[1].rssi, Some(-50));
    }
}
