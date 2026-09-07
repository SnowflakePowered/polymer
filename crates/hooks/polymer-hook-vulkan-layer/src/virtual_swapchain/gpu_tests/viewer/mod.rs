use super::*;
mod d3d12;
use polymer_ipc::{LocalSocketListener, Message};
use polymer_protocol::{ConsumerApi, Handshake, PROTOCOL_VERSION};
use std::sync::mpsc;

pub(super) struct Viewer {
    worker: std::thread::JoinHandle<()>,
    frames: mpsc::Receiver<()>,
}
impl Viewer {
    pub(super) fn new(session: &str, connections: usize) -> Self {
        let api = if std::env::var("POLYMER_VULKAN_WSI_TEST_CHILD").unwrap() == "d3d12" {
            ConsumerApi::D3d12
        } else {
            ConsumerApi::Vulkan
        };
        let listener = LocalSocketListener::bind(session).unwrap();
        let (send, frames) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..connections {
                let mut socket = listener.accept().unwrap();
                let send = send.clone();
                workers.push(std::thread::spawn(move || {
                    socket
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    socket
                        .send(Message::Hello(Handshake {
                            protocol_version: PROTOCOL_VERSION,
                            process_id: std::process::id(),
                            preferred_consumer: api,
                        }))
                        .unwrap();
                    let Message::MapOffer(offer) = socket.receive().unwrap() else {
                        panic!("missing frame mapping");
                    };
                    let mut frames =
                        polymer_ipc::frame::FrameSubscriber::from_offer(offer).unwrap();
                    assert!(matches!(
                        socket.receive().unwrap(),
                        Message::RequestBufferAvailability
                    ));
                    let Message::StreamReady(ready) = socket.receive().unwrap() else {
                        panic!("missing ring");
                    };
                    let mut imported = Reader::new(api, &ready);
                    socket.send(Message::StreamAccepted).unwrap();
                    while let Ok(message) = socket.receive() {
                        let frame = match message {
                            Message::FrameReady(frame) => frame,
                            Message::FrameAvailable => {
                                let Some(frame) = frames.latest().unwrap() else {
                                    continue;
                                };
                                frame
                            }
                            _ => panic!("unexpected message"),
                        };
                        let pixels = imported.read(frame.slot, frame.sync_value);
                        assert!(
                            pixels
                                .chunks_exact(4)
                                .all(|pixel| pixel == [255, 0, 0, 255]),
                            "Vulkan WSI frame had unexpected pixels: {:?}",
                            &pixels[..16]
                        );
                        socket
                            .send(Message::BufferAvailable {
                                stream_id: frame.stream_id,
                                through_sync_value: frame.sync_value,
                            })
                            .unwrap();
                        send.send(()).unwrap();
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self { worker, frames }
    }
    pub(super) fn frame(&self) {
        self.frames.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    pub(super) fn finish(self) {
        self.worker.join().unwrap();
    }
}

enum Reader {
    Vulkan(polymer_consumer_vulkan::ImportedStream),
    D3d12(d3d12::Reader),
}
impl Reader {
    fn new(api: ConsumerApi, ready: &polymer_ipc::StreamReady) -> Self {
        match api {
            ConsumerApi::D3d12 => Self::D3d12(d3d12::Reader::new(ready)),
            _ => Self::Vulkan(polymer_consumer_vulkan::ImportedStream::import(ready).unwrap()),
        }
    }
    fn read(&mut self, slot: u32, value: u64) -> Vec<u8> {
        match self {
            Self::Vulkan(reader) => reader.read_frame_rgba(slot, value).unwrap(),
            Self::D3d12(reader) => reader.read(slot, value),
        }
    }
}
