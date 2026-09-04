//! Egui rendering commands appended to a Polymer D3D12 frame.
#![allow(unsafe_code)]

use std::{
    collections::HashMap,
    error::Error,
    mem::{ManuallyDrop, size_of},
    ptr,
};

use egui::{ClippedPrimitive, TextureId, epaint::Primitive};
use polymer_renderer::renderer::d3d12::D3d12RenderPass;
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile, ID3DBlob},
            Direct3D12::*,
            Dxgi::Common::*,
        },
    },
    core::{PCSTR, Result as WindowsResult},
};

const DESCRIPTOR_COUNT: u32 = 1_024;

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

struct Texture {
    resource: ID3D12Resource,
    pixels: Vec<u8>,
    size: [usize; 2],
    descriptor: u32,
}

struct UploadArena {
    buffer: Option<ID3D12Resource>,
    address: u64,
    mapped: usize,
    capacity: u64,
    used: u64,
}

impl UploadArena {
    const ALIGNMENT: u64 = 16;
    const INITIAL_CAPACITY: u64 = 256 * 1_024;

    const fn new() -> Self {
        Self {
            buffer: None,
            address: 0,
            mapped: 0,
            capacity: 0,
            used: 0,
        }
    }

    const fn reset(&mut self) {
        self.used = 0;
    }

    const fn placement(used: u64, capacity: u64, length: u64) -> Option<u64> {
        let Some(offset) = used.checked_next_multiple_of(Self::ALIGNMENT) else {
            return None;
        };
        match offset.checked_add(length) {
            Some(end) if end <= capacity => Some(offset),
            _ => None,
        }
    }

    const fn grown_capacity(capacity: u64, length: u64) -> u64 {
        let doubled = capacity.saturating_mul(2);
        let floor = if doubled > Self::INITIAL_CAPACITY {
            doubled
        } else {
            Self::INITIAL_CAPACITY
        };
        let needed = length.next_multiple_of(Self::ALIGNMENT);
        if floor > needed { floor } else { needed }
    }

    fn write(
        &mut self,
        device: &ID3D12Device,
        bytes: &[u8],
        garbage: &mut Vec<ID3D12Resource>,
    ) -> WindowsResult<u64> {
        let length = (bytes.len() as u64).max(1);
        let mut offset = Self::placement(self.used, self.capacity, length);
        if self.buffer.is_none() || offset.is_none() {
            self.replace(device, Self::grown_capacity(self.capacity, length), garbage)?;
            offset = Self::placement(0, self.capacity, length);
        }
        let offset = offset.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_OUTOFMEMORY)
        })?;
        // SAFETY: the persistent mapping covers the resource, and placement
        // checked that this write fits.
        unsafe {
            ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (self.mapped as *mut u8).add(offset as usize),
                bytes.len(),
            );
        }
        self.used = offset + length;
        Ok(self.address + offset)
    }

    fn replace(
        &mut self,
        device: &ID3D12Device,
        capacity: u64,
        garbage: &mut Vec<ID3D12Resource>,
    ) -> WindowsResult<()> {
        // SAFETY: the upload resource remains mapped and owned by this arena.
        let (buffer, address, mapped) = unsafe {
            let buffer = create_upload_buffer(device, capacity)?;
            let mut mapped = ptr::null_mut();
            buffer.Map(
                0,
                Some(&D3D12_RANGE { Begin: 0, End: 0 }),
                Some(&mut mapped),
            )?;
            (buffer.clone(), buffer.GetGPUVirtualAddress(), mapped)
        };
        self.address = address;
        self.mapped = mapped as usize;
        self.capacity = capacity;
        self.used = 0;
        if let Some(previous) = self.buffer.replace(buffer) {
            garbage.push(previous);
        }
        Ok(())
    }
}

struct FrameResources {
    uploads: UploadArena,
    garbage: Vec<ID3D12Resource>,
    descriptors: Vec<u32>,
}

impl FrameResources {
    const fn new() -> Self {
        Self {
            uploads: UploadArena::new(),
            garbage: Vec::new(),
            descriptors: Vec::new(),
        }
    }
}

/// D3D12 egui resources owned by a viewer rather than the game compositor.
pub struct D3d12EguiPainter {
    device: ID3D12Device,
    root_signature: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
    srv_heap: ID3D12DescriptorHeap,
    srv_stride: u32,
    textures: HashMap<TextureId, Texture>,
    free_descriptors: Vec<u32>,
    frames: Vec<FrameResources>,
}

impl D3d12EguiPainter {
    pub fn new(
        device: ID3D12Device,
        target_format: DXGI_FORMAT,
        frames_in_flight: usize,
    ) -> Result<Self, Box<dyn Error>> {
        // SAFETY: every created D3D object is retained by the painter.
        unsafe {
            let root_signature = create_root_signature(&device)?;
            let pipeline = create_pipeline(&device, &root_signature, target_format)?;
            let srv_heap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: DESCRIPTOR_COUNT,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                ..Default::default()
            })?;
            let srv_stride =
                device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            Ok(Self {
                device,
                root_signature,
                pipeline,
                srv_heap,
                srv_stride,
                textures: HashMap::new(),
                free_descriptors: (0..DESCRIPTOR_COUNT).rev().collect(),
                frames: (0..frames_in_flight)
                    .map(|_| FrameResources::new())
                    .collect(),
            })
        }
    }

    pub fn paint(
        &mut self,
        pass: &mut D3d12RenderPass<'_>,
        output: &egui::FullOutput,
        primitives: &[ClippedPrimitive],
    ) -> Result<(), Box<dyn Error>> {
        let frame_index = pass.frame_index();
        {
            let frame = self
                .frames
                .get_mut(frame_index)
                .ok_or("D3D12 egui frame index is outside the painter's flight slots")?;
            frame.garbage.clear();
            frame.uploads.reset();
            self.free_descriptors.append(&mut frame.descriptors);
        }

        self.upload_texture_deltas(frame_index, pass.command_list(), &output.textures_delta.set)?;

        let command_list = pass.command_list();
        // SAFETY: the compositor's command list is open with a render target
        // bound for the lifetime of the pass.
        unsafe {
            command_list.SetPipelineState(&self.pipeline);
            command_list.SetGraphicsRootSignature(&self.root_signature);
            command_list.SetDescriptorHeaps(&[Some(self.srv_heap.clone())]);
            command_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        }

        let extent = pass.extent();
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                continue;
            };
            let Some(descriptor) = self
                .textures
                .get(&mesh.texture_id)
                .map(|texture| texture.descriptor)
            else {
                continue;
            };
            let vertices = mesh
                .vertices
                .iter()
                .map(|vertex| Vertex {
                    position: [
                        vertex.pos.x * output.pixels_per_point / extent.0 as f32 * 2.0 - 1.0,
                        1.0 - vertex.pos.y * output.pixels_per_point / extent.1 as f32 * 2.0,
                    ],
                    uv: [vertex.uv.x, vertex.uv.y],
                    color: vertex.color.to_array(),
                })
                .collect::<Vec<_>>();
            let clip = primitive.clip_rect;
            let scissor = RECT {
                left: (clip.min.x * output.pixels_per_point).floor().max(0.0) as i32,
                top: (clip.min.y * output.pixels_per_point).floor().max(0.0) as i32,
                right: (clip.max.x * output.pixels_per_point)
                    .ceil()
                    .min(extent.0 as f32) as i32,
                bottom: (clip.max.y * output.pixels_per_point)
                    .ceil()
                    .min(extent.1 as f32) as i32,
            };
            self.draw_indexed(
                frame_index,
                command_list,
                extent,
                descriptor,
                &vertices,
                &mesh.indices,
                scissor,
            )?;
        }

        for texture_id in &output.textures_delta.free {
            if let Some(texture) = self.textures.remove(texture_id) {
                let frame = &mut self.frames[frame_index];
                frame.descriptors.push(texture.descriptor);
                frame.garbage.push(texture.resource);
            }
        }
        Ok(())
    }

    fn draw_indexed(
        &mut self,
        frame_index: usize,
        command_list: &ID3D12GraphicsCommandList,
        extent: (u32, u32),
        descriptor: u32,
        vertices: &[Vertex],
        indices: &[u32],
        scissor: RECT,
    ) -> Result<(), Box<dyn Error>> {
        let frame = &mut self.frames[frame_index];
        let vbv = D3D12_VERTEX_BUFFER_VIEW {
            BufferLocation: frame.uploads.write(
                &self.device,
                as_bytes(vertices),
                &mut frame.garbage,
            )?,
            SizeInBytes: size_of_val(vertices) as u32,
            StrideInBytes: size_of::<Vertex>() as u32,
        };
        let ibv = D3D12_INDEX_BUFFER_VIEW {
            BufferLocation: frame.uploads.write(
                &self.device,
                as_bytes(indices),
                &mut frame.garbage,
            )?,
            SizeInBytes: size_of_val(indices) as u32,
            Format: DXGI_FORMAT_R32_UINT,
        };
        // SAFETY: views point into this frame slot's retained upload arena.
        unsafe {
            command_list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: extent.0 as f32,
                Height: extent.1 as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            command_list.RSSetScissorRects(&[scissor]);
            command_list.SetGraphicsRootDescriptorTable(
                0,
                gpu_handle(&self.srv_heap, self.srv_stride, descriptor),
            );
            command_list.IASetVertexBuffers(0, Some(&[vbv]));
            command_list.IASetIndexBuffer(Some(&ibv));
            command_list.DrawIndexedInstanced(indices.len() as u32, 1, 0, 0, 0);
        }
        Ok(())
    }

    fn upload_texture_deltas(
        &mut self,
        frame_index: usize,
        command_list: &ID3D12GraphicsCommandList,
        deltas: &[(TextureId, egui::epaint::ImageDelta)],
    ) -> Result<(), Box<dyn Error>> {
        for (id, delta) in deltas {
            let patch_size = delta.image.size();
            let patch = match &delta.image {
                egui::ImageData::Color(image) => image
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect::<Vec<_>>(),
                egui::ImageData::Font(image) => image
                    .srgba_pixels(None)
                    .flat_map(|pixel| pixel.to_array())
                    .collect::<Vec<_>>(),
            };
            let (size, mut pixels) = if let Some(position) = delta.pos {
                let old = self.textures.get(id);
                let size = [
                    old.map_or(0, |texture| texture.size[0])
                        .max(position[0] + patch_size[0]),
                    old.map_or(0, |texture| texture.size[1])
                        .max(position[1] + patch_size[1]),
                ];
                let mut pixels = vec![0; size[0] * size[1] * 4];
                if let Some(old) = old {
                    for row in 0..old.size[1] {
                        let source = row * old.size[0] * 4;
                        let destination = row * size[0] * 4;
                        pixels[destination..destination + old.size[0] * 4]
                            .copy_from_slice(&old.pixels[source..source + old.size[0] * 4]);
                    }
                }
                for row in 0..patch_size[1] {
                    let destination = ((position[1] + row) * size[0] + position[0]) * 4;
                    let source = row * patch_size[0] * 4;
                    pixels[destination..destination + patch_size[0] * 4]
                        .copy_from_slice(&patch[source..source + patch_size[0] * 4]);
                }
                (size, pixels)
            } else {
                (patch_size, patch)
            };

            let descriptor = self
                .free_descriptors
                .pop()
                .ok_or("D3D12 egui descriptor heap exhausted")?;
            // SAFETY: the copy is recorded into the open frame command list,
            // and returned resources are retained until this slot completes.
            let (resource, upload) =
                unsafe { upload_texture(&self.device, command_list, size, &pixels)? };
            // SAFETY: the descriptor is unused and the resource outlives it.
            unsafe {
                create_srv(
                    &self.device,
                    &self.srv_heap,
                    self.srv_stride,
                    descriptor,
                    &resource,
                )
            };
            if let Some(old) = self.textures.insert(
                *id,
                Texture {
                    resource,
                    pixels: std::mem::take(&mut pixels),
                    size,
                    descriptor,
                },
            ) {
                self.frames[frame_index].descriptors.push(old.descriptor);
                self.frames[frame_index].garbage.push(old.resource);
            }
            self.frames[frame_index].garbage.push(upload);
        }
        Ok(())
    }
}

unsafe fn create_root_signature(device: &ID3D12Device) -> WindowsResult<ID3D12RootSignature> {
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
    let description = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: 1,
        pParameters: &parameter,
        NumStaticSamplers: 1,
        pStaticSamplers: &sampler,
        Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
    };
    let mut blob = None;
    // SAFETY: every pointer in the description refers to locals alive for the call.
    unsafe {
        D3D12SerializeRootSignature(&description, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None)?;
    }
    let blob = blob
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))?;
    // SAFETY: the blob owns the byte range for the duration of creation.
    unsafe {
        device.CreateRootSignature(
            0,
            std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()),
        )
    }
}

fn shader() -> &'static [u8] {
    br#"
struct VIn { float2 pos : POSITION; float2 uv : TEXCOORD0; float4 color : COLOR0; };
struct VOut { float4 pos : SV_POSITION; float2 uv : TEXCOORD0; float4 color : COLOR0; };
VOut vs_main(VIn v) {
    VOut o; o.pos=float4(v.pos,0,1); o.uv=v.uv; o.color=v.color; return o;
}
Texture2D tex : register(t0); SamplerState samp : register(s0);
float4 ps_main(VOut i) : SV_TARGET { return tex.Sample(samp, i.uv) * i.color; }
"#
}

unsafe fn create_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    target_format: DXGI_FORMAT,
) -> WindowsResult<ID3D12PipelineState> {
    let vertex = unsafe { compile(shader(), b"vs_main\0", b"vs_5_0\0")? };
    let pixel = unsafe { compile(shader(), b"ps_main\0", b"ps_5_0\0")? };
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
    ];
    let mut description = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        pRootSignature: ManuallyDrop::new(Some(root.clone())),
        VS: D3D12_SHADER_BYTECODE {
            pShaderBytecode: unsafe { vertex.GetBufferPointer() },
            BytecodeLength: unsafe { vertex.GetBufferSize() },
        },
        PS: D3D12_SHADER_BYTECODE {
            pShaderBytecode: unsafe { pixel.GetBufferPointer() },
            BytecodeLength: unsafe { pixel.GetBufferSize() },
        },
        BlendState: D3D12_BLEND_DESC {
            RenderTarget: [D3D12_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
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
    description.RTVFormats[0] = target_format;
    // SAFETY: all pointer-backed members remain alive for the call.
    let result = unsafe { device.CreateGraphicsPipelineState(&description) };
    // SAFETY: release the manually-held COM pointer once the call is complete.
    unsafe {
        ManuallyDrop::drop(&mut description.pRootSignature);
    }
    result
}

unsafe fn compile(source: &[u8], entry: &[u8], target: &[u8]) -> WindowsResult<ID3DBlob> {
    let mut blob = None;
    // SAFETY: byte slices and nul-terminated entry/target strings are live.
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

unsafe fn upload_texture(
    device: &ID3D12Device,
    command_list: &ID3D12GraphicsCommandList,
    size: [usize; 2],
    pixels: &[u8],
) -> WindowsResult<(ID3D12Resource, ID3D12Resource)> {
    let texture = unsafe {
        create_texture(
            device,
            size[0] as u32,
            size[1] as u32,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            D3D12_RESOURCE_STATE_COPY_DEST,
        )?
    };
    let pitch = (size[0] * 4).div_ceil(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize)
        * D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize;
    let upload = unsafe { create_upload_buffer(device, (pitch * size[1]) as u64)? };
    let mut mapped = ptr::null_mut();
    unsafe {
        upload.Map(0, None, Some(&mut mapped))?;
        for row in 0..size[1] {
            ptr::copy_nonoverlapping(
                pixels.as_ptr().add(row * size[0] * 4),
                mapped.cast::<u8>().add(row * pitch),
                size[0] * 4,
            );
        }
        upload.Unmap(0, None);
    }
    let mut destination = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(texture.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    };
    let mut source = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(upload.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: 0,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    Width: size[0] as u32,
                    Height: size[1] as u32,
                    Depth: 1,
                    RowPitch: pitch as u32,
                },
            },
        },
    };
    unsafe {
        command_list.CopyTextureRegion(&destination, 0, 0, 0, &source, None);
        transition(
            command_list,
            &texture,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
        );
        ManuallyDrop::drop(&mut destination.pResource);
        ManuallyDrop::drop(&mut source.pResource);
    }
    Ok((texture, upload))
}

unsafe fn create_texture(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    state: D3D12_RESOURCE_STATES,
) -> WindowsResult<ID3D12Resource> {
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: u64::from(width),
                Height: height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_NONE,
                Alignment: 0,
            },
            state,
            None,
            &mut resource,
        )?;
    }
    resource
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
}

unsafe fn create_upload_buffer(device: &ID3D12Device, size: u64) -> WindowsResult<ID3D12Resource> {
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_UPLOAD,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(1),
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
            D3D12_RESOURCE_STATE_GENERIC_READ,
            None,
            &mut resource,
        )?;
    }
    resource
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
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
            cpu_handle(heap, stride, index),
        );
    }
}

unsafe fn transition(
    command_list: &ID3D12GraphicsCommandList,
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) {
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
        command_list.ResourceBarrier(std::slice::from_ref(&barrier));
        let transition = &mut *barrier.Anonymous.Transition;
        ManuallyDrop::drop(&mut transition.pResource);
    }
}

fn cpu_handle(heap: &ID3D12DescriptorHeap, stride: u32, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let mut handle = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    handle.ptr += (stride * index) as usize;
    handle
}

fn gpu_handle(heap: &ID3D12DescriptorHeap, stride: u32, index: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    let mut handle = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
    handle.ptr += u64::from(stride * index);
    handle
}

fn as_bytes<T>(items: &[T]) -> &[u8] {
    // SAFETY: this byte view has the exact lifetime and size of the source.
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast(), size_of_val(items)) }
}

#[cfg(test)]
mod tests {
    use super::{compile, shader};

    #[test]
    fn painter_shaders_compile() {
        // SAFETY: compilation only reads the static shader and entry strings.
        unsafe {
            compile(shader(), b"vs_main\0", b"vs_5_0\0").expect("vertex shader compiles");
            compile(shader(), b"ps_main\0", b"ps_5_0\0").expect("pixel shader compiles");
        }
    }
}
