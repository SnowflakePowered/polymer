use super::*;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::System::Threading::CreateEventW;

const PROBE_INPUT: (u32, u32) = (640, 480);
const PROBE_OUTPUT: (u32, u32) = (1920, 1080);
const PROBE_INSET: f32 = 0.1;
const PROBE_FRAMES: usize = 4;
const PROBE_FIRST_FRAME: usize = 1 << 20;
const STRETCHED_WIDTH: f32 = 0.7;

/// Runs a filter chain over a black input, with or without a white inset
/// rectangle, to find where the chain draws its input.
struct Probe<'a> {
    device: &'a ID3D12Device,
    queue: &'a ID3D12CommandQueue,
    chain: &'a mut FilterChain,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_value: u64,
    event: HANDLE,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_stride: u32,
    input: (ID3D12Resource, (u32, u32)),
    output: (ID3D12Resource, (u32, u32)),
}

impl ShaderStage {
    /// Measures whether a preset places the image inside its viewport itself,
    /// from how widely it responds to a 4:3 input inside a 16:9 viewport.
    pub fn probe_viewport(pack: ShaderPresetPack) -> Result<ShaderViewport, String> {
        unsafe {
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device)
                .map_err(|error| error.to_string())?;
            let device = device.ok_or("D3D12 did not create a device")?;
            let mut chain =
                FilterChain::load_from_pack(pack, &device, Some(&FilterChainOptions::default()))
                    .map_err(|error| error.to_string())?;
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })
                .map_err(|error| error.to_string())?;
            let bounds = Probe::new(&device, &queue, &mut chain, PROBE_INPUT, PROBE_OUTPUT)
                .and_then(|mut probe| probe.bounds())
                .map_err(|error| error.to_string())?;
            Ok(match bounds {
                Some([left, _, right, _]) if right - left < STRETCHED_WIDTH => {
                    ShaderViewport::Element
                }
                _ => ShaderViewport::Content,
            })
        }
    }
}

impl D3d12Compositor {
    /// Where an `element` preset draws the composed surfaces, in swapchain pixels.
    pub fn measure_shader_content(&mut self) -> Result<Option<[f32; 4]>, Box<dyn Error>> {
        let Some(mut stage) = self.shader.take() else {
            return Ok(None);
        };
        let measured = self.measure_through(&mut stage);
        self.shader = Some(stage);
        measured
    }

    fn measure_through(
        &mut self,
        stage: &mut ShaderStage,
    ) -> Result<Option<[f32; 4]>, Box<dyn Error>> {
        let (Some(canvas), Some(output), ShaderViewport::Element) = (
            stage.canvas.as_ref().map(|target| target.extent),
            stage.output.as_ref().map(|target| target.extent),
            stage.viewport,
        ) else {
            return Ok(None);
        };
        self.wait_idle()?;
        let (device, queue) = (
            self.receiver.device().clone(),
            self.receiver.queue().clone(),
        );
        let bounds =
            unsafe { Probe::new(&device, &queue, &mut stage.chain, canvas, output)?.bounds()? };
        stage.fresh = true;
        let grow = 0.5 / (1.0 - 2.0 * PROBE_INSET);
        Ok(bounds.map(|[left, top, right, bottom]| {
            let (x, y) = ((left + right) * 0.5, (top + bottom) * 0.5);
            let (width, height) = ((right - left) * grow, (bottom - top) * grow);
            [
                (x - width) * output.0 as f32,
                (y - height) * output.1 as f32,
                (x + width) * output.0 as f32,
                (y + height) * output.1 as f32,
            ]
        }))
    }
}

impl<'a> Probe<'a> {
    unsafe fn new(
        device: &'a ID3D12Device,
        queue: &'a ID3D12CommandQueue,
        chain: &'a mut FilterChain,
        input: (u32, u32),
        output: (u32, u32),
    ) -> Result<Self, Box<dyn Error>> {
        unsafe {
            let allocator: ID3D12CommandAllocator =
                device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?;
            list.Close()?;
            let rtv_heap: ID3D12DescriptorHeap =
                device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: 2,
                    ..Default::default()
                })?;
            let rtv_stride =
                device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);
            let input_texture =
                ShaderTarget::create(device, input, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE)?;
            let output_texture =
                ShaderTarget::create(device, output, D3D12_RESOURCE_STATE_RENDER_TARGET)?;
            device.CreateRenderTargetView(
                &input_texture,
                None,
                rtv_handle(&rtv_heap, rtv_stride, 0),
            );
            device.CreateRenderTargetView(
                &output_texture,
                None,
                rtv_handle(&rtv_heap, rtv_stride, 1),
            );
            Ok(Self {
                fence: device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?,
                fence_value: 0,
                event: CreateEventW(None, false, false, None)?,
                device,
                queue,
                chain,
                allocator,
                list,
                rtv_heap,
                rtv_stride,
                input: (input_texture, input),
                output: (output_texture, output),
            })
        }
    }

    /// The normalized output bounds that respond at full strength to the
    /// inset rectangle, or `None` when the output ignores its input.
    unsafe fn bounds(&mut self) -> Result<Option<[f32; 4]>, Box<dyn Error>> {
        let dark = unsafe { self.render(false)? };
        let lit = unsafe { self.render(true)? };
        let (width, height) = (self.output.1.0 as usize, self.output.1.1 as usize);
        let response = |index: usize| {
            (0..3)
                .map(|channel| {
                    u16::from(lit[index * 4 + channel].saturating_sub(dark[index * 4 + channel]))
                })
                .max()
                .unwrap_or_default()
        };
        let strongest = (0..width * height).map(response).max().unwrap_or_default();
        if strongest < 16 {
            return Ok(None);
        }
        let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
        for y in 0..height {
            for x in 0..width {
                if response(y * width + x) * 10 >= strongest * 9 {
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x + 1);
                    bottom = bottom.max(y + 1);
                }
            }
        }
        Ok(Some([
            left as f32 / width as f32,
            top as f32 / height as f32,
            right as f32 / width as f32,
            bottom as f32 / height as f32,
        ]))
    }

    /// Renders the chain over the probe input and reads the final frame back
    /// as tightly packed BGRA.
    unsafe fn render(&mut self, lit: bool) -> Result<Vec<u8>, Box<dyn Error>> {
        let input_rtv = rtv_handle(&self.rtv_heap, self.rtv_stride, 0);
        let output_rtv = rtv_handle(&self.rtv_heap, self.rtv_stride, 1);
        let (input, output) = (self.input.1, self.output.1);
        let size = Size::new(output.0, output.1);
        let inset = RECT {
            left: (input.0 as f32 * PROBE_INSET) as i32,
            top: (input.1 as f32 * PROBE_INSET) as i32,
            right: (input.0 as f32 * (1.0 - PROBE_INSET)) as i32,
            bottom: (input.1 as f32 * (1.0 - PROBE_INSET)) as i32,
        };
        let desc = unsafe { self.output.0.GetDesc() };
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut total = 0;
        unsafe {
            self.device.GetCopyableFootprints(
                &raw const desc,
                0,
                1,
                0,
                Some(&raw mut footprint),
                None,
                None,
                Some(&raw mut total),
            );
        }
        let readback =
            unsafe { super::super::snapshot::create_readback_buffer(self.device, total)? };
        for frame in 0..PROBE_FRAMES {
            unsafe {
                self.allocator.Reset()?;
                self.list.Reset(&self.allocator, None)?;
                if frame == 0 {
                    transition(
                        &self.list,
                        &self.input.0,
                        D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                    );
                    self.list
                        .ClearRenderTargetView(input_rtv, &[0.0, 0.0, 0.0, 1.0], None);
                    if lit {
                        self.list.ClearRenderTargetView(
                            input_rtv,
                            &[1.0, 1.0, 1.0, 1.0],
                            Some(&[inset]),
                        );
                    }
                    transition(
                        &self.list,
                        &self.input.0,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                        D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                    );
                }
                self.chain.frame(
                    &self.list,
                    D3D12InputImage::from(self.input.0.to_ref()),
                    &Viewport {
                        x: 0.0,
                        y: 0.0,
                        mvp: None,
                        output: D3D12OutputView::new_from_raw(output_rtv, size, SWAPCHAIN_FORMAT),
                        size,
                    },
                    PROBE_FIRST_FRAME + frame,
                    Some(&FrameOptions {
                        clear_history: frame == 0,
                        ..Default::default()
                    }),
                )?;
                if frame + 1 == PROBE_FRAMES {
                    self.copy_output(&readback, footprint);
                }
                self.list.Close()?;
                self.queue
                    .ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>()?)]);
                self.fence_value += 1;
                self.queue.Signal(&self.fence, self.fence_value)?;
                self.fence
                    .SetEventOnCompletion(self.fence_value, self.event)?;
                WaitForSingleObject(self.event, INFINITE);
            }
        }
        let row = output.0 as usize * 4;
        let pitch = footprint.Footprint.RowPitch as usize;
        let mut pixels = Vec::with_capacity(row * output.1 as usize);
        unsafe {
            let mut mapped = ptr::null_mut();
            readback.Map(0, None, Some(&mut mapped))?;
            let bytes = std::slice::from_raw_parts(mapped.cast::<u8>(), total as usize);
            for y in 0..output.1 as usize {
                let start = footprint.Offset as usize + y * pitch;
                pixels.extend_from_slice(&bytes[start..start + row]);
            }
            readback.Unmap(0, None);
        }
        Ok(pixels)
    }

    unsafe fn copy_output(
        &self,
        readback: &ID3D12Resource,
        footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    ) {
        unsafe {
            transition(
                &self.list,
                &self.output.0,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            let mut destination = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(readback.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: footprint,
                },
            };
            let mut source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(self.output.0.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            self.list
                .CopyTextureRegion(&destination, 0, 0, 0, &source, None);
            ManuallyDrop::drop(&mut destination.pResource);
            ManuallyDrop::drop(&mut source.pResource);
            transition(
                &self.list,
                &self.output.0,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
        }
    }
}

impl Drop for Probe<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}
