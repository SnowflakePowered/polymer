use super::reader::{VideoFormat, VideoFrame};
use polymer_consumer_d3d12::{FenceWaiter, ImportedD3d12Stream};
use polymer_protocol::FrameReady;
use polymer_renderer_d3d12::renderer::compositor::create_texture;
use std::mem::ManuallyDrop;
use windows::Win32::{
    Graphics::{Direct3D12::*, Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM},
    System::Threading::INFINITE,
};
use windows::core::Interface;

const SLOTS: usize = 3;

pub(super) struct FrameRing {
    queue: ID3D12CommandQueue,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    copied: ID3D12Fence,
    copied_value: u64,
    released: ID3D12Fence,
    waiter: FenceWaiter,
    slots: Vec<ID3D12Resource>,
    slot_values: [u64; SLOTS],
    sequence: u64,
    crop: D3D12_BOX,
}

impl FrameRing {
    pub fn new(
        device: &ID3D12Device,
        format: &VideoFormat,
    ) -> windows::core::Result<(Self, ImportedD3d12Stream)> {
        unsafe {
            let queue: ID3D12CommandQueue =
                device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_COPY,
                    ..Default::default()
                })?;
            let allocator: ID3D12CommandAllocator =
                device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COPY)?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COPY, &allocator, None)?;
            list.Close()?;
            let slots = (0..SLOTS)
                .map(|_| {
                    create_texture(
                        device,
                        format.width,
                        format.height,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        D3D12_RESOURCE_STATE_COMMON,
                    )
                })
                .collect::<windows::core::Result<Vec<_>>>()?;
            let released: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let stream = ImportedD3d12Stream::new(
                slots.clone(),
                released.clone(),
                format.width,
                format.height,
                DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            );
            Ok((
                Self {
                    queue,
                    allocator,
                    list,
                    copied: device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?,
                    copied_value: 0,
                    released,
                    waiter: FenceWaiter::new()?,
                    slots,
                    slot_values: [0; SLOTS],
                    sequence: 0,
                    crop: format.crop,
                },
                stream,
            ))
        }
    }

    pub fn publish(&mut self, frame: &VideoFrame) -> windows::core::Result<Option<FrameReady>> {
        let sequence = self.sequence + 1;
        let slot = (sequence % SLOTS as u64) as usize;
        if !self
            .waiter
            .wait(&self.released, self.slot_values[slot], 0)?
        {
            return Ok(None);
        }
        unsafe {
            self.allocator.Reset()?;
            self.list.Reset(&self.allocator, None)?;
            let destination = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(self.slots[slot].clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            let source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(frame.resource.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            self.list
                .CopyTextureRegion(&destination, 0, 0, 0, &source, Some(&self.crop));
            drop(ManuallyDrop::into_inner(destination.pResource));
            drop(ManuallyDrop::into_inner(source.pResource));
            self.list.Close()?;
            frame.sync.EnqueueResourceReadyWait(&self.queue)?;
            self.queue
                .ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>()?)]);
            frame.sync.EnqueueResourceRelease(&self.queue)?;
            self.copied_value += 1;
            self.queue.Signal(&self.copied, self.copied_value)?;
        }
        self.waiter
            .wait(&self.copied, self.copied_value, INFINITE)?;
        self.sequence = sequence;
        let ready = FrameReady {
            slot: slot as u32,
            frame_number: sequence,
            sync_value: FrameReady::ready_value(sequence, slot as u32),
        };
        self.slot_values[slot] = ready.sync_value;
        Ok(Some(ready))
    }
}
