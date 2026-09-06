use super::*;
use polymer_ipc::{LocalSocketListener, Message, frame::FrameSubscriber};
use polymer_protocol::{ConsumerApi, Handshake, PROTOCOL_VERSION};
use std::{mem::ManuallyDrop, time::Duration};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND},
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_11_0,
            Direct3D12::*,
            Dxgi::{Common::*, *},
        },
    },
    core::Interface,
};
use windows_sys::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{CW_USEDEFAULT, CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW},
};

struct Scenario;
impl Scenario {
    unsafe fn enhanced_transition(
        list: &ID3D12GraphicsCommandList,
        source: &ID3D12Resource,
        before: D3D12_BARRIER_LAYOUT,
        after: D3D12_BARRIER_LAYOUT,
    ) {
        let access = |layout| match layout {
            D3D12_BARRIER_LAYOUT_RENDER_TARGET => D3D12_BARRIER_ACCESS_RENDER_TARGET,
            D3D12_BARRIER_LAYOUT_COPY_SOURCE => D3D12_BARRIER_ACCESS_COPY_SOURCE,
            _ => D3D12_BARRIER_ACCESS_COMMON,
        };
        let mut barrier = D3D12_TEXTURE_BARRIER {
            SyncBefore: D3D12_BARRIER_SYNC_ALL,
            SyncAfter: D3D12_BARRIER_SYNC_ALL,
            AccessBefore: access(before),
            AccessAfter: access(after),
            LayoutBefore: before,
            LayoutAfter: after,
            pResource: ManuallyDrop::new(Some(source.clone())),
            Subresources: D3D12_BARRIER_SUBRESOURCE_RANGE {
                IndexOrFirstMipLevel: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let group = D3D12_BARRIER_GROUP {
            Type: D3D12_BARRIER_TYPE_TEXTURE,
            NumBarriers: 1,
            Anonymous: D3D12_BARRIER_GROUP_0 {
                pTextureBarriers: &barrier,
            },
        };
        unsafe {
            list.cast::<ID3D12GraphicsCommandList7>()
                .unwrap()
                .Barrier(&[group]);
            ManuallyDrop::drop(&mut barrier.pResource);
        }
    }
    unsafe fn verify_pixels(
        device: &ID3D12Device,
        queue: &ID3D12CommandQueue,
        ready: &polymer_ipc::StreamReady,
        frame: &polymer_protocol::FrameReady,
        expected: [u8; 4],
    ) {
        unsafe {
            let mut image: Option<ID3D12Resource> = None;
            device
                .OpenSharedHandle(
                    HANDLE(ready.image_handles[frame.slot as usize] as *mut _),
                    &mut image,
                )
                .unwrap();
            let mut producer: Option<ID3D12Fence> = None;
            device
                .OpenSharedHandle(
                    HANDLE(ready.timeline_semaphore_handle as *mut _),
                    &mut producer,
                )
                .unwrap();
            queue.Wait(&producer.unwrap(), frame.sync_value).unwrap();
            let pixels = polymer_presentation::producer::testing::D3d12Readback::pixels(
                device,
                queue,
                &image.unwrap(),
                frame.render_width,
                frame.render_height,
            );
            assert!(
                pixels.iter().all(|pixel| *pixel == expected),
                "captured source or crop differs: expected {expected:?}, found {:?}",
                pixels.iter().find(|pixel| **pixel != expected)
            );
            let mut consumer: Option<ID3D12Fence> = None;
            device
                .OpenSharedHandle(
                    HANDLE(ready.consumer_semaphore_handle as *mut _),
                    &mut consumer,
                )
                .unwrap();
            queue.Signal(&consumer.unwrap(), frame.sync_value).unwrap();
        }
    }
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
                    Subresource: u32::MAX,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }
    unsafe fn barriers(
        list: &ID3D12GraphicsCommandList,
        mut barriers: Vec<D3D12_RESOURCE_BARRIER>,
    ) {
        unsafe {
            list.ResourceBarrier(&barriers);
        }
        for barrier in &mut barriers {
            unsafe {
                ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource);
            }
        }
    }
    unsafe fn location(resource: &ID3D12Resource) -> D3D12_TEXTURE_COPY_LOCATION {
        D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(resource.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        }
    }
}

#[test]
fn padded_unequal_d3d12_sources_publish_independently_and_release_before_resize() {
    let _guard = TEST_LOCK.lock();
    Scenario::capture(false, false);
    Scenario::capture(false, false);
}

#[test]
fn rejected_d3d12_stream_preserves_sibling_import_and_resize() {
    let _guard = TEST_LOCK.lock();
    Scenario::capture(false, true);
}

#[test]
fn enhanced_array_sources_publish_crops_and_release_before_resize() {
    let _guard = TEST_LOCK.lock();
    unsafe {
        let mut device: Option<ID3D12Device> = None;
        D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device).unwrap();
        let tracking = polymer_producer_d3d12::Tracking::default();
        if !tracking
            .detect_device_features(&device.unwrap())
            .unwrap()
            .unwrap()
        {
            eprintln!("enhanced array capture unavailable: device/runtime lacks enhanced barriers");
            return;
        }
    }
    Scenario::capture(true, false);
}

impl Scenario {
    fn capture(enhanced: bool, reject_first: bool) {
        let name = format!("polymer-d3d12-multi-{}", std::process::id());
        let listener = LocalSocketListener::bind(&name).unwrap();
        unsafe {
            std::env::set_var(polymer_hook::config::SESSION, &name);
            std::env::set_var(polymer_hook::config::TRACK_RENDER_TARGET, "1");
            std::env::set_var(polymer_hook::config::GRAPH_TRACKING, "1");
            std::env::set_var(polymer_hook::config::TRACK_RENDER_TARGETS, "0,1");
            assert!(polymer_hook_dxgi::install_factory_detours());
            assert_eq!(PolymerInstallD3d12Hook(), 0);
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for index in 0..if reject_first { 5 } else { 3 } {
                let mut socket = listener.accept().unwrap();
                socket
                    .send(Message::Hello(Handshake {
                        protocol_version: PROTOCOL_VERSION,
                        process_id: std::process::id(),
                        preferred_consumer: ConsumerApi::D3d12,
                    }))
                    .unwrap();
                let Message::MapOffer(offer) = socket.receive().unwrap() else {
                    panic!("no frame channel");
                };
                let mut frames = FrameSubscriber::from_offer(offer).unwrap();
                let Message::StreamReady(ready) = socket.receive().unwrap() else {
                    panic!("no stream");
                };
                if reject_first && index == 0 {
                    sender.send((ready, None)).unwrap();
                    continue;
                }
                socket.send(Message::StreamAccepted).unwrap();
                assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
                sender
                    .send((ready, Some(frames.latest().unwrap().unwrap())))
                    .unwrap();
            }
        });
        unsafe {
            let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
            let title = "polymer D3D12 multistream\0"
                .encode_utf16()
                .collect::<Vec<_>>();
            let window = CreateWindowExW(
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
            );
            assert!(!window.is_null());
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
            let adapter = factory.EnumAdapters1(0).unwrap();
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device).unwrap();
            let device = device.unwrap();
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })
                .unwrap();
            let swapchain: IDXGISwapChain3 = factory
                .CreateSwapChainForHwnd(
                    &queue,
                    HWND(window),
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: 64,
                        Height: 64,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        ..Default::default()
                    },
                    None,
                    None::<&IDXGIOutput>,
                )
                .unwrap()
                .cast()
                .unwrap();
            let buffer: ID3D12Resource = swapchain
                .GetBuffer(swapchain.GetCurrentBackBufferIndex())
                .unwrap();
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
            Scenario::barriers(
                &list,
                vec![Scenario::transition(
                    &buffer,
                    D3D12_RESOURCE_STATE_PRESENT,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                )],
            );
            let rtv_heap: ID3D12DescriptorHeap = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: 3,
                    ..Default::default()
                })
                .unwrap();
            for (index, (x, y, width, height, dx)) in [(8, 0, 32, 64, 0), (4, 3, 24, 32, 32)]
                .into_iter()
                .enumerate()
            {
                let mut source: Option<ID3D12Resource> = None;
                device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES {
                            Type: D3D12_HEAP_TYPE_DEFAULT,
                            CreationNodeMask: 1,
                            VisibleNodeMask: 1,
                            ..Default::default()
                        },
                        D3D12_HEAP_FLAG_NONE,
                        &D3D12_RESOURCE_DESC {
                            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                            Width: 64,
                            Height: 64,
                            DepthOrArraySize: if enhanced { 2 } else { 1 },
                            MipLevels: 1,
                            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            ..Default::default()
                        },
                        if enhanced {
                            D3D12_RESOURCE_STATE_COMMON
                        } else {
                            D3D12_RESOURCE_STATE_RENDER_TARGET
                        },
                        None,
                        &mut source,
                    )
                    .unwrap();
                let source = source.unwrap();
                let rtv = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: rtv_heap.GetCPUDescriptorHandleForHeapStart().ptr
                        + index
                            * device
                                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV)
                                as usize,
                };
                let array_view = D3D12_RENDER_TARGET_VIEW_DESC {
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D12_RENDER_TARGET_VIEW_DESC_0 {
                        Texture2DArray: D3D12_TEX2D_ARRAY_RTV {
                            FirstArraySlice: 1,
                            ArraySize: 1,
                            ..Default::default()
                        },
                    },
                };
                device.CreateRenderTargetView(
                    &source,
                    enhanced.then_some(&array_view as *const _),
                    rtv,
                );
                if enhanced {
                    Scenario::enhanced_transition(
                        &list,
                        &source,
                        D3D12_BARRIER_LAYOUT_COMMON,
                        D3D12_BARRIER_LAYOUT_RENDER_TARGET,
                    );
                }
                // Blue padding surrounds red/green content. Reading the published
                // content region must find content only, including its corners.
                list.ClearRenderTargetView(rtv, &[0.0, 0.0, 1.0, 1.0], None);
                let color = if index == 0 {
                    [1.0, 0.0, 0.0, 1.0]
                } else {
                    [0.0, 1.0, 0.0, 1.0]
                };
                list.ClearRenderTargetView(
                    rtv,
                    &color,
                    Some(&[windows::Win32::Foundation::RECT {
                        left: x as i32,
                        top: y as i32,
                        right: (x + width) as i32,
                        bottom: (y + height) as i32,
                    }]),
                );
                if enhanced {
                    Scenario::enhanced_transition(
                        &list,
                        &source,
                        D3D12_BARRIER_LAYOUT_RENDER_TARGET,
                        D3D12_BARRIER_LAYOUT_COPY_SOURCE,
                    );
                } else {
                    Scenario::barriers(
                        &list,
                        vec![Scenario::transition(
                            &source,
                            D3D12_RESOURCE_STATE_RENDER_TARGET,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                        )],
                    );
                }
                let mut from = Scenario::location(&source);
                if enhanced {
                    from.Anonymous.SubresourceIndex = 1;
                }
                let mut to = Scenario::location(&buffer);
                let crop = D3D12_BOX {
                    left: x,
                    top: y,
                    front: 0,
                    right: x + width,
                    bottom: y + height,
                    back: 1,
                };
                list.CopyTextureRegion(&to, dx, 0, 0, &from, Some(&crop));
                ManuallyDrop::drop(&mut from.pResource);
                ManuallyDrop::drop(&mut to.pResource);
                if enhanced {
                    Scenario::enhanced_transition(
                        &list,
                        &source,
                        D3D12_BARRIER_LAYOUT_COPY_SOURCE,
                        D3D12_BARRIER_LAYOUT_COMMON,
                    );
                }
            }
            // Model a later scissored UI draw at the recording boundary. Its
            // uncertain raster bounds must not replace either proven copy source.
            // No shader draw is submitted; the copied pixels remain unchanged.
            let overlay_rtv = D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: rtv_heap.GetCPUDescriptorHandleForHeapStart().ptr
                    + 2 * device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV)
                        as usize,
            };
            device.CreateRenderTargetView(&buffer, None, overlay_rtv);
            list.OMSetRenderTargets(1, Some(&overlay_rtv), false, None);
            list.RSSetScissorRects(&[windows::Win32::Foundation::RECT {
                left: 3,
                top: 5,
                right: 19,
                bottom: 21,
            }]);
            vtable::queue_tracking(queue.as_raw())
                .unwrap()
                .record_draw(list.as_raw());
            Scenario::barriers(
                &list,
                vec![Scenario::transition(
                    &buffer,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_PRESENT,
                )],
            );
            list.Close().unwrap();
            queue.ExecuteCommandLists(&[Some(list.cast().unwrap())]);
            let tracking = vtable::queue_tracking(queue.as_raw()).unwrap();
            std::env::remove_var(polymer_hook::config::GRAPH_TRACKING);
            let selected =
                tracking.swapchain_presentations(swapchain.as_raw(), 2, queue.as_raw() as usize);
            assert_eq!(selected.len(), 2);
            assert!(selected.iter().all(|surface| surface.source.is_some()));
            if enhanced {
                assert!(selected.iter().all(|surface| matches!(
                    surface.source.as_ref().unwrap().state,
                    Some(polymer_producer_d3d12::D3d12CaptureSourceState::Enhanced { .. })
                )));
                assert!(
                    selected
                        .iter()
                        .all(|surface| surface.source.as_ref().unwrap().subresource == 1)
                );
            }
            let repeated =
                tracking.swapchain_presentations(swapchain.as_raw(), 2, queue.as_raw() as usize);
            assert_eq!(repeated.len(), 1);
            assert!(repeated[0].source.is_none());
            // Executing the immutable list again refreshes consumed observations.
            queue.ExecuteCommandLists(&[Some(list.cast().unwrap())]);
            std::env::set_var(polymer_hook::config::GRAPH_TRACKING, "1");
            for _ in 0..2 {
                let retained = tracking.swapchain_presentations(
                    swapchain.as_raw(),
                    2,
                    queue.as_raw() as usize,
                );
                assert_eq!(retained.len(), 2);
                assert!(retained.iter().all(|surface| surface.source.is_some()));
            }
            swapchain.Present(0, DXGI_PRESENT(0)).ok().unwrap();
            let mut ids = Vec::new();
            for (index, (extent, pixel)) in
                [((32, 64), [0, 0, 255, 255]), ((24, 32), [0, 255, 0, 255])]
                    .into_iter()
                    .enumerate()
            {
                let (ready, frame) = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(ready.width >= extent.0 && ready.height >= extent.1);
                ids.push(ready.surface_id);
                if reject_first && index == 0 {
                    assert!(frame.is_none());
                } else {
                    let frame = frame.expect("accepted sibling published no frame");
                    assert_eq!((frame.render_width, frame.render_height), extent);
                    assert_eq!((frame.frame_number, frame.sync_value), (1, 1));
                    Scenario::verify_pixels(&device, &queue, &ready, &frame, pixel);
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
                    windows::Win32::Foundation::CloseHandle(HANDLE(handle as *mut _)).unwrap();
                }
            }
            assert_ne!(ids[0], ids[1]);
            if reject_first {
                std::env::set_var(polymer_hook::config::TRACK_RENDER_TARGETS, "1,0");
                swapchain.Present(0, DXGI_PRESENT(0)).ok().unwrap();
                for (slot, (extent, pixel)) in
                    [((24, 32), [0, 255, 0, 255]), ((32, 64), [0, 0, 255, 255])]
                        .into_iter()
                        .enumerate()
                {
                    let (ready, frame) = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
                    let frame = frame.expect("changed request remained parked");
                    assert_eq!(ready.surface_id, ids[slot]);
                    assert_eq!((ready.width, ready.height), (64, 64));
                    assert_eq!((frame.render_width, frame.render_height), extent);
                    Scenario::verify_pixels(&device, &queue, &ready, &frame, pixel);
                    for handle in ready.image_handles[..ready.slot_count as usize]
                        .iter()
                        .copied()
                        .chain([
                            ready.timeline_semaphore_handle,
                            ready.consumer_semaphore_handle,
                        ])
                        .filter(|handle| *handle != 0)
                    {
                        windows::Win32::Foundation::CloseHandle(HANDLE(handle as *mut _)).unwrap();
                    }
                }
            }
            drop(buffer);
            drop(list);
            swapchain
                .ResizeBuffers(0, 80, 48, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))
                .unwrap();
            swapchain.Present(0, DXGI_PRESENT(0)).ok().unwrap();
            let (ready, frame) = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
            let frame = frame.expect("resized stream published no frame");
            assert_eq!((frame.render_width, frame.render_height), (80, 48));
            for handle in ready.image_handles[..ready.slot_count as usize]
                .iter()
                .copied()
                .chain([
                    ready.timeline_semaphore_handle,
                    ready.consumer_semaphore_handle,
                ])
                .filter(|handle| *handle != 0)
            {
                windows::Win32::Foundation::CloseHandle(HANDLE(handle as *mut _)).unwrap();
            }
            drop(swapchain);
            DestroyWindow(window);
            std::env::remove_var(polymer_hook::config::SESSION);
            std::env::remove_var(polymer_hook::config::TRACK_RENDER_TARGET);
            std::env::remove_var(polymer_hook::config::TRACK_RENDER_TARGETS);
            std::env::remove_var(polymer_hook::config::GRAPH_TRACKING);
        }
        server.join().unwrap();
    }
}
