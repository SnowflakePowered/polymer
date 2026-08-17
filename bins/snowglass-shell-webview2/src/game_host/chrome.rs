//! Device-local chrome textures Vello rasterises for the compositor.
//!
//! wgpu and the compositor share one D3D12 device. The two crates use
//! different `windows` versions, so an owned COM reference crosses their Rust
//! type boundary as its ABI pointer; no Win32 shared handle is involved.

#![allow(unsafe_code)]

use std::{error::Error, ffi::c_void};
use vello::wgpu;
use windows_wgpu::Win32::Graphics::{
    Direct3D12::{
        D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_DESC,
        D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12Resource,
    },
    Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
};
use windows_wgpu::core::Interface;

pub struct ChromeTexture {
    resource: ID3D12Resource,
    pub width: u32,
    pub height: u32,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

/// An owned reference to a rasterized band resource that can cross from the
/// Vello worker to the presentation thread without copying its pixels.
pub struct ChromeResource {
    resource: ID3D12Resource,
    pub width: u32,
    pub height: u32,
}

impl ChromeResource {
    /// Transfers the retained COM reference into the compositor's windows
    /// crate version.
    pub fn into_abi_reference(self) -> *mut c_void {
        self.resource.into_raw()
    }
}

impl ChromeTexture {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Self, Box<dyn Error>> {
        let (width, height) = (width.max(1), height.max(1));
        let description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        };
        let properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };

        // SAFETY: the HAL device outlives this borrow, and the resource created
        // through it is retained by both wrappers below.
        let resource = unsafe {
            let hal = device
                .as_hal::<wgpu_hal::api::Dx12>()
                .ok_or("the game layer is not running on D3D12")?;
            let mut resource: Option<ID3D12Resource> = None;
            hal.raw_device().CreateCommittedResource(
                &properties,
                D3D12_HEAP_FLAG_NONE,
                &description,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )?;
            resource.ok_or("D3D12 returned no chrome texture")?
        };

        let descriptor = wgpu::TextureDescriptor {
            label: Some("snowglass game layer band"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        };
        // SAFETY: the resource was created to match `descriptor` exactly, and
        // the clone handed over keeps it alive for as long as wgpu holds it.
        let texture = unsafe {
            let hal = wgpu_hal::dx12::Device::texture_from_raw(
                resource.clone(),
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureDimension::D2,
                descriptor.size,
                1,
                1,
            );
            device.create_texture_from_hal::<wgpu_hal::api::Dx12>(hal, &descriptor)
        };
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Ok(Self {
            resource,
            width,
            height,
            texture,
            view,
        })
    }

    pub fn retained_resource(&self) -> ChromeResource {
        ChromeResource {
            resource: self.resource.clone(),
            width: self.width,
            height: self.height,
        }
    }
}
