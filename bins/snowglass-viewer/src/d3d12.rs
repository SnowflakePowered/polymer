//! Native D3D12/DXGI viewer compositor.
#![allow(unsafe_code)]

use std::{
    collections::HashMap,
    error::Error,
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr,
};

use egui::{ClippedPrimitive, TextureId, epaint::Primitive};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use snowglass_consumer_d3d12::{D3d12ConsumerDevice, ImportedD3d12Stream};
use snowglass_ipc::StreamReady;
use snowglass_protocol::{AdapterLuid, FrameReady, RELEASE_ALL_SYNC_VALUE};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, RECT},
        Graphics::{
            Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile, ID3DBlob},
            Direct3D12::*,
            Dxgi::{Common::*, *},
        },
        System::Threading::{CreateEventW, INFINITE, WaitForSingleObject},
    },
    core::{Interface, PCSTR},
};
use winit::window::Window;

const FRAME_COUNT: usize = 2;
const DESCRIPTOR_COUNT: u32 = 1024;

const KEYED_MUTEX_TIMEOUT_MS: u32 = 100;
const FIRST_CONTENT_DESCRIPTOR: u32 = 0;
const FIRST_EGUI_DESCRIPTOR: u32 = 64;
const SWAPCHAIN_FORMAT: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM;

#[repr(C)]
#[derive(Clone, Copy)]
struct GpuVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

struct EguiTexture {
    resource: ID3D12Resource,
    pixels: Vec<u8>,
    size: [usize; 2],
    descriptor: u32,
}

struct ImportedSurface {
    connection: u64,
    descriptor: u32,
    imported: ImportedD3d12Stream,
    cache: ID3D12Resource,
    cache_state: D3D12_RESOURCE_STATES,
    latest: Option<FrameReady>,
    display_frame: Option<FrameReady>,
    video_completion_value: u64,
}

pub(crate) struct D3d12Compositor {
    consumer: D3d12ConsumerDevice,
    swapchain: IDXGISwapChain3,
    extent: (u32, u32),
    backbuffers: Vec<ID3D12Resource>,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_stride: u32,
    srv_heap: ID3D12DescriptorHeap,
    srv_stride: u32,
    root_signature: ID3D12RootSignature,
    content_pipeline: ID3D12PipelineState,
    egui_pipeline: ID3D12PipelineState,
    allocators: Vec<ID3D12CommandAllocator>,
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_event: HANDLE,
    fence_value: u64,
    frame_values: [u64; FRAME_COUNT],
    frame_garbage: [Vec<ID3D12Resource>; FRAME_COUNT],
    frame_descriptors: [Vec<u32>; FRAME_COUNT],
    surfaces: Vec<ImportedSurface>,
    // Standalone copy submission used to bound keyed-mutex holds; the main
    // command list executes too late to release the mutex promptly.
    mutex_copy: Option<(ID3D12CommandAllocator, ID3D12GraphicsCommandList)>,
    textures: HashMap<TextureId, EguiTexture>,
    free_descriptors: Vec<u32>,
    publish_video: bool,
    video_completion_fence: ID3D12Fence,
    video_completion_value: u64,
    completed_video: Vec<super::webrtc::VideoFrame>,
}

impl D3d12Compositor {
    pub(crate) fn new(
        window: &Window,
        adapter: AdapterLuid,
        publish_video: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let consumer = D3d12ConsumerDevice::new(adapter)?;
        let size = window.inner_size();
        let extent = (size.width.max(1), size.height.max(1));
        let hwnd = match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get() as *mut c_void),
            _ => return Err("D3D12 composition requires a Win32 window".into()),
        };

        // SAFETY: all COM objects and the event are owned by this compositor and GPU work is
        // drained before resources referenced by a submitted command list are replaced.
        unsafe {
            let swapchain1 = consumer.factory().CreateSwapChainForHwnd(
                consumer.queue(),
                hwnd,
                &DXGI_SWAP_CHAIN_DESC1 {
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
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                    Flags: 0,
                },
                None,
                None,
            )?;
            consumer
                .factory()
                .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)?;
            let swapchain: IDXGISwapChain3 = swapchain1.cast()?;

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
            let egui_pipeline = create_pipeline(consumer.device(), &root_signature, true)?;
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
                Some(&egui_pipeline),
            )?;
            command_list.Close()?;
            let fence = consumer.device().CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let video_completion_fence = consumer.device().CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, None)?;

            Ok(Self {
                consumer,
                swapchain,
                extent,
                backbuffers,
                rtv_heap,
                rtv_stride,
                srv_heap,
                srv_stride,
                root_signature,
                content_pipeline,
                egui_pipeline,
                allocators,
                command_list,
                fence,
                fence_event,
                fence_value: 0,
                frame_values: [0; FRAME_COUNT],
                frame_garbage: std::array::from_fn(|_| Vec::new()),
                frame_descriptors: std::array::from_fn(|_| Vec::new()),
                surfaces: Vec::new(),
                mutex_copy: None,
                textures: HashMap::new(),
                free_descriptors: (FIRST_EGUI_DESCRIPTOR..DESCRIPTOR_COUNT).rev().collect(),
                publish_video,
                video_completion_fence,
                video_completion_value: 0,
                completed_video: Vec::new(),
            })
        }
    }

    pub(crate) fn adapter_luid(&self) -> AdapterLuid {
        self.consumer.adapter_luid()
    }

    pub(crate) fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
    ) -> Result<(), Box<dyn Error>> {
        self.disconnect_stream(connection)?;
        let imported = self.consumer.import(stream)?;
        if imported.uses_keyed_mutex() && self.mutex_copy.is_none() {
            unsafe {
                let allocator: ID3D12CommandAllocator = self
                    .consumer
                    .device()
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
                let list: ID3D12GraphicsCommandList = self.consumer.device().CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None,
                )?;
                list.Close()?;
                self.mutex_copy = Some((allocator, list));
            }
        }
        // The private texture decouples viewer redraws from producer slot reuse.
        let cache = unsafe {
            create_texture(
                self.consumer.device(),
                imported.width,
                imported.height,
                DXGI_FORMAT(imported.format as i32),
                D3D12_RESOURCE_STATE_COMMON,
            )?
        };
        unsafe {
            create_srv(
                self.consumer.device(),
                &self.srv_heap,
                self.srv_stride,
                FIRST_CONTENT_DESCRIPTOR + self.surfaces.len() as u32,
                &cache,
            )
        };
        self.surfaces.push(ImportedSurface {
            connection,
            descriptor: FIRST_CONTENT_DESCRIPTOR + self.surfaces.len() as u32,
            imported,
            cache,
            cache_state: D3D12_RESOURCE_STATE_COMMON,
            latest: None,
            display_frame: None,
            video_completion_value: 0,
        });
        Ok(())
    }

    pub(crate) fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.connection == connection)
        else {
            return;
        };
        if let Some(previous) = surface.latest.replace(frame)
            && previous.slot != frame.slot
        {
            surface.imported.recycle_slot(previous.slot);
        }
    }
    pub(crate) fn has_pending_frame(&self) -> bool {
        self.surfaces.iter().any(|surface| surface.latest.is_some())
    }

    pub(crate) fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.connection == connection)
        {
            let surface = self.surfaces.remove(index);
            surface
                .imported
                .signal_release(self.consumer.queue(), RELEASE_ALL_SYNC_VALUE)?;
            self.wait_idle()?;
            self.refresh_surface_descriptors()?;
        }
        Ok(())
    }

    fn release_imported_streams(&mut self) -> windows::core::Result<()> {
        for surface in &self.surfaces {
            surface
                .imported
                .signal_release(self.consumer.queue(), RELEASE_ALL_SYNC_VALUE)?;
        }
        self.wait_idle()?;
        self.surfaces.clear();
        Ok(())
    }

    fn refresh_surface_descriptors(&mut self) -> windows::core::Result<()> {
        for (index, surface) in self.surfaces.iter_mut().enumerate() {
            surface.descriptor = FIRST_CONTENT_DESCRIPTOR + index as u32;
            unsafe {
                create_srv(
                    self.consumer.device(),
                    &self.srv_heap,
                    self.srv_stride,
                    surface.descriptor,
                    &surface.cache,
                )
            };
        }
        Ok(())
    }

    pub(crate) fn resize(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(false);
        }
        self.wait_idle()?;
        self.backbuffers.clear();
        unsafe {
            self.swapchain.ResizeBuffers(
                FRAME_COUNT as u32,
                size.width,
                size.height,
                SWAPCHAIN_FORMAT,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
            self.backbuffers = create_backbuffers(
                self.consumer.device(),
                &self.swapchain,
                &self.rtv_heap,
                self.rtv_stride,
            )?;
        }
        self.extent = (size.width, size.height);
        Ok(true)
    }

    pub(crate) fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[ClippedPrimitive],
    ) -> Result<bool, Box<dyn Error>> {
        let frame_index = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize;
        self.wait_frame(frame_index)?;
        self.frame_garbage[frame_index].clear();
        self.free_descriptors
            .append(&mut self.frame_descriptors[frame_index]);
        let allocator = &self.allocators[frame_index];
        unsafe {
            allocator.Reset()?;
            self.command_list
                .Reset(allocator, Some(&self.egui_pipeline))?;
        }

        self.upload_texture_deltas(frame_index, &output.textures_delta.set)?;

        let mut release_values = Vec::new();
        let mut video_captures = Vec::new();
        for index in 0..self.surfaces.len() {
            let mut surface = self.surfaces.remove(index);
            let mut captured = None;
            let pending = surface.latest.and_then(|frame| {
                surface
                    .imported
                    .resource(frame.slot)
                    .map(|source| (frame, source.clone(), surface.imported.uses_keyed_mutex()))
            });
            if let Some((frame, source, keyed_mutex)) = pending {
                if self.publish_video && surface.video_completion_value != 0 {
                    unsafe {
                        self.consumer
                            .queue()
                            .Wait(&self.video_completion_fence, surface.video_completion_value)?;
                    }
                }
                if keyed_mutex {
                    let acquired = surface
                        .imported
                        .acquire_slot(frame.slot, KEYED_MUTEX_TIMEOUT_MS)
                        .is_ok();
                    if acquired {
                        let copied = self.copy_to_cache_sync(
                            &source,
                            &surface.cache,
                            &mut surface.cache_state,
                        );
                        let _ = surface.imported.release_slot(frame.slot);
                        copied?;
                        surface.display_frame = Some(frame);
                        surface.latest = None;
                        captured = Some(frame);
                    }
                } else {
                    surface
                        .imported
                        .wait_on_queue(self.consumer.queue(), frame.sync_value)?;
                    unsafe {
                        transition(
                            &self.command_list,
                            &source,
                            D3D12_RESOURCE_STATE_COMMON,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                        );
                        transition(
                            &self.command_list,
                            &surface.cache,
                            surface.cache_state,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        );
                        self.command_list.CopyResource(&surface.cache, &source);
                        transition(
                            &self.command_list,
                            &source,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                            D3D12_RESOURCE_STATE_COMMON,
                        );
                        transition(
                            &self.command_list,
                            &surface.cache,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                        );
                    }
                    surface.cache_state = D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
                    surface.display_frame = Some(frame);
                    surface.latest = None;
                    captured = Some(frame);
                    release_values.push((surface.connection, frame.sync_value));
                }
            }
            if self.publish_video && captured.is_some() {
                video_captures.push((
                    surface.connection,
                    surface.cache.clone(),
                    surface.imported.width,
                    surface.imported.height,
                    DXGI_FORMAT(surface.imported.format as i32),
                ));
            }
            self.surfaces.insert(index, surface);
        }

        let backbuffer = self.backbuffers[frame_index].clone();
        unsafe {
            transition(
                &self.command_list,
                &backbuffer,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
            let rtv = rtv_handle(&self.rtv_heap, self.rtv_stride, frame_index as u32);
            self.command_list
                .OMSetRenderTargets(1, Some(&rtv), false, None);
            self.command_list
                .ClearRenderTargetView(rtv, &[1.0, 0.0, 1.0, 1.0], None);
            self.command_list
                .SetGraphicsRootSignature(&self.root_signature);
            self.command_list
                .SetDescriptorHeaps(&[Some(self.srv_heap.clone())]);
            self.command_list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        }

        if self
            .surfaces
            .iter()
            .any(|surface| surface.display_frame.is_some())
        {
            self.draw_content(frame_index)?;
        }
        self.draw_egui(frame_index, output.pixels_per_point, primitives)?;

        unsafe {
            transition(
                &self.command_list,
                &backbuffer,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            );
            self.command_list.Close()?;
            let list: ID3D12CommandList = self.command_list.cast()?;
            self.consumer.queue().ExecuteCommandLists(&[Some(list)]);
            for (connection, value) in release_values {
                if let Some(surface) = self
                    .surfaces
                    .iter()
                    .find(|surface| surface.connection == connection)
                {
                    surface
                        .imported
                        .signal_release(self.consumer.queue(), value)?;
                }
            }
            self.fence_value += 1;
            self.consumer
                .queue()
                .Signal(&self.fence, self.fence_value)?;
            self.frame_values[frame_index] = self.fence_value;
            for (connection, resource, width, height, format) in video_captures {
                self.video_completion_value += 1;
                let completion_value = self.video_completion_value;
                if let Some(surface) = self
                    .surfaces
                    .iter_mut()
                    .find(|surface| surface.connection == connection)
                {
                    surface.video_completion_value = completion_value;
                }
                self.completed_video.push(super::webrtc::VideoFrame {
                    connection,
                    width,
                    height,
                    data: super::webrtc::VideoFrameData::D3d12(super::webrtc::D3d12Frame {
                        device: self.consumer.device().clone(),
                        resource,
                        state: D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                        format,
                        ready_fence: self.fence.clone(),
                        ready_value: self.fence_value,
                        completion_fence: self.video_completion_fence.clone(),
                        completion_value,
                        completion_submitted: false,
                    }),
                });
            }
            self.swapchain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        for texture_id in &output.textures_delta.free {
            if let Some(texture) = self.textures.remove(texture_id) {
                self.frame_descriptors[frame_index].push(texture.descriptor);
                self.frame_garbage[frame_index].push(texture.resource);
            }
        }
        Ok(true)
    }

    pub(crate) fn take_video_frames(&mut self) -> Vec<super::webrtc::VideoFrame> {
        std::mem::take(&mut self.completed_video)
    }

    fn draw_content(&mut self, frame_index: usize) -> Result<(), Box<dyn Error>> {
        unsafe {
            self.command_list.SetPipelineState(&self.content_pipeline);
        }
        let visible = self
            .surfaces
            .iter()
            .filter(|surface| surface.display_frame.is_some())
            .count() as u32;
        let cell_height = self.extent.1 / visible.max(1);
        let draws = self
            .surfaces
            .iter()
            .filter(|surface| surface.display_frame.is_some())
            .enumerate()
            .map(|(position, surface)| {
                let frame = surface.display_frame.expect("filtered display frame");
                let (crop_w, crop_h) = super::fit_inside(
                    frame.render_width.max(1),
                    frame.render_height.max(1),
                    surface.imported.width,
                    surface.imported.height,
                );
                let (x, local_y, w, h) =
                    super::fit_extent(crop_w, crop_h, self.extent.0, cell_height);
                let y = position as i32 * cell_height as i32 + local_y;
                let l = x as f32 / self.extent.0 as f32 * 2.0 - 1.0;
                let r = (x + w) as f32 / self.extent.0 as f32 * 2.0 - 1.0;
                let t = 1.0 - y as f32 / self.extent.1 as f32 * 2.0;
                let b = 1.0 - (y + h) as f32 / self.extent.1 as f32 * 2.0;
                let u = crop_w as f32 / surface.imported.width as f32;
                let v = crop_h as f32 / surface.imported.height as f32;
                (surface.descriptor, quad(l, t, r, b, u, v, [255; 4]))
            })
            .collect::<Vec<_>>();
        for (descriptor, vertices) in draws {
            self.draw_vertices(frame_index, descriptor, &vertices, None)?;
        }
        Ok(())
    }

    fn draw_egui(
        &mut self,
        frame_index: usize,
        ppp: f32,
        primitives: &[ClippedPrimitive],
    ) -> Result<(), Box<dyn Error>> {
        unsafe {
            self.command_list.SetPipelineState(&self.egui_pipeline);
        }
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                continue;
            };
            let Some(texture) = self.textures.get(&mesh.texture_id) else {
                continue;
            };
            let vertices = mesh
                .vertices
                .iter()
                .map(|vertex| GpuVertex {
                    position: [
                        vertex.pos.x * ppp / self.extent.0 as f32 * 2.0 - 1.0,
                        1.0 - vertex.pos.y * ppp / self.extent.1 as f32 * 2.0,
                    ],
                    uv: [vertex.uv.x, vertex.uv.y],
                    color: vertex.color.to_array(),
                })
                .collect::<Vec<_>>();
            let clip = primitive.clip_rect;
            let rect = RECT {
                left: (clip.min.x * ppp).floor().max(0.0) as i32,
                top: (clip.min.y * ppp).floor().max(0.0) as i32,
                right: (clip.max.x * ppp).ceil().min(self.extent.0 as f32) as i32,
                bottom: (clip.max.y * ppp).ceil().min(self.extent.1 as f32) as i32,
            };
            self.draw_indexed(
                frame_index,
                texture.descriptor,
                &vertices,
                &mesh.indices,
                rect,
            )?;
        }
        Ok(())
    }

    fn draw_vertices(
        &mut self,
        frame_index: usize,
        descriptor: u32,
        vertices: &[GpuVertex],
        scissor: Option<RECT>,
    ) -> Result<(), Box<dyn Error>> {
        let indices = [0u32, 1, 2, 3, 4, 5];
        self.draw_indexed(
            frame_index,
            descriptor,
            vertices,
            &indices,
            scissor.unwrap_or(RECT {
                left: 0,
                top: 0,
                right: self.extent.0 as i32,
                bottom: self.extent.1 as i32,
            }),
        )
    }

    fn draw_indexed(
        &mut self,
        frame_index: usize,
        descriptor: u32,
        vertices: &[GpuVertex],
        indices: &[u32],
        scissor: RECT,
    ) -> Result<(), Box<dyn Error>> {
        let vb = unsafe { upload_buffer(self.consumer.device(), as_bytes(vertices))? };
        let ib = unsafe { upload_buffer(self.consumer.device(), as_bytes(indices))? };
        let vbv = D3D12_VERTEX_BUFFER_VIEW {
            BufferLocation: unsafe { vb.GetGPUVirtualAddress() },
            SizeInBytes: size_of_val(vertices) as u32,
            StrideInBytes: size_of::<GpuVertex>() as u32,
        };
        let ibv = D3D12_INDEX_BUFFER_VIEW {
            BufferLocation: unsafe { ib.GetGPUVirtualAddress() },
            SizeInBytes: size_of_val(indices) as u32,
            Format: DXGI_FORMAT_R32_UINT,
        };
        unsafe {
            self.command_list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: self.extent.0 as f32,
                Height: self.extent.1 as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            self.command_list.RSSetScissorRects(&[scissor]);
            self.command_list.SetGraphicsRootDescriptorTable(
                0,
                gpu_handle(&self.srv_heap, self.srv_stride, descriptor),
            );
            self.command_list.IASetVertexBuffers(0, Some(&[vbv]));
            self.command_list.IASetIndexBuffer(Some(&ibv));
            self.command_list
                .DrawIndexedInstanced(indices.len() as u32, 1, 0, 0, 0);
        }
        self.frame_garbage[frame_index].extend([vb, ib]);
        Ok(())
    }

    fn upload_texture_deltas(
        &mut self,
        frame_index: usize,
        deltas: &[(TextureId, egui::epaint::ImageDelta)],
    ) -> Result<(), Box<dyn Error>> {
        for (id, delta) in deltas {
            let patch_size = delta.image.size();
            let patch = match &delta.image {
                egui::ImageData::Color(image) => image
                    .pixels
                    .iter()
                    .flat_map(|p| p.to_array())
                    .collect::<Vec<_>>(),
                egui::ImageData::Font(image) => image
                    .srgba_pixels(None)
                    .flat_map(|p| p.to_array())
                    .collect::<Vec<_>>(),
            };
            let (size, mut pixels) = if let Some(pos) = delta.pos {
                let old = self.textures.get(id);
                // A backend switch can happen after egui emitted its initial full atlas,
                // leaving the new renderer's first delta as a partial update. Grow an
                // initially transparent retained image in that case (and whenever the
                // atlas expands).
                let size = [
                    old.map_or(0, |value| value.size[0])
                        .max(pos[0] + patch_size[0]),
                    old.map_or(0, |value| value.size[1])
                        .max(pos[1] + patch_size[1]),
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
                    let dst = ((pos[1] + row) * size[0] + pos[0]) * 4;
                    let src = row * patch_size[0] * 4;
                    pixels[dst..dst + patch_size[0] * 4]
                        .copy_from_slice(&patch[src..src + patch_size[0] * 4]);
                }
                (size, pixels)
            } else {
                (patch_size, patch)
            };
            // Never overwrite a shader-visible descriptor still referenced by another
            // in-flight frame. Its old slot is recycled with that frame's fence.
            let descriptor = self
                .free_descriptors
                .pop()
                .ok_or("egui descriptor heap exhausted")?;
            let (resource, upload) = unsafe {
                upload_texture(self.consumer.device(), &self.command_list, size, &pixels)?
            };
            unsafe {
                create_srv(
                    self.consumer.device(),
                    &self.srv_heap,
                    self.srv_stride,
                    descriptor,
                    &resource,
                )
            };
            if let Some(old) = self.textures.insert(
                *id,
                EguiTexture {
                    resource,
                    pixels: std::mem::take(&mut pixels),
                    size,
                    descriptor,
                },
            ) {
                self.frame_descriptors[frame_index].push(old.descriptor);
                self.frame_garbage[frame_index].push(old.resource);
            }
            self.frame_garbage[frame_index].push(upload);
        }
        Ok(())
    }

    /// Copies `source` into `cache` and blocks until the GPU finishes. Used
    /// for keyed-mutex streams, where the mutex must not be released before
    /// every read of the slot has completed.
    fn copy_to_cache_sync(
        &mut self,
        source: &ID3D12Resource,
        cache: &ID3D12Resource,
        cache_state: &mut D3D12_RESOURCE_STATES,
    ) -> Result<(), Box<dyn Error>> {
        let (allocator, list) = self
            .mutex_copy
            .as_ref()
            .ok_or("keyed-mutex stream has no copy command list")?
            .clone();
        unsafe {
            allocator.Reset()?;
            list.Reset(&allocator, None)?;
            transition(
                &list,
                source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            transition(&list, cache, *cache_state, D3D12_RESOURCE_STATE_COPY_DEST);
            list.CopyResource(cache, source);
            transition(
                &list,
                source,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            );
            transition(
                &list,
                cache,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            list.Close()?;
            let command: ID3D12CommandList = list.cast()?;
            self.consumer.queue().ExecuteCommandLists(&[Some(command)]);
        }
        *cache_state = D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
        self.wait_idle()?;
        Ok(())
    }

    fn wait_frame(&self, index: usize) -> windows::core::Result<()> {
        self.wait_value(self.frame_values[index])
    }
    fn wait_idle(&mut self) -> windows::core::Result<()> {
        unsafe {
            self.fence_value += 1;
            self.consumer
                .queue()
                .Signal(&self.fence, self.fence_value)?;
        }
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

impl Drop for D3d12Compositor {
    fn drop(&mut self) {
        let _ = self.release_imported_streams();
        unsafe {
            let _ = CloseHandle(self.fence_event);
        }
    }
}

fn quad(l: f32, t: f32, r: f32, b: f32, u: f32, v: f32, color: [u8; 4]) -> [GpuVertex; 6] {
    let p = |position, uv| GpuVertex {
        position,
        uv,
        color,
    };
    [
        p([l, t], [0.0, 0.0]),
        p([r, t], [u, 0.0]),
        p([r, b], [u, v]),
        p([l, t], [0.0, 0.0]),
        p([r, b], [u, v]),
        p([l, b], [0.0, v]),
    ]
}

unsafe fn create_backbuffers(
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

unsafe fn create_texture(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    state: D3D12_RESOURCE_STATES,
) -> windows::core::Result<ID3D12Resource> {
    let mut resource: Option<ID3D12Resource> = None;
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

unsafe fn create_srv(
    device: &ID3D12Device,
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
    resource: &ID3D12Resource,
) {
    let desc = unsafe { resource.GetDesc() };
    unsafe {
        device.CreateShaderResourceView(
            resource,
            Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: desc.Format,
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

unsafe fn upload_buffer(
    device: &ID3D12Device,
    bytes: &[u8],
) -> windows::core::Result<ID3D12Resource> {
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_UPLOAD,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: bytes.len().max(1) as u64,
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
    let resource = resource
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))?;
    let mut mapped = ptr::null_mut();
    unsafe {
        resource.Map(0, None, Some(&mut mapped))?;
        ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast(), bytes.len());
        resource.Unmap(0, None);
    }
    Ok(resource)
}

unsafe fn upload_texture(
    device: &ID3D12Device,
    list: &ID3D12GraphicsCommandList,
    size: [usize; 2],
    pixels: &[u8],
) -> windows::core::Result<(ID3D12Resource, ID3D12Resource)> {
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
    let upload = unsafe { upload_buffer(device, &vec![0; pitch * size[1]])? };
    let mut mapped = ptr::null_mut();
    unsafe {
        upload.Map(0, None, Some(&mut mapped))?;
    }
    for row in 0..size[1] {
        unsafe {
            ptr::copy_nonoverlapping(
                pixels.as_ptr().add(row * size[0] * 4),
                mapped.cast::<u8>().add(row * pitch),
                size[0] * 4,
            );
        }
    }
    unsafe {
        upload.Unmap(0, None);
    }
    let mut dst = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(texture.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    };
    let mut src = D3D12_TEXTURE_COPY_LOCATION {
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
        list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
        transition(
            list,
            &texture,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
        );
        ManuallyDrop::drop(&mut dst.pResource);
        ManuallyDrop::drop(&mut src.pResource);
    }
    Ok((texture, upload))
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

fn create_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    alpha_blend: bool,
) -> windows::core::Result<ID3D12PipelineState> {
    const SHADER: &[u8] = br#"
struct VIn { float2 pos : POSITION; float2 uv : TEXCOORD0; float4 color : COLOR0; };
struct VOut { float4 pos : SV_POSITION; float2 uv : TEXCOORD0; float4 color : COLOR0; };
VOut vs_main(VIn v) { VOut o; o.pos=float4(v.pos,0,1); o.uv=v.uv; o.color=v.color; return o; }
Texture2D tex : register(t0); SamplerState samp : register(s0);
float4 ps_main(VOut i) : SV_TARGET { return tex.Sample(samp, i.uv) * i.color; }
"#;
    unsafe {
        let vs = compile(SHADER, b"vs_main\0", b"vs_5_0\0")?;
        let ps = compile(SHADER, b"ps_main\0", b"ps_5_0\0")?;
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
    let mut h = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    h.ptr += (stride * index) as usize;
    h
}
fn cpu_srv_handle(
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let mut h = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    h.ptr += (stride * index) as usize;
    h
}
fn gpu_handle(heap: &ID3D12DescriptorHeap, stride: u32, index: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    let mut h = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
    h.ptr += u64::from(stride * index);
    h
}
fn as_bytes<T>(items: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast(), size_of_val(items)) }
}
