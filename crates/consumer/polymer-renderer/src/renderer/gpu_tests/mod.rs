//! Opt-in driver regressions. Run with `cargo test -p polymer-renderer
//! --lib gpu_tests -- --ignored --test-threads=1` on a Windows GPU host.

mod d3d12;
mod vulkan;

use super::RenderWindow;
use crate::layout::Extent;
use polymer_consumer_d3d12::D3d12ConsumerDevice;
use polymer_ipc::StreamReady;
use polymer_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PresentationTransform, StreamId, SurfaceId,
};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle,
    Win32WindowHandle, WindowHandle,
};
use std::{
    mem::ManuallyDrop,
    num::NonZeroIsize,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::{Direct3D12::*, Dxgi::Common::*},
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

struct TestWindow(HWND);

impl TestWindow {
    fn new() -> Self {
        Self(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Polymer GPU regression"),
                WS_POPUP,
                0,
                0,
                16,
                16,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        })
    }
}
impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
impl HasWindowHandle for TestWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let mut handle = Win32WindowHandle::new(NonZeroIsize::new(self.0.0 as isize).unwrap());
        handle.hinstance = NonZeroIsize::new(unsafe {
            windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null())
        } as isize);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}
impl HasDisplayHandle for TestWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::windows())
    }
}
impl RenderWindow for TestWindow {
    fn render_extent(&self) -> Extent {
        Extent::new(16, 16)
    }
}

struct Producer {
    native: D3d12ConsumerDevice,
    images: Vec<ID3D12Resource>,
    ready: ID3D12Fence,
    released: ID3D12Fence,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    rtv: ID3D12DescriptorHeap,
}

impl Producer {
    fn new(adapter: AdapterLuid, slots: usize) -> Self {
        let native = D3d12ConsumerDevice::new(adapter).unwrap();
        let device = native.device();
        unsafe {
            let mut images = Vec::new();
            for _ in 0..slots {
                let mut image = None;
                device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES {
                            Type: D3D12_HEAP_TYPE_DEFAULT,
                            ..Default::default()
                        },
                        D3D12_HEAP_FLAG_SHARED,
                        &D3D12_RESOURCE_DESC {
                            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                            Width: 16,
                            Height: 16,
                            DepthOrArraySize: 1,
                            MipLevels: 1,
                            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                                | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
                            ..Default::default()
                        },
                        D3D12_RESOURCE_STATE_COMMON,
                        None,
                        &mut image,
                    )
                    .unwrap();
                images.push(image.unwrap());
            }
            let ready = device.CreateFence(0, D3D12_FENCE_FLAG_SHARED).unwrap();
            let released = device.CreateFence(0, D3D12_FENCE_FLAG_SHARED).unwrap();
            let allocator = device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .unwrap();
            let list: ID3D12GraphicsCommandList = device
                .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                .unwrap();
            list.Close().unwrap();
            let rtv = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: 1,
                    ..Default::default()
                })
                .unwrap();
            Self {
                native,
                images,
                ready,
                released,
                allocator,
                list,
                rtv,
            }
        }
    }

    fn stream(&self) -> StreamReady {
        let device = self.native.device();
        let mut handles = [0; MAX_IMAGE_SLOTS];
        unsafe {
            for (index, image) in self.images.iter().enumerate() {
                handles[index] = device
                    .CreateSharedHandle(image, None, 0x10000000, None)
                    .unwrap()
                    .0 as i64;
            }
            StreamReady {
                producer_process_id: std::process::id(),
                surface_id: SurfaceId(1),
                adapter_luid: self.native.adapter_luid(),
                source_window: 0,
                width: 16,
                height: 16,
                format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
                image_kind: ExternalImageKind::D3d12Resource,
                sync_kind: ExternalSyncKind::D3d12Fence,
                slot_count: self.images.len() as u32,
                image_handles: handles,
                timeline_semaphore_handle: device
                    .CreateSharedHandle(&self.ready, None, 0x10000000, None)
                    .unwrap()
                    .0 as i64,
                consumer_semaphore_handle: device
                    .CreateSharedHandle(&self.released, None, 0x10000000, None)
                    .unwrap()
                    .0 as i64,
                bottom_up: false,
            }
        }
    }

    fn paint(&self, slot: usize, value: u64, color: [f32; 4]) -> FrameReady {
        unsafe {
            self.allocator.Reset().unwrap();
            self.list.Reset(&self.allocator, None).unwrap();
            let handle = self.rtv.GetCPUDescriptorHandleForHeapStart();
            self.native
                .device()
                .CreateRenderTargetView(&self.images[slot], None, handle);
            self.transition(
                slot,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
            self.list.ClearRenderTargetView(handle, &color, None);
            self.transition(
                slot,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_COMMON,
            );
            self.list.Close().unwrap();
            use windows::core::Interface;
            self.native
                .queue()
                .ExecuteCommandLists(&[Some(self.list.cast().unwrap())]);
            self.native.queue().Signal(&self.ready, value).unwrap();
            Self::wait(&self.ready, value);
        }
        Self::frame(slot, value)
    }

    fn frame(slot: usize, value: u64) -> FrameReady {
        FrameReady {
            stream_id: StreamId(1),
            slot: slot as u32,
            render_width: 16,
            render_height: 16,
            capture_x: 0,
            capture_y: 0,
            capture_width: 16,
            capture_height: 16,
            presentation_width: 16,
            presentation_height: 16,
            presentation_transform: PresentationTransform::UNAVAILABLE,
            frame_number: value,
            sync_value: value,
            timestamp_qpc: 0,
        }
    }

    fn transition(&self, slot: usize, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) {
        let mut barrier = D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(Some(self.images[slot].clone())),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
            ..Default::default()
        };
        unsafe {
            self.list.ResourceBarrier(std::slice::from_ref(&barrier));
            ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource);
        }
    }

    fn wait(fence: &ID3D12Fence, value: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { fence.GetCompletedValue() } < value {
            assert!(Instant::now() < deadline, "GPU fence {value} timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
