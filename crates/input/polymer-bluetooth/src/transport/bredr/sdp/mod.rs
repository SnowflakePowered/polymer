//! Minimal Service Discovery Protocol support used for controller identity.

pub(super) mod server;

use std::collections::BTreeMap;

use bytes::{BufMut, Bytes, BytesMut};
use uuid::Uuid;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::big_endian::U16};

use crate::{Error, Result, devices::PnpIdentity};

use super::l2cap::{Link, SDP_PSM};

const SERVICE_SEARCH_ATTRIBUTE_REQUEST: u8 = 0x06;
const SERVICE_SEARCH_ATTRIBUTE_RESPONSE: u8 = 0x07;
const HID_SERVICE: u16 = 0x1124;
const PNP_INFORMATION_SERVICE: u16 = 0x1200;
const MAX_ATTRIBUTE_BYTES: u16 = 4096;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ResponseHeader {
    pdu_id: u8,
    transaction: U16,
    parameter_length: U16,
    attribute_byte_count: U16,
}

#[derive(Clone, Debug, Default)]
pub struct SdpIdentity {
    pub services: Vec<Uuid>,
    pub pnp: Option<PnpIdentity>,
}

pub async fn query_identity(link: &Link) -> Result<SdpIdentity> {
    let mut identity = SdpIdentity::default();
    for (transaction, service) in [(1, HID_SERVICE), (2, PNP_INFORMATION_SERVICE)] {
        let attributes = query_service(link, transaction, service).await?;
        if let Some(Element::Sequence(classes)) = attributes.get(&0x0001) {
            for class in classes {
                if let Element::Uuid16(value) = class {
                    identity.services.push(bluetooth_uuid(*value));
                }
            }
        }
        if service == PNP_INFORMATION_SERVICE {
            let integer = |id| match attributes.get(&id) {
                Some(Element::Unsigned(value)) => u16::try_from(*value).ok(),
                _ => None,
            };
            if let (Some(vendor_id_source), Some(vendor_id), Some(product_id), Some(version)) = (
                integer(0x0205),
                integer(0x0201),
                integer(0x0202),
                integer(0x0203),
            ) {
                identity.pnp = Some(PnpIdentity {
                    vendor_id_source,
                    vendor_id,
                    product_id,
                    version,
                });
            }
        }
    }
    identity.services.sort_unstable();
    identity.services.dedup();
    Ok(identity)
}

async fn query_service(
    link: &Link,
    transaction: u16,
    service: u16,
) -> Result<BTreeMap<u16, Element>> {
    let mut channel = link.open_channel(SDP_PSM).await?;
    let mut continuation = Vec::new();
    let mut attribute_bytes = Vec::new();
    loop {
        channel
            .send(request(transaction, service, &continuation), false)
            .await?;
        let response = channel.receive().await?;
        let (part, next) = parse_response(&response, transaction)?;
        attribute_bytes.extend_from_slice(part);
        if attribute_bytes.len() > usize::from(MAX_ATTRIBUTE_BYTES) {
            return Err(Error::InvalidPacket("SDP attribute response exceeds limit"));
        }
        continuation = next.to_vec();
        if continuation.is_empty() {
            break;
        }
    }
    parse_attribute_lists(&attribute_bytes)
}

fn request(transaction: u16, service: u16, continuation: &[u8]) -> Bytes {
    let mut parameters = BytesMut::new();
    parameters.extend_from_slice(&[0x35, 0x03, 0x19]);
    parameters.put_u16(service);
    parameters.put_u16(MAX_ATTRIBUTE_BYTES);
    parameters.extend_from_slice(&[0x35, 0x05, 0x0a]);
    parameters.put_u32(0x0000_ffff);
    parameters.put_u8(continuation.len() as u8);
    parameters.extend_from_slice(continuation);
    let mut packet = BytesMut::with_capacity(parameters.len() + 5);
    packet.put_u8(SERVICE_SEARCH_ATTRIBUTE_REQUEST);
    packet.put_u16(transaction);
    packet.put_u16(parameters.len() as u16);
    packet.extend_from_slice(&parameters);
    packet.freeze()
}

fn parse_response(packet: &[u8], transaction: u16) -> Result<(&[u8], &[u8])> {
    let (header, parameters) = ResponseHeader::ref_from_prefix(packet)
        .map_err(|_| Error::InvalidPacket("unexpected SDP response"))?;
    if header.pdu_id != SERVICE_SEARCH_ATTRIBUTE_RESPONSE {
        return Err(Error::InvalidPacket("unexpected SDP response"));
    }
    if header.transaction.get() != transaction {
        return Err(Error::InvalidPacket("SDP transaction mismatch"));
    }
    let parameter_len = usize::from(header.parameter_length.get());
    if packet.len() != parameter_len + 5 {
        return Err(Error::InvalidPacket("truncated SDP response"));
    }
    let byte_count = usize::from(header.attribute_byte_count.get());
    let bytes = parameters
        .get(..byte_count)
        .ok_or(Error::InvalidPacket("truncated SDP attributes"))?;
    let continuation_len = usize::from(
        *parameters
            .get(byte_count)
            .ok_or(Error::InvalidPacket("missing SDP continuation"))?,
    );
    let continuation = parameters
        .get(byte_count + 1..byte_count + 1 + continuation_len)
        .ok_or(Error::InvalidPacket("truncated SDP continuation"))?;
    Ok((bytes, continuation))
}

#[derive(Clone, Debug)]
enum Element {
    Unsigned(u64),
    Uuid16(u16),
    Sequence(Vec<Element>),
    Other,
}

fn parse_attribute_lists(bytes: &[u8]) -> Result<BTreeMap<u16, Element>> {
    let (root, consumed) = parse_element(bytes)?;
    if consumed != bytes.len() {
        return Err(Error::InvalidPacket("trailing SDP attribute bytes"));
    }
    let Element::Sequence(records) = root else {
        return Err(Error::InvalidPacket("SDP response is not a sequence"));
    };
    let mut attributes = BTreeMap::new();
    for record in records {
        let Element::Sequence(values) = record else {
            continue;
        };
        let mut values = values.into_iter();
        while let (Some(Element::Unsigned(id)), Some(value)) = (values.next(), values.next()) {
            if let Ok(id) = u16::try_from(id) {
                attributes.insert(id, value);
            }
        }
    }
    Ok(attributes)
}

fn parse_element(bytes: &[u8]) -> Result<(Element, usize)> {
    let descriptor = *bytes
        .first()
        .ok_or(Error::InvalidPacket("empty SDP data element"))?;
    let kind = descriptor >> 3;
    let size_index = descriptor & 7;
    let (length, header) = match size_index {
        0..=4 => (1usize << size_index, 1),
        5 => (
            usize::from(
                *bytes
                    .get(1)
                    .ok_or(Error::InvalidPacket("truncated SDP length"))?,
            ),
            2,
        ),
        6 => (
            usize::from(u16::from_be_bytes(
                bytes
                    .get(1..3)
                    .ok_or(Error::InvalidPacket("truncated SDP length"))?
                    .try_into()
                    .unwrap(),
            )),
            3,
        ),
        _ => return Err(Error::Unsupported("32-bit SDP data-element lengths")),
    };
    let data = bytes
        .get(header..header + length)
        .ok_or(Error::InvalidPacket("truncated SDP data element"))?;
    let element = match kind {
        1 if length <= 8 => Element::Unsigned(
            data.iter()
                .fold(0u64, |value, byte| value << 8 | u64::from(*byte)),
        ),
        3 if length == 2 => Element::Uuid16(u16::from_be_bytes(data.try_into().unwrap())),
        6 | 7 => {
            let mut values = Vec::new();
            let mut at = 0;
            while at < data.len() {
                let (value, used) = parse_element(&data[at..])?;
                values.push(value);
                at += used;
            }
            Element::Sequence(values)
        }
        _ => Element::Other,
    };
    Ok((element, header + length))
}

fn bluetooth_uuid(short: u16) -> Uuid {
    Uuid::from_u128((u128::from(short) << 96) | 0x0000_0000_0000_1000_8000_0080_5f9b_34fb)
}
