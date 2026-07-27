//! Textures Vello rasterises into that the compositor can sample directly.
//!
//! Vello draws through wgpu while the compositor is plain D3D12, so the two
//! would normally only meet through system memory: read the pixels back, then
//! upload them again. Both steps are pure loss — the pixels are already on the
//! GPU, and the second device is on the same adapter.
//!
//! A shared resource removes them. The texture is created on wgpu's D3D12
//! device with `SHARED`, handed back to wgpu so Vello can draw into it, and
//! published as a Win32 handle that any device on the adapter can open.
//!
//! The handle is what crosses the boundary rather than the resource itself:
//! `wgpu-hal` is built against an older `windows` than the rest of the shell,
//! so its `ID3D12Resource` is a different Rust type from ours. A `HANDLE` is
//! just an integer, and both sides agree on it.

#![allow(unsafe_code)]

use std::error::Error;
use vello::wgpu;
use windows_wgpu::Win32::Graphics::{
    Direct3D12::{
        D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT,
        D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON,
        D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12Resource,
    },
    Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
};

/// What the compositor needs to find a band's pixels: a handle to open, and the
/// size of what is in it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SharedChrome {
    /// Win32 shared handle, valid for as long as the band that owns it. It is
    /// carried as an integer because the two halves of this interop are built
    /// against different `windows` crates.
    pub handle: isize,
    pub width: u32,
    pub height: u32,
}

/// One band's render target, shared with whichever device composites it.
pub struct SharedTexture {
    /// Kept alive because the handle and the wgpu texture both point at it.
    _resource: ID3D12Resource,
    /// Closed on drop, after the texture that reads it is gone.
    handle: windows_wgpu::Win32::Foundation::HANDLE,
    /// Retained alongside the view so the wgpu side of the resource lives
    /// exactly as long as the D3D12 side does.
    _texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub shared: SharedChrome,
}

impl SharedTexture {
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
            // Unordered access because Vello rasterises with a compute shader.
            // Render target because a D3D11 device refuses to open a shared
            // resource without it. Simultaneous access because two devices read
            // and write this without transitioning it between them.
            Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS
                | D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
        };
        let properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };

        // SAFETY: the HAL device outlives this borrow, and the resource created
        // through it is retained by the returned value.
        let (resource, handle) = unsafe {
            let hal = device
                .as_hal::<wgpu_hal::api::Dx12>()
                .ok_or("the game layer is not running on D3D12")?;
            let raw = hal.raw_device();
            let mut resource: Option<ID3D12Resource> = None;
            raw.CreateCommittedResource(
                &properties,
                D3D12_HEAP_FLAG_SHARED,
                &description,
                // A simultaneous-access resource starts common and stays there,
                // which is what lets the other device read it without a barrier
                // this one would have to issue.
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )?;
            let resource = resource.ok_or("D3D12 returned no shared texture")?;
            let handle = raw.CreateSharedHandle(&resource, None, 0x1000_0000, None)?;
            (resource, handle)
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
            usage: wgpu::TextureUsages::STORAGE_BINDING,
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
            _resource: resource,
            handle,
            _texture: texture,
            view,
            shared: SharedChrome {
                handle: handle.0 as isize,
                width,
                height,
            },
        })
    }
}

impl Drop for SharedTexture {
    fn drop(&mut self) {
        // SAFETY: the handle was produced by `CreateSharedHandle` and is closed
        // once, here. Consumers opened their own resources from it, which hold
        // their own references.
        unsafe {
            let _ = windows_wgpu::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}
