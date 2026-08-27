use bytes::Bytes;

use crate::{DeviceAddr, Error, Result};

use super::Opcode;

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
        if packet.len() < 2 {
            return Err(Error::InvalidPacket("short HCI event header"));
        }
        let code = packet[0];
        let len = packet[1] as usize;
        if packet.len() < len + 2 {
            return Err(Error::InvalidPacket("truncated HCI event"));
        }
        let p = &packet[2..2 + len];
        match code {
            0x01 => Ok(Self::InquiryComplete { status: one(p, 0)? }),
            0x02 => parse_inquiry_results(p, false),
            0x03 => {
                require(p, 11)?;
                Ok(Self::ConnectionComplete {
                    status: p[0],
                    handle: le16(p, 1)? & 0x0fff,
                    addr: addr(p, 3)?,
                    link_type: p[9].into(),
                    encryption: p[10] != 0,
                })
            }
            0x04 => {
                require(p, 10)?;
                Ok(Self::ConnectionRequest {
                    addr: addr(p, 0)?,
                    class_of_device: p[6..9].try_into().unwrap(),
                    link_type: p[9].into(),
                })
            }
            0x05 => {
                require(p, 4)?;
                Ok(Self::DisconnectionComplete {
                    status: p[0],
                    handle: le16(p, 1)? & 0x0fff,
                    reason: p[3],
                })
            }
            0x06 => {
                require(p, 3)?;
                Ok(Self::AuthenticationComplete {
                    status: p[0],
                    handle: le16(p, 1)? & 0x0fff,
                })
            }
            0x07 => {
                require(p, 7)?;
                let name_bytes = &p[7..];
                let end = name_bytes
                    .iter()
                    .position(|b| *b == 0)
                    .unwrap_or(name_bytes.len());
                Ok(Self::RemoteNameComplete {
                    status: p[0],
                    addr: addr(p, 1)?,
                    name: String::from_utf8_lossy(&name_bytes[..end]).into_owned(),
                })
            }
            0x08 => {
                require(p, 4)?;
                Ok(Self::EncryptionChange {
                    status: p[0],
                    handle: le16(p, 1)? & 0x0fff,
                    enabled: p[3] != 0,
                })
            }
            0x0b => {
                require(p, 11)?;
                Ok(Self::RemoteSupportedFeaturesComplete {
                    status: p[0],
                    handle: le16(p, 1)? & 0x0fff,
                    features: p[3..11].try_into().unwrap(),
                })
            }
            0x12 => {
                require(p, 8)?;
                Ok(Self::RoleChange {
                    status: p[0],
                    addr: addr(p, 1)?,
                    role: p[7],
                })
            }
            0x0e => {
                require(p, 3)?;
                Ok(Self::CommandComplete {
                    command_packets: p[0],
                    opcode: Opcode(le16(p, 1)?),
                    return_parameters: Bytes::copy_from_slice(&p[3..]),
                })
            }
            0x0f => {
                require(p, 4)?;
                Ok(Self::CommandStatus {
                    status: p[0],
                    command_packets: p[1],
                    opcode: Opcode(le16(p, 2)?),
                })
            }
            0x13 => parse_completed_packets(p),
            0x16 => Ok(Self::PinCodeRequest { addr: addr(p, 0)? }),
            0x17 => Ok(Self::LinkKeyRequest { addr: addr(p, 0)? }),
            0x18 => {
                require(p, 23)?;
                Ok(Self::LinkKeyNotification {
                    addr: addr(p, 0)?,
                    key: p[6..22].try_into().unwrap(),
                    key_type: p[22],
                })
            }
            0x22 => parse_inquiry_results(p, true),
            0x23 => {
                require(p, 13)?;
                Ok(Self::RemoteExtendedFeaturesComplete {
                    status: p[0],
                    handle: le16(p, 1)? & 0x0fff,
                    page: p[3],
                    max_page: p[4],
                    features: p[5..13].try_into().unwrap(),
                })
            }
            0x2f => parse_extended_inquiry_result(p),
            0x30 => {
                require(p, 3)?;
                Ok(Self::EncryptionKeyRefreshComplete {
                    status: p[0],
                    handle: le16(p, 1)? & 0x0fff,
                })
            }
            0x31 => Ok(Self::IoCapabilityRequest { addr: addr(p, 0)? }),
            0x33 => {
                require(p, 10)?;
                Ok(Self::UserConfirmationRequest {
                    addr: addr(p, 0)?,
                    numeric_value: le32(p, 6)?,
                })
            }
            0x36 => {
                require(p, 7)?;
                Ok(Self::SimplePairingComplete {
                    status: p[0],
                    addr: addr(p, 1)?,
                })
            }
            _ => Ok(Self::Unknown {
                code,
                parameters: Bytes::copy_from_slice(p),
            }),
        }
    }
}

fn parse_inquiry_results(p: &[u8], with_rssi: bool) -> Result<Event> {
    let count = one(p, 0)? as usize;
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
            clock_offset: le16(p, clock_at + index * 2)?,
            rssi: with_rssi.then(|| p[rssi_at + index] as i8),
        });
    }
    Ok(Event::InquiryResults(results))
}

fn parse_extended_inquiry_result(p: &[u8]) -> Result<Event> {
    require(p, 15)?;
    if p[0] == 0 {
        return Ok(Event::InquiryResults(Vec::new()));
    }
    Ok(Event::InquiryResults(vec![InquiryResult {
        addr: addr(p, 1)?,
        class_of_device: p[9..12].try_into().unwrap(),
        clock_offset: le16(p, 12)?,
        rssi: Some(p[14] as i8),
    }]))
}

fn parse_completed_packets(p: &[u8]) -> Result<Event> {
    let count = one(p, 0)? as usize;
    require(p, 1 + count * 4)?;
    let completed = p[1..]
        .chunks_exact(4)
        .take(count)
        .map(|v| {
            (
                u16::from_le_bytes([v[0], v[1]]) & 0x0fff,
                u16::from_le_bytes([v[2], v[3]]),
            )
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
fn one(p: &[u8], at: usize) -> Result<u8> {
    p.get(at)
        .copied()
        .ok_or(Error::InvalidPacket("short HCI event parameters"))
}
fn le16(p: &[u8], at: usize) -> Result<u16> {
    require(p, at + 2)?;
    Ok(u16::from_le_bytes([p[at], p[at + 1]]))
}
fn le32(p: &[u8], at: usize) -> Result<u32> {
    require(p, at + 4)?;
    Ok(u32::from_le_bytes(p[at..at + 4].try_into().unwrap()))
}
fn addr(p: &[u8], at: usize) -> Result<DeviceAddr> {
    require(p, at + 6)?;
    Ok(DeviceAddr::from_hci_bytes(
        p[at..at + 6].try_into().unwrap(),
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
