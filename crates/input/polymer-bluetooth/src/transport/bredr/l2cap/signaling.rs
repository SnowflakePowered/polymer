use bytes::{BufMut, Bytes, BytesMut};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{U16, U32},
};

use crate::{Error, Result};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SignalHeader {
    code: u8,
    identifier: u8,
    length: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Pair {
    first: U16,
    second: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ConnectionResponse {
    destination_cid: U16,
    source_cid: U16,
    result: U16,
    status: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ConfigurationResponse {
    source_cid: U16,
    flags: U16,
    result: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct OptionHeader {
    kind: u8,
    length: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationOption {
    Mtu(u16),
    FlushTimeout(u16),
    QualityOfService(QualityOfService),
    Unknown { kind: u8, hint: bool, value: Bytes },
}

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct QualityOfService {
    pub flags: u8,
    pub service_type: u8,
    pub token_rate: U32,
    pub token_bucket_size: U32,
    pub peak_bandwidth: U32,
    pub latency: U32,
    pub delay_variation: U32,
}

impl ConfigurationOption {
    pub(super) fn negotiate(options: Vec<Self>) -> (u16, Vec<Self>) {
        let mtu = options
            .iter()
            .find_map(|option| match option {
                Self::Mtu(value) => Some(*value),
                _ => None,
            })
            .unwrap_or(super::DEFAULT_MTU);
        // Like Linux l2cap_parse_conf_req, recognize legacy QoS without
        // reserving bandwidth. DS3 requires acceptance of option 0x03 on its
        // interrupt channel even though basic-mode traffic is best effort.
        let unknown: Vec<_> = options
            .into_iter()
            .filter(|option| matches!(option, Self::Unknown { hint: false, .. }))
            .collect();
        if !unknown.is_empty() {
            return (3, unknown);
        }
        (0, vec![Self::Mtu(mtu)])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Signal {
    CommandReject {
        identifier: u8,
        reason: u16,
        data: Bytes,
    },
    ConnectionRequest {
        identifier: u8,
        psm: u16,
        source_cid: u16,
    },
    ConnectionResponse {
        identifier: u8,
        destination_cid: u16,
        source_cid: u16,
        result: u16,
        status: u16,
    },
    ConfigurationRequest {
        identifier: u8,
        destination_cid: u16,
        flags: u16,
        options: Vec<ConfigurationOption>,
    },
    ConfigurationResponse {
        identifier: u8,
        source_cid: u16,
        flags: u16,
        result: u16,
        options: Vec<ConfigurationOption>,
    },
    DisconnectionRequest {
        identifier: u8,
        destination_cid: u16,
        source_cid: u16,
    },
    DisconnectionResponse {
        identifier: u8,
        destination_cid: u16,
        source_cid: u16,
    },
    EchoRequest {
        identifier: u8,
        data: Bytes,
    },
    EchoResponse {
        identifier: u8,
        data: Bytes,
    },
    InformationRequest {
        identifier: u8,
        info_type: u16,
    },
    InformationResponse {
        identifier: u8,
        info_type: u16,
        result: u16,
        data: Bytes,
    },
    Unknown {
        code: u8,
        identifier: u8,
        data: Bytes,
    },
}

impl Signal {
    pub fn parse_all(mut payload: &[u8]) -> Result<Vec<Self>> {
        let mut signals = Vec::new();
        while !payload.is_empty() {
            let (header, remaining) = SignalHeader::ref_from_prefix(payload)
                .map_err(|_| Error::InvalidPacket("short L2CAP signaling header"))?;
            if header.identifier == 0 {
                return Err(Error::InvalidPacket("zero L2CAP signaling identifier"));
            }
            let length = usize::from(header.length.get());
            let data = remaining
                .get(..length)
                .ok_or(Error::InvalidPacket("truncated L2CAP signal"))?;
            signals.push(Self::parse_one(header.code, header.identifier, data)?);
            payload = &remaining[length..];
        }
        Ok(signals)
    }

    fn parse_one(code: u8, identifier: u8, data: &[u8]) -> Result<Self> {
        let pair = || -> Result<(u16, u16)> {
            let (pair, _) = wire::<Pair>(data)?;
            Ok((pair.first.get(), pair.second.get()))
        };
        Ok(match code {
            0x01 => {
                let (reason, remaining) = wire::<U16>(data)?;
                Self::CommandReject {
                    identifier,
                    reason: reason.get(),
                    data: Bytes::copy_from_slice(remaining),
                }
            }
            0x02 => {
                let (psm, source_cid) = pair()?;
                Self::ConnectionRequest {
                    identifier,
                    psm,
                    source_cid,
                }
            }
            0x03 => {
                let (response, _) = wire::<ConnectionResponse>(data)?;
                Self::ConnectionResponse {
                    identifier,
                    destination_cid: response.destination_cid.get(),
                    source_cid: response.source_cid.get(),
                    result: response.result.get(),
                    status: response.status.get(),
                }
            }
            0x04 => {
                let (request, options) = wire::<Pair>(data)?;
                Self::ConfigurationRequest {
                    identifier,
                    destination_cid: request.first.get(),
                    flags: request.second.get(),
                    options: parse_options(options)?,
                }
            }
            0x05 => {
                let (response, options) = wire::<ConfigurationResponse>(data)?;
                Self::ConfigurationResponse {
                    identifier,
                    source_cid: response.source_cid.get(),
                    flags: response.flags.get(),
                    result: response.result.get(),
                    options: parse_options(options)?,
                }
            }
            0x06 => {
                let (destination_cid, source_cid) = pair()?;
                Self::DisconnectionRequest {
                    identifier,
                    destination_cid,
                    source_cid,
                }
            }
            0x07 => {
                let (destination_cid, source_cid) = pair()?;
                Self::DisconnectionResponse {
                    identifier,
                    destination_cid,
                    source_cid,
                }
            }
            0x08 => Self::EchoRequest {
                identifier,
                data: Bytes::copy_from_slice(data),
            },
            0x09 => Self::EchoResponse {
                identifier,
                data: Bytes::copy_from_slice(data),
            },
            0x0a => {
                let (info_type, _) = wire::<U16>(data)?;
                Self::InformationRequest {
                    identifier,
                    info_type: info_type.get(),
                }
            }
            0x0b => {
                let (response, remaining) = wire::<Pair>(data)?;
                Self::InformationResponse {
                    identifier,
                    info_type: response.first.get(),
                    result: response.second.get(),
                    data: Bytes::copy_from_slice(remaining),
                }
            }
            _ => Self::Unknown {
                code,
                identifier,
                data: Bytes::copy_from_slice(data),
            },
        })
    }

    pub fn encode(&self) -> Bytes {
        let (code, identifier, data) = match self {
            Self::CommandReject {
                identifier,
                reason,
                data,
            } => {
                let mut p = BytesMut::with_capacity(2 + data.len());
                p.put_u16_le(*reason);
                p.extend_from_slice(data);
                (0x01, *identifier, p)
            }
            Self::ConnectionRequest {
                identifier,
                psm,
                source_cid,
            } => (0x02, *identifier, pair(*psm, *source_cid)),
            Self::ConnectionResponse {
                identifier,
                destination_cid,
                source_cid,
                result,
                status,
            } => {
                let mut p = BytesMut::with_capacity(8);
                p.put_u16_le(*destination_cid);
                p.put_u16_le(*source_cid);
                p.put_u16_le(*result);
                p.put_u16_le(*status);
                (0x03, *identifier, p)
            }
            Self::ConfigurationRequest {
                identifier,
                destination_cid,
                flags,
                options,
            } => {
                let mut p = pair(*destination_cid, *flags);
                encode_options(&mut p, options);
                (0x04, *identifier, p)
            }
            Self::ConfigurationResponse {
                identifier,
                source_cid,
                flags,
                result,
                options,
            } => {
                let mut p = BytesMut::with_capacity(6);
                p.put_u16_le(*source_cid);
                p.put_u16_le(*flags);
                p.put_u16_le(*result);
                encode_options(&mut p, options);
                (0x05, *identifier, p)
            }
            Self::DisconnectionRequest {
                identifier,
                destination_cid,
                source_cid,
            } => (0x06, *identifier, pair(*destination_cid, *source_cid)),
            Self::DisconnectionResponse {
                identifier,
                destination_cid,
                source_cid,
            } => (0x07, *identifier, pair(*destination_cid, *source_cid)),
            Self::EchoRequest { identifier, data } => {
                (0x08, *identifier, BytesMut::from(&data[..]))
            }
            Self::EchoResponse { identifier, data } => {
                (0x09, *identifier, BytesMut::from(&data[..]))
            }
            Self::InformationRequest {
                identifier,
                info_type,
            } => {
                let mut p = BytesMut::with_capacity(2);
                p.put_u16_le(*info_type);
                (0x0a, *identifier, p)
            }
            Self::InformationResponse {
                identifier,
                info_type,
                result,
                data,
            } => {
                let mut p = BytesMut::with_capacity(4 + data.len());
                p.put_u16_le(*info_type);
                p.put_u16_le(*result);
                p.extend_from_slice(data);
                (0x0b, *identifier, p)
            }
            Self::Unknown {
                code,
                identifier,
                data,
            } => (*code, *identifier, BytesMut::from(&data[..])),
        };
        let mut packet = BytesMut::with_capacity(4 + data.len());
        packet.put_u8(code);
        packet.put_u8(identifier);
        packet.put_u16_le(data.len() as u16);
        packet.extend_from_slice(&data);
        packet.freeze()
    }
}

fn parse_options(mut data: &[u8]) -> Result<Vec<ConfigurationOption>> {
    let mut options = Vec::new();
    while !data.is_empty() {
        let (header, remaining) = wire::<OptionHeader>(data)?;
        let length = usize::from(header.length);
        let value = remaining
            .get(..length)
            .ok_or(Error::InvalidPacket("short L2CAP signal data"))?;
        options.push(match (header.kind & 0x7f, length) {
            (0x01, 2) => ConfigurationOption::Mtu(wire::<U16>(value)?.0.get()),
            (0x02, 2) => ConfigurationOption::FlushTimeout(wire::<U16>(value)?.0.get()),
            (0x03, 22) => {
                ConfigurationOption::QualityOfService(*wire::<QualityOfService>(value)?.0)
            }
            (kind, _) => ConfigurationOption::Unknown {
                kind,
                hint: header.kind & 0x80 != 0,
                value: Bytes::copy_from_slice(value),
            },
        });
        data = &remaining[length..];
    }
    Ok(options)
}

fn encode_options(out: &mut BytesMut, options: &[ConfigurationOption]) {
    for option in options {
        match option {
            ConfigurationOption::Mtu(value) => {
                out.put_u8(1);
                out.put_u8(2);
                out.put_u16_le(*value);
            }
            ConfigurationOption::FlushTimeout(value) => {
                out.put_u8(2);
                out.put_u8(2);
                out.put_u16_le(*value);
            }
            ConfigurationOption::QualityOfService(value) => {
                out.extend_from_slice(&[3, 22]);
                out.extend_from_slice(value.as_bytes());
            }
            ConfigurationOption::Unknown { kind, hint, value } => {
                out.put_u8(*kind | (u8::from(*hint) << 7));
                out.put_u8(value.len() as u8);
                out.extend_from_slice(value);
            }
        }
    }
}

fn pair(a: u16, b: u16) -> BytesMut {
    let mut p = BytesMut::with_capacity(4);
    p.put_u16_le(a);
    p.put_u16_le(b);
    p
}
fn wire<T>(data: &[u8]) -> Result<(&T, &[u8])>
where
    T: FromBytes + KnownLayout + Immutable + Unaligned,
{
    T::ref_from_prefix(data).map_err(|_| Error::InvalidPacket("short L2CAP signal data"))
}
