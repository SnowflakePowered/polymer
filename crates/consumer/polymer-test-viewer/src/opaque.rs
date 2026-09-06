use polymer_ipc::{LocalSocketListener, Message, StreamReady, frame::FrameSubscriber};
use polymer_presentation::producer::{ExportDescriptor, ProducerSession};
use polymer_protocol::{ConsumerApi, FrameReady, Handshake, PROTOCOL_VERSION};
use std::{
    os::windows::process::CommandExt,
    process::Command,
    time::{Duration, Instant},
};

pub struct OpaqueViewer;
impl OpaqueViewer {
    pub(super) fn verify(ready: &StreamReady, frame: &FrameReady, red: bool) {
        Self::verify_export(
            ExportDescriptor {
                surface_id: ready.surface_id,
                adapter_luid: ready.adapter_luid,
                source_window: ready.source_window,
                width: ready.width,
                height: ready.height,
                format: ready.format,
                image_kind: ready.image_kind,
                sync_kind: ready.sync_kind,
                image_handles: &ready.image_handles[..ready.slot_count as usize],
                timeline_semaphore_handle: ready.timeline_semaphore_handle,
                consumer_semaphore_handle: ready.consumer_semaphore_handle,
                bottom_up: ready.bottom_up,
            },
            frame,
            red,
        );
    }

    pub fn verify_export(descriptor: ExportDescriptor<'_>, frame: &FrameReady, red: bool) {
        let session = format!("polymer-opaque-import-{}", std::process::id());
        let executable = std::env::current_exe().unwrap();
        let directory = executable.parent().unwrap();
        let directory = if directory.file_name().unwrap() == "deps" {
            directory.parent().unwrap()
        } else {
            directory
        };
        let mut child = Command::new(directory.join("polymer-test-viewer.exe"))
            .env("POLYMER_OPAQUE_IMPORT_SESSION", &session)
            .env("POLYMER_OPAQUE_IMPORT_RED", if red { "1" } else { "0" })
            .env_remove("VK_INSTANCE_LAYERS")
            .env_remove("VK_IMPLICIT_LAYER_PATH")
            .env_remove("VK_LAYER_PATH")
            .env_remove("POLYMER_VULKAN_LAYER")
            .creation_flags(0x08000000)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut connection = loop {
            if let Ok(connection) = ProducerSession::connect(&session) {
                break connection;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "opaque viewer exited before connection"
            );
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("opaque viewer did not start");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        connection.publish(descriptor).unwrap();
        connection.send(Message::FrameReady(*frame)).unwrap();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "opaque viewer import/readback failed: {status}"
                );
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("opaque viewer import/readback timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

pub fn run_child() {
    let session = std::env::var("POLYMER_OPAQUE_IMPORT_SESSION").unwrap();
    let listener = LocalSocketListener::bind(&session).unwrap();
    let mut socket = listener.accept().unwrap();
    socket
        .send(Message::Hello(Handshake {
            protocol_version: PROTOCOL_VERSION,
            process_id: std::process::id(),
            preferred_consumer: ConsumerApi::Vulkan,
        }))
        .unwrap();
    let Message::MapOffer(offer) = socket.receive().unwrap() else {
        panic!("missing frame channel")
    };
    let mut frames = FrameSubscriber::from_offer(offer).unwrap();
    let Message::StreamReady(ready) = socket.receive().unwrap() else {
        panic!("missing stream")
    };
    let mut imported = polymer_consumer_vulkan::ImportedStream::import(&ready).unwrap();
    socket.send(Message::StreamAccepted).unwrap();
    assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
    let frame = frames.latest().unwrap().unwrap();
    let pixels = imported
        .read_frame_rgba(frame.slot, frame.sync_value)
        .unwrap();
    let expected = if std::env::var("POLYMER_OPAQUE_IMPORT_RED").unwrap() == "1" {
        [255, 0, 0, 255]
    } else {
        [0, 255, 0, 255]
    };
    assert!(pixels.chunks_exact(4).all(|pixel| pixel == expected));
}
