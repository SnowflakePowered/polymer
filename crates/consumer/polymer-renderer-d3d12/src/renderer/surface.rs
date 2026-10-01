use polymer_consumer_d3d12::ImportedD3d12Stream;
use polymer_ipc::StreamReady;
use polymer_protocol::MAX_IMAGE_SLOTS;
use windows::Win32::Graphics::{Direct3D12::*, Dxgi::Common::DXGI_FORMAT};

use super::compositor::{create_srv, create_texture, transition};
use polymer_renderer::renderer::frame_ownership::{Retention, SurfaceFrames};

pub(super) const SURFACE_DESCRIPTORS: u32 = MAX_IMAGE_SLOTS as u32 + 1;

pub(super) struct ImportedSurface {
    pub connection: u64,
    pub surface_id: u64,
    pub target_window: i64,
    pub descriptor: u32,
    pub imported: ImportedD3d12Stream,
    pub flip_y: bool,
    pub quarter_turns: u8,
    pub cache: Option<ID3D12Resource>,
    pub video_cache: Option<ID3D12Resource>,
    pub frames: SurfaceFrames,
}

impl ImportedSurface {
    pub fn new(
        device: &ID3D12Device,
        imported: ImportedD3d12Stream,
        stream: &StreamReady,
        connection: u64,
        quarter_turns: u8,
    ) -> windows::core::Result<Self> {
        // Single-slot producers must get their only image back after each
        // capture.
        let needs_cache = imported.resources().len() == 1
            || imported.resources().iter().any(|resource| unsafe {
                resource
                    .GetDesc()
                    .Flags
                    .contains(D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE)
            });
        let mut surface = Self {
            connection,
            surface_id: stream.surface_id.0,
            target_window: stream.source_window,
            descriptor: 0,
            imported,
            flip_y: stream.bottom_up,
            quarter_turns: quarter_turns % 4,
            cache: None,
            video_cache: None,
            frames: SurfaceFrames::default(),
        };
        if needs_cache {
            surface.cache = Some(surface.create_cache(device)?);
        }
        Ok(surface)
    }

    /// A cached surface copies each frame out of the producer's slot.
    pub fn retention(&self) -> Retention {
        if self.cache.is_some() {
            Retention::Cached
        } else {
            Retention::Retained
        }
    }

    pub fn create_cache(&self, device: &ID3D12Device) -> windows::core::Result<ID3D12Resource> {
        unsafe {
            create_texture(
                device,
                self.imported.width,
                self.imported.height,
                DXGI_FORMAT(self.imported.format as i32),
                D3D12_RESOURCE_STATE_COMMON,
            )
        }
    }

    pub fn texture(&self) -> Option<&ID3D12Resource> {
        let frame = self.frames.frame()?;
        self.cache
            .as_ref()
            .or_else(|| self.imported.resource(frame.slot))
    }

    pub fn sample_descriptor(&self) -> u32 {
        if self.cache.is_some() {
            self.descriptor
        } else {
            self.descriptor + self.frames.frame().map_or(0, |frame| frame.slot) + 1
        }
    }

    /// Descriptors are immutable between imports, so in-flight redraws cannot
    /// accidentally sample a newly selected slot through an overwritten SRV.
    pub fn write_descriptors(
        &self,
        device: &ID3D12Device,
        heap: &ID3D12DescriptorHeap,
        stride: u32,
    ) {
        unsafe {
            if let Some(cache) = &self.cache {
                create_srv(device, heap, stride, self.descriptor, cache);
            } else {
                for (slot, resource) in self.imported.resources().iter().enumerate() {
                    create_srv(
                        device,
                        heap,
                        stride,
                        self.descriptor + slot as u32 + 1,
                        resource,
                    );
                }
            }
        }
    }

    pub fn copy(
        list: &ID3D12GraphicsCommandList,
        source: &ID3D12Resource,
        destination: &ID3D12Resource,
    ) {
        unsafe {
            transition(
                list,
                source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            transition(
                list,
                destination,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_DEST,
            );
            list.CopyResource(destination, source);
            transition(
                list,
                source,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            );
            transition(
                list,
                destination,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_COMMON,
            );
        }
    }
}
