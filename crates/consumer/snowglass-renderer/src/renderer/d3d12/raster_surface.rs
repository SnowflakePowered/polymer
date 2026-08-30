//! Minimal D3D12 surface for presenting one retained raster texture through WUC.

#![allow(unsafe_code)]

use std::{error::Error, ffi::c_void, mem::ManuallyDrop, time::Duration};

use crate::layout::Extent;
use snowglass_protocol::AdapterLuid;
use windows::{
    UI::Composition::{Compositor, ICompositionSurface},
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Graphics::{
            Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile, ID3DBlob},
            Direct3D12::*,
            Dxgi::{Common::*, *},
        },
        System::{
            Threading::{CreateEventW, INFINITE, WaitForSingleObject},
            WinRT::Composition::ICompositorInterop,
        },
    },
    core::{IUnknown, Interface, PCSTR},
};

const FRAME_COUNT: usize = 2;
const FORMAT: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM;

struct RasterSource {
    resource: ID3D12Resource,
    identity: usize,
    width: u32,
    height: u32,
}

/// A local transparent swapchain that publishes one Vello texture as a WUC
/// composition surface. It deliberately contains no stream receiver, layout
/// plan, dynamic descriptor allocator, upload arena, or WebRTC state.
pub struct RasterSurface {
    adapter: AdapterLuid,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    swapchain: IDXGISwapChain3,
    surface: ICompositionSurface,
    capacity: Extent,
    swapchain_flags: DXGI_SWAP_CHAIN_FLAG,
    backbuffers: Vec<ID3D12Resource>,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_stride: u32,
    srv_heap: ID3D12DescriptorHeap,
    srv_stride: u32,
    root_signature: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
    allocators: [ID3D12CommandAllocator; FRAME_COUNT],
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_event: HANDLE,
    fence_value: u64,
    frame_values: [u64; FRAME_COUNT],
    frame_latency_waitable: HANDLE,
    frame_permit: bool,
    source: Option<RasterSource>,
    retired_sources: Vec<(ID3D12Resource, u64)>,
}

/// D3D12, DXGI, and WUC composition surfaces are agile and a raster surface
/// is transferred only with exclusive ownership.
unsafe impl Send for RasterSurface {}

impl RasterSurface {
    /// Creates a transparent composition swapchain on the supplied device and
    /// queue and immediately wraps it in a WUC composition surface.
    ///
    /// # Safety
    ///
    /// `device` and `queue` must each carry one owned ABI-compatible COM
    /// reference. They must identify the adapter named by `adapter` and the
    /// queue used by the producer of textures passed to [`Self::set_source`].
    pub unsafe fn from_external(
        compositor: &Compositor,
        adapter: AdapterLuid,
        device: *mut c_void,
        queue: *mut c_void,
        capacity: Extent,
    ) -> Result<Self, Box<dyn Error>> {
        if device.is_null() || queue.is_null() {
            return Err("a raster surface requires a D3D12 device and queue".into());
        }
        // SAFETY: ownership of both references is transferred by the caller.
        let device = unsafe { ID3D12Device::from_raw(device) };
        let queue = unsafe { ID3D12CommandQueue::from_raw(queue) };
        let capacity = Extent::new(capacity.width.max(1), capacity.height.max(1));
        let swapchain_flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))? };
        let description = swapchain_description(capacity, swapchain_flags);
        let swapchain1 =
            unsafe { factory.CreateSwapChainForComposition(&queue, &description, None)? };
        let swapchain: IDXGISwapChain3 = swapchain1.cast()?;
        let swapchain2: IDXGISwapChain2 = swapchain.cast()?;
        unsafe { swapchain2.SetMaximumFrameLatency(1)? };
        let frame_latency_waitable = unsafe { swapchain2.GetFrameLatencyWaitableObject() };
        if frame_latency_waitable.is_invalid() {
            return Err("DXGI returned no raster-surface latency handle".into());
        }

        let swapchain_unknown: IUnknown = swapchain.cast()?;
        let interop: ICompositorInterop = compositor.cast()?;
        // SAFETY: DXGI created this swapchain specifically for composition.
        let surface = unsafe { interop.CreateCompositionSurfaceForSwapChain(&swapchain_unknown)? };

        let rtv_heap = unsafe {
            device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: FRAME_COUNT as u32,
                ..Default::default()
            })?
        };
        let srv_heap = unsafe {
            device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: FRAME_COUNT as u32,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                ..Default::default()
            })?
        };
        let rtv_stride =
            unsafe { device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) };
        let srv_stride = unsafe {
            device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
        };
        let backbuffers =
            unsafe { create_backbuffers(&device, &swapchain, &rtv_heap, rtv_stride)? };
        let root_signature = create_root_signature(&device)?;
        let pipeline = create_pipeline(&device, &root_signature)?;
        let allocators = [
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)? },
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)? },
        ];
        let command_list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &allocators[0],
                Some(&pipeline),
            )?
        };
        unsafe { command_list.Close()? };
        let fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? };
        let fence_event = unsafe { CreateEventW(None, false, false, None)? };

        Ok(Self {
            adapter,
            device,
            queue,
            swapchain,
            surface,
            capacity,
            swapchain_flags,
            backbuffers,
            rtv_heap,
            rtv_stride,
            srv_heap,
            srv_stride,
            root_signature,
            pipeline,
            allocators,
            command_list,
            fence,
            fence_event,
            fence_value: 0,
            frame_values: [0; FRAME_COUNT],
            frame_latency_waitable,
            frame_permit: false,
            source: None,
            retired_sources: Vec::new(),
        })
    }

    pub const fn adapter_luid(&self) -> AdapterLuid {
        self.adapter
    }

    pub fn d3d12_device(&self) -> &ID3D12Device {
        &self.device
    }

    pub fn composition_surface(&self) -> ICompositionSurface {
        self.surface.clone()
    }

    pub const fn capacity(&self) -> Extent {
        self.capacity
    }

    pub fn resize(&mut self, capacity: Extent) -> Result<bool, Box<dyn Error>> {
        let capacity = Extent::new(capacity.width.max(1), capacity.height.max(1));
        if capacity == self.capacity {
            return Ok(false);
        }
        self.wait_idle()?;
        self.backbuffers.clear();
        unsafe {
            self.swapchain.ResizeBuffers(
                FRAME_COUNT as u32,
                capacity.width,
                capacity.height,
                FORMAT,
                self.swapchain_flags,
            )?;
            self.backbuffers = create_backbuffers(
                &self.device,
                &self.swapchain,
                &self.rtv_heap,
                self.rtv_stride,
            )?;
        }
        self.capacity = capacity;
        self.frame_permit = false;
        Ok(true)
    }

    /// Retains the texture Vello most recently rasterized for this surface.
    ///
    /// # Safety
    ///
    /// `resource` must carry one owned ABI-compatible `ID3D12Resource`
    /// reference created on this surface's device. Its current state must be
    /// `UNORDERED_ACCESS`, matching Vello's storage-texture output state.
    pub unsafe fn set_source(&mut self, resource: *mut c_void, width: u32, height: u32) {
        if resource.is_null() || width == 0 || height == 0 {
            return;
        }
        let identity = resource as usize;
        // SAFETY: ownership of this reference is transferred by the caller.
        let resource = unsafe { ID3D12Resource::from_raw(resource) };
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.identity == identity)
        {
            return;
        }
        if let Some(previous) = self.source.take() {
            self.retired_sources
                .push((previous.resource, self.fence_value));
        }
        self.source = Some(RasterSource {
            resource,
            identity,
            width: width.min(self.capacity.width),
            height: height.min(self.capacity.height),
        });
    }

    /// Presents the current source without blocking for composition capacity.
    /// `false` means WUC still owns the single queued frame and the caller
    /// should retry on a later compositor tick.
    pub fn try_present(&mut self) -> Result<bool, Box<dyn Error>> {
        if self.source.is_none() || !self.try_begin_frame() {
            return Ok(false);
        }
        let frame_index = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize;
        self.wait_value(self.frame_values[frame_index])?;
        let completed = unsafe { self.fence.GetCompletedValue() };
        self.retired_sources
            .retain(|(_, fence_value)| *fence_value > completed);

        let allocator = &self.allocators[frame_index];
        unsafe {
            allocator.Reset()?;
            self.command_list.Reset(allocator, Some(&self.pipeline))?;
        }
        let source = self.source.as_ref().expect("source was checked above");
        unsafe {
            create_srv(
                &self.device,
                &self.srv_heap,
                self.srv_stride,
                frame_index as u32,
                &source.resource,
            );
            transition(
                &self.command_list,
                &source.resource,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            let backbuffer = &self.backbuffers[frame_index];
            transition(
                &self.command_list,
                backbuffer,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
            let rtv = rtv_handle(&self.rtv_heap, self.rtv_stride, frame_index as u32);
            self.command_list
                .OMSetRenderTargets(1, Some(&rtv), false, None);
            self.command_list
                .ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 0.0], None);
            self.command_list
                .SetGraphicsRootSignature(&self.root_signature);
            self.command_list
                .SetDescriptorHeaps(&[Some(self.srv_heap.clone())]);
            self.command_list.SetGraphicsRootDescriptorTable(
                0,
                gpu_srv_handle(&self.srv_heap, self.srv_stride, frame_index as u32),
            );
            self.command_list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: source.width as f32,
                Height: source.height as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            self.command_list
                .RSSetScissorRects(&[windows::Win32::Foundation::RECT {
                    left: 0,
                    top: 0,
                    right: source.width as i32,
                    bottom: source.height as i32,
                }]);
            self.command_list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.command_list.DrawInstanced(3, 1, 0, 0);
            transition(
                &self.command_list,
                backbuffer,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            );
            transition(
                &self.command_list,
                &source.resource,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
            self.command_list.Close()?;
            let list: ID3D12CommandList = self.command_list.cast()?;
            self.queue.ExecuteCommandLists(&[Some(list)]);
            self.fence_value += 1;
            self.queue.Signal(&self.fence, self.fence_value)?;
            self.frame_values[frame_index] = self.fence_value;
            self.swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        self.frame_permit = false;
        Ok(true)
    }

    fn try_begin_frame(&mut self) -> bool {
        if self.frame_permit {
            return true;
        }
        self.frame_permit = unsafe {
            WaitForSingleObject(
                self.frame_latency_waitable,
                Duration::ZERO.as_millis() as u32,
            )
        } == WAIT_OBJECT_0;
        self.frame_permit
    }

    fn wait_idle(&mut self) -> windows::core::Result<()> {
        self.fence_value += 1;
        unsafe { self.queue.Signal(&self.fence, self.fence_value)? };
        self.wait_value(self.fence_value)
    }

    fn wait_value(&self, value: u64) -> windows::core::Result<()> {
        unsafe {
            if value != 0 && self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, self.fence_event)?;
                WaitForSingleObject(self.fence_event, INFINITE);
            }
        }
        Ok(())
    }
}

impl Drop for RasterSurface {
    fn drop(&mut self) {
        let _ = self.wait_idle();
        unsafe {
            let _ = CloseHandle(self.fence_event);
            let _ = CloseHandle(self.frame_latency_waitable);
        }
    }
}

fn swapchain_description(capacity: Extent, flags: DXGI_SWAP_CHAIN_FLAG) -> DXGI_SWAP_CHAIN_DESC1 {
    DXGI_SWAP_CHAIN_DESC1 {
        Width: capacity.width,
        Height: capacity.height,
        Format: FORMAT,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: FRAME_COUNT as u32,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: flags.0 as u32,
    }
}

unsafe fn create_backbuffers(
    device: &ID3D12Device,
    swapchain: &IDXGISwapChain3,
    heap: &ID3D12DescriptorHeap,
    stride: u32,
) -> windows::core::Result<Vec<ID3D12Resource>> {
    let mut buffers = Vec::with_capacity(FRAME_COUNT);
    for index in 0..FRAME_COUNT {
        let resource = unsafe { swapchain.GetBuffer::<ID3D12Resource>(index as u32)? };
        unsafe {
            device.CreateRenderTargetView(&resource, None, rtv_handle(heap, stride, index as u32))
        };
        buffers.push(resource);
    }
    Ok(buffers)
}

unsafe fn create_srv(
    device: &ID3D12Device,
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
    resource: &ID3D12Resource,
) {
    let description = unsafe { resource.GetDesc() };
    unsafe {
        device.CreateShaderResourceView(
            resource,
            Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: description.Format,
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MipLevels: 1,
                        ..Default::default()
                    },
                },
            }),
            cpu_srv_handle(heap, stride, index),
        )
    };
}

unsafe fn transition(
    list: &ID3D12GraphicsCommandList,
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) {
    if before == after {
        return;
    }
    let mut barrier = D3D12_RESOURCE_BARRIER {
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
    };
    unsafe {
        list.ResourceBarrier(std::slice::from_ref(&barrier));
        let transition = &mut *barrier.Anonymous.Transition;
        ManuallyDrop::drop(&mut transition.pResource);
    }
}

fn create_root_signature(device: &ID3D12Device) -> windows::core::Result<ID3D12RootSignature> {
    unsafe {
        let range = D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 1,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        };
        let parameter = D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &range,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        };
        let sampler = D3D12_STATIC_SAMPLER_DESC {
            Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            MaxLOD: f32::MAX,
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
            ..Default::default()
        };
        let description = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: 1,
            pParameters: &parameter,
            NumStaticSamplers: 1,
            pStaticSamplers: &sampler,
            Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
        };
        let mut blob = None;
        D3D12SerializeRootSignature(&description, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None)?;
        let blob = blob.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER)
        })?;
        device.CreateRootSignature(
            0,
            std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()),
        )
    }
}

fn raster_shader() -> &'static [u8] {
    br#"
struct VOut { float4 pos : SV_POSITION; float2 uv : TEXCOORD0; };
VOut vs_main(uint id : SV_VertexID) {
    VOut o;
    if (id == 0) { o.pos=float4(-1, 1, 0, 1); o.uv=float2(0, 0); }
    else if (id == 1) { o.pos=float4(3, 1, 0, 1); o.uv=float2(2, 0); }
    else { o.pos=float4(-1, -3, 0, 1); o.uv=float2(0, 2); }
    return o;
}
Texture2D tex : register(t0); SamplerState samp : register(s0);
float4 ps_main(VOut i) : SV_TARGET {
    float4 result = tex.Sample(samp, i.uv);
    result.rgb *= result.a;
    return result;
}
"#
}

fn create_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
) -> windows::core::Result<ID3D12PipelineState> {
    let shader = raster_shader();
    unsafe {
        let vertex = compile(shader, b"vs_main\0", b"vs_5_0\0")?;
        let pixel = compile(shader, b"ps_main\0", b"ps_5_0\0")?;
        let mut description = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: ManuallyDrop::new(Some(root.clone())),
            VS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: vertex.GetBufferPointer(),
                BytecodeLength: vertex.GetBufferSize(),
            },
            PS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: pixel.GetBufferPointer(),
                BytecodeLength: pixel.GetBufferSize(),
            },
            BlendState: D3D12_BLEND_DESC {
                RenderTarget: [D3D12_RENDER_TARGET_BLEND_DESC {
                    RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
                    ..Default::default()
                }; 8],
                ..Default::default()
            },
            SampleMask: u32::MAX,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            DepthStencilState: D3D12_DEPTH_STENCIL_DESC::default(),
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        description.RTVFormats[0] = FORMAT;
        let pipeline = device.CreateGraphicsPipelineState(&description);
        ManuallyDrop::drop(&mut description.pRootSignature);
        pipeline
    }
}

unsafe fn compile(source: &[u8], entry: &[u8], target: &[u8]) -> windows::core::Result<ID3DBlob> {
    let mut blob = None;
    unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            None,
            None,
            None,
            PCSTR(entry.as_ptr()),
            PCSTR(target.as_ptr()),
            0,
            0,
            &mut blob,
            None,
        )?;
    }
    blob.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
}

fn rtv_handle(heap: &ID3D12DescriptorHeap, stride: u32, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let mut handle = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    handle.ptr += (stride * index) as usize;
    handle
}

fn cpu_srv_handle(
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let mut handle = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    handle.ptr += (stride * index) as usize;
    handle
}

fn gpu_srv_handle(
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    let mut handle = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
    handle.ptr += u64::from(stride * index);
    handle
}
