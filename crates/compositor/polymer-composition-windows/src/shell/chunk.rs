//! Composition drawing surfaces that show the shell's raster chunks.
//!
//! A DXGI swapchain per chunk presents on its own schedule, separately from
//! the composition commit that places it, so a layout change could show one
//! chunk's new pixels a frame before or after another's. Drawing surface
//! updates are applied by the composition commit itself. Every chunk's pixels
//! and the visual tree therefore change in the same frame.

#![allow(unsafe_code)]

use super::ShellDeviceContext;
use std::{error::Error, ffi::c_void};
use windows::{
    Graphics::{
        DirectX::{DirectXAlphaMode, DirectXPixelFormat},
        SizeInt32,
    },
    UI::Composition::{
        CompositionDrawingSurface, CompositionGraphicsDevice, Compositor, ICompositionSurface,
    },
    Win32::{
        Foundation::POINT,
        Graphics::{
            Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile, ID3DBlob},
            Direct3D11::{
                D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_SAMPLER_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
                D3D11_VIEWPORT, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader,
                ID3D11RenderTargetView, ID3D11Resource, ID3D11SamplerState,
                ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
            },
            Direct3D11on12::{D3D11_RESOURCE_FLAGS, D3D11On12CreateDevice, ID3D11On12Device},
            Direct3D12::{D3D12_RESOURCE_STATE_UNORDERED_ACCESS, ID3D12Device, ID3D12Resource},
        },
        System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop},
    },
    core::{Interface, PCSTR, s},
};

/// Vello stores straight alpha; composition surfaces take premultiplied.
const SHADER: &str = r"
Texture2D<float4> source : register(t0);
SamplerState nearest : register(s0);
struct Vertex { float4 position : SV_Position; float2 uv : TEXCOORD0; };
Vertex vertex_main(uint id : SV_VertexID) {
    Vertex output;
    output.uv = float2((id << 1) & 2, id & 2);
    output.position = float4(output.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return output;
}
float4 pixel_main(Vertex input) : SV_Target {
    float4 color = source.Sample(nearest, input.uv);
    return float4(color.rgb * color.a, color.a);
}
";

struct WrappedSource {
    identity: usize,
    _resource: ID3D12Resource,
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
}

pub(super) struct ChunkSurface {
    surface: CompositionDrawingSurface,
    size: (u32, u32),
    source: Option<WrappedSource>,
}

impl ChunkSurface {
    pub(super) fn composition_surface(&self) -> windows::core::Result<ICompositionSurface> {
        self.surface.cast()
    }
}

/// Draws Vello's chunk textures into drawing surfaces through a D3D11On12
/// device layered on the rasterizer's own D3D12 device and queue, so the draw
/// is ordered after the raster work without any cross-device fence.
pub(super) struct ChunkSurfaces {
    device: ID3D11On12Device,
    d3d11: ID3D11Device,
    context: ID3D11DeviceContext,
    graphics: CompositionGraphicsDevice,
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
}

impl ChunkSurfaces {
    pub(super) fn new(
        compositor: &Compositor,
        context: &ShellDeviceContext,
    ) -> Result<Self, Box<dyn Error>> {
        let device: ID3D12Device = context.device.cast()?;
        let mut d3d11: Option<ID3D11Device> = None;
        let mut immediate: Option<ID3D11DeviceContext> = None;
        // SAFETY: the D3D12 device and queue are retained by the shell device
        // context for as long as this layered device exists.
        unsafe {
            D3D11On12CreateDevice(
                &device,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 as u32,
                None,
                Some(&[Some(context.queue.clone())]),
                0,
                Some(&mut d3d11),
                Some(&mut immediate),
                None,
            )?;
        }
        let d3d11 = d3d11.ok_or("D3D11On12 returned no device")?;
        let interop: ICompositorInterop = compositor.cast()?;
        // SAFETY: the layered device outlives the graphics device created on it.
        let graphics = unsafe { interop.CreateGraphicsDevice(&d3d11)? };
        let vertex_code = Self::compile(s!("vertex_main"), s!("vs_5_0"))?;
        let pixel_code = Self::compile(s!("pixel_main"), s!("ps_5_0"))?;
        let mut vertex = None;
        let mut pixel = None;
        let mut sampler = None;
        // SAFETY: the bytecode blobs stay alive for the duration of the calls.
        unsafe {
            d3d11.CreateVertexShader(Self::bytes(&vertex_code), None, Some(&mut vertex))?;
            d3d11.CreatePixelShader(Self::bytes(&pixel_code), None, Some(&mut pixel))?;
            d3d11.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
        }
        Ok(Self {
            device: d3d11.cast()?,
            d3d11,
            context: immediate.ok_or("D3D11On12 returned no device context")?,
            graphics,
            vertex: vertex.ok_or("D3D11 returned no vertex shader")?,
            pixel: pixel.ok_or("D3D11 returned no pixel shader")?,
            sampler: sampler.ok_or("D3D11 returned no sampler")?,
        })
    }

    fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, Box<dyn Error>> {
        let mut code = None;
        let mut errors = None;
        // SAFETY: the source buffer outlives the call, and both output blobs
        // are owned by the returned references.
        let compiled = unsafe {
            D3DCompile(
                SHADER.as_ptr().cast(),
                SHADER.len(),
                None,
                None,
                None,
                entry,
                target,
                0,
                0,
                &mut code,
                Some(&mut errors),
            )
        };
        if let Err(error) = compiled {
            let message = errors
                .map(|errors| String::from_utf8_lossy(Self::bytes(&errors)).into_owned())
                .unwrap_or_default();
            return Err(format!("compiling the chunk shader failed: {error} {message}").into());
        }
        Ok(code.ok_or("D3DCompile returned no bytecode")?)
    }

    fn bytes(blob: &ID3DBlob) -> &[u8] {
        // SAFETY: a blob's pointer and size describe one buffer it owns.
        unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()) }
    }

    pub(super) fn create(&self, width: u32, height: u32) -> windows::core::Result<ChunkSurface> {
        let size = (width.max(1), height.max(1));
        let surface = self.graphics.CreateDrawingSurface2(
            SizeInt32 {
                Width: size.0 as i32,
                Height: size.1 as i32,
            },
            DirectXPixelFormat::R8G8B8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        Ok(ChunkSurface {
            surface,
            size,
            source: None,
        })
    }

    fn wrap(
        &self,
        resource: ID3D12Resource,
        identity: usize,
    ) -> windows::core::Result<WrappedSource> {
        let mut texture: Option<ID3D11Texture2D> = None;
        let mut view = None;
        // SAFETY: the resource is a live texture on the layered device's
        // D3D12 device, and stays in the declared state between uses.
        unsafe {
            self.device.CreateWrappedResource(
                &resource,
                &D3D11_RESOURCE_FLAGS {
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                &mut texture,
            )?;
        }
        let texture = texture.ok_or(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_POINTER,
        ))?;
        // SAFETY: the view describes the whole wrapped texture.
        unsafe {
            self.d3d11
                .CreateShaderResourceView(&texture, None, Some(&mut view))?
        };
        Ok(WrappedSource {
            identity,
            _resource: resource,
            texture,
            view: view.ok_or(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_POINTER,
            ))?,
        })
    }

    /// Draws a rasterized chunk into its drawing surface, premultiplying its
    /// alpha. The new pixels become visible with the next composition commit.
    ///
    /// # Safety
    ///
    /// `resource` must carry one owned ABI-compatible `ID3D12Resource`
    /// reference created on the rasterizer's device, left in the
    /// `UNORDERED_ACCESS` state by Vello.
    pub(super) unsafe fn update(
        &self,
        chunk: &mut ChunkSurface,
        resource: *mut c_void,
        width: u32,
        height: u32,
    ) -> windows::core::Result<()> {
        let identity = resource as usize;
        // SAFETY: ownership of this reference is transferred by the caller.
        let resource = unsafe { ID3D12Resource::from_raw(resource) };
        if chunk
            .source
            .as_ref()
            .is_none_or(|source| source.identity != identity)
        {
            chunk.source = Some(self.wrap(resource, identity)?);
        }
        let size = (width.max(1), height.max(1));
        if chunk.size != size {
            chunk.surface.Resize(SizeInt32 {
                Width: size.0 as i32,
                Height: size.1 as i32,
            })?;
            chunk.size = size;
        }
        let Some(source) = &chunk.source else {
            return Ok(());
        };
        let wrapped = [Some(source.texture.cast::<ID3D11Resource>()?)];
        let interop: ICompositionDrawingSurfaceInterop = chunk.surface.cast()?;
        let mut offset = POINT::default();
        // SAFETY: the wrapped texture is acquired for D3D11 use only between
        // these calls, and the drawing surface is updated on the device that
        // owns it.
        unsafe {
            self.device.AcquireWrappedResources(&wrapped);
            let target: ID3D11Texture2D = interop.BeginDraw(None, &mut offset)?;
            let mut render_target: Option<ID3D11RenderTargetView> = None;
            self.d3d11
                .CreateRenderTargetView(&target, None, Some(&mut render_target))?;
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.IASetInputLayout(None);
            self.context.VSSetShader(&self.vertex, None);
            self.context.PSSetShader(&self.pixel, None);
            self.context
                .PSSetShaderResources(0, Some(&[Some(source.view.clone())]));
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.context
                .OMSetRenderTargets(Some(&[render_target]), None);
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: offset.x as f32,
                TopLeftY: offset.y as f32,
                Width: size.0 as f32,
                Height: size.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            self.context.Draw(3, 0);
            self.context.PSSetShaderResources(0, Some(&[None]));
            self.context.OMSetRenderTargets(None, None);
            interop.EndDraw()?;
            self.device.ReleaseWrappedResources(&wrapped);
        }
        Ok(())
    }

    /// Submits every pending draw to the rasterizer's queue.
    pub(super) fn flush(&self) {
        // SAFETY: flushing only submits work recorded on this device.
        unsafe { self.context.Flush() };
    }
}
