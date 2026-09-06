//! Native readback utilities for producer integration fixtures.
use windows::{Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC, core::Interface};

pub struct D3d12Readback;
impl D3d12Readback {
    /// Reads a COMMON-state image after previously queued producer waits.
    ///
    /// # Safety
    /// The device, queue, and image must belong to a compatible D3D12 device.
    /// The caller must order producer writes before this queue's image access
    /// and retain exclusive consumer access until this function returns.
    pub unsafe fn pixels(
        device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
        queue: &windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue,
        source: &windows::Win32::Graphics::Direct3D12::ID3D12Resource,
        width: u32,
        height: u32,
    ) -> Vec<[u8; 4]> {
        use std::mem::ManuallyDrop;
        use windows::Win32::{
            Foundation::CloseHandle,
            Graphics::Direct3D12::*,
            System::Threading::{CreateEventW, WaitForSingleObject},
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
                            Format: source.GetDesc().Format,
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
            list.CopyTextureRegion(
                &dst,
                0,
                0,
                0,
                &src,
                Some(&D3D12_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: width,
                    bottom: height,
                    back: 1,
                }),
            );
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
            assert_eq!(
                WaitForSingleObject(event, 2000).0,
                0,
                "external image readback timed out"
            );
            let _ = CloseHandle(event);
            let mut mapped = std::ptr::null_mut();
            buffer
                .Map(0, None, Some(&mut mapped))
                .expect("readback map");
            let mut pixels = Vec::new();
            for y in 0..height {
                for x in 0..width {
                    pixels.push(std::ptr::read_unaligned(
                        mapped
                            .cast::<u8>()
                            .add((y * pitch + x * 4) as usize)
                            .cast::<[u8; 4]>(),
                    ));
                }
            }
            buffer.Unmap(0, None);
            pixels
        }
    }
}
