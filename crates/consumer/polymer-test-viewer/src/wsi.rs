use polymer_ipc::{LocalSocketListener, Message, StreamReady, frame::FrameSubscriber};
use polymer_protocol::{ConsumerApi, FrameReady, Handshake, PROTOCOL_VERSION};
use std::sync::{Arc, Condvar, Mutex, mpsc};

pub struct WsiViewer {
    frames: mpsc::Receiver<(StreamReady, FrameReady)>,
    release: Arc<(Mutex<bool>, Condvar)>,
    server: Option<std::thread::JoinHandle<()>>,
}

impl WsiViewer {
    pub fn start(streams: usize) -> Self {
        let opaque = std::env::var_os("POLYMER_WSI_OPAQUE").is_some();
        let session = std::env::var("POLYMER_SESSION")
            .expect("viewer fixture requires a private session name");
        let listener = LocalSocketListener::bind(&session).unwrap();
        let (sender, frames) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = release.clone();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..streams {
                let mut socket = listener.accept().unwrap();
                let sender = sender.clone();
                let release = worker_release.clone();
                workers.push(std::thread::spawn(move || {
                    socket
                        .send(Message::Hello(Handshake {
                            protocol_version: PROTOCOL_VERSION,
                            process_id: std::process::id(),
                            preferred_consumer: if opaque {
                                ConsumerApi::Vulkan
                            } else {
                                ConsumerApi::D3d12
                            },
                        }))
                        .unwrap();
                    let Message::MapOffer(offer) = socket.receive().unwrap() else {
                        panic!("missing frame channel")
                    };
                    let mut frames = FrameSubscriber::from_offer(offer).unwrap();
                    let Message::StreamReady(ready) = socket.receive().unwrap() else {
                        panic!("missing resource publication")
                    };
                    socket.send(Message::StreamAccepted).unwrap();
                    assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
                    sender
                        .send((ready, frames.latest().unwrap().unwrap()))
                        .unwrap();
                    let (lock, wake) = &*release;
                    drop(
                        wake.wait_while(lock.lock().unwrap(), |released| !*released)
                            .unwrap(),
                    );
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            frames,
            release,
            server: Some(server),
        }
    }

    pub fn verify(&mut self, streams: usize) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        let mut surfaces = Vec::new();
        let mut received = (0..streams)
            .map(|_| {
                self.frames
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("hook did not publish both streams")
            })
            .collect::<Vec<_>>();
        received.sort_by_key(|(ready, _)| ready.width);
        for (index, (ready, frame)) in received.into_iter().enumerate() {
            let opaque = std::env::var_os("POLYMER_WSI_OPAQUE").is_some();
            assert_eq!(
                (frame.render_width, frame.render_height),
                (ready.width, ready.height)
            );
            assert_eq!(
                ready.image_kind,
                if opaque {
                    polymer_protocol::ExternalImageKind::VulkanOpaqueWin32
                } else {
                    polymer_protocol::ExternalImageKind::VulkanD3d12Resource
                }
            );
            assert_eq!(
                ready.sync_kind,
                if opaque {
                    polymer_protocol::ExternalSyncKind::VulkanTimelineSemaphore
                } else {
                    polymer_protocol::ExternalSyncKind::D3d12Fence
                }
            );
            assert_eq!(frame.frame_number, 1);
            assert!(frame.timestamp_qpc > 0 && frame.sync_value > 0);
            assert!(
                !surfaces.contains(&ready.surface_id),
                "two windows reused one logical stream"
            );
            surfaces.push(ready.surface_id);
            let graph_fixture = std::env::var_os("POLYMER_WSI_GRAPH").is_some();
            let red = if graph_fixture {
                assert!(
                    matches!((ready.width, ready.height), (32, 48) | (24, 32)),
                    "graph capture included allocation padding: {}x{}",
                    ready.width,
                    ready.height
                );
                ready.width == 32
            } else {
                index == 0
            };
            assert_ne!(ready.consumer_semaphore_handle, 0);
            if opaque {
                crate::opaque::OpaqueViewer::verify(&ready, &frame, red);
            } else {
                let pixels = unsafe { Self::read_pixels(&ready, &frame) };
                let expected = match (ready.format, red) {
                    (87, true) => [0, 0, 255, 255],
                    (28, true) => [255, 0, 0, 255],
                    (87 | 28, false) => [0, 255, 0, 255],
                    _ => panic!("unexpected native fixture format {}", ready.format),
                };
                assert!(
                    pixels.iter().all(|pixel| *pixel == expected),
                    "incorrect Vulkan capture pixels"
                );
            }
            for handle in ready.image_handles[..ready.slot_count as usize]
                .iter()
                .copied()
                .chain([
                    ready.timeline_semaphore_handle,
                    ready.consumer_semaphore_handle,
                ])
                .filter(|handle| *handle != 0)
            {
                unsafe { CloseHandle(HANDLE(handle as *mut _)) }.unwrap();
            }
        }
    }

    pub fn verify_application(&mut self, tracking_enabled: bool) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        let (ready, frame) = self
            .frames
            .recv_timeout(std::time::Duration::from_secs(15))
            .expect("application did not publish a captured frame");
        assert!(frame.frame_number > 0 && frame.sync_value > 0);
        let expected_extent = if tracking_enabled {
            (240, 240)
        } else {
            (320, 240)
        };
        assert_eq!(
            (frame.render_width, frame.render_height),
            expected_extent,
            "vkcube selected a sampled asset instead of its rendered viewport"
        );
        assert_eq!(
            ready.image_kind,
            polymer_protocol::ExternalImageKind::VulkanD3d12Resource
        );
        let pixels = unsafe { Self::read_pixels(&ready, &frame) };
        let distinct = pixels
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert!(
            distinct.len() > 8,
            "application capture is blank or uniform"
        );
        assert_eq!(
            pixels.len(),
            (frame.render_width * frame.render_height) as usize
        );
        if let Some(path) = std::env::var_os("POLYMER_APPLICATION_CAPTURE") {
            let mut bytes =
                format!("P6\n{} {}\n255\n", frame.render_width, frame.render_height).into_bytes();
            for pixel in pixels {
                let rgb = match ready.format {
                    87 => [pixel[2], pixel[1], pixel[0]],
                    28 => [pixel[0], pixel[1], pixel[2]],
                    _ => panic!("unsupported application format"),
                };
                bytes.extend_from_slice(&rgb);
            }
            std::fs::write(path, bytes).unwrap();
        }
        for handle in ready.image_handles[..ready.slot_count as usize]
            .iter()
            .copied()
            .chain([
                ready.timeline_semaphore_handle,
                ready.consumer_semaphore_handle,
            ])
            .filter(|handle| *handle != 0)
        {
            unsafe { CloseHandle(HANDLE(handle as *mut _)) }.unwrap();
        }
        eprintln!(
            "application capture: {}x{}, frame {}, {} distinct colors",
            frame.render_width,
            frame.render_height,
            frame.frame_number,
            distinct.len()
        );
    }

    unsafe fn read_pixels(ready: &StreamReady, frame: &FrameReady) -> Vec<[u8; 4]> {
        use windows::Win32::{Foundation::HANDLE, Graphics::Direct3D12::*};
        unsafe {
            let consumer_device =
                polymer_consumer_d3d12::D3d12ConsumerDevice::new(ready.adapter_luid).unwrap();
            let device = consumer_device.device();
            let mut image: Option<ID3D12Resource> = None;
            let mut producer: Option<ID3D12Fence> = None;
            let mut consumer: Option<ID3D12Fence> = None;
            device
                .OpenSharedHandle(
                    HANDLE(ready.image_handles[frame.slot as usize] as *mut _),
                    &mut image,
                )
                .unwrap();
            device
                .OpenSharedHandle(
                    HANDLE(ready.timeline_semaphore_handle as *mut _),
                    &mut producer,
                )
                .unwrap();
            device
                .OpenSharedHandle(
                    HANDLE(ready.consumer_semaphore_handle as *mut _),
                    &mut consumer,
                )
                .unwrap();
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })
                .unwrap();
            queue.Wait(&producer.unwrap(), frame.sync_value).unwrap();
            let pixels = polymer_presentation::producer::testing::D3d12Readback::pixels(
                device,
                &queue,
                &image.unwrap(),
                frame.render_width,
                frame.render_height,
            );
            consumer.unwrap().Signal(frame.sync_value).unwrap();
            pixels
        }
    }
}

impl Drop for WsiViewer {
    fn drop(&mut self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
        if !std::thread::panicking() {
            self.server.take().unwrap().join().unwrap();
        }
    }
}
