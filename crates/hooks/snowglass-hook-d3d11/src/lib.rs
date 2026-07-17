//! `d3d11.dll` proxy and D3D11 device bootstrap.
#![allow(unsafe_code)]

mod capture;
mod device;
mod hook_chain;
mod hooks;
mod install;
mod loader;
mod sys;
mod tracking;

pub use hooks::{D3D11CreateDevice, D3D11CreateDeviceAndSwapChain};
pub use sys::SnowglassInstallD3d11Hook;

pub(crate) fn d3d11_log(message: impl std::fmt::Display) {
    use std::io::Write;
    eprintln!("{message}");
    let Ok(path) = std::env::var("SNOWGLASS_D3D11_LOG_FILE") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}

pub(crate) fn register_hooks() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        hooks::register_create_device(device::create_device);
        hooks::register_create_device_and_swapchain(device::create_device_and_swapchain);
        let _ = snowglass_hook_dxgi::set_callbacks(snowglass_hook_dxgi::SwapchainCallbacks {
            before_present: Some(capture::before_present),
            before_resize: Some(capture::before_resize),
            after_resize: None,
            destroyed: Some(capture::destroyed),
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_ipc::{LocalSocketListener, Message};
    use snowglass_protocol::{Handshake, PROTOCOL_VERSION};
    use std::ffi::c_void;
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_DESC, DXGI_MODE_SCALING_UNSPECIFIED,
        DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
    };
    use windows::{
        Win32::Graphics::{
            Direct3D11::{
                ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
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

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner())
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
    fn proxies_system_create_device() {
        let _test_lock = test_lock();
        let mut device = std::ptr::null_mut();
        let mut context = std::ptr::null_mut();
        let mut level = 0;
        let result = unsafe {
            D3D11CreateDevice(
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
    fn successful_call_with_null_optional_outputs_is_ignored() {
        let _test_lock = test_lock();
        unsafe {
            device::create_device(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            );
            device::create_device_and_swapchain(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            );
        }
    }

    #[test]
    fn capture_rejects_null_swapchain() {
        let _test_lock = test_lock();
        let result = unsafe {
            snowglass_producer_d3d11::D3d11SwapchainCapture::new(std::ptr::null_mut(), 3)
        };
        assert!(result.is_err());
    }

    #[test]
    fn early_detour_installation_is_idempotent() {
        let _test_lock = test_lock();
        assert_eq!(unsafe { SnowglassInstallD3d11Hook() }, 0);
        assert_eq!(unsafe { SnowglassInstallD3d11Hook() }, 0);
    }

    #[test]
    fn captures_and_imports_a_real_hardware_swapchain_present() {
        let _test_lock = test_lock();
        let session = format!(
            "snowglass-d3d11-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let listener = LocalSocketListener::bind(&session).unwrap();
        unsafe { std::env::set_var("SNOWGLASS_SESSION", &session) };
        let (message_sender, message_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let mut socket = listener.accept().unwrap();
                socket
                    .send(Message::Hello(Handshake {
                        protocol_version: PROTOCOL_VERSION,
                        process_id: std::process::id(),
                    }))
                    .unwrap();
                let ready = socket.receive().unwrap();
                let frame = socket.receive().unwrap();
                let _ = message_sender.send((ready, frame));
            }
        });

        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let title = "snowglass-d3d11-test\0".encode_utf16().collect::<Vec<_>>();
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
        assert_eq!(unsafe { SnowglassInstallD3d11Hook() }, 0);
        let factory =
            unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.expect("CreateDXGIFactory1 failed");
        let adapter = unsafe { factory.EnumAdapters(0) }.expect("DXGI did not return an adapter");
        let mut device = std::ptr::null_mut();
        let mut context = std::ptr::null_mut();
        let mut level = 0;
        let result = unsafe {
            D3D11CreateDevice(
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

        let (ready, frame) = message_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("D3D11 capture did not publish a frame");
        let Message::StreamReady(ready) = ready else {
            panic!("expected StreamReady, got {ready:?}");
        };
        assert_eq!(ready.width, 64);
        assert_eq!(ready.height, 64);
        assert_eq!(ready.slot_count, 3);
        assert!(ready.image_handles[..3].iter().all(|handle| *handle != 0));
        assert_ne!(ready.timeline_semaphore_handle, 0);
        let Message::FrameReady(frame) = frame else {
            panic!("expected FrameReady, got {frame:?}");
        };
        assert_eq!(frame.render_width, 64);
        assert_eq!(frame.render_height, 64);
        assert_eq!(frame.sync_value, 1);

        let (import_sender, import_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = import_sender.send(("importing", None));
            let mut imported = match snowglass_consumer_vulkan::ImportedStream::import(&ready) {
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
            ("read", Some(format!("{}:[64, 127, 191, 255]", 64 * 64 * 4))),
            "{read:?}"
        );

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
            std::env::remove_var("SNOWGLASS_SESSION");
        }
    }
}
