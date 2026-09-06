use super::*;
use windows::Win32::Graphics::{Direct3D11::D3D11_BIND_RENDER_TARGET, Dxgi::DXGI_PRESENT};
#[test]
fn exports_two_unequal_padded_sources_as_independent_streams() {
    Scenario::capture(false);
}

#[test]
fn rejecting_one_native_stream_does_not_disable_its_sibling() {
    Scenario::capture(true);
}

struct Scenario;

impl Scenario {
    fn capture(reject_first: bool) {
        let _test_lock = test_lock();
        let session = format!(
            "polymer-d3d11-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let listener = LocalSocketListener::bind(&session).unwrap();
        unsafe { std::env::set_var(polymer_hook::config::SESSION, &session) };
        unsafe { std::env::set_var(polymer_hook::config::TRACK_RENDER_TARGET, "1") };
        unsafe { std::env::set_var(polymer_hook::config::GRAPH_TRACKING, "1") };
        unsafe { std::env::set_var(polymer_hook::config::TRACK_RENDER_TARGETS, "0,1") };
        let (message_sender, message_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for index in 0..2 {
                let mut socket = listener.accept().unwrap();
                socket
                    .send(Message::Hello(Handshake {
                        protocol_version: PROTOCOL_VERSION,
                        process_id: std::process::id(),
                        preferred_consumer: polymer_protocol::ConsumerApi::Vulkan,
                    }))
                    .unwrap();
                let Message::MapOffer(offer) = socket.receive().unwrap() else {
                    panic!("capture did not offer its frame channel");
                };
                let mut frames = FrameSubscriber::from_offer(offer).unwrap();
                let ready = socket.receive().unwrap();
                if reject_first && index == 0 {
                    // Closing after publication is the wire protocol's rejection.
                    let _ = message_sender.send((ready, None));
                    continue;
                }
                socket.send(Message::StreamAccepted).unwrap();
                let Message::FrameAvailable = socket.receive().unwrap() else {
                    panic!("capture did not signal a shared frame");
                };
                let frame = Message::FrameReady(frames.latest().unwrap().unwrap());
                let _ = message_sender.send((ready, Some(frame)));
            }
        });

        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let title = "polymer-d3d11-test\0".encode_utf16().collect::<Vec<_>>();
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                64,
                64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        assert!(!window.is_null());
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: 64,
                Height: 64,
                RefreshRate: DXGI_RATIONAL {
                    Numerator: 0,
                    Denominator: 1,
                },
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED,
                Scaling: DXGI_MODE_SCALING_UNSPECIFIED,
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: windows::Win32::Foundation::HWND(window),
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
            Flags: 0,
        };
        assert!(unsafe { polymer_hook_dxgi::install_factory_detours() });
        assert_eq!(unsafe { PolymerInstallD3d11Hook() }, 0);
        let factory =
            unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.expect("CreateDXGIFactory1 failed");
        let adapter = unsafe { factory.EnumAdapters(0) }.expect("DXGI did not return an adapter");
        let mut device = std::ptr::null_mut();
        let mut context = std::ptr::null_mut();
        let mut level = 0;
        let result = unsafe {
            install::system_create_device(
                adapter.as_raw(),
                0,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                0,
                7,
                &mut device,
                &mut level,
                &mut context,
            )
        };
        assert!(result >= 0, "D3D11CreateDevice failed: {result:#x}");
        assert!(!device.is_null());
        let device = unsafe { ID3D11Device::from_raw(device) };
        let mut swapchain = None;
        unsafe {
            factory
                .CreateSwapChain(&device, &raw const desc, &mut swapchain)
                .ok()
                .expect("IDXGIFactory1::CreateSwapChain failed")
        };
        let swapchain = swapchain.expect("DXGI did not return a swapchain");
        let buffer: ID3D11Texture2D =
            unsafe { swapchain.GetBuffer(0) }.expect("DXGI did not return a backbuffer");
        let mut target: Option<ID3D11RenderTargetView> = None;
        unsafe { device.CreateRenderTargetView(&buffer, None, Some(&mut target)) }
            .expect("D3D11 did not create a render target");
        let target = target.expect("D3D11 returned a null render target");
        let context_ref = unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }
            .expect("D3D11 returned a null context");
        unsafe { context_ref.ClearRenderTargetView(&target, &[0.25, 0.5, 0.75, 1.0]) };
        let mut recording = None;
        unsafe { device.CreateDeferredContext(0, Some(&mut recording)) }.unwrap();
        let recording = recording.unwrap();

        for (x, y, width, height, destination_x) in [(8, 0, 32, 64, 0), (4, 3, 24, 32, 32)] {
            let mut texture = None;
            unsafe {
                device
                    .CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: 64,
                            Height: 64,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                            ..Default::default()
                        },
                        None,
                        Some(&mut texture),
                    )
                    .unwrap();
            }
            let texture: ID3D11Texture2D = texture.unwrap();
            let mut view = None;
            unsafe {
                device
                    .CreateRenderTargetView(&texture, None, Some(&mut view))
                    .unwrap();
            }
            unsafe {
                recording.ClearRenderTargetView(&view.unwrap(), &[0.25, 0.5, 0.75, 1.0]);
            }
            let crop = D3D11_BOX {
                left: x,
                top: y,
                front: 0,
                right: x + width,
                bottom: y + height,
                back: 1,
            };
            unsafe {
                recording.CopySubresourceRegion(
                    &buffer,
                    0,
                    destination_x,
                    0,
                    0,
                    &texture,
                    0,
                    Some(&crop),
                );
            }
        }
        unsafe {
            let mut list = None;
            recording.FinishCommandList(false, Some(&mut list)).unwrap();
            context_ref.ExecuteCommandList(list.as_ref().unwrap(), reject_first);
            swapchain.Present(0, DXGI_PRESENT(0)).ok().unwrap();
        }
        let mut surfaces = Vec::new();
        for (index, expected) in [(32, 64), (24, 32)].into_iter().enumerate() {
            let (Message::StreamReady(ready), frame) = message_receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
            else {
                panic!("missing independent stream");
            };
            assert_eq!((ready.width, ready.height), expected);
            if reject_first && index == 0 {
                assert!(frame.is_none());
            } else {
                let Some(Message::FrameReady(frame)) = frame else {
                    panic!("missing sibling frame")
                };
                assert_eq!((frame.render_width, frame.render_height), expected);
                assert_eq!(frame.frame_number, 1);
                assert_eq!(frame.sync_value, 1);
            }
            surfaces.push(ready.surface_id);
            for handle in ready.image_handles[..ready.slot_count as usize]
                .iter()
                .copied()
                .chain([
                    ready.timeline_semaphore_handle,
                    ready.consumer_semaphore_handle,
                ])
                .filter(|handle| *handle != 0)
            {
                unsafe {
                    CloseHandle(handle as _);
                }
            }
        }
        assert_ne!(surfaces[0], surfaces[1]);
        drop(target);
        drop(buffer);
        drop(swapchain);
        unsafe {
            release(context);
            DestroyWindow(window);
            std::env::remove_var(polymer_hook::config::SESSION);
            std::env::remove_var(polymer_hook::config::TRACK_RENDER_TARGET);
            std::env::remove_var(polymer_hook::config::TRACK_RENDER_TARGETS);
            std::env::remove_var(polymer_hook::config::GRAPH_TRACKING);
        }
    }
}
