//! Answer peer discovery with an empty service database. Polymer is an HID
//! host, not an HID peripheral; SDP probes must still receive a response.
use crate::transport::bredr::l2cap::{Link, SDP_PSM};
use bytes::{BufMut, Bytes, BytesMut};
use tokio::task::JoinHandle;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::big_endian::U16};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Header {
    pdu: u8,
    transaction: U16,
    length: U16,
}

pub(crate) struct Server;

impl Server {
    pub fn spawn(link: Link) -> JoinHandle<()> {
        tokio::spawn(async move {
            // Owned by BrEdrHid and aborted when its connection closes.
            while let Ok(mut channel) = link.accept_channel(SDP_PSM).await {
                while let Ok(request) = channel.receive().await {
                    tracing::debug!(request = ?request, "received host SDP probe");
                    let Some(response) = Self::response(&request) else {
                        break;
                    };
                    if channel.send(response, false).await.is_err() {
                        break;
                    }
                }
            }
        })
    }

    fn response(request: &[u8]) -> Option<Bytes> {
        let (header, parameters) = Header::ref_from_prefix(request).ok()?;
        let result = if usize::from(header.length.get()) != parameters.len() {
            Err(4u16) // Invalid PDU size
        } else {
            Self::parameters(header.pdu, parameters)
        };
        let (pdu, data) = match result {
            Ok(response) => response,
            Err(code) => (1, code.to_be_bytes().to_vec()),
        };
        let mut response = BytesMut::new();
        response.put_u8(pdu);
        response.put_u16(header.transaction.get());
        response.put_u16(data.len() as u16);
        response.extend_from_slice(&data);
        Some(response.freeze())
    }

    fn parameters(pdu: u8, parameters: &[u8]) -> Result<(u8, Vec<u8>), u16> {
        if pdu == 4 {
            return Err(2);
        } // No valid service record handles.
        if pdu != 2 && pdu != 6 {
            return Err(3);
        }
        let (pattern, used) = super::parse_element(parameters).map_err(|_| 3u16)?;
        if !matches!(pattern, super::Element::Sequence(_)) {
            return Err(3);
        }
        let remaining = parameters.get(used..).ok_or(3u16)?;
        let maximum = U16::read_from_prefix(remaining).map_err(|_| 3u16)?.0.get();
        if maximum == 0 || (pdu == 6 && maximum < 7) {
            return Err(3);
        }
        let mut tail = &remaining[2..];
        if pdu == 6 {
            let (attributes, used) = super::parse_element(tail).map_err(|_| 3u16)?;
            if !matches!(attributes, super::Element::Sequence(_)) {
                return Err(3);
            }
            tail = &tail[used..];
        }
        if tail != [0] {
            return Err(5);
        }
        Ok(match pdu {
            2 => (3, vec![0, 0, 0, 0, 0]), // total/current handles, continuation
            _ => (7, vec![0, 2, 0x35, 0, 0]), // empty attribute-list sequence
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_searches_complete_without_inventing_host_services() {
        let search = [2, 0x12, 0x34, 0, 8, 0x35, 3, 0x19, 0x12, 0, 0, 8, 0];
        assert_eq!(
            Server::response(&search).unwrap().as_ref(),
            &[3, 0x12, 0x34, 0, 5, 0, 0, 0, 0, 0]
        );
        let search_attributes = super::super::request(42, 0x1200, &[]);
        let response = Server::response(&search_attributes).unwrap();
        let (attributes, continuation) = super::super::parse_response(&response, 42).unwrap();
        assert_eq!(attributes, &[0x35, 0]);
        assert!(continuation.is_empty());
        let mut truncated = search.to_vec();
        truncated.pop();
        assert_eq!(
            Server::response(&truncated).unwrap().as_ref(),
            &[1, 0x12, 0x34, 0, 2, 0, 4]
        );
        let continued = super::super::request(7, 0x1200, &[1]);
        assert_eq!(
            Server::response(&continued).unwrap().as_ref(),
            &[1, 0, 7, 0, 2, 0, 5]
        );
    }
}
