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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_box_request_round_trips_json() {
        let request = MessageBoxRequest {
            protocol_version: UI_PROTOCOL_VERSION,
            process_id: 12,
            thread_id: 34,
            owner_window: 56,
            caption: "Error".into(),
            text: "Something failed: caf\u{e9}".into(),
            style: 0x11,
            buttons: vec![DialogButton {
                result: 1,
                label: "OK".into(),
            }],
            default_result: Some(1),
            escape_result: Some(1),
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            serde_json::from_slice::<MessageBoxRequest>(&bytes).unwrap(),
            request
        );
    }

    #[test]
    fn request_and_response_round_trip_over_socket() {
        let name = format!(
            "polymer-ui-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let listener = crate::LocalSocketListener::bind(&name).unwrap();
        let server = std::thread::spawn(move || {
            let mut socket = listener.accept().unwrap();
            let request: MessageBoxRequest = receive(&mut socket).unwrap();
            assert_eq!(request.text, "forward me");
            send(&mut socket, &MessageBoxResponse { result: 6 }).unwrap();
        });
        let mut socket = crate::LocalSocket::connect(&name).unwrap();
        send(
            &mut socket,
            &MessageBoxRequest {
                protocol_version: UI_PROTOCOL_VERSION,
                process_id: 1,
                thread_id: 2,
                owner_window: 3,
                caption: "Question".into(),
                text: "forward me".into(),
                style: 4,
                buttons: vec![],
                default_result: None,
                escape_result: None,
            },
        )
        .unwrap();
        assert_eq!(
            receive::<MessageBoxResponse>(&mut socket).unwrap().result,
            6
        );
        server.join().unwrap();
    }
}
