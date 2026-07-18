//! D3D12 export detours, DXGI ownership, and swapchain capture.
#![allow(unsafe_code)]

mod hook_chain;
mod hooks;
mod install;
mod sys;
mod vtable;

pub use sys::SnowglassInstallD3d12Hook;

snowglass_hook::register_hooks! {
    hooks::register_create_device -> |device, result| {
        if result >= 0 {
            d3d12_log("snowglass: intercepted D3D12CreateDevice");
            unsafe { vtable::install_device(device) };
        }
    };
    snowglass_hook_dxgi::register_callbacks -> snowglass_hook_dxgi::SwapchainCallbacks {
        claim: snowglass_producer_d3d12::claim,
        before_present: Some(snowglass_producer_d3d12::before_present),
        before_resize: Some(snowglass_producer_d3d12::before_resize),
        after_resize: None,
        destroyed: Some(snowglass_producer_d3d12::destroyed),
    };
}

fn d3d12_log(message: impl std::fmt::Display) {
    use std::io::Write;
    eprintln!("{message}");
    if let Ok(path) = std::env::var("SNOWGLASS_D3D12_LOG_FILE")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_ipc::{LocalSocketListener, Message};
    use snowglass_protocol::{Handshake, PROTOCOL_VERSION};
    use std::mem::ManuallyDrop;
    use windows::{
        Win32::{
            Foundation::HWND,
            Graphics::{
                Direct3D::D3D_FEATURE_LEVEL_11_0,
                Direct3D12::*,
                Dxgi::{
                    Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT,
                    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
                    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory4,
                    IDXGISwapChain3,
                },
            },
        },
        core::Interface,
    };
    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CW_USEDEFAULT, CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW,
        },
    };

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn transition(
        resource: &ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(Some(resource.clone())),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }

    unsafe fn record_barriers<const N: usize>(
        list: &ID3D12GraphicsCommandList,
        mut barriers: [D3D12_RESOURCE_BARRIER; N],
    ) {
        unsafe {
            list.ResourceBarrier(&barriers);
            for barrier in &mut barriers {
                let transition = &mut *barrier.Anonymous.Transition;
                ManuallyDrop::drop(&mut transition.pResource);
            }
        }
    }

    #[test]
    fn early_detour_installation_is_idempotent() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(unsafe { SnowglassInstallD3d12Hook() }, 0);
        assert_eq!(unsafe { SnowglassInstallD3d12Hook() }, 0);
    }

    #[test]
    fn captures_and_imports_a_real_hardware_swapchain_present() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let session = format!(
            "snowglass-d3d12-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let listener = LocalSocketListener::bind(&session).unwrap();
        unsafe {
            std::env::set_var("SNOWGLASS_SESSION", &session);
            std::env::set_var("SNOWGLASS_TRACK_RENDER_TARGET", "1");
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let mut socket = listener.accept().unwrap();
                socket
                    .send(Message::Hello(Handshake {
                        protocol_version: PROTOCOL_VERSION,
                        process_id: std::process::id(),
                        preferred_consumer: snowglass_protocol::ConsumerApi::D3d12,
                    }))
                    .unwrap();
                let ready = socket.receive().unwrap();
                let frame = socket.receive().unwrap();
                let _ = sender.send((ready, frame));
            }
        });

        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let title = "snowglass-d3d12-test\0".encode_utf16().collect::<Vec<_>>();
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

        unsafe {
            assert!(snowglass_hook_dxgi::install_factory_detours());
            assert_eq!(SnowglassInstallD3d12Hook(), 0);
            let factory = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))
                .expect("CreateDXGIFactory2 failed");
            let adapter = factory.EnumAdapters1(0).expect("DXGI returned no adapter");
            let mut device = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device)
                .expect("D3D12CreateDevice failed");
            let device: ID3D12Device = device.expect("D3D12 returned a null device");
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    Priority: 0,
                    Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                    NodeMask: 0,
                })
                .expect("CreateCommandQueue failed");
            let swapchain = factory
                .CreateSwapChainForHwnd(
                    &queue,
                    HWND(window),
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: 64,
                        Height: 64,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        Stereo: false.into(),
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        AlphaMode: Default::default(),
                        Flags: 0,
                    },
                    None,
                    None,
                )
                .expect("CreateSwapChainForHwnd failed")
                .cast::<IDXGISwapChain3>()
                .unwrap();
            d3d12_log("snowglass-test: swapchain created");

            let allocator: ID3D12CommandAllocator = device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .unwrap();
            let list: ID3D12GraphicsCommandList = device
                .CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None::<&ID3D12PipelineState>,
                )
                .unwrap();
            let heap: ID3D12DescriptorHeap = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: 1,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
                    NodeMask: 0,
                })
                .unwrap();
            let backbuffer: ID3D12Resource = swapchain.GetBuffer(0).unwrap();
            let rtv = heap.GetCPUDescriptorHandleForHeapStart();
            device.CreateRenderTargetView(&backbuffer, None, rtv);
            record_barriers(
                &list,
                [transition(
                    &backbuffer,
                    D3D12_RESOURCE_STATE_PRESENT,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                )],
            );
            list.ClearRenderTargetView(rtv, &[0.25, 0.5, 0.75, 1.0], None);
            list.RSSetViewports(&[D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: 64.0,
                Height: 64.0,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]);
            list.RSSetScissorRects(&[windows::Win32::Foundation::RECT {
                left: 8,
                top: 4,
                right: 56,
                bottom: 52,
            }]);
            snowglass_producer_d3d12::record_draw(list.as_raw());
            record_barriers(
                &list,
                [transition(
                    &backbuffer,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_PRESENT,
                )],
            );
            list.Close().unwrap();
            let command: ID3D12CommandList = list.cast().unwrap();
            queue.ExecuteCommandLists(&[Some(command)]);
            d3d12_log("snowglass-test: application clear submitted");
            swapchain
                .Present(0, DXGI_PRESENT(0))
                .ok()
                .expect("Present failed");
            d3d12_log("snowglass-test: present returned");

            let (ready, frame) = receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("D3D12 capture did not publish a frame");
            d3d12_log("snowglass-test: IPC frame received");
            let Message::StreamReady(ready) = ready else {
                panic!("expected StreamReady, got {ready:?}");
            };
            let Message::FrameReady(frame) = frame else {
                panic!("expected FrameReady, got {frame:?}");
            };
            assert_eq!((ready.width, ready.height), (64, 64));
            assert_ne!(ready.timeline_semaphore_handle, 0);
            assert_ne!(ready.consumer_semaphore_handle, 0);
            assert_ne!(
                (ready.adapter_luid.low_part, ready.adapter_luid.high_part),
                (0, 0)
            );
            assert_eq!(frame.sync_value, 1);
            assert_eq!((frame.capture_x, frame.capture_y), (8, 4));
            assert_eq!((frame.render_width, frame.render_height), (48, 48));

            d3d12_log("snowglass-test: importing D3D12 handles into Vulkan");
            let mut imported = snowglass_consumer_vulkan::ImportedStream::import(&ready)
                .expect("Vulkan could not import D3D12 resources");
            d3d12_log("snowglass-test: D3D12 handles imported into Vulkan");
            let pixels = imported
                .read_frame_rgba(frame.slot, frame.sync_value)
                .expect("Vulkan could not read the D3D12 frame");
            d3d12_log("snowglass-test: Vulkan readback complete");
            let pixel = &pixels[..4];
            assert!((63..=65).contains(&pixel[0]), "red channel: {pixel:?}");
            assert!((127..=129).contains(&pixel[1]), "green channel: {pixel:?}");
            assert!((190..=192).contains(&pixel[2]), "blue channel: {pixel:?}");
            assert_eq!(pixel[3], 255);

            std::mem::forget(imported);
            drop(backbuffer);
            drop(list);
            drop(heap);
            drop(allocator);

            let resize = swapchain.ResizeBuffers(
                2,
                80,
                48,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            );
            assert!(resize.is_ok(), "ResizeBuffers failed: {resize:?}");
            swapchain
                .Present(0, DXGI_PRESENT(0))
                .ok()
                .expect("resized Present failed");
            let (resized_ready, resized_frame) = receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("resized D3D12 capture did not publish a frame");
            let Message::StreamReady(resized_ready) = resized_ready else {
                panic!("expected resized StreamReady, got {resized_ready:?}");
            };
            let Message::FrameReady(resized_frame) = resized_frame else {
                panic!("expected resized FrameReady, got {resized_frame:?}");
            };
            assert_eq!((resized_ready.width, resized_ready.height), (80, 48));
            assert_eq!(
                (resized_frame.render_width, resized_frame.render_height),
                (80, 48)
            );
            assert_eq!(resized_frame.sync_value, 1);
            for handle in resized_ready.image_handles[..resized_ready.slot_count as usize]
                .iter()
                .copied()
                .chain([
                    resized_ready.timeline_semaphore_handle,
                    resized_ready.consumer_semaphore_handle,
                ])
            {
                windows_sys::Win32::Foundation::CloseHandle(handle as _);
            }

            // The validation Vulkan device is intentionally leaked above;
            // keep its same-process D3D12 peer alive for the same reason.
            std::mem::forget(swapchain);
            DestroyWindow(window);
            d3d12_log("snowglass-test: window destroyed");
            std::env::remove_var("SNOWGLASS_SESSION");
        }
    }
}
