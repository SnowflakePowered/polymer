use bytes::{BufMut, Bytes, BytesMut};

use crate::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationOption {
    Mtu(u16),
    FlushTimeout(u16),
    Unknown { kind: u8, hint: bool, value: Bytes },
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
            if payload.len() < 4 {
                return Err(Error::InvalidPacket("short L2CAP signaling header"));
            }
            let code = payload[0];
            let identifier = payload[1];
            if identifier == 0 {
                return Err(Error::InvalidPacket("zero L2CAP signaling identifier"));
            }
            let length = u16::from_le_bytes([payload[2], payload[3]]) as usize;
            if payload.len() < 4 + length {
                return Err(Error::InvalidPacket("truncated L2CAP signal"));
            }
            signals.push(Self::parse_one(code, identifier, &payload[4..4 + length])?);
            payload = &payload[4 + length..];
        }
        Ok(signals)
    }

    fn parse_one(code: u8, identifier: u8, data: &[u8]) -> Result<Self> {
        let pair = || -> Result<(u16, u16)> {
            require(data, 4)?;
            Ok((le16(data, 0), le16(data, 2)))
        };
        Ok(match code {
            0x01 => {
                require(data, 2)?;
                Self::CommandReject {
                    identifier,
                    reason: le16(data, 0),
                    data: Bytes::copy_from_slice(&data[2..]),
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
                require(data, 8)?;
                Self::ConnectionResponse {
                    identifier,
                    destination_cid: le16(data, 0),
                    source_cid: le16(data, 2),
                    result: le16(data, 4),
                    status: le16(data, 6),
                }
            }
            0x04 => {
                require(data, 4)?;
                Self::ConfigurationRequest {
                    identifier,
                    destination_cid: le16(data, 0),
                    flags: le16(data, 2),
                    options: parse_options(&data[4..])?,
                }
            }
            0x05 => {
                require(data, 6)?;
                Self::ConfigurationResponse {
                    identifier,
                    source_cid: le16(data, 0),
                    flags: le16(data, 2),
                    result: le16(data, 4),
                    options: parse_options(&data[6..])?,
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
                require(data, 2)?;
                Self::InformationRequest {
                    identifier,
                    info_type: le16(data, 0),
                }
            }
            0x0b => {
                require(data, 4)?;
                Self::InformationResponse {
                    identifier,
                    info_type: le16(data, 0),
                    result: le16(data, 2),
                    data: Bytes::copy_from_slice(&data[4..]),
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
        require(data, 2)?;
        let raw_kind = data[0];
        let length = data[1] as usize;
        require(data, 2 + length)?;
        let value = &data[2..2 + length];
        options.push(match (raw_kind & 0x7f, length) {
            (0x01, 2) => ConfigurationOption::Mtu(le16(value, 0)),
            (0x02, 2) => ConfigurationOption::FlushTimeout(le16(value, 0)),
            (kind, _) => ConfigurationOption::Unknown {
                kind,
                hint: raw_kind & 0x80 != 0,
                value: Bytes::copy_from_slice(value),
            },
        });
        data = &data[2 + length..];
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
fn require(data: &[u8], length: usize) -> Result<()> {
    if data.len() < length {
        Err(Error::InvalidPacket("short L2CAP signal data"))
    } else {
        Ok(())
    }
}
fn le16(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_request_round_trip() {
        let signal = Signal::ConnectionRequest {
            identifier: 7,
            psm: 0x13,
            source_cid: 0x41,
        };
        assert_eq!(Signal::parse_all(&signal.encode()).unwrap(), vec![signal]);
    }

    #[test]
    fn config_options_round_trip() {
        let signal = Signal::ConfigurationRequest {
            identifier: 3,
            destination_cid: 0x42,
            flags: 0,
            options: vec![
                ConfigurationOption::Mtu(672),
                ConfigurationOption::FlushTimeout(0xffff),
            ],
        };
        assert_eq!(Signal::parse_all(&signal.encode()).unwrap(), vec![signal]);
    }

    #[test]
    fn information_response_round_trip() {
        let signal = Signal::InformationResponse {
            identifier: 9,
            info_type: 2,
            result: 0,
            data: Bytes::copy_from_slice(&0u32.to_le_bytes()),
        };
        assert_eq!(Signal::parse_all(&signal.encode()).unwrap(), vec![signal]);
    }
}
