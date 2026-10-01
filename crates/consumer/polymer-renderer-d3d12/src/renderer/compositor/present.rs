use super::*;

impl D3d12Compositor {
    pub fn present_with(
        &mut self,
        plan: &CompositingPlan,
        mut webrtc: Option<&mut WebRtcCompositor>,
        record: impl FnOnce(&mut D3d12RenderPass<'_>) -> Result<(), Box<dyn Error>>,
    ) -> Result<bool, Box<dyn Error>> {
        self.poll_display_latency();
        if webrtc
            .as_deref()
            .is_some_and(|webrtc| !webrtc.d3d12_matches(self.adapter_luid()))
        {
            return Err("WebRTC compositor belongs to a different D3D12 adapter".into());
        }
        let frame_index = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize;
        self.wait_frame(frame_index)?;
        self.frame_garbage[frame_index].clear();
        // The fence above is what makes reusing these bytes safe: the GPU has
        // finished every draw that read from this frame's arena.
        self.frame_uploads[frame_index].reset();
        let allocator = &self.allocators[frame_index];
        unsafe {
            allocator.Reset()?;
            self.command_list
                .Reset(allocator, Some(&self.content_pipeline))?;
        }

        let mut release_values = Vec::new();
        let mut video_captures = Vec::new();
        // Only publish video when every previously published frame has
        // resolved its completion value, and never more per draw than the
        // encoder channel can buffer. Out-of-order completion signals would
        // rewind the fence (dropped frames host-signal immediately while an
        // in-flight frame signals its older value later), and a rewound
        // fence leaves the compositor queue waiting forever.
        let video_publish_ready = webrtc.as_deref().is_some_and(WebRtcCompositor::d3d12_ready);
        let mut video_publish_budget = if video_publish_ready {
            webrtc.as_deref().map_or(0, WebRtcCompositor::available)
        } else {
            0
        };
        // The oldest guest frame this present carries. A pass composing several
        // streams is only as fresh as the stalest one on it, so latency is
        // measured against that rather than the most recent.
        let mut oldest_capture_qpc = None::<u64>;
        for index in 0..self.surfaces.len() {
            let mut surface = self.surfaces.remove(index);
            // Always reinsert the surface, including on recording errors: its
            // retained slot and native resources must survive failed frames.
            let result = (|| -> Result<(), Box<dyn Error>> {
                surface.frames.begin();
                if surface.cache.is_some()
                    && let Some(video) = webrtc.as_deref()
                {
                    // Cached textures may be read by an encoder even on a
                    // redraw without a new producer frame.
                    video.wait_d3d12_stream(self.receiver.queue(), surface.connection)?;
                }
                let mut captured = None;
                if let Some(frame) = surface.frames.pending() {
                    let source = surface
                        .imported
                        .resource(frame.slot)
                        .expect("validated slot")
                        .clone();
                    if surface.imported.producer_ready(frame.sync_value) {
                        if let Some(cache) = &surface.cache {
                            ImportedSurface::copy(&self.command_list, &source, cache);
                        }
                        surface.frames.prepare_pending(if surface.cache.is_some() {
                            Retention::Cached
                        } else {
                            Retention::Retained
                        });
                        captured = Some(frame);
                        if let Some(value) = surface.frames.release_value() {
                            release_values.push((surface.connection, value));
                        }
                    }
                }
                if video_publish_budget > 0
                    && captured.is_some()
                    && webrtc
                        .as_deref()
                        .is_some_and(|video| video.accepts_d3d12_stream(surface.connection))
                {
                    let texture = if let Some(cache) = &surface.cache {
                        cache.clone()
                    } else {
                        // Encoding owns a private snapshot. It must never
                        // outlive the lease on a producer's displayed slot.
                        if surface.video_cache.is_none() {
                            surface.video_cache =
                                Some(surface.create_cache(self.receiver.device())?);
                        }
                        let cache = surface.video_cache.as_ref().expect("video cache");
                        ImportedSurface::copy(
                            &self.command_list,
                            surface.texture().expect("displayed texture"),
                            cache,
                        );
                        cache.clone()
                    };
                    video_publish_budget -= 1;
                    video_captures.push((
                        surface.connection,
                        texture,
                        surface.imported.width,
                        surface.imported.height,
                        DXGI_FORMAT(surface.imported.format as i32),
                    ));
                }
                if let Some(texture) = surface.texture() {
                    unsafe {
                        transition(
                            &self.command_list,
                            texture,
                            D3D12_RESOURCE_STATE_COMMON,
                            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                        )
                    };
                }
                if let Some(frame) = captured {
                    oldest_capture_qpc =
                        Some(oldest_capture_qpc.map_or(frame.timestamp_qpc, |oldest| {
                            oldest.min(frame.timestamp_qpc)
                        }));
                }
                Ok(())
            })();
            self.surfaces.insert(index, surface);
            result?;
        }

        let backbuffer = self.backbuffers[frame_index].clone();
        let clear_color = plan.clear_color();
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
                .ClearRenderTargetView(rtv, &clear_color, None);
            self.command_list
                .SetGraphicsRootSignature(&self.root_signature);
            self.command_list
                .SetDescriptorHeaps(&[Some(self.srv_heap.clone())]);
            self.command_list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        }

        if plan.placements().next().is_some() {
            self.draw_content(frame_index, plan)?;
        }
        record(&mut D3d12RenderPass {
            device: self.receiver.device(),
            command_list: &self.command_list,
            frame_index,
            extent: self.extent,
        })?;

        for surface in &self.surfaces {
            if let Some(texture) = surface.texture() {
                unsafe {
                    transition(
                        &self.command_list,
                        texture,
                        D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                        D3D12_RESOURCE_STATE_COMMON,
                    )
                };
            }
        }
        unsafe {
            transition(
                &self.command_list,
                &backbuffer,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            );
            self.command_list.Close()?;
            let list: ID3D12CommandList = self.command_list.cast()?;
            self.receiver.queue().ExecuteCommandLists(&[Some(list)]);
            for surface in &mut self.surfaces {
                surface.frames.commit();
            }
            for (connection, value) in release_values {
                if let Some(surface) = self
                    .surfaces
                    .iter()
                    .find(|surface| surface.connection == connection)
                {
                    surface
                        .imported
                        .signal_release(self.receiver.queue(), value)?;
                    surface.frames.acknowledge_release(value)?;
                }
            }
            self.fence_value += 1;
            self.receiver
                .queue()
                .Signal(&self.fence, self.fence_value)?;
            self.frame_values[frame_index] = self.fence_value;
            for (connection, resource, width, height, format) in video_captures {
                if let Some(webrtc) = webrtc.as_deref_mut() {
                    webrtc.present_d3d12(
                        connection,
                        self.receiver.device().clone(),
                        resource,
                        D3D12_RESOURCE_STATE_COMMON,
                        format,
                        width,
                        height,
                        self.fence.clone(),
                        self.fence_value,
                    );
                }
            }
            self.swapchain
                .Present(self.present_interval, DXGI_PRESENT(0))
                .ok()?;
            self.frame_permit = false;
            // Only presents that actually carried a new guest frame are worth
            // attributing; a pass that redrew unchanged content would report
            // the age of a frame the display had already shown.
            if let Some(captured_qpc) = oldest_capture_qpc {
                self.display_latency
                    .record(self.swapchain.GetLastPresentCount()?, captured_qpc);
            }
        }
        Ok(true)
    }

    pub fn present_with_webrtc(
        &mut self,
        plan: &CompositingPlan,
        webrtc: Option<&mut WebRtcCompositor>,
    ) -> Result<bool, Box<dyn Error>> {
        self.present_with(plan, webrtc, |_| Ok(()))
    }

    pub fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        self.present_with_webrtc(plan, None)
    }

    pub(super) fn draw_content(
        &mut self,
        frame_index: usize,
        plan: &CompositingPlan,
    ) -> Result<(), Box<dyn Error>> {
        // Painter order follows the plan, not the order streams happened to be
        // imported in, so a shell that stacks surfaces gets the z-order it
        // asked for.
        let corners = |rect: [f32; 4]| {
            (
                rect[0] / self.extent.0 as f32 * 2.0 - 1.0,
                1.0 - rect[1] / self.extent.1 as f32 * 2.0,
                rect[2] / self.extent.0 as f32 * 2.0 - 1.0,
                1.0 - rect[3] / self.extent.1 as f32 * 2.0,
            )
        };
        let draws = plan
            .placements()
            .filter_map(|placement| {
                if placement.content.opacity <= 0.0
                    || placement.content.destination[2] <= placement.content.destination[0]
                    || placement.content.destination[3] <= placement.content.destination[1]
                {
                    return None;
                }
                let surface = self.surfaces.iter().find(|surface| {
                    surface.connection == placement.connection
                        && surface.surface_id == placement.surface_id
                })?;
                surface.frames.frame()?;
                let (l, t, r, b) = corners(placement.content.destination);
                let width = placement.content.destination[2] - placement.content.destination[0];
                let height = placement.content.destination[3] - placement.content.destination[1];
                let opacity = (placement.content.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
                Some((
                    placement.corner_radius > 0.0 || opacity < 255,
                    surface.sample_descriptor(),
                    quad(
                        l,
                        t,
                        r,
                        b,
                        placement.content.source,
                        surface.flip_y,
                        surface.quarter_turns,
                        [255, 255, 255, opacity],
                        [width, height, placement.corner_radius, 0.0],
                    ),
                ))
            })
            .collect::<Vec<_>>();
        let mut blending = None;
        for (blend, descriptor, vertices) in draws {
            if blending != Some(blend) {
                blending = Some(blend);
                // SAFETY: the command list is open for this frame.
                unsafe {
                    self.command_list.SetPipelineState(if blend {
                        &self.blend_pipeline
                    } else {
                        &self.content_pipeline
                    });
                }
            }
            self.draw_vertices(frame_index, descriptor, &vertices, None)?;
        }
        Ok(())
    }

    pub(super) fn draw_vertices(
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

    pub(super) fn draw_indexed(
        &mut self,
        frame_index: usize,
        descriptor: u32,
        vertices: &[GpuVertex],
        indices: &[u32],
        scissor: RECT,
    ) -> Result<(), Box<dyn Error>> {
        let arena = &mut self.frame_uploads[frame_index];
        let garbage = &mut self.frame_garbage[frame_index];
        let device = self.receiver.device();
        let vbv = D3D12_VERTEX_BUFFER_VIEW {
            BufferLocation: arena.write(device, as_bytes(vertices), garbage)?,
            SizeInBytes: size_of_val(vertices) as u32,
            StrideInBytes: size_of::<GpuVertex>() as u32,
        };
        let ibv = D3D12_INDEX_BUFFER_VIEW {
            BufferLocation: arena.write(device, as_bytes(indices), garbage)?,
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
        Ok(())
    }
}

#[expect(clippy::too_many_arguments)]
pub(super) fn quad(
    l: f32,
    t: f32,
    r: f32,
    b: f32,
    source: [f32; 4],
    flip_y: bool,
    quarter_turns: u8,
    color: [u8; 4],
    clip: [f32; 4],
) -> [GpuVertex; 6] {
    let p = |position, uv, local| GpuVertex {
        position,
        uv,
        color,
        local,
        clip,
    };
    let (top_v, bottom_v) = if flip_y { (1.0, 0.0) } else { (0.0, 1.0) };
    let corners = [[0.0, top_v], [1.0, top_v], [1.0, bottom_v], [0.0, bottom_v]];
    let full = [
        corners[quarter_turns as usize % 4],
        corners[(1 + quarter_turns as usize) % 4],
        corners[(2 + quarter_turns as usize) % 4],
        corners[(3 + quarter_turns as usize) % 4],
    ];
    let sample = |x: f32, y: f32| {
        let top = [
            full[0][0] + (full[1][0] - full[0][0]) * x,
            full[0][1] + (full[1][1] - full[0][1]) * x,
        ];
        let bottom = [
            full[3][0] + (full[2][0] - full[3][0]) * x,
            full[3][1] + (full[2][1] - full[3][1]) * x,
        ];
        [
            top[0] + (bottom[0] - top[0]) * y,
            top[1] + (bottom[1] - top[1]) * y,
        ]
    };
    let uv = [
        sample(source[0], source[1]),
        sample(source[2], source[1]),
        sample(source[2], source[3]),
        sample(source[0], source[3]),
    ];
    [
        p([l, t], uv[0], [0.0, 0.0]),
        p([r, t], uv[1], [clip[0], 0.0]),
        p([r, b], uv[2], [clip[0], clip[1]]),
        p([l, t], uv[0], [0.0, 0.0]),
        p([r, b], uv[2], [clip[0], clip[1]]),
        p([l, b], uv[3], [0.0, clip[1]]),
    ]
}
