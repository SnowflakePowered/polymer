//! Process handshake, shared-memory rings, and external-handle exchange.

#![forbid(unsafe_code)]

use snowglass_protocol::PROTOCOL_VERSION;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handshake {
    pub protocol_version: u32,
    pub process_id: u32,
}

impl Handshake {
    pub fn for_process(process_id: u32) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            process_id,
        }
    }
}
