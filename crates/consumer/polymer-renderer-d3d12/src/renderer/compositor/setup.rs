use super::*;

impl D3d12Compositor {
    pub fn new_default(window: &impl RenderWindow) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            swapchain_target(window)?,
            SwapchainMode::Shared,
        )
    }

    pub fn new_default_with_webrtc(
        window: &impl RenderWindow,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let compositor = Self::new_default(window)?;
        let webrtc = WebRtcCompositor::new_d3d12(&compositor)?;
        Ok((compositor, webrtc))
    }

    /// Creates a compositor whose swapchain can be attached to an
    /// `IDCompositionVisual` with [`Self::composition_content`].
    pub fn new_default_for_composition(window: &impl RenderWindow) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            SwapchainTarget::Composition {
                transparent: false,
                sync_interval: 0,
            },
            SwapchainMode::Shared,
        )
    }

    /// Creates the window-spanning composition swapchain on DXGI's default
    /// hardware adapter. It can render chrome before any captured stream has
    /// identified the producer adapter.
    pub fn new_default_for_shared_composition(
        window: &impl RenderWindow,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            SwapchainTarget::Composition {
                transparent: false,
                sync_interval: 1,
            },
            SwapchainMode::Shared,
        )
    }

    /// Creates a transparent composition swapchain suitable for an overlay
    /// visual. Its backbuffer alpha is interpreted as premultiplied alpha by
    /// DirectComposition.
    pub fn new_default_for_transparent_composition(
        window: &impl RenderWindow,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            SwapchainTarget::Composition {
                transparent: true,
                sync_interval: 0,
            },
            SwapchainMode::Shared,
        )
    }

    pub fn new_default_for_composition_with_webrtc(
        window: &impl RenderWindow,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let compositor = Self::new_default_for_composition(window)?;
        let webrtc = WebRtcCompositor::new_d3d12(&compositor)?;
        Ok((compositor, webrtc))
    }

    pub fn new(window: &impl RenderWindow, adapter: AdapterLuid) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new(adapter)?,
            swapchain_target(window)?,
            SwapchainMode::Shared,
        )
    }

    pub fn new_with_webrtc(
        window: &impl RenderWindow,
        adapter: AdapterLuid,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let compositor = Self::new(window, adapter)?;
        let webrtc = WebRtcCompositor::new_d3d12(&compositor)?;
        Ok((compositor, webrtc))
    }

    /// Creates an adapter-specific composition swapchain dedicated to one
    /// connection. It owns a separate D3D12 queue so unrelated UI raster and
    /// presents cannot serialize ahead of the guest frame.
    pub fn new_for_stream_composition(
        window: &impl RenderWindow,
        adapter: AdapterLuid,
        connection: u64,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new(adapter)?,
            SwapchainTarget::Composition {
                transparent: false,
                sync_interval: 0,
            },
            SwapchainMode::Dedicated(connection),
        )
    }

    pub(super) fn with_consumer(
        window: &impl RenderWindow,
        consumer: D3d12ConsumerDevice,
        target: SwapchainTarget,
        mode: SwapchainMode,
    ) -> Result<Self, Box<dyn Error>> {
        let size = window.render_extent();
        let extent = (size.width.max(1), size.height.max(1));

        // SAFETY: all COM objects and the event are owned by this compositor and GPU work is
        // drained before resources referenced by a submitted command list are replaced.
        unsafe {
            let present_interval = match target {
                SwapchainTarget::Hwnd(_) => 1,
                SwapchainTarget::Composition { sync_interval, .. } => sync_interval,
            };
            let swapchain_flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
            let description = DXGI_SWAP_CHAIN_DESC1 {
                Width: extent.0,
                Height: extent.1,
                Format: SWAPCHAIN_FORMAT,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: FRAME_COUNT as u32,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: match target {
                    SwapchainTarget::Hwnd(_) => DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    // DXGI requires the sequential flip model for swapchains
                    // supplied as DirectComposition visual content.
                    SwapchainTarget::Composition { .. } => DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                },
                AlphaMode: match target {
                    SwapchainTarget::Composition {
                        transparent: true, ..
                    } => DXGI_ALPHA_MODE_PREMULTIPLIED,
                    _ => DXGI_ALPHA_MODE_IGNORE,
                },
                Flags: swapchain_flags.0 as u32,
            };
            let swapchain1 = match target {
                SwapchainTarget::Hwnd(hwnd) => {
                    let swapchain = consumer.factory().CreateSwapChainForHwnd(
                        consumer.queue(),
                        hwnd,
                        &description,
                        None,
                        None,
                    )?;
                    consumer
                        .factory()
                        .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)?;
                    swapchain
                }
                SwapchainTarget::Composition { .. } => consumer
                    .factory()
                    .CreateSwapChainForComposition(consumer.queue(), &description, None)?,
            };
            let swapchain: IDXGISwapChain3 = swapchain1.cast()?;
            let swapchain2: IDXGISwapChain2 = swapchain.cast()?;
            swapchain2.SetMaximumFrameLatency(1)?;
            let handle = swapchain2.GetFrameLatencyWaitableObject();
            if handle.is_invalid() {
                return Err("DXGI did not create a frame-latency waitable object".into());
            }
            let frame_latency_waitable = Some(handle);

            let rtv_heap = consumer
                .device()
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: FRAME_COUNT as u32,
                    ..Default::default()
                })?;
            let srv_heap = consumer
                .device()
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: DESCRIPTOR_COUNT,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    ..Default::default()
                })?;
            let rtv_stride = consumer
                .device()
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);
            let srv_stride = consumer
                .device()
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            let backbuffers =
                create_backbuffers(consumer.device(), &swapchain, &rtv_heap, rtv_stride)?;
            let root_signature = create_root_signature(consumer.device())?;
            let content_pipeline = create_pipeline(consumer.device(), &root_signature, false)?;
            let blend_pipeline = create_pipeline(consumer.device(), &root_signature, true)?;
            let mut allocators = Vec::with_capacity(FRAME_COUNT);
            for _ in 0..FRAME_COUNT {
                allocators.push(
                    consumer
                        .device()
                        .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?,
                );
            }
            let command_list: ID3D12GraphicsCommandList = consumer.device().CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &allocators[0],
                Some(&content_pipeline),
            )?;
            command_list.SetName(windows::core::w!("Polymer compositor"))?;
            command_list.Close()?;
            let fence = consumer.device().CreateFence(0, D3D12_FENCE_FLAG_NONE)?;

            Ok(Self {
                receiver: D3d12Receiver::from_consumer(consumer),
                mode,
                swapchain,
                swapchain_flags,
                present_interval,
                frame_latency_waitable,
                frame_permit: false,
                removal_reported: false,
                extent,
                backbuffers,
                rtv_heap,
                rtv_stride,
                srv_heap,
                srv_stride,
                root_signature,
                content_pipeline,
                blend_pipeline,
                allocators,
                command_list,
                fence,
                fence_waiter: FenceWaiter::new()?,
                fence_value: 0,
                frame_values: [0; FRAME_COUNT],
                frame_garbage: std::array::from_fn(|_| Vec::new()),
                frame_uploads: std::array::from_fn(|_| UploadArena::new()),
                surfaces: Vec::new(),
                display_latency: DisplayLatency::default(),
                counter_frequency: counter_frequency(),
            })
        }
    }
}

pub(super) fn swapchain_target(
    window: &impl RenderWindow,
) -> Result<SwapchainTarget, Box<dyn Error>> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(SwapchainTarget::Hwnd(HWND(
            handle.hwnd.get() as *mut c_void
        ))),
        _ => Err("D3D12 composition requires a Win32 window".into()),
    }
}

pub(super) unsafe fn create_backbuffers(
    device: &ID3D12Device,
    swapchain: &IDXGISwapChain3,
    heap: &ID3D12DescriptorHeap,
    stride: u32,
) -> windows::core::Result<Vec<ID3D12Resource>> {
    let mut result = Vec::with_capacity(FRAME_COUNT);
    for index in 0..FRAME_COUNT {
        let resource = unsafe { swapchain.GetBuffer::<ID3D12Resource>(index as u32)? };
        unsafe {
            device.CreateRenderTargetView(&resource, None, rtv_handle(heap, stride, index as u32))
        };
        result.push(resource);
    }
    Ok(result)
}

pub(super) fn create_root_signature(
    device: &ID3D12Device,
) -> windows::core::Result<ID3D12RootSignature> {
    unsafe {
        let range = D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 1,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        };
        let table = D3D12_ROOT_DESCRIPTOR_TABLE {
            NumDescriptorRanges: 1,
            pDescriptorRanges: &range,
        };
        let parameter = D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: table,
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
        let desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: 1,
            pParameters: &parameter,
            NumStaticSamplers: 1,
            pStaticSamplers: &sampler,
            Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
        };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None)?;
        let blob = blob.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER)
        })?;
        device.CreateRootSignature(
            0,
            std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()),
        )
    }
}

pub(super) fn compositor_shader() -> &'static [u8] {
    br#"
struct VIn {
    float2 pos : POSITION;
    float2 uv : TEXCOORD0;
    float4 color : COLOR0;
    float2 local : TEXCOORD1;
    float4 clip : TEXCOORD2;
};
struct VOut {
    float4 pos : SV_POSITION;
    float2 uv : TEXCOORD0;
    float4 color : COLOR0;
    float2 local : TEXCOORD1;
    float4 clip : TEXCOORD2;
};
VOut vs_main(VIn v) {
    VOut o;
    o.pos=float4(v.pos,0,1);
    o.uv=v.uv;
    o.color=v.color;
    o.local=v.local;
    o.clip=v.clip;
    return o;
}
Texture2D tex : register(t0); SamplerState samp : register(s0);
float4 ps_main(VOut i) : SV_TARGET {
    float4 result = tex.Sample(samp, i.uv) * float4(i.color.rgb, 1.0);
    if (i.clip.w > 0.5) {
        result.rgb *= result.a;
    }
    result *= i.color.a;
    if (i.clip.z > 0.0) {
        float radius = min(i.clip.z, min(i.clip.x, i.clip.y) * 0.5);
        float2 half_size = i.clip.xy * 0.5;
        float2 q = abs(i.local - half_size) - (half_size - radius);
        float distance = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
        float coverage = saturate(0.5 - distance / max(fwidth(distance), 0.0001));
        result *= coverage;
    }
    return result;
}
"#
}

pub(super) fn create_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    alpha_blend: bool,
) -> windows::core::Result<ID3D12PipelineState> {
    let shader = compositor_shader();
    unsafe {
        let vs = compile(shader, b"vs_main\0", b"vs_5_0\0")?;
        let ps = compile(shader, b"ps_main\0", b"ps_5_0\0")?;
        let input = [
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(c"POSITION".as_ptr().cast()),
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(c"TEXCOORD".as_ptr().cast()),
                Format: DXGI_FORMAT_R32G32_FLOAT,
                AlignedByteOffset: 8,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(c"COLOR".as_ptr().cast()),
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                AlignedByteOffset: 16,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(c"TEXCOORD".as_ptr().cast()),
                SemanticIndex: 1,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                AlignedByteOffset: 20,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(c"TEXCOORD".as_ptr().cast()),
                SemanticIndex: 2,
                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                AlignedByteOffset: 28,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            },
        ];
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: ManuallyDrop::new(Some(root.clone())),
            VS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: vs.GetBufferPointer(),
                BytecodeLength: vs.GetBufferSize(),
            },
            PS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: ps.GetBufferPointer(),
                BytecodeLength: ps.GetBufferSize(),
            },
            BlendState: D3D12_BLEND_DESC {
                RenderTarget: [D3D12_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: alpha_blend.into(),
                    SrcBlend: D3D12_BLEND_ONE,
                    DestBlend: D3D12_BLEND_INV_SRC_ALPHA,
                    BlendOp: D3D12_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D12_BLEND_ONE,
                    DestBlendAlpha: D3D12_BLEND_INV_SRC_ALPHA,
                    BlendOpAlpha: D3D12_BLEND_OP_ADD,
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
            InputLayout: D3D12_INPUT_LAYOUT_DESC {
                pInputElementDescs: input.as_ptr(),
                NumElements: input.len() as u32,
            },
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        desc.RTVFormats[0] = SWAPCHAIN_FORMAT;
        let result = device.CreateGraphicsPipelineState(&desc);
        ManuallyDrop::drop(&mut desc.pRootSignature);
        result
    }
}

pub(super) unsafe fn compile(
    source: &[u8],
    entry: &[u8],
    target: &[u8],
) -> windows::core::Result<ID3DBlob> {
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
