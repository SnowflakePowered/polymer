//! Opt-in comparison fixture using the installed FFplay application.
use polymer_ipc::{LocalSocketListener, Message, StreamReady, frame::FrameSubscriber};
use polymer_platform_windows::{InjectionLibrary, TargetProcess};
use polymer_protocol::{ConsumerApi, FrameReady, Handshake, PROTOCOL_VERSION};
use std::{path::PathBuf, sync::mpsc, time::Duration};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, LUID},
    Graphics::{Direct3D::D3D_FEATURE_LEVEL_11_0, Direct3D12::*, Dxgi::*},
};

struct ApplicationFrame;
impl ApplicationFrame {
    unsafe fn pixels(ready: &StreamReady, frame: &FrameReady) -> Vec<[u8; 4]> {
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapterByLuid(LUID {
                    LowPart: ready.adapter_luid.low_part,
                    HighPart: ready.adapter_luid.high_part,
                })
                .unwrap();
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device).unwrap();
            let device = device.unwrap();
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
                &device,
                &queue,
                &image.unwrap(),
                frame.render_width,
                frame.render_height,
            );
            consumer.unwrap().Signal(frame.sync_value).unwrap();
            pixels
        }
    }

    fn receive(listener: LocalSocketListener) -> (FrameReady, u32, Vec<[u8; 4]>) {
        let mut socket = listener.accept().unwrap();
        socket
            .send(Message::Hello(Handshake {
                protocol_version: PROTOCOL_VERSION,
                process_id: std::process::id(),
                preferred_consumer: ConsumerApi::D3d12,
            }))
            .unwrap();
        let Message::MapOffer(offer) = socket.receive().unwrap() else {
            panic!("missing frame channel")
        };
        let mut frames = FrameSubscriber::from_offer(offer).unwrap();
        let Message::StreamReady(ready) = socket.receive().unwrap() else {
            panic!("missing stream")
        };
        assert_eq!(
            ready.image_kind,
            polymer_protocol::ExternalImageKind::OpenGlD3d12Resource
        );
        socket.send(Message::StreamAccepted).unwrap();
        loop {
            assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
            let frame = frames.latest().unwrap().unwrap();
            let pixels = unsafe { Self::pixels(&ready, &frame) };
            let colors = pixels
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len();
            // SDL can swap an initial cleared window before presenting video.
            if colors < 6 {
                continue;
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
                "FFplay capture: {}x{}, frame {}, {} colors, bottom_up={}, mapping={:?}",
                frame.render_width,
                frame.render_height,
                frame.frame_number,
                colors,
                ready.bottom_up,
                frame.presentation_transform
            );
            return (frame, ready.format, pixels);
        }
    }
}

#[test]
#[ignore = "requires FFplay, built OpenGL hook/injector, and private POLYMER_SESSION"]
fn external_opengl_application_publishes_importable_pixels() {
    let session = std::env::var("POLYMER_SESSION").unwrap();
    let application = std::env::var("POLYMER_APPLICATION_PATH").unwrap();
    let layer = PathBuf::from(std::env::var_os("POLYMER_APPLICATION_LAYER_PATH").unwrap());
    let artifacts = PathBuf::from(std::env::var_os("POLYMER_APPLICATION_ARTIFACTS").unwrap());
    let listener = LocalSocketListener::bind(&session).unwrap();
    let (sender, receiver) = mpsc::channel();
    let viewer = std::thread::spawn(move || {
        sender.send(ApplicationFrame::receive(listener)).unwrap();
    });
    let command = [
        application.as_str(),
        "-hide_banner",
        "-loglevel",
        "warning",
        "-autoexit",
        "-an",
        "-f",
        "lavfi",
        "-i",
        "smptebars=size=320x240:rate=30:duration=4",
        "-x",
        "320",
        "-y",
        "240",
        "-noborder",
    ]
    .map(str::to_owned);
    let process = TargetProcess::launch(
        &command,
        &artifacts,
        &[("SDL_RENDER_DRIVER".into(), "opengl".into())],
        Some(&session),
        &[InjectionLibrary::new(
            "OpenGL comparison",
            &layer,
            c"PolymerInstallOpenGlHook",
        )],
    )
    .unwrap();
    eprintln!("Injected OpenGL comparison layer: {}", layer.display());
    let (frame, format, pixels) = receiver
        .recv_timeout(Duration::from_secs(15))
        .expect("FFplay published no nonuniform frame");
    if let Some(path) = std::env::var_os("POLYMER_APPLICATION_CAPTURE") {
        let mut ppm =
            format!("P6\n{} {}\n255\n", frame.render_width, frame.render_height).into_bytes();
        for pixel in pixels {
            let rgb = match format {
                87 => [pixel[2], pixel[1], pixel[0]],
                28 => [pixel[0], pixel[1], pixel[2]],
                _ => panic!("unsupported format {format}"),
            };
            ppm.extend_from_slice(&rgb);
        }
        std::fs::write(path, ppm).unwrap();
    }
    assert_eq!((frame.render_width, frame.render_height), (320, 240));
    viewer.join().unwrap();
    process.terminate_tree_and_wait().unwrap();
}
