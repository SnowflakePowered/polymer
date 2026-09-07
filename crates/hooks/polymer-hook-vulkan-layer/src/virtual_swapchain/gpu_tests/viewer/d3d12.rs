use polymer_consumer_d3d12::{D3d12ConsumerDevice, ImportedD3d12Stream};
use polymer_ipc::StreamReady;
use std::{
    mem::ManuallyDrop,
    time::{Duration, Instant},
};
use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::Interface,
};

pub(super) struct Reader {
    native: D3d12ConsumerDevice,
    imported: ImportedD3d12Stream,
    buffer: ID3D12Resource,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    value: u64,
}
impl Reader {
    pub(super) fn new(ready: &StreamReady) -> Self {
        let native = D3d12ConsumerDevice::new(ready.adapter_luid).unwrap();
        let imported = native.import(ready).unwrap();
        unsafe {
            let device = native.device();
            let mut buffer = None;
            device
                .CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_READBACK,
                        ..Default::default()
                    },
                    D3D12_HEAP_FLAG_NONE,
                    &D3D12_RESOURCE_DESC {
                        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                        Width: 256 * 16,
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
                .unwrap();
            let allocator = device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .unwrap();
            let list: ID3D12GraphicsCommandList = device
                .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                .unwrap();
            list.Close().unwrap();
            let fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            Self {
                native,
                imported,
                buffer: buffer.unwrap(),
                allocator,
                list,
                fence,
                value: 0,
            }
        }
    }
    pub(super) fn read(&mut self, slot: u32, value: u64) -> Vec<u8> {
        unsafe {
            self.allocator.Reset().unwrap();
            self.list.Reset(&self.allocator, None).unwrap();
            let mut source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(self.imported.resource(slot).unwrap().clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            let mut target = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(self.buffer.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: 0,
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                            Width: 16,
                            Height: 16,
                            Depth: 1,
                            RowPitch: 256,
                        },
                    },
                },
            };
            self.list.CopyTextureRegion(&target, 0, 0, 0, &source, None);
            ManuallyDrop::drop(&mut source.pResource);
            ManuallyDrop::drop(&mut target.pResource);
            self.list.Close().unwrap();
            self.imported
                .wait_on_queue(self.native.queue(), value)
                .unwrap();
            self.native
                .queue()
                .ExecuteCommandLists(&[Some(self.list.cast().unwrap())]);
            self.imported
                .signal_release(self.native.queue(), value)
                .unwrap();
            self.value += 1;
            self.native.queue().Signal(&self.fence, self.value).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.fence.GetCompletedValue() < self.value {
                assert!(Instant::now() < deadline, "D3D12 WSI readback stalled");
                std::thread::yield_now();
            }
            let mut mapped = std::ptr::null_mut();
            self.buffer.Map(0, None, Some(&mut mapped)).unwrap();
            let bytes = std::slice::from_raw_parts(mapped.cast::<u8>(), 256 * 16);
            let result = bytes
                .chunks_exact(256)
                .flat_map(|row| row[..64].iter().copied())
                .collect();
            self.buffer.Unmap(0, None);
            result
        }
    }
}
