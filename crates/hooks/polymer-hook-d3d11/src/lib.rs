//! D3D11 export detours and COM device bootstrap.
#![allow(unsafe_code)]

mod device;
mod hooks;
mod install;
mod sys;
mod vtable;

pub use sys::{PolymerInstallD3d11Hook, PolymerUninstallD3d11Hook};

#[cfg(test)]
mod tests {
    use parking_lot::{Mutex, MutexGuard};

    use super::*;
    use polymer_ipc::{LocalSocketListener, Message, frame::FrameSubscriber};
    use polymer_protocol::{Handshake, PROTOCOL_VERSION};
    use std::ffi::c_void;
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_DESC, DXGI_MODE_SCALING_UNSPECIFIED,
        DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
    };
    use windows::{
        Win32::Graphics::{
            Direct3D11::{
                D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
                D3D11_VIEWPORT, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView,
                ID3D11ShaderResourceView, ID3D11Texture2D,
            },
            Dxgi::{
                CreateDXGIFactory1, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_DISCARD,
                DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory1,
            },
        },
        core::Interface,
    };
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CW_USEDEFAULT, CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW,
        },
    };

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_lock() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock()
    }

    unsafe fn release(object: *mut c_void) {
        if object.is_null() {
            return;
        }
        let vtable = unsafe { *object.cast::<*const usize>() };
        let release = unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void) -> u32>(
                *vtable.add(2),
            )
        };
        unsafe { release(object) };
    }

    #[test]
    fn detours_system_create_device() {
        let _test_lock = test_lock();
        let mut device = std::ptr::null_mut();
        let mut context = std::ptr::null_mut();
        let mut level = 0;
        let result = unsafe {
            install::system_create_device(
                std::ptr::null_mut(),
                5,
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
        assert!(!context.is_null());
        unsafe {
            release(context);
            release(device);
        }
    }

    #[test]
    fn capture_rejects_null_swapchain() {
        let _test_lock = test_lock();
        let result = unsafe {
            polymer_producer_d3d11::D3d11SwapchainCapture::new(
                std::ptr::null_mut(),
                3,
                polymer_producer_d3d11::D3d11RingMode::KeyedMutex,
                None,
            )
        };
        assert!(result.is_err());
    }

    #[test]
    fn early_detour_installation_is_idempotent() {
        let _test_lock = test_lock();
        assert!(unsafe { polymer_hook_dxgi::install_factory_detours() });
        assert_eq!(unsafe { PolymerInstallD3d11Hook() }, 0);
        assert_eq!(unsafe { PolymerInstallD3d11Hook() }, 0);
    }

    #[test]
    fn captures_and_imports_a_real_hardware_swapchain_present() {
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
        let (message_sender, message_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..2 {
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
                let Message::FrameAvailable = socket.receive().unwrap() else {
                    panic!("capture did not signal a shared frame");
                };
                let frame = Message::FrameReady(frames.latest().unwrap().unwrap());
                let _ = message_sender.send((ready, frame));
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
        let mut presentation_source = None;
        unsafe {
            device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: 48,
                    Height: 64,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                },
                None,
                Some(&mut presentation_source),
            )
        }
        .expect("D3D11 did not create the presentation source");
        let presentation_source =
            presentation_source.expect("D3D11 returned a null presentation source");
        let source_box = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: 48,
            bottom: 64,
            back: 1,
        };
        unsafe {
            context_ref.CopySubresourceRegion(
                &presentation_source,
                0,
                0,
                0,
                0,
                &buffer,
                0,
                Some(&raw const source_box),
            )
        };
        let mut presentation_view: Option<ID3D11ShaderResourceView> = None;
        unsafe {
            device.CreateShaderResourceView(
                &presentation_source,
                None,
                Some(&mut presentation_view),
            )
        }
        .expect("D3D11 did not create the presentation source view");
        let presentation_view =
            presentation_view.expect("D3D11 returned a null presentation source view");
        unsafe {
            context_ref.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            context_ref.PSSetShaderResources(0, Some(&[Some(presentation_view)]));
            context_ref.RSSetViewports(Some(&[D3D11_VIEWPORT {
                Width: 64.0,
                Height: 64.0,
                MaxDepth: 1.0,
                ..Default::default()
            }]));
            vtable::device_tracking(device.as_raw())
                .expect("D3D11 context was not associated with its tracker")
                .record_context_draw(context, None);
            // The test intentionally omits shaders; restore deterministic
            // pixels after recording while retaining the tracking data.
            context_ref.ClearRenderTargetView(&target, &[0.25, 0.5, 0.75, 1.0]);
            context_ref.OMSetRenderTargets(None, None);
        }
        let swapchain_raw = swapchain.as_raw();
        let vtable = unsafe { *swapchain_raw.cast::<*const usize>() };
        let present = unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void, u32, u32) -> i32>(
                *vtable.add(8),
            )
        };
        let present_result = unsafe { present(swapchain_raw, 0, 0) };
        assert!(present_result >= 0, "Present failed: {present_result:#x}");

        let (ready, frame) = message_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("D3D11 capture did not publish a frame");
        let Message::StreamReady(ready) = ready else {
            panic!("expected StreamReady, got {ready:?}");
        };
        assert_eq!(ready.width, 48);
        assert_eq!(ready.height, 64);
        assert_eq!(ready.slot_count, 3);
        assert!(ready.image_handles[..3].iter().all(|handle| *handle != 0));
        assert_ne!(ready.timeline_semaphore_handle, 0);
        let Message::FrameReady(frame) = frame else {
            panic!("expected FrameReady, got {frame:?}");
        };
        assert_eq!((frame.render_width, frame.render_height), (48, 64));
        assert_eq!((frame.capture_x, frame.capture_y), (0, 0));
        assert_eq!((frame.capture_width, frame.capture_height), (64, 64));
        assert_eq!(
            (frame.presentation_width, frame.presentation_height),
            (64, 64)
        );
        assert_eq!(
            frame.presentation_transform,
            polymer_protocol::PresentationTransform::axis_aligned(0, 0, 64, 64)
        );
        assert_eq!(frame.sync_value, 1);

        let (import_sender, import_receiver) = std::sync::mpsc::channel();
        let import_thread = std::thread::spawn(move || {
            let _ = import_sender.send(("importing", None));
            let mut imported = match polymer_consumer_vulkan::ImportedStream::import(&ready) {
                Ok(imported) => imported,
                Err(error) => {
                    let _ = import_sender.send(("import failed", Some(error.to_string())));
                    return;
                }
            };
            let _ = import_sender.send(("imported", None));
            match imported.read_frame_rgba(frame.slot, frame.sync_value) {
                Ok(pixels) => {
                    let _ = import_sender.send((
                        "read",
                        Some(format!(
                            "{}:{:?}",
                            pixels.len(),
                            pixels.get(..4).unwrap_or_default()
                        )),
                    ));
                }
                Err(error) => {
                    let _ = import_sender.send(("read failed", Some(error.to_string())));
                }
            }
            // leak the imported stream to prevent crashes
            std::mem::forget(imported);
        });
        assert_eq!(
            import_receiver.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(("importing", None))
        );
        let imported = import_receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("Vulkan D3D11 resource import stalled");
        assert_eq!(imported, ("imported", None), "{imported:?}");
        let read = import_receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("Vulkan D3D11 readback stalled");
        assert_eq!(
            read,
            ("read", Some(format!("{}:[64, 127, 191, 255]", 48 * 64 * 4))),
            "{read:?}"
        );
        import_thread.join().expect("Vulkan import thread panicked");

        drop(target);
        drop(buffer);
        let vtable = unsafe { *swapchain_raw.cast::<*const usize>() };
        let resize = unsafe {
            std::mem::transmute::<
                usize,
                unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32, u32) -> i32,
            >(*vtable.add(13))
        };
        let resize_result = unsafe { resize(swapchain_raw, 0, 80, 48, 0, 0) };
        assert!(
            resize_result >= 0,
            "ResizeBuffers failed: {resize_result:#x}"
        );
        let present_result = unsafe { present(swapchain_raw, 0, 0) };
        assert!(present_result >= 0, "Present failed: {present_result:#x}");
        let (ready, frame) = message_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("resized D3D11 capture did not publish a frame");
        let Message::StreamReady(ready) = ready else {
            panic!("expected resized StreamReady, got {ready:?}");
        };
        let Message::FrameReady(frame) = frame else {
            panic!("expected resized FrameReady, got {frame:?}");
        };
        assert_eq!((ready.width, ready.height), (80, 48));
        assert_eq!((frame.render_width, frame.render_height), (80, 48));
        assert_eq!(frame.sync_value, 1);
        for handle in ready.image_handles[..ready.slot_count as usize]
            .iter()
            .copied()
        {
            unsafe { CloseHandle(handle as _) };
        }
        unsafe { CloseHandle(ready.timeline_semaphore_handle as _) };

        unsafe {
            release(context);
            DestroyWindow(window);
            std::env::remove_var(polymer_hook::config::SESSION);
            std::env::remove_var(polymer_hook::config::TRACK_RENDER_TARGET);
            std::env::remove_var(polymer_hook::config::GRAPH_TRACKING);
        }
    }

    #[test]
    fn negotiates_a_fence_ring_for_a_d3d12_consumer() {
        use polymer_consumer_d3d12::D3d12ConsumerDevice;
        use polymer_protocol::{ConsumerApi, ExternalImageKind, ExternalSyncKind};

        let _test_lock = test_lock();
        let session = format!(
            "polymer-d3d11-d3d12-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let listener = LocalSocketListener::bind(&session).unwrap();
        unsafe { std::env::set_var(polymer_hook::config::SESSION, &session) };
        let (message_sender, message_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut socket = listener.accept().unwrap();
            socket
                .send(Message::Hello(Handshake {
                    protocol_version: PROTOCOL_VERSION,
                    process_id: std::process::id(),
                    preferred_consumer: ConsumerApi::D3d12,
                }))
                .unwrap();
            let Message::MapOffer(offer) = socket.receive().unwrap() else {
                panic!("capture did not offer its frame channel");
            };
            let mut frames = FrameSubscriber::from_offer(offer).unwrap();
            while let Ok(message) = socket.receive() {
                let message = if matches!(message, Message::FrameAvailable) {
                    Message::FrameReady(frames.latest().unwrap().unwrap())
                } else {
                    message
                };
                if message_sender.send(message).is_err() {
                    break;
                }
            }
        });

        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let title = "polymer-d3d11-d3d12-test\0"
            .encode_utf16()
            .collect::<Vec<_>>();
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
        let swapchain_raw = swapchain.as_raw();
        let vtable = unsafe { *swapchain_raw.cast::<*const usize>() };
        let present = unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void, u32, u32) -> i32>(
                *vtable.add(8),
            )
        };
        let present_result = unsafe { present(swapchain_raw, 0, 0) };
        assert!(present_result >= 0, "Present failed: {present_result:#x}");

        let ready = message_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("D3D11 capture did not publish a stream");
        let Message::StreamReady(ready) = ready else {
            panic!("expected StreamReady, got {ready:?}");
        };
        assert_eq!(ready.image_kind, ExternalImageKind::D3d11D3d12Resource);
        assert_eq!(ready.sync_kind, ExternalSyncKind::D3d12Fence);
        assert!(
            ready.adapter_luid.low_part != 0 || ready.adapter_luid.high_part != 0,
            "fence-mode stream must report its adapter LUID"
        );
        assert_ne!(ready.consumer_semaphore_handle, 0);
        assert_eq!(ready.slot_count, 3);
        let frame = message_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("D3D11 capture did not publish a frame");
        let Message::FrameReady(frame) = frame else {
            panic!("expected FrameReady, got {frame:?}");
        };
        assert_eq!(frame.slot, 0);
        assert_eq!(frame.sync_value, 1);

        let consumer =
            D3d12ConsumerDevice::new(ready.adapter_luid).expect("D3D12 consumer device failed");
        let imported = consumer
            .import(&ready)
            .expect("D3D12 import of the D3D11 fence ring failed");
        imported
            .wait_on_queue(consumer.queue(), frame.sync_value)
            .expect("D3D12 wait on the producer fence failed");
        let pixel = unsafe {
            read_back_pixel(
                consumer.device(),
                consumer.queue(),
                imported
                    .resource(frame.slot)
                    .expect("missing slot resource"),
                64,
                64,
            )
        };
        // B8G8R8A8 layout of the cleared (0.25, 0.5, 0.75, 1.0) color.
        assert_eq!(pixel, [191, 127, 64, 255]);

        // The remaining two slots are still unused, so presents keep flowing
        // without any consumer release.
        for expected_slot in [1u32, 2u32] {
            let present_result = unsafe { present(swapchain_raw, 0, 0) };
            assert!(present_result >= 0, "Present failed: {present_result:#x}");
            let message = message_receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("D3D11 capture did not publish a follow-up frame");
            let Message::FrameReady(frame) = message else {
                panic!("expected FrameReady, got {message:?}");
            };
            assert_eq!(frame.slot, expected_slot);
        }

        // Every slot is now in flight; capture must defer instead of
        // overwriting a frame the consumer has not released.
        let present_result = unsafe { present(swapchain_raw, 0, 0) };
        assert!(present_result >= 0, "Present failed: {present_result:#x}");
        assert!(
            message_receiver
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_err(),
            "capture published a frame without a released slot"
        );

        // Releasing slot 0 through the consumer fence lets capture resume.
        imported
            .signal_release(consumer.queue(), 1)
            .expect("D3D12 consumer release signal failed");
        let mut resumed = None;
        for _ in 0..100 {
            let present_result = unsafe { present(swapchain_raw, 0, 0) };
            assert!(present_result >= 0, "Present failed: {present_result:#x}");
            if let Ok(message) =
                message_receiver.recv_timeout(std::time::Duration::from_millis(100))
            {
                resumed = Some(message);
                break;
            }
        }
        let Some(Message::FrameReady(frame)) = resumed else {
            panic!("capture did not resume after the consumer release: {resumed:?}");
        };
        assert_eq!(frame.slot, 0);

        drop(imported);
        drop(consumer);
        drop(target);
        drop(buffer);
        unsafe {
            release(context);
            DestroyWindow(window);
            std::env::remove_var(polymer_hook::config::SESSION);
        }
    }

    #[test]
    fn keyed_mutex_ring_supports_a_d3d12_consumer() {
        use polymer_consumer_d3d12::D3d12ConsumerDevice;
        use polymer_ipc::StreamReady;
        use polymer_platform_windows::{ProcessId, duplicate_handle_into_process};
        use polymer_producer_d3d11::{D3d11RingMode, D3d11SwapchainCapture};
        use polymer_protocol::{ExternalImageKind, ExternalSyncKind, MAX_IMAGE_SLOTS};

        let _test_lock = test_lock();
        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let title = "polymer-d3d11-mutex-test\0"
            .encode_utf16()
            .collect::<Vec<_>>();
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

        let mut capture = unsafe {
            D3d11SwapchainCapture::new(
                swapchain.as_raw(),
                3,
                D3d11RingMode::KeyedMutexNoFence,
                None,
            )
        }
        .expect("keyed-mutex-only capture creation failed");
        let submission = unsafe { capture.copy_frame(None, None) }.expect("first copy failed");
        assert_eq!(submission.slot, 0);

        // Hand the ring to a D3D12 consumer the way connect_viewer would.
        let ring = capture.ring();
        assert!(ring.raw_fence_handle().is_none());
        let pid = ProcessId(std::process::id());
        let mut image_handles = [0i64; MAX_IMAGE_SLOTS];
        for (destination, slot) in image_handles.iter_mut().zip(ring.slots()) {
            *destination =
                duplicate_handle_into_process(slot.raw_handle() as isize, pid).unwrap() as i64;
        }
        let adapter_luid = capture.adapter_luid();
        assert!(adapter_luid.low_part != 0 || adapter_luid.high_part != 0);
        let ring_desc = ring.desc();
        let ready = StreamReady {
            producer_process_id: std::process::id(),
            surface_id: polymer_protocol::SurfaceId(capture.source_window() as u64),
            adapter_luid,
            source_window: capture.source_window() as i64,
            width: ring_desc.width,
            height: ring_desc.height,
            format: ring_desc.format,
            image_kind: ExternalImageKind::D3d11SharedResourceNt,
            sync_kind: ExternalSyncKind::D3d11KeyedMutex,
            slot_count: ring_desc.slots,
            image_handles,
            timeline_semaphore_handle: 0,
            consumer_semaphore_handle: 0,
            bottom_up: false,
        };
        let consumer = D3d12ConsumerDevice::new(adapter_luid).expect("D3D12 consumer failed");
        let imported = consumer
            .import(&ready)
            .expect("D3D12 import of the keyed-mutex ring failed");
        assert!(imported.uses_keyed_mutex());

        imported
            .acquire_slot(0, 2000)
            .expect("D3D12 keyed-mutex acquire failed");
        let pixel = unsafe {
            read_back_pixel(
                consumer.device(),
                consumer.queue(),
                imported.resource(0).expect("missing slot resource"),
                64,
                64,
            )
        };
        assert_eq!(pixel, [191, 127, 64, 255]);
        imported
            .release_slot(0)
            .expect("D3D12 keyed-mutex release failed");

        // Slots 1 and 2 are virgin, and the consumer released slot 0 back to
        // the producer, so three more copies succeed.
        for expected_slot in [1u32, 2, 0] {
            let submission = unsafe { capture.copy_frame(None, None) }.expect("copy failed");
            assert_eq!(submission.slot, expected_slot);
        }
        // Slot 1 is still released to the consumer; without a consumer
        // release the producer must defer instead of overwriting it.
        assert!(unsafe { capture.copy_frame(None, None) }.is_err());
        // Recycling the skipped slot lets capture resume.
        imported.recycle_slot(1);
        let submission =
            unsafe { capture.copy_frame(None, None) }.expect("copy after recycle failed");
        assert_eq!(submission.slot, 1);

        drop(imported);
        drop(consumer);
        drop(capture);
        drop(target);
        drop(buffer);
        drop(swapchain);
        unsafe {
            release(context);
            DestroyWindow(window);
        }
    }

    /// Copies the first pixel of `source` into CPU memory through a readback buffer.
    unsafe fn read_back_pixel(
        device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
        queue: &windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue,
        source: &windows::Win32::Graphics::Direct3D12::ID3D12Resource,
        width: u32,
        height: u32,
    ) -> [u8; 4] {
        use std::mem::ManuallyDrop;
        use windows::Win32::{
            Foundation::CloseHandle,
            Graphics::Direct3D12::*,
            System::Threading::{CreateEventW, INFINITE, WaitForSingleObject},
        };

        unsafe {
            let allocator: ID3D12CommandAllocator = device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .expect("readback allocator");
            let list: ID3D12GraphicsCommandList = device
                .CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None::<&ID3D12PipelineState>,
                )
                .expect("readback command list");
            let pitch = (width * 4).div_ceil(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT)
                * D3D12_TEXTURE_DATA_PITCH_ALIGNMENT;
            let mut buffer: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_READBACK,
                        ..Default::default()
                    },
                    D3D12_HEAP_FLAG_NONE,
                    &D3D12_RESOURCE_DESC {
                        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                        Width: u64::from(pitch * height),
                        Height: 1,
                        DepthOrArraySize: 1,
                        MipLevels: 1,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                        ..Default::default()
                    },
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    None,
                    &mut buffer,
                )
                .expect("readback buffer");
            let buffer = buffer.expect("D3D12 returned a null readback buffer");
            let mut barrier = D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: ManuallyDrop::new(Some(source.clone())),
                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: D3D12_RESOURCE_STATE_COMMON,
                        StateAfter: D3D12_RESOURCE_STATE_COPY_SOURCE,
                    }),
                },
            };
            list.ResourceBarrier(std::slice::from_ref(&barrier));
            let mut dst = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(buffer.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: 0,
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            Width: width,
                            Height: height,
                            Depth: 1,
                            RowPitch: pitch,
                        },
                    },
                },
            };
            let mut src = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(source.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
            ManuallyDrop::drop(&mut dst.pResource);
            ManuallyDrop::drop(&mut src.pResource);
            {
                let transition = &mut *barrier.Anonymous.Transition;
                std::mem::swap(&mut transition.StateBefore, &mut transition.StateAfter);
            }
            list.ResourceBarrier(std::slice::from_ref(&barrier));
            ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource);
            list.Close().expect("readback list close");
            let command: ID3D12CommandList = list.cast().expect("command list cast");
            queue.ExecuteCommandLists(&[Some(command)]);
            let fence: ID3D12Fence = device
                .CreateFence(0, D3D12_FENCE_FLAG_NONE)
                .expect("readback fence");
            queue.Signal(&fence, 1).expect("readback signal");
            let event = CreateEventW(None, false, false, None).expect("readback event");
            fence
                .SetEventOnCompletion(1, event)
                .expect("readback completion event");
            WaitForSingleObject(event, INFINITE);
            let _ = CloseHandle(event);
            let mut mapped = std::ptr::null_mut();
            buffer
                .Map(0, None, Some(&mut mapped))
                .expect("readback map");
            let mut pixel = [0u8; 4];
            std::ptr::copy_nonoverlapping(mapped.cast::<u8>(), pixel.as_mut_ptr(), 4);
            buffer.Unmap(0, None);
            pixel
        }
    }
}
