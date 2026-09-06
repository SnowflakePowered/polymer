use super::*;
use polymer_ipc::{LocalSocketListener, Message, StreamReady, frame::FrameSubscriber};
use polymer_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, Handshake, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, StreamId, SurfaceId,
};
use std::{
    io,
    os::windows::io::AsRawHandle,
    sync::atomic::{AtomicU64, Ordering},
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    name: String,
    file: std::fs::File,
    path: std::path::PathBuf,
}
impl Fixture {
    fn accepted(
        &self,
        serve: impl FnOnce(polymer_ipc::LocalSocket, FrameSubscriber) + Send + 'static,
    ) -> (Option<ProducerSession>, std::thread::JoinHandle<()>) {
        let listener = LocalSocketListener::bind(&self.name).unwrap();
        let server = std::thread::spawn(move || {
            let mut socket = listener.accept().unwrap();
            socket.send(Self::hello(PROTOCOL_VERSION)).unwrap();
            let Message::MapOffer(offer) = socket.receive().unwrap() else {
                panic!("missing frame channel");
            };
            let frames = FrameSubscriber::from_offer(offer).unwrap();
            let Message::StreamReady(ready) = socket.receive().unwrap() else {
                panic!("missing resource publication");
            };
            unsafe { CloseHandle(HANDLE(ready.image_handles[0] as *mut _)).unwrap() };
            socket.send(Message::StreamAccepted).unwrap();
            serve(socket, frames);
        });
        let mut session = ProducerSession::connect(&self.name).unwrap();
        session
            .publish(self.descriptor(&[self.file.as_raw_handle() as i64]))
            .unwrap();
        (Some(session), server)
    }

    fn new() -> Self {
        let name = format!(
            "polymer-presentation-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(&name);
        Self {
            file: std::fs::File::create(&path).unwrap(),
            path,
            name,
        }
    }
    fn ready(&self) -> StreamReady {
        self.descriptor(&[self.file.as_raw_handle() as i64])
            .into_ready()
            .unwrap()
    }
    fn descriptor<'a>(&self, handles: &'a [i64]) -> ExportDescriptor<'a> {
        ExportDescriptor {
            surface_id: SurfaceId(1),
            adapter_luid: AdapterLuid {
                low_part: 0,
                high_part: 0,
            },
            source_window: 0,
            width: 32,
            height: 64,
            format: 87,
            image_kind: ExternalImageKind::D3d12Resource,
            sync_kind: ExternalSyncKind::D3d12Fence,
            image_handles: handles,
            timeline_semaphore_handle: 0,
            consumer_semaphore_handle: 0,
            bottom_up: false,
        }
    }
    fn hello(version: u32) -> Message {
        Message::Hello(Handshake {
            protocol_version: version,
            process_id: std::process::id(),
            preferred_consumer: ConsumerApi::D3d12,
        })
    }
}

#[test]
fn disconnected_publisher_releases_only_its_session_and_sibling_numbers_only_sent_frames() {
    let disconnected = Fixture::new();
    let sibling = Fixture::new();
    let (mut failed, closed) = disconnected.accepted(|_, _| {});
    let (acknowledge, received) = std::sync::mpsc::channel();
    let (mut healthy, viewer) = sibling.accepted(move |mut socket, mut frames| {
        for (number, sync, timestamp) in [(1, 42, 17), (2, 81, 29)] {
            assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
            let frame = frames.latest().unwrap().unwrap();
            assert_eq!(
                (frame.frame_number, frame.sync_value, frame.timestamp_qpc),
                (number, sync, timestamp)
            );
            acknowledge.send(()).unwrap();
        }
    });
    let frame = |sync_value, timestamp_qpc| {
        let extent = crate::Extent2D {
            width: 32,
            height: 64,
        };
        CapturedFrame {
            stream_id: StreamId(1),
            slot: 0,
            extent,
            sync_value,
            timestamp_qpc,
        }
        .present(FramePlacement {
            extent,
            bounds: extent.rect(),
            transform: polymer_protocol::PresentationTransform::UNAVAILABLE,
        })
    };
    assert!(ProducerSession::publish_frame(&mut healthy, frame(42, 17)));
    received
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    closed.join().unwrap();
    // A socket may buffer a notification after peer closure. The finite burst
    // also exercises clearing the session when the transport reports closure.
    for _ in 0..256 {
        if !ProducerSession::publish_frame(&mut failed, frame(100, 20)) {
            break;
        }
    }
    assert!(failed.is_none());
    assert!(!ProducerSession::publish_frame(&mut failed, frame(101, 21)));
    assert!(ProducerSession::publish_frame(&mut healthy, frame(81, 29)));
    received
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    viewer.join().unwrap();
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn incompatible_protocol_is_distinct_from_a_missing_viewer() {
    let fixture = Fixture::new();
    assert_eq!(
        ProducerSession::connect(&fixture.name)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::ConnectionRefused
    );
    let listener = LocalSocketListener::bind(&fixture.name).unwrap();
    let server = std::thread::spawn(move || {
        listener
            .accept()
            .unwrap()
            .send(Fixture::hello(PROTOCOL_VERSION + 1))
            .unwrap()
    });
    assert_eq!(
        ProducerSession::connect(&fixture.name)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    server.join().unwrap();
}

#[test]
fn rejection_is_isolated_and_accepted_frames_preserve_capture_clock_and_gpu_value() {
    for outcome in 0..3 {
        let accept = outcome == 2;
        let fixture = Fixture::new();
        let listener = LocalSocketListener::bind(&fixture.name).unwrap();
        let server = std::thread::spawn(move || {
            let mut socket = listener.accept().unwrap();
            socket.send(Fixture::hello(PROTOCOL_VERSION)).unwrap();
            let Message::MapOffer(offer) = socket.receive().unwrap() else {
                panic!("missing frame channel");
            };
            let mut frames = FrameSubscriber::from_offer(offer).unwrap();
            let Message::StreamReady(ready) = socket.receive().unwrap() else {
                panic!("missing resource publication");
            };
            // Receiver ownership begins at publication, including rejection.
            unsafe {
                CloseHandle(HANDLE(ready.image_handles[0] as *mut _)).unwrap();
            }
            if !accept {
                if outcome == 0 {
                    socket.send(Fixture::hello(PROTOCOL_VERSION)).unwrap();
                }
                return;
            }
            socket.send(Message::StreamAccepted).unwrap();
            assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
            let frame = frames.latest().unwrap().unwrap();
            assert_eq!(
                (frame.frame_number, frame.sync_value, frame.timestamp_qpc),
                (1, 42, 17)
            );
        });
        let mut client = ProducerSession::connect(&fixture.name).unwrap();
        let result = client.publish(fixture.descriptor(&[fixture.file.as_raw_handle() as i64]));
        if accept {
            result.unwrap();
            let extent = crate::Extent2D {
                width: 32,
                height: 64,
            };
            client
                .send(
                    CapturedFrame {
                        stream_id: StreamId(1),
                        slot: 0,
                        extent,
                        sync_value: 42,
                        timestamp_qpc: 17,
                    }
                    .present(FramePlacement {
                        extent,
                        bounds: extent.rect(),
                        transform: polymer_protocol::PresentationTransform::UNAVAILABLE,
                    }),
                )
                .unwrap();
        } else {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }
        server.join().unwrap();
        assert!(client.send(Message::StreamAccepted).is_err());
    }
}

#[test]
fn failed_handle_preparation_rolls_back_and_slot_limits_are_validated_before_publication() {
    let fixture = Fixture::new();
    for count in [0, MAX_IMAGE_SLOTS as u32 + 1] {
        let handles = vec![fixture.file.as_raw_handle() as i64; count as usize];
        assert_eq!(
            fixture
                .descriptor(&handles)
                .into_ready()
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let mut ready = fixture.ready();
        ready.slot_count = count;
        assert_eq!(
            super::export::Export::prepare(std::process::id(), &mut ready)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    let mut ready = fixture.ready();
    ready.slot_count = 2;
    assert!(super::export::Export::prepare(std::process::id(), &mut ready).is_err());
    // The failed batch must leave the borrowed original handle usable.
    assert!(fixture.file.metadata().is_ok());
    let mut flags = 0;
    assert!(
        unsafe {
            windows::Win32::Foundation::GetHandleInformation(
                HANDLE(ready.image_handles[0] as *mut _),
                &mut flags,
            )
        }
        .is_err()
    );
}
