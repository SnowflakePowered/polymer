//! Bidirectional forwarding for modal guest user-interface requests.

use serde::{Deserialize, Serialize};
use std::io;

const MAX_PACKET_SIZE: usize = 1024 * 1024;
pub const UI_PROTOCOL_VERSION: u32 = 2;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DialogButton {
    pub result: i32,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MessageBoxRequest {
    pub protocol_version: u32,
    pub process_id: u32,
    pub thread_id: u32,
    pub owner_window: i64,
    pub caption: String,
    pub text: String,
    pub style: u32,
    pub buttons: Vec<DialogButton>,
    pub default_result: Option<i32>,
    pub escape_result: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MessageBoxResponse {
    /// A Win32 `ID*` result such as `IDOK`, `IDCANCEL`, or `IDYES`.
    pub result: i32,
}

pub fn send<T: Serialize>(socket: &mut crate::LocalSocket, value: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
    if payload.len() > MAX_PACKET_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "UI IPC packet is too large",
        ));
    }
    socket.write_packet(&payload)
}

pub fn receive<T: for<'de> Deserialize<'de>>(socket: &mut crate::LocalSocket) -> io::Result<T> {
    let payload = socket.read_packet(MAX_PACKET_SIZE)?;
    serde_json::from_slice(&payload).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid UI packet: {error}"),
        )
    })
}
