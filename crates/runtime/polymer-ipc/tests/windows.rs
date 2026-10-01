#![cfg(windows)]

use polymer_ipc::{LocalSocket, LocalSocketListener, Message};
use polymer_protocol::{ConsumerApi, FrameReady, Handshake, PROTOCOL_VERSION, StreamId};
use std::{process::Command, time::Duration};

#[test]
fn connects_with_redirected_guest_temp() {
    let frame = FrameReady {
        stream_id: StreamId(1),
        slot: 0,
        frame_number: 1,
        sync_value: 1,
        timestamp_qpc: 123,
    };
    if let Ok(session) = std::env::var("POLYMER_TEST_SESSION") {
        assert_ne!(std::env::temp_dir(), LocalSocketListener::directory());
        let mut socket = LocalSocket::connect_timeout(&session, Duration::ZERO).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let Message::Hello(_) = socket.receive().unwrap() else {
            panic!("expected host handshake");
        };
        socket.send(Message::FrameReady(frame)).unwrap();
        assert_eq!(socket.receive().unwrap(), Message::StreamAccepted);
        socket.send(Message::Shutdown).unwrap();
        return;
    }

    let guest_temp = tempfile::tempdir().unwrap();
    for suffix in ["", "-input", "-ui"] {
        let session = format!("polymer-temp-test-{}{suffix}", std::process::id());
        let listener = LocalSocketListener::bind(&session).unwrap();
        let acceptor = listener.clone();
        let worker = std::thread::spawn(move || -> std::io::Result<()> {
            let mut socket = acceptor.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            socket.send(Message::Hello(Handshake {
                protocol_version: PROTOCOL_VERSION,
                process_id: std::process::id(),
                preferred_consumer: ConsumerApi::D3d12,
            }))?;
            assert_eq!(socket.receive()?, Message::FrameReady(frame));
            socket.send(Message::StreamAccepted)?;
            assert_eq!(socket.receive()?, Message::Shutdown);
            Ok(())
        });
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "connects_with_redirected_guest_temp",
                "--nocapture",
            ])
            .env("POLYMER_TEST_SESSION", &session)
            .env(
                LocalSocketListener::DIRECTORY_ENV,
                LocalSocketListener::directory(),
            )
            .env("TEMP", guest_temp.path().join("unavailable"))
            .env("TMP", guest_temp.path().join("unavailable"))
            .output()
            .unwrap();
        listener.close();
        let exchange = worker.join().unwrap();
        assert!(
            output.status.success(),
            "guest failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        exchange.unwrap();
    }
}
